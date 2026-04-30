//! Native (tokio TCP + filesystem) transport for
//! [`poker_client_core`]. DESIGN.md step 25b.
//!
//! Two layers:
//!
//! 1. A small [`Transport`] trait with a channel-backed
//!    [`TransportHandle`]. Native TCP ([`NativeTransport`]) is the
//!    only implementation today; a future wasm/WebSocket transport
//!    will implement the same trait and hand back the same handle
//!    shape, keeping the core agnostic of platform.
//! 2. A [`NativeClient`] runtime facade that owns the core, the
//!    transport, and a filesystem session-key [`SessionStore`]
//!    behind a thread-safe synchronous API — what the Tauri shell
//!    (and integration tests) actually consume.
//!
//! See [`docs/CLIENT_PRINCIPLES.md`](../../../../docs/CLIENT_PRINCIPLES.md)
//! for the load-bearing architectural constraints. In particular:
//! the transport owns all I/O, all session-key persistence, and
//! all platform plumbing; the core stays sync and deterministic.

#![forbid(unsafe_code)]

mod native;
mod runtime;
mod session;
mod transport;

pub use native::{NativeTransport, NativeTransportError};
pub use runtime::{LogEntry, NativeClient};
pub use session::SessionStore;
pub use transport::{Closed, Transport, TransportHandle, TransportIn};
