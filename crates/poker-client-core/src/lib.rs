//! Pure synchronous client state machine. **No I/O, no async, no platform
//! code.** Depends only on [`poker_engine`] for wire types.
//!
//! See [`docs/CLIENT_PRINCIPLES.md`](../../../../docs/CLIENT_PRINCIPLES.md)
//! for the load-bearing principles.
//!
//! # Shape
//!
//! ```text
//!     ┌──────────────────────────┐
//!     │      transport / app     │
//!     │  ──────────────────────  │
//!     │  inbound ServerMessage ──┼───▶ ClientCore::handle_inbound
//!     │  user Intent           ──┼───▶ ClientCore::handle_intent
//!     │  ◀── Effect stream     ──┤
//!     │  ◀── ClientView (poll) ──┤
//!     └──────────────────────────┘
//! ```
//!
//! Every call to `handle_inbound` / `handle_intent` is synchronous,
//! deterministic, and returns the same `Vec<Effect>` for the same input
//! sequence. That property is what makes principle 1 (strict correctness
//! validated by tests) cheap: every state transition is testable in a few
//! lines.
//!
//! `ClientView` is the read-only projection the host renders. It is the
//! whole authoritative client state — the frontend MUST NOT keep a
//! parallel copy. See principle 2 in `CLIENT_PRINCIPLES.md`.

#![forbid(unsafe_code)]

pub mod effect;
pub mod intent;
pub mod view;

mod core;

pub use core::ClientCore;
pub use effect::{Effect, LogLevel};
pub use intent::Intent;
pub use view::{ClientView, Phase};

/// Errors the host might surface back into the core (transport
/// failures, etc.). The core itself never returns errors from its
/// public API — failures are encoded as `Effect`s and reflected in
/// the `ClientView`.
#[derive(Debug, thiserror::Error)]
pub enum ExternalError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("I/O error: {0}")]
    Io(String),
}
