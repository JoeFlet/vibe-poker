//! Headless client harness used by the workspace-root full-flow
//! tests in `tests/`. **Skeleton at step 25c.**
//!
//! Purpose: drive a [`poker_client_core::ClientCore`] against an
//! in-process [`poker-server`] without any of the Tauri / SolidJS
//! shell, so principle 1 (strict correctness, full-flow tests) is
//! attainable cheaply.
//!
//! Two pieces:
//!
//! - [`HeadlessClient`] — a synchronous helper that wraps
//!   `ClientCore`, records the full `Effect` stream, and exposes
//!   waiter helpers (`wait_for_phase`, `wait_for_view`).
//! - [`scenario`] — a small DSL for scripted scenarios (a `Vec<Step>`
//!   that the harness pumps; on each step the resulting snapshot
//!   is checked against assertions).

#![forbid(unsafe_code)]

use poker_client_core::{ClientCore, ClientView, Effect, Intent, Phase};
use poker_engine::net::protocol::ServerMessage;

pub mod scenario;

/// Wraps a [`ClientCore`] with effect recording and assertion
/// helpers. The full effect stream is preserved across every
/// `intent` / `inbound` call, so failed assertions can dump it.
pub struct HeadlessClient {
    core: ClientCore,
    effects: Vec<Effect>,
    snapshots: Vec<ClientView>,
}

impl HeadlessClient {
    pub fn new(initial_session_key: Option<String>) -> Self {
        let core = ClientCore::new(initial_session_key);
        let initial_snapshot = core.snapshot();
        Self {
            core,
            effects: Vec::new(),
            snapshots: vec![initial_snapshot],
        }
    }

    /// Issue an intent and return the slice of effects produced.
    /// The full effect stream is also retained on the harness for
    /// later inspection.
    pub fn intent(&mut self, intent: Intent) -> &[Effect] {
        let start = self.effects.len();
        let effs = self.core.handle_intent(intent);
        self.effects.extend(effs);
        self.snapshots.push(self.core.snapshot());
        &self.effects[start..]
    }

    /// Hand an inbound `ServerMessage` to the core and return the
    /// effects it produced.
    pub fn inbound(&mut self, msg: ServerMessage) -> &[Effect] {
        let start = self.effects.len();
        let effs = self.core.handle_inbound(msg);
        self.effects.extend(effs);
        self.snapshots.push(self.core.snapshot());
        &self.effects[start..]
    }

    /// Read-only access to the core's view.
    pub fn view(&self) -> ClientView {
        self.core.snapshot()
    }

    /// Full effect stream since the harness was constructed.
    pub fn effect_log(&self) -> &[Effect] {
        &self.effects
    }

    /// All snapshots taken (one initial + one per intent/inbound).
    /// Useful in failure messages: prints the entire state lineage.
    pub fn snapshot_log(&self) -> &[ClientView] {
        &self.snapshots
    }

    /// Synchronous predicate check; for time-based waits the
    /// caller drives the harness via inbound messages or scheduled
    /// intents. The harness itself has no clock.
    pub fn assert_phase(&self, expected: &Phase) {
        let actual = &self.core.snapshot().phase;
        assert_eq!(
            actual, expected,
            "phase mismatch\n  expected: {expected:?}\n  actual:   {actual:?}\n  effect log:\n{}",
            self.effect_log()
                .iter()
                .map(|e| format!("    {e:?}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_engine::net::protocol::{LifetimeStats, PROTOCOL_VERSION};

    #[test]
    fn fresh_harness_starts_disconnected() {
        let h = HeadlessClient::new(None);
        h.assert_phase(&Phase::Disconnected);
    }

    #[test]
    fn drive_to_lobby_via_welcome() {
        let mut h = HeadlessClient::new(None);
        h.intent(Intent::Connect { addr: "addr".into() });
        h.assert_phase(&Phase::Connecting);

        h.inbound(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: 1,
            username: "alice".into(),
            session_key: "abc".into(),
            stats: LifetimeStats::default(),
        });
        h.assert_phase(&Phase::Lobby);
        assert_eq!(h.view().username.as_deref(), Some("alice"));
    }
}
