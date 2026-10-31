//! sonnette-agent: the guest's notification server. Runs in the guest's
//! user session, owns org.freedesktop.Notifications on its session bus, and
//! hands every notification to the host over vsock. Only the host (CID 2)
//! may connect.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::BufReader;
use tokio::sync::mpsc;
use zbus::message::Header;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{interface, Connection};

use carte::{icons, xdg};
use sonnette::proto::{self, Action, AgentMessage, Hints, HostMessage, Image, ImageFile, Notification};
use sonnette::vsock;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// Notifications kept while the host isn't connected, e.g. during boot.
const MAX_PENDING: usize = 32;

fn log(msg: impl std::fmt::Display) {
    eprintln!("sonnette-agent: {msg}");
}

#[derive(Default)]
struct State {
    /// The last ID handed out.
    last_id: u32,
    /// Notifications that haven't been closed, shown or pending.
    open: BTreeSet<u32>,
    host: Option<Host>,
    /// Notifications for the host while it isn't connected.
    pending: VecDeque<Notification>,
    /// From the host's Hello.
    capabilities: Option<Vec<String>>,
    connections: u64,
}

/// The current connection to the host.
struct Host {
    /// Tells this connection from the ones before it.
    generation: u64,
    tx: mpsc::UnboundedSender<AgentMessage>,
}

impl State {
    fn new_id(&mut self) -> u32 {
        loop {
            self.last_id = self.last_id.wrapping_add(1);
            if self.last_id != 0 && !self.open.contains(&self.last_id) {
                return self.last_id;
            }
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        self.host.as_ref().is_some_and(|h| h.generation == generation)
    }

    /// Sends `msg` to the host. Gives it back if the host isn't connected.
    fn send(&mut self, msg: AgentMessage) -> Result<(), AgentMessage> {
        match &self.host {
            Some(host) => host.tx.send(msg).map_err(|e| e.0),
            None => Err(msg),
        }
    }

    /// Forgets every open notification; they were on a host connection that
    /// is gone. Returns their IDs.
    fn drop_open(&mut self) -> Vec<u32> {
        self.pending.clear();
        std::mem::take(&mut self.open).into_iter().collect()
    }
}

struct Agent {
    vm: String,
    state: Mutex<State>,
}

impl Agent {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Shows `n`, replacing `replaces_id` if that's still open. Returns its
    /// ID, and the ID of a pending notification that had to go.
    fn post(&self, mut n: Notification, replaces_id: u32) -> (u32, Option<u32>) {
        let mut state = self.state();
        let id = if replaces_id != 0 && state.open.contains(&replaces_id) { replaces_id } else { state.new_id() };
        n.id = id;
        state.open.insert(id);
        let mut dropped = None;
        if let Err(AgentMessage::Notify(n)) = state.send(AgentMessage::Notify(Box::new(n))) {
            state.pending.retain(|p| p.id != id);
            if state.pending.len() >= MAX_PENDING {
                let old = state.pending.pop_front().unwrap();
                state.open.remove(&old.id);
                dropped = Some(old.id);
            }
            state.pending.push_back(*n);
        }
        (id, dropped)
    }

    /// Closes `id`. Returns true if it was pending and is now gone: the host
    /// never saw it, so it won't say it closed it.
    fn close(&self, id: u32) -> bool {
        let mut state = self.state();
        if !state.open.contains(&id) {
            return false;
        }
        if let Some(i) = state.pending.iter().position(|p| p.id == id) {
            state.pending.remove(i);
            state.open.remove(&id);
            return true;
        }
        // The host answers with Closed. If it goes away first, disconnect()
        // closes it.
        let _ = state.send(AgentMessage::Close { id });
        false
    }

    /// Makes `tx` the connection to the host, and queues the hello and the
    /// pending notifications on it. Returns its generation and the
    /// notifications that were on the connection it replaces.
    fn connect(&self, tx: mpsc::UnboundedSender<AgentMessage>) -> (u64, Vec<u32>) {
        let mut state = self.state();
        let replaced = if state.host.is_some() { state.drop_open() } else { Vec::new() };
        state.connections += 1;
        let generation = state.connections;
        let _ = tx.send(AgentMessage::Hello { vm: self.vm.clone() });
        for n in state.pending.drain(..) {
            let _ = tx.send(AgentMessage::Notify(Box::new(n)));
        }
        state.host = Some(Host { generation, tx });
        (generation, replaced)
    }

    /// The connection `generation` ended. Returns the notifications that were
    /// on it.
    fn disconnect(&self, generation: u64) -> Vec<u32> {
        let mut state = self.state();
        if !state.is_current(generation) {
            return Vec::new();
        }
        state.host = None;
        state.drop_open()
    }
}

struct Server(Arc<Agent>);

#[interface(name = "org.freedesktop.Notifications")]
impl Server {
    async fn get_capabilities(&self) -> Vec<String> {
        let state = self.0.state();
        match &state.capabilities {
            Some(caps) => caps.clone(),
            None => proto::DEFAULT_CAPABILITIES.iter().map(|&c| c.to_owned()).collect(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let app_id = match hint_str(&hints, "desktop-entry") {
            Some(entry) => Some(entry.strip_suffix(".desktop").unwrap_or(&entry).to_owned()),
            None => sender_app_id(conn, &header).await,
        };
        let image = hint_pixels(&hints, &["image-data", "image_data", "icon_data"])
            .map(Ok)
            .or_else(|| hint_str(&hints, "image-path").or_else(|| hint_str(&hints, "image_path")).map(Err))
            .and_then(|found| match found {
                Ok(pixels) => Some(pixels),
                Err(path) => load_icon(&path).map(Image::File),
            });
        let n = Notification {
            id: 0,
            app_name,
            app_id,
            summary,
            body,
            actions: actions
                .chunks_exact(2)
                .map(|pair| Action { key: pair[0].clone(), label: pair[1].clone() })
                .collect(),
            hints: Hints {
                urgency: hint_u8(&hints, "urgency"),
                category: hint_str(&hints, "category"),
                resident: hint_bool(&hints, "resident"),
                transient: hint_bool(&hints, "transient"),
                action_icons: hint_bool(&hints, "action-icons"),
                sound_name: hint_str(&hints, "sound-name"),
                suppress_sound: hint_bool(&hints, "suppress-sound"),
            },
            expire_timeout,
            app_icon: Some(app_icon).filter(|i| !i.is_empty()).and_then(|i| load_icon(&i)),
            image,
        };
        let app = n.app_id.clone().unwrap_or_else(|| n.app_name.clone());
        let (id, dropped) = self.0.post(n, replaces_id);
        log(format!("notification {id} from {app:?}"));
        if let Some(old) = dropped {
            log(format!("the host isn't connected, dropping notification {old}"));
            emit_closed(conn, old, proto::CLOSED_OTHER).await;
        }
        id
    }

    async fn close_notification(&self, #[zbus(connection)] conn: &Connection, id: u32) {
        if self.0.close(id) {
            emit_closed(conn, id, proto::CLOSED_BY_CALL).await;
        }
    }

    async fn get_server_information(&self) -> (String, String, String, String) {
        ("sonnette".into(), "garnish".into(), env!("CARGO_PKG_VERSION").into(), "1.2".into())
    }

    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: &str) -> zbus::Result<()>;
}

async fn emit_closed(conn: &Connection, id: u32, reason: u32) {
    let result = match SignalEmitter::new(conn, PATH) {
        Ok(emitter) => Server::notification_closed(&emitter, id, reason).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        log(format!("sending NotificationClosed: {e}"));
    }
}

async fn emit_action(conn: &Connection, id: u32, key: &str) {
    let result = match SignalEmitter::new(conn, PATH) {
        Ok(emitter) => Server::action_invoked(&emitter, id, key).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        log(format!("sending ActionInvoked: {e}"));
    }
}

fn hint<'a>(hints: &'a HashMap<String, OwnedValue>, key: &str) -> Option<&'a Value<'static>> {
    hints.get(key).map(|v| &**v)
}

fn hint_str(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match hint(hints, key)? {
        Value::Str(s) => Some(s.as_str().to_owned()),
        _ => None,
    }
}

/// The spec says bool; some applications send numbers.
fn hint_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    match *hint(hints, key)? {
        Value::Bool(b) => Some(b),
        Value::U8(n) => Some(n != 0),
        Value::I32(n) => Some(n != 0),
        Value::U32(n) => Some(n != 0),
        _ => None,
    }
}

/// The spec says byte; some applications send other integers.
fn hint_u8(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    match *hint(hints, key)? {
        Value::U8(n) => Some(n),
        Value::I32(n) => u8::try_from(n).ok(),
        Value::U32(n) => u8::try_from(n).ok(),
        _ => None,
    }
}

/// The first of `keys` that holds pixel data, (iiibiiay).
fn hint_pixels(hints: &HashMap<String, OwnedValue>, keys: &[&str]) -> Option<Image> {
    let value = keys.iter().find_map(|k| hint(hints, k))?;
    let Value::Structure(s) = value else { return None };
    let [Value::I32(width), Value::I32(height), Value::I32(rowstride), Value::Bool(has_alpha), Value::I32(bits_per_sample), Value::I32(channels), Value::Array(pixels)] =
        s.fields()
    else {
        return None;
    };
    if pixels.len() > proto::MAX_IMAGE {
        return None;
    }
    let data = pixels.iter().map(|b| if let Value::U8(b) = b { Some(*b) } else { None }).collect::<Option<Vec<u8>>>()?;
    let image = Image::Pixels {
        width: *width,
        height: *height,
        rowstride: *rowstride,
        has_alpha: *has_alpha,
        bits_per_sample: *bits_per_sample,
        channels: *channels,
        data,
    };
    image.is_valid().then_some(image)
}

/// Reads an icon given as a file:// URI, a path or an icon name.
fn load_icon(icon: &str) -> Option<ImageFile> {
    let name = match icon.strip_prefix("file://") {
        Some(path) => percent_decode(path)?,
        None => icon.to_owned(),
    };
    let (path, format) = icons::find(&name, proto::ICON_SIZE, &xdg::data_dirs())?;
    let mut data = Vec::new();
    File::open(&path).and_then(|f| f.take(proto::MAX_IMAGE as u64 + 1).read_to_end(&mut data)).ok()?;
    let file = ImageFile { format: format.to_owned(), data };
    file.is_valid().then_some(file)
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    String::from_utf8(out).ok()
}

/// The application carte started the sender in, from its scope,
/// app-carte-<id>-<n>.scope.
async fn sender_app_id(conn: &Connection, header: &Header<'_>) -> Option<String> {
    let sender = header.sender()?.to_owned();
    let dbus = zbus::fdo::DBusProxy::new(conn).await.ok()?;
    let pid = dbus.get_connection_unix_process_id(sender.into()).await.ok()?;
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    app_id_from_cgroup(&cgroup)
}

fn app_id_from_cgroup(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        let path = line.splitn(3, ':').nth(2)?;
        let scope = Path::new(path).file_name()?.to_str()?;
        let rest = scope.strip_prefix("app-carte-")?.strip_suffix(".scope")?;
        let (id, n) = rest.rsplit_once('-')?;
        // carte escapes only '-', the one character of an ID that systemd
        // escapes in unit names.
        let id = id.replace("\\x2d", "-");
        (n.bytes().all(|b| b.is_ascii_digit()) && carte::proto::is_valid_id(&id)).then_some(id)
    })
}

/// Talks to the host until the connection ends.
async fn serve_host(agent: Arc<Agent>, conn: Connection, stream: vsock::Stream) {
    let (reader, mut writer) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (generation, replaced) = agent.connect(tx);
    for id in replaced {
        emit_closed(&conn, id, proto::CLOSED_OTHER).await;
    }
    log(format!("host connected (connection {generation})"));

    let write = async {
        // Ends when a newer connection replaces this one.
        while let Some(msg) = rx.recv().await {
            proto::send(&mut writer, &msg).await?;
        }
        Ok(())
    };
    let read = async {
        let mut reader = BufReader::new(reader);
        while let Some(msg) = proto::recv::<HostMessage>(&mut reader).await? {
            match msg {
                HostMessage::Hello { capabilities } => {
                    let caps: Vec<String> = capabilities
                        .into_iter()
                        .filter(|c| proto::SUPPORTED_CAPABILITIES.contains(&c.as_str()))
                        .collect();
                    log(format!("the host's server can do {caps:?}"));
                    agent.state().capabilities = Some(caps);
                }
                HostMessage::Action { id, key } => {
                    let open = {
                        let state = agent.state();
                        state.is_current(generation) && state.open.contains(&id)
                    };
                    if open {
                        emit_action(&conn, id, &key).await;
                    }
                }
                HostMessage::Closed { id, reason } => {
                    let was_open = {
                        let mut state = agent.state();
                        state.is_current(generation) && state.open.remove(&id)
                    };
                    if was_open {
                        emit_closed(&conn, id, reason).await;
                    }
                }
            }
        }
        Ok::<_, std::io::Error>(())
    };
    let result = tokio::select! {
        r = write => r,
        r = read => r,
    };
    let closed = agent.disconnect(generation);
    match result {
        Ok(()) => log(format!("connection {generation} closed")),
        Err(e) => log(format!("connection {generation}: {e}")),
    }
    for id in closed {
        emit_closed(&conn, id, proto::CLOSED_OTHER).await;
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // The VM's name is the hostname, which gerantd sets to the name of the
    // VM's configuration, as carte-agent does.
    let vm = match std::fs::read_to_string("/proc/sys/kernel/hostname") {
        Ok(name) => name.trim().to_owned(),
        Err(e) => {
            log(format!("reading the hostname: {e}"));
            return ExitCode::FAILURE;
        }
    };
    if !carte::proto::is_valid_vm(&vm) {
        log(format!("invalid VM name (hostname) {vm:?}: use letters, digits and '-'"));
        return ExitCode::FAILURE;
    }

    // Listen first: if the bus name is taken, D-Bus activation must not
    // start a second agent that can't listen.
    let listener = match vsock::Listener::bind(proto::AGENT_PORT) {
        Ok(listener) => listener,
        Err(e) => {
            log(format!("listening on vsock port {}: {e}", proto::AGENT_PORT));
            return ExitCode::FAILURE;
        }
    };
    let agent = Arc::new(Agent { vm, state: Mutex::default() });
    let conn = zbus::connection::Builder::session()
        .and_then(|b| b.serve_at(PATH, Server(Arc::clone(&agent))))
        .and_then(|b| b.name(NAME));
    let conn = match conn {
        Ok(builder) => builder.build().await,
        Err(e) => Err(e),
    };
    let conn = match conn {
        Ok(conn) => conn,
        Err(zbus::Error::NameTaken) => {
            log(format!("{NAME} is taken: another notification server is running"));
            return ExitCode::FAILURE;
        }
        Err(e) => {
            log(format!("connecting to the session bus: {e}"));
            return ExitCode::FAILURE;
        }
    };
    log(format!("VM {}, listening on vsock port {}", agent.vm, proto::AGENT_PORT));

    loop {
        let (stream, cid) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                log(format!("accept: {e}"));
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        if cid != vsock::HOST_CID {
            log(format!("refused connection from CID {cid}"));
            continue;
        }
        tokio::spawn(serve_host(Arc::clone(&agent), conn.clone(), stream));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> Agent {
        Agent { vm: "test".into(), state: Mutex::default() }
    }

    fn notification(summary: &str) -> Notification {
        Notification { summary: summary.into(), ..Default::default() }
    }

    #[test]
    fn cgroup() {
        let line = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-carte-org.gnome.Calculator-1234.scope\n";
        assert_eq!(app_id_from_cgroup(line).as_deref(), Some("org.gnome.Calculator"));
        let line = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-carte-kde4\\x2dkate-5670.scope";
        assert_eq!(app_id_from_cgroup(line).as_deref(), Some("kde4-kate"));
        assert_eq!(app_id_from_cgroup("0::/user.slice/user@1000.service/app.slice/sonnette-agent.service"), None);
        assert_eq!(app_id_from_cgroup("0::/app-carte-..\\x2f-1.scope"), None);
    }

    #[test]
    fn percent() {
        assert_eq!(percent_decode("/a%20b/c").as_deref(), Some("/a b/c"));
        assert_eq!(percent_decode("/a%2"), None);
    }

    #[test]
    fn pending_until_connected() {
        let agent = agent();
        let (a, _) = agent.post(notification("a"), 0);
        let (b, _) = agent.post(notification("b"), 0);
        assert_ne!(a, b);
        // Replacing a pending notification keeps its place in the queue once.
        let (again, _) = agent.post(notification("a2"), a);
        assert_eq!(again, a);
        assert!(agent.close(b));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (generation, replaced) = agent.connect(tx);
        assert!(replaced.is_empty());
        assert_eq!(rx.try_recv().unwrap(), AgentMessage::Hello { vm: "test".into() });
        let AgentMessage::Notify(n) = rx.try_recv().unwrap() else { panic!() };
        assert_eq!((n.id, n.summary.as_str()), (a, "a2"));
        assert!(rx.try_recv().is_err());

        // Connected: closing goes through the host.
        assert!(!agent.close(a));
        assert_eq!(rx.try_recv().unwrap(), AgentMessage::Close { id: a });
        assert_eq!(agent.disconnect(generation), vec![a]);
        assert!(agent.state().open.is_empty());
    }

    #[test]
    fn pending_is_bounded() {
        let agent = agent();
        let first = agent.post(notification("0"), 0).0;
        for i in 1..MAX_PENDING {
            assert_eq!(agent.post(notification(&i.to_string()), 0).1, None);
        }
        assert_eq!(agent.post(notification("last"), 0).1, Some(first));
        assert_eq!(agent.state().pending.len(), MAX_PENDING);
    }

    #[test]
    fn replaced_connection() {
        let agent = agent();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (first, _) = agent.connect(tx1);
        let (id, _) = agent.post(notification("a"), 0);
        let (tx2, _rx2) = mpsc::unbounded_channel();
        let (second, replaced) = agent.connect(tx2);
        assert_eq!(replaced, vec![id]);
        // The old connection ending doesn't touch the new one.
        assert!(agent.disconnect(first).is_empty());
        assert!(agent.state().is_current(second));
    }
}
