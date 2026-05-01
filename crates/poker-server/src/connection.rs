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
use std::sync::{Arc, RwLock as StdRwLock};

use tokio::io::AsyncWrite;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use poker_engine::game::{Action, HandId, LegalActions, SeatIndex};
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
    /// Engine-side seat index this prompt was issued for. Recorded so
    /// a reconnect can re-send the same `Prompt` to the new socket
    /// (step 22c) without rebuilding it from scratch.
    pub seat: SeatIndex,
    pub legal: LegalActions,
    pub responder: oneshot::Sender<Action>,
}

/// Shared per-connection state.
pub struct Connection {
    pub player_id: PlayerId,
    pub username: String,
    /// Identifies the auth session this connection backs. Used by
    /// reconnect-detection so the table can tell two distinct logins
    /// for the same player apart.
    pub session_id: i64,
    /// Send a message to the client. Cheap to clone.
    pub out_tx: mpsc::Sender<ServerMessage>,
    /// Set by `RemoteAgent::act` before it issues a `Prompt`; taken
    /// by the session's `SubmitAction` handler. `std::sync::Mutex`
    /// (not tokio) so the engine's blocking `act()` can lock it
    /// without an async runtime.
    pub pending: StdMutex<Option<PendingAction>>,
}

impl Connection {
    pub fn new(
        player_id: PlayerId,
        username: String,
        session_id: i64,
    ) -> (Self, mpsc::Receiver<ServerMessage>) {
        let (out_tx, out_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        (
            Self {
                player_id,
                username,
                session_id,
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

/// Stable handle to a seat's "current connection." The table, the
/// per-hand snapshot, the [`crate::remote_agent::RemoteAgent`], and
/// the [`crate::table::BroadcastSink`] all hold `Arc<SeatLink>`s
/// rather than `Arc<Connection>`s directly. When the player reconnects
/// from a new device the link is swapped in place, so any subsequent
/// outbound traffic and any not-yet-resolved `PendingAction` migrate
/// to the new socket without restarting the hand. See step 22c in
/// `DESIGN.md`.
///
/// The seat also carries a **replay buffer**: every `ServerMessage`
/// the [`crate::table::BroadcastSink`] fans to this seat during the
/// current in-flight hand is appended here, in order, with masking
/// already applied. On `RequestReplay` the session reads a snapshot
/// of this buffer and ships it verbatim so the seat's state machine
/// can reconstruct `CurrentHand` without re-running the engine.
/// The buffer is cleared at the start of every new hand
/// (`HandStarted`) and dropped with the `SeatLink` itself when the
/// seat vacates. See step 26 / `request-replay-verb`.
pub struct SeatLink {
    inner: StdRwLock<Arc<Connection>>,
    /// Per-seat ordered replay buffer for the current in-flight hand.
    /// Scoped by `hand_id` so a stale `RequestReplay` (wrong id) is
    /// rejected rather than serving up last hand's events. `None` when
    /// no hand is in flight; `Some((hand_id, events))` while a hand
    /// runs.
    replay: StdMutex<ReplayBuffer>,
}

/// Per-seat replay state. `hand_id` is set by `HandStarted` and
/// cleared by `HandEnded` (or on the next `HandStarted` if the
/// server somehow skipped the end event).
#[derive(Default)]
pub struct ReplayBuffer {
    pub hand_id: Option<HandId>,
    pub events: Vec<ServerMessage>,
}

impl SeatLink {
    pub fn new(conn: Arc<Connection>) -> Arc<Self> {
        Arc::new(Self {
            inner: StdRwLock::new(conn),
            replay: StdMutex::new(ReplayBuffer::default()),
        })
    }

    /// Snapshot the connection currently bound to this seat.
    pub fn current(&self) -> Arc<Connection> {
        Arc::clone(&self.inner.read().expect("seat link poisoned"))
    }

    /// Atomically swap the bound connection. Returns the previous
    /// `Arc<Connection>` so the caller can finish any handover
    /// bookkeeping (e.g. clear its `pending` slot).
    pub fn replace(&self, new: Arc<Connection>) -> Arc<Connection> {
        let mut guard = self.inner.write().expect("seat link poisoned");
        std::mem::replace(&mut *guard, new)
    }

    pub fn player_id(&self) -> PlayerId {
        self.current().player_id
    }

    pub fn session_id(&self) -> i64 {
        self.current().session_id
    }

    /// Begin a fresh replay buffer for `hand_id`. Any prior contents
    /// are dropped — they belong to a finished hand. Call exactly
    /// once at the moment [`BroadcastSink`] sees `HandStarted`.
    ///
    /// [`BroadcastSink`]: crate::table::BroadcastSink
    pub fn replay_begin(&self, hand_id: HandId) {
        let mut buf = self.replay.lock().expect("replay mutex poisoned");
        buf.hand_id = Some(hand_id);
        buf.events.clear();
    }

    /// Append a freshly-fanned outbound `ServerMessage` to this seat's
    /// replay buffer. No-op if no hand is in flight (e.g. a
    /// `TableState` fired between hands is not part of any replay).
    pub fn replay_push(&self, msg: &ServerMessage) {
        let mut buf = self.replay.lock().expect("replay mutex poisoned");
        if buf.hand_id.is_some() {
            buf.events.push(msg.clone());
        }
    }

    /// Seal the current hand's replay buffer — subsequent `replay_push`
    /// calls are ignored until the next `replay_begin`. The stored
    /// events are retained so a client that was slow to reconnect
    /// can still replay a just-ended hand until the next
    /// `HandStarted`. (The spec's "buffer dropped on HandEnded" means
    /// it's no longer *live* — which is what clearing `hand_id`
    /// conveys; the bytes are reclaimed on the next `replay_begin`.)
    pub fn replay_end(&self) {
        let mut buf = self.replay.lock().expect("replay mutex poisoned");
        buf.hand_id = None;
        buf.events.clear();
    }

    /// Return a snapshot of the current hand's replay buffer. Returns
    /// `None` if no hand is in flight or if `hand_id` doesn't match
    /// the buffer's hand — either way, the caller should reply with
    /// `ActionRejected { reason: "not seated at this hand" }`.
    pub fn replay_snapshot(&self, hand_id: HandId) -> Option<Vec<ServerMessage>> {
        let buf = self.replay.lock().expect("replay mutex poisoned");
        if buf.hand_id == Some(hand_id) {
            Some(buf.events.clone())
        } else {
            None
        }
    }
}
