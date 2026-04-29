//! Table state, broadcast sink, and per-table game loop.
//!
//! Each table is an actor: shared seat layout behind a `tokio::sync::Mutex`,
//! a `Notify` that wakes the loop whenever the seat layout changes, and
//! one task per table running hands sequentially. Hands themselves run
//! on tokio's blocking pool so the synchronous [`Engine::run_hand`]
//! call can drive [`RemoteAgent`]s that block on per-player oneshots.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tracing::{debug, info, warn};

use poker_engine::agent::Agent;
use poker_engine::core::{Card, RsPokerEvaluator};
use poker_engine::game::{
    BettingRules, Engine, EngineEvent, EventSink, HandId, HandResult, SeatIndex, SeatOutcome,
};
use poker_engine::net::frame;
use poker_engine::net::protocol::{
    PlayerId, SeatInfo, ServerMessage, TableId, TableInfo,
};

use crate::registry::{HandSeatRecord, Registry};

use crate::connection::{Connection, SeatLink};
use crate::remote_agent::RemoteAgent;

/// Static configuration for a table.
#[derive(Debug, Clone)]
pub struct TableConfig {
    pub name: String,
    pub max_seats: u8,
    pub min_seats: u8,
    pub small_blind: u32,
    pub big_blind: u32,
    pub default_buy_in: u32,
    /// How long a player has to respond to a `Prompt` before the
    /// server folds for them.
    pub action_deadline: Duration,
    /// Brief pause between hands so clients can render `HandEnded`
    /// before the next `HandStarted` arrives.
    pub between_hands: Duration,
}

impl Default for TableConfig {
    fn default() -> Self {
        Self {
            name: "Main".into(),
            max_seats: 2,
            min_seats: 2,
            small_blind: 1,
            big_blind: 2,
            default_buy_in: 200,
            action_deadline: Duration::from_secs(30),
            between_hands: Duration::from_millis(500),
        }
    }
}

/// One occupied seat.
struct Seat {
    player_id: PlayerId,
    username: String,
    stack: u32,
    /// Indirection so a reconnect (step 22c) can swap the bound
    /// connection without rebuilding the seat or the in-flight hand.
    link: Arc<SeatLink>,
    /// True if the player asked to leave; honored at the next hand boundary.
    leave_pending: bool,
}

struct TableInner {
    seats: Vec<Option<Seat>>,
    button: SeatIndex,
    next_hand_id: HandId,
    hand_in_progress: bool,
}

pub struct Table {
    pub id: TableId,
    pub config: TableConfig,
    state: Mutex<TableInner>,
    /// Wakes the actor whenever a seat is taken or vacated, or a
    /// hand finishes.
    notify: Notify,
    /// Signals the actor to exit cleanly.
    shutdown: Notify,
}

impl Table {
    pub fn new(id: TableId, config: TableConfig) -> Arc<Self> {
        let max = config.max_seats as usize;
        let mut seats = Vec::with_capacity(max);
        seats.resize_with(max, || None);
        Arc::new(Self {
            id,
            config,
            state: Mutex::new(TableInner {
                seats,
                button: 0,
                next_hand_id: 1,
                hand_in_progress: false,
            }),
            notify: Notify::new(),
            shutdown: Notify::new(),
        })
    }

    /// Snapshot for the lobby.
    pub async fn info(&self) -> TableInfo {
        let inner = self.state.lock().await;
        TableInfo {
            table_id: self.id,
            name: self.config.name.clone(),
            small_blind: self.config.small_blind,
            big_blind: self.config.big_blind,
            max_seats: self.config.max_seats,
            seated: inner.seats.iter().filter(|s| s.is_some()).count() as u8,
            default_buy_in: self.config.default_buy_in,
        }
    }

    /// Public seat layout, used in `JoinedTable` / `TableState`.
    pub async fn seat_infos(&self) -> Vec<SeatInfo> {
        let inner = self.state.lock().await;
        seat_infos_locked(&inner)
    }

    /// Find an open seat and place the player there. Returns the
    /// seat index, or an error string suitable for `ActionRejected`.
    ///
    /// Does NOT broadcast — the caller owns message ordering so the
    /// joiner sees `JoinedTable` before any subsequent `TableState`.
    pub async fn sit(
        &self,
        conn: Arc<Connection>,
        buy_in: u32,
    ) -> Result<SeatIndex, String> {
        if buy_in < self.config.big_blind * 10 {
            return Err(format!(
                "buy-in must be at least {}",
                self.config.big_blind * 10
            ));
        }
        let mut inner = self.state.lock().await;
        if inner.seats.iter().any(|s| s.as_ref().map_or(false, |s| s.player_id == conn.player_id)) {
            return Err("already seated at this table".into());
        }
        let Some(seat_idx) = inner.seats.iter().position(|s| s.is_none()) else {
            return Err("table full".into());
        };
        inner.seats[seat_idx] = Some(Seat {
            player_id: conn.player_id,
            username: conn.username.clone(),
            stack: buy_in,
            link: SeatLink::new(conn),
            leave_pending: false,
        });
        drop(inner);
        self.notify.notify_one();
        Ok(seat_idx)
    }

    /// Step 22c — splice a fresh connection into an already-seated
    /// player without disturbing an in-flight hand.
    ///
    /// If `new_conn.player_id` matches an existing seat the link is
    /// swapped in place. Any [`PendingAction`] that was waiting on the
    /// previous connection is migrated to `new_conn` and a fresh
    /// `Prompt` is pushed to the new socket so the player can answer
    /// it. Returns `Some(seat_idx)` on success, `None` if the player
    /// is not seated at this table.
    ///
    /// [`PendingAction`]: crate::connection::PendingAction
    pub async fn reconnect_player(&self, new_conn: Arc<Connection>) -> Option<SeatIndex> {
        let inner = self.state.lock().await;
        let mut found: Option<(SeatIndex, Arc<SeatLink>)> = None;
        for (idx, slot) in inner.seats.iter().enumerate() {
            if let Some(seat) = slot {
                if seat.player_id == new_conn.player_id {
                    found = Some((idx, Arc::clone(&seat.link)));
                    break;
                }
            }
        }
        let seats_snapshot = seat_infos_locked(&inner);
        drop(inner);
        let (idx, link) = found?;

        // Tell the new client where they are, mirroring the JoinedTable
        // they would have seen on a fresh `JoinTable`. This goes out
        // before any in-hand frames so the UI knows its seat.
        new_conn.try_send(ServerMessage::JoinedTable {
            table_id: self.id,
            seat: idx,
            seats: seats_snapshot,
        });

        let old = link.current();
        // Lift any in-flight prompt off the old connection so the
        // engine's `block_on` keeps waiting on the same `oneshot` and
        // the new socket can resolve it.
        let migrated = old
            .pending
            .lock()
            .expect("pending mutex poisoned")
            .take();
        // Capture the prompt context before we move the struct, so we
        // can re-issue the matching Prompt over the new socket.
        let reprompt = migrated.as_ref().map(|p| (p.table_id, p.hand_id, p.seat, p.legal));
        link.replace(Arc::clone(&new_conn));
        if let Some(pending) = migrated {
            *new_conn.pending.lock().expect("pending mutex poisoned") = Some(pending);
        }
        if let Some((table_id, hand_id, seat, legal)) = reprompt {
            new_conn.try_send(ServerMessage::Prompt {
                table_id,
                hand_id,
                seat,
                legal,
                deadline_ms: self.config.action_deadline.as_millis() as u32,
            });
        }
        Some(idx)
    }

    /// Push a fresh `TableState` to every seated player except the
    /// one whose `PlayerId` is given (or to everyone, if `None`).
    pub async fn broadcast_table_state(&self, except: Option<PlayerId>) {
        let inner = self.state.lock().await;
        let snapshot = seat_infos_locked(&inner);
        let button = inner.button;
        let links: Vec<_> = inner
            .seats
            .iter()
            .filter_map(|s| s.as_ref())
            .filter(|s| except.map_or(true, |id| s.player_id != id))
            .map(|s| Arc::clone(&s.link))
            .collect();
        drop(inner);
        for link in links {
            link.current().try_send(ServerMessage::TableState {
                table_id: self.id,
                seats: snapshot.clone(),
                button,
            });
        }
    }

    /// Mark the player's seat as leaving. If a hand is in progress
    /// they finish it; otherwise they're removed immediately.
    /// Caller is responsible for any post-leave broadcast.
    pub async fn leave(&self, player_id: PlayerId) -> Result<(), String> {
        let mut inner = self.state.lock().await;
        let seat_idx = inner
            .seats
            .iter()
            .position(|s| s.as_ref().map_or(false, |s| s.player_id == player_id))
            .ok_or_else(|| "not seated at this table".to_string())?;
        if inner.hand_in_progress {
            if let Some(seat) = inner.seats[seat_idx].as_mut() {
                seat.leave_pending = true;
            }
        } else {
            inner.seats[seat_idx] = None;
        }
        drop(inner);
        self.notify.notify_one();
        Ok(())
    }

    /// Drop a connection from any seat it holds. Used when a session
    /// closes without a graceful `LeaveTable`.
    ///
    /// Skips seats whose [`SeatLink`] no longer points at the given
    /// `session_id` — that means a newer session has taken the seat
    /// over (step 22c) and the OLD session's cleanup must not vacate
    /// it. Pass `None` to vacate regardless of the current owner
    /// (e.g. a `LeaveTable` from a still-bound session).
    pub async fn force_leave(&self, player_id: PlayerId, session_id: Option<i64>) {
        let mut inner = self.state.lock().await;
        let mut changed = false;
        let hand_in_progress = inner.hand_in_progress;
        for slot in inner.seats.iter_mut() {
            if let Some(seat) = slot {
                if seat.player_id == player_id {
                    if let Some(sid) = session_id {
                        if seat.link.session_id() != sid {
                            // A newer session already owns this seat.
                            break;
                        }
                    }
                    if hand_in_progress {
                        seat.leave_pending = true;
                    } else {
                        *slot = None;
                    }
                    changed = true;
                    break;
                }
            }
        }
        if changed {
            drop(inner);
            self.notify.notify_one();
            self.broadcast_table_state(None).await;
        }
    }

    pub async fn shutdown(&self) {
        self.shutdown.notify_one();
    }

}

fn seat_infos_locked(inner: &TableInner) -> Vec<SeatInfo> {
    inner
        .seats
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            s.as_ref().map(|s| SeatInfo {
                seat: i,
                player_id: s.player_id,
                username: s.username.clone(),
                stack: s.stack,
            })
        })
        .collect()
}

fn collect_links(inner: &TableInner) -> Vec<Arc<SeatLink>> {
    inner
        .seats
        .iter()
        .filter_map(|s| s.as_ref().map(|s| Arc::clone(&s.link)))
        .collect()
}

// ─── Game loop ───────────────────────────────────────────────────────────────

pub async fn run_table(table: Arc<Table>, rules: BettingRules, registry: Arc<Registry>) {
    info!(table_id = table.id, name = %table.config.name, "table actor started");
    loop {
        // Wait for a hand-startable state.
        let ready = wait_for_quorum(&table).await;
        if !ready {
            info!(table_id = table.id, "table actor shutting down");
            return;
        }

        // Snapshot seats (only seated positions go into the engine).
        let snapshot = take_running_snapshot(&table).await;
        if snapshot.players.len() < table.config.min_seats as usize {
            // Quorum was lost between wake-up and snapshot.
            let mut inner = table.state.lock().await;
            inner.hand_in_progress = false;
            drop(inner);
            continue;
        }

        let hand_id = snapshot.hand_id;
        let deck_seed = derive_seed(table.id, hand_id);
        debug!(
            table_id = table.id,
            hand_id,
            seats = snapshot.players.len(),
            "starting hand",
        );

        let player_ids: Vec<PlayerId> =
            snapshot.players.iter().map(|p| p.link.player_id()).collect();
        let started_at = unix_now();
        let (result, log) = run_one_hand(&table, &rules, snapshot, deck_seed).await;
        let ended_at = unix_now();
        apply_hand_result(&table, &result).await;
        persist_hand(&registry, &table, started_at, ended_at, log, &result, &player_ids)
            .await;

        // Pause briefly so clients can render the result.
        tokio::time::sleep(table.config.between_hands).await;
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn persist_hand(
    registry: &Arc<Registry>,
    table: &Arc<Table>,
    started_at: i64,
    ended_at: i64,
    log: Vec<u8>,
    result: &HandResult,
    player_ids: &[PlayerId],
) {
    // The HandResult's seat indices have been remapped to the table's
    // own seat layout, but the order of `result.seats` still matches
    // the engine-side player list — so the parallel `player_ids` lines
    // up by position.
    let seats: Vec<HandSeatRecord> = result
        .seats
        .iter()
        .enumerate()
        .map(|(engine_seat, outcome)| HandSeatRecord {
            seat: outcome.seat,
            player_id: player_ids.get(engine_seat).copied(),
            chip_delta: outcome.chip_delta,
            sat_out: outcome.sat_out,
        })
        .collect();

    if let Err(e) = registry
        .record_hand(table.id, started_at, ended_at, log, &seats)
        .await
    {
        warn!(
            table_id = table.id,
            hand_id = result.hand_id,
            error = %e,
            "failed to persist hand",
        );
    }
}

/// Block until either we have quorum or the table is shut down.
/// Returns `false` on shutdown.
async fn wait_for_quorum(table: &Arc<Table>) -> bool {
    loop {
        {
            let mut inner = table.state.lock().await;
            let seated = inner.seats.iter().filter(|s| s.is_some()).count();
            if !inner.hand_in_progress && seated >= table.config.min_seats as usize {
                inner.hand_in_progress = true;
                return true;
            }
        }
        tokio::select! {
            _ = table.notify.notified() => continue,
            _ = table.shutdown.notified() => return false,
        }
    }
}

/// Frozen view of the seats taking part in one hand.
struct RunningSnapshot {
    hand_id: HandId,
    /// Subset of seats that are actually playing this hand, in seat
    /// order. Each carries the engine-side seat index it occupies in
    /// the hand (always `0..players.len()`) and a back-link to the
    /// table's seat index for stack updates.
    players: Vec<SnapshotPlayer>,
    /// Engine-side dealer index, in `[0, players.len())`.
    dealer: usize,
}

struct SnapshotPlayer {
    table_seat: SeatIndex,
    stack: u32,
    link: Arc<SeatLink>,
}

async fn take_running_snapshot(table: &Arc<Table>) -> RunningSnapshot {
    let mut inner = table.state.lock().await;
    let hand_id = inner.next_hand_id;
    inner.next_hand_id += 1;

    // Walk seats in order, picking up only the occupied ones.
    let mut players = Vec::new();
    for (idx, slot) in inner.seats.iter().enumerate() {
        if let Some(seat) = slot {
            // A seat with stack < BB cannot post the blind; skip them
            // for this hand but leave them sitting at the table.
            if seat.stack < table.config.big_blind {
                continue;
            }
            players.push(SnapshotPlayer {
                table_seat: idx,
                stack: seat.stack,
                link: Arc::clone(&seat.link),
            });
        }
    }

    // Map the table-side button to an engine-side dealer index. If
    // the current button-seat is short-stacked or vacant this hand,
    // pick the next eligible seat in rotation.
    let table_button = inner.button;
    let dealer = players
        .iter()
        .position(|p| p.table_seat >= table_button)
        .unwrap_or(0);

    RunningSnapshot { hand_id, players, dealer }
}

async fn run_one_hand(
    table: &Arc<Table>,
    rules: &BettingRules,
    snapshot: RunningSnapshot,
    deck_seed: u64,
) -> (HandResult, Vec<u8>) {
    let stacks: Vec<u32> = snapshot.players.iter().map(|p| p.stack).collect();
    let links: Vec<Arc<SeatLink>> = snapshot.players.iter().map(|p| Arc::clone(&p.link)).collect();
    let table_seats: Vec<SeatIndex> = snapshot.players.iter().map(|p| p.table_seat).collect();

    let runtime = Handle::current();
    let table_id = table.id;
    let hand_id = snapshot.hand_id;
    let deadline = table.config.action_deadline;

    let mut agents: Vec<Box<dyn Agent>> = links
        .iter()
        .map(|l| -> Box<dyn Agent> {
            Box::new(RemoteAgent::new(Arc::clone(l), table_id, deadline, runtime.clone()))
        })
        .collect();

    let rules = rules.clone();
    let dealer = snapshot.dealer;
    let stacks_for_blocking = stacks.clone();
    let links_for_blocking = links.clone();

    let outcome = tokio::task::spawn_blocking(move || {
        let engine = Engine::new(rules, RsPokerEvaluator);
        let mut sink = BroadcastSink::new(table_id, links_for_blocking);
        let result = engine.run_hand(
            hand_id,
            deck_seed,
            &stacks_for_blocking,
            dealer,
            None,
            &mut agents,
            &mut sink,
        );
        (result, sink.into_log())
    })
    .await;

    match outcome {
        Ok((r, log)) => (r_with_table_seats(r, &table_seats), log),
        Err(e) => {
            warn!(table_id, hand_id, error = %e, "hand task panicked");
            (HandResult { hand_id, board: vec![], seats: vec![] }, Vec::new())
        }
    }
}

/// Remap engine-seat indices in a `HandResult` to table-seat indices,
/// so downstream stack updates target the right slot.
fn r_with_table_seats(mut r: HandResult, table_seats: &[SeatIndex]) -> HandResult {
    for s in r.seats.iter_mut() {
        if let Some(table_seat) = table_seats.get(s.seat) {
            s.seat = *table_seat;
        }
    }
    r
}

async fn apply_hand_result(table: &Arc<Table>, result: &HandResult) {
    let mut inner = table.state.lock().await;

    // Apply chip deltas to the matching seats.
    for outcome in &result.seats {
        if let Some(slot) = inner.seats.get_mut(outcome.seat) {
            if let Some(seat) = slot {
                seat.stack = (seat.stack as i64 + outcome.chip_delta as i64).max(0) as u32;
            }
        }
    }

    // Honor pending leaves now that the hand has settled, and bust
    // out any seat that ran out of chips.
    for slot in inner.seats.iter_mut() {
        if let Some(seat) = slot {
            if seat.leave_pending || seat.stack == 0 {
                if seat.stack == 0 {
                    info!(table_id = table.id, player_id = seat.player_id, "seat busted");
                }
                *slot = None;
            }
        }
    }

    // Advance button to the next occupied seat.
    inner.button = next_occupied_seat(&inner.seats, inner.button)
        .unwrap_or(inner.button);
    inner.hand_in_progress = false;

    let snapshot = seat_infos_locked(&inner);
    let button = inner.button;
    let links = collect_links(&inner);
    drop(inner);

    for link in links {
        link.current().try_send(ServerMessage::TableState {
            table_id: table.id,
            seats: snapshot.clone(),
            button,
        });
    }
}

fn next_occupied_seat(seats: &[Option<Seat>], from: SeatIndex) -> Option<SeatIndex> {
    let n = seats.len();
    if n == 0 {
        return None;
    }
    for offset in 1..=n {
        let i = (from + offset) % n;
        if seats[i].is_some() {
            return Some(i);
        }
    }
    None
}

fn derive_seed(table_id: TableId, hand_id: HandId) -> u64 {
    let mut x = (table_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= hand_id.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x.wrapping_mul(0x94D0_49BB_1331_11EB)
}

// ─── Broadcast sink with hole-card filtering ─────────────────────────────────

/// Fans engine events out to seated players, filtering hole cards so
/// each player only sees their own — except at showdown, where the
/// reveal rules are: a non-folded seat reveals its cards iff the
/// hand reached the river with two or more contenders still in.
///
/// Also accumulates the unfiltered event stream as a `FileSink`-
/// compatible byte log (`[u32 LE length][rmp-serde bytes]` per event)
/// so the table actor can persist it under `hands.log` and clients can
/// replay it without re-encoding.
pub struct BroadcastSink {
    table_id: TableId,
    /// Engine seat index → seat link. Each link resolves to whichever
    /// connection the seat is currently bound to (handles reconnect).
    /// The vector is parallel to the hand's stacks/agents.
    links: Vec<Arc<SeatLink>>,
    folded: HashSet<SeatIndex>,
    board_cards: usize,
    /// Concatenated `[u32 LE len][msgpack]` frames of every event seen
    /// this hand, ready to drop into `hands.log`.
    log: Vec<u8>,
}

impl BroadcastSink {
    pub fn new(table_id: TableId, links: Vec<Arc<SeatLink>>) -> Self {
        Self {
            table_id,
            links,
            folded: HashSet::new(),
            board_cards: 0,
            log: Vec::new(),
        }
    }

    /// Consume the sink and return the accumulated event log.
    pub fn into_log(self) -> Vec<u8> {
        self.log
    }

    fn record(&mut self, event: &EngineEvent) {
        match frame::encode(event) {
            Ok(framed) => self.log.extend_from_slice(&framed),
            Err(e) => warn!(table_id = self.table_id, error = %e, "log encode failed"),
        }
    }

    fn broadcast_all(&self, event: EngineEvent) {
        for link in &self.links {
            link.current().try_send(ServerMessage::TableEvent {
                table_id: self.table_id,
                event: event.clone(),
            });
        }
    }

    fn send_to(&self, engine_seat: SeatIndex, event: EngineEvent) {
        if let Some(link) = self.links.get(engine_seat) {
            link.current().try_send(ServerMessage::TableEvent {
                table_id: self.table_id,
                event,
            });
        }
    }

    fn filtered_hand_end(&self, recipient: Option<SeatIndex>, result: &HandResult) -> HandResult {
        // Showdown rule: ≥2 non-folded seats AND river dealt.
        let non_folded: Vec<SeatIndex> = (0..self.links.len())
            .filter(|i| !self.folded.contains(i))
            .collect();
        let showdown = self.board_cards >= 5 && non_folded.len() >= 2;

        let mut filtered = result.clone();
        for seat in filtered.seats.iter_mut() {
            let is_self = recipient == Some(seat.seat);
            let reveals = showdown && !self.folded.contains(&seat.seat);
            if !is_self && !reveals {
                seat.hole_cards = None;
            }
        }
        filtered
    }
}

impl EventSink for BroadcastSink {
    fn on_event(&mut self, event: &EngineEvent) {
        // The persisted log carries the unfiltered truth so a replay
        // from `hands.log` matches the engine's own event stream.
        self.record(event);
        match event {
            EngineEvent::HoleCardsDealt { seat, .. } => {
                // Only the owning seat sees its hole cards.
                self.send_to(*seat, event.clone());
            }
            EngineEvent::ActionTaken { seat, action, .. } => {
                if matches!(action, poker_engine::game::Action::Fold) {
                    self.folded.insert(*seat);
                }
                self.broadcast_all(event.clone());
            }
            EngineEvent::BoardDealt { cards, .. } => {
                self.board_cards += cards.len();
                self.broadcast_all(event.clone());
            }
            EngineEvent::HandEnded { hand_id, result } => {
                // Per-recipient filtered result.
                for (engine_seat, link) in self.links.iter().enumerate() {
                    let filtered = self.filtered_hand_end(Some(engine_seat), result);
                    link.current().try_send(ServerMessage::TableEvent {
                        table_id: self.table_id,
                        event: EngineEvent::HandEnded {
                            hand_id: *hand_id,
                            result: filtered,
                        },
                    });
                }
                // Reset for next hand (the sink is single-hand-scoped
                // in practice, but be robust if reused).
                self.folded.clear();
                self.board_cards = 0;
            }
            other => self.broadcast_all(other.clone()),
        }
    }
}

// Suppress unused-import warning when building without certain features.
#[allow(dead_code)]
fn _unused(_: Card, _: SeatOutcome) {}

// ─── Manager ─────────────────────────────────────────────────────────────────

/// Owns all tables. Cheap to clone.
#[derive(Clone)]
pub struct TableManager {
    inner: Arc<RwLock<TableManagerInner>>,
}

struct TableManagerInner {
    tables: HashMap<TableId, Arc<Table>>,
    /// Per-player current-table membership. A player can be sat at
    /// at most one table at a time in 19b.
    player_table: HashMap<PlayerId, TableId>,
}

impl TableManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(TableManagerInner {
                tables: HashMap::new(),
                player_table: HashMap::new(),
            })),
        }
    }

    /// Register an already-constructed table and spawn its actor.
    pub async fn install(&self, table: Arc<Table>, rules: BettingRules, registry: Arc<Registry>) {
        {
            let mut inner = self.inner.write().await;
            inner.tables.insert(table.id, Arc::clone(&table));
        }
        tokio::spawn(run_table(table, rules, registry));
    }

    pub async fn list_infos(&self) -> Vec<TableInfo> {
        let inner = self.inner.read().await;
        let tables: Vec<Arc<Table>> = inner.tables.values().cloned().collect();
        drop(inner);
        let mut out = Vec::with_capacity(tables.len());
        for t in tables {
            out.push(t.info().await);
        }
        out.sort_by_key(|t| t.table_id);
        out
    }

    pub async fn get(&self, table_id: TableId) -> Option<Arc<Table>> {
        let inner = self.inner.read().await;
        inner.tables.get(&table_id).cloned()
    }

    pub async fn current_table(&self, player_id: PlayerId) -> Option<TableId> {
        let inner = self.inner.read().await;
        inner.player_table.get(&player_id).copied()
    }

    pub async fn record_join(&self, player_id: PlayerId, table_id: TableId) {
        let mut inner = self.inner.write().await;
        inner.player_table.insert(player_id, table_id);
    }

    pub async fn record_leave(&self, player_id: PlayerId) -> Option<TableId> {
        let mut inner = self.inner.write().await;
        inner.player_table.remove(&player_id)
    }
}

impl Default for TableManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Send a forced shutdown to every table.
pub async fn shutdown_all(mgr: &TableManager) {
    let inner = mgr.inner.read().await;
    for t in inner.tables.values() {
        t.shutdown().await;
    }
}

// Channel type alias kept in case downstream code wants to subscribe
// to a future event-stream API.
pub type EventChannel = mpsc::UnboundedSender<EngineEvent>;
