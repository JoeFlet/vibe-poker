use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use crate::core::Card;

pub type SeatIndex = usize;
pub type HandId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Street {
    Preflop,
    Flop,
    Turn,
    River,
    Showdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerStatus {
    Active,
    Folded,
    AllIn,
    /// Sitting out / not in this hand.
    Out,
}

/// Per-seat designation for a single hand. Passed to `Engine::run_hand`
/// via `seat_kinds`; defaults to `Live` for every seat when omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeatKind {
    /// Normal participant: dealt in, posts blinds if in SB/BB, may act and win.
    Live,
    /// Dealt cards and posts any owed blind, but the hand is dead from the start —
    /// the seat cannot act or win. Used when an absent player is paying to hold
    /// their seat. The blind functions as a normal (live) blind.
    DeadHand,
}

impl Default for SeatKind {
    fn default() -> Self {
        SeatKind::Live
    }
}

/// Per-seat state for one hand. Cloned cheaply for tree search.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlayerState {
    /// Chips remaining (not yet committed to pot).
    pub stack: u32,
    pub hole_cards: Option<[Card; 2]>,
    /// Chips committed to the pot this street only (reset each street).
    pub bet_this_street: u32,
    /// Cumulative chips committed across all streets this hand. Used for side-pot calculation.
    pub total_committed: u32,
    pub status: PlayerStatus,
}

/// One side pot: the capped amount and which seats are eligible.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SidePot {
    pub amount: u32,
    pub eligible_seats: SmallVec<[SeatIndex; 6]>,
}

/// Full pot state, tracking side pots from the start.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Pot {
    pub main: u32,
    pub side: SmallVec<[SidePot; 4]>,
}

impl Pot {
    pub fn total(&self) -> u32 {
        self.main + self.side.iter().map(|s| s.amount).sum::<u32>()
    }
}

/// Snapshot of the game visible to both engine and agents.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GameState {
    pub hand_id: HandId,
    pub street: Street,
    pub board: SmallVec<[Card; 5]>,
    pub players: Vec<PlayerState>,
    pub pot: Pot,
    /// Index into `players` of the player currently to act. None = hand over.
    pub action_on: Option<SeatIndex>,
    pub dealer: SeatIndex,
    /// Seat of the last aggressor this street (for action-closing logic).
    pub last_aggressor: Option<SeatIndex>,
    /// How much it costs to call right now.
    pub current_bet: u32,
    /// Minimum raise increment (size of the last full valid bet or raise).
    /// Per TDA Rule 47A, this stays pegged to the last *full* raise — short
    /// all-ins never update it.
    pub min_raise_increment: u32,
    /// Sum of sub-minimum all-in increments since the last full raise.
    /// When this crosses `min_raise_increment`, betting reopens for players
    /// who have already acted (Rule 47A, "cumulative multiple short all-ins").
    pub cumulative_short_increment: u32,
    /// Per-seat flag set when the seat's hand is a dead hand (or otherwise
    /// should not be counted toward stats for this hand).
    pub sat_out: Vec<bool>,
}

impl GameState {
    pub fn active_players(&self) -> impl Iterator<Item = (SeatIndex, &PlayerState)> {
        self.players
            .iter()
            .enumerate()
            .filter(|(_, p)| p.status == PlayerStatus::Active || p.status == PlayerStatus::AllIn)
    }

    pub fn players_still_in(&self) -> usize {
        self.players
            .iter()
            .filter(|p| p.status == PlayerStatus::Active || p.status == PlayerStatus::AllIn)
            .count()
    }
}
