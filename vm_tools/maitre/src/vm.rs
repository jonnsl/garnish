//! Command lines for crosvm and the programs that run next to it.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use tokio::process::{Child, Command};

use crate::config::VmConfig;
use crate::net::{self, Tap};

/// crosvm's exit status (with --extended-status) when the guest reboots.
pub const EXIT_RESET: i32 = 32;

/// What a running VM was given.
#[derive(Debug, Clone)]
pub struct Resources {
    pub cid: u32,
    pub net: Option<Net>,
    pub dns: Vec<Ipv4Addr>,
}

/// A VM's network.
#[derive(Debug, Clone)]
pub enum Net {
    /// passt, a helper that serves the device over vhost-user at
    /// `Paths::passt_socket`.
    Passt,
    /// A tap from maitre-net. Shared with crosvm, which gets the
    /// descriptor; the tap goes away once the last copy is dropped and
    /// crosvm has exited.
    Tap(Arc<Tap>),
}

impl Net {
    /// The guest's address, its gateway and the netmask.
    pub fn addresses(&self) -> (Ipv4Addr, Ipv4Addr, Ipv4Addr) {
        match self {
            Net::Passt => (net::PASST_GUEST, net::PASST_GATEWAY, net::PASST_NETMASK),
            Net::Tap(tap) => (tap.info.guest, tap.info.host, tap.info.netmask()),
        }
    }

    /// The descriptor crosvm has to inherit.
    pub fn tap_fd(&self) -> Option<RawFd> {
        match self {
            Net::Passt => None,
            Net::Tap(tap) => Some(tap.fd.as_raw_fd()),
        }
    }
}

/// Where a VM's files go.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Sockets: $XDG_RUNTIME_DIR/garnish/<name>.
    pub runtime: PathBuf,
    /// Logs: $XDG_STATE_HOME/garnish/<name>.
    pub logs: PathBuf,
    /// The host's Wayland socket.
    pub wayland: PathBuf,
}

impl Paths {
    pub fn control_socket(&self) -> PathBuf {
        self.runtime.join("crosvm.sock")
    }

    pub fn sound_socket(&self) -> PathBuf {
        self.runtime.join("snd.sock")
    }

    pub fn passt_socket(&self) -> PathBuf {
        self.runtime.join("passt.sock")
    }

    pub fn log(&self, name: &str) -> PathBuf {
        self.logs.join(format!("{name}.log"))
    }
}

/// The kernel command line for the VM called `name`.
pub fn kernel_cmdline(name: &str, config: &VmConfig, res: &Resources) -> String {
    // The guest is named after its configuration file. sommelier and
    // carte-agent use the hostname as the VM's name, so windows and menu
    // entries can be told apart.
    let mut params: Vec<String> = vec![config.cmdline.clone(), format!("systemd.hostname={name}")];
    if let Some(net) = &res.net {
        // systemd-network-generator turns this into a .network file that
        // takes precedence over the image's static one. It needs the
        // interface name, hence net.ifnames=0.
        let dns: Vec<String> = res.dns.iter().take(2).map(|a| a.to_string()).collect();
        let (guest, gateway, netmask) = net.addresses();
        let mut ip = format!("ip={guest}::{gateway}:{netmask}::eth0:off");
        if !dns.is_empty() {
            ip = format!("{ip}:{}", dns.join(":"));
        }
        params.push(ip);
        params.push("net.ifnames=0".into());
    }
    if config.shared_home.is_some() {
        // nofail: if the mount fails, the guest still boots with the home
        // from the image.
        params.push(format!(
            "systemd.mount-extra=home:/home/{}:virtiofs:nofail,x-systemd.before=systemd-user-sessions.service",
            config.user
        ));
    }
    params.extend(config.cmdline_extra.iter().cloned());
    params.join(" ")
}

/// `crosvm` and its arguments for the VM called `name`. `host_uid`/`host_gid`
/// own the shared home.
pub fn crosvm_args(name: &str, config: &VmConfig, res: &Resources, paths: &Paths, host_uid: u32, host_gid: u32) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut arg = |a: &str| args.push(a.into());
    arg("--extended-status");
    arg("run");
    arg("--cpus");
    arg(&config.cpus.to_string());
    arg("--mem");
    arg(&config.memory_mib.to_string());
    arg("--socket");
    args.push(paths.control_socket().into());
    args.push("--block".into());
    args.push(prefixed("path=", &config.image));
    if let Some(initrd) = &config.initrd {
        args.push("--initrd".into());
        args.push(initrd.into());
    }
    args.push("--params".into());
    args.push(kernel_cmdline(name, config, res).into());
    args.push("--vsock".into());
    args.push(format!("cid={}", res.cid).into());
    if config.wayland {
        // sommelier --virtgpu-channel talks to the host through the GPU's
        // cross-domain context.
        args.push("--gpu".into());
        args.push(format!("context-types={}", config.gpu_context_types.join(":")).into());
        args.push("--wayland-sock".into());
        args.push(paths.wayland.clone().into());
    }
    // crosvm waits up to --vhost-user-connect-timeout-ms for the sockets
    // of vhost-user devices to appear.
    let mut vhost_user = false;
    match &res.net {
        Some(Net::Tap(tap)) => {
            // `spawn` keeps the descriptor open across exec.
            args.push("--net".into());
            args.push(format!("tap-fd={}", tap.fd.as_raw_fd()).into());
        }
        Some(Net::Passt) => {
            // crosvm offers the guest 32768 entries per queue unless told
            // otherwise. passt is built for at most 1024 (QEMU's maximum)
            // and dies on an assertion once a TCP download fills 1024
            // receive buffers (passt 2026_10_02). 256 is QEMU's default.
            args.push("--vhost-user".into());
            args.push(prefixed("type=net,max-queue-size=256,socket=", &paths.passt_socket()));
            vhost_user = true;
        }
        None => {}
    }
    if let Some(home) = &config.shared_home {
        // The device runs in a user namespace where only the guest user's
        // ids exist, mapped to ours. Files the guest user creates are owned
        // by us on the host. Other host owners show up as nobody, and guest
        // root can't create files there.
        let (uid, gid) = (config.uid, config.gid);
        let mut spec = home.as_os_str().to_owned();
        spec.push(format!(
            ":home:type=fs:cache=auto:uidmap={uid} {host_uid} 1:gidmap={gid} {host_gid} 1:uid={uid}:gid={gid}"
        ));
        args.push("--shared-dir".into());
        args.push(spec);
    }
    if config.audio {
        // vhost-device-sound takes at most 64 entries per queue and exits on
        // anything bigger, but crosvm offers the guest its own maximum
        // unless told otherwise.
        args.push("--vhost-user".into());
        args.push(prefixed("type=sound,max-queue-size=64,socket=", &paths.sound_socket()));
        vhost_user = true;
    }
    if vhost_user {
        args.push("--vhost-user-connect-timeout-ms".into());
        args.push("5000".into());
    }
    args.extend(config.extra_args.iter().map(OsString::from));
    args.push(config.kernel.clone().into());
    args
}

/// passt's arguments. It stays up while crosvm restarts after a guest
/// reboot, and serves the next connection.
pub fn passt_args(paths: &Paths) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--foreground".into(), "--vhost-user".into(), "--socket".into()];
    args.push(paths.passt_socket().into());
    for a in [
        // The same addresses for every VM: each has a passt of its own. The
        // guest gets them from maitred on the kernel command line.
        "--address",
        &net::PASST_GUEST.to_string(),
        "--netmask",
        &net::PASST_NETMASK.to_string(),
        "--gateway",
        &net::PASST_GATEWAY.to_string(),
        // By default the gateway address leads to the host's loopback, so
        // the guest could reach services that listen only there.
        "--no-map-gw",
    ] {
        args.push(a.into());
    }
    args
}

pub fn sound_args(config: &VmConfig, paths: &Paths) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["--socket".into(), paths.sound_socket().into(), "--backend".into(), "pipewire".into()];
    // The patched vhost-device-sound in host/vhost-device-sound: no input
    // stream, so the guest can't record from the microphone.
    if !config.microphone {
        args.push("--no-capture".into());
    }
    args
}

/// The `PIPEWIRE_PROPS` that vhost-device-sound's streams get, so the host's
/// mixers can tell VMs apart. WirePlumber remembers a stream's volume by
/// `application.id`, so each VM keeps its own volume, even though the stream
/// is made again whenever the guest reopens its sound card. Salle finds the
/// stream by `node.name`.
pub fn sound_props(name: &str, config: &VmConfig) -> (&'static str, String) {
    let title = config.title.as_deref().unwrap_or(name);
    let props = format!(
        "{{ application.id={} application.name={} application.icon-name=computer node.name={} }}",
        spa_string(&format!("org.garnish.vm.{name}")),
        spa_string(title),
        spa_string(&format!("garnish.{name}")),
    );
    ("PIPEWIRE_PROPS", props)
}

/// `s` as a quoted SPA JSON string.
fn spa_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn prefixed(prefix: &str, path: &Path) -> OsString {
    let mut s = OsString::from(prefix);
    s.push(path);
    s
}

/// Starts `program` with `env` added to its environment and stdout and stderr
/// appended to `log`, in its own process group so a Ctrl-C meant for the
/// daemon doesn't reach it. `keep` stays open in it, with the same number;
/// our other descriptors are close-on-exec.
pub fn spawn(
    program: &str,
    args: &[OsString],
    env: &[(&str, String)],
    stdout: &Path,
    stderr: &Path,
    keep: Option<RawFd>,
) -> io::Result<Child> {
    let out = File::create(stdout)?;
    let err = if stderr == stdout { out.try_clone()? } else { File::create(stderr)? };
    let mut cmd = Command::new(program);
    cmd.args(args).envs(env.iter().cloned()).stdin(Stdio::null()).stdout(out).stderr(err).process_group(0);
    // If the daemon dies, take the VM down with it rather than leaving it
    // behind with resources nobody tracks. The runtime is single-threaded,
    // so the thread that forks is the one that lives as long as the daemon.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            if let Some(fd) = keep
                && libc::fcntl(fd, libc::F_SETFD, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| io::Error::new(e.kind(), format!("starting {program}: {e}")))
}

/// Sends SIGTERM to `child`.
pub fn terminate(child: &Child) {
    if let Some(pid) = child.id() {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
}

/// Whether `program` is in $PATH.
pub fn in_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// The last `n` lines of a log, for error messages.
pub fn log_tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ini;

    fn config(text: &str) -> VmConfig {
        let entries = ini::parse(text, Path::new("/cfg/debian.conf")).unwrap();
        VmConfig::from_entries(&entries, "me", Path::new("/home/me")).unwrap()
    }

    fn net() -> Resources {
        // Any descriptor will do.
        let fd = File::open("/dev/null").unwrap().into();
        Resources {
            cid: 7,
            net: Some(Net::Tap(Arc::new(Tap { info: crate::net::NetConfig::default().slot(1), fd }))),
            dns: vec![Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(9, 9, 9, 9)],
        }
    }

    fn paths() -> Paths {
        Paths {
            runtime: "/run/user/1000/garnish/debian".into(),
            logs: "/home/me/.local/state/garnish/debian".into(),
            wayland: "/run/user/1000/wayland-0".into(),
        }
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn cmdline() {
        let c = config("[Boot]\nImage=d.raw\nKernelCommandLineExtra=quiet\n[Machine]\nSharedHome=/h\n");
        assert_eq!(
            kernel_cmdline("debian", &c, &net()),
            format!(
                "{} systemd.hostname=debian ip=192.168.10.6::192.168.10.5:255.255.255.252::eth0:off:1.1.1.1:9.9.9.9 net.ifnames=0 \
                 systemd.mount-extra=home:/home/me:virtiofs:nofail,x-systemd.before=systemd-user-sessions.service quiet",
                crate::config::DEFAULT_CMDLINE
            )
        );
        let no_net = Resources { cid: 3, net: None, dns: vec![] };
        assert_eq!(kernel_cmdline("debian", &c, &no_net).matches("ip=").count(), 0);
    }

    #[test]
    fn crosvm() {
        let c = config("[Boot]\nImage=d.raw\nInitrd=d.initrd\n[Machine]\nCPUs=2\nMemory=2G\nAudio=yes\nSharedHome=/h\nExtraArguments=--x 'y z'\n");
        let res = net();
        let args = strings(crosvm_args("debian", &c, &res, &paths(), 1001, 1002));
        let cmdline = kernel_cmdline("debian", &c, &res);
        let tap_fd = format!("tap-fd={}", res.net.as_ref().unwrap().tap_fd().unwrap());
        let expected = [
            "--extended-status",
            "run",
            "--cpus",
            "2",
            "--mem",
            "2048",
            "--socket",
            "/run/user/1000/garnish/debian/crosvm.sock",
            "--block",
            "path=/cfg/d.raw",
            "--initrd",
            "/cfg/d.initrd",
            "--params",
            &cmdline,
            "--vsock",
            "cid=7",
            "--gpu",
            "context-types=cross-domain",
            "--wayland-sock",
            "/run/user/1000/wayland-0",
            "--net",
            &tap_fd,
            "--shared-dir",
            "/h:home:type=fs:cache=auto:uidmap=1000 1001 1:gidmap=1000 1002 1:uid=1000:gid=1000",
            "--vhost-user",
            "type=sound,max-queue-size=64,socket=/run/user/1000/garnish/debian/snd.sock",
            "--vhost-user-connect-timeout-ms",
            "5000",
            "--x",
            "y z",
            "/cfg/d.vmlinuz",
        ];
        assert_eq!(args, expected);
    }

    #[test]
    fn minimal() {
        let c = config("[Boot]\nImage=d.raw\nInitrd=\n[Machine]\nWayland=no\nGpuContextTypes=\n");
        let res = Resources { cid: 3, net: None, dns: vec![] };
        let args = strings(crosvm_args("debian", &c, &res, &paths(), 1000, 1000));
        for absent in ["--gpu", "--wayland-sock", "--net", "--shared-dir", "--vhost-user"] {
            assert!(!args.iter().any(|a| a == absent), "{absent} in {args:?}");
        }
    }

    #[test]
    fn passt() {
        let c = config("[Boot]\nImage=d.raw\n[Machine]\nWayland=no\n");
        let res = Resources { cid: 3, net: Some(Net::Passt), dns: vec![Ipv4Addr::new(1, 1, 1, 1)] };
        assert!(kernel_cmdline("debian", &c, &res).contains(" ip=10.0.2.15::10.0.2.2:255.255.255.0::eth0:off:1.1.1.1 "));
        let args = strings(crosvm_args("debian", &c, &res, &paths(), 1000, 1000));
        let tail = &args[args.len() - 5..];
        assert_eq!(tail, [
            "--vhost-user",
            "type=net,max-queue-size=256,socket=/run/user/1000/garnish/debian/passt.sock",
            "--vhost-user-connect-timeout-ms",
            "5000",
            "/cfg/d.vmlinuz"
        ]);
        assert!(!args.iter().any(|a| a == "--net"));
        assert_eq!(strings(passt_args(&paths())), [
            "--foreground",
            "--vhost-user",
            "--socket",
            "/run/user/1000/garnish/debian/passt.sock",
            "--address",
            "10.0.2.15",
            "--netmask",
            "255.255.255.0",
            "--gateway",
            "10.0.2.2",
            "--no-map-gw",
        ]);
        // With sound too, the timeout comes once.
        let c = config("[Boot]\nImage=d.raw\n[Machine]\nAudio=yes\n");
        let args = strings(crosvm_args("debian", &c, &res, &paths(), 1000, 1000));
        assert_eq!(args.iter().filter(|a| *a == "--vhost-user").count(), 2);
        assert_eq!(args.iter().filter(|a| *a == "--vhost-user-connect-timeout-ms").count(), 1);
    }

    #[test]
    fn sound() {
        let c = config("[Boot]\nImage=d.raw\n");
        assert_eq!(
            strings(sound_args(&c, &paths())),
            ["--socket", "/run/user/1000/garnish/debian/snd.sock", "--backend", "pipewire", "--no-capture"]
        );
    }

    #[test]
    fn sound_props() {
        let c = config("[Boot]\nImage=d.raw\n");
        assert_eq!(
            super::sound_props("debian", &c).1,
            "{ application.id=\"org.garnish.vm.debian\" application.name=\"debian\" \
             application.icon-name=computer node.name=\"garnish.debian\" }"
        );
        let c = config("[Boot]\nImage=d.raw\n[Machine]\nTitle=Débian \"work\" \\ play\n");
        assert!(super::sound_props("debian", &c).1.contains(r#"application.name="Débian \"work\" \\ play""#));
    }
}
