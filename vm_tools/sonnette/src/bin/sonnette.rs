//! sonnette: the host side of sonnette-agent. Shows a VM's notifications
//! through the host's notification server and passes the user's clicks and
//! dismissals back.

use std::collections::HashMap;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::io::{AsyncWrite, BufReader};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{Structure, Value};
use zbus::Connection;

use carte::{menu, xdg};
use sonnette::proto::{self, AgentMessage, HostMessage, Image, ImageFile, Notification};
use sonnette::vsock;

const USAGE: &str = "\
usage: sonnette relay --cid <cid>   show the notifications of the VM at <cid> on
                                   the host, until killed";

const RETRY: Duration = Duration::from_secs(1);
/// How long the agent has to say hello after connecting.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// New notifications a VM may show at once, and per second after that.
/// Replacing one doesn't count.
const BURST: f64 = 20.0;
const RATE: f64 = 2.0;

fn log(msg: impl std::fmt::Display) {
    eprintln!("sonnette: {msg}");
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["relay", "--cid", cid] => match cid.parse() {
            Ok(cid) => relay(cid).await,
            Err(_) => Err(format!("invalid CID {cid}")),
        },
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(e);
            ExitCode::FAILURE
        }
    }
}

/// What happens on the host, in one queue.
enum Event {
    Closed {
        id: u32,
        reason: u32,
    },
    Action {
        id: u32,
        key: String,
    },
    /// SIGTERM or SIGINT.
    Shutdown,
}

/// Forwards the host server's signals and our own termination to `tx`.
/// The signal streams are always read, so they never back up.
async fn watch_host(
    proxy: NotificationsProxy<'static>,
    tx: mpsc::UnboundedSender<Event>,
) -> zbus::Result<()> {
    let mut closed = proxy.receive_notification_closed().await?;
    let mut actions = proxy.receive_action_invoked().await?;
    let (mut term, mut int) = match (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) {
        (Ok(term), Ok(int)) => (term, int),
        (Err(e), _) | (_, Err(e)) => return Err(zbus::Error::InputOutput(e.into())),
    };
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                Some(signal) = closed.next() => match signal.args() {
                    Ok(args) => Event::Closed { id: args.id, reason: args.reason },
                    Err(_) => continue,
                },
                Some(signal) = actions.next() => match signal.args() {
                    Ok(args) => Event::Action { id: args.id, key: args.action_key },
                    Err(_) => continue,
                },
                _ = term.recv() => Event::Shutdown,
                _ = int.recv() => Event::Shutdown,
            };
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    Ok(())
}

async fn relay(cid: u32) -> Result<(), String> {
    let conn = Connection::session()
        .await
        .map_err(|e| format!("connecting to the session bus: {e}"))?;
    let proxy = NotificationsProxy::builder(&conn)
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .map_err(|e| e.to_string())?;
    let (tx, mut events) = mpsc::unbounded_channel();
    watch_host(proxy.clone(), tx)
        .await
        .map_err(|e| format!("watching the notification server: {e}"))?;

    loop {
        let stream = {
            let connect = connect(cid);
            tokio::pin!(connect);
            loop {
                tokio::select! {
                    stream = &mut connect => break stream,
                    event = events.recv() => match event {
                        // Nothing of ours is open while disconnected.
                        Some(Event::Closed { .. } | Event::Action { .. }) => {}
                        Some(Event::Shutdown) | None => return Ok(()),
                    },
                }
            }
        };
        log(format!("connected to CID {cid}"));
        let mut session = Session::new(proxy.clone());
        let result = session.run(stream, &mut events).await;
        session.close_all().await;
        match result {
            Ok(End::Shutdown) => return Ok(()),
            Ok(End::Disconnected) => log(format!("CID {cid} closed the connection")),
            Err(e) => log(format!("connection to CID {cid}: {e}")),
        }
        sleep(RETRY).await;
    }
}

/// Waits for the agent, retrying every second.
async fn connect(cid: u32) -> vsock::Stream {
    let mut waiting = false;
    loop {
        match vsock::connect(cid, proto::AGENT_PORT).await {
            Ok(stream) => return stream,
            Err(e) => {
                if !waiting {
                    log(format!("waiting for sonnette-agent on CID {cid} ({e})"));
                    waiting = true;
                }
                sleep(RETRY).await;
            }
        }
    }
}

enum End {
    Disconnected,
    Shutdown,
}

/// Limits how fast a VM can show new notifications.
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new() -> Self {
        Bucket {
            tokens: BURST,
            last: Instant::now(),
        }
    }

    fn take(&mut self, now: Instant) -> bool {
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * RATE).min(BURST);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// One connection to the agent, and the notifications shown for it.
struct Session {
    proxy: NotificationsProxy<'static>,
    /// Set by the agent's hello.
    vm: String,
    /// Where the guest's icons are written, for the server to read.
    icons: PathBuf,
    /// Guest IDs to host IDs and back.
    to_host: HashMap<u32, u32>,
    to_guest: HashMap<u32, u32>,
    bucket: Bucket,
}

impl Session {
    fn new(proxy: NotificationsProxy<'static>) -> Self {
        Session {
            proxy,
            vm: String::new(),
            icons: PathBuf::new(),
            to_host: HashMap::new(),
            to_guest: HashMap::new(),
            bucket: Bucket::new(),
        }
    }

    async fn run(
        &mut self,
        stream: vsock::Stream,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> io::Result<End> {
        let (reader, mut writer) = tokio::io::split(stream);
        // Reading a line isn't cancel safe, so it gets a task of its own.
        let (guest_tx, mut guest) = mpsc::channel(16);
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            loop {
                let msg = proto::recv::<AgentMessage>(&mut reader).await;
                let end = !matches!(msg, Ok(Some(_)));
                if guest_tx.send(msg).await.is_err() || end {
                    break;
                }
            }
        });
        let result = self.serve(&mut writer, &mut guest, events).await;
        reader_task.abort();
        result
    }

    async fn serve(
        &mut self,
        writer: &mut (impl AsyncWrite + Unpin),
        guest: &mut mpsc::Receiver<io::Result<Option<AgentMessage>>>,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> io::Result<End> {
        let invalid = |what: String| io::Error::new(io::ErrorKind::InvalidData, what);
        let hello = timeout(HELLO_TIMEOUT, guest.recv())
            .await
            .map_err(|_| invalid("no hello from the agent".into()))?;
        let vm = match hello {
            Some(Ok(Some(AgentMessage::Hello { vm }))) => vm,
            Some(Err(e)) => return Err(e),
            None | Some(Ok(None)) => return Ok(End::Disconnected),
            Some(Ok(Some(other))) => {
                return Err(invalid(format!("expected a hello, got {other:?}")))
            }
        };
        if !carte::proto::is_valid_vm(&vm) {
            return Err(invalid(format!("invalid VM name {vm:?}")));
        }
        self.icons = xdg::runtime_dir().join("sonnette").join(&vm);
        self.vm = vm;
        // Left over if an earlier relay was killed.
        let _ = fs::remove_dir_all(&self.icons);
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.icons)?;

        let capabilities = match self.proxy.get_capabilities().await {
            Ok(caps) => caps
                .into_iter()
                .filter(|c| proto::SUPPORTED_CAPABILITIES.contains(&c.as_str()))
                .collect(),
            Err(e) => {
                log(format!(
                    "{}: asking the notification server what it can do: {e}",
                    self.vm
                ));
                proto::DEFAULT_CAPABILITIES
                    .iter()
                    .map(|&c| c.to_owned())
                    .collect()
            }
        };
        proto::send(writer, &HostMessage::Hello { capabilities }).await?;
        log(format!("{}: connected", self.vm));

        loop {
            tokio::select! {
                msg = guest.recv() => match msg {
                    Some(Ok(Some(AgentMessage::Notify(n)))) => {
                        if let Some(reply) = self.notify(*n).await {
                            proto::send(writer, &reply).await?;
                        }
                    }
                    Some(Ok(Some(AgentMessage::Close { id }))) => {
                        if let Some(reply) = self.close(id).await {
                            proto::send(writer, &reply).await?;
                        }
                    }
                    Some(Ok(Some(AgentMessage::Hello { .. }))) => {}
                    Some(Err(e)) => return Err(e),
                    None | Some(Ok(None)) => return Ok(End::Disconnected),
                },
                event = events.recv() => match event {
                    Some(Event::Closed { id, reason }) => {
                        if let Some(guest_id) = self.forget(id) {
                            proto::send(writer, &HostMessage::Closed { id: guest_id, reason }).await?;
                        }
                    }
                    Some(Event::Action { id, key }) => {
                        if let Some(&guest_id) = self.to_guest.get(&id) {
                            proto::send(writer, &HostMessage::Action { id: guest_id, key }).await?;
                        }
                    }
                    Some(Event::Shutdown) | None => return Ok(End::Shutdown),
                },
            }
        }
    }

    /// Shows `n` on the host. Returns what to tell the agent if it isn't
    /// shown.
    async fn notify(&mut self, mut n: Notification) -> Option<HostMessage> {
        let id = n.id;
        let replaces = self.to_host.get(&id).copied().unwrap_or(0);
        if replaces == 0 && !self.bucket.take(Instant::now()) {
            log(format!(
                "{}: too many notifications, dropping {id}",
                self.vm
            ));
            return Some(HostMessage::Closed {
                id,
                reason: proto::CLOSED_OTHER,
            });
        }
        n.sanitize();
        self.remove_icons(id);

        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        let h = &n.hints;
        if let Some(urgency) = h.urgency {
            hints.insert("urgency", Value::U8(urgency));
        }
        for (key, value) in [("category", &h.category), ("sound-name", &h.sound_name)] {
            if let Some(value) = value {
                hints.insert(key, Value::from(value.as_str()));
            }
        }
        for (key, value) in [
            ("resident", h.resident),
            ("transient", h.transient),
            ("action-icons", h.action_icons),
            ("suppress-sound", h.suppress_sound),
        ] {
            if let Some(value) = value {
                hints.insert(key, Value::Bool(value));
            }
        }

        // The application's carte launcher, so the server shows its name and
        // icon (it has the VM in its name), and the user can set its
        // notification settings.
        let launcher = n
            .app_id
            .as_deref()
            .map(|app| menu::file_name(&self.vm, app));
        let launcher =
            launcher.filter(|file| xdg::data_home().join("applications").join(file).is_file());
        if let Some(file) = &launcher {
            hints.insert(
                "desktop-entry",
                Value::from(file.strip_suffix(".desktop").unwrap_or(file)),
            );
        }

        let app_icon = match &n.app_icon {
            Some(icon) => self.write_icon(id, "icon", icon).unwrap_or_default(),
            None => String::new(),
        };
        match &n.image {
            Some(Image::Pixels {
                width,
                height,
                rowstride,
                has_alpha,
                bits_per_sample,
                channels,
                data,
            }) => {
                let pixels = (
                    *width,
                    *height,
                    *rowstride,
                    *has_alpha,
                    *bits_per_sample,
                    *channels,
                    data.clone(),
                );
                hints.insert("image-data", Value::Structure(Structure::from(pixels)));
            }
            Some(Image::File(file)) => {
                if let Some(uri) = self.write_icon(id, "image", file) {
                    hints.insert("image-path", Value::from(uri));
                }
            }
            None => {}
        }

        let app_name = if n.app_name.is_empty() {
            self.vm.clone()
        } else {
            format!("{} ({})", n.app_name, self.vm)
        };
        let actions: Vec<&str> = n
            .actions
            .iter()
            .flat_map(|a| [a.key.as_str(), a.label.as_str()])
            .collect();
        let result = self
            .proxy
            .notify(
                &app_name,
                replaces,
                &app_icon,
                &n.summary,
                &n.body,
                &actions,
                hints,
                n.expire_timeout,
            )
            .await;
        match result {
            Ok(host_id) => {
                if replaces != 0 && replaces != host_id {
                    self.to_guest.remove(&replaces);
                }
                self.to_host.insert(id, host_id);
                self.to_guest.insert(host_id, id);
                let from = launcher.as_deref().unwrap_or(&app_name);
                log(format!(
                    "{}: notification {id} from {from:?} is {host_id} on the host",
                    self.vm
                ));
                None
            }
            Err(e) => {
                log(format!("{}: showing notification {id}: {e}", self.vm));
                self.forget_guest(id);
                Some(HostMessage::Closed {
                    id,
                    reason: proto::CLOSED_OTHER,
                })
            }
        }
    }

    /// Closes the guest's notification `id`. Returns what to tell the agent
    /// if the host server won't.
    async fn close(&mut self, id: u32) -> Option<HostMessage> {
        let Some(&host_id) = self.to_host.get(&id) else {
            return Some(HostMessage::Closed {
                id,
                reason: proto::CLOSED_BY_CALL,
            });
        };
        // The server answers with NotificationClosed.
        match self.proxy.close_notification(host_id).await {
            Ok(()) => None,
            Err(e) => {
                log(format!("{}: closing notification {id}: {e}", self.vm));
                self.forget(host_id);
                Some(HostMessage::Closed {
                    id,
                    reason: proto::CLOSED_BY_CALL,
                })
            }
        }
    }

    /// Forgets the host's notification `host_id`. Returns the guest's ID.
    fn forget(&mut self, host_id: u32) -> Option<u32> {
        let id = self.to_guest.remove(&host_id)?;
        self.to_host.remove(&id);
        self.remove_icons(id);
        Some(id)
    }

    fn forget_guest(&mut self, id: u32) {
        if let Some(host_id) = self.to_host.remove(&id) {
            self.to_guest.remove(&host_id);
        }
        self.remove_icons(id);
    }

    /// Closes everything this VM showed: nobody is left to act on it.
    async fn close_all(&mut self) {
        for &host_id in self.to_guest.keys() {
            let _ = self.proxy.close_notification(host_id).await;
        }
        self.to_guest.clear();
        self.to_host.clear();
        if !self.icons.as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&self.icons);
        }
    }

    fn icon_path(&self, id: u32, kind: &str, format: &str) -> PathBuf {
        self.icons.join(format!("{id}-{kind}.{format}"))
    }

    /// Writes an icon from the guest for the server to read. Returns its URI.
    fn write_icon(&self, id: u32, kind: &str, icon: &ImageFile) -> Option<String> {
        let path = self.icon_path(id, kind, &icon.format);
        match fs::write(&path, &icon.data) {
            Ok(()) => Some(file_uri(&path)),
            Err(e) => {
                log(format!("{}: writing {}: {e}", self.vm, path.display()));
                None
            }
        }
    }

    fn remove_icons(&self, id: u32) {
        for kind in ["icon", "image"] {
            for format in ["png", "svg"] {
                let _ = fs::remove_file(self.icon_path(id, kind, format));
            }
        }
    }
}

fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri() {
        assert_eq!(
            file_uri(Path::new("/run/user/1000/sonnette/debian/1-icon.png")),
            "file:///run/user/1000/sonnette/debian/1-icon.png"
        );
        assert_eq!(file_uri(Path::new("/a b/%")), "file:///a%20b/%25");
    }

    #[test]
    fn bucket() {
        let start = Instant::now();
        let mut bucket = Bucket {
            tokens: BURST,
            last: start,
        };
        for _ in 0..BURST as usize {
            assert!(bucket.take(start));
        }
        assert!(!bucket.take(start));
        assert!(bucket.take(start + Duration::from_millis(500)));
        assert!(!bucket.take(start + Duration::from_millis(500)));
    }
}
