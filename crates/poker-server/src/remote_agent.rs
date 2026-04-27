//! `Agent` adapter that drives a remote (network) player.
//!
//! The engine calls `Agent::act` synchronously. We're inside a
//! `spawn_blocking` task on tokio's blocking pool, so we can:
//!
//!   1. Register a [`PendingAction`] on the player's [`Connection`]
//!      that carries a `oneshot::Sender<Action>`.
//!   2. Push a `Prompt` into the connection's outbound queue.
//!   3. Block on the `oneshot::Receiver` (with a deadline) using
//!      `tokio::runtime::Handle::block_on`.
//!
//! If the client disconnects, the matching session calls
//! `Connection::cancel_pending` which drops the sender — the receive
//! side then yields an error, and we fold for the player.

use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tracing::debug;

use poker_engine::agent::{Agent, Observation};
use poker_engine::game::{Action, EngineEvent, HandId, HandResult};
use poker_engine::net::protocol::{ServerMessage, TableId};

use crate::connection::{Connection, PendingAction};

pub struct RemoteAgent {
    pub conn: Arc<Connection>,
    pub table_id: TableId,
    pub current_hand: HandId,
    pub deadline: Duration,
    pub runtime: Handle,
}

impl RemoteAgent {
    pub fn new(
        conn: Arc<Connection>,
        table_id: TableId,
        deadline: Duration,
        runtime: Handle,
    ) -> Self {
        Self { conn, table_id, current_hand: 0, deadline, runtime }
    }
}

impl Agent for RemoteAgent {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let (tx, rx) = oneshot::channel();
        let pending = PendingAction {
            table_id: self.table_id,
            hand_id: self.current_hand,
            legal: obs.legal_actions,
            responder: tx,
        };
        {
            let mut slot = self.conn.pending.lock().expect("pending mutex poisoned");
            *slot = Some(pending);
        }

        self.conn.try_send(ServerMessage::Prompt {
            table_id: self.table_id,
            hand_id: self.current_hand,
            seat: obs.position,
            legal: obs.legal_actions,
            deadline_ms: self.deadline.as_millis() as u32,
        });

        let deadline = self.deadline;
        let result = self.runtime.block_on(async move {
            tokio::time::timeout(deadline, rx).await
        });

        match result {
            Ok(Ok(action)) => action,
            Ok(Err(_recv_err)) => {
                // Sender dropped — usually a disconnect. Clear any
                // residual pending entry (defensive; cancel_pending
                // already cleared it on the disconnect path).
                let _ = self.conn.pending.lock().map(|mut s| s.take());
                debug!(player_id = self.conn.player_id, "remote agent: response channel dropped, folding");
                Action::Fold
            }
            Err(_elapsed) => {
                let _ = self.conn.pending.lock().map(|mut s| s.take());
                debug!(player_id = self.conn.player_id, "remote agent: action deadline elapsed, folding");
                Action::Fold
            }
        }
    }

    fn on_event(&mut self, _event: &EngineEvent) {
        // Server-wide BroadcastSink already pushes the per-recipient
        // event stream to this connection; no per-agent work needed.
    }

    fn on_hand_start(&mut self, hand_id: HandId) {
        self.current_hand = hand_id;
    }

    fn on_hand_end(&mut self, _result: &HandResult) {
        self.current_hand = 0;
    }
}
