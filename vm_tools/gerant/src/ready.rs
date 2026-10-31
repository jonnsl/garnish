//! Guests tell gerantd when they have booted. gerant-ready.service in the
//! image runs `systemd-notify --ready` with
//! `NOTIFY_SOCKET=vsock-stream:2:<READY_PORT>`: it connects to the host on
//! that vsock port, sends `READY=1` and closes the connection. The kernel
//! tells us the guest's CID, and so which VM it is.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::time::{sleep, timeout};

use crate::manager::Manager;

/// The host's vsock port for the notifications. Also in
/// guest/garnish-guest/files/gerant-ready.service.
pub const READY_PORT: u32 = 8891;
/// How long a guest gets to send its message.
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// sd_notify messages are short; anything longer isn't one.
const MAX_MESSAGE: usize = 4096;

fn log(msg: impl std::fmt::Display) {
    eprintln!("gerantd: {msg}");
}

/// Hands a socket we own to the tokio reactor.
fn register(fd: OwnedFd) -> io::Result<AsyncFd<OwnedFd>> {
    // Safe: the OwnedFd owns the descriptor and always returns it.
    Ok(unsafe { AsyncFd::register(fd) }?)
}

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// Whether an sd_notify message says the guest is ready.
pub fn is_ready(message: &[u8]) -> bool {
    message
        .split(|&b| b == b'\n')
        .any(|line| line == b"READY=1")
}

/// A non-blocking AF_VSOCK stream socket listening on every CID.
pub struct Listener(AsyncFd<OwnedFd>);

impl Listener {
    /// Must be called inside the tokio runtime.
    pub fn bind(port: u32) -> io::Result<Listener> {
        let fd = check(unsafe {
            libc::socket(
                libc::AF_VSOCK,
                libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        })?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut addr: libc::sockaddr_vm = unsafe { mem::zeroed() };
        addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        addr.svm_cid = libc::VMADDR_CID_ANY;
        addr.svm_port = port;
        let len = mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
        check(unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                len,
            )
        })?;
        check(unsafe { libc::listen(fd.as_raw_fd(), 16) })?;
        Ok(Listener(register(fd)?))
    }

    /// The connection, non-blocking, and the peer's CID and port.
    async fn accept(&self) -> io::Result<(AsyncFd<OwnedFd>, u32, u32)> {
        loop {
            let mut guard = self.0.readable().await?;
            let result = guard.try_io(|fd| {
                let mut addr: libc::sockaddr_vm = unsafe { mem::zeroed() };
                let mut len = mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;
                let conn = check(unsafe {
                    libc::accept4(
                        fd.as_raw_fd(),
                        &mut addr as *mut _ as *mut libc::sockaddr,
                        &mut len,
                        libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    )
                })?;
                Ok((
                    unsafe { OwnedFd::from_raw_fd(conn) },
                    addr.svm_cid,
                    addr.svm_port,
                ))
            });
            match result {
                Ok(Ok((conn, cid, port))) => return Ok((register(conn)?, cid, port)),
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(e)) => return Err(e),
                // Not ready after all; wait again.
                Err(_) => continue,
            }
        }
    }
}

/// Reads until the guest closes its end, or MAX_MESSAGE bytes.
async fn read_message(conn: &AsyncFd<OwnedFd>) -> io::Result<Vec<u8>> {
    let mut message = Vec::new();
    let mut buf = [0u8; 512];
    while message.len() < MAX_MESSAGE {
        let mut guard = conn.readable().await?;
        let n = match guard.try_io(|fd| {
            let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }) {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => n,
            Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(e)) => return Err(e),
            Err(_) => continue,
        };
        message.extend_from_slice(&buf[..n]);
    }
    Ok(message)
}

/// Accepts notifications forever and passes them on to the manager.
pub async fn serve(listener: Listener, manager: Arc<Manager>) {
    loop {
        let (conn, cid, port) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                // E.g. out of file descriptors: don't spin.
                log(format!("accepting a boot notification: {e}"));
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        // sd_notify sends from port 1023 when it runs as root; only root
        // in the guest can bind ports below 1024.
        if port >= 1024 {
            continue;
        }
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            match timeout(READ_TIMEOUT, read_message(&conn)).await {
                Ok(Ok(message)) if is_ready(&message) => manager.guest_ready(cid).await,
                Ok(Ok(_)) => {}
                Ok(Err(e)) => log(format!("reading a boot notification from CID {cid}: {e}")),
                Err(_) => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready() {
        assert!(is_ready(b"READY=1"));
        assert!(is_ready(b"READY=1\n"));
        assert!(is_ready(b"STATUS=up\nREADY=1\nMAINPID=1"));
        assert!(!is_ready(b""));
        assert!(!is_ready(b"READY=0"));
        assert!(!is_ready(b"X_READY=1"));
        assert!(!is_ready(b"READY=10"));
    }
}
