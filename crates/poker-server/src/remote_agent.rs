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
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tracing::debug;

use poker_engine::agent::{Agent, Observation};
use poker_engine::game::{Action, EngineEvent, HandId, HandResult};
use poker_engine::net::protocol::{ServerMessage, TableId};

use crate::connection::{PendingAction, SeatLink};

pub struct RemoteAgent {
    pub link: Arc<SeatLink>,
    pub table_id: TableId,
    pub current_hand: HandId,
    pub deadline: Duration,
    pub runtime: Handle,
}

impl RemoteAgent {
    pub fn new(
        link: Arc<SeatLink>,
        table_id: TableId,
        deadline: Duration,
        runtime: Handle,
    ) -> Self {
        Self { link, table_id, current_hand: 0, deadline, runtime }
    }
}

impl Agent for RemoteAgent {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let (tx, rx) = oneshot::channel();
        // Resolve the seat's CURRENT connection. After a reconnect
        // (step 22c) this is the new socket; the previous one is gone.
        let conn = self.link.current();

        // Fast-fold: if the connection has been marked as departed
        // (LeaveTable or non-superseded session close), skip the prompt
        // and fold immediately to avoid stalling the engine for the
        // full action deadline. See server-behavior spec: "Force-fold
        // on mid-hand leave".
        if conn.departed.load(Ordering::Acquire) {
            debug!(player_id = conn.player_id, "remote agent: connection departed, fast-folding");
            return Action::Fold;
        }

        let pending = PendingAction {
            table_id: self.table_id,
            hand_id: self.current_hand,
            seat: obs.position,
            legal: obs.legal_actions,
            responder: tx,
        };
        {
            let mut slot = conn.pending.lock().expect("pending mutex poisoned");
            *slot = Some(pending);
        }

        conn.try_send(ServerMessage::Prompt {
            table_id: self.table_id,
            hand_id: self.current_hand,
            seat: obs.position,
            legal: obs.legal_actions,
            deadline_ms: self.deadline.as_millis() as u32,
        });
        // Mirror into the seat's replay buffer so a mid-hand
        // `RequestReplay` reconstructs the in-flight prompt the same
        // way the seat saw it live. The re-issue path in
        // `Table::reconnect_player` still re-sends the active prompt
        // directly, so replay + reconnect compose idempotently.
        self.link.replay_push(&ServerMessage::Prompt {
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

        // Re-resolve the link in case a reconnect swapped the
        // connection underneath us; defensive cleanup of the pending
        // slot lands on the *current* one.
        let cleanup = self.link.current();
        match result {
            Ok(Ok(action)) => action,
            Ok(Err(_recv_err)) => {
                let _ = cleanup.pending.lock().map(|mut s| s.take());
                debug!(player_id = cleanup.player_id, "remote agent: response channel dropped, folding");
                Action::Fold
            }
            Err(_elapsed) => {
                let _ = cleanup.pending.lock().map(|mut s| s.take());
                debug!(player_id = cleanup.player_id, "remote agent: action deadline elapsed, folding");
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
