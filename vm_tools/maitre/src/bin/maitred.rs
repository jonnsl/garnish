//! maitred: the daemon behind org.garnish.Maitre on the session bus.

use std::io;
use std::process::ExitCode;

use tokio::signal::unix::{SignalKind, signal};
use zbus::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

use maitre::config::DaemonConfig;
use maitre::dbus::{BUS_NAME, OBJECT_PATH};
use maitre::manager::{Maitre, Manager};
use maitre::{ini, xdg};

const USAGE: &str = "\
usage: maitred

Starts and stops the VMs configured in ~/.config/garnish/vms/ when asked to
over D-Bus ({BUS_NAME} on the session bus). Use `maitre` to talk to it.
SIGTERM or SIGINT shut every VM down; a second one stops them right away.";

fn load_config() -> Result<DaemonConfig, String> {
    let path = xdg::config_dir().join("maitred.conf");
    match ini::load(&path) {
        Ok(entries) => DaemonConfig::from_entries(&entries?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(DaemonConfig::default()),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    if std::env::args().len() > 1 {
        eprintln!("{}", USAGE.replace("{BUS_NAME}", BUS_NAME));
        return ExitCode::from(2);
    }
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("maitred: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let config = load_config()?;
    let conn = Connection::session().await.map_err(|e| format!("connecting to the session bus: {e}"))?;
    let manager = Manager::new(conn.clone(), config);
    conn.object_server()
        .at(OBJECT_PATH, Maitre(manager.clone()))
        .await
        .map_err(|e| format!("serving {OBJECT_PATH}: {e}"))?;
    // The name last, so a call that activated us doesn't arrive before the
    // object is there.
    match conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner) => {}
        Ok(_) => return Err(format!("{BUS_NAME} is taken: is maitred already running?")),
        Err(e) => return Err(format!("requesting {BUS_NAME}: {e}")),
    }
    eprintln!("maitred: ready, VMs are configured in {}", xdg::config_dir().join("vms").display());

    let signal_error = |e: io::Error| format!("setting up signal handlers: {e}");
    let mut term = signal(SignalKind::terminate()).map_err(signal_error)?;
    let mut int = signal(SignalKind::interrupt()).map_err(signal_error)?;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    eprintln!("maitred: shutting down all VMs");
    let shutdown = manager.shutdown();
    tokio::pin!(shutdown);
    tokio::select! {
        () = &mut shutdown => return Ok(()),
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    eprintln!("maitred: stopping all VMs now");
    manager.kill_all();
    shutdown.await;
    Ok(())
}
