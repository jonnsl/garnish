//! Non-blocking AF_VSOCK stream sockets on the tokio reactor.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

pub const HOST_CID: u32 = libc::VMADDR_CID_HOST;

fn check(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn socket(flags: libc::c_int) -> io::Result<OwnedFd> {
    let fd = check(unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC | flags, 0) })?;
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

/// Hands a socket to the tokio reactor. Must be called inside the runtime.
fn register(fd: OwnedFd) -> io::Result<AsyncFd<OwnedFd>> {
    // Safe: the OwnedFd owns the descriptor and always returns it.
    Ok(unsafe { AsyncFd::register(fd) }?)
}

/// A connected socket.
pub struct Stream(AsyncFd<OwnedFd>);

/// Connects to `port` on `cid`. The connect itself blocks (vsock gives up
/// after two seconds), so it runs on tokio's blocking threads.
pub async fn connect(cid: u32, port: u32) -> io::Result<Stream> {
    let fd = tokio::task::spawn_blocking(move || {
        let fd = socket(0)?;
        let addr = address(cid, port);
        check(unsafe { libc::connect(fd.as_raw_fd(), &addr as *const _ as *const libc::sockaddr, ADDRESS_LEN) })?;
        let flags = check(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) })?;
        check(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) })?;
        Ok::<_, io::Error>(fd)
    })
    .await??;
    Ok(Stream(register(fd)?))
}

pub struct Listener(AsyncFd<OwnedFd>);

impl Listener {
    /// Listens on `port` for every CID. Must be called inside the runtime.
    pub fn bind(port: u32) -> io::Result<Listener> {
        let fd = socket(libc::SOCK_NONBLOCK)?;
        let addr = address(libc::VMADDR_CID_ANY, port);
        check(unsafe { libc::bind(fd.as_raw_fd(), &addr as *const _ as *const libc::sockaddr, ADDRESS_LEN) })?;
        check(unsafe { libc::listen(fd.as_raw_fd(), 16) })?;
        Ok(Listener(register(fd)?))
    }

    /// Returns the connection and the peer's CID.
    pub async fn accept(&self) -> io::Result<(Stream, u32)> {
        loop {
            let mut guard = self.0.readable().await?;
            let result = guard.try_io(|fd| {
                let mut addr = address(0, 0);
                let mut len = ADDRESS_LEN;
                let conn = check(unsafe {
                    libc::accept4(
                        fd.as_raw_fd(),
                        &mut addr as *mut _ as *mut libc::sockaddr,
                        &mut len,
                        libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    )
                })?;
                Ok((unsafe { OwnedFd::from_raw_fd(conn) }, addr.svm_cid))
            });
            match result {
                Ok(Ok((conn, cid))) => return Ok((Stream(register(conn)?), cid)),
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(e)) => return Err(e),
                // Not ready after all; wait again.
                Err(_) => continue,
            }
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            let result = guard.try_io(|fd| {
                let n = unsafe { libc::read(fd.as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len()) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match result {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(e)) => return Poll::Ready(Err(e)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(cx))?;
            let result = guard.try_io(|fd| {
                let n = unsafe { libc::send(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len(), libc::MSG_NOSIGNAL) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            });
            match result {
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(check(unsafe { libc::shutdown(self.0.as_raw_fd(), libc::SHUT_WR) }).map(drop))
    }
}
