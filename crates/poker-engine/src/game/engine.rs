use smallvec::SmallVec;

use crate::agent::{Agent, Observation, PublicPlayerState};
use crate::core::{Card, Deck, HandEvaluator};
use crate::game::action::{Action, LegalActions};
use crate::game::event::{EngineEvent, EventSink, HandResult, SeatOutcome};
use crate::game::pot_calc::{build_showdown_pots, collect_street_bets};
use crate::game::rules::{BetVariant, BettingRules};
use crate::game::state::{
    GameState, HandId, PlayerState, PlayerStatus, Pot, SeatIndex, SeatKind, Street,
};

pub struct Engine<E: HandEvaluator> {
    pub rules: BettingRules,
    pub evaluator: E,
}

impl<E: HandEvaluator> Engine<E> {
    pub fn new(rules: BettingRules, evaluator: E) -> Self {
        Engine { rules, evaluator }
    }

    /// Run one complete hand. Returns a `HandResult` with per-seat chip deltas.
    ///
    /// `stacks`: chip counts per seat going into this hand.
    /// `dealer`: button position (seat index).
    /// `seat_kinds`: per-seat designation; `None` defaults every seat to `Live`.
    /// `agents`: one agent per seat, indexed identically to `stacks`.
    pub fn run_hand(
        &self,
        hand_id: HandId,
        deck_seed: u64,
        stacks: &[u32],
        dealer: SeatIndex,
        seat_kinds: Option<&[SeatKind]>,
        agents: &mut [Box<dyn Agent>],
        sink: &mut dyn EventSink,
    ) -> HandResult {
        debug_assert_eq!(stacks.len(), agents.len(), "one agent per seat required");
        if let Some(kinds) = seat_kinds {
            debug_assert_eq!(kinds.len(), stacks.len(), "one SeatKind per seat required");
        }

        let mut deck = Deck::new(deck_seed);
        let mut state = self.init_state(hand_id, stacks, dealer);

        sink.on_event(&EngineEvent::HandStarted { hand_id, dealer, deck_seed });
        for agent in agents.iter_mut() {
            agent.on_hand_start(hand_id);
        }

        // Post antes, then blinds.
        if self.rules.ante > 0 {
            self.post_antes(&mut state, sink);
        }
        self.post_blinds(&mut state, sink);

        // Capture the canonical BB seat BEFORE dead-hand folding — first-to-act
        // preflop is anchored on the seat that actually posted BB, not on the
        // seat that would be BB after dead hands are removed.
        let canonical_bb = self.bb_seat(&state);

        // Deal two hole cards to every active seat (dead hands included —
        // their cards are dealt but the hand will be marked dead next).
        self.deal_hole_cards(&mut state, &mut deck, sink);

        // Mark dead-hand seats as folded and flag them for stats exclusion.
        // Their blinds (if any) have already been posted as normal blinds.
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

        // --- Preflop betting ---
        // UTG = first active seat after BB; handles heads-up naturally via wrap-around.
        let chip_deltas = match next_active_after(canonical_bb, &state.players) {
            None => {
                // Only one player with chips — award everything immediately.
                self.award_uncontested_and_finish(&mut state, sink, agents)
            }
            Some(first) => {
                self.run_streets(&mut state, &mut deck, first, agents, sink)
            }
        };

        let result = build_result(hand_id, &state, &chip_deltas);
        sink.on_event(&EngineEvent::HandEnded { hand_id, result: result.clone() });
        for agent in agents.iter_mut() {
            agent.on_hand_end(&result);
        }
        result
    }

    // -----------------------------------------------------------------------
    // Initialisation
    // -----------------------------------------------------------------------

    pub(crate) fn init_state(&self, hand_id: HandId, stacks: &[u32], dealer: SeatIndex) -> GameState {
        let n = stacks.len();
        GameState {
            hand_id,
            street: Street::Preflop,
            board: SmallVec::new(),
            players: stacks
                .iter()
                .map(|&stack| PlayerState {
                    stack,
                    hole_cards: None,
                    bet_this_street: 0,
                    total_committed: 0,
                    status: if stack > 0 {
                        PlayerStatus::Active
                    } else {
                        PlayerStatus::Out
                    },
                })
                .collect(),
            pot: Pot::default(),
            action_on: None,
            dealer,
            last_aggressor: None,
            current_bet: 0,
            min_raise_increment: self.rules.big_blind,
            cumulative_short_increment: 0,
            sat_out: vec![false; n],
        }
    }

    pub(crate) fn post_antes(&self, state: &mut GameState, sink: &mut dyn EventSink) {
        let ante = self.rules.ante;
        let n = state.players.len();
        for seat in 0..n {
            let p = &mut state.players[seat];
            if p.status == PlayerStatus::Active {
                let posted = ante.min(p.stack);
                p.stack -= posted;
                p.bet_this_street += posted;
                p.total_committed += posted;
                if p.stack == 0 {
                    p.status = PlayerStatus::AllIn;
                    sink.on_event(&EngineEvent::PlayerAllIn { seat, total_committed: p.total_committed });
                }
            }
        }
    }

    pub(crate) fn post_blinds(&self, state: &mut GameState, sink: &mut dyn EventSink) {
        let sb_seat = self.sb_seat(state);
        let bb_seat = self.bb_seat(state);

        let post = |state: &mut GameState, seat: SeatIndex, amount: u32, sink: &mut dyn EventSink| {
            let p = &mut state.players[seat];
            let actual = amount.min(p.stack);
            p.stack -= actual;
            p.bet_this_street += actual;
            p.total_committed += actual;
            if p.stack == 0 {
                p.status = PlayerStatus::AllIn;
                sink.on_event(&EngineEvent::PlayerAllIn { seat, total_committed: p.total_committed });
            }
        };

        post(state, sb_seat, self.rules.small_blind, sink);
        post(state, bb_seat, self.rules.big_blind, sink);

        state.current_bet = self.rules.big_blind;
        state.min_raise_increment = self.rules.big_blind;
    }

    pub(crate) fn deal_hole_cards(&self, state: &mut GameState, deck: &mut Deck, sink: &mut dyn EventSink) {
        let n = state.players.len();
        for seat in 0..n {
            if state.players[seat].status == PlayerStatus::Active
                || state.players[seat].status == PlayerStatus::AllIn
            {
                let cards = [deck.deal(), deck.deal()];
                state.players[seat].hole_cards = Some(cards);
                sink.on_event(&EngineEvent::HoleCardsDealt { seat, cards });
            }
        }
    }

    // -----------------------------------------------------------------------
    // Street sequencing
    // -----------------------------------------------------------------------

    fn run_streets(
        &self,
        state: &mut GameState,
        deck: &mut Deck,
        first_preflop: SeatIndex,
        agents: &mut [Box<dyn Agent>],
        sink: &mut dyn EventSink,
    ) -> Vec<i64> {
        // Preflop
        if let Some(winner) = self.run_betting_round(state, first_preflop, agents, sink) {
            return self.award_uncontested(state, winner);
        }
        collect_street_bets(&mut state.players, &mut state.pot);

        // Flop / Turn / River
        let postflop = [(Street::Flop, 3usize), (Street::Turn, 1), (Street::River, 1)];
        for (street, n_cards) in postflop {
            if state.players_still_in() <= 1 {
                break; // safety — shouldn't happen here
            }

            self.begin_street(state, street);
            self.deal_board_cards(state, deck, n_cards, street, sink);

            // Broadcast board to all agents (enables display for human players).
            let board_event = EngineEvent::BoardDealt {
                street,
                cards: state.board[state.board.len() - n_cards..].to_vec(),
            };
            for agent in agents.iter_mut() {
                agent.on_event(&board_event);
            }

            // Only run betting if 2+ players can still act.
            if active_count(&state.players) >= 2 {
                let first = next_active_after(state.dealer, &state.players);
                if let Some(first) = first {
                    if let Some(winner) = self.run_betting_round(state, first, agents, sink) {
                        return self.award_uncontested(state, winner);
                    }
                }
            }
            collect_street_bets(&mut state.players, &mut state.pot);
        }

        // Deal any remaining board cards (run-out when all-in before river).
        self.deal_remaining_board(state, deck, sink);

        self.resolve_showdown(state)
    }

    pub(crate) fn begin_street(&self, state: &mut GameState, street: Street) {
        state.street = street;
        state.current_bet = 0;
        state.min_raise_increment = self.rules.big_blind;
        state.cumulative_short_increment = 0;
        state.last_aggressor = None;
        for p in state.players.iter_mut() {
            p.bet_this_street = 0;
        }
    }

    pub(crate) fn deal_board_cards(
        &self,
        state: &mut GameState,
        deck: &mut Deck,
        n: usize,
        street: Street,
        sink: &mut dyn EventSink,
    ) {
        let cards: Vec<Card> = (0..n).map(|_| deck.deal()).collect();
        state.board.extend_from_slice(&cards);
        sink.on_event(&EngineEvent::BoardDealt { street, cards });
    }

    pub(crate) fn deal_remaining_board(&self, state: &mut GameState, deck: &mut Deck, sink: &mut dyn EventSink) {
        let remaining = 5usize.saturating_sub(state.board.len());
        if remaining == 0 {
            return;
        }
        let streets = [Street::Flop, Street::Turn, Street::River];
        let n_per = [3usize, 1, 1];
        let mut dealt = state.board.len();
        for (&street, &n) in streets.iter().zip(n_per.iter()) {
            if dealt >= 5 {
                break;
            }
            if dealt < n + (5 - n_per.iter().rev().take(3 - streets.iter().position(|&s| s == street).unwrap()).sum::<usize>()) {
                let to_deal = n.min(5 - dealt);
                let cards: Vec<Card> = (0..to_deal).map(|_| deck.deal()).collect();
                state.board.extend_from_slice(&cards);
                sink.on_event(&EngineEvent::BoardDealt { street, cards });
                dealt += to_deal;
            }
        }
        // Simpler fallback: just fill to 5 without emitting per-street events.
        while state.board.len() < 5 {
            state.board.push(deck.deal());
        }
    }

    // -----------------------------------------------------------------------
    // Betting round
    // -----------------------------------------------------------------------

    /// Run one street's betting round.
    /// Returns `Some(winner)` if everyone else folded; `None` if the street ended normally.
    fn run_betting_round(
        &self,
        state: &mut GameState,
        first_to_act: SeatIndex,
        agents: &mut [Box<dyn Agent>],
        sink: &mut dyn EventSink,
    ) -> Option<SeatIndex> {
        let n = state.players.len();
        // has_responded[i]: true once player i has voluntarily acted since the last full raise.
        // Blind posts don't count — this ensures BB gets a raise option in an unraised pot.
        // Reset to false for all other players when a full raise occurs.
        let mut has_responded: Vec<bool> = vec![false; n];
        let mut current = first_to_act;

        loop {
            // Break when every active player has both matched current_bet AND responded.
            // The two-part check handles BB option: BB has bet_this_street == current_bet
            // via the forced blind, but has_responded=false keeps them in until they act.
            let all_done = (0..n).all(|i| {
                let p = &state.players[i];
                p.status != PlayerStatus::Active
                    || (p.bet_this_street == state.current_bet && has_responded[i])
            });
            if all_done {
                break;
            }

            // Skip players who are already settled for this round.
            {
                let p = &state.players[current];
                if p.status != PlayerStatus::Active
                    || (p.bet_this_street == state.current_bet && has_responded[current])
                {
                    current = (current + 1) % n;
                    continue;
                }
            }

            state.action_on = Some(current);

            // A player can re-raise only if they haven't responded since the last full raise.
            // If a partial all-in bumped current_bet, has_responded stays true for previously-
            // acted players → can_reopen=false → they must call the extra but cannot raise.
            let can_reopen = !has_responded[current];
            let legal = self.compute_legal_actions(state, current, can_reopen);

            let public: Vec<PublicPlayerState> = state
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
            let hole = state.players[current].hole_cards.unwrap();
            let obs = Observation {
                hole_cards: hole,
                board: &state.board,
                pot: &state.pot,
                legal_actions: legal,
                players: &public,
                street: state.street,
                position: current,
                dealer: state.dealer,
            };

            let raw_action = agents[current].act(&obs);
            let action = self.normalize_action(state, current, raw_action);
            let was_aggression = self.apply_action(state, current, action, sink);

            // Broadcast the action to all agents (enables opponent-action display).
            let pot_total = state.pot.total()
                + state.players.iter().map(|p| p.bet_this_street).sum::<u32>();
            let action_event = EngineEvent::ActionTaken { seat: current, action, pot_total };
            for agent in agents.iter_mut() {
                agent.on_event(&action_event);
            }

            has_responded[current] = true;
            if was_aggression {
                // Full raise: everyone else must respond again (can re-raise).
                for i in 0..n {
                    if i != current {
                        has_responded[i] = false;
                    }
                }
            }

            // Check for immediate winner (everyone else folded).
            if state.players_still_in() == 1 {
                let winner = state
                    .players
                    .iter()
                    .position(|p| {
                        p.status == PlayerStatus::Active || p.status == PlayerStatus::AllIn
                    })
                    .unwrap();
                return Some(winner);
            }

            current = (current + 1) % n;
        }

        state.action_on = None;
        None
    }

    // -----------------------------------------------------------------------
    // Action normalization (TDA Rule 43)
    // -----------------------------------------------------------------------

    /// Rewrite an agent-submitted action into a rules-legal one before dispatch.
    ///
    /// * A `Raise` that would consume the player's entire stack is rewritten
    ///   as `AllIn` so short-all-in reopening semantics (Rule 47A) apply.
    /// * A `Raise` below the minimum raise that would *not* exhaust the stack
    ///   is illegal under TDA — there is no "short" non-all-in raise. It is
    ///   downgraded to `Call` (or `Check` if nothing is owed). A debug
    ///   assertion fires because well-behaved agents must consult
    ///   `LegalActions::min_raise` and never submit such a raise.
    pub(crate) fn normalize_action(
        &self,
        state: &GameState,
        seat: SeatIndex,
        action: Action,
    ) -> Action {
        if let Action::Raise(amount) = action {
            let p = &state.players[seat];
            let additional = amount.saturating_sub(p.bet_this_street);
            if additional >= p.stack {
                return Action::AllIn;
            }
            let min_raise_total = state.current_bet + state.min_raise_increment;
            if amount < min_raise_total {
                debug_assert!(
                    false,
                    "short non-all-in raise: amount={amount} min_raise_total={min_raise_total} (agent bug)"
                );
                let owed = state.current_bet.saturating_sub(p.bet_this_street);
                return if owed == 0 { Action::Check } else { Action::Call };
            }
        }
        action
    }

    // -----------------------------------------------------------------------
    // Legal action computation
    // -----------------------------------------------------------------------

    pub(crate) fn compute_legal_actions(
        &self,
        state: &GameState,
        seat: SeatIndex,
        can_reopen: bool,
    ) -> LegalActions {
        let p = &state.players[seat];
        let owed = state.current_bet.saturating_sub(p.bet_this_street);
        let can_check = owed == 0;
        let can_call = owed > 0 && p.stack > owed;
        let call_amount = owed.min(p.stack);
        let all_in_amount = p.stack;

        let min_raise_total = state.current_bet + state.min_raise_increment;
        let total_if_raised_min = min_raise_total;

        // can_reopen=false when a partial all-in bumped current_bet without constituting
        // a full raise — previously-acted players must call the extra but cannot re-raise.
        let can_raise = can_reopen && match self.rules.variant {
            BetVariant::NoLimit | BetVariant::PotLimit => {
                p.stack + p.bet_this_street > min_raise_total
            }
            BetVariant::FixedLimit => p.stack + p.bet_this_street >= min_raise_total,
        };

        let max_raise = match self.rules.variant {
            BetVariant::NoLimit => p.stack + p.bet_this_street,
            BetVariant::PotLimit => {
                // Max raise = call + pot after call.
                let pot_after_call = state.pot.total() + owed;
                let pot_raise = owed + pot_after_call + pot_after_call; // call + (call + pot)
                (p.stack + p.bet_this_street).min(pot_raise)
            }
            BetVariant::FixedLimit => total_if_raised_min,
        };

        LegalActions {
            can_check,
            can_call,
            call_amount,
            can_raise,
            min_raise: min_raise_total,
            max_raise,
            all_in_amount,
        }
    }

    // -----------------------------------------------------------------------
    // Action application
    // -----------------------------------------------------------------------

    /// Apply a validated action to `state`. Returns `true` if the action constitutes
    /// a raise (i.e., betting action should reset `has_acted` for other players).
    pub(crate) fn apply_action(
        &self,
        state: &mut GameState,
        seat: SeatIndex,
        action: Action,
        sink: &mut dyn EventSink,
    ) -> bool {
        let mut was_aggression = false;

        match action {
            Action::Fold => {
                state.players[seat].status = PlayerStatus::Folded;
            }

            Action::Check => {}

            Action::Call => {
                let p = &mut state.players[seat];
                let owed = state.current_bet.saturating_sub(p.bet_this_street);
                let paid = owed.min(p.stack);
                p.stack -= paid;
                p.bet_this_street += paid;
                p.total_committed += paid;
                if p.stack == 0 {
                    p.status = PlayerStatus::AllIn;
                    sink.on_event(&EngineEvent::PlayerAllIn {
                        seat,
                        total_committed: p.total_committed,
                    });
                }
            }

            Action::Raise(amount) => {
                // Normalized raises are full raises that do not exhaust the stack —
                // all-in raises are routed through `Action::AllIn` by `normalize_action`.
                let p = &mut state.players[seat];
                let prev_bet = p.bet_this_street;
                let additional = amount.saturating_sub(prev_bet);
                let paid = additional.min(p.stack);
                p.stack -= paid;
                p.bet_this_street += paid;
                p.total_committed += paid;
                let new_total = prev_bet + paid;

                debug_assert!(
                    new_total > state.current_bet,
                    "normalized Raise must increase the current bet"
                );
                debug_assert!(
                    p.stack > 0,
                    "all-in raises should have been normalized to Action::AllIn"
                );

                state.min_raise_increment = new_total - state.current_bet;
                state.cumulative_short_increment = 0;
                state.current_bet = new_total;
                state.last_aggressor = Some(seat);
                was_aggression = true;
            }

            Action::AllIn => {
                let p = &mut state.players[seat];
                let all_in_total = p.bet_this_street + p.stack;
                p.total_committed += p.stack;
                p.bet_this_street = all_in_total;
                p.stack = 0;
                p.status = PlayerStatus::AllIn;

                if all_in_total > state.current_bet {
                    let increment = all_in_total - state.current_bet;
                    state.current_bet = all_in_total;
                    state.last_aggressor = Some(seat);

                    if increment >= state.min_raise_increment {
                        // Full raise via all-in: pin the new min-raise increment,
                        // reset cumulative-short tracker, and reopen action.
                        state.min_raise_increment = increment;
                        state.cumulative_short_increment = 0;
                        was_aggression = true;
                    } else {
                        // Short all-in: `min_raise_increment` stays pinned to the last
                        // full valid raise (Rule 47A). Accumulate the increment — once
                        // cumulative shorts total a full raise, betting reopens.
                        state.cumulative_short_increment =
                            state.cumulative_short_increment.saturating_add(increment);
                        if state.cumulative_short_increment >= state.min_raise_increment {
                            was_aggression = true;
                        }
                    }
                }
                sink.on_event(&EngineEvent::PlayerAllIn {
                    seat,
                    total_committed: state.players[seat].total_committed,
                });
            }
        }

        sink.on_event(&EngineEvent::ActionTaken {
            seat,
            action,
            pot_total: state.pot.total() + state.players.iter().map(|p| p.bet_this_street).sum::<u32>(),
        });

        was_aggression
    }

    // -----------------------------------------------------------------------
    // Showdown & awards
    // -----------------------------------------------------------------------

    pub(crate) fn resolve_showdown(&self, state: &mut GameState) -> Vec<i64> {
        let board: [Card; 5] = state.board[..].try_into().expect("board must be 5 cards at showdown");
        let pots = build_showdown_pots(&state.players);
        let n = state.players.len();
        let mut deltas: Vec<i64> = vec![0; n];

        for (amount, eligible) in &pots {
            if eligible.is_empty() || *amount == 0 {
                continue;
            }
            if eligible.len() == 1 {
                deltas[eligible[0]] += *amount as i64;
                continue;
            }

            // Evaluate best hand for each eligible player.
            let ranks: Vec<(SeatIndex, crate::core::HandRank)> = eligible
                .iter()
                .map(|&seat| {
                    let hole = state.players[seat].hole_cards.expect("non-folded player has hole cards");
                    let seven = [hole[0], hole[1], board[0], board[1], board[2], board[3], board[4]];
                    (seat, self.evaluator.rank_7(seven))
                })
                .collect();

            let best = ranks.iter().map(|(_, r)| *r).max().unwrap();
            let winners: Vec<SeatIndex> = ranks
                .iter()
                .filter(|(_, r)| *r == best)
                .map(|(s, _)| *s)
                .collect();

            let share = *amount / winners.len() as u32;
            let remainder = *amount % winners.len() as u32;
            for &w in &winners {
                deltas[w] += share as i64;
            }
            // Remainder chip goes to earliest winner left of dealer.
            if remainder > 0 {
                for i in 0..n {
                    let seat = (state.dealer + 1 + i) % n;
                    if winners.contains(&seat) {
                        deltas[seat] += remainder as i64;
                        break;
                    }
                }
            }
        }

        // Apply deltas to stacks.
        for (i, p) in state.players.iter_mut().enumerate() {
            p.stack = (p.stack as i64 + deltas[i]) as u32;
        }

        deltas
    }

    pub(crate) fn award_uncontested(&self, state: &mut GameState, winner: SeatIndex) -> Vec<i64> {
        collect_street_bets(&mut state.players, &mut state.pot);
        let total = state.pot.total();
        let n = state.players.len();
        let mut deltas = vec![0i64; n];
        deltas[winner] = total as i64;
        state.players[winner].stack += total;
        state.pot.main = 0;
        deltas
    }

    fn award_uncontested_and_finish(
        &self,
        state: &mut GameState,
        _sink: &mut dyn EventSink,
        _agents: &mut [Box<dyn Agent>],
    ) -> Vec<i64> {
        let winner = state
            .players
            .iter()
            .position(|p| p.status == PlayerStatus::Active || p.status == PlayerStatus::AllIn)
            .unwrap();
        self.award_uncontested(state, winner)
    }

    // -----------------------------------------------------------------------
    // Seat helpers
    // -----------------------------------------------------------------------

    pub(crate) fn sb_seat(&self, state: &GameState) -> SeatIndex {
        let n = state.players.len();
        // In heads-up, the button/dealer is the SB.
        next_active_after_in_list(state.dealer, &state.players, n)
            .unwrap_or(state.dealer)
    }

    pub(crate) fn bb_seat(&self, state: &GameState) -> SeatIndex {
        let sb = self.sb_seat(state);
        next_active_after_in_list(sb, &state.players, state.players.len())
            .unwrap_or(sb)
    }
}

// -----------------------------------------------------------------------
// Free helpers
// -----------------------------------------------------------------------

/// First Active seat strictly after `from`, wrapping around.
pub(crate) fn next_active_after(from: SeatIndex, players: &[PlayerState]) -> Option<SeatIndex> {
    let n = players.len();
    for i in 1..=n {
        let seat = (from + i) % n;
        if players[seat].status == PlayerStatus::Active {
            return Some(seat);
        }
    }
    None
}

fn next_active_after_in_list(
    from: SeatIndex,
    players: &[PlayerState],
    limit: usize,
) -> Option<SeatIndex> {
    let n = players.len();
    for i in 1..=limit {
        let seat = (from + i) % n;
        if players[seat].status == PlayerStatus::Active
            || players[seat].status == PlayerStatus::AllIn
        {
            return Some(seat);
        }
    }
    None
}

pub(crate) fn active_count(players: &[PlayerState]) -> usize {
    players.iter().filter(|p| p.status == PlayerStatus::Active).count()
}

pub(crate) fn build_result(hand_id: HandId, state: &GameState, deltas: &[i64]) -> HandResult {
    let seats = state
        .players
        .iter()
        .enumerate()
        .filter(|(_, p)| p.status != PlayerStatus::Out)
        .map(|(i, p)| SeatOutcome {
            seat: i,
            hole_cards: p.hole_cards,
            chip_delta: deltas[i] as i32 - p.total_committed as i32,
            sat_out: state.sat_out.get(i).copied().unwrap_or(false),
        })
        .collect();

    HandResult {
        hand_id,
        board: state.board.to_vec(),
        seats,
    }
}
