//! carte-agent: runs in the guest's user session. Sends the host the list
//! of installed applications, and their icons, and launches them when the
//! host asks. Only the host (CID 2) may connect.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{self, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use carte::desktop::{self, DesktopEntry};
use carte::inotify::Watcher;
use carte::proto::{self, App, AppList, Reply, Request};
use carte::{icons, vsock, xdg};

/// How long the applications directories must be quiet before a rescan.
const SETTLE: Duration = Duration::from_secs(1);

#[derive(Default)]
struct State {
    /// 0 until the first scan.
    generation: u64,
    apps: Arc<Vec<App>>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

impl Shared {
    fn publish(&self, apps: Vec<App>) {
        let mut state = self.state.lock().unwrap();
        if state.generation > 0 && *state.apps == apps {
            return;
        }
        state.generation += 1;
        state.apps = Arc::new(apps);
        self.changed.notify_all();
    }

    fn app(&self, id: &str) -> Option<App> {
        self.state.lock().unwrap().apps.iter().find(|a| a.id == id).cloned()
    }
}

fn main() {
    // sommelier names windows after this, so it's how the host tells VMs apart.
    let vm = std::env::var("SOMMELIER_VM_IDENTIFIER")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "termina".into());
    if !proto::is_valid_vm(&vm) {
        eprintln!("carte-agent: invalid VM name {vm:?}: use letters, digits, '_' and '.'");
        process::exit(1);
    }
    let vm: &'static str = vm.leak();

    let shared: &'static Shared = Box::leak(Box::default());
    thread::spawn(|| watch_apps(shared));

    let listener = vsock::Listener::bind(proto::AGENT_PORT).unwrap_or_else(|e| {
        eprintln!("carte-agent: listening on vsock port {}: {e}", proto::AGENT_PORT);
        process::exit(1);
    });
    eprintln!("carte-agent: VM {vm}, listening on vsock port {}", proto::AGENT_PORT);
    loop {
        let (conn, cid) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(e) => {
                eprintln!("carte-agent: accept: {e}");
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        if cid != vsock::HOST_CID {
            eprintln!("carte-agent: refused connection from CID {cid}");
            continue;
        }
        thread::spawn(move || {
            if let Err(e) = serve(&conn, vm, shared) {
                eprintln!("carte-agent: connection closed: {e}");
            }
        });
    }
}

/// Keeps the application list in `shared` up to date.
fn watch_apps(shared: &Shared) {
    loop {
        let data_dirs = xdg::data_dirs();
        // Watch before scanning, so a change during the scan isn't missed.
        let watcher = match Watcher::new() {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("carte-agent: inotify: {e}, rescanning every minute");
                None
            }
        };
        if let Some(w) = &watcher {
            for dir in &data_dirs {
                w.watch_tree(&dir.join("applications"));
            }
        }
        let entries = desktop::scan(&data_dirs);
        let apps: Vec<App> = entries.iter().filter(|e| e.should_export()).map(DesktopEntry::to_app).collect();
        shared.publish(apps);
        match watcher.map(|w| w.wait(SETTLE)) {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                eprintln!("carte-agent: inotify: {e}");
                thread::sleep(Duration::from_secs(5));
            }
            None => thread::sleep(Duration::from_secs(60)),
        }
    }
}

fn serve(conn: &File, vm: &str, shared: &Shared) -> io::Result<()> {
    let mut reader = BufReader::new(conn);
    let mut writer = conn;
    while let Some(request) = proto::recv::<Request>(&mut reader)? {
        match request {
            Request::Watch => return send_apps(writer, vm, shared),
            Request::Icon { id, size } => send_icon(writer, &id, size, shared)?,
            Request::Launch { vm: wanted, id } => {
                let result = if wanted == vm { launch(&id) } else { Err(format!("this is {vm}, not {wanted}")) };
                let reply = match result {
                    Ok(()) => Reply::default(),
                    Err(e) => {
                        eprintln!("carte-agent: launching {id}: {e}");
                        Reply::error(e)
                    }
                };
                proto::send(&mut writer, &reply)?;
            }
        }
    }
    Ok(())
}

/// Sends the list now and after every change, until the host goes away.
fn send_apps(mut writer: &File, vm: &str, shared: &Shared) -> io::Result<()> {
    let mut seen = 0;
    loop {
        let apps = {
            let mut state = shared.state.lock().unwrap();
            while state.generation == seen {
                state = shared.changed.wait(state).unwrap();
            }
            seen = state.generation;
            state.apps.clone()
        };
        proto::send(&mut writer, &AppList { vm: vm.to_owned(), apps: apps.to_vec() })?;
    }
}

fn send_icon(mut writer: &File, id: &str, size: u32, shared: &Shared) -> io::Result<()> {
    let found = shared
        .app(id)
        .and_then(|app| app.icon)
        .and_then(|icon| icons::find(&icon, size, &xdg::data_dirs()));
    let Some((path, format)) = found else {
        return proto::send(&mut writer, &Reply::error(format!("no icon for {id}")));
    };
    let mut data = Vec::new();
    if let Err(e) = File::open(&path).and_then(|f| f.take(proto::MAX_ICON + 1).read_to_end(&mut data)) {
        return proto::send(&mut writer, &Reply::error(format!("{}: {e}", path.display())));
    }
    if data.len() as u64 > proto::MAX_ICON {
        return proto::send(&mut writer, &Reply::error(format!("{}: too large", path.display())));
    }
    let header = Reply { format: Some(format.into()), len: Some(data.len() as u64), error: None };
    proto::send(&mut writer, &header)?;
    writer.write_all(&data)
}

/// Starts the application in its own systemd scope, like GNOME does, so it
/// outlives the agent and shows up as app-carte-<id>-<n>.scope.
fn launch(id: &str) -> Result<(), String> {
    // Read the file again rather than trusting the last scan: it may have
    // changed in the last second.
    let entry = desktop::scan(&xdg::data_dirs())
        .into_iter()
        .find(|e| e.id == id)
        .filter(DesktopEntry::should_export)
        .ok_or_else(|| format!("no application {id}"))?;
    let argv = entry.exec_argv()?;
    if desktop::find_executable(&argv[0]).is_none() {
        return Err(format!("{} not found", argv[0]));
    }

    static LAUNCHES: AtomicU64 = AtomicU64::new(0);
    let unit = format!(
        "app-carte-{}-{}{}.scope",
        id.replace('-', "\\x2d"),
        process::id(),
        LAUNCHES.fetch_add(1, Ordering::Relaxed)
    );
    let dir = entry.working_dir.as_deref().map(Path::new).filter(|d| d.is_dir());
    let mut command = Command::new("systemd-run");
    command
        .args(["--user", "--scope", "--quiet", "--collect"])
        .arg(format!("--unit={unit}"))
        .arg("--")
        .args(&argv)
        .current_dir(dir.map_or_else(xdg::home, Path::to_owned))
        .stdin(process::Stdio::null());
    // Its own session, away from the agent's.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|e| format!("running systemd-run: {e}"))?;
    eprintln!("carte-agent: launched {id} as {unit}: {argv:?}");
    // systemd-run execs the application, so this waits for it to exit.
    thread::spawn(move || child.wait());
    Ok(())
}
