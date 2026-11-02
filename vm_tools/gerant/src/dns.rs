//! DNS for NAT guests: gerant-net binds UDP and TCP sockets to port 53 of
//! the host's addresses on the tap, and this passes what the guest sends
//! there to the host's resolver, the first nameserver in /etc/resolv.conf.
//! That can be a loopback one, like systemd-resolved's 127.0.0.53, which
//! the guest couldn't reach itself. passt does the same for its guests
//! (`--dns-forward`).

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::net::DnsSocket;

const RESOLV_CONF: &str = "/etc/resolv.conf";
/// How long the host's resolver gets to answer a query over UDP, or to
/// accept a TCP connection.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a TCP connection to the resolver may last.
const TCP_TIMEOUT: Duration = Duration::from_secs(30);
/// Queries in flight at once per VM, over UDP and over TCP. More are
/// dropped, as a busy server would: the guest's resolver retries.
const MAX_UDP: usize = 64;
const MAX_TCP: usize = 16;

/// A VM's forwarders. They stop when this is dropped, with the VM.
pub struct Forwarder {
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Forwarder {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Who may ask, and where their queries go: the first nameserver in a
/// resolv.conf.
struct Route {
    /// The guest's addresses. Other guests reach this VM's sockets too
    /// (the firewall only checks that queries come from a tap), but get no
    /// answer: their own gerantd, maybe another user's, serves them.
    guests: Vec<IpAddr>,
    resolv_conf: PathBuf,
    port: u16,
}

impl Route {
    fn addr(&self) -> SocketAddr {
        SocketAddr::new(nameserver(&self.resolv_conf), self.port)
    }
}

impl Forwarder {
    /// Serves the sockets from gerant-net to the guest at `guests`. Has to
    /// run inside the runtime.
    pub fn start(
        name: &str,
        sockets: Vec<(DnsSocket, OwnedFd)>,
        guests: Vec<IpAddr>,
    ) -> io::Result<Forwarder> {
        let route = Route {
            guests,
            resolv_conf: RESOLV_CONF.into(),
            port: 53,
        };
        Forwarder::start_with(name, sockets, route)
    }

    fn start_with(
        name: &str,
        sockets: Vec<(DnsSocket, OwnedFd)>,
        route: Route,
    ) -> io::Result<Forwarder> {
        let route = Arc::new(route);
        let udp_limit = Arc::new(Semaphore::new(MAX_UDP));
        let tcp_limit = Arc::new(Semaphore::new(MAX_TCP));
        let mut tasks = Vec::new();
        for (kind, fd) in sockets {
            let task = match kind {
                DnsSocket::Udp => {
                    let sock = std::net::UdpSocket::from(fd);
                    sock.set_nonblocking(true)?;
                    let sock = UdpSocket::from_std(sock)?;
                    let limit = udp_limit.clone();
                    tokio::spawn(serve_udp(name.to_owned(), sock, route.clone(), limit))
                }
                DnsSocket::Tcp => {
                    let listener = std::net::TcpListener::from(fd);
                    listener.set_nonblocking(true)?;
                    let listener = TcpListener::from_std(listener)?;
                    let limit = tcp_limit.clone();
                    tokio::spawn(serve_tcp(name.to_owned(), listener, route.clone(), limit))
                }
            };
            tasks.push(task);
        }
        Ok(Forwarder { tasks })
    }
}

/// The host's resolver: the first usable nameserver in `resolv_conf`, read
/// for every query so the forwarder follows network changes. Without one,
/// 127.0.0.1, as for the host's own programs (resolv.conf(5)).
fn nameserver(resolv_conf: &Path) -> IpAddr {
    let text = std::fs::read_to_string(resolv_conf).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some("nameserver")).then(|| words.next())?
        })
        // Link-local servers need a scope (fe80::1%eth0), which this skips.
        .find_map(|word| word.parse::<IpAddr>().ok())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

async fn serve_udp(name: String, sock: UdpSocket, route: Arc<Route>, limit: Arc<Semaphore>) {
    let sock = Arc::new(sock);
    let mut buf = vec![0u8; 65535];
    loop {
        let (n, guest) = match sock.recv_from(&mut buf).await {
            Ok(got) => got,
            Err(e) => {
                eprintln!("gerantd: {name}: DNS over UDP: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if !route.guests.contains(&guest.ip()) {
            continue;
        }
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            continue;
        };
        let query = buf[..n].to_vec();
        let sock = sock.clone();
        let server = route.addr();
        tokio::spawn(async move {
            let _permit = permit;
            if let Ok(reply) = udp_query(&query, server).await {
                let _ = sock.send_to(&reply, guest).await;
            }
        });
    }
}

/// One query and its answer, from a socket of its own.
async fn udp_query(query: &[u8], server: SocketAddr) -> io::Result<Vec<u8>> {
    let local: SocketAddr = match server {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let sock = UdpSocket::bind(local).await?;
    sock.connect(server).await?;
    sock.send(query).await?;
    let mut buf = vec![0u8; 65535];
    let n = timeout(QUERY_TIMEOUT, sock.recv(&mut buf))
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
    buf.truncate(n);
    Ok(buf)
}

async fn serve_tcp(name: String, listener: TcpListener, route: Arc<Route>, limit: Arc<Semaphore>) {
    loop {
        let (mut conn, peer) = match listener.accept().await {
            Ok(got) => got,
            Err(e) => {
                eprintln!("gerantd: {name}: DNS over TCP: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if !route.guests.contains(&peer.ip()) {
            continue;
        }
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            continue;
        };
        let server = route.addr();
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(Ok(mut up)) = timeout(QUERY_TIMEOUT, TcpStream::connect(server)).await else {
                return;
            };
            let _ = timeout(
                TCP_TIMEOUT,
                tokio::io::copy_bidirectional(&mut conn, &mut up),
            )
            .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gerant-dns-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nameservers() {
        let dir = temp_dir("nameservers");
        let file = dir.join("resolv.conf");
        let with = |text: &str| {
            std::fs::write(&file, text).unwrap();
            nameserver(&file)
        };
        assert_eq!(
            with("# systemd-resolved\nnameserver 127.0.0.53\noptions edns0\n"),
            "127.0.0.53".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            with("search lan\nnameserver fe80::1%eth0\nnameserver 2001:db8::1\n"),
            "2001:db8::1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(with("search lan\n"), IpAddr::V4(Ipv4Addr::LOCALHOST));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(nameserver(&file), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    /// Queries over UDP and TCP, through the forwarder to a stand-in for
    /// the host's resolver that answers with the query reversed.
    #[tokio::test]
    async fn forwarding() {
        let resolver_udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = resolver_udp.local_addr().unwrap().port();
        let resolver_tcp = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            loop {
                let (n, from) = resolver_udp.recv_from(&mut buf).await.unwrap();
                buf[..n].reverse();
                resolver_udp.send_to(&buf[..n], from).await.unwrap();
            }
        });
        tokio::spawn(async move {
            loop {
                let (mut conn, _) = resolver_tcp.accept().await.unwrap();
                let mut buf = [0u8; 512];
                let n = conn.read(&mut buf).await.unwrap();
                buf[..n].reverse();
                conn.write_all(&buf[..n]).await.unwrap();
            }
        });
        let dir = temp_dir("forwarding");
        let resolv_conf = dir.join("resolv.conf");
        std::fs::write(&resolv_conf, "nameserver 127.0.0.1\n").unwrap();

        // Stand-ins for the sockets from gerant-net.
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let udp_addr = udp.local_addr().unwrap();
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp_addr = tcp.local_addr().unwrap();
        let forwarder = Forwarder::start_with(
            "test",
            vec![
                (DnsSocket::Udp, OwnedFd::from(udp)),
                (DnsSocket::Tcp, OwnedFd::from(tcp)),
            ],
            Route {
                guests: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                resolv_conf,
                port,
            },
        )
        .unwrap();

        let guest = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        guest.send_to(b"query", udp_addr).await.unwrap();
        let mut buf = [0u8; 512];
        let n = timeout(QUERY_TIMEOUT, guest.recv(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"yreuq");

        let mut conn = TcpStream::connect(tcp_addr).await.unwrap();
        conn.write_all(b"tcp query").await.unwrap();
        let n = timeout(QUERY_TIMEOUT, conn.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"yreuq pct");

        // Another guest gets no answer.
        let stranger = UdpSocket::bind("127.0.0.2:0").await.unwrap();
        stranger.send_to(b"query", udp_addr).await.unwrap();
        assert!(
            timeout(Duration::from_millis(300), stranger.recv(&mut buf))
                .await
                .is_err()
        );
        let mut conn = TcpStream::connect(tcp_addr).await.unwrap();
        conn.write_all(b"tcp query").await.unwrap();
        let n = timeout(QUERY_TIMEOUT, conn.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"yreuq pct");

        // Stopped with the VM: nothing answers any more.
        drop(forwarder);
        tokio::task::yield_now().await;
        guest.send_to(b"query", udp_addr).await.unwrap();
        assert!(
            timeout(Duration::from_millis(300), guest.recv(&mut buf))
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
