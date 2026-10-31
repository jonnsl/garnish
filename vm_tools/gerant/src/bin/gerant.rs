//! gerant: command line client for gerantd.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use futures_util::StreamExt;
use zbus::Connection;

use gerant::dbus::{GerantProxy, VmInfo};

const USAGE: &str = "\
usage: gerant start <name>              start the VM in ~/.config/garnish/vms/<name>.conf
       gerant stop [--force] <name>     shut a VM down and wait until it's off;
                                        --force stops crosvm without asking the guest
       gerant list                      the configured VMs and what the running ones got
       gerant ssh <name> [command...]   ssh into a running VM over vsock";

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
        zbus::Error::MethodError(name, Some(msg), _) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" => {
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
    let conn = Connection::session().await.map_err(|e| format!("connecting to the session bus: {e}"))?;
    GerantProxy::new(&conn).await.map_err(message)
}

fn describe(info: &VmInfo) -> String {
    let net = if info.address.is_empty() { "no network".to_owned() } else { format!("{} on {}", info.address, info.tap) };
    format!("CID {}, {net}", info.cid)
}

async fn start(name: &str) -> Result<(), String> {
    let info = proxy().await?.start_vm(name).await.map_err(message)?;
    println!("{name}: {}", describe(&info));
    println!("logs: {}", info.log_dir);
    println!("ssh: gerant ssh {name}  (or ssh {}@vsock/{})", info.user, info.cid);
    Ok(())
}

async fn stop(name: &str, force: bool) -> Result<(), String> {
    let proxy = proxy().await?;
    // Subscribe first, so the signal can't come before we listen.
    let mut stopped = proxy.receive_vm_stopped().await.map_err(message)?;
    if force { proxy.kill_vm(name).await } else { proxy.stop_vm(name).await }.map_err(message)?;
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
    println!("{:width$}  {:8}  {:>4}  {:15}  TAP", "NAME", "STATE", "CID", "ADDRESS");
    for vm in vms {
        let cid = if vm.cid == 0 { String::new() } else { vm.cid.to_string() };
        println!("{:width$}  {:8}  {cid:>4}  {:15}  {}", vm.name, vm.state, vm.address, vm.tap);
    }
    Ok(())
}

async fn ssh(name: &str, command: &[&str]) -> Result<(), String> {
    let vms = proxy().await?.list_vms().await.map_err(message)?;
    let vm = vms.into_iter().find(|vm| vm.name == name).ok_or_else(|| format!("no VM named {name}"))?;
    if vm.cid == 0 {
        return Err(format!("{name} isn't running"));
    }
    // The vsock/<cid> host comes from systemd's ssh proxy (systemd >= 256).
    let err = Command::new("ssh").arg(format!("{}@vsock/{}", vm.user, vm.cid)).args(command).exec();
    Err(format!("running ssh: {err}"))
}
