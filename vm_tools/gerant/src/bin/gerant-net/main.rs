//! gerant-net: creates a tap device with NAT for a user's VM. Installed
//! setuid root; gerantd runs it. See `gerant::net` for the protocol.
//!
//! In order, it
//! 1. becomes the calling user, keeping only CAP_NET_ADMIN (privs.rs),
//! 2. opens every file and socket it will use,
//! 3. installs a seccomp filter: no more opening, and the calls that are
//!    left work only on those descriptors (seccomp.rs),
//! 4. reads the settings, creates the tap, configures it, enables IPv4
//!    forwarding and loads the firewall rules (netlink.rs),
//! 5. sends the tap's descriptor and addresses to standard output, which
//!    has to be a Unix socket, and exits.
//!
//! Nothing the caller passes picks the tap's name or addresses: those come
//! from the next free /30 in the subnet that root configured. The tap isn't
//! persistent: the kernel removes it when the last descriptor is closed.

mod netlink;
mod privs;
mod seccomp;

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::process::ExitCode;

use gerant::net::{self, NatTap, NetConfig};
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
    rtnl: Netlink,
    nfnl: Netlink,
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
    let tap = create_tap(&opened.tun, &config)?;
    let index = if_index(&opened.rtnl, &tap.name)?;
    netlink::configure_tap(&opened.rtnl, &tap, index)?;
    // NAT needs forwarding. Like libvirt, we turn it on and leave it on:
    // something else may need it too.
    (&opened.ip_forward)
        .write_all(b"1\n")
        .map_err(|e| format!("enabling IPv4 forwarding: {e}"))?;
    opened
        .nfnl
        .send(&netlink::nat_ruleset(&config))
        .map_err(|e| format!("loading nftables table ip {}: {e}", netlink::TABLE))?;
    net::send_reply(REPLY_FD, &tap, opened.tun.as_raw_fd())
        .map_err(|e| format!("sending the tap: {e}"))
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
    Ok(Opened {
        config,
        tun: open("/dev/net/tun", false)?,
        ip_forward: open("/proc/sys/net/ipv4/ip_forward", true)?,
        rtnl: Netlink::open(libc::NETLINK_ROUTE)?,
        nfnl: Netlink::open(libc::NETLINK_NETFILTER)?,
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
    vec![
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
    ]
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
