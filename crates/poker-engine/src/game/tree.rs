//! Pausable, cloneable hand stepper for solver training.
//!
//! `Engine::run_hand` drives a full hand to completion against fixed agents.
//! Solvers need something different: the ability to stop at a decision point,
//! clone the state, try a candidate action, and see what terminal utility
//! comes out — repeatedly, against many branches. `GameTree` is that.
//!
//! Invariants worth preserving in your head while reading:
//!
//! * The `Deck` lives inside the tree. Cloning the tree clones the deck, so
//!   two branches from the same decision point produce the **same**
//!   subsequent board and hole-card deals. This is what makes CFR's branch
//!   comparisons apples-to-apples.
//! * All chance resolution (board dealing, showdown) happens inside
//!   `apply_action`. Consumers see a pure Decision → Decision → ... →
//!   Terminal sequence with no explicit chance nodes.
//! * The public surface is minimal on purpose: `current`, `observation`,
//!   `legal_actions`, `apply_action`, `utilities`. The internal phase
//!   machine mirrors `Engine::run_betting_round` byte-for-byte; that is how
//!   we stay bug-compatible with the main engine's TDA rule handling.

use smallvec::SmallVec;

use crate::agent::{Observation, PublicPlayerState};
use crate::core::{Deck, HandEvaluator};
use crate::game::action::{Action, LegalActions};
use crate::game::engine::{active_count, next_active_after, Engine};
use crate::game::event::{EngineEvent, EventSink, HandResult};
use crate::game::pot_calc::collect_street_bets;
use crate::game::state::{
    GameState, HandId, PlayerStatus, SeatIndex, SeatKind, Street,
};

/// The kind of node the tree is currently at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// A player is to act. Call `observation_owned` / `legal_actions` to
    /// inspect the decision, then `apply_action` to advance.
    Decision { seat: SeatIndex },
    /// The hand is over. Call `utilities` for per-seat chip deltas.
    Terminal,
}

/// Pausable, cloneable stepper over a single hand.
#[derive(Clone)]
pub struct GameTree {
    pub state: GameState,
    pub deck: Deck,
    phase: Phase,
}

#[derive(Clone)]
enum Phase {
    /// A betting round is in progress. `current` is the seat to act.
    /// `has_responded[i]` is `true` once seat `i` has voluntarily acted since
    /// the last full raise — blind posts do not count. Reset on new street
    /// and on aggression.
    Decision {
        current: SeatIndex,
        has_responded: Vec<bool>,
    },
    /// Hand is over. `chip_deltas[i]` is the raw payout to seat `i` (the
    /// chips awarded from the pot, *not* the net P/L — subtract
    /// `total_committed` to get net).
    Terminal {
        chip_deltas: Vec<i64>,
    },
}

impl GameTree {
    /// Start a new hand. Posts antes and blinds, deals hole cards, marks
    /// dead hands folded, and advances to the first decision (or directly to
    /// terminal when only one seat has chips).
    pub fn new<E: HandEvaluator>(
        engine: &Engine<E>,
        hand_id: HandId,
        deck_seed: u64,
        stacks: &[u32],
        dealer: SeatIndex,
        seat_kinds: Option<&[SeatKind]>,
        sink: &mut dyn EventSink,
    ) -> Self {
        debug_assert!(!stacks.is_empty(), "at least one seat required");
        if let Some(kinds) = seat_kinds {
            debug_assert_eq!(kinds.len(), stacks.len(), "one SeatKind per seat required");
        }

        let mut deck = Deck::new(deck_seed);
        let mut state = engine.init_state(hand_id, stacks, dealer);

        sink.on_event(&EngineEvent::HandStarted { hand_id, dealer, deck_seed });

        if engine.rules.ante > 0 {
            engine.post_antes(&mut state, sink);
        }
        engine.post_blinds(&mut state, sink);

        // Capture canonical BB before dead-hand folding — first-to-act preflop
        // is anchored on the seat that actually posted BB.
        let canonical_bb = engine.bb_seat(&state);

        engine.deal_hole_cards(&mut state, &mut deck, sink);

        if let Some(kinds) = seat_kinds {
            for (seat, kind) in kinds.iter().enumerate() {
                if *kind == SeatKind::DeadHand
                    && state.players[seat].status != PlayerStatus::Out
                {
                    state.players[seat].status = PlayerStatus::Folded;
                    state.sat_out[seat] = true;
                }
            }
        }

        let phase = match next_active_after(canonical_bb, &state.players) {
            None => {
                // Only one player has chips — award uncontested.
                let winner = state
                    .players
                    .iter()
                    .position(|p| {
                        p.status == PlayerStatus::Active || p.status == PlayerStatus::AllIn
                    })
                    .expect("at least one player must still be in");
                let chip_deltas = engine.award_uncontested(&mut state, winner);
                state.action_on = None;
                Phase::Terminal { chip_deltas }
            }
            Some(first) => {
                let n = state.players.len();
                state.action_on = Some(first);
                Phase::Decision { current: first, has_responded: vec![false; n] }
            }
        };

        GameTree { state, deck, phase }
    }

    /// The node currently facing the consumer.
    pub fn current(&self) -> NodeKind {
        match &self.phase {
            Phase::Decision { current, .. } => NodeKind::Decision { seat: *current },
            Phase::Terminal { .. } => NodeKind::Terminal,
        }
    }

    /// Legal actions at the current decision, or `None` at terminal.
    pub fn legal_actions<E: HandEvaluator>(&self, engine: &Engine<E>) -> Option<LegalActions> {
        match &self.phase {
            Phase::Decision { current, has_responded } => {
                let can_reopen = !has_responded[*current];
                Some(engine.compute_legal_actions(&self.state, *current, can_reopen))
            }
            Phase::Terminal { .. } => None,
        }
    }

    /// Apply an action at the current Decision node. Advances the tree
    /// through any chance events (board deals, showdown) until the next
    /// Decision or Terminal.
    ///
    /// Panics if the tree is already at Terminal.
    pub fn apply_action<E: HandEvaluator>(
        &mut self,
        engine: &Engine<E>,
        action: Action,
        sink: &mut dyn EventSink,
    ) {
        let seat = match &self.phase {
            Phase::Decision { current, .. } => *current,
            Phase::Terminal { .. } => panic!("apply_action called at terminal"),
        };

        let normalized = engine.normalize_action(&self.state, seat, action);
        let was_aggression = engine.apply_action(&mut self.state, seat, normalized, sink);

        if let Phase::Decision { has_responded, .. } = &mut self.phase {
            has_responded[seat] = true;
            if was_aggression {
                for (i, r) in has_responded.iter_mut().enumerate() {
                    if i != seat {
                        *r = false;
                    }
                }
            }
        }

        self.advance(engine, sink);
    }

    /// Per-seat chip deltas at terminal. Returns `None` while a decision is
    /// still pending.
    pub fn utilities(&self) -> Option<&[i64]> {
        match &self.phase {
            Phase::Terminal { chip_deltas } => Some(chip_deltas),
            Phase::Decision { .. } => None,
        }
    }

    /// Build the final `HandResult`. Panics unless the tree is at Terminal.
    pub fn into_result(self, hand_id: HandId) -> HandResult {
        let chip_deltas = match self.phase {
            Phase::Terminal { chip_deltas } => chip_deltas,
            Phase::Decision { .. } => panic!("into_result called at decision"),
        };
        crate::game::engine::build_result(hand_id, &self.state, &chip_deltas)
    }

    // -----------------------------------------------------------------------
    // Internal: advance from just-after-action to the next Decision or
    // Terminal. Mirrors `Engine::run_betting_round` + `run_streets`.
    // -----------------------------------------------------------------------

    fn advance<E: HandEvaluator>(&mut self, engine: &Engine<E>, sink: &mut dyn EventSink) {
        // 1. Uncontested win at any step.
        if self.state.players_still_in() == 1 {
            let winner = self
                .state
                .players
                .iter()
                .position(|p| {
                    p.status == PlayerStatus::Active || p.status == PlayerStatus::AllIn
                })
                .expect("players_still_in == 1 implies a winner");
            let chip_deltas = engine.award_uncontested(&mut self.state, winner);
            self.state.action_on = None;
            self.phase = Phase::Terminal { chip_deltas };
            return;
        }

        // 2. Still in the current betting round? Find the next seat to act.
        if let Phase::Decision { current, has_responded } = &mut self.phase {
            let n = self.state.players.len();
            let start = *current;
            let mut found = None;
            for step in 1..=n {
                let seat = (start + step) % n;
                let p = &self.state.players[seat];
                let settled = p.bet_this_street == self.state.current_bet
                    && has_responded[seat];
                if p.status == PlayerStatus::Active && !settled {
                    found = Some(seat);
                    break;
                }
            }
            if let Some(next) = found {
                *current = next;
                self.state.action_on = Some(next);
                return;
            }
            // Round is done — fall through to street advancement.
        }

        // 3. Round over: collect bets and advance through streets until we
        //    find a new betting round or hit showdown.
        collect_street_bets(&mut self.state.players, &mut self.state.pot);
        self.state.action_on = None;

        loop {
            let next = match self.state.street {
                Street::Preflop => Some((Street::Flop, 3usize)),
                Street::Flop => Some((Street::Turn, 1)),
                Street::Turn => Some((Street::River, 1)),
                Street::River | Street::Showdown => None,
            };

            let (street, n_cards) = match next {
                Some(x) => x,
                None => {
                    // Showdown. `deal_remaining_board` fills the board if
                    // it's short (defensive — should already be full).
                    engine.deal_remaining_board(&mut self.state, &mut self.deck, sink);
                    self.state.street = Street::Showdown;
                    let chip_deltas = engine.resolve_showdown(&mut self.state);
                    self.phase = Phase::Terminal { chip_deltas };
                    return;
                }
            };

            engine.begin_street(&mut self.state, street);
            engine.deal_board_cards(&mut self.state, &mut self.deck, n_cards, street, sink);

            if active_count(&self.state.players) >= 2 {
                if let Some(first) = next_active_after(self.state.dealer, &self.state.players) {
                    let n = self.state.players.len();
                    self.phase = Phase::Decision {
                        current: first,
                        has_responded: vec![false; n],
                    };
                    self.state.action_on = Some(first);
                    return;
                }
            }

            // No betting this street — collect (no-op when nobody bet) and
            // continue to the next one.
            collect_street_bets(&mut self.state.players, &mut self.state.pot);
        }
    }
}

/// Owned form of `Observation` used when the borrowed form's lifetime is
/// inconvenient (e.g. passing across a boundary that needs `'static`).
///
/// `GameTree::observation_owned` builds and returns this. Solver trainers
/// that don't need zero-copy should prefer it — it sidesteps the public-
/// state-slice lifetime dance entirely.
pub struct OwnedObservation {
    pub hole_cards: [crate::core::Card; 2],
    pub board: SmallVec<[crate::core::Card; 5]>,
    pub pot: crate::game::state::Pot,
    pub legal_actions: LegalActions,
    pub players: Vec<PublicPlayerState>,
    pub street: Street,
    pub position: SeatIndex,
    pub dealer: SeatIndex,
}

impl OwnedObservation {
    /// Borrow as a standard `Observation<'_>` for trait dispatch.
    pub fn as_observation(&self) -> Observation<'_> {
        Observation {
            hole_cards: self.hole_cards,
            board: &self.board,
            pot: &self.pot,
            legal_actions: self.legal_actions,
            players: &self.players,
            street: self.street,
            position: self.position,
            dealer: self.dealer,
        }
    }
}

impl GameTree {
    /// Owned-observation variant of `observation`. Prefer this for solver
    /// code; the zero-copy `observation` exists for engine-adjacent callers
    /// who already hold the tree by reference.
    pub fn observation_owned<E: HandEvaluator>(
        &self,
        engine: &Engine<E>,
    ) -> Option<OwnedObservation> {
        let Phase::Decision { current, has_responded } = &self.phase else {
            return None;
        };
        let seat = *current;
        let can_reopen = !has_responded[seat];
        let legal = engine.compute_legal_actions(&self.state, seat, can_reopen);
        let players: Vec<PublicPlayerState> = self
            .state
            .players
            .iter()
            .enumerate()
            .map(|(i, p)| PublicPlayerState {
                seat: i,
                stack: p.stack,
                bet_this_street: p.bet_this_street,
                is_folded: p.status == PlayerStatus::Folded,
                is_all_in: p.status == PlayerStatus::AllIn,
            })
            .collect();
        let hole = self.state.players[seat]
            .hole_cards
            .expect("acting seat must hold hole cards");
        Some(OwnedObservation {
            hole_cards: hole,
            board: self.state.board.clone(),
            pot: self.state.pot.clone(),
            legal_actions: legal,
            players,
            street: self.state.street,
            position: seat,
            dealer: self.state.dealer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::NaiveEvaluator;
    use crate::game::{BettingRules, NullSink};

    fn mk_engine() -> Engine<NaiveEvaluator> {
        Engine::new(BettingRules::no_limit_holdem(1, 2, 3), NaiveEvaluator)
    }

    fn play_always_call(tree: &mut GameTree, engine: &Engine<NaiveEvaluator>) {
        let mut sink = NullSink;
        while let NodeKind::Decision { .. } = tree.current() {
            let legal = tree.legal_actions(engine).unwrap();
            let action = if legal.can_check {
                Action::Check
            } else if legal.can_call {
                Action::Call
            } else {
                Action::AllIn
            };
            tree.apply_action(engine, action, &mut sink);
        }
    }

    #[test]
    fn all_call_chip_conservation_three_handed() {
        let engine = mk_engine();
        let stacks = [200u32, 200, 200];
        let mut tree = GameTree::new(&engine, 1, 42, &stacks, 0, None, &mut NullSink);
        play_always_call(&mut tree, &engine);
        let utils = tree.utilities().expect("should be terminal");
        let sum: i64 = utils.iter().sum();
        let committed: i64 = tree
            .state
            .players
            .iter()
            .map(|p| p.total_committed as i64)
            .sum();
        assert_eq!(sum, committed, "awarded chips must match committed chips");
    }

    #[test]
    fn clone_branch_reaches_same_terminal_with_same_actions() {
        let engine = mk_engine();
        let stacks = [200u32, 200, 200];
        let mut a = GameTree::new(&engine, 1, 1337, &stacks, 0, None, &mut NullSink);
        let mut b = a.clone();
        play_always_call(&mut a, &engine);
        play_always_call(&mut b, &engine);
        assert_eq!(a.utilities(), b.utilities());
        assert_eq!(a.state.board, b.state.board);
    }

    #[test]
    fn clone_then_diverge_produces_different_outcomes() {
        // Preflop fold from branch A vs call in branch B must produce
        // different terminal utilities.
        let engine = mk_engine();
        let stacks = [200u32, 200, 200];
        let mut base = GameTree::new(&engine, 1, 7, &stacks, 0, None, &mut NullSink);

        // Advance `base` past one action so all seats have cards.
        // Then clone and have UTG fold vs call.
        let mut a = base.clone();
        let mut b = base.clone();
        let _ = &mut base; // silence unused warning if any
        // Play UTG (seat 0 is dealer; UTG is seat 0 in 3-handed after SB/BB).
        // Whoever is first-to-act acts differently in A vs B.
        a.apply_action(&engine, Action::Fold, &mut NullSink);
        b.apply_action(&engine, Action::Call, &mut NullSink);
        // Finish both with always-call for everyone else.
        play_always_call(&mut a, &engine);
        play_always_call(&mut b, &engine);
        // The folding seat in A committed less than in B.
        let committed_a: u32 = a.state.players.iter().map(|p| p.total_committed).sum();
        let committed_b: u32 = b.state.players.iter().map(|p| p.total_committed).sum();
        assert!(committed_b > committed_a);
    }

    #[test]
    fn fold_out_immediately_terminal() {
        // Three-handed: UTG folds, SB folds — BB wins uncontested at the
        // first opportunity, hand ends without reaching the flop.
        let engine = mk_engine();
        let stacks = [200u32, 200, 200];
        let mut tree = GameTree::new(&engine, 1, 11, &stacks, 0, None, &mut NullSink);
        tree.apply_action(&engine, Action::Fold, &mut NullSink);
        tree.apply_action(&engine, Action::Fold, &mut NullSink);
        assert_eq!(tree.current(), NodeKind::Terminal);
        // Board wasn't dealt — fold-out skips the run-out.
        assert!(tree.state.board.is_empty());
    }
}
