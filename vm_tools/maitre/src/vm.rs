//! Command lines for crosvm and the programs that run next to it.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::config::VmConfig;
use crate::pool::NetSlot;

/// crosvm's exit status (with --extended-status) when the guest reboots.
pub const EXIT_RESET: i32 = 32;

/// What a running VM was given.
#[derive(Debug, Clone)]
pub struct Resources {
    pub cid: u32,
    pub net: Option<NetSlot>,
    pub dns: Vec<Ipv4Addr>,
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

    pub fn log(&self, name: &str) -> PathBuf {
        self.logs.join(format!("{name}.log"))
    }
}

pub fn kernel_cmdline(config: &VmConfig, res: &Resources) -> String {
    let mut params: Vec<String> = vec![config.cmdline.clone()];
    if let Some(net) = &res.net {
        // systemd-network-generator turns this into a .network file that
        // takes precedence over the image's static one. It needs the
        // interface name, hence net.ifnames=0.
        let dns: Vec<String> = res.dns.iter().take(2).map(|a| a.to_string()).collect();
        let mut ip = format!("ip={}::{}:{}::eth0:off", net.guest, net.host, net.netmask());
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

/// `crosvm` and its arguments. `host_uid`/`host_gid` own the shared home.
pub fn crosvm_args(config: &VmConfig, res: &Resources, paths: &Paths, host_uid: u32, host_gid: u32) -> Vec<OsString> {
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
    args.push(kernel_cmdline(config, res).into());
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
    if let Some(net) = &res.net {
        args.push("--net".into());
        args.push(format!("tap-name={}", net.tap).into());
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
        // unless told otherwise. crosvm waits up to the timeout for the
        // socket to appear.
        args.push("--vhost-user".into());
        args.push(prefixed("type=sound,max-queue-size=64,socket=", &paths.sound_socket()));
        args.push("--vhost-user-connect-timeout-ms".into());
        args.push("5000".into());
    }
    args.extend(config.extra_args.iter().map(OsString::from));
    args.push(config.kernel.clone().into());
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

fn prefixed(prefix: &str, path: &Path) -> OsString {
    let mut s = OsString::from(prefix);
    s.push(path);
    s
}

/// Starts `program` with stdout and stderr appended to `log`, in its own
/// process group so a Ctrl-C meant for the daemon doesn't reach it.
pub fn spawn(program: &str, args: &[OsString], stdout: &Path, stderr: &Path) -> io::Result<Child> {
    let out = File::create(stdout)?;
    let err = if stderr == stdout { out.try_clone()? } else { File::create(stderr)? };
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(out).stderr(err).process_group(0);
    // If the daemon dies, take the VM down with it rather than leaving it
    // behind with resources nobody tracks. The runtime is single-threaded,
    // so the thread that forks is the one that lives as long as the daemon.
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
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
        Resources {
            cid: 7,
            net: Some(NetSlot {
                index: 1,
                tap: "crosvm_tap1".into(),
                host: Ipv4Addr::new(192, 168, 10, 5),
                guest: Ipv4Addr::new(192, 168, 10, 6),
            }),
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
            kernel_cmdline(&c, &net()),
            format!(
                "{} ip=192.168.10.6::192.168.10.5:255.255.255.252::eth0:off:1.1.1.1:9.9.9.9 net.ifnames=0 \
                 systemd.mount-extra=home:/home/me:virtiofs:nofail,x-systemd.before=systemd-user-sessions.service quiet",
                crate::config::DEFAULT_CMDLINE
            )
        );
        let no_net = Resources { cid: 3, net: None, dns: vec![] };
        assert_eq!(kernel_cmdline(&c, &no_net).matches("ip=").count(), 0);
    }

    #[test]
    fn crosvm() {
        let c = config("[Boot]\nImage=d.raw\nInitrd=d.initrd\n[Machine]\nCPUs=2\nMemory=2G\nAudio=yes\nSharedHome=/h\nExtraArguments=--x 'y z'\n");
        let args = strings(crosvm_args(&c, &net(), &paths(), 1001, 1002));
        let cmdline = kernel_cmdline(&c, &net());
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
            "tap-name=crosvm_tap1",
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
        let args = strings(crosvm_args(&c, &res, &paths(), 1000, 1000));
        for absent in ["--gpu", "--wayland-sock", "--net", "--shared-dir", "--vhost-user"] {
            assert!(!args.iter().any(|a| a == absent), "{absent} in {args:?}");
        }
    }

    #[test]
    fn sound() {
        let c = config("[Boot]\nImage=d.raw\n");
        assert_eq!(
            strings(sound_args(&c, &paths())),
            ["--socket", "/run/user/1000/garnish/debian/snd.sock", "--backend", "pipewire", "--no-capture"]
        );
    }
}
