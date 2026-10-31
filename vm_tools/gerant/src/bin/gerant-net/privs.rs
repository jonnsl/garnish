//! Giving up root: everything but CAP_NET_ADMIN, and the root user itself.

use std::io;
use std::mem;
use std::os::fd::RawFd;

/// From linux/capability.h; not in the libc crate.
const CAP_NET_ADMIN: u32 = 12;
const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn check(what: &str, ret: libc::c_long) -> Result<(), String> {
    if ret < 0 { Err(format!("{what}: {}", io::Error::last_os_error())) } else { Ok(()) }
}

fn prctl(what: &str, option: libc::c_int, arg: libc::c_ulong) -> Result<(), String> {
    check(what, unsafe { libc::prctl(option, arg, 0 as libc::c_ulong, 0 as libc::c_ulong, 0 as libc::c_ulong) }.into())
}

/// Becomes the user who ran us, keeping only CAP_NET_ADMIN, which is all
/// that creating and configuring a tap and changing the firewall need. The
/// capability can't be passed on (bounding set, no ambient or inheritable
/// capabilities, no_new_privs), and we can't be traced or dumped.
pub fn drop_to_net_admin() -> Result<(), String> {
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    if unsafe { libc::geteuid() } != 0 {
        return Err("not running as root: gerant-net has to be installed setuid root \
                    (owned by root, mode 4755)"
            .into());
    }

    // Keep the permitted capabilities across the change of user.
    prctl("PR_SET_KEEPCAPS", libc::PR_SET_KEEPCAPS, 1)?;
    for cap in 0..64 {
        if cap == CAP_NET_ADMIN {
            continue;
        }
        if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap as libc::c_ulong, 0, 0, 0) } < 0 {
            // Past the last capability this kernel knows.
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(format!("dropping capability {cap}: {}", io::Error::last_os_error()));
        }
    }
    check("PR_CAP_AMBIENT_CLEAR_ALL", unsafe {
        libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL as libc::c_ulong, 0, 0, 0)
    }
    .into())?;

    // Group first: changing it needs the root we're about to give up.
    check("setresgid", unsafe { libc::setresgid(gid, gid, gid) }.into())?;
    check("setresuid", unsafe { libc::setresuid(uid, uid, uid) }.into())?;

    let bit = 1 << CAP_NET_ADMIN;
    let mut header = CapHeader { version: LINUX_CAPABILITY_VERSION_3, pid: 0 };
    let data = [CapData { effective: bit, permitted: bit, inheritable: 0 }, CapData::default()];
    check("capset", unsafe { libc::syscall(libc::SYS_capset, &mut header, data.as_ptr()) })?;

    prctl("PR_SET_KEEPCAPS", libc::PR_SET_KEEPCAPS, 0)?;
    prctl("PR_SET_DUMPABLE", libc::PR_SET_DUMPABLE, 0)?;
    // Needed for seccomp without CAP_SYS_ADMIN; also, nothing we could exec
    // would gain privileges.
    prctl("PR_SET_NO_NEW_PRIVS", libc::PR_SET_NO_NEW_PRIVS, 1)?;

    // Make sure it worked.
    if uid != 0 && unsafe { libc::setuid(0) } == 0 {
        return Err("could still become root after dropping privileges".into());
    }
    let mut got = [CapData::default(); 2];
    check("capget", unsafe { libc::syscall(libc::SYS_capget, &mut header, got.as_mut_ptr()) })?;
    if got[0].effective != bit || got[0].permitted != bit || got[0].inheritable != 0 || got[1].permitted != 0 {
        return Err("capabilities other than CAP_NET_ADMIN are left".into());
    }
    Ok(())
}

/// Checks that `fd` is a Unix socket, where the reply goes.
pub fn check_unix_socket(fd: RawFd) -> Result<(), String> {
    let mut st: libc::stat = unsafe { mem::zeroed() };
    let mut domain: libc::c_int = 0;
    let mut len = mem::size_of::<libc::c_int>() as libc::socklen_t;
    let is_unix = unsafe { libc::fstat(fd, &mut st) } == 0
        && st.st_mode & libc::S_IFMT == libc::S_IFSOCK
        && unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_DOMAIN, (&raw mut domain).cast(), &mut len) } == 0
        && domain == libc::AF_UNIX;
    if is_unix {
        Ok(())
    } else {
        Err("standard output has to be a Unix socket, for the reply (gerantd runs gerant-net)".into())
    }
}
