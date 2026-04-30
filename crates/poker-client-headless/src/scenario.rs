//! Scripted test scenarios. **Stub at step 25c** — full DSL lands as
//! the workspace-root full-flow tests are written; this file fixes
//! the public path so dependents can reference it.

use poker_client_core::Intent;
use poker_engine::net::protocol::ServerMessage;

/// One step in a scripted scenario. The harness alternates
/// `Intent`-style inputs and inbound `ServerMessage`s in whatever
/// order the test demands.
#[derive(Debug, Clone)]
pub enum Step {
    Issue(Intent),
    Receive(ServerMessage),
}

/// A linear sequence of steps. Drives a
/// [`super::HeadlessClient`] in `Step` order.
pub type Script = Vec<Step>;
