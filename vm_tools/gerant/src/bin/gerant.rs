//! gerant: command line client for gerantd.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use futures_util::StreamExt;
use zbus::Connection;

use gerant::dbus::{GerantProxy, VmInfo};
use gerant::xdg;

const USAGE: &str = "\
usage: gerant start <name>              start the VM in ~/.config/garnish/vms/<name>.conf
       gerant stop [--force] <name>     shut a VM down and wait until it's off;
                                        --force stops crosvm without asking the guest
       gerant list                      the configured VMs and what the running ones got
       gerant ssh <name> [command...]   ssh into a running VM over vsock
       gerant console <name>            attach to a running VM's serial console;
                                        Ctrl-] detaches";

/// Ctrl-], as in telnet and virsh.
const DETACH: u8 = 0x1d;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["start", name] => start(name).await,
        ["stop", name] => stop(name, false).await,
        ["stop", "--force", name] | ["stop", name, "--force"] => stop(name, true).await,
        ["list"] => list().await,
        ["ssh", name, ref command @ ..] => ssh(name, command).await,
        ["console", name] => console(name).await,
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gerant: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The message of a D-Bus error, without the error name when there is one.
fn message(e: zbus::Error) -> String {
    match e {
        // The bus couldn't start gerantd.
        zbus::Error::MethodError(name, Some(msg), _)
            if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" =>
        {
            format!(
                "{msg}\nInstall data/org.garnish.Gerant.service to ~/.local/share/dbus-1/services/ and \
                 data/gerantd.service to ~/.config/systemd/user/, then run `systemctl --user daemon-reload` and \
                 `busctl --user call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus ReloadConfig`"
            )
        }
        zbus::Error::MethodError(_, Some(msg), _) => msg,
        e => e.to_string(),
    }
}

async fn proxy() -> Result<GerantProxy<'static>, String> {
    let conn = Connection::session()
        .await
        .map_err(|e| format!("connecting to the session bus: {e}"))?;
    GerantProxy::new(&conn).await.map_err(message)
}

fn describe(info: &VmInfo) -> String {
    let net = if info.address.is_empty() {
        "no network".to_owned()
    } else if info.address6.is_empty() {
        format!("{} on {}", info.address, info.tap)
    } else {
        format!("{} and {} on {}", info.address, info.address6, info.tap)
    };
    format!("CID {}, {net}", info.cid)
}

async fn start(name: &str) -> Result<(), String> {
    let info = proxy().await?.start_vm(name).await.map_err(message)?;
    println!("{name}: {}", describe(&info));
    println!("logs: {}", info.log_dir);
    println!("ssh: gerant ssh {name}");
    Ok(())
}

async fn stop(name: &str, force: bool) -> Result<(), String> {
    let proxy = proxy().await?;
    // Subscribe first, so the signal can't come before we listen.
    let mut stopped = proxy.receive_vm_stopped().await.map_err(message)?;
    if force {
        proxy.kill_vm(name).await
    } else {
        proxy.stop_vm(name).await
    }
    .map_err(message)?;
    while let Some(signal) = stopped.next().await {
        let args = signal.args().map_err(message)?;
        if args.name == name {
            println!("{name}: {}", args.reason);
            return Ok(());
        }
    }
    Err("lost the connection to gerantd".into())
}

async fn list() -> Result<(), String> {
    let vms = proxy().await?.list_vms().await.map_err(message)?;
    let width = vms.iter().map(|vm| vm.name.len()).max().unwrap_or(0).max(4);
    println!(
        "{:width$}  {:8}  {:>4}  {:15}  {:10}  IPV6",
        "NAME", "STATE", "CID", "ADDRESS", "TAP"
    );
    for vm in vms {
        let cid = if vm.cid == 0 {
            String::new()
        } else {
            vm.cid.to_string()
        };
        println!(
            "{:width$}  {:8}  {cid:>4}  {:15}  {:10}  {}",
            vm.name, vm.state, vm.address, vm.tap, vm.address6
        );
    }
    Ok(())
}

/// The running VM called `name`.
async fn running(name: &str) -> Result<VmInfo, String> {
    let vms = proxy().await?.list_vms().await.map_err(message)?;
    let vm = vms
        .into_iter()
        .find(|vm| vm.name == name)
        .ok_or_else(|| format!("no VM named {name}"))?;
    if vm.cid == 0 {
        return Err(format!("{name} isn't running"));
    }
    Ok(vm)
}

async fn ssh(name: &str, command: &[&str]) -> Result<(), String> {
    let vm = running(name).await?;
    // The vsock/<cid> host comes from systemd's ssh proxy (systemd >= 256),
    // which turns host key checking off. CIDs are shared by every user's VMs
    // and reused, so check the key against the VM's name instead: the first
    // connection records it, a different key later is refused. ssh takes the
    // first value it sees for an option, so these beat the proxy's config.
    let state = xdg::state_dir();
    std::fs::create_dir_all(&state).map_err(|e| format!("creating {}: {e}", state.display()))?;
    let known_hosts = state.join("known_hosts");
    let err = Command::new("ssh")
        .arg(format!("-oHostKeyAlias={name}"))
        .arg(format!(
            "-oUserKnownHostsFile=\"{}\"",
            known_hosts.display()
        ))
        .arg("-oStrictHostKeyChecking=accept-new")
        .arg(format!("{}@vsock/{}", vm.user, vm.cid))
        .args(command)
        .exec();
    Err(format!("running ssh: {err}"))
}

async fn console(name: &str) -> Result<(), String> {
    running(name).await?;
    let socket = gerant::vm::console_socket(&xdg::vm_runtime_dir(name));
    let mut stream = UnixStream::connect(&socket)
        .map_err(|e| format!("connecting to {}: {e}", socket.display()))?;
    let raw = RawMode::enter();
    if raw.is_some() {
        eprint!("{name}'s console, Ctrl-] detaches. Press Enter for a prompt.\r\n");
    }
    let result = relay(&mut stream, raw.is_some());
    drop(raw);
    match result {
        Ok(End::Detached) => Ok(()),
        Ok(End::Stopped) => {
            eprintln!("\n{name} stopped");
            Ok(())
        }
        Err(e) => Err(format!("console: {e}")),
    }
}

enum End {
    /// Ctrl-], or the end of stdin.
    Detached,
    Stopped,
}

/// Copies stdin to the console and the console to stdout, until Ctrl-]
/// (when `detach` is set), the end of stdin, or the VM stopping.
fn relay(stream: &mut UnixStream, detach: bool) -> io::Result<End> {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut buf = [0u8; 4096];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: stdin.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if fds[1].revents != 0 {
            let n = stream.read(&mut buf)?;
            if n == 0 {
                return Ok(End::Stopped);
            }
            stdout.write_all(&buf[..n])?;
            stdout.flush()?;
        }
        if fds[0].revents != 0 {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                return Ok(End::Detached);
            }
            let typed = &buf[..n];
            match typed.iter().position(|&b| detach && b == DETACH) {
                Some(i) => {
                    stream.write_all(&typed[..i])?;
                    return Ok(End::Detached);
                }
                None => stream.write_all(typed)?,
            }
        }
    }
}

/// The terminal on stdin in raw mode, so keys like Ctrl-C reach the guest;
/// restored when dropped.
struct RawMode(libc::termios);

impl RawMode {
    /// None if stdin isn't a terminal.
    fn enter() -> Option<RawMode> {
        let fd = libc::STDIN_FILENO;
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return None;
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } != 0 {
            return None;
        }
        Some(RawMode(saved))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &self.0) };
    }
}
