//! The guest's login console, on its second serial port (ttyS1). crosvm's
//! stdin and stdout are pipes to gerantd, which relays both directions to
//! whoever connects to the console socket (`gerant console`). Nothing of it
//! is kept: the boot messages are on the first port, which crosvm writes to
//! console.log (see `vm::crosvm_args`).

use std::io::{self, PipeReader, PipeWriter};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// How many chunks of output a slow client may fall behind before it
/// misses some.
const BACKLOG: usize = 256;

pub struct Console {
    /// crosvm's ends, kept for the next crosvm after a guest reboot. While
    /// we hold them the pipes stay open, so a reboot doesn't end the relay.
    stdin: PipeReader,
    stdout: PipeWriter,
    listener: JoinHandle<()>,
}

impl Console {
    /// Listens on `socket`.
    pub fn start(socket: &Path) -> io::Result<Console> {
        let listener = UnixListener::bind(socket)
            .map_err(|e| io::Error::new(e.kind(), format!("binding {}: {e}", socket.display())))?;
        let (stdin, input) = io::pipe()?;
        let (output, stdout) = io::pipe()?;
        // Our ends don't block; crosvm's ends are other open files, so
        // they still do.
        let input = pipe::Sender::from_owned_fd(input.into())?;
        let output = pipe::Receiver::from_owned_fd(output.into())?;

        let (out_tx, _) = broadcast::channel(BACKLOG);
        let (in_tx, in_rx) = mpsc::channel(16);
        tokio::spawn(read_output(output, out_tx.clone()));
        tokio::spawn(write_input(input, in_rx));
        let listener = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(client(stream, out_tx.subscribe(), in_tx.clone()));
                    }
                    Err(e) => {
                        eprintln!("gerantd: console: {e}");
                        return;
                    }
                }
            }
        });
        Ok(Console {
            stdin,
            stdout,
            listener,
        })
    }

    /// stdin and stdout for a crosvm.
    pub fn stdio(&self) -> io::Result<(Stdio, Stdio)> {
        Ok((
            self.stdin.try_clone()?.into(),
            self.stdout.try_clone()?.into(),
        ))
    }
}

impl Drop for Console {
    /// Once crosvm has exited too, the output pipe has no writer left:
    /// read_output drains it and ends, and so do the clients.
    fn drop(&mut self) {
        self.listener.abort();
    }
}

/// Hands crosvm's output to the clients; with none attached, it's dropped.
async fn read_output(mut output: pipe::Receiver, clients: broadcast::Sender<Arc<[u8]>>) {
    let mut buf = vec![0; 4096];
    loop {
        let n = match output.read(&mut buf).await {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) => {
                eprintln!("gerantd: console: {e}");
                return;
            }
        };
        // Fails only when nobody is attached.
        let _ = clients.send(Arc::from(&buf[..n]));
    }
}

/// Passes what the clients type to crosvm, until they're all gone.
async fn write_input(mut input: pipe::Sender, mut typed: mpsc::Receiver<Vec<u8>>) {
    while let Some(bytes) = typed.recv().await {
        // crosvm has exited for good.
        if input.write_all(&bytes).await.is_err() {
            return;
        }
    }
}

async fn client(
    stream: UnixStream,
    mut output: broadcast::Receiver<Arc<[u8]>>,
    input: mpsc::Sender<Vec<u8>>,
) {
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = vec![0; 1024];
    loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Ok(chunk) => {
                    if writer.write_all(&chunk).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                // The VM is gone.
                Err(broadcast::error::RecvError::Closed) => return,
            },
            n = reader.read(&mut buf) => match n {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if input.send(buf[..n].to_vec()).await.is_err() {
                        return;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    async fn read_until(stream: &mut UnixStream, want: &[u8]) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0; 256];
        while !got.windows(want.len()).any(|w| w == want) {
            let n = timeout(Duration::from_secs(5), stream.read(&mut buf))
                .await
                .expect("timed out")
                .unwrap();
            assert_ne!(n, 0, "EOF after {got:?}");
            got.extend_from_slice(&buf[..n]);
        }
        got
    }

    // `cat` stands in for crosvm: what's typed comes back as output.
    fn crosvm(console: &Console) -> tokio::process::Child {
        let (stdin, stdout) = console.stdio().unwrap();
        Command::new("cat")
            .stdin(stdin)
            .stdout(stdout)
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn relay() {
        let dir = std::env::temp_dir().join(format!("gerant-console-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("console.sock");
        let console = Console::start(&socket).unwrap();
        let mut first = crosvm(&console);
        let mut a = UnixStream::connect(&socket).await.unwrap();
        let mut b = UnixStream::connect(&socket).await.unwrap();
        // Let the listener take both before there's output.
        tokio::time::sleep(Duration::from_millis(50)).await;
        a.write_all(b"login\n").await.unwrap();
        read_until(&mut a, b"login\n").await;
        read_until(&mut b, b"login\n").await;

        // A guest reboot: the next crosvm gets the same pipes.
        first.kill().await.unwrap();
        let mut second = crosvm(&console);
        b.write_all(b"again\n").await.unwrap();
        read_until(&mut a, b"again\n").await;

        // The VM stops: the clients see the end.
        drop(console);
        second.kill().await.unwrap();
        let mut rest = Vec::new();
        timeout(Duration::from_secs(5), a.read_to_end(&mut rest))
            .await
            .expect("no EOF")
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
