//! gerantd: the daemon behind org.garnish.Gerant on the session bus.

use std::io;
use std::process::ExitCode;

use tokio::signal::unix::{SignalKind, signal};
use zbus::Connection;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

use gerant::config::DaemonConfig;
use gerant::dbus::{BUS_NAME, OBJECT_PATH};
use gerant::manager::{Gerant, Manager};
use gerant::ready::{self, READY_PORT};
use gerant::{ini, xdg};

const USAGE: &str = "\
usage: gerantd

Starts and stops the VMs configured in ~/.config/garnish/vms/ when asked to
over D-Bus ({BUS_NAME} on the session bus). Use `gerant` to talk to it.
SIGTERM or SIGINT shut every VM down; a second one stops them right away.";

fn load_config() -> Result<DaemonConfig, String> {
    let path = xdg::config_dir().join("gerantd.conf");
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
            eprintln!("gerantd: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let config = load_config()?;
    let conn = Connection::session().await.map_err(|e| format!("connecting to the session bus: {e}"))?;
    // Guests say when they have booted. Without the listener (vsock ports
    // are shared by all users: another gerantd may have it) VMs are shown
    // as running once crosvm runs.
    let listener = match ready::Listener::bind(READY_PORT) {
        Ok(listener) => Some(listener),
        Err(e) => {
            eprintln!("gerantd: can't listen on vsock port {READY_PORT} ({e}), VMs will be running as soon as crosvm is");
            None
        }
    };
    let manager = Manager::new(conn.clone(), config, listener.is_some());
    if let Some(listener) = listener {
        tokio::spawn(ready::serve(listener, manager.clone()));
    }
    conn.object_server()
        .at(OBJECT_PATH, Gerant(manager.clone()))
        .await
        .map_err(|e| format!("serving {OBJECT_PATH}: {e}"))?;
    // The name last, so a call that activated us doesn't arrive before the
    // object is there.
    match conn.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into()).await {
        Ok(RequestNameReply::PrimaryOwner) => {}
        Ok(_) => return Err(format!("{BUS_NAME} is taken: is gerantd already running?")),
        Err(e) => return Err(format!("requesting {BUS_NAME}: {e}")),
    }
    eprintln!("gerantd: ready, VMs are configured in {}", xdg::config_dir().join("vms").display());

    let signal_error = |e: io::Error| format!("setting up signal handlers: {e}");
    let mut term = signal(SignalKind::terminate()).map_err(signal_error)?;
    let mut int = signal(SignalKind::interrupt()).map_err(signal_error)?;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    eprintln!("gerantd: shutting down all VMs");
    let shutdown = manager.shutdown();
    tokio::pin!(shutdown);
    tokio::select! {
        () = &mut shutdown => return Ok(()),
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    eprintln!("gerantd: stopping all VMs now");
    manager.kill_all();
    shutdown.await;
    Ok(())
}
