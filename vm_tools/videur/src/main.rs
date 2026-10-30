use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use videur::filter::{Filter, Policy};
use videur::protocol::Protocols;
use videur::proxy::Proxy;

const USAGE: &str = "usage: videur --display SOCKET --listen SOCKET --clipboard both|to-host|from-host|none [--protocols DIR]...

Listens on --listen for Wayland clients (crosvm's --wayland-sock) and
connects each to the compositor at --display, passing on only the clipboard
data --clipboard allows: to-host lets the host paste what the guest copies,
from-host the guest paste what the host copies.

Protocol descriptions come from /usr/share/wayland, /usr/share/wayland-protocols
and each --protocols directory; globals of other protocols are hidden.";

struct Args {
    display: PathBuf,
    listen: PathBuf,
    policy: Policy,
    protocols: Vec<PathBuf>,
}

fn args() -> Result<Args, String> {
    let mut display = None;
    let mut listen = None;
    let mut policy = None;
    let mut protocols = vec![PathBuf::from("/usr/share/wayland"), PathBuf::from("/usr/share/wayland-protocols")];
    let mut it = std::env::args_os().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{} needs a value", a.to_string_lossy()));
        match a.to_str() {
            Some("--display") => display = Some(PathBuf::from(value()?)),
            Some("--listen") => listen = Some(PathBuf::from(value()?)),
            Some("--protocols") => protocols.push(PathBuf::from(value()?)),
            Some("--clipboard") => {
                let v = value()?;
                policy = Some(v.to_str().and_then(Policy::parse).ok_or_else(|| format!("bad --clipboard {}", v.to_string_lossy()))?);
            }
            Some("-h" | "--help") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument {}", a.to_string_lossy())),
        }
    }
    Ok(Args {
        display: display.ok_or("--display is required")?,
        listen: listen.ok_or("--listen is required")?,
        policy: policy.ok_or("--clipboard is required")?,
        protocols,
    })
}

fn main() -> ExitCode {
    let args = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("videur: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let dirs: Vec<&Path> = args.protocols.iter().map(PathBuf::as_path).collect();
    let run = || -> Result<(), String> {
        let protocols = Protocols::load(&dirs).map_err(|e| format!("reading protocols: {e}"))?;
        let filter = Filter::new(Rc::new(protocols), args.policy)?;
        let _ = std::fs::remove_file(&args.listen);
        let listener = UnixListener::bind(&args.listen).map_err(|e| format!("{}: {e}", args.listen.display()))?;
        eprintln!("videur: {} -> {}, clipboard {:?}", args.listen.display(), args.display.display(), args.policy);
        Proxy::new(listener, args.display.clone(), filter).and_then(|mut p| p.run()).map_err(|e| e.to_string())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("videur: {e}");
            ExitCode::FAILURE
        }
    }
}
