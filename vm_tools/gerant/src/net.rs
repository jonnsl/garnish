//! Guest networks. Only root can create taps, so gerantd asks gerant-net, a
//! small setuid-root helper, for one. The helper creates it, puts the host's
//! address on it, sets up NAT and passes the open tap back over a socket.
//! crosvm gets that file descriptor, so the tap exists exactly as long as
//! the VM: when the last descriptor closes, the kernel removes it.
//!
//! This module is shared by both sides: the system-wide settings, which
//! only root can change, the helper's reply and gerantd's side of the call.

use std::io;
use std::mem;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::ini::Entry;

/// The helper's name, looked up in $PATH.
pub const HELPER: &str = "gerant-net";
/// Settings for every user's VMs. Read by the helper, as root.
pub const CONFIG_PATH: &str = "/etc/garnish/network.conf";
/// Taps are `<TAP_PREFIX><n>` for the n-th /30 in the subnet. Also matched
/// by the firewall rules.
pub const TAP_PREFIX: &str = "garnish";

/// The guest's side of `[Network] Mode=passt`. Every VM has a passt of its
/// own, so they can all have the same addresses.
pub const PASST_GUEST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
pub const PASST_GATEWAY: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
pub const PASST_NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);

/// `/etc/garnish/network.conf`.
#[derive(Debug, Clone, PartialEq)]
pub struct NetConfig {
    /// Split into /30 networks, one per VM: the host has the first address
    /// of each and the guest the second.
    pub subnet: Ipv4Addr,
    pub prefix_len: u8,
}

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig {
            subnet: Ipv4Addr::new(192, 168, 10, 0),
            prefix_len: 24,
        }
    }
}

impl NetConfig {
    pub fn from_entries(entries: &[Entry]) -> Result<NetConfig, String> {
        let mut config = NetConfig::default();
        for e in entries {
            match (e.section.as_str(), e.key.as_str()) {
                ("NAT", "Subnet") if e.value.is_empty() => config = NetConfig::default(),
                ("NAT", "Subnet") => {
                    let parsed = e.value.split_once('/').and_then(|(addr, len)| {
                        Some((
                            addr.parse::<Ipv4Addr>().ok()?,
                            len.parse::<u8>().ok().filter(|l| (16..=30).contains(l))?,
                        ))
                    });
                    let Some((subnet, prefix_len)) = parsed else {
                        return Err(e.error(
                            "expected an IPv4 network from /16 to /30, like 192.168.10.0/24",
                        ));
                    };
                    if u32::from(subnet) & !mask(prefix_len) != 0 {
                        return Err(e.error("host bits are set"));
                    }
                    config = NetConfig { subnet, prefix_len };
                }
                _ => {
                    return Err(format!(
                        "{}:{}: unknown setting [{}] {}=",
                        e.file.display(),
                        e.line,
                        e.section,
                        e.key
                    ));
                }
            }
        }
        Ok(config)
    }

    /// Number of /30 networks in the subnet.
    pub fn slots(&self) -> u32 {
        1 << (30 - self.prefix_len)
    }

    /// The n-th /30.
    pub fn slot(&self, index: u32) -> NatTap {
        let base = u32::from(self.subnet) + 4 * index;
        NatTap {
            name: format!("{TAP_PREFIX}{index}"),
            host: Ipv4Addr::from(base + 1),
            guest: Ipv4Addr::from(base + 2),
            prefix_len: 30,
        }
    }

    pub fn netmask(&self) -> Ipv4Addr {
        Ipv4Addr::from(mask(self.prefix_len))
    }
}

fn mask(prefix_len: u8) -> u32 {
    u32::MAX
        .checked_shl(32 - u32::from(prefix_len))
        .unwrap_or(0)
}

/// A tap with NAT to the outside: the helper's reply, next to the tap's
/// file descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct NatTap {
    pub name: String,
    pub host: Ipv4Addr,
    pub guest: Ipv4Addr,
    pub prefix_len: u8,
}

impl NatTap {
    pub fn netmask(&self) -> Ipv4Addr {
        Ipv4Addr::from(mask(self.prefix_len))
    }

    /// `key=value` lines.
    pub fn encode(&self) -> String {
        format!(
            "tap={}\nhost={}\nguest={}\nprefix={}\n",
            self.name, self.host, self.guest, self.prefix_len
        )
    }

    pub fn decode(text: &str) -> Result<NatTap, String> {
        let (mut name, mut host, mut guest, mut prefix_len) = (None, None, None, None);
        for line in text.lines() {
            let bad = || format!("bad line {line:?}");
            let (key, value) = line.split_once('=').ok_or_else(bad)?;
            match key {
                "tap" => name = Some(value.to_owned()),
                "host" => host = Some(value.parse().map_err(|_| bad())?),
                "guest" => guest = Some(value.parse().map_err(|_| bad())?),
                "prefix" => {
                    prefix_len = Some(value.parse().ok().filter(|l| *l <= 32).ok_or_else(bad)?)
                }
                // Room for more in later versions.
                _ => {}
            }
        }
        match (name, host, guest, prefix_len) {
            (Some(name), Some(host), Some(guest), Some(prefix_len)) => Ok(NatTap {
                name,
                host,
                guest,
                prefix_len,
            }),
            _ => Err(format!("incomplete reply {text:?}")),
        }
    }
}

/// A tap of our own: it goes away when the last copy of `fd` is closed.
#[derive(Debug)]
pub struct Tap {
    pub info: NatTap,
    pub fd: OwnedFd,
}

/// The first `gerant-net` in $PATH that is setuid root. A copy that isn't,
/// such as the one `cargo install` puts in ~/.cargo/bin, is skipped.
fn find_helper() -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut unprivileged = None;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(HELPER);
        let Ok(meta) = candidate.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.uid() == 0 && meta.permissions().mode() & 0o4000 != 0 {
            return Ok(candidate);
        }
        unprivileged.get_or_insert(candidate);
    }
    Err(match unprivileged {
        Some(path) => format!(
            "{} isn't setuid root, so it can't create network devices; install it with \
             `sudo install -o root -g root -m 4755 {} /usr/local/bin/`",
            path.display(),
            path.display()
        ),
        None => format!("{HELPER} isn't installed (it creates the network devices)"),
    })
}

/// Asks the helper for a tap with NAT.
pub async fn nat_tap() -> Result<Tap, String> {
    run_helper(&find_helper()?).await
}

async fn run_helper(helper: &Path) -> Result<Tap, String> {
    let (ours, theirs) = seqpacket_pair().map_err(|e| format!("socketpair: {e}"))?;
    // Not Command::output(): tokio's replaces stdout with a pipe.
    let child = tokio::process::Command::new(helper)
        .arg("nat")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::from(theirs))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running {}: {e}", helper.display()))?;
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("running {}: {e}", helper.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = stderr.trim().trim_start_matches("gerant-net: ");
        return Err(if msg.is_empty() {
            format!("{HELPER} failed ({})", output.status)
        } else {
            msg.to_owned()
        });
    }
    recv_reply(&ours).map_err(|e| format!("{HELPER}: {e}"))
}

/// gerantd's side of `send_reply`.
fn recv_reply(sock: &OwnedFd) -> Result<Tap, String> {
    let (text, fd) = recv_with_fd(sock).map_err(|e| format!("reading the reply: {e}"))?;
    let fd = fd.ok_or("no tap in the reply")?;
    Ok(Tap {
        info: NatTap::decode(&text)?,
        fd,
    })
}

fn seqpacket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// The helper's side: sends the addresses with the tap's descriptor.
pub fn send_reply(sock: RawFd, tap: &NatTap, fd: RawFd) -> io::Result<()> {
    let text = tap.encode();
    let mut control = [0u64; 4];
    let mut iov = libc::iovec {
        iov_base: text.as_ptr().cast_mut().cast(),
        iov_len: text.len(),
    };
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) } as _;
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of::<RawFd>() as u32) as _;
        libc::CMSG_DATA(cmsg).cast::<RawFd>().write_unaligned(fd);
    }
    if unsafe { libc::sendmsg(sock, &msg, libc::MSG_NOSIGNAL) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Reads the reply the helper left in the socket before it exited.
fn recv_with_fd(sock: &OwnedFd) -> io::Result<(String, Option<OwnedFd>)> {
    let mut data = [0u8; 1024];
    // Room for one descriptor, as u64s for the alignment cmsghdr needs.
    let mut control = [0u64; 8];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = mem::size_of_val(&control);
    let n = unsafe {
        libc::recvmsg(
            sock.as_raw_fd(),
            &mut msg,
            libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fd = None;
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    while !cmsg.is_null() {
        let c = unsafe { &*cmsg };
        if c.cmsg_level == libc::SOL_SOCKET && c.cmsg_type == libc::SCM_RIGHTS {
            let count = (c.cmsg_len as usize - unsafe { libc::CMSG_LEN(0) } as usize)
                / mem::size_of::<RawFd>();
            let fds = unsafe { libc::CMSG_DATA(cmsg) }.cast::<RawFd>();
            for i in 0..count {
                // Keep the first, close any others.
                let owned = unsafe { OwnedFd::from_raw_fd(fds.add(i).read_unaligned()) };
                fd.get_or_insert(owned);
            }
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(&msg, cmsg) };
    }
    if msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err(io::Error::other("reply truncated"));
    }
    Ok((
        String::from_utf8_lossy(&data[..n as usize]).into_owned(),
        fd,
    ))
}

/// Reads `CONFIG_PATH` from an open file, or the defaults if there is none.
pub fn parse_config(text: Option<&str>) -> Result<NetConfig, String> {
    match text {
        Some(text) => NetConfig::from_entries(&crate::ini::parse(text, Path::new(CONFIG_PATH))?),
        None => Ok(NetConfig::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Result<NetConfig, String> {
        parse_config(Some(text))
    }

    #[test]
    fn settings() {
        assert_eq!(config("").unwrap(), NetConfig::default());
        assert_eq!(parse_config(None).unwrap().slots(), 64);
        let c = config("[NAT]\nSubnet=10.0.0.0/28\n").unwrap();
        assert_eq!(
            (c.subnet, c.prefix_len, c.slots()),
            (Ipv4Addr::new(10, 0, 0, 0), 28, 4)
        );
        assert_eq!(c.netmask(), Ipv4Addr::new(255, 255, 255, 240));
        assert!(
            config("[NAT]\nSubnet=10.0.0.1/24\n")
                .unwrap_err()
                .contains("host bits")
        );
        assert!(config("[NAT]\nSubnet=10.0.0.0/31\n").is_err());
        assert!(config("[NAT]\nSubnet=10.0.0.0/8\n").is_err());
        assert!(
            config("[NAT]\nTapPrefix=x\n")
                .unwrap_err()
                .contains("unknown setting")
        );
        assert_eq!(
            config(include_str!("../examples/network.conf")).unwrap(),
            NetConfig::default()
        );
    }

    #[test]
    fn slots() {
        let c = NetConfig::default();
        let t = c.slot(1);
        assert_eq!(t.name, "garnish1");
        assert_eq!(
            (t.host, t.guest),
            (
                Ipv4Addr::new(192, 168, 10, 5),
                Ipv4Addr::new(192, 168, 10, 6)
            )
        );
        assert_eq!(t.netmask(), Ipv4Addr::new(255, 255, 255, 252));
        // The longest name fits in IFNAMSIZ with its NUL.
        let c = config("[NAT]\nSubnet=10.0.0.0/16\n").unwrap();
        assert!(c.slot(c.slots() - 1).name.len() < 16);
    }

    #[test]
    fn passing() {
        let (ours, theirs) = seqpacket_pair().unwrap();
        let tap = NetConfig::default().slot(2);
        let file = std::fs::File::open("/dev/null").unwrap();
        send_reply(theirs.as_raw_fd(), &tap, file.as_raw_fd()).unwrap();
        let got = recv_reply(&ours).unwrap();
        assert_eq!(got.info, tap);
        // Another descriptor for the same file, close-on-exec.
        assert_ne!(got.fd.as_raw_fd(), file.as_raw_fd());
        assert_eq!(
            std::fs::read_link(format!("/proc/self/fd/{}", got.fd.as_raw_fd())).unwrap(),
            Path::new("/dev/null")
        );
        assert_eq!(
            unsafe { libc::fcntl(got.fd.as_raw_fd(), libc::F_GETFD) },
            libc::FD_CLOEXEC
        );
        // Nothing left: the helper failed before sending.
        assert!(recv_reply(&ours).unwrap_err().contains("reading the reply"));
    }

    /// gerantd's side, with a stand-in for the helper that only checks
    /// what it was given.
    #[tokio::test]
    async fn running() {
        let dir = std::env::temp_dir().join(format!("gerant-net-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-helper");
        std::fs::write(
            &script,
            "#!/bin/sh\n[ \"$1\" = nat ] && [ -S /dev/stdout ] || { echo 'gerant-net: wrong stdout' >&2; exit 1; }\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // It exits successfully without a reply: stdout was the socket.
        assert_eq!(
            run_helper(&script).await.unwrap_err(),
            "gerant-net: no tap in the reply"
        );
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'gerant-net: all 64 networks are in use' >&2\nexit 1\n",
        )
        .unwrap();
        assert_eq!(
            run_helper(&script).await.unwrap_err(),
            "all 64 networks are in use"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reply() {
        let t = NetConfig::default().slot(3);
        assert_eq!(NatTap::decode(&t.encode()), Ok(t.clone()));
        assert_eq!(NatTap::decode(&format!("{}future=1\n", t.encode())), Ok(t));
        assert!(
            NatTap::decode("tap=x\n")
                .unwrap_err()
                .contains("incomplete")
        );
        assert!(
            NatTap::decode("host=nope\n")
                .unwrap_err()
                .contains("bad line")
        );
    }
}
