//! The sockets: one thread polls the listening socket and every
//! connection, guest side and compositor side.

use std::collections::{HashMap, VecDeque};
use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};

use crate::filter::{ConnId, Filter, Out};
use crate::wire;

/// libwayland sends at most 28 descriptors at a time, and reads as many.
const MAX_FDS: usize = 28;
/// Stop reading a side while this much waits to go out on the other.
const BACKLOG: usize = 4 << 20;

#[derive(Default)]
struct Queue {
    bytes: Vec<u8>,
    /// Descriptors, each with where its message starts in `bytes`. They
    /// must arrive no later than the message's first byte.
    fds: VecDeque<(usize, OwnedFd)>,
}

impl Queue {
    fn push(&mut self, msg: Vec<u8>, fds: Vec<OwnedFd>) {
        let at = self.bytes.len();
        self.bytes.extend_from_slice(&msg);
        self.fds.extend(fds.into_iter().map(|f| (at, f)));
    }

    /// Writes what the socket takes. Ok(false) if it's full.
    fn flush(&mut self, sock: &UnixStream) -> io::Result<bool> {
        while !self.bytes.is_empty() {
            let mut n = self
                .fds
                .iter()
                .take_while(|(at, _)| *at < self.bytes.len())
                .count()
                .min(MAX_FDS);
            let end = match self.fds.get(MAX_FDS) {
                Some((at, _)) if n == MAX_FDS => *at,
                _ => self.bytes.len(),
            };
            if end == 0 {
                return Err(io::Error::other("a message with too many descriptors"));
            }
            n = n.min(self.fds.iter().take_while(|(at, _)| *at < end).count());
            let fds: Vec<_> = self.fds.iter().take(n).map(|(_, f)| f.as_fd()).collect();
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
            let mut anc = SendAncillaryBuffer::new(&mut space);
            if !fds.is_empty() {
                anc.push(SendAncillaryMessage::ScmRights(&fds));
            }
            let sent = match sendmsg(
                sock,
                &[IoSlice::new(&self.bytes[..end])],
                &mut anc,
                SendFlags::DONTWAIT | SendFlags::NOSIGNAL,
            ) {
                Ok(sent) => sent,
                Err(rustix::io::Errno::AGAIN) => return Ok(false),
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            };
            drop(fds);
            self.bytes.drain(..sent);
            self.fds.drain(..n);
            for (at, _) in self.fds.iter_mut() {
                *at -= sent;
            }
        }
        Ok(true)
    }
}

struct Side {
    sock: UnixStream,
    input: Vec<u8>,
    fds: VecDeque<OwnedFd>,
    out: Queue,
}

impl Side {
    fn new(sock: UnixStream) -> io::Result<Side> {
        sock.set_nonblocking(true)?;
        Ok(Side {
            sock,
            input: Vec::new(),
            fds: VecDeque::new(),
            out: Queue::default(),
        })
    }

    /// Reads what's there. Ok(false) at the end.
    fn fill(&mut self) -> io::Result<bool> {
        let mut buf = [0u8; 65536];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS * 2))];
        let mut anc = RecvAncillaryBuffer::new(&mut space);
        let flags = RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC;
        let got = match recvmsg(
            &self.sock,
            &mut [IoSliceMut::new(&mut buf)],
            &mut anc,
            flags,
        ) {
            Ok(r) => r.bytes,
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => return Ok(true),
            Err(e) => return Err(e.into()),
        };
        for m in anc.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = m {
                self.fds.extend(fds);
            }
        }
        self.input.extend_from_slice(&buf[..got]);
        Ok(got > 0)
    }
}

struct Conn {
    client: Side,
    server: Side,
}

pub struct Proxy {
    listener: UnixListener,
    display: PathBuf,
    filter: Filter<OwnedFd>,
    conns: HashMap<ConnId, Conn>,
    next: ConnId,
}

impl Proxy {
    pub fn new(
        listener: UnixListener,
        display: PathBuf,
        filter: Filter<OwnedFd>,
    ) -> io::Result<Proxy> {
        listener.set_nonblocking(true)?;
        Ok(Proxy {
            listener,
            display,
            filter,
            conns: HashMap::new(),
            next: 1,
        })
    }

    pub fn run(&mut self) -> io::Result<()> {
        loop {
            let ids: Vec<ConnId> = self.conns.keys().copied().collect();
            let mut pollfds = vec![PollFd::new(&self.listener, PollFlags::IN)];
            for id in &ids {
                let c = &self.conns[id];
                for (side, other) in [(&c.client, &c.server), (&c.server, &c.client)] {
                    let mut flags = PollFlags::empty();
                    if other.out.bytes.len() < BACKLOG && side.out.bytes.len() < BACKLOG {
                        flags |= PollFlags::IN;
                    }
                    if !side.out.bytes.is_empty() {
                        flags |= PollFlags::OUT;
                    }
                    pollfds.push(PollFd::new(&side.sock, flags));
                }
            }
            match poll(&mut pollfds, None) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
            let ready: Vec<PollFlags> = pollfds.iter().map(|p| p.revents()).collect();
            drop(pollfds);

            if !ready[0].is_empty() {
                self.accept();
            }
            for (i, id) in ids.iter().enumerate() {
                for (k, from_client) in [(1 + 2 * i, true), (2 + 2 * i, false)] {
                    let r = ready[k];
                    if r.is_empty() || !self.conns.contains_key(id) {
                        continue;
                    }
                    if let Err(e) = self.service(*id, from_client, r) {
                        self.close(*id, &e);
                    }
                }
            }
        }
    }

    fn accept(&mut self) {
        loop {
            let client = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) => {
                    eprintln!("videur: accepting a connection: {e}");
                    return;
                }
            };
            let id = self.next;
            self.next += 1;
            let conn = UnixStream::connect(&self.display).and_then(|server| {
                Ok(Conn {
                    client: Side::new(client)?,
                    server: Side::new(server)?,
                })
            });
            match conn {
                Ok(conn) => {
                    eprintln!("videur: connection {id}");
                    self.filter.open(id);
                    self.conns.insert(id, conn);
                }
                Err(e) => eprintln!("videur: connecting to {}: {e}", self.display.display()),
            }
        }
    }

    fn close(&mut self, id: ConnId, why: &str) {
        eprintln!("videur: connection {id} closed: {why}");
        self.conns.remove(&id);
        self.filter.close(id);
    }

    fn service(&mut self, id: ConnId, from_client: bool, ready: PollFlags) -> Result<(), String> {
        let who = if from_client { "guest" } else { "compositor" };
        let conn = self.conns.get_mut(&id).unwrap();
        let side = if from_client {
            &mut conn.client
        } else {
            &mut conn.server
        };
        if ready.contains(PollFlags::OUT) {
            side.out
                .flush(&side.sock)
                .map_err(|e| format!("writing to the {who}: {e}"))?;
        }
        if ready.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            if !side
                .fill()
                .map_err(|e| format!("reading from the {who}: {e}"))?
            {
                return Err(format!("the {who} hung up"));
            }
            self.process(id, from_client)?;
        }
        Ok(())
    }

    /// Hands the filter every whole message read from one side.
    fn process(&mut self, id: ConnId, from_client: bool) -> Result<(), String> {
        loop {
            let conn = self.conns.get_mut(&id).unwrap();
            let side = if from_client {
                &mut conn.client
            } else {
                &mut conn.server
            };
            let Some((_, _, size)) = wire::header(&side.input) else {
                break;
            };
            if size < wire::HEADER || size % 4 != 0 {
                return Err(format!("bad message size {size}"));
            }
            if side.input.len() < size {
                break;
            }
            let need = self.filter.fds_needed(id, from_client, &side.input)?;
            if side.fds.len() < need {
                break;
            }
            let msg: Vec<u8> = side.input.drain(..size).collect();
            let fds: Vec<OwnedFd> = side.fds.drain(..need).collect();
            for out in self.filter.handle(id, from_client, msg, fds)? {
                let (to, msg, fds, client) = match out {
                    Out::ToServer(c, m, f) => (c, m, f, false),
                    Out::ToClient(c, m, f) => (c, m, f, true),
                };
                // A connection that went away takes its messages with it.
                if let Some(c) = self.conns.get_mut(&to) {
                    let s = if client { &mut c.client } else { &mut c.server };
                    s.out.push(msg, fds);
                    let flushed = s.out.flush(&s.sock);
                    if let Err(e) = flushed {
                        if to == id {
                            return Err(format!("writing: {e}"));
                        }
                        self.close(to, &format!("writing: {e}"));
                    }
                }
            }
        }
        Ok(())
    }
}
