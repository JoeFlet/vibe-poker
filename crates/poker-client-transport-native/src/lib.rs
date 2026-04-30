//! Native (tokio TCP + filesystem) transport for [`poker_client_core`].
//!
//! **Skeleton at step 25b.** Provides the `Transport` trait that the
//! core's effect pump consumes, plus a `NativeTransport` placeholder
//! whose actual wire I/O lands in follow-up commits as the core's
//! state machine fills in. The split between `core` (stateful, sync,
//! deterministic) and `transport` (stateless I/O glue) is the
//! load-bearing architectural seam from
//! [`docs/CLIENT_PRINCIPLES.md`](../../../../docs/CLIENT_PRINCIPLES.md).

#![forbid(unsafe_code)]

use std::path::PathBuf;

use poker_client_core::Effect;
use poker_engine::net::protocol::ServerMessage;

/// Abstract over native vs (future) browser transport. The host
/// pumps a [`poker_client_core::ClientCore`] through `Transport`:
/// every `Effect::Send` becomes `Transport::send`, every inbound
/// `ServerMessage` becomes a `ClientCore::handle_inbound`, and so on.
///
/// Async on purpose so the wasm transport can implement it on top
/// of WebSocket promises without re-thinking the surface.
#[allow(async_fn_in_trait)]
pub trait Transport {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Open the underlying connection. Implementors typically also
    /// kick off a reader task that pumps inbound frames into the
    /// host (see [`NativeTransport::take_inbound`]).
    async fn open(&mut self, addr: &str) -> Result<(), Self::Error>;

    /// Send a single client message over the wire. Returns once the
    /// frame is queued; doesn't wait for the peer.
    async fn send(
        &mut self,
        msg: poker_engine::net::protocol::ClientMessage,
    ) -> Result<(), Self::Error>;

    /// Close the connection. Idempotent — safe to call when already
    /// closed.
    async fn close(&mut self);
}

/// Errors the native transport can raise. String-typed leaves keep
/// the public surface small while we build out the implementation.
#[derive(Debug, thiserror::Error)]
pub enum NativeTransportError {
    #[error("not yet implemented: {0}")]
    NotImplemented(&'static str),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Tokio-driven TCP transport with filesystem-backed session-key
/// persistence. Skeleton — actual wire pumping lands in step 25b
/// follow-up commits.
pub struct NativeTransport {
    /// Where the persisted session key lives on disk. Defaults to
    /// the platform's data directory; tests override.
    pub session_path: PathBuf,
}

impl NativeTransport {
    pub fn new(session_path: PathBuf) -> Self {
        Self { session_path }
    }

    /// Drain the inbound queue. Returns nothing yet; placeholder so
    /// the public shape is set.
    pub fn take_inbound(&mut self) -> Vec<ServerMessage> {
        Vec::new()
    }
}

impl Transport for NativeTransport {
    type Error = NativeTransportError;

    async fn open(&mut self, _addr: &str) -> Result<(), Self::Error> {
        Err(NativeTransportError::NotImplemented(
            "NativeTransport::open: scaffolded at step 25b, implementation in follow-up",
        ))
    }

    async fn send(
        &mut self,
        _msg: poker_engine::net::protocol::ClientMessage,
    ) -> Result<(), Self::Error> {
        Err(NativeTransportError::NotImplemented(
            "NativeTransport::send: scaffolded at step 25b",
        ))
    }

    async fn close(&mut self) {
        // No-op until `open` is implemented.
    }
}

/// Apply one effect against a transport. Returned `Vec<ServerMessage>`
/// is messages the transport synthesised for the core (typically
/// empty until inbound pumping is wired in).
///
/// Skeleton — the real pump uses a tokio task per direction; this
/// signature pins the integration shape so the headless harness can
/// build against it today.
pub async fn apply<T: Transport>(
    transport: &mut T,
    effect: Effect,
) -> Result<Vec<ServerMessage>, T::Error> {
    match effect {
        Effect::OpenConnection { addr } => {
            transport.open(&addr).await?;
        }
        Effect::CloseConnection => {
            transport.close().await;
        }
        Effect::Send(msg) => {
            transport.send(msg).await?;
        }
        Effect::PersistSessionKey(_)
        | Effect::Schedule { .. }
        | Effect::Log { .. } => {
            // Persistence + scheduling + logging are host concerns;
            // the transport routes the wire-side effects only.
        }
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_transport_open_returns_not_implemented_for_now() {
        let mut t = NativeTransport::new(PathBuf::from("/tmp/session.dat"));
        let err = t.open("127.0.0.1:7878").await.unwrap_err();
        assert!(matches!(err, NativeTransportError::NotImplemented(_)));
    }
}
