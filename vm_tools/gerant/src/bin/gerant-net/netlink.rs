//! Just enough netlink to put addresses on a tap, bring it up, look at the
//! host's IPv6 routes and load nftables tables, without running `ip` or
//! `nft` as root.

use std::io;
use std::mem;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use gerant::net::{NatTap, NatTap6, NetConfig, TAP_PREFIX};

const NLMSG_HDRLEN: usize = 16;
const NLA_HDRLEN: usize = 4;
const NLA_F_NESTED: u16 = 0x8000;
/// An attribute's type without the nested and byte-order flags.
const NLA_TYPE_MASK: u16 = 0x3fff;

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// One or more netlink messages in a buffer, sent with one sendmsg.
#[derive(Default)]
pub struct Batch {
    buf: Vec<u8>,
    msg_start: usize,
    seq: u32,
    /// Sequence numbers of the messages that asked for an ack.
    acked: Vec<u32>,
}

impl Batch {
    /// Starts a message; `header` is the family-specific header after
    /// nlmsghdr.
    fn begin(&mut self, ty: u16, flags: u16, header: &[u8]) {
        self.seq += 1;
        if flags & libc::NLM_F_ACK as u16 != 0 {
            self.acked.push(self.seq);
        }
        self.msg_start = self.buf.len();
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // nlmsg_len, set in end()
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(&flags.to_ne_bytes());
        self.buf.extend_from_slice(&self.seq.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes()); // nlmsg_pid: the kernel
        self.buf.extend_from_slice(header);
        self.pad();
    }

    fn end(&mut self) {
        let len = (self.buf.len() - self.msg_start) as u32;
        self.buf[self.msg_start..self.msg_start + 4].copy_from_slice(&len.to_ne_bytes());
    }

    fn pad(&mut self) {
        self.buf.resize(align4(self.buf.len()), 0);
    }

    fn attr(&mut self, ty: u16, data: &[u8]) {
        self.buf
            .extend_from_slice(&((NLA_HDRLEN + data.len()) as u16).to_ne_bytes());
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(data);
        self.pad();
    }

    fn attr_str(&mut self, ty: u16, s: &str) {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        self.attr(ty, &data);
    }

    /// nftables wants its numbers big-endian.
    fn attr_be32(&mut self, ty: u16, value: u32) {
        self.attr(ty, &value.to_be_bytes());
    }

    fn nested(&mut self, ty: u16, f: impl FnOnce(&mut Batch)) {
        let start = self.buf.len();
        self.attr(ty | NLA_F_NESTED, &[]);
        f(self);
        let len = (self.buf.len() - start) as u16;
        self.buf[start..start + 2].copy_from_slice(&len.to_ne_bytes());
    }
}

pub struct Netlink {
    fd: OwnedFd,
}

impl Netlink {
    pub fn open(protocol: libc::c_int) -> Result<Netlink, String> {
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                protocol,
            )
        };
        if fd < 0 {
            return Err(format!("netlink socket: {}", io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // Don't wait forever for a reply.
        let timeout = libc::timeval {
            tv_sec: 5,
            tv_usec: 0,
        };
        let ret = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const timeout).cast(),
                mem::size_of_val(&timeout) as libc::socklen_t,
            )
        };
        if ret < 0 {
            return Err(format!("netlink socket: {}", io::Error::last_os_error()));
        }
        Ok(Netlink { fd })
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Sends the batch and waits for the acks it asked for. The first error
    /// the kernel reports is returned.
    pub fn send(&self, batch: &Batch) -> Result<(), io::Error> {
        let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        let mut iov = libc::iovec {
            iov_base: batch.buf.as_ptr().cast_mut().cast(),
            iov_len: batch.buf.len(),
        };
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_name = (&raw mut addr).cast();
        msg.msg_namelen = mem::size_of_val(&addr) as libc::socklen_t;
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if unsafe { libc::sendmsg(self.fd(), &msg, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut pending = batch.acked.clone();
        let mut buf = vec![0u8; 8192];
        while !pending.is_empty() {
            let n = unsafe { libc::recv(self.fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut rest = &buf[..n as usize];
            while rest.len() >= NLMSG_HDRLEN {
                let len = u32::from_ne_bytes(rest[0..4].try_into().unwrap()) as usize;
                let ty = u16::from_ne_bytes(rest[4..6].try_into().unwrap());
                let seq = u32::from_ne_bytes(rest[8..12].try_into().unwrap());
                if len < NLMSG_HDRLEN || len > rest.len() {
                    return Err(io::Error::other("malformed netlink reply"));
                }
                if ty == libc::NLMSG_ERROR as u16 && len >= NLMSG_HDRLEN + 4 {
                    let error = i32::from_ne_bytes(
                        rest[NLMSG_HDRLEN..NLMSG_HDRLEN + 4].try_into().unwrap(),
                    );
                    if error != 0 {
                        return Err(io::Error::from_raw_os_error(-error));
                    }
                    pending.retain(|s| *s != seq);
                }
                rest = &rest[align4(len).min(rest.len())..];
            }
        }
        Ok(())
    }

    /// Sends one dump request and returns each reply message's payload, the
    /// part after nlmsghdr.
    pub fn dump(&self, ty: u16, header: &[u8]) -> Result<Vec<Vec<u8>>, io::Error> {
        let mut batch = Batch::default();
        batch.begin(ty, (libc::NLM_F_REQUEST | libc::NLM_F_DUMP) as u16, header);
        batch.end();
        let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        let mut iov = libc::iovec {
            iov_base: batch.buf.as_ptr().cast_mut().cast(),
            iov_len: batch.buf.len(),
        };
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_name = (&raw mut addr).cast();
        msg.msg_namelen = mem::size_of_val(&addr) as libc::socklen_t;
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if unsafe { libc::sendmsg(self.fd(), &msg, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut messages = Vec::new();
        // The kernel fills at most 32 KiB per read during a dump.
        let mut buf = vec![0u8; 65536];
        loop {
            let n = unsafe { libc::recv(self.fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut rest = &buf[..n as usize];
            while rest.len() >= NLMSG_HDRLEN {
                let len = u32::from_ne_bytes(rest[0..4].try_into().unwrap()) as usize;
                let ty = u16::from_ne_bytes(rest[4..6].try_into().unwrap());
                let seq = u32::from_ne_bytes(rest[8..12].try_into().unwrap());
                if len < NLMSG_HDRLEN || len > rest.len() {
                    return Err(io::Error::other("malformed netlink reply"));
                }
                if seq == batch.seq {
                    match ty as libc::c_int {
                        libc::NLMSG_DONE => return Ok(messages),
                        libc::NLMSG_ERROR if len >= NLMSG_HDRLEN + 4 => {
                            let error = i32::from_ne_bytes(
                                rest[NLMSG_HDRLEN..NLMSG_HDRLEN + 4].try_into().unwrap(),
                            );
                            return Err(if error != 0 {
                                io::Error::from_raw_os_error(-error)
                            } else {
                                io::Error::other("netlink dump ended without NLMSG_DONE")
                            });
                        }
                        _ => messages.push(rest[NLMSG_HDRLEN..len].to_vec()),
                    }
                }
                rest = &rest[align4(len).min(rest.len())..];
            }
        }
    }
}

/// The attributes after a message's fixed header, as (type, data).
fn attrs(mut rest: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while rest.len() >= NLA_HDRLEN {
        let len = u16::from_ne_bytes(rest[0..2].try_into().unwrap()) as usize;
        let ty = u16::from_ne_bytes(rest[2..4].try_into().unwrap());
        if len < NLA_HDRLEN || len > rest.len() {
            break;
        }
        out.push((ty & NLA_TYPE_MASK, &rest[NLA_HDRLEN..len]));
        rest = &rest[align4(len).min(rest.len())..];
    }
    out
}

// rtnetlink, from linux/rtnetlink.h and linux/if_addr.h.
const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_NEWADDR: u16 = 20;
const RTM_GETROUTE: u16 = 26;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_BROADCAST: u16 = 4;
const IFA_F_NODAD: u8 = 0x02;
const IFLA_IFNAME: u16 = 3;
const IFLA_PROTINFO: u16 = 12;
const IFLA_INET6_CONF: u16 = 2;
/// Index of accept_ra in IFLA_INET6_CONF, from enum DEVCONF_* in
/// linux/ipv6.h.
const DEVCONF_ACCEPT_RA: usize = 3;
const RTA_OIF: u16 = 4;
/// Routes learned from router advertisements, by the kernel or a daemon.
const RTPROT_RA: u8 = 9;
const IFINFOMSG_LEN: usize = 16;
const RTMSG_LEN: usize = 12;

/// Puts the host's address on the tap and brings it up.
pub fn configure_tap(rtnl: &Netlink, tap: &NatTap, index: u32) -> Result<(), String> {
    let flags = (libc::NLM_F_REQUEST | libc::NLM_F_ACK) as u16;
    let mut batch = Batch::default();

    // struct ifaddrmsg
    let mut ifaddr = vec![
        libc::AF_INET as u8,
        tap.prefix_len,
        0,
        libc::RT_SCOPE_UNIVERSE,
    ];
    ifaddr.extend_from_slice(&index.to_ne_bytes());
    batch.begin(
        RTM_NEWADDR,
        flags | (libc::NLM_F_CREATE | libc::NLM_F_EXCL) as u16,
        &ifaddr,
    );
    batch.attr(IFA_LOCAL, &tap.host.octets());
    batch.attr(IFA_ADDRESS, &tap.host.octets());
    let broadcast = u32::from(tap.host) | !u32::from(tap.netmask());
    batch.attr(IFA_BROADCAST, &Ipv4Addr::from(broadcast).octets());
    batch.end();

    // struct ifinfomsg: family, pad, type, index, flags, change
    let mut ifinfo = vec![libc::AF_UNSPEC as u8, 0, 0, 0];
    ifinfo.extend_from_slice(&(index as i32).to_ne_bytes());
    ifinfo.extend_from_slice(&(libc::IFF_UP as u32).to_ne_bytes());
    ifinfo.extend_from_slice(&(libc::IFF_UP as u32).to_ne_bytes());
    batch.begin(RTM_NEWLINK, flags, &ifinfo);
    batch.end();

    rtnl.send(&batch)
        .map_err(|e| format!("configuring {}: {e}", tap.name))
}

/// Puts the host's IPv6 address on the tap. No duplicate address
/// detection: the network is the tap's alone, and the address would be
/// unusable for a second while it ran.
pub fn configure_tap6(rtnl: &Netlink, v6: &NatTap6, index: u32) -> Result<(), io::Error> {
    let flags =
        (libc::NLM_F_REQUEST | libc::NLM_F_ACK | libc::NLM_F_CREATE | libc::NLM_F_EXCL) as u16;
    let mut batch = Batch::default();
    let mut ifaddr = vec![
        libc::AF_INET6 as u8,
        v6.prefix_len,
        IFA_F_NODAD,
        libc::RT_SCOPE_UNIVERSE,
    ];
    ifaddr.extend_from_slice(&index.to_ne_bytes());
    batch.begin(RTM_NEWADDR, flags, &ifaddr);
    batch.attr(IFA_ADDRESS, &v6.host.octets());
    batch.end();
    rtnl.send(&batch)
}

/// The interfaces whose IPv6 routes come from router advertisements that
/// the kernel handles itself (accept_ra=1). Once forwarding is on, the
/// kernel ignores those advertisements, and the routes expire. accept_ra=2
/// keeps them; NetworkManager and systemd-networkd set 0 and handle them
/// themselves, which forwarding doesn't change.
pub fn ra_dependent_interfaces(rtnl: &Netlink) -> Result<Vec<String>, String> {
    let dump_error = |what: &str, e: io::Error| format!("listing the host's IPv6 {what}: {e}");
    let mut rtmsg = [0u8; RTMSG_LEN];
    rtmsg[0] = libc::AF_INET6 as u8;
    let mut ra_ifs: Vec<u32> = Vec::new();
    for m in rtnl
        .dump(RTM_GETROUTE, &rtmsg)
        .map_err(|e| dump_error("routes", e))?
    {
        if m.len() < RTMSG_LEN || m[5] != RTPROT_RA {
            continue;
        }
        for (ty, data) in attrs(&m[RTMSG_LEN..]) {
            if ty == RTA_OIF && data.len() == 4 {
                let oif = u32::from_ne_bytes(data.try_into().unwrap());
                if !ra_ifs.contains(&oif) {
                    ra_ifs.push(oif);
                }
            }
        }
    }
    if ra_ifs.is_empty() {
        return Ok(Vec::new());
    }

    let mut ifinfo = [0u8; IFINFOMSG_LEN];
    ifinfo[0] = libc::AF_INET6 as u8;
    let mut names = Vec::new();
    for m in rtnl
        .dump(RTM_GETLINK, &ifinfo)
        .map_err(|e| dump_error("interfaces", e))?
    {
        if m.len() < IFINFOMSG_LEN {
            continue;
        }
        let index = i32::from_ne_bytes(m[4..8].try_into().unwrap()) as u32;
        if !ra_ifs.contains(&index) {
            continue;
        }
        let (mut name, mut accept_ra) = (None, None);
        for (ty, data) in attrs(&m[IFINFOMSG_LEN..]) {
            match ty {
                IFLA_IFNAME => {
                    let data = data.split(|b| *b == 0).next().unwrap_or_default();
                    name = Some(String::from_utf8_lossy(data).into_owned());
                }
                IFLA_PROTINFO => {
                    for (ty, conf) in attrs(data) {
                        let at = DEVCONF_ACCEPT_RA * 4;
                        if ty == IFLA_INET6_CONF && conf.len() >= at + 4 {
                            accept_ra =
                                Some(i32::from_ne_bytes(conf[at..at + 4].try_into().unwrap()));
                        }
                    }
                }
                _ => {}
            }
        }
        if accept_ra == Some(1) {
            names.push(name.unwrap_or_else(|| format!("interface {index}")));
        }
    }
    Ok(names)
}

// nftables, from linux/netfilter/nf_tables.h and linux/netfilter/nfnetlink.h.
const NFNL_MSG_BATCH_BEGIN: u16 = 0x10;
const NFNL_MSG_BATCH_END: u16 = 0x11;
const NFT_MSG_NEWTABLE: u16 = 0;
const NFT_MSG_DELTABLE: u16 = 2;
const NFT_MSG_NEWCHAIN: u16 = 3;
const NFT_MSG_NEWRULE: u16 = 6;
const NFTA_TABLE_NAME: u16 = 1;
const NFTA_CHAIN_TABLE: u16 = 1;
const NFTA_CHAIN_NAME: u16 = 3;
const NFTA_CHAIN_HOOK: u16 = 4;
const NFTA_CHAIN_TYPE: u16 = 7;
const NFTA_HOOK_HOOKNUM: u16 = 1;
const NFTA_HOOK_PRIORITY: u16 = 2;
const NFTA_RULE_TABLE: u16 = 1;
const NFTA_RULE_CHAIN: u16 = 2;
const NFTA_RULE_EXPRESSIONS: u16 = 4;
const NFTA_LIST_ELEM: u16 = 1;
const NFTA_EXPR_NAME: u16 = 1;
const NFTA_EXPR_DATA: u16 = 2;
const NFTA_META_DREG: u16 = 1;
const NFTA_META_KEY: u16 = 2;
const NFT_META_IIFNAME: u32 = 6;
const NFT_META_OIFNAME: u32 = 7;
const NFTA_CMP_SREG: u16 = 1;
const NFTA_CMP_OP: u16 = 2;
const NFTA_CMP_DATA: u16 = 3;
const NFT_CMP_EQ: u32 = 0;
const NFT_CMP_NEQ: u32 = 1;
const NFTA_DATA_VALUE: u16 = 1;
const NFTA_DATA_VERDICT: u16 = 2;
const NFTA_VERDICT_CODE: u16 = 1;
const NFTA_IMMEDIATE_DREG: u16 = 1;
const NFTA_IMMEDIATE_DATA: u16 = 2;
const NFTA_PAYLOAD_DREG: u16 = 1;
const NFTA_PAYLOAD_BASE: u16 = 2;
const NFTA_PAYLOAD_OFFSET: u16 = 3;
const NFTA_PAYLOAD_LEN: u16 = 4;
const NFT_PAYLOAD_NETWORK_HEADER: u32 = 1;
/// Where the source address is in an IPv4 and an IPv6 header.
const IPV4_SADDR: (u32, u32) = (12, 4);
const IPV6_SADDR: (u32, u32) = (8, 16);
const NFTA_BITWISE_SREG: u16 = 1;
const NFTA_BITWISE_DREG: u16 = 2;
const NFTA_BITWISE_LEN: u16 = 3;
const NFTA_BITWISE_MASK: u16 = 4;
const NFTA_BITWISE_XOR: u16 = 5;
const NFT_REG_VERDICT: u32 = 0;
const NFT_REG_1: u32 = 1;
const NF_DROP: u32 = 0;
const NF_INET_FORWARD: u32 = 2;
const NF_INET_POST_ROUTING: u32 = 4;
const NF_IP_PRI_FILTER: i32 = 0;
const NF_IP_PRI_NAT_SRC: i32 = 100;

/// Our nftables tables, `ip garnish` and `ip6 garnish`.
pub const TABLE: &str = "garnish";

/// One expression of a rule.
enum Expr<'a> {
    /// Interface name into register 1.
    Meta(u32),
    /// The source address into register 1: its offset in the network
    /// header and its length.
    SourceAddress((u32, u32)),
    /// Register 1 &= mask.
    Mask(&'a [u8]),
    /// Compares the start of register 1 with the bytes.
    Cmp(u32, &'a [u8]),
    Masquerade,
    Drop,
}

impl Batch {
    /// `family` is NFPROTO_IPV4 or NFPROTO_IPV6.
    fn nft_begin(&mut self, family: u8, ty: u16, flags: u16) {
        let nfgenmsg = [family, 0, 0, 0];
        self.begin(
            (libc::NFNL_SUBSYS_NFTABLES as u16) << 8 | ty,
            libc::NLM_F_REQUEST as u16 | flags,
            &nfgenmsg,
        );
    }

    fn nft_table(&mut self, family: u8, ty: u16, flags: u16) {
        self.nft_begin(family, ty, flags);
        self.attr_str(NFTA_TABLE_NAME, TABLE);
        self.end();
    }

    fn nft_chain(&mut self, family: u8, name: &str, ty: &str, hook: u32, priority: i32) {
        self.nft_begin(family, NFT_MSG_NEWCHAIN, libc::NLM_F_CREATE as u16);
        self.attr_str(NFTA_CHAIN_TABLE, TABLE);
        self.attr_str(NFTA_CHAIN_NAME, name);
        self.nested(NFTA_CHAIN_HOOK, |b| {
            b.attr_be32(NFTA_HOOK_HOOKNUM, hook);
            b.attr_be32(NFTA_HOOK_PRIORITY, priority as u32);
        });
        self.attr_str(NFTA_CHAIN_TYPE, ty);
        self.end();
    }

    fn nft_rule(&mut self, family: u8, chain: &str, exprs: &[Expr], flags: u16) {
        self.nft_begin(
            family,
            NFT_MSG_NEWRULE,
            (libc::NLM_F_CREATE | libc::NLM_F_APPEND) as u16 | flags,
        );
        self.attr_str(NFTA_RULE_TABLE, TABLE);
        self.attr_str(NFTA_RULE_CHAIN, chain);
        self.nested(NFTA_RULE_EXPRESSIONS, |b| {
            for expr in exprs {
                b.nested(NFTA_LIST_ELEM, |b| b.nft_expr(expr));
            }
        });
        self.end();
    }

    fn nft_expr(&mut self, expr: &Expr) {
        let name = match expr {
            Expr::Meta(_) => "meta",
            Expr::SourceAddress(_) => "payload",
            Expr::Mask(_) => "bitwise",
            Expr::Cmp(..) => "cmp",
            Expr::Masquerade => "masq",
            Expr::Drop => "immediate",
        };
        self.attr_str(NFTA_EXPR_NAME, name);
        self.nested(NFTA_EXPR_DATA, |b| match expr {
            Expr::Meta(key) => {
                b.attr_be32(NFTA_META_DREG, NFT_REG_1);
                b.attr_be32(NFTA_META_KEY, *key);
            }
            Expr::SourceAddress((offset, len)) => {
                b.attr_be32(NFTA_PAYLOAD_DREG, NFT_REG_1);
                b.attr_be32(NFTA_PAYLOAD_BASE, NFT_PAYLOAD_NETWORK_HEADER);
                b.attr_be32(NFTA_PAYLOAD_OFFSET, *offset);
                b.attr_be32(NFTA_PAYLOAD_LEN, *len);
            }
            Expr::Mask(mask) => {
                b.attr_be32(NFTA_BITWISE_SREG, NFT_REG_1);
                b.attr_be32(NFTA_BITWISE_DREG, NFT_REG_1);
                b.attr_be32(NFTA_BITWISE_LEN, mask.len() as u32);
                b.nested(NFTA_BITWISE_MASK, |b| b.attr(NFTA_DATA_VALUE, mask));
                b.nested(NFTA_BITWISE_XOR, |b| {
                    b.attr(NFTA_DATA_VALUE, &vec![0; mask.len()])
                });
            }
            Expr::Cmp(op, data) => {
                b.attr_be32(NFTA_CMP_SREG, NFT_REG_1);
                b.attr_be32(NFTA_CMP_OP, *op);
                b.nested(NFTA_CMP_DATA, |b| b.attr(NFTA_DATA_VALUE, data));
            }
            Expr::Masquerade => {}
            Expr::Drop => {
                b.attr_be32(NFTA_IMMEDIATE_DREG, NFT_REG_VERDICT);
                b.nested(NFTA_IMMEDIATE_DATA, |b| {
                    b.nested(NFTA_DATA_VERDICT, |b| {
                        b.attr_be32(NFTA_VERDICT_CODE, NF_DROP)
                    });
                });
            }
        });
    }
}

/// One family's table: masquerade for the guests' network, and no
/// forwarding between taps or from addresses that aren't the guests'.
struct NatTable<'a> {
    family: u8,
    saddr: (u32, u32),
    /// The guests' network and its mask. None: guests can't send through
    /// the host at all in this family.
    subnet: Option<(&'a [u8], &'a [u8])>,
}

impl Batch {
    /// Replaces the table, in the batch's transaction.
    fn nat_table(&mut self, t: &NatTable) {
        let taps = TAP_PREFIX.as_bytes();
        let f = t.family;
        // Creating the table first makes the delete work if there was none.
        self.nft_table(f, NFT_MSG_NEWTABLE, libc::NLM_F_CREATE as u16);
        self.nft_table(f, NFT_MSG_DELTABLE, 0);
        self.nft_table(f, NFT_MSG_NEWTABLE, libc::NLM_F_CREATE as u16);
        self.nft_chain(f, "forward", "filter", NF_INET_FORWARD, NF_IP_PRI_FILTER);
        let Some((subnet, mask)) = t.subnet else {
            self.nft_rule(
                f,
                "forward",
                &[
                    Expr::Meta(NFT_META_IIFNAME),
                    Expr::Cmp(NFT_CMP_EQ, taps),
                    Expr::Drop,
                ],
                0,
            );
            return;
        };
        self.nft_chain(
            f,
            "postrouting",
            "nat",
            NF_INET_POST_ROUTING,
            NF_IP_PRI_NAT_SRC,
        );
        self.nft_rule(
            f,
            "postrouting",
            &[
                Expr::SourceAddress(t.saddr),
                Expr::Mask(mask),
                Expr::Cmp(NFT_CMP_EQ, subnet),
                Expr::Meta(NFT_META_OIFNAME),
                Expr::Cmp(NFT_CMP_NEQ, taps),
                Expr::Masquerade,
            ],
            0,
        );
        self.nft_rule(
            f,
            "forward",
            &[
                Expr::Meta(NFT_META_IIFNAME),
                Expr::Cmp(NFT_CMP_EQ, taps),
                Expr::Meta(NFT_META_OIFNAME),
                Expr::Cmp(NFT_CMP_EQ, taps),
                Expr::Drop,
            ],
            0,
        );
        self.nft_rule(
            f,
            "forward",
            &[
                Expr::Meta(NFT_META_IIFNAME),
                Expr::Cmp(NFT_CMP_EQ, taps),
                Expr::SourceAddress(t.saddr),
                Expr::Mask(mask),
                Expr::Cmp(NFT_CMP_NEQ, subnet),
                Expr::Drop,
            ],
            0,
        );
    }
}

/// Replaces tables `ip garnish` and `ip6 garnish`, in one transaction,
/// with:
///
/// ```text
/// table ip garnish {
///     chain forward {
///         type filter hook forward priority filter;
///         iifname "garnish*" oifname "garnish*" drop
///         iifname "garnish*" ip saddr != <subnet> drop
///     }
///     chain postrouting {
///         type nat hook postrouting priority srcnat;
///         ip saddr <subnet> oifname != "garnish*" masquerade
///     }
/// }
/// table ip6 garnish {
///     # the same, with ip6 saddr <subnet6>; without Subnet6 only:
///     chain forward {
///         type filter hook forward priority filter;
///         iifname "garnish*" drop
///     }
/// }
/// ```
///
/// Guests reach the outside through the host's address, but not each
/// other, and can't send from addresses that aren't theirs. The tables
/// depend only on the settings, so every tap gets the same ones.
pub fn nat_ruleset(config: &NetConfig) -> Batch {
    let subnet = config.subnet.octets();
    let mask = config.netmask().octets();
    let v6 = config.subnet6.map(|p| (p.addr.octets(), p.mask().octets()));
    let mut batch = Batch::default();
    let batch_header = [
        libc::AF_UNSPEC as u8,
        0,
        // res_id: the subsystem, big-endian
        0,
        libc::NFNL_SUBSYS_NFTABLES as u8,
    ];
    batch.begin(
        NFNL_MSG_BATCH_BEGIN,
        libc::NLM_F_REQUEST as u16,
        &batch_header,
    );
    batch.end();
    batch.nat_table(&NatTable {
        family: libc::NFPROTO_IPV4 as u8,
        saddr: IPV4_SADDR,
        subnet: Some((&subnet, &mask)),
    });
    batch.nat_table(&NatTable {
        family: libc::NFPROTO_IPV6 as u8,
        saddr: IPV6_SADDR,
        subnet: v6.as_ref().map(|(a, m)| (&a[..], &m[..])),
    });
    // The batch's last message asks for the ack that says it went in.
    batch.nft_table(
        libc::NFPROTO_IPV6 as u8,
        NFT_MSG_NEWTABLE,
        (libc::NLM_F_CREATE | libc::NLM_F_ACK) as u16,
    );
    batch.begin(
        NFNL_MSG_BATCH_END,
        libc::NLM_F_REQUEST as u16,
        &batch_header,
    );
    batch.end();
    batch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes() {
        let mut b = Batch::default();
        b.attr_str(3, "eth0");
        b.nested(12, |b| b.attr(2, &[1, 2, 3, 4, 5]));
        let a = attrs(&b.buf);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0], (3, &b"eth0\0"[..]));
        // The nested flag is masked off.
        assert_eq!(a[1].0, 12);
        assert_eq!(attrs(a[1].1), [(2, &[1, 2, 3, 4, 5][..])]);
    }

    /// Dumps need no privileges: this runs the real thing.
    #[test]
    fn dumps() {
        let rtnl = Netlink::open(libc::NETLINK_ROUTE).unwrap();
        let mut ifinfo = [0u8; IFINFOMSG_LEN];
        ifinfo[0] = libc::AF_INET6 as u8;
        let links = rtnl.dump(RTM_GETLINK, &ifinfo).unwrap();
        // At least lo, with its accept_ra.
        assert!(links.iter().any(|m| {
            attrs(&m[IFINFOMSG_LEN..])
                .iter()
                .any(|(ty, data)| *ty == IFLA_IFNAME && *data == b"lo\0")
        }));
        ra_dependent_interfaces(&rtnl).unwrap();
    }
}
