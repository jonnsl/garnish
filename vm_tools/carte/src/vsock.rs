//! AF_VSOCK stream sockets. The connections are plain `File`s: reads and
//! writes on a socket fd are all we need.

use std::fs::File;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

pub const HOST_CID: u32 = libc::VMADDR_CID_HOST;

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn socket() -> io::Result<OwnedFd> {
    let fd =
        check(unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) })?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn address(cid: u32, port: u32) -> libc::sockaddr_vm {
    let mut addr: libc::sockaddr_vm = unsafe { mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_cid = cid;
    addr.svm_port = port;
    addr
}

const ADDRESS_LEN: libc::socklen_t = mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;

pub fn connect(cid: u32, port: u32) -> io::Result<File> {
    let fd = socket()?;
    let addr = address(cid, port);
    check(unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            ADDRESS_LEN,
        )
    })?;
    Ok(File::from(fd))
}

/// Makes blocking reads and writes on `conn` fail after `timeout`.
pub fn set_timeout(conn: &File, timeout: Duration) -> io::Result<()> {
    let tv = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    for opt in [libc::SO_RCVTIMEO, libc::SO_SNDTIMEO] {
        check(unsafe {
            libc::setsockopt(
                conn.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &tv as *const _ as *const libc::c_void,
                mem::size_of_val(&tv) as libc::socklen_t,
            )
        })?;
    }
    Ok(())
}

pub struct Listener(OwnedFd);

impl Listener {
    pub fn bind(port: u32) -> io::Result<Self> {
        let fd = socket()?;
        let addr = address(libc::VMADDR_CID_ANY, port);
        check(unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                ADDRESS_LEN,
            )
        })?;
        check(unsafe { libc::listen(fd.as_raw_fd(), 16) })?;
        Ok(Listener(fd))
    }

    /// Returns the connection and the peer's CID.
    pub fn accept(&self) -> io::Result<(File, u32)> {
        let mut addr = address(0, 0);
        let mut len = ADDRESS_LEN;
        let fd = loop {
            let ret = unsafe {
                libc::accept4(
                    self.0.as_raw_fd(),
                    &mut addr as *mut _ as *mut libc::sockaddr,
                    &mut len,
                    libc::SOCK_CLOEXEC,
                )
            };
            match check(ret) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        Ok((
            File::from(unsafe { OwnedFd::from_raw_fd(fd) }),
            addr.svm_cid,
        ))
    }
}
