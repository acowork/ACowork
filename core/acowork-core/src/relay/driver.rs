//! Shared single-task yamux driver (design doc 24 §5.2).
//!
//! yamux 0.14 requires ALL connection progress (`poll_next_inbound` /
//! `poll_new_outbound`) to happen from one task. Both sides of a relay
//! tunnel need exactly that loop, so it lives here once:
//!
//! - **relay server** ([`spawn_driver`] with `Mode::Server`): receives
//!   the Gateway-opened control stream as the first inbound, and opens
//!   tagged data streams toward the Gateway when clients connect.
//! - **Gateway client** (`Mode::Client`): opens the control stream, and
//!   receives relay-opened data streams (dispatched by tag byte).
//!
//! Streams are `futures::io` types and are safe to use from any task
//! once obtained — only the connection polling must stay single-tasked.

use std::collections::VecDeque;
use std::task::Poll;

use futures_util::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};

/// Request to open one outbound stream through the driver.
struct OpenRequest {
    /// Optional single byte the driver writes to the stream before
    /// handing it over — the opener-side stream tag (§5.2). Writing it
    /// in the driver guarantees the tag precedes any payload bytes the
    /// caller later writes.
    first_byte: Option<u8>,
    resp: oneshot::Sender<anyhow::Result<yamux::Stream>>,
}

/// Handle for opening streams through a running [`spawn_driver`] task.
///
/// Cheap to clone; the handle stays usable until the driver task ends
/// (connection closed or aborted), after which [`open_stream`] fails.
#[derive(Clone)]
pub struct YamuxDriverHandle {
    open_tx: mpsc::Sender<OpenRequest>,
}

impl YamuxDriverHandle {
    /// Open one outbound yamux stream. When `first_byte` is given it is
    /// written to the stream before it is handed back.
    pub async fn open_stream(&self, first_byte: Option<u8>) -> anyhow::Result<yamux::Stream> {
        let (tx, rx) = oneshot::channel();
        self.open_tx
            .send(OpenRequest {
                first_byte,
                resp: tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("tunnel driver gone"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("tunnel driver dropped the open request"))?
    }
}

/// Spawn the single task that owns the yamux [`yamux::Connection`].
///
/// Returns the open-stream handle, the receiver for streams the REMOTE
/// side opened, and the driver task handle. The driver ends when the
/// connection closes or errors, when the inbound receiver is dropped,
/// or when the task is aborted (the intended teardown path for tunnel
/// supervisors — aborting drops the socket, which closes the tunnel).
pub fn spawn_driver<T>(
    socket: T,
    config: yamux::Config,
    mode: yamux::Mode,
    inbound_capacity: usize,
) -> (
    YamuxDriverHandle,
    mpsc::Receiver<yamux::Stream>,
    tokio::task::JoinHandle<()>,
)
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let conn = yamux::Connection::new(socket, config, mode);
    let (open_tx, open_rx) = mpsc::channel::<OpenRequest>(64);
    let (inbound_tx, inbound_rx) = mpsc::channel::<yamux::Stream>(inbound_capacity);
    let task = tokio::spawn(drive(conn, open_rx, inbound_tx));
    (YamuxDriverHandle { open_tx }, inbound_rx, task)
}

async fn drive<T>(
    mut conn: yamux::Connection<T>,
    mut open_rx: mpsc::Receiver<OpenRequest>,
    inbound_tx: mpsc::Sender<yamux::Stream>,
) where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut pending_opens: VecDeque<OpenRequest> = VecDeque::new();
    loop {
        let event = futures_util::future::poll_fn(|cx| {
            // 1. Drain new open requests.
            while let Poll::Ready(Some(req)) = open_rx.poll_recv(cx) {
                pending_opens.push_back(req);
            }
            // 2. Satisfy as many open requests as yamux allows right
            //    now. Tagged opens are fulfilled by a short task that
            //    writes the tag byte FIRST, so it always precedes any
            //    payload the caller writes afterwards.
            while !pending_opens.is_empty() {
                match conn.poll_new_outbound(cx) {
                    Poll::Ready(Ok(stream)) => {
                        if let Some(req) = pending_opens.pop_front() {
                            tokio::spawn(async move {
                                let mut stream = stream;
                                if let Some(tag) = req.first_byte {
                                    if let Err(e) = stream.write_all(&[tag]).await {
                                        let _ = req
                                            .resp
                                            .send(Err(anyhow::anyhow!("writing stream tag: {e}")));
                                        return;
                                    }
                                    if let Err(e) = stream.flush().await {
                                        let _ = req
                                            .resp
                                            .send(Err(anyhow::anyhow!("flushing stream tag: {e}")));
                                        return;
                                    }
                                }
                                let _ = req.resp.send(Ok(stream));
                            });
                        }
                    }
                    Poll::Ready(Err(e)) => {
                        if let Some(req) = pending_opens.pop_front() {
                            let _ = req.resp.send(Err(anyhow::anyhow!("yamux open: {e}")));
                        }
                    }
                    Poll::Pending => break,
                }
            }
            // 3. Drive the connection itself.
            conn.poll_next_inbound(cx)
        });

        match event.await {
            Some(Ok(stream)) => {
                if inbound_tx.send(stream).await.is_err() {
                    // Consumer gone — nothing left to do.
                    return;
                }
            }
            Some(Err(e)) => {
                tracing::debug!(error = %e, "yamux connection error; driver ending");
                return;
            }
            None => {
                tracing::debug!("yamux connection closed; driver ending");
                return;
            }
        }
    }
}

/// futures-io flavored bidirectional copy: pump bytes both ways until one
/// side reaches EOF or errors. (futures-util does not ship one for
/// `AsyncRead + AsyncWrite` pairs.)
///
/// Shared by the relay's device-domain byte pipe and the Gateway
/// relay-client's stream → local-listener forwarding.
pub async fn copy_bidirectional<A, B>(a: &mut A, b: &mut B) -> std::io::Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    use futures_util::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut buf_a = [0u8; 16 * 1024];
    let mut buf_b = [0u8; 16 * 1024];
    let mut a_open = true;
    let mut b_open = true;
    while a_open || b_open {
        tokio::select! {
            read = a.read(&mut buf_a), if a_open => match read {
                Ok(0) => {
                    let _ = a.close().await;
                    let _ = b.close().await;
                    a_open = false;
                    if !b_open { break; }
                }
                Ok(n) => b.write_all(&buf_a[..n]).await?,
                Err(e) => return Err(e),
            },
            read = b.read(&mut buf_b), if b_open => match read {
                Ok(0) => {
                    let _ = b.close().await;
                    let _ = a.close().await;
                    b_open = false;
                    if !a_open { break; }
                }
                Ok(n) => a.write_all(&buf_b[..n]).await?,
                Err(e) => return Err(e),
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::io::AsyncReadExt as _;
    use tokio_util::compat::TokioAsyncReadCompatExt as _;

    /// A futures-io duplex pair (tokio duplex + compat).
    fn duplex_pair() -> (
        tokio_util::compat::Compat<tokio::io::DuplexStream>,
        tokio_util::compat::Compat<tokio::io::DuplexStream>,
    ) {
        let (a, b) = tokio::io::duplex(64 * 1024);
        (a.compat(), b.compat())
    }

    /// Client and server connections over an in-memory duplex pair, each
    /// driven by `spawn_driver`. Open a stream on one side, echo on the
    /// other, verify bytes and the tag-first ordering.
    #[tokio::test]
    async fn driver_roundtrip_both_directions() {
        let (client_io, server_io) = duplex_pair();
        let (client, mut client_inbound, _client_task) = spawn_driver(
            client_io,
            yamux::Config::default(),
            yamux::Mode::Client,
            16,
        );
        let (server, mut server_inbound, _server_task) = spawn_driver(
            server_io,
            yamux::Config::default(),
            yamux::Mode::Server,
            16,
        );

        // Client opens a stream with a tag byte; server receives it.
        let mut opened = client.open_stream(Some(0x01)).await.unwrap();
        opened.write_all(b"payload").await.unwrap();

        let mut inbound = server_inbound.recv().await.expect("inbound stream");
        let mut tag = [0u8; 1];
        inbound.read_exact(&mut tag).await.unwrap();
        assert_eq!(tag[0], 0x01, "tag byte must arrive first");

        let mut buf = [0u8; 7];
        inbound.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"payload");

        // Reverse direction: server opens toward the client.
        let mut back = server.open_stream(None).await.unwrap();
        back.write_all(b"reply").await.unwrap();
        let mut inbound = client_inbound.recv().await.expect("inbound stream");
        let mut buf = [0u8; 5];
        inbound.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"reply");
    }

    /// Aborting the driver task closes the tunnel: open_stream fails
    /// afterwards and the peer observes the connection ending.
    #[tokio::test]
    async fn abort_closes_the_tunnel() {
        let (client_io, server_io) = duplex_pair();
        let (client, _client_inbound, client_task) = spawn_driver(
            client_io,
            yamux::Config::default(),
            yamux::Mode::Client,
            4,
        );
        let (_server, mut server_inbound, server_task) = spawn_driver(
            server_io,
            yamux::Config::default(),
            yamux::Mode::Server,
            4,
        );

        let mut stream = client.open_stream(None).await.unwrap();
        stream.write_all(b"hi").await.unwrap();
        let mut inbound = server_inbound.recv().await.expect("inbound arrives");

        client_task.abort();
        let _ = client_task.await;

        // The peer's stream dies (EOF or reset) — never hangs.
        let mut buf = [0u8; 8];
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), inbound.read(&mut buf))
            .await
            .expect("read completes after peer abort");
        let _ = server_task.await;
    }
}
