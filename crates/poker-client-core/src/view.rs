//! The read-only projection the host renders. Per principle 2 in
//! `CLIENT_PRINCIPLES.md`, the frontend MUST NOT keep a parallel
//! copy of any of this — it derives its UI as a pure function of
//! `ClientView`.
//!
//! `Serialize` + `Deserialize` are derived so the Tauri shell can
//! ship `ClientView` over the invoke / event channel without a
//! mirror type. The on-wire JSON follows serde's default
//! representation for enums (internally tagged for struct variants);
//! frontend types are hand-maintained in `client/src/types.ts`.

use poker_engine::game::{LegalActions, Street};
use poker_engine::net::protocol::{LifetimeStats, PlayerId, SeatInfo, TableId, TableInfo};
use serde::{Deserialize, Serialize};

/// High-level connection / session phase. Richer per-phase state is
/// in [`ClientView`] sibling fields. The presence of
/// [`ClientView::current_hand`] is what distinguishes "seated waiting
/// between hands" from "seated, hand in flight."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// No connection. Initial state, and after `Disconnect` /
    /// `ConnectionLost`.
    Disconnected,
    /// Transport is opening the socket; we have not yet sent the
    /// handshake.
    Connecting,
    /// Handshake in flight (`Register` / `Authenticate` sent, awaiting
    /// `Welcome` or `Rejected`).
    Authenticating,
    /// Authenticated, not at a table.
    Lobby,
    /// At a table.
    Seated { table_id: TableId, seat: usize },
    /// Server sent `Goodbye` or terminal `Rejected`.
    Ended { reason: String },
}

/// The full read-only client state.
///
/// **Stability.** Fields can be added without protocol churn.
/// Removing or renaming fields is a breaking change to the host
/// integration; coordinate with the frontend / Tauri commands.
///
/// **Scope.** Per `CLIENT_PRINCIPLES.md` §3, "current game state"
/// means strictly the in-flight hand. Lifetime stats from
/// `Welcome.stats` ride here for convenience but are surface
/// data only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientView {
    pub phase: Phase,

    /// Set after a successful `Welcome`.
    pub player_id: Option<PlayerId>,
    /// Username as canonicalised by the server.
    pub username: Option<String>,
    /// Cumulative counters from `Welcome.stats`. Surface-only.
    pub lifetime_stats: Option<LifetimeStats>,

    /// Most recent lobby snapshot. Empty until a `TableList` arrives.
    pub tables: Vec<TableInfo>,

    /// Seat layout when seated. Empty in any other phase.
    pub seats: Vec<SeatInfo>,
    /// Dealer-button seat index when seated and known.
    pub button: Option<usize>,

    /// In-flight hand state, present in `Seated` between
    /// `HandStarted` and `HandEnded`. `None` outside of a live hand.
    /// Reconstructed in full from a `RequestReplay` response on
    /// reconnect (see DESIGN.md step 26).
    pub current_hand: Option<CurrentHand>,

    /// Last `ActionRejected` reason since the previous successful
    /// action. Cleared whenever a fresh prompt is issued. UI uses
    /// this to surface "that action wasn't allowed" feedback.
    pub last_action_rejection: Option<String>,
}

/// Per-hand state. Reset at every `HandStarted`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentHand {
    pub hand_id: u64,
    pub dealer: usize,
    /// Furthest street reached so far (Preflop, Flop, Turn, River, or
    /// Showdown). Updated on `BoardDealt`. `Showdown` is set at
    /// `HandEnded` if the hand reached one.
    pub street: Street,
    /// Our hole cards, encoded as raw `Card` bytes. `None` when not
    /// yet dealt or the server has masked them on reconnect.
    pub hole_cards: Option<[u8; 2]>,
    /// Board cards in deal order.
    pub board: Vec<u8>,
    /// Pot total as of the most recent `ActionTaken`.
    pub pot_total: u32,
    /// Seats whose hands are folded this hand. Tracked for UI
    /// dimming / arrow placement.
    pub folded_seats: Vec<usize>,
    /// Seats that have moved all-in this hand.
    pub all_in_seats: Vec<usize>,
    /// True iff there is currently an open `Prompt` addressed to us.
    /// When true, [`Self::legal_actions`] carries the allowed
    /// response set.
    pub awaiting_action: bool,
    /// Server-asserted legal actions for the open prompt. `None`
    /// when `awaiting_action` is false.
    pub legal_actions: Option<LegalActions>,
    /// Server-asserted action deadline in milliseconds, sampled at
    /// the moment the prompt was issued. `None` when not awaiting.
    pub action_deadline_ms: Option<u32>,
    /// Per-seat amount committed this street, reconstructed from
    /// `EngineEvent::ActionTaken` and `EngineEvent::PlayerAllIn`.
    /// Reset on `HandStarted` (via `CurrentHand::started`) and on
    /// `BoardDealt` (new street). This is a UI affordance only; the
    /// authoritative pot total is `pot_total`.
    pub bet_this_street: Vec<(usize, u32)>,
}

impl ClientView {
    pub(crate) fn empty() -> Self {
        Self {
            phase: Phase::Disconnected,
            player_id: None,
            username: None,
            lifetime_stats: None,
            tables: Vec::new(),
            seats: Vec::new(),
            button: None,
            current_hand: None,
            last_action_rejection: None,
        }
    }

    /// Helper: our own seat index when seated, or `None` otherwise.
    pub fn our_seat(&self) -> Option<usize> {
        match self.phase {
            Phase::Seated { seat, .. } => Some(seat),
            _ => None,
        }
    }
}

impl CurrentHand {
    pub(crate) fn started(hand_id: u64, dealer: usize) -> Self {
        Self {
            hand_id,
            dealer,
            street: Street::Preflop,
            hole_cards: None,
            board: Vec::new(),
            pot_total: 0,
            folded_seats: Vec::new(),
            all_in_seats: Vec::new(),
            awaiting_action: false,
            legal_actions: None,
            action_deadline_ms: None,
            bet_this_street: Vec::new(),
        }
    }
}
