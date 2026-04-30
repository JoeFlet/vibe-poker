//! In-memory `Transport` implementation for testing.
//!
//! [`InMemoryTransport`] satisfies the same
//! [`poker_client_transport_native::Transport`] trait that
//! [`poker_client_transport_native::NativeTransport`] does, so a
//! test can hand it to [`poker_client_transport_native::NativeClient::with_transport`]
//! and exercise the full runtime facade without touching a socket.
//! The paired [`InMemoryServer`] is the test-side peer: push
//! [`ServerMessage`]s to it, pull [`ClientMessage`]s off it,
//! simulate disconnects at will.
//!
//! Each call to [`InMemoryTransport::open`] produces a fresh pair
//! of channels and hands the server side to the
//! [`InMemoryConnections`] queue. Scripts that reconnect just
//! receive the next [`InMemoryServer`] off the queue.
//!
//! # Example
//!
//! ```ignore
//! use poker_client_headless::in_memory_pair;
//! use poker_client_transport_native::{NativeClient, SessionStore};
//! use poker_client_core::Intent;
//!
//! let (transport, mut conns) = in_memory_pair();
//! let session = SessionStore::new("/tmp/session.key".into());
//! let client = NativeClient::with_transport(transport, session).unwrap();
//!
//! client.issue(Intent::Connect { addr: "test".into() });
//! // Grab the server side of the (only) connection (async):
//! // let server = conns.next().await.unwrap();
//! // Scripts can then call `server.send(ServerMessage::...)` etc.
//! ```

use poker_client_transport_native::{Transport, TransportHandle, TransportIn};
use poker_engine::net::protocol::{ClientMessage, ServerMessage};
use tokio::sync::mpsc;

/// Errors raised by [`InMemoryTransport::open`]. None in practice —
/// the in-memory pair never fails to open — but the type exists
/// so the `Transport` trait bound is satisfiable.
#[derive(Debug, thiserror::Error)]
pub enum InMemoryTransportError {
    /// Would be raised if the paired [`InMemoryConnections`] has
    /// been dropped by the test, i.e. nobody is listening for new
    /// "connections" any more. Treat as programmer error.
    #[error("paired InMemoryConnections was dropped; no listener")]
    Dropped,
}

/// The client-side factory. Cloneable by construction — call
/// [`crate::in_memory_pair`] to obtain one together with its
/// matching [`InMemoryConnections`].
#[derive(Clone, Debug)]
pub struct InMemoryTransport {
    new_connection_tx: mpsc::UnboundedSender<InMemoryServer>,
}

/// The test-side "listener" for new connections. A script pulls an
/// [`InMemoryServer`] off this queue each time the client opens a
/// connection, then drives the server side of that connection.
pub struct InMemoryConnections {
    new_connection_rx: mpsc::UnboundedReceiver<InMemoryServer>,
}

impl InMemoryConnections {
    /// Await the next client-initiated connection. Returns `None`
    /// if the paired [`InMemoryTransport`] has been dropped and no
    /// further connections will arrive.
    pub async fn next(&mut self) -> Option<InMemoryServer> {
        self.new_connection_rx.recv().await
    }

    /// Non-blocking pull. `None` if nothing pending right now.
    pub fn try_next(&mut self) -> Option<InMemoryServer> {
        self.new_connection_rx.try_recv().ok()
    }
}

/// One connection's worth of server-side plumbing. Dropping this
/// value closes the connection (the client sees
/// [`TransportIn::Lost`] with reason `"server dropped"`).
#[derive(Debug)]
pub struct InMemoryServer {
    inbound_rx: mpsc::UnboundedReceiver<ClientMessage>,
    outbound_tx: mpsc::UnboundedSender<TransportIn>,
}

impl InMemoryServer {
    /// Send a [`ServerMessage`] to the client. `Err` only if the
    /// client has already dropped its handle.
    pub fn send(&self, msg: ServerMessage) -> Result<(), Disconnected> {
        self.outbound_tx
            .send(TransportIn::Message(msg))
            .map_err(|_| Disconnected)
    }

    /// Tear the connection down from the server side. After this
    /// call the client receives [`TransportIn::Lost { reason }`].
    pub fn close(self, reason: impl Into<String>) {
        let _ = self.outbound_tx.send(TransportIn::Lost {
            reason: reason.into(),
        });
    }

    /// Await the next outbound [`ClientMessage`] the client wrote.
    /// Returns `None` once the client has dropped its handle.
    pub async fn recv(&mut self) -> Option<ClientMessage> {
        self.inbound_rx.recv().await
    }

    /// Non-blocking variant.
    pub fn try_recv(&mut self) -> Option<ClientMessage> {
        self.inbound_rx.try_recv().ok()
    }
}

/// Drop into the client channel as a failed-send marker.
#[derive(Debug, thiserror::Error)]
#[error("client handle already dropped")]
pub struct Disconnected;

/// Construct a client-side [`InMemoryTransport`] and its paired
/// server-side [`InMemoryConnections`] listener. The two halves
/// are joined by an unbounded channel.
///
/// Each `transport.open()` call allocates a fresh
/// client↔server channel pair and pushes the server side onto
/// the listener.
pub fn in_memory_pair() -> (InMemoryTransport, InMemoryConnections) {
    let (new_connection_tx, new_connection_rx) = mpsc::unbounded_channel();
    (
        InMemoryTransport { new_connection_tx },
        InMemoryConnections { new_connection_rx },
    )
}

impl Transport for InMemoryTransport {
    type OpenError = InMemoryTransportError;

    async fn open(&self, _addr: String) -> Result<TransportHandle, Self::OpenError> {
        let (c2s_tx, c2s_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (s2c_tx, s2c_rx) = mpsc::unbounded_channel::<TransportIn>();

        // Mirror NativeTransport: fire TransportIn::Connected
        // immediately so the host observes the "TCP up" transition.
        // If this fails the client dropped during open — still
        // legal; we just skip.
        let _ = s2c_tx.send(TransportIn::Connected);

        let server = InMemoryServer {
            inbound_rx: c2s_rx,
            outbound_tx: s2c_tx,
        };
        self.new_connection_tx
            .send(server)
            .map_err(|_| InMemoryTransportError::Dropped)?;

        Ok(TransportHandle::from_channels(c2s_tx, s2c_rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_client_transport_native::Transport;

    #[tokio::test]
    async fn open_produces_matched_pair_and_fires_connected() {
        let (t, mut conns) = in_memory_pair();
        let mut handle = t.open("test".into()).await.unwrap();

        // Client observes TransportIn::Connected first.
        let first = handle.recv().await.expect("connected");
        assert!(matches!(first, TransportIn::Connected));

        // Server side is listening.
        let mut server = conns.next().await.expect("server side");

        // Client → server.
        handle.send(ClientMessage::Heartbeat).unwrap();
        match server.recv().await {
            Some(ClientMessage::Heartbeat) => {}
            other => panic!("expected Heartbeat, got {other:?}"),
        }

        // Server → client.
        server.send(ServerMessage::Heartbeat).unwrap();
        match handle.recv().await {
            Some(TransportIn::Message(ServerMessage::Heartbeat)) => {}
            other => panic!("expected Heartbeat message, got {other:?}"),
        }

        // Server close → client sees Lost.
        server.close("test-end");
        match handle.recv().await {
            Some(TransportIn::Lost { reason }) => assert_eq!(reason, "test-end"),
            other => panic!("expected Lost, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn open_fails_after_connections_dropped() {
        let (t, conns) = in_memory_pair();
        drop(conns);

        match t.open("test".into()).await {
            Err(InMemoryTransportError::Dropped) => {}
            Ok(_) => panic!("expected Err(Dropped)"),
        }
    }
}
