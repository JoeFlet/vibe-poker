//! Headless client harness used by the workspace-root full-flow
//! tests and scripted unit tests. DESIGN.md step 25c.
//!
//! Three layers:
//!
//! - [`HeadlessClient`] wraps a [`poker_client_core::ClientCore`]
//!   with effect and snapshot logging. Scripts push intents and
//!   inbound server messages **directly**, bypassing any transport.
//! - [`scenario`] provides a scripted scenario DSL
//!   ([`scenario::Step`] / [`scenario::Script`] /
//!   [`scenario::Driver`]) plus a serde-friendly
//!   [`scenario::FileScript`] for CLI reproduction files.
//! - [`transport`] supplies an [`InMemoryTransport`] that
//!   implements [`poker_client_transport_native::Transport`], so
//!   tests can drive a real [`poker_client_transport_native::NativeClient`]
//!   runtime without touching a socket.
//!
//! See [`docs/CLIENT_PRINCIPLES.md`](../../../../docs/CLIENT_PRINCIPLES.md)
//! §1 for why cross-crate full-flow tests live at the workspace
//! root rather than under this crate; tests here are scoped to
//! this crate's own harness helpers.

#![forbid(unsafe_code)]

mod harness;
pub mod scenario;
pub mod transport;

pub use harness::HeadlessClient;
pub use scenario::{Driver, DriverResult, FileScript, FileStep, Script, Step};
pub use transport::{
    Disconnected, InMemoryConnections, InMemoryServer, InMemoryTransport, InMemoryTransportError,
    in_memory_pair,
};
