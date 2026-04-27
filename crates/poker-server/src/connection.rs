//! Per-connection shared state.
//!
//! A connection has three concurrent owners: the read task that
//! parses inbound messages, the write task that drains a queue back
//! to the wire, and any [`RemoteAgent`](crate::remote_agent::RemoteAgent)
//! built for this player while they are sat at a table. They all hold
//! `Arc<Connection>`, so the data here is the canonical source of
//! truth for "who this socket belongs to and where their action
//! response should land."

use std::sync::Mutex as StdMutex;

use tokio::io::AsyncWrite;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use poker_engine::game::{Action, HandId, LegalActions};
use poker_engine::net::protocol::{PlayerId, ServerMessage, TableId};

use crate::wire::write_message;

/// Capacity of the per-connection outbound queue. Pipelined event
/// streams (`HoleCardsDealt`, `BoardDealt`, ...) and prompts go
/// through this queue. 256 is more than enough for any one hand.
const OUTBOUND_CAPACITY: usize = 256;

/// One in-flight prompt awaiting the client's response.
pub struct PendingAction {
    pub table_id: TableId,
    pub hand_id: HandId,
    pub legal: LegalActions,
    pub responder: oneshot::Sender<Action>,
}

/// Shared per-connection state.
pub struct Connection {
    pub player_id: PlayerId,
    pub username: String,
    /// Send a message to the client. Cheap to clone.
    pub out_tx: mpsc::Sender<ServerMessage>,
    /// Set by `RemoteAgent::act` before it issues a `Prompt`; taken
    /// by the session's `SubmitAction` handler. `std::sync::Mutex`
    /// (not tokio) so the engine's blocking `act()` can lock it
    /// without an async runtime.
    pub pending: StdMutex<Option<PendingAction>>,
}

impl Connection {
    pub fn new(player_id: PlayerId, username: String) -> (Self, mpsc::Receiver<ServerMessage>) {
        let (out_tx, out_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        (
            Self {
                player_id,
                username,
                out_tx,
                pending: StdMutex::new(None),
            },
            out_rx,
        )
    }

    /// Best-effort send. If the queue is full we drop and warn — the
    /// client is too slow to keep up, and stalling the server-side
    /// game loop is worse than the client missing a frame.
    pub fn try_send(&self, msg: ServerMessage) {
        if let Err(e) = self.out_tx.try_send(msg) {
            warn!(player_id = self.player_id, error = %e, "drop outbound message");
        }
    }

    /// Resolve a pending prompt. Returns `false` if the action did
    /// not match an outstanding prompt (e.g. stale `hand_id`); the
    /// caller should reply with `ActionRejected` in that case.
    pub fn deliver_action(
        &self,
        table_id: TableId,
        hand_id: HandId,
        action: Action,
    ) -> bool {
        let mut slot = self.pending.lock().expect("pending mutex poisoned");
        let Some(pending) = slot.as_ref() else {
            return false;
        };
        if pending.table_id != table_id || pending.hand_id != hand_id {
            return false;
        }
        if !pending.legal.is_legal(action) {
            return false;
        }
        let pending = slot.take().unwrap();
        pending.responder.send(action).is_ok()
    }

    /// Drop any in-flight prompt without resolving it. Used on
    /// disconnect so the engine's blocking `act()` returns instead
    /// of waiting forever.
    pub fn cancel_pending(&self) {
        let mut slot = self.pending.lock().expect("pending mutex poisoned");
        if let Some(p) = slot.take() {
            // Sending `Fold` here would race the timeout path; just
            // drop the sender — the receiver yields a `RecvError`
            // which `RemoteAgent::act` interprets as fold.
            drop(p);
        }
    }
}

/// Drive the write side of one connection: drain the outbound queue
/// and write each message as a framed msgpack packet. Returns when
/// the queue closes (last sender dropped) or the socket errors.
pub async fn writer_task<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut out_rx: mpsc::Receiver<ServerMessage>,
) {
    while let Some(msg) = out_rx.recv().await {
        if let Err(e) = write_message(&mut writer, &msg).await {
            warn!(error = %e, "writer task: send failed");
            break;
        }
    }
}
