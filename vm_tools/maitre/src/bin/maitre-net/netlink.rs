//! Just enough netlink to put an address on a tap, bring it up and load an
//! nftables table, without running `ip` or `nft` as root.

use std::io;
use std::mem;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use maitre::net::{NatTap, NetConfig, TAP_PREFIX};

const NLMSG_HDRLEN: usize = 16;
const NLA_HDRLEN: usize = 4;
const NLA_F_NESTED: u16 = 0x8000;

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
        self.buf.extend_from_slice(&((NLA_HDRLEN + data.len()) as u16).to_ne_bytes());
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
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, protocol) };
        if fd < 0 {
            return Err(format!("netlink socket: {}", io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // Don't wait forever for a reply.
        let timeout = libc::timeval { tv_sec: 5, tv_usec: 0 };
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
        let mut iov = libc::iovec { iov_base: batch.buf.as_ptr().cast_mut().cast(), iov_len: batch.buf.len() };
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
                    let error = i32::from_ne_bytes(rest[NLMSG_HDRLEN..NLMSG_HDRLEN + 4].try_into().unwrap());
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
}

// rtnetlink, from linux/rtnetlink.h and linux/if_addr.h.
const RTM_NEWLINK: u16 = 16;
const RTM_NEWADDR: u16 = 20;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_BROADCAST: u16 = 4;

/// Puts the host's address on the tap and brings it up.
pub fn configure_tap(rtnl: &Netlink, tap: &NatTap, index: u32) -> Result<(), String> {
    let flags = (libc::NLM_F_REQUEST | libc::NLM_F_ACK) as u16;
    let mut batch = Batch::default();

    // struct ifaddrmsg
    let mut ifaddr = vec![libc::AF_INET as u8, tap.prefix_len, 0, libc::RT_SCOPE_UNIVERSE];
    ifaddr.extend_from_slice(&index.to_ne_bytes());
    batch.begin(RTM_NEWADDR, flags | (libc::NLM_F_CREATE | libc::NLM_F_EXCL) as u16, &ifaddr);
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

    rtnl.send(&batch).map_err(|e| format!("configuring {}: {e}", tap.name))
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

/// Our nftables table, `ip garnish`.
pub const TABLE: &str = "garnish";

/// One expression of a rule.
enum Expr<'a> {
    /// Interface name into register 1.
    Meta(u32),
    /// The IPv4 source address into register 1.
    SourceAddress,
    /// Register 1 &= mask.
    Mask(Ipv4Addr),
    /// Compares the start of register 1 with the bytes.
    Cmp(u32, &'a [u8]),
    Masquerade,
    Drop,
}

impl Batch {
    fn nft_begin(&mut self, ty: u16, flags: u16) {
        let nfgenmsg = [libc::NFPROTO_IPV4 as u8, 0, 0, 0];
        self.begin((libc::NFNL_SUBSYS_NFTABLES as u16) << 8 | ty, libc::NLM_F_REQUEST as u16 | flags, &nfgenmsg);
    }

    fn nft_table(&mut self, ty: u16, flags: u16) {
        self.nft_begin(ty, flags);
        self.attr_str(NFTA_TABLE_NAME, TABLE);
        self.end();
    }

    fn nft_chain(&mut self, name: &str, ty: &str, hook: u32, priority: i32) {
        self.nft_begin(NFT_MSG_NEWCHAIN, libc::NLM_F_CREATE as u16);
        self.attr_str(NFTA_CHAIN_TABLE, TABLE);
        self.attr_str(NFTA_CHAIN_NAME, name);
        self.nested(NFTA_CHAIN_HOOK, |b| {
            b.attr_be32(NFTA_HOOK_HOOKNUM, hook);
            b.attr_be32(NFTA_HOOK_PRIORITY, priority as u32);
        });
        self.attr_str(NFTA_CHAIN_TYPE, ty);
        self.end();
    }

    fn nft_rule(&mut self, chain: &str, exprs: &[Expr], flags: u16) {
        self.nft_begin(NFT_MSG_NEWRULE, (libc::NLM_F_CREATE | libc::NLM_F_APPEND) as u16 | flags);
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
            Expr::SourceAddress => "payload",
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
            Expr::SourceAddress => {
                b.attr_be32(NFTA_PAYLOAD_DREG, NFT_REG_1);
                b.attr_be32(NFTA_PAYLOAD_BASE, NFT_PAYLOAD_NETWORK_HEADER);
                b.attr_be32(NFTA_PAYLOAD_OFFSET, 12);
                b.attr_be32(NFTA_PAYLOAD_LEN, 4);
            }
            Expr::Mask(mask) => {
                b.attr_be32(NFTA_BITWISE_SREG, NFT_REG_1);
                b.attr_be32(NFTA_BITWISE_DREG, NFT_REG_1);
                b.attr_be32(NFTA_BITWISE_LEN, 4);
                b.nested(NFTA_BITWISE_MASK, |b| b.attr(NFTA_DATA_VALUE, &mask.octets()));
                b.nested(NFTA_BITWISE_XOR, |b| b.attr(NFTA_DATA_VALUE, &[0; 4]));
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
                    b.nested(NFTA_DATA_VERDICT, |b| b.attr_be32(NFTA_VERDICT_CODE, NF_DROP));
                });
            }
        });
    }
}

/// Replaces table `ip garnish`, in one transaction, with:
///
/// ```text
/// table ip garnish {
///     chain postrouting {
///         type nat hook postrouting priority srcnat;
///         ip saddr <subnet> oifname != "garnish*" masquerade
///     }
///     chain forward {
///         type filter hook forward priority filter;
///         iifname "garnish*" oifname "garnish*" drop
///         iifname "garnish*" ip saddr != <subnet> drop
///     }
/// }
/// ```
///
/// Guests reach the outside through the host's address, but not each
/// other, and can't send from addresses that aren't theirs.
pub fn nat_ruleset(config: &NetConfig) -> Batch {
    let subnet = config.subnet.octets();
    let taps = TAP_PREFIX.as_bytes();
    let mut batch = Batch::default();
    batch.begin(NFNL_MSG_BATCH_BEGIN, libc::NLM_F_REQUEST as u16, &[
        libc::AF_UNSPEC as u8,
        0,
        // res_id: the subsystem, big-endian
        0,
        libc::NFNL_SUBSYS_NFTABLES as u8,
    ]);
    batch.end();
    // Creating the table first makes the delete work if there was none.
    batch.nft_table(NFT_MSG_NEWTABLE, libc::NLM_F_CREATE as u16);
    batch.nft_table(NFT_MSG_DELTABLE, 0);
    batch.nft_table(NFT_MSG_NEWTABLE, libc::NLM_F_CREATE as u16);
    batch.nft_chain("postrouting", "nat", NF_INET_POST_ROUTING, NF_IP_PRI_NAT_SRC);
    batch.nft_chain("forward", "filter", NF_INET_FORWARD, NF_IP_PRI_FILTER);
    batch.nft_rule(
        "postrouting",
        &[
            Expr::SourceAddress,
            Expr::Mask(config.netmask()),
            Expr::Cmp(NFT_CMP_EQ, &subnet),
            Expr::Meta(NFT_META_OIFNAME),
            Expr::Cmp(NFT_CMP_NEQ, taps),
            Expr::Masquerade,
        ],
        0,
    );
    batch.nft_rule(
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
    // The last message asks for the ack that says the batch went in.
    batch.nft_rule(
        "forward",
        &[
            Expr::Meta(NFT_META_IIFNAME),
            Expr::Cmp(NFT_CMP_EQ, taps),
            Expr::SourceAddress,
            Expr::Mask(config.netmask()),
            Expr::Cmp(NFT_CMP_NEQ, &subnet),
            Expr::Drop,
        ],
        libc::NLM_F_ACK as u16,
    );
    batch.begin(NFNL_MSG_BATCH_END, libc::NLM_F_REQUEST as u16, &[libc::AF_UNSPEC as u8, 0, 0, libc::NFNL_SUBSYS_NFTABLES as u8]);
    batch.end();
    batch
}
