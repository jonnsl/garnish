//! What the guest may copy to the host and paste from it.
//!
//! sommelier passes the guest's clipboard straight to the host compositor,
//! so even a copy and paste between two guest apps goes through it: the
//! guest sets the selection, the compositor offers it back, and a paste asks
//! the compositor, which asks the guest's source. The filter keeps that
//! working while cutting the host out:
//!
//! - Every offer the compositor makes is held back until the selection or
//!   drag it belongs to is known. If the guest owns that selection or drag,
//!   the offer is the guest's own data: it's passed on, and a paste from it
//!   is answered by the guest's source directly, never by the compositor.
//!   Otherwise the data is a host app's, and the offer is passed on only if
//!   the guest may paste from the host. If not, the guest never sees it, and
//!   the compositor is told it's no longer wanted.
//! - If the guest may not copy to the host, the compositor's requests for
//!   the guest's data (wl_data_source.send) are dropped, which leaves the
//!   host app with nothing.
//!
//! Getting the owner wrong can only make a guest paste its own data, never
//! a host app's: pastes from offers taken for the guest's never reach the
//! compositor.
//!
//! The filter parses every message, so it knows each object's interface and
//! which file descriptors go with which message. Globals whose protocol it
//! doesn't know are hidden, and so are the data-control protocols, which
//! read the clipboard without focus. Anything it can't make sense of ends
//! the connection.

use std::collections::HashMap;
use std::rc::Rc;

use crate::protocol::{Arg, IfaceId, Protocols};
use crate::wire::{self, Builder, Value};

/// Globals the guest never gets, whatever it may copy.
const ALWAYS_HIDDEN: &[&str] = &[
    // Read and set the clipboard without focus.
    "ext_data_control_manager_v1",
    "zwlr_data_control_manager_v1",
    // An older primary selection protocol the filter doesn't handle.
    "gtk_primary_selection_device_manager",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The host may read what the guest copies.
    pub to_host: bool,
    /// The guest may read what the host copies.
    pub from_host: bool,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        let (to_host, from_host) = match s {
            "both" => (true, true),
            "to-host" => (true, false),
            "from-host" => (false, true),
            "none" => (false, false),
            _ => return None,
        };
        Some(Policy { to_host, from_host })
    }
}

pub type ConnId = u64;

/// A message to send, with its file descriptors.
#[derive(Debug)]
pub enum Out<F> {
    ToServer(ConnId, Vec<u8>, Vec<F>),
    ToClient(ConnId, Vec<u8>, Vec<F>),
}

/// The clipboard (wl_data_device) or the primary selection
/// (zwp_primary_selection_device_v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Clipboard,
    Primary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Device(Family),
    Offer(Family),
    Source(Family),
}

/// A guest data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceRef {
    conn: ConnId,
    id: u32,
}

#[derive(Debug)]
enum OfferState<F> {
    /// Not passed on yet: its messages, until it's known whose it is.
    Pending(Vec<(Vec<u8>, Vec<F>)>),
    /// Passed on. `owner` is the guest source it's for, None for host data.
    Given { owner: Option<SourceRef> },
    /// A host app's, kept from the guest.
    Hidden,
}

#[derive(Debug)]
enum Role<F> {
    Plain,
    Registry,
    Offer(OfferState<F>),
    /// A guest source; `destroyed` once the guest destroyed it.
    Source {
        destroyed: bool,
    },
}

#[derive(Debug)]
struct Object<F> {
    iface: IfaceId,
    role: Role<F>,
}

#[derive(Debug)]
struct Global {
    iface: String,
    /// What the guest may bind: the compositor's version, or the newest
    /// the filter knows if that's older.
    version: u32,
    hidden: bool,
}

#[derive(Debug)]
struct Conn<F> {
    objects: HashMap<u32, Object<F>>,
    globals: HashMap<u32, Global>,
}

pub struct Filter<F> {
    protocols: Rc<Protocols>,
    policy: Policy,
    kinds: HashMap<IfaceId, Kind>,
    registry: Option<IfaceId>,
    display: IfaceId,
    conns: HashMap<ConnId, Conn<F>>,
    /// The guest source that owns the clipboard and the primary selection.
    selection: [Option<SourceRef>; 2],
    /// The guest source being dragged.
    drag: Option<SourceRef>,
}

const SERVER_ID_START: u32 = 0xff00_0000;

impl<F> Filter<F> {
    pub fn new(protocols: Rc<Protocols>, policy: Policy) -> Result<Self, String> {
        let display = protocols
            .find("wl_display")
            .ok_or("no wl_display protocol (is wayland.xml installed?)")?;
        let mut kinds = HashMap::new();
        for (family, device, offer, source) in [
            (
                Family::Clipboard,
                "wl_data_device",
                "wl_data_offer",
                "wl_data_source",
            ),
            (
                Family::Primary,
                "zwp_primary_selection_device_v1",
                "zwp_primary_selection_offer_v1",
                "zwp_primary_selection_source_v1",
            ),
        ] {
            for (name, kind) in [
                (device, Kind::Device(family)),
                (offer, Kind::Offer(family)),
                (source, Kind::Source(family)),
            ] {
                if let Some(id) = protocols.find(name) {
                    kinds.insert(id, kind);
                }
            }
        }
        let registry = protocols.find("wl_registry");
        Ok(Filter {
            protocols,
            policy,
            kinds,
            registry,
            display,
            conns: HashMap::new(),
            selection: [None; 2],
            drag: None,
        })
    }

    pub fn open(&mut self, conn: ConnId) {
        let mut objects = HashMap::new();
        objects.insert(
            1,
            Object {
                iface: self.display,
                role: Role::Plain,
            },
        );
        self.conns.insert(
            conn,
            Conn {
                objects,
                globals: HashMap::new(),
            },
        );
    }

    pub fn close(&mut self, conn: ConnId) {
        self.conns.remove(&conn);
        for s in self.selection.iter_mut().chain([&mut self.drag]) {
            if s.is_some_and(|s| s.conn == conn) {
                *s = None;
            }
        }
    }

    /// How many file descriptors come with the message whose header is at
    /// the start of `msg`.
    pub fn fds_needed(&self, conn: ConnId, from_client: bool, msg: &[u8]) -> Result<usize, String> {
        let (object, opcode, _) = wire::header(msg).ok_or("short header")?;
        let c = self.conns.get(&conn).ok_or("unknown connection")?;
        let obj = c
            .objects
            .get(&object)
            .ok_or_else(|| format!("message for unknown object {object}"))?;
        Ok(self.message(obj.iface, from_client, opcode)?.fds())
    }

    fn message(
        &self,
        iface: IfaceId,
        request: bool,
        opcode: u16,
    ) -> Result<&crate::protocol::Message, String> {
        let i = self.protocols.get(iface);
        let list = if request { &i.requests } else { &i.events };
        list.get(opcode as usize).ok_or_else(|| {
            format!(
                "{} {} {opcode} isn't in the protocol",
                i.name,
                if request { "request" } else { "event" }
            )
        })
    }

    fn opcode(&self, iface: IfaceId, request: bool, name: &str) -> u16 {
        let i = self.protocols.get(iface);
        let list = if request { &i.requests } else { &i.events };
        list.iter()
            .position(|m| m.name == name)
            .expect("message the filter relies on") as u16
    }

    /// A message from the guest (`from_client`) or the compositor, with
    /// its file descriptors. Returns what to send instead.
    pub fn handle(
        &mut self,
        conn: ConnId,
        from_client: bool,
        msg: Vec<u8>,
        fds: Vec<F>,
    ) -> Result<Vec<Out<F>>, String> {
        if from_client {
            self.request(conn, msg, fds)
        } else {
            self.event(conn, msg, fds)
        }
    }

    fn conn(&mut self, conn: ConnId) -> Result<&mut Conn<F>, String> {
        self.conns
            .get_mut(&conn)
            .ok_or_else(|| "unknown connection".into())
    }

    fn role_for(&self, iface: IfaceId) -> Role<F> {
        match self.kinds.get(&iface) {
            Some(Kind::Source(_)) => Role::Source { destroyed: false },
            Some(Kind::Offer(_)) => Role::Offer(OfferState::Pending(Vec::new())),
            _ if Some(iface) == self.registry => Role::Registry,
            _ => Role::Plain,
        }
    }

    /// Adds the objects a message creates.
    fn create(
        &mut self,
        conn: ConnId,
        from_client: bool,
        args: &[Arg],
        values: &[wire::Parsed],
    ) -> Result<(), String> {
        for (arg, v) in args.iter().zip(values) {
            let (iface, id) = match (arg, &v.value) {
                (Arg::NewId(Some(name)), Value::NewId(id)) => (
                    self.protocols
                        .find(name)
                        .ok_or_else(|| format!("unknown interface {name}"))?,
                    *id,
                ),
                (Arg::NewId(None), Value::TypedNewId(name, _, id)) => {
                    let name = String::from_utf8_lossy(name);
                    (
                        self.protocols
                            .find(&name)
                            .ok_or_else(|| format!("unknown interface {name}"))?,
                        *id,
                    )
                }
                _ => continue,
            };
            if id == 0 || (id >= SERVER_ID_START) == from_client {
                return Err(format!("new object {id} in the wrong range"));
            }
            if from_client && matches!(self.kinds.get(&iface), Some(Kind::Offer(_))) {
                return Err("the guest made an offer".into());
            }
            let role = self.role_for(iface);
            self.conn(conn)?.objects.insert(id, Object { iface, role });
        }
        Ok(())
    }

    fn source_alive(&self, s: SourceRef) -> bool {
        self.conns
            .get(&s.conn)
            .and_then(|c| c.objects.get(&s.id))
            .is_some_and(|o| matches!(o.role, Role::Source { destroyed: false }))
    }

    fn forget_source(&mut self, s: SourceRef) {
        for r in self.selection.iter_mut().chain([&mut self.drag]) {
            if *r == Some(s) {
                *r = None;
            }
        }
    }

    fn request(&mut self, conn: ConnId, msg: Vec<u8>, fds: Vec<F>) -> Result<Vec<Out<F>>, String> {
        let (object, opcode, _) = wire::header(&msg).ok_or("short header")?;
        let obj = self
            .conn(conn)?
            .objects
            .get(&object)
            .ok_or_else(|| format!("request for unknown object {object}"))?;
        let iface = obj.iface;
        let given_owner = match &obj.role {
            Role::Offer(OfferState::Given { owner }) => Some(*owner),
            Role::Offer(_) => {
                return Err(format!(
                    "request for offer {object}, which the guest wasn't given"
                ));
            }
            _ => None,
        };
        let is_registry = matches!(obj.role, Role::Registry);
        let desc = self.message(iface, true, opcode)?;
        let name = desc.name.clone();
        let args = desc.args.clone();
        let values = wire::parse(&msg, &args)?;

        if is_registry && name == "bind" {
            let Value::Uint(global) = values[0].value else {
                unreachable!()
            };
            let Value::TypedNewId(ref want, version, _) = values[1].value else {
                unreachable!()
            };
            let g = self.conn(conn)?.globals.get(&global);
            let ok = g.is_some_and(|g| {
                !g.hidden && g.iface.as_bytes() == want.as_slice() && version <= g.version
            });
            if !ok {
                return Err(format!(
                    "bind to global {global} ({}) v{version}, which it wasn't offered",
                    String::from_utf8_lossy(want)
                ));
            }
        }
        self.create(conn, true, &args, &values)?;

        let source_arg = |i: usize| match values[i].value {
            Value::Object(0) => None,
            Value::Object(id) => Some(SourceRef { conn, id }),
            _ => unreachable!(),
        };
        match (self.kinds.get(&iface).copied(), name.as_str()) {
            (Some(Kind::Device(f)), "set_selection") => self.selection[f as usize] = source_arg(0),
            (Some(Kind::Device(Family::Clipboard)), "start_drag") => self.drag = source_arg(0),
            (Some(Kind::Source(_)), "destroy") => {
                if let Some(Object {
                    role: Role::Source { destroyed },
                    ..
                }) = self.conn(conn)?.objects.get_mut(&object)
                {
                    *destroyed = true;
                }
                self.forget_source(SourceRef { conn, id: object });
            }
            (Some(Kind::Offer(_)), "receive") => {
                let Value::String(Some(mime)) = &values[0].value else {
                    return Err("receive without a type".into());
                };
                return Ok(match given_owner.unwrap() {
                    // The guest's own data: its source answers.
                    Some(s) if self.source_alive(s) => {
                        let source = self.conns[&s.conn].objects[&s.id].iface;
                        let send = self.opcode(source, false, "send");
                        vec![Out::ToClient(
                            s.conn,
                            Builder::new(s.id, send).string(mime).build(),
                            fds,
                        )]
                    }
                    // Gone since: nothing to paste. Dropping the descriptor
                    // ends the paste.
                    Some(_) => Vec::new(),
                    None if self.policy.from_host => vec![Out::ToServer(conn, msg, fds)],
                    None => Vec::new(),
                });
            }
            _ => {}
        }
        Ok(vec![Out::ToServer(conn, msg, fds)])
    }

    fn event(
        &mut self,
        conn: ConnId,
        mut msg: Vec<u8>,
        fds: Vec<F>,
    ) -> Result<Vec<Out<F>>, String> {
        let (object, opcode, _) = wire::header(&msg).ok_or("short header")?;
        let obj = self
            .conn(conn)?
            .objects
            .get(&object)
            .ok_or_else(|| format!("event for unknown object {object}"))?;
        let iface = obj.iface;
        let is_registry = matches!(obj.role, Role::Registry);
        let desc = self.message(iface, false, opcode)?;
        let name = desc.name.clone();
        let args = desc.args.clone();
        let values = wire::parse(&msg, &args)?;
        self.create(conn, false, &args, &values)?;

        // Messages for an offer wait with it, or go nowhere.
        match &mut self.conn(conn)?.objects.get_mut(&object).unwrap().role {
            Role::Offer(OfferState::Pending(held)) => {
                held.push((msg, fds));
                return Ok(Vec::new());
            }
            Role::Offer(OfferState::Hidden) => return Ok(Vec::new()),
            _ => {}
        }

        if iface == self.display && name == "delete_id" {
            let Value::Uint(id) = values[0].value else {
                unreachable!()
            };
            if self
                .conn(conn)?
                .objects
                .remove(&id)
                .is_some_and(|o| matches!(o.role, Role::Source { .. }))
            {
                self.forget_source(SourceRef { conn, id });
            }
        } else if is_registry && name == "global" {
            let Value::Uint(global) = values[0].value else {
                unreachable!()
            };
            let Value::String(Some(ref iface_name)) = values[1].value else {
                return Err("global without an interface".into());
            };
            let Value::Uint(version) = values[2].value else {
                unreachable!()
            };
            let iface_name = String::from_utf8_lossy(iface_name).into_owned();
            let known = self.protocols.find(&iface_name);
            let hidden = known.is_none() || ALWAYS_HIDDEN.contains(&iface_name.as_str());
            let version = known.map_or(0, |k| version.min(self.protocols.get(k).version));
            wire::set_word(&mut msg, values[2].offset, version);
            self.conn(conn)?.globals.insert(
                global,
                Global {
                    iface: iface_name,
                    version,
                    hidden,
                },
            );
            if hidden {
                return Ok(Vec::new());
            }
        } else if is_registry && name == "global_remove" {
            let Value::Uint(global) = values[0].value else {
                unreachable!()
            };
            if self
                .conn(conn)?
                .globals
                .remove(&global)
                .is_none_or(|g| g.hidden)
            {
                return Ok(Vec::new());
            }
        }

        let mut out = Vec::new();
        match (self.kinds.get(&iface).copied(), name.as_str()) {
            // The offer stays held until it's used: its data_offer event
            // was held with it.
            (Some(Kind::Device(_)), "data_offer") => {
                let Value::NewId(offer) = values[0].value else {
                    unreachable!()
                };
                let o = self.conn(conn)?.objects.get_mut(&offer).unwrap();
                let Role::Offer(OfferState::Pending(held)) = &mut o.role else {
                    unreachable!()
                };
                held.push((msg, fds));
                return Ok(Vec::new());
            }
            (Some(Kind::Device(f)), "selection") => {
                let owner = self.selection[f as usize];
                self.decide(conn, &mut msg, &values[0], owner, &mut out)?;
            }
            (Some(Kind::Device(_)), "enter") => {
                let owner = self.drag;
                self.decide(conn, &mut msg, &values[4], owner, &mut out)?;
            }
            (Some(Kind::Source(_)), "send") if !self.policy.to_host => {
                // A host app wants the guest's data. Dropping the
                // descriptor gives it nothing.
                return Ok(Vec::new());
            }
            (Some(Kind::Source(_)), "cancelled" | "dnd_finished") => {
                self.forget_source(SourceRef { conn, id: object })
            }
            _ => {}
        }
        out.push(Out::ToClient(conn, msg, fds));
        Ok(out)
    }

    /// A selection or drag `enter` names an offer: whose data is it?
    fn decide(
        &mut self,
        conn: ConnId,
        msg: &mut [u8],
        arg: &wire::Parsed,
        owner: Option<SourceRef>,
        out: &mut Vec<Out<F>>,
    ) -> Result<(), String> {
        let Value::Object(offer) = arg.value else {
            unreachable!()
        };
        if offer == 0 {
            return Ok(());
        }
        let owner = owner.filter(|s| self.source_alive(*s));
        let from_host = self.policy.from_host;
        let o = self
            .conn(conn)?
            .objects
            .get_mut(&offer)
            .ok_or_else(|| format!("unknown offer {offer}"))?;
        let iface = o.iface;
        let Role::Offer(state) = &mut o.role else {
            return Err(format!("{offer} isn't an offer"));
        };
        match state {
            OfferState::Pending(_) => {
                let given = owner.is_some() || from_host;
                let new = if given {
                    OfferState::Given { owner }
                } else {
                    OfferState::Hidden
                };
                let OfferState::Pending(held) = std::mem::replace(state, new) else {
                    unreachable!()
                };
                if given {
                    out.extend(held.into_iter().map(|(m, f)| Out::ToClient(conn, m, f)));
                } else {
                    wire::set_word(msg, arg.offset, 0);
                    let destroy = self.opcode(iface, true, "destroy");
                    out.push(Out::ToServer(
                        conn,
                        Builder::new(offer, destroy).build(),
                        Vec::new(),
                    ));
                }
            }
            OfferState::Given { .. } => {}
            OfferState::Hidden => wire::set_word(msg, arg.offset, 0),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
