//! gerant-net: creates a tap device with NAT for a user's VM. Installed
//! setuid root; gerantd runs it. See `gerant::net` for the protocol.
//!
//! In order, it
//! 1. becomes the calling user, keeping only CAP_NET_ADMIN and
//!    CAP_NET_BIND_SERVICE (privs.rs),
//! 2. opens every file and socket it will use,
//! 3. installs a seccomp filter: no more opening, and the calls that are
//!    left work only on those descriptors (seccomp.rs),
//! 4. reads the settings, creates the tap, configures it, enables IPv4
//!    and IPv6 forwarding and loads the firewall rules (netlink.rs),
//! 5. binds the DNS sockets to port 53 of the host's addresses on the tap,
//! 6. sends the tap's descriptor, the DNS sockets and the addresses to
//!    standard output, which has to be a Unix socket, and exits.
//!
//! gerantd forwards what guests send to the DNS sockets to the host's
//! resolver. Without them (port 53 is taken, say), the guest gets public
//! DNS servers instead, and the reason goes to standard error.
//!
//! Nothing the caller passes picks the tap's name or addresses: those come
//! from the next free /30 (and /64) in the subnets that root configured.
//! When IPv6 can't be set up on this host, the tap gets only IPv4, and the
//! reason goes to standard error, which gerantd logs. The tap isn't
//! persistent: the kernel removes it when the last descriptor is closed.

mod netlink;
mod privs;
mod seccomp;

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::process::ExitCode;

use gerant::net::{self, DnsSocket, NatTap, NatTap6, NetConfig};
use netlink::Netlink;
use seccomp::Rule;

const USAGE: &str = "\
usage: gerant-net nat

Creates a tap device with NAT for a VM and passes it to the caller over
standard output, which has to be a Unix socket. gerantd runs it; it is not
meant to be run by hand. The addresses come from /etc/garnish/network.conf.";

const REPLY_FD: RawFd = 1;

// From linux/sockios.h and linux/if_tun.h.
const SIOCGIFINDEX: libc::c_ulong = 0x8933;

/// struct ifreq, with the union as bytes.
#[repr(C)]
struct IfReq {
    name: [u8; libc::IFNAMSIZ],
    data: [u8; 24],
}

impl IfReq {
    fn new(name: &str) -> IfReq {
        let mut req = IfReq {
            name: [0; libc::IFNAMSIZ],
            data: [0; 24],
        };
        req.name[..name.len()].copy_from_slice(name.as_bytes());
        req
    }
}

/// What step 2 opens.
struct Opened {
    /// /etc/garnish/network.conf, if there is one.
    config: Option<String>,
    tun: File,
    ip_forward: File,
    /// None if the host has no IPv6 (ipv6.disable=1).
    ipv6: Option<Ipv6Forwarding>,
    rtnl: Netlink,
    nfnl: Netlink,
    /// Unbound UDP and TCP sockets, for IPv4 and (with ipv6) IPv6.
    dns4: Vec<DnsFd>,
    dns6: Vec<DnsFd>,
}

/// A socket for port 53. It is bound once the tap has its addresses.
struct DnsFd {
    kind: DnsSocket,
    fd: OwnedFd,
}

/// Where the guest's DNS queries go, on the host's side of the tap.
const DNS_PORT: u16 = 53;

/// /proc/sys/net/ipv6/conf/all/forwarding, twice: a write after a read
/// would be at a non-zero offset, which sysctl ignores.
struct Ipv6Forwarding {
    read: File,
    write: File,
}

fn main() -> ExitCode {
    // The default hook can read RUST_BACKTRACE and files; the filter would
    // kill us for the latter.
    std::panic::set_hook(Box::new(|info| eprintln!("gerant-net: {info}")));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["nat"] => {}
        ["--help" | "-h"] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gerant-net: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    privs::check_unix_socket(REPLY_FD)?;
    privs::drop_to_net_admin()?;
    let opened = open_all()?;
    seccomp::install(&rules(&opened))?;
    let config = net::parse_config(opened.config.as_deref())?;
    let mut tap = create_tap(&opened.tun, &config)?;
    let index = if_index(&opened.rtnl, &tap.name)?;
    netlink::configure_tap(&opened.rtnl, &tap, index)?;
    // NAT needs forwarding. Like libvirt, we turn it on and leave it on:
    // something else may need it too.
    (&opened.ip_forward)
        .write_all(b"1\n")
        .map_err(|e| format!("enabling IPv4 forwarding: {e}"))?;
    if let Some(v6) = tap.v6
        && let Err(why) = setup_ipv6(&opened, &tap.name, &v6, index)
    {
        eprintln!("gerant-net: {}: IPv4 only: {why}", tap.name);
        tap.v6 = None;
    }
    opened
        .nfnl
        .send(&netlink::nat_ruleset(&config))
        .map_err(|e| format!("loading nftables tables ip and ip6 {}: {e}", netlink::TABLE))?;

    let mut fds = vec![opened.tun.as_raw_fd()];
    match bind_dns(&opened.dns4, tap.host.into()) {
        Ok(()) => {
            let v6 = match tap.v6 {
                Some(v6) => bind_dns(&opened.dns6, v6.host.into())
                    .map_err(|e| eprintln!("gerant-net: {}: no DNS over IPv6: {e}", tap.name))
                    .is_ok(),
                None => false,
            };
            let bound = opened.dns4.iter().chain(opened.dns6.iter().filter(|_| v6));
            for s in bound {
                tap.dns.push(s.kind);
                fds.push(s.fd.as_raw_fd());
            }
        }
        Err(e) => eprintln!(
            "gerant-net: {}: no DNS forwarding, the guest gets public servers: {e}",
            tap.name
        ),
    }
    net::send_reply(REPLY_FD, &tap, &fds).map_err(|e| format!("sending the tap: {e}"))
}

/// Binds UDP and TCP to port 53 of `addr`, and has TCP listen.
fn bind_dns(sockets: &[DnsFd], addr: IpAddr) -> Result<(), String> {
    for s in sockets {
        let (storage, len) = sockaddr(addr, DNS_PORT);
        let fd = s.fd.as_raw_fd();
        if unsafe { libc::bind(fd, (&raw const storage).cast(), len) } != 0
            || s.kind == DnsSocket::Tcp && unsafe { libc::listen(fd, 16) } != 0
        {
            return Err(format!(
                "{} port {DNS_PORT} on {addr}: {}",
                match s.kind {
                    DnsSocket::Udp => "UDP",
                    DnsSocket::Tcp => "TCP",
                },
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn sockaddr(addr: IpAddr, port: u16) -> (libc::sockaddr_storage, libc::socklen_t) {
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let len = match addr {
        IpAddr::V4(a) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: port.to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(a.octets()),
                },
                sin_zero: [0; 8],
            };
            unsafe { (&raw mut storage).cast::<libc::sockaddr_in>().write(sin) };
            std::mem::size_of::<libc::sockaddr_in>()
        }
        IpAddr::V6(a) => {
            let sin6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: port.to_be(),
                sin6_flowinfo: 0,
                sin6_addr: libc::in6_addr {
                    s6_addr: a.octets(),
                },
                sin6_scope_id: 0,
            };
            unsafe { (&raw mut storage).cast::<libc::sockaddr_in6>().write(sin6) };
            std::mem::size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, len as libc::socklen_t)
}

/// An unbound UDP and TCP socket for `family`. TCP gets SO_REUSEADDR: a
/// tap's address comes back with its /30, maybe while connections of the
/// last VM that had it are still in TIME_WAIT.
fn open_dns(family: libc::c_int) -> Result<Vec<DnsFd>, String> {
    let mut sockets = Vec::new();
    for (kind, ty) in [
        (DnsSocket::Udp, libc::SOCK_DGRAM),
        (DnsSocket::Tcp, libc::SOCK_STREAM),
    ] {
        let fd = unsafe { libc::socket(family, ty | libc::SOCK_CLOEXEC, 0) };
        if fd < 0 {
            return Err(format!("DNS socket: {}", io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if kind == DnsSocket::Tcp {
            let one: libc::c_int = 1;
            let ret = unsafe {
                libc::setsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_REUSEADDR,
                    (&raw const one).cast(),
                    std::mem::size_of_val(&one) as libc::socklen_t,
                )
            };
            if ret != 0 {
                return Err(format!("DNS socket: {}", io::Error::last_os_error()));
            }
        }
        sockets.push(DnsFd { kind, fd });
    }
    Ok(sockets)
}

/// Puts the host's IPv6 address on the tap and makes sure forwarding is on.
/// An error means the guest gets only IPv4.
fn setup_ipv6(opened: &Opened, name: &str, v6: &NatTap6, index: u32) -> Result<(), String> {
    let Some(forwarding) = &opened.ipv6 else {
        return Err("the host has IPv6 turned off".into());
    };
    let mut current = [0u8; 4];
    let n = (&forwarding.read)
        .read(&mut current)
        .map_err(|e| format!("reading net.ipv6.conf.all.forwarding: {e}"))?;
    if current[..n].first() != Some(&b'1') {
        // Like libvirt: forwarding would make the kernel ignore router
        // advertisements, and the host would lose the routes they gave it.
        let at_risk = netlink::ra_dependent_interfaces(&opened.rtnl)?;
        if !at_risk.is_empty() {
            let sysctls: Vec<String> = at_risk
                .iter()
                .map(|i| format!("net.ipv6.conf.{i}.accept_ra=2"))
                .collect();
            return Err(format!(
                "turning on IPv6 forwarding would make the kernel ignore the router \
                 advertisements that the host's IPv6 routes on {} come from; set {} \
                 (or turn on net.ipv6.conf.all.forwarding yourself)",
                at_risk.join(", "),
                sysctls.join(" and ")
            ));
        }
    }
    netlink::configure_tap6(&opened.rtnl, v6, index)
        .map_err(|e| format!("adding {}/{} to {name}: {e}", v6.host, v6.prefix_len))?;
    (&forwarding.write)
        .write_all(b"1\n")
        .map_err(|e| format!("enabling IPv6 forwarding: {e}"))
}

fn open_all() -> Result<Opened, String> {
    let config = match File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(net::CONFIG_PATH)
    {
        Ok(mut file) => {
            // It decides what the host's addresses are: only root may write it.
            let meta = file
                .metadata()
                .map_err(|e| format!("{}: {e}", net::CONFIG_PATH))?;
            if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                return Err(format!(
                    "{}: has to be a file owned by root and writable only by root",
                    net::CONFIG_PATH
                ));
            }
            let mut text = String::new();
            file.read_to_string(&mut text)
                .map_err(|e| format!("{}: {e}", net::CONFIG_PATH))?;
            Some(text)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", net::CONFIG_PATH)),
    };
    let open = |path: &str, write_only: bool| {
        File::options()
            .read(!write_only)
            .write(true)
            .open(path)
            .map_err(|e| format!("{path}: {e}"))
    };
    const IPV6_FORWARDING: &str = "/proc/sys/net/ipv6/conf/all/forwarding";
    let ipv6 = match File::open(IPV6_FORWARDING) {
        Ok(read) => Some(Ipv6Forwarding {
            read,
            write: open(IPV6_FORWARDING, true)?,
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{IPV6_FORWARDING}: {e}")),
    };
    let dns6 = match ipv6 {
        Some(_) => open_dns(libc::AF_INET6)?,
        None => Vec::new(),
    };
    Ok(Opened {
        config,
        tun: open("/dev/net/tun", false)?,
        ip_forward: open("/proc/sys/net/ipv4/ip_forward", true)?,
        ipv6,
        rtnl: Netlink::open(libc::NETLINK_ROUTE)?,
        nfnl: Netlink::open(libc::NETLINK_NETFILTER)?,
        dns4: open_dns(libc::AF_INET)?,
        dns6,
    })
}

/// Everything the rest of the run needs, and nothing else.
fn rules(o: &Opened) -> Vec<Rule> {
    let (tun, forward, rtnl, nfnl) = (
        o.tun.as_raw_fd(),
        o.ip_forward.as_raw_fd(),
        o.rtnl.fd(),
        o.nfnl.fd(),
    );
    let mut rules = vec![
        Rule::ioctl(tun, &[libc::TUNSETIFF as libc::c_ulong]),
        Rule::ioctl(rtnl, &[SIOCGIFINDEX]),
        Rule::on(libc::SYS_sendmsg, rtnl),
        Rule::on(libc::SYS_sendmsg, nfnl),
        Rule::on(libc::SYS_sendmsg, REPLY_FD),
        // recv() is recvfrom().
        Rule::on(libc::SYS_recvfrom, rtnl),
        Rule::on(libc::SYS_recvfrom, nfnl),
        Rule::on(libc::SYS_write, forward),
        Rule::on(libc::SYS_write, libc::STDERR_FILENO),
        // Memory, closing what's open, and leaving.
        Rule::any(libc::SYS_brk),
        Rule::any(libc::SYS_mmap),
        Rule::any(libc::SYS_munmap),
        Rule::any(libc::SYS_mremap),
        Rule::any(libc::SYS_madvise),
        Rule::any(libc::SYS_close),
        // Debug builds check that a descriptor is open before closing it.
        Rule::with(libc::SYS_fcntl, libc::F_GETFD as u32),
        Rule::any(libc::SYS_sigaltstack),
        Rule::any(libc::SYS_rt_sigreturn),
        Rule::any(libc::SYS_exit),
        Rule::any(libc::SYS_exit_group),
    ];
    if let Some(v6) = &o.ipv6 {
        rules.push(Rule::on(libc::SYS_read, v6.read.as_raw_fd()));
        rules.push(Rule::on(libc::SYS_write, v6.write.as_raw_fd()));
    }
    for s in o.dns4.iter().chain(&o.dns6) {
        rules.push(Rule::on(libc::SYS_bind, s.fd.as_raw_fd()));
        if s.kind == DnsSocket::Tcp {
            rules.push(Rule::on(libc::SYS_listen, s.fd.as_raw_fd()));
        }
    }
    rules
}

/// Creates the tap of the lowest free /30. A tap's name is taken while it
/// exists, by our VMs or anyone's, so the kernel does the bookkeeping.
fn create_tap(tun: &File, config: &NetConfig) -> Result<NatTap, String> {
    for index in 0..config.slots() {
        let tap = config.slot(index);
        let mut req = IfReq::new(&tap.name);
        // IFF_TUN_EXCL: fail if it exists, instead of attaching to it.
        let flags =
            (libc::IFF_TAP | libc::IFF_NO_PI | libc::IFF_VNET_HDR | libc::IFF_TUN_EXCL) as i16;
        req.data[..2].copy_from_slice(&flags.to_ne_bytes());
        if unsafe { libc::ioctl(tun.as_raw_fd(), libc::TUNSETIFF as _, &mut req) } == 0 {
            return Ok(tap);
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(libc::EBUSY) {
            return Err(format!("creating {}: {e}", tap.name));
        }
    }
    Err(format!(
        "all {} networks in {}/{} are in use",
        config.slots(),
        config.subnet,
        config.prefix_len
    ))
}

fn if_index(rtnl: &Netlink, name: &str) -> Result<u32, String> {
    let mut req = IfReq::new(name);
    if unsafe { libc::ioctl(rtnl.fd(), SIOCGIFINDEX as _, &mut req) } != 0 {
        return Err(format!("{name}: {}", io::Error::last_os_error()));
    }
    Ok(i32::from_ne_bytes(req.data[..4].try_into().unwrap()) as u32)
}
