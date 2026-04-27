pub mod builtin;
pub mod human;
pub mod personas;

use crate::core::Card;
use crate::game::{Action, EngineEvent, HandId, HandResult, LegalActions, Pot, SeatIndex, Street};

/// What an agent is allowed to see on each decision.
pub struct Observation<'a> {
    pub hole_cards: [Card; 2],
    pub board: &'a [Card],
    pub pot: &'a Pot,
    pub legal_actions: LegalActions,
    pub players: &'a [PublicPlayerState],
    pub street: Street,
    pub position: SeatIndex,
    /// Button position. Combined with `position` this yields a rotation-invariant
    /// relative position (`(position - dealer) mod n`) which is what solvers key on.
    pub dealer: SeatIndex,
}

/// Publicly visible per-player state (no hole cards).
#[derive(Clone, Debug)]
pub struct PublicPlayerState {
    pub seat: SeatIndex,
    pub stack: u32,
    pub bet_this_street: u32,
    pub is_folded: bool,
    pub is_all_in: bool,
}

/// Configuration available to agents at the start of a run.
#[derive(Clone, Debug)]
pub struct RunConfig {
    pub small_blind: u32,
    pub big_blind: u32,
    pub max_players: usize,
}

/// The only contract an AI must satisfy.
///
/// All lifecycle hooks have default no-op implementations.
pub trait Agent: Send {
    fn act(&mut self, obs: &Observation<'_>) -> Action;

    /// Called for every engine event. Useful for observing opponent actions and board cards.
    /// Default is a no-op; only override when you need the event stream.
    fn on_event(&mut self, _event: &EngineEvent) {}

    /// Called once before the first hand. Load durable state (e.g. trained strategy) here.
    fn on_run_start(&mut self, _config: &RunConfig) {}

    /// Called after the last hand. Flush durable state to disk here.
    fn on_run_end(&mut self) {}

    /// Called before each hand. Reset per-hand transient state here.
    fn on_hand_start(&mut self, _hand_id: HandId) {}

    /// Called after each hand with full outcome. Update opponent models here.
    fn on_hand_end(&mut self, _result: &HandResult) {}
}
