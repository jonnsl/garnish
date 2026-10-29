//! carte: the host side of carte-agent.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;
use std::time::Duration;

use carte::proto::{self, App, AppList, Reply, Request};
use carte::{menu, vsock, xdg};

const USAGE: &str = "\
usage: carte sync --cid <cid>      put the applications of the VM at <cid> in the
                                  application menu and keep them up to date
       carte launch <vm> <app-id>  start an application in a running VM
       carte remove <vm>           remove a VM's applications from the menu";

const RETRY: Duration = Duration::from_secs(1);
const TIMEOUT: Duration = Duration::from_secs(10);
/// GNOME's app grid shows icons at up to 192 px (96 at 2x).
const ICON_SIZE: u32 = 256;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["sync", "--cid", cid] => match cid.parse() {
            Ok(cid) => sync(cid),
            Err(_) => Err(format!("invalid CID {cid}")),
        },
        ["launch", vm, id] => launch(vm, id),
        ["remove", vm] => remove(vm),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("carte: {e}");
            ExitCode::FAILURE
        }
    }
}

fn check_vm(vm: &str) -> Result<(), String> {
    if proto::is_valid_vm(vm) {
        Ok(())
    } else {
        Err(format!("invalid VM name {vm:?}"))
    }
}

/// Where `sync` records the CID of a running VM, for `launch`.
fn cid_file(vm: &str) -> PathBuf {
    xdg::runtime_dir().join("carte").join(format!("{vm}.cid"))
}

fn icon_dir(vm: &str) -> PathBuf {
    xdg::data_home().join("carte/icons").join(vm)
}

fn applications_dir() -> PathBuf {
    xdg::data_home().join("applications")
}

fn sync(cid: u32) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("finding the carte binary: {e}"))?;
    let mut waiting = false;
    loop {
        let conn = match vsock::connect(cid, proto::AGENT_PORT) {
            Ok(conn) => conn,
            Err(e) => {
                if !waiting {
                    eprintln!("carte: waiting for carte-agent on CID {cid} ({e})");
                    waiting = true;
                }
                thread::sleep(RETRY);
                continue;
            }
        };
        waiting = false;
        eprintln!("carte: connected to CID {cid}");
        let mut vm = None;
        let result = follow(&conn, cid, &exe, &mut vm);
        if let Some(vm) = vm {
            remove_cid_file(&vm, cid);
        }
        match result {
            Ok(()) => eprintln!("carte: CID {cid} closed the connection"),
            Err(e) => eprintln!("carte: connection to CID {cid}: {e}"),
        }
        thread::sleep(RETRY);
    }
}

fn read_cid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Removes the CID file of `vm` if it still points at `cid`. Another VM
/// with the same name may have replaced it, and that VM is still running.
fn remove_cid_file(vm: &str, cid: u32) {
    let path = cid_file(vm);
    if read_cid(&path) == Some(cid) {
        let _ = fs::remove_file(path);
    }
}

/// Applies application lists from the agent until the connection ends.
/// `vm` is set to the VM's name once it is known.
fn follow(conn: &File, cid: u32, exe: &Path, vm: &mut Option<String>) -> io::Result<()> {
    proto::send(&mut &*conn, &Request::Watch)?;
    let mut reader = BufReader::new(conn);
    while let Some(list) = proto::recv::<AppList>(&mut reader)? {
        if !proto::is_valid_vm(&list.vm) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid VM name {:?}", list.vm)));
        }
        if vm.as_deref() != Some(list.vm.as_str()) {
            let path = cid_file(&list.vm);
            fs::create_dir_all(path.parent().unwrap())?;
            if let Some(other) = read_cid(&path).filter(|&c| c != cid) {
                eprintln!("carte: {}: replacing CID {other}, is another VM also named {}?", list.vm, list.vm);
            }
            fs::write(&path, format!("{cid}\n"))?;
            *vm = Some(list.vm.clone());
        }
        export(cid, &list, exe);
    }
    Ok(())
}

/// Writes the .desktop files and icons for `list` and removes those of
/// applications that are gone.
fn export(cid: u32, list: &AppList, exe: &Path) {
    let vm = &list.vm;
    let apps: Vec<&App> = list
        .apps
        .iter()
        .filter(|app| {
            let ok = proto::is_valid_id(&app.id) && app.name.contains_key("");
            if !ok {
                eprintln!("carte: {vm}: skipping invalid application {:?}", app.id);
            }
            ok
        })
        .collect();

    let icons = icon_dir(vm);
    let apps_dir = applications_dir();
    for dir in [&icons, &apps_dir] {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("carte: creating {}: {e}", dir.display());
            return;
        }
    }

    // Icons first, so they're there when GNOME reads the .desktop files.
    let mut fetcher = IconFetcher::new(cid);
    let mut icon_files = HashSet::new();
    let mut desktop_files = HashSet::new();
    for app in &apps {
        let icon = app.icon.as_ref().and_then(|_| update_icon(&mut fetcher, &icons, &app.id));
        if let Some(icon) = &icon {
            icon_files.insert(icon.file_name().unwrap().to_owned());
        }
        let name = menu::file_name(vm, &app.id);
        let text = menu::render(vm, app, icon.as_deref(), exe);
        if let Err(e) = write_if_changed(&apps_dir.join(&name), text.as_bytes()) {
            eprintln!("carte: writing {name}: {e}");
        }
        desktop_files.insert(name);
    }

    remove_others(&apps_dir, |name| is_vms_desktop_file(&apps_dir, vm, name), &desktop_files);
    remove_others(&icons, |_| true, &icon_files);
    eprintln!("carte: {vm}: {} applications", desktop_files.len());
}

/// Fetches the icon of `id` into `dir` and returns its path. Keeps the
/// existing file if the agent can't be reached.
fn update_icon(fetcher: &mut IconFetcher, dir: &Path, id: &str) -> Option<PathBuf> {
    let existing = || ["png", "svg"].map(|ext| dir.join(format!("{id}.{ext}"))).into_iter().find(|p| p.is_file());
    match fetcher.fetch(id) {
        Ok(Some((format, data))) => {
            let path = dir.join(format!("{id}.{format}"));
            if let Err(e) = write_if_changed(&path, &data) {
                eprintln!("carte: writing {}: {e}", path.display());
                return existing();
            }
            Some(path)
        }
        Ok(None) => None,
        Err(e) => {
            eprintln!("carte: icon for {id}: {e}");
            existing()
        }
    }
}

/// One connection for all the icons of an update.
struct IconFetcher {
    cid: u32,
    conn: Option<BufReader<File>>,
}

impl IconFetcher {
    fn new(cid: u32) -> Self {
        IconFetcher { cid, conn: None }
    }

    fn fetch(&mut self, id: &str) -> io::Result<Option<(String, Vec<u8>)>> {
        let result = self.try_fetch(id);
        if result.is_err() {
            // Don't reuse a connection that is in an unknown state.
            self.conn = None;
        }
        result
    }

    fn try_fetch(&mut self, id: &str) -> io::Result<Option<(String, Vec<u8>)>> {
        if self.conn.is_none() {
            let conn = vsock::connect(self.cid, proto::AGENT_PORT)?;
            vsock::set_timeout(&conn, TIMEOUT)?;
            self.conn = Some(BufReader::new(conn));
        }
        let reader = self.conn.as_mut().unwrap();
        proto::send(&mut reader.get_ref(), &Request::Icon { id: id.to_owned(), size: ICON_SIZE })?;
        let reply: Reply = proto::recv(reader)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        if reply.error.is_some() {
            return Ok(None);
        }
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("invalid icon {what}"));
        let format = reply.format.filter(|f| f == "png" || f == "svg").ok_or_else(|| invalid("format"))?;
        let len = reply.len.filter(|&l| l <= proto::MAX_ICON).ok_or_else(|| invalid("size"))?;
        let mut data = vec![0; len as usize];
        reader.read_exact(&mut data)?;
        Ok(Some((format, data)))
    }
}

/// Replaces `path` atomically, and only if the content differs, so GNOME
/// doesn't reload unchanged entries.
fn write_if_changed(path: &Path, data: &[u8]) -> io::Result<()> {
    if fs::read(path).is_ok_and(|old| old == data) {
        return Ok(());
    }
    // A dot file without .desktop at the end, so GNOME ignores it.
    let tmp = path.with_file_name(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

/// Whether `name` in `dir` is a .desktop file carte wrote for `vm`.
fn is_vms_desktop_file(dir: &Path, vm: &str, name: &str) -> bool {
    name.starts_with("carte-")
        && fs::read_to_string(dir.join(name)).is_ok_and(|text| menu::belongs_to(vm, name, &text))
}

/// Removes the files in `dir` that match `ours` but aren't in `keep`.
fn remove_others(dir: &Path, ours: impl Fn(&str) -> bool, keep: &HashSet<impl AsRef<std::ffi::OsStr> + Eq + std::hash::Hash>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let keep: HashSet<&std::ffi::OsStr> = keep.iter().map(AsRef::as_ref).collect();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_str().is_some_and(&ours) && !keep.contains(name.as_os_str()) {
            if let Err(e) = fs::remove_file(entry.path()) {
                eprintln!("carte: removing {}: {e}", entry.path().display());
            }
        }
    }
}

fn launch(vm: &str, id: &str) -> Result<(), String> {
    let result = try_launch(vm, id);
    // Started from the application menu, nobody sees stderr.
    if let Err(e) = &result {
        let _ = Command::new("notify-send")
            .args(["--app-name=carte", &format!("Could not start {id}"), e])
            .status();
    }
    result
}

fn try_launch(vm: &str, id: &str) -> Result<(), String> {
    check_vm(vm)?;
    if !proto::is_valid_id(id) {
        return Err(format!("invalid application ID {id:?}"));
    }
    let cid = read_cid(&cid_file(vm)).ok_or_else(|| format!("{vm} is not running"))?;
    let conn = vsock::connect(cid, proto::AGENT_PORT).map_err(|e| format!("connecting to {vm} (CID {cid}): {e}"))?;
    vsock::set_timeout(&conn, TIMEOUT).map_err(|e| e.to_string())?;
    proto::send(&mut &conn, &Request::Launch { vm: vm.to_owned(), id: id.to_owned() })
        .map_err(|e| format!("talking to {vm}: {e}"))?;
    let reply: Reply = proto::recv(&mut BufReader::new(&conn))
        .map_err(|e| format!("talking to {vm}: {e}"))?
        .ok_or_else(|| format!("{vm} closed the connection"))?;
    match reply.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn remove(vm: &str) -> Result<(), String> {
    check_vm(vm)?;
    let apps_dir = applications_dir();
    let none: HashSet<String> = HashSet::new();
    remove_others(&apps_dir, |name| is_vms_desktop_file(&apps_dir, vm, name), &none);
    match fs::remove_dir_all(icon_dir(vm)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(format!("removing icons: {e}")),
        _ => {}
    }
    Ok(())
}
