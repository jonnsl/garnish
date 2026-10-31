//! The daemon: keeps track of the running VMs and the resources they hold.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::process::Child;
use tokio::sync::{Notify, mpsc};
use tokio::time::{Instant, sleep, sleep_until, timeout};
use zbus::object_server::SignalEmitter;
use zbus::{Connection, interface};

use crate::config::{self, Clipboard, DaemonConfig, NetworkMode, VmConfig};
use crate::dbus::{Error, OBJECT_PATH, VmInfo};
use crate::ini;
use crate::net;
use crate::pool::Pool;
use crate::vm::{self, Net, Paths, Resources};
use crate::xdg;

/// How long crosvm gets to create its control socket.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// How long crosvm and the helpers get to exit after being asked to.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
/// A guest that reboots more often than this is stopped instead.
const MAX_RESETS: usize = 5;
const RESET_WINDOW: Duration = Duration::from_secs(60);
/// A guest that hasn't said it booted by then (an image without
/// gerant-ready.service) is shown as running anyway.
const BOOT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
enum Command {
    /// Power button, then `crosvm stop` after the timeout.
    Stop,
    /// `crosvm stop` now.
    Kill,
}

struct RunningVm {
    info: VmInfo,
    res: Resources,
    paths: Paths,
    commands: mpsc::UnboundedSender<Command>,
}

struct Inner {
    pool: Pool,
    vms: BTreeMap<String, RunningVm>,
    shutting_down: bool,
}

pub struct Manager {
    conn: Connection,
    config_dir: PathBuf,
    stop_timeout: Duration,
    /// Whether guests can tell us they booted (see `ready`). Without it,
    /// VMs are running as soon as crosvm runs.
    boot_notifications: bool,
    inner: Mutex<Inner>,
    /// Signalled whenever a VM is removed.
    removed: Notify,
}

/// What `launch` started.
struct Launched {
    crosvm: Child,
    /// crosvm's arguments, to start it again when the guest reboots.
    args: Vec<OsString>,
    /// The network crosvm's arguments refer to; a tap stays open for those
    /// restarts.
    net: Option<Net>,
    helpers: Vec<(&'static str, Child)>,
}

fn log(msg: impl std::fmt::Display) {
    eprintln!("gerantd: {msg}");
}

fn describe(status: &io::Result<ExitStatus>) -> String {
    match status {
        Ok(s) => match (s.code(), s.signal()) {
            (Some(0), _) => "the guest shut down".to_owned(),
            (Some(vm::EXIT_RESET), _) => "the guest rebooted".to_owned(),
            (Some(code), _) => format!("crosvm exited with status {code}"),
            (None, Some(sig)) => format!("crosvm was killed by signal {sig}"),
            (None, None) => "crosvm exited".to_owned(),
        },
        Err(e) => format!("waiting for crosvm: {e}"),
    }
}

/// Runs `crosvm <command> <socket>`, for powerbtn and stop.
async fn control(name: &str, command: &str, paths: &Paths) {
    let result = timeout(
        EXIT_TIMEOUT,
        tokio::process::Command::new("crosvm")
            .arg(command)
            .arg(paths.control_socket())
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await;
    match result {
        Ok(Ok(out)) if out.status.success() => {}
        Ok(Ok(out)) => log(format!(
            "{name}: crosvm {command}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Ok(Err(e)) => log(format!("{name}: crosvm {command}: {e}")),
        Err(_) => log(format!("{name}: crosvm {command}: timed out")),
    }
}

/// Asks the helpers to exit and kills those that don't.
async fn stop_helpers(name: &str, helpers: &mut Vec<(&'static str, Child)>) {
    for (_, child) in helpers.iter() {
        vm::terminate(child);
    }
    for (program, mut child) in helpers.drain(..) {
        if timeout(EXIT_TIMEOUT, child.wait()).await.is_err() {
            log(format!("{name}: killing {program}"));
            let _ = child.kill().await;
        }
    }
}

impl Manager {
    pub fn new(conn: Connection, config: DaemonConfig, boot_notifications: bool) -> Arc<Manager> {
        Arc::new(Manager {
            conn,
            config_dir: xdg::config_dir(),
            stop_timeout: Duration::from_secs(config.stop_timeout_secs),
            boot_notifications,
            inner: Mutex::new(Inner {
                pool: Pool::new(config),
                vms: BTreeMap::new(),
                shutting_down: false,
            }),
            removed: Notify::new(),
        })
    }

    fn vm_dir(&self) -> PathBuf {
        self.config_dir.join("vms")
    }

    fn load(&self, name: &str) -> Result<VmConfig, Error> {
        if !config::is_valid_name(name) {
            return Err(Error::NoSuchVm(format!("invalid VM name {name:?}")));
        }
        let path = self.vm_dir().join(format!("{name}.conf"));
        let entries = match ini::load(&path) {
            Ok(Ok(entries)) => entries,
            Ok(Err(e)) => return Err(Error::InvalidConfig(e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(Error::NoSuchVm(format!(
                    "no VM named {name} ({} doesn't exist)",
                    path.display()
                )));
            }
            Err(e) => {
                return Err(Error::InvalidConfig(format!(
                    "reading {}: {e}",
                    path.display()
                )));
            }
        };
        let config = VmConfig::from_entries(&entries, &xdg::user_name(), &xdg::home())
            .map_err(Error::InvalidConfig)?;
        config
            .check_files()
            .map_err(|e| Error::InvalidConfig(format!("{}: {e}", path.display())))?;
        Ok(config)
    }

    fn emitter(&self) -> zbus::Result<SignalEmitter<'_>> {
        SignalEmitter::new(&self.conn, OBJECT_PATH)
    }

    fn info(&self, name: &str) -> Option<VmInfo> {
        self.inner
            .lock()
            .unwrap()
            .vms
            .get(name)
            .map(|vm| vm.info.clone())
    }

    /// Sends VmStateChanged.
    async fn state_changed(&self, info: VmInfo) {
        if let Ok(emitter) = self.emitter() {
            let _ = Gerant::vm_state_changed(&emitter, info).await;
        }
    }

    /// Records a running VM's new state, or crosvm's new PID after a guest
    /// reboot, and tells the clients.
    async fn set_state(&self, name: &str, state: &str, pid: Option<u32>) {
        let changed = {
            let mut inner = self.inner.lock().unwrap();
            let Some(vm) = inner.vms.get_mut(name) else {
                return;
            };
            let old = vm.info.clone();
            vm.info.state = state.to_owned();
            if let Some(pid) = pid {
                vm.info.pid = pid;
            }
            (vm.info != old).then(|| vm.info.clone())
        };
        if let Some(info) = changed {
            self.state_changed(info).await;
        }
    }

    /// The state once crosvm runs.
    fn booting_state(&self) -> &'static str {
        if self.boot_notifications {
            "booting"
        } else {
            "running"
        }
    }

    /// Moves a booting VM to running. Returns false if it wasn't booting.
    async fn finish_boot(&self, name: &str) -> bool {
        let info = {
            let mut inner = self.inner.lock().unwrap();
            match inner.vms.get_mut(name) {
                Some(vm) if vm.info.state == "booting" => {
                    vm.info.state = "running".to_owned();
                    vm.info.clone()
                }
                _ => return false,
            }
        };
        self.state_changed(info).await;
        true
    }

    /// The guest with this CID says it has booted.
    pub async fn guest_ready(&self, cid: u32) {
        let name = {
            let inner = self.inner.lock().unwrap();
            inner
                .vms
                .iter()
                .find(|(_, vm)| vm.res.cid == cid)
                .map(|(name, _)| name.clone())
        };
        // Not ours, or already running (the service was started again).
        if let Some(name) = name
            && self.finish_boot(&name).await
        {
            log(format!("{name}: the guest has booted"));
        }
    }

    pub async fn start(self: &Arc<Self>, name: &str) -> Result<VmInfo, Error> {
        let config = self.load(name)?;
        let wayland = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into());
        let paths = Paths {
            runtime: xdg::runtime_dir().join("garnish").join(name),
            logs: xdg::state_dir().join(name),
            wayland: xdg::runtime_dir().join(wayland),
        };
        if config.wayland && !paths.wayland.exists() {
            return Err(Error::StartFailed(format!(
                "no Wayland socket at {} (Wayland=no starts the VM without one)",
                paths.wayland.display()
            )));
        }
        // Not silently: the clipboard would be open.
        if config.wayland && config.clipboard != Clipboard::Both && !vm::in_path("videur") {
            return Err(Error::StartFailed(format!(
                "Clipboard={} needs videur (vm_tools/videur); or set Clipboard=both",
                config.clipboard.as_str()
            )));
        }
        if config.network == NetworkMode::Passt && !vm::in_path("passt") {
            return Err(Error::StartFailed(
                "[Network] Mode=passt needs passt (https://passt.top); or choose Mode=nat or none"
                    .into(),
            ));
        }
        if config.audio && !vm::in_path("vhost-device-sound") {
            return Err(Error::StartFailed(
                "Audio=yes needs vhost-device-sound (see host/vhost-device-sound)".into(),
            ));
        }
        let already_running = || Error::AlreadyRunning(format!("{name} is already running"));
        if self.inner.lock().unwrap().vms.contains_key(name) {
            return Err(already_running());
        }
        // Before the VM is listed as starting: this can take a moment. If
        // the start fails after all, dropping the tap removes it.
        let net = match config.network {
            NetworkMode::None => None,
            NetworkMode::Passt => Some(Net::Passt),
            NetworkMode::Nat => Some(Net::Tap(Arc::new(net::nat_tap().await.map_err(|e| {
                Error::StartFailed(format!(
                    "no network: {e} ([Network] Mode=passt or none start the VM without gerant-net)"
                ))
            })?))),
        };

        let (tx, rx) = mpsc::unbounded_channel();
        let (res, info) = {
            let mut inner = self.inner.lock().unwrap();
            if inner.shutting_down {
                return Err(Error::ShuttingDown("gerantd is shutting down".into()));
            }
            if inner.vms.contains_key(name) {
                return Err(already_running());
            }
            let cid = inner.pool.alloc_cid().map_err(Error::NoResources)?;
            let res = Resources {
                cid,
                net,
                dns: inner.pool.dns().to_vec(),
            };
            // A tap's address is the guest's, from the host too. passt's
            // isn't: the host can't reach the guest there.
            let tap = match &res.net {
                Some(Net::Tap(tap)) => Some(&tap.info),
                _ => None,
            };
            let info = VmInfo {
                state: "starting".into(),
                cid,
                tap: tap.map(|t| t.name.clone()).unwrap_or_default(),
                address: tap.map(|t| t.guest.to_string()).unwrap_or_default(),
                user: config.user.clone(),
                log_dir: paths.logs.display().to_string(),
                ..VmInfo::stopped(name)
            };
            let vm = RunningVm {
                info: info.clone(),
                res: res.clone(),
                paths: paths.clone(),
                commands: tx,
            };
            inner.vms.insert(name.to_owned(), vm);
            (res, info)
        };
        self.state_changed(info).await;

        let launched = match self.launch(name, &config, &res, &paths).await {
            Ok(launched) => launched,
            Err(e) => {
                log(format!("{name}: {e}"));
                self.release(name);
                self.state_changed(VmInfo::stopped(name)).await;
                return Err(e);
            }
        };
        self.set_state(name, self.booting_state(), launched.crosvm.id())
            .await;
        let info = self.info(name).unwrap();
        let net = match &res.net {
            None => "no network".to_owned(),
            Some(Net::Passt) => "network through passt".to_owned(),
            Some(Net::Tap(tap)) => format!("{} on {}", tap.info.guest, tap.info.name),
        };
        log(format!(
            "{name}: started, CID {}, {net}, logs in {}",
            res.cid,
            paths.logs.display()
        ));
        if let Ok(emitter) = self.emitter() {
            let _ = Gerant::vm_started(&emitter, info.clone()).await;
        }
        let this = Arc::clone(self);
        let name = name.to_owned();
        tokio::spawn(async move { this.supervise(name, launched, paths, rx).await });
        Ok(info)
    }

    /// Starts the helpers and crosvm and waits for crosvm's control socket.
    async fn launch(
        &self,
        name: &str,
        config: &VmConfig,
        res: &Resources,
        paths: &Paths,
    ) -> Result<Launched, Error> {
        let failed = |what: &str, e: io::Error| Error::StartFailed(format!("{what}: {e}"));
        // sockaddr_un's sun_path is 108 bytes, including the terminating NUL.
        for socket in [
            paths.control_socket(),
            paths.sound_socket(),
            paths.passt_socket(),
            paths.videur_socket(),
        ] {
            if socket.as_os_str().len() >= 108 {
                return Err(Error::StartFailed(format!(
                    "{}: socket path too long",
                    socket.display()
                )));
            }
        }
        // Sockets from an earlier run that didn't clean up would make us
        // think crosvm is up.
        match fs::remove_dir_all(&paths.runtime) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(&format!("removing {}", paths.runtime.display()), e)),
        }
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&paths.runtime)
            .map_err(|e| failed(&format!("creating {}", paths.runtime.display()), e))?;
        fs::create_dir_all(&paths.logs)
            .map_err(|e| failed(&format!("creating {}", paths.logs.display()), e))?;

        let mut helpers = Vec::new();
        let result = self
            .launch_inner(name, config, res, paths, &mut helpers)
            .await;
        if result.is_err() {
            stop_helpers(name, &mut helpers).await;
        }
        result.map(|(crosvm, args)| Launched {
            crosvm,
            args,
            net: res.net.clone(),
            helpers,
        })
    }

    async fn launch_inner(
        &self,
        name: &str,
        config: &VmConfig,
        res: &Resources,
        paths: &Paths,
        helpers: &mut Vec<(&'static str, Child)>,
    ) -> Result<(Child, Vec<OsString>), Error> {
        let failed = |e: io::Error| Error::StartFailed(e.to_string());
        // Truncated once per start; crosvm appends after a reboot.
        let truncate = |log: &str| File::create(paths.log(log)).map(drop).map_err(failed);

        if let Some(Net::Passt) = res.net {
            truncate("passt")?;
            let log = paths.log("passt");
            helpers.push((
                "passt",
                vm::spawn("passt", &vm::passt_args(paths), &[], &log, &log, None)
                    .map_err(failed)?,
            ));
        }
        if config.audio {
            truncate("vhost-device-sound")?;
            let log = paths.log("vhost-device-sound");
            let props = [vm::sound_props(name, config)];
            let child = vm::spawn(
                "vhost-device-sound",
                &vm::sound_args(config, paths),
                &props,
                &log,
                &log,
                None,
            )
            .map_err(failed)?;
            helpers.push(("vhost-device-sound", child));
        }
        // Without AppMenu, carte still keeps the list of the guest's
        // applications, which Salle shows.
        if config.app_menu || config.wayland {
            if vm::in_path("carte") {
                truncate("carte")?;
                let log = paths.log("carte");
                // Waits for the guest's carte-agent, and reconnects if it restarts.
                let mut args: Vec<OsString> =
                    vec!["sync".into(), "--cid".into(), res.cid.to_string().into()];
                if !config.app_menu {
                    args.push("--no-menu".into());
                }
                helpers.push((
                    "carte",
                    vm::spawn("carte", &args, &[], &log, &log, None).map_err(failed)?,
                ));
            } else {
                log(format!(
                    "{name}: carte isn't installed, the guest's applications won't be in the menu"
                ));
            }
        }
        if config.notifications {
            if vm::in_path("sonnette") {
                truncate("sonnette")?;
                let log = paths.log("sonnette");
                // Waits for the guest's sonnette-agent, and reconnects if it restarts.
                let args = ["relay".into(), "--cid".into(), res.cid.to_string().into()];
                helpers.push((
                    "sonnette",
                    vm::spawn("sonnette", &args, &[], &log, &log, None).map_err(failed)?,
                ));
            } else {
                log(format!(
                    "{name}: sonnette isn't installed, the guest's notifications won't be shown"
                ));
            }
        }

        if config.wayland && config.clipboard != Clipboard::Both {
            truncate("videur")?;
            let log = paths.log("videur");
            let mut child = vm::spawn(
                "videur",
                &vm::videur_args(config, paths),
                &[],
                &log,
                &log,
                None,
            )
            .map_err(failed)?;
            // crosvm connects to it when the guest's sommelier starts, but
            // a missing socket would only show up then.
            let socket = paths.videur_socket();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !socket.exists() {
                if child.try_wait().map_err(failed)?.is_some() || Instant::now() > deadline {
                    let _ = child.kill().await;
                    return Err(Error::StartFailed(format!(
                        "videur didn't start, end of {}:\n{}",
                        log.display(),
                        vm::log_tail(&log, 5)
                    )));
                }
                sleep(Duration::from_millis(20)).await;
            }
            helpers.push(("videur", child));
        }

        truncate("console")?;
        truncate("crosvm")?;
        let args = vm::crosvm_args(
            name,
            config,
            res,
            paths,
            unsafe { libc::getuid() },
            unsafe { libc::getgid() },
        );
        let tap_fd = res.net.as_ref().and_then(Net::tap_fd);
        let mut crosvm = vm::spawn(
            "crosvm",
            &args,
            &[],
            &paths.log("console"),
            &paths.log("crosvm"),
            tap_fd,
        )
        .map_err(failed)?;

        let socket = paths.control_socket();
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Some(status) = crosvm.try_wait().map_err(failed)? {
                let tail = vm::log_tail(&paths.log("crosvm"), 5);
                return Err(Error::StartFailed(format!(
                    "{}, end of {}:\n{tail}",
                    describe(&Ok(status)),
                    paths.log("crosvm").display()
                )));
            }
            if socket.exists() {
                return Ok((crosvm, args));
            }
            if Instant::now() > deadline {
                let _ = crosvm.kill().await;
                return Err(Error::StartFailed(format!(
                    "crosvm didn't create its control socket in {}s, see {}",
                    START_TIMEOUT.as_secs(),
                    paths.log("crosvm").display()
                )));
            }
            sleep(Duration::from_millis(50)).await;
        }
    }

    /// Follows a VM until crosvm exits for good, then cleans up.
    async fn supervise(
        self: Arc<Self>,
        name: String,
        mut launched: Launched,
        paths: Paths,
        mut commands: mpsc::UnboundedReceiver<Command>,
    ) {
        enum Next {
            CrosvmStop,
            Kill,
        }
        let mut deadline: Option<(Instant, Next)> = None;
        let mut stopping = false;
        // crosvm exits with 0 both when the guest powers off and after
        // `crosvm stop`.
        let mut stopped_crosvm = false;
        let mut resets: Vec<Instant> = Vec::new();
        let mut boot_deadline = self
            .boot_notifications
            .then(|| Instant::now() + BOOT_TIMEOUT);
        let reason = loop {
            let timer = async {
                match &deadline {
                    Some((at, _)) => sleep_until(*at).await,
                    None => std::future::pending().await,
                }
            };
            let boot_timer = async {
                match boot_deadline {
                    Some(at) => sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                status = launched.crosvm.wait() => {
                    let reset = matches!(&status, Ok(s) if s.code() == Some(vm::EXIT_RESET));
                    if stopped_crosvm && matches!(&status, Ok(s) if s.success()) {
                        break "crosvm was stopped".to_owned();
                    }
                    if !reset || stopping {
                        break describe(&status);
                    }
                    let now = Instant::now();
                    resets.retain(|t| now.duration_since(*t) < RESET_WINDOW);
                    resets.push(now);
                    if resets.len() > MAX_RESETS {
                        break format!("the guest rebooted more than {MAX_RESETS} times in {}s", RESET_WINDOW.as_secs());
                    }
                    log(format!("{name}: the guest rebooted, starting crosvm again"));
                    // The helpers stay: passt and vhost-device-sound serve
                    // the next connection, and carte and sonnette reconnect.
                    let _ = fs::remove_file(paths.control_socket());
                    let tap_fd = launched.net.as_ref().and_then(Net::tap_fd);
                    match vm::spawn("crosvm", &launched.args, &[], &paths.log("console"), &paths.log("crosvm"), tap_fd) {
                        Ok(child) => {
                            launched.crosvm = child;
                            self.set_state(&name, self.booting_state(), launched.crosvm.id()).await;
                            boot_deadline = self.boot_notifications.then(|| Instant::now() + BOOT_TIMEOUT);
                        }
                        Err(e) => break format!("the guest rebooted, but {e}"),
                    }
                }
                Some(command) = commands.recv() => match command {
                    Command::Stop if !stopping => {
                        stopping = true;
                        self.set_state(&name, "stopping", None).await;
                        log(format!("{name}: pressing the power button"));
                        control(&name, "powerbtn", &paths).await;
                        deadline = Some((Instant::now() + self.stop_timeout, Next::CrosvmStop));
                    }
                    Command::Stop => {}
                    Command::Kill => {
                        stopping = true;
                        stopped_crosvm = true;
                        self.set_state(&name, "stopping", None).await;
                        control(&name, "stop", &paths).await;
                        deadline = Some((Instant::now() + EXIT_TIMEOUT, Next::Kill));
                    }
                },
                () = timer => match deadline.take() {
                    Some((_, Next::CrosvmStop)) => {
                        log(format!("{name}: the guest didn't shut down in {}s, stopping crosvm", self.stop_timeout.as_secs()));
                        stopped_crosvm = true;
                        control(&name, "stop", &paths).await;
                        deadline = Some((Instant::now() + EXIT_TIMEOUT, Next::Kill));
                    }
                    Some((_, Next::Kill)) => {
                        log(format!("{name}: killing crosvm"));
                        let _ = launched.crosvm.start_kill();
                    }
                    None => {}
                },
                () = boot_timer => {
                    boot_deadline = None;
                    if self.finish_boot(&name).await {
                        log(format!(
                            "{name}: the guest didn't say it booted in {}s (no gerant-ready.service?), \
                             showing it as running",
                            BOOT_TIMEOUT.as_secs()
                        ));
                    }
                }
            }
        };
        stop_helpers(&name, &mut launched.helpers).await;
        log(format!("{name}: stopped: {reason}"));
        self.release(&name);
        self.state_changed(VmInfo::stopped(&name)).await;
        if let Ok(emitter) = self.emitter() {
            let _ = Gerant::vm_stopped(&emitter, &name, &reason).await;
        }
    }

    /// Forgets a VM and gives its resources back.
    fn release(&self, name: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(vm) = inner.vms.remove(name) {
            // The tap goes once the supervisor drops its copy too.
            inner.pool.free_cid(vm.res.cid);
            let _ = fs::remove_dir_all(&vm.paths.runtime);
        }
        drop(inner);
        self.removed.notify_waiters();
    }

    fn send(&self, name: &str, command: Command) -> Result<(), Error> {
        let inner = self.inner.lock().unwrap();
        let vm = inner
            .vms
            .get(name)
            .ok_or_else(|| Error::NotRunning(format!("{name} isn't running")))?;
        // Only fails once the VM is gone, which `release` is about to record.
        let _ = vm.commands.send(command);
        Ok(())
    }

    fn list(&self) -> Vec<VmInfo> {
        let mut vms: BTreeMap<String, VmInfo> = BTreeMap::new();
        if let Ok(dir) = fs::read_dir(self.vm_dir()) {
            for entry in dir.flatten() {
                let file = entry.file_name();
                let Some(name) = file.to_str().and_then(|f| f.strip_suffix(".conf")) else {
                    continue;
                };
                if config::is_valid_name(name) && entry.path().is_file() {
                    vms.insert(name.to_owned(), VmInfo::stopped(name));
                }
            }
        }
        for (name, vm) in &self.inner.lock().unwrap().vms {
            vms.insert(name.clone(), vm.info.clone());
        }
        vms.into_values().collect()
    }

    /// Stops crosvm for every VM, without waiting for the guests.
    pub fn kill_all(&self) {
        let names: Vec<String> = self.inner.lock().unwrap().vms.keys().cloned().collect();
        for name in names {
            let _ = self.send(&name, Command::Kill);
        }
    }

    /// Stops every VM and waits until they're gone. No VM can be started
    /// afterwards.
    pub async fn shutdown(&self) {
        let names: Vec<String> = {
            let mut inner = self.inner.lock().unwrap();
            inner.shutting_down = true;
            inner.vms.keys().cloned().collect()
        };
        for name in names {
            let _ = self.send(&name, Command::Stop);
        }
        loop {
            let removed = self.removed.notified();
            tokio::pin!(removed);
            removed.as_mut().enable();
            if self.inner.lock().unwrap().vms.is_empty() {
                return;
            }
            removed.await;
        }
    }
}

/// The object at /org/garnish/Gerant.
pub struct Gerant(pub Arc<Manager>);

#[interface(name = "org.garnish.Gerant1")]
impl Gerant {
    // A one-element tuple, so the reply is the struct the introspection
    // data promises, `(ssusssus)`; a bare struct would go out as its fields.
    async fn start_vm(&self, name: String) -> Result<(VmInfo,), Error> {
        self.0.start(&name).await.map(|info| (info,))
    }

    async fn stop_vm(&self, name: String) -> Result<(), Error> {
        self.0.send(&name, Command::Stop)
    }

    async fn kill_vm(&self, name: String) -> Result<(), Error> {
        self.0.send(&name, Command::Kill)
    }

    async fn list_vms(&self) -> Vec<VmInfo> {
        self.0.list()
    }

    #[zbus(signal)]
    async fn vm_started(emitter: &SignalEmitter<'_>, info: VmInfo) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn vm_stopped(emitter: &SignalEmitter<'_>, name: &str, reason: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn vm_state_changed(emitter: &SignalEmitter<'_>, info: VmInfo) -> zbus::Result<()>;
}
