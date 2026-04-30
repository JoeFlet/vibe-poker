//! Tokio TCP implementation of [`Transport`].
//!
//! Opens a [`TcpStream`], splits it into owned halves, and spawns
//! a pump task that forwards framed messages both ways until
//! either the peer closes or the host drops its
//! [`TransportHandle`]. All post-connect errors surface as
//! [`TransportIn::Lost`] rather than tearing down the whole
//! [`poker_client_core::ClientCore`] state machine from below.

use std::io;

use poker_engine::net::frame::{self, FrameError, LENGTH_PREFIX_BYTES};
use poker_engine::net::protocol::{ClientMessage, ServerMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;

use crate::transport::{Transport, TransportHandle, TransportIn};

/// Errors raised before we have a live socket.
#[derive(Debug, thiserror::Error)]
pub enum NativeTransportError {
    #[error("connect {addr}: {source}")]
    Connect {
        addr: String,
        #[source]
        source: io::Error,
    },

    #[error("set_nodelay: {0}")]
    SetNoDelay(#[source] io::Error),
}

/// Tokio TCP transport factory. Cloneable by construction.
#[derive(Debug, Clone, Default)]
pub struct NativeTransport {
    // Room for future knobs (connect timeout, keepalive, TLS).
}

impl NativeTransport {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Transport for NativeTransport {
    type OpenError = NativeTransportError;

    // Written as `async fn` here (rather than the `fn ... -> impl
    // Future + Send` shape on the trait) because the trait's Send
    // bound is inherited by the impl. Keeping the trait explicit
    // documents the requirement for future implementors.
    async fn open(&self, addr: String) -> Result<TransportHandle, Self::OpenError> {
        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|source| NativeTransportError::Connect {
                addr: addr.clone(),
                source,
            })?;
        stream
            .set_nodelay(true)
            .map_err(NativeTransportError::SetNoDelay)?;
        let (reader, writer) = stream.into_split();

        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<TransportIn>();

        // The first thing the host sees is Connected. Pushing it
        // before spawning the pump guarantees order: the pump's
        // first message (if any) lands strictly after.
        let _ = inbound_tx.send(TransportIn::Connected);

        tokio::spawn(run_pump(reader, writer, outbound_rx, inbound_tx));

        Ok(TransportHandle::new(outbound_tx, inbound_rx))
    }
}

/// The pump. Owns the two socket halves and the two channel ends
/// until either side reports end-of-stream.
async fn run_pump(
    mut reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
    mut outbound_rx: mpsc::UnboundedReceiver<ClientMessage>,
    inbound_tx: mpsc::UnboundedSender<TransportIn>,
) {
    loop {
        tokio::select! {
            outbound = outbound_rx.recv() => match outbound {
                Some(msg) => {
                    if let Err(e) = send_one(&mut writer, &msg).await {
                        let _ = inbound_tx.send(TransportIn::Lost { reason: e });
                        return;
                    }
                }
                None => {
                    // Host dropped the handle: clean shutdown.
                    let _ = writer.shutdown().await;
                    return;
                }
            },
            inbound = read_one(&mut reader) => match inbound {
                Ok(msg) => {
                    if inbound_tx.send(TransportIn::Message(msg)).is_err() {
                        // Host dropped the receiver: clean shutdown.
                        let _ = writer.shutdown().await;
                        return;
                    }
                }
                Err(reason) => {
                    let _ = inbound_tx.send(TransportIn::Lost { reason });
                    return;
                }
            },
        }
    }
}

async fn send_one(writer: &mut OwnedWriteHalf, msg: &ClientMessage) -> Result<(), String> {
    let bytes = frame::encode(msg).map_err(|e: FrameError| format!("encode: {e}"))?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|e: io::Error| format!("write: {e}"))?;
    writer
        .flush()
        .await
        .map_err(|e: io::Error| format!("flush: {e}"))?;
    Ok(())
}

async fn read_one(reader: &mut OwnedReadHalf) -> Result<ServerMessage, String> {
    let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
    match reader.read_exact(&mut prefix).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            return Err("server closed connection".to_string());
        }
        Err(e) => return Err(format!("read: {e}")),
    }
    let len = frame::parse_length_prefix(prefix).map_err(|e: FrameError| format!("frame: {e}"))?;
    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|e: io::Error| format!("read payload: {e}"))?;
    frame::decode::<ServerMessage>(&payload).map_err(|e: FrameError| format!("decode: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_engine::net::protocol::{PROTOCOL_VERSION, ServerMessage};
    use tokio::net::TcpListener;

    /// End-to-end loopback: spin up a minimal TCP server that
    /// reads one framed ClientMessage, replies with one framed
    /// ServerMessage, and closes. The `NativeTransport` handle
    /// must see Connected → Message → Lost in order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn roundtrip_against_echo_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut rd, mut wr) = stream.into_split();

            // Read one frame (ignore its contents).
            let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
            rd.read_exact(&mut prefix).await.unwrap();
            let len = frame::parse_length_prefix(prefix).unwrap();
            let mut payload = vec![0u8; len];
            rd.read_exact(&mut payload).await.unwrap();

            // Send one Goodbye and close.
            let bytes = frame::encode(&ServerMessage::Goodbye {
                reason: "test-end".into(),
            })
            .unwrap();
            wr.write_all(&bytes).await.unwrap();
            wr.flush().await.unwrap();
            drop(wr); // half-close; reader side stays open till pump exits
        });

        let t = NativeTransport::new();
        let mut handle = t.open(addr.to_string()).await.unwrap();

        // First inbound is always Connected.
        match handle.recv().await {
            Some(TransportIn::Connected) => {}
            other => panic!("expected Connected, got {other:?}"),
        }

        // Heartbeat is the cheapest ClientMessage to round-trip.
        handle.send(ClientMessage::Heartbeat).unwrap();

        // Goodbye comes back through as Message.
        match handle.recv().await {
            Some(TransportIn::Message(ServerMessage::Goodbye { reason })) => {
                assert_eq!(reason, "test-end");
            }
            other => panic!("expected Goodbye, got {other:?}"),
        }

        // Server closed its write half → our reader hits EOF →
        // Lost is delivered.
        match handle.recv().await {
            Some(TransportIn::Lost { reason }) => {
                assert!(
                    reason.contains("closed"),
                    "unexpected Lost reason: {reason}"
                );
            }
            other => panic!("expected Lost, got {other:?}"),
        }
    }

    /// Connecting to a closed port surfaces as `OpenError` without
    /// ever producing a handle.
    #[tokio::test(flavor = "multi_thread")]
    async fn open_failure_reports_connect_error() {
        // Reserve a port then drop the listener, so the target is
        // known-not-listening but not rejected by the OS outright.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let t = NativeTransport::new();
        match t.open(addr.to_string()).await {
            Err(NativeTransportError::Connect { addr: a, .. }) => {
                assert_eq!(a, addr.to_string());
            }
            other => panic!("expected Connect error, got {other:?}"),
        }
    }

    /// Handle drop closes the write half of the socket; the pump
    /// task exits without leaking.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_drop_tears_down_pump() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let accepted = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut rd, _wr) = stream.into_split();
            // Wait for the peer to close (handle drop causes our
            // pump to shutdown its write half; read_exact sees EOF).
            let mut buf = [0u8; 4];
            // Use a timeout so a pump bug can't hang the test forever.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                rd.read_exact(&mut buf),
            )
            .await;
        });

        // Keep `PROTOCOL_VERSION` referenced so the import pulls
        // its weight on a clean build.
        let _ = PROTOCOL_VERSION;

        let t = NativeTransport::new();
        let handle = t.open(addr.to_string()).await.unwrap();
        drop(handle);

        // If the pump didn't shut down, the server task would hang
        // on read_exact and trip its timeout — the join below would
        // return `Ok(())` either way, but would be 2s slow. We
        // assert it finishes quickly.
        tokio::time::timeout(std::time::Duration::from_millis(500), accepted)
            .await
            .expect("pump failed to tear down after handle drop")
            .unwrap();
    }
}
