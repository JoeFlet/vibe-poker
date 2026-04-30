//! Abstract transport layer.
//!
//! Native TCP and (future) browser WebSocket transports both hand
//! back the same [`TransportHandle`] — a pair of channels carrying
//! outbound [`ClientMessage`]s one way and inbound [`TransportIn`]
//! events the other. The sync / deterministic
//! [`poker_client_core::ClientCore`] never touches a socket; it
//! only feeds this channel abstraction.
//!
//! The only thing a [`Transport`] implementation decides is how to
//! open a new connection.

use std::future::Future;

use poker_engine::net::protocol::{ClientMessage, ServerMessage};
use tokio::sync::mpsc;

/// Events a transport reports back to its host.
///
/// - [`Self::Connected`] fires exactly once per successful open,
///   immediately after the socket is up. The host typically
///   translates this into
///   [`poker_client_core::Intent::ConnectionOpened`].
/// - [`Self::Message`] carries a decoded inbound
///   [`ServerMessage`]. One per frame.
/// - [`Self::Lost`] is terminal — the pump task has already
///   exited by the time it is delivered. The host typically
///   translates this into
///   [`poker_client_core::Intent::ConnectionLost`].
#[derive(Debug)]
pub enum TransportIn {
    Connected,
    Message(ServerMessage),
    Lost { reason: String },
}

/// Returned by every [`Transport`] implementor. Channel-backed so
/// the sync core can consume inbound events without touching the
/// async world directly.
///
/// The handle is `!Clone`: a second consumer would race the
/// receiver. If you need shared access, wrap it in a mutex.
#[derive(Debug)]
pub struct TransportHandle {
    outbound_tx: mpsc::UnboundedSender<ClientMessage>,
    inbound_rx: mpsc::UnboundedReceiver<TransportIn>,
}

/// `send` failed because the transport worker has exited. Drop the
/// handle and open a fresh one to recover.
#[derive(Debug, thiserror::Error)]
#[error("transport closed")]
pub struct Closed;

impl TransportHandle {
    /// Construction is `pub(crate)` so only our own transport
    /// implementations can produce one in the default code path.
    pub(crate) fn new(
        outbound_tx: mpsc::UnboundedSender<ClientMessage>,
        inbound_rx: mpsc::UnboundedReceiver<TransportIn>,
    ) -> Self {
        Self {
            outbound_tx,
            inbound_rx,
        }
    }

    /// Constructor for out-of-crate [`Transport`] implementors
    /// (e.g. the in-memory transport in `poker-client-headless`).
    /// The channel ends are the ones the host will see: `outbound_tx`
    /// is where [`Self::send`] pushes [`ClientMessage`]s, and
    /// `inbound_rx` is where [`Self::recv`] / [`Self::try_recv`]
    /// pulls [`TransportIn`] events from.
    ///
    /// Kept as a separate named function (rather than making
    /// `new` itself `pub`) so the extension point is explicit and
    /// easy to audit.
    pub fn from_channels(
        outbound_tx: mpsc::UnboundedSender<ClientMessage>,
        inbound_rx: mpsc::UnboundedReceiver<TransportIn>,
    ) -> Self {
        Self::new(outbound_tx, inbound_rx)
    }

    /// Queue an outbound message. Returns [`Closed`] once the
    /// pump task has exited (peer closed, I/O error, etc.); the
    /// matching [`TransportIn::Lost`] will already be, or shortly
    /// be, in the inbound queue.
    pub fn send(&self, msg: ClientMessage) -> Result<(), Closed> {
        self.outbound_tx.send(msg).map_err(|_| Closed)
    }

    /// Await the next inbound event. Returns `None` when the
    /// pump task has exited *and* the inbound queue is drained —
    /// which is strictly after a [`TransportIn::Lost`] has been
    /// delivered, so hosts that follow `Lost` with "stop reading"
    /// will never see `None`.
    pub async fn recv(&mut self) -> Option<TransportIn> {
        self.inbound_rx.recv().await
    }

    /// Non-blocking inbound drain. `None` if no event is ready
    /// right now.
    pub fn try_recv(&mut self) -> Option<TransportIn> {
        self.inbound_rx.try_recv().ok()
    }
}

/// Factory for transports. Stateless in practice — the factory
/// instance stores config knobs (future: TLS roots, connect
/// timeout); actual per-connection state lives on the returned
/// [`TransportHandle`].
///
/// Trait bounds:
/// - `Clone` so the runtime can hand a copy to future reconnect
///   logic without re-threading the factory through every layer.
/// - `Send + 'static` because both native and browser
///   implementations run their pump on a spawned task.
pub trait Transport: Clone + Send + 'static {
    /// Errors raised during `open` — i.e. before we have a live
    /// socket. Failures after open surface as
    /// [`TransportIn::Lost`] instead.
    type OpenError: std::error::Error + Send + Sync + 'static;

    /// Open a new connection. Spawns whatever background task
    /// (tokio, wasm-bindgen) the implementation needs; the task
    /// exits when either the peer closes or the [`TransportHandle`]
    /// is dropped.
    ///
    /// `addr` is an implementation-specific string. For
    /// [`crate::NativeTransport`] it is anything [`tokio::net::TcpStream`]
    /// accepts (`host:port`). For a future browser transport it
    /// would be a `ws://` / `wss://` URL.
    fn open(
        &self,
        addr: String,
    ) -> impl Future<Output = Result<TransportHandle, Self::OpenError>> + Send;
}
