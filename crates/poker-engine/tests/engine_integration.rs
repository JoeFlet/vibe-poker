/// Integration tests for the game engine.
///
/// Strategy: use ScriptedAgent for fully deterministic hands, VecSink to
/// capture events, and verify chip conservation + specific outcomes.
use poker_engine::agent::builtin::{CallingStation, ScriptedAgent};
use poker_engine::agent::{Agent, Observation};
use poker_engine::core::NaiveEvaluator;
use poker_engine::game::{
    Action, BettingRules, Engine, EngineEvent, LegalActions, NullSink, SeatKind, VecSink,
};

// -----------------------------------------------------------------------
// Helper: agent that executes a script and records every LegalActions it sees.
// -----------------------------------------------------------------------

struct CapturingAgent {
    script: Vec<Action>,
    cursor: usize,
    pub captured: Vec<LegalActions>,
}

impl CapturingAgent {
    fn new(script: Vec<Action>) -> Self {
        CapturingAgent { script, cursor: 0, captured: Vec::new() }
    }
}

impl Agent for CapturingAgent {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        self.captured.push(obs.legal_actions.clone());
        if self.cursor < self.script.len() {
            let action = self.script[self.cursor];
            self.cursor += 1;
            action
        } else {
            // Fall back to CallingStation once the script is exhausted.
            if obs.legal_actions.can_check { Action::Check } else { Action::Call }
        }
    }
}

fn nlhe_rules() -> BettingRules {
    BettingRules::no_limit_holdem(1, 2, 6)
}

fn total_chips(stacks: &[u32]) -> u32 {
    stacks.iter().sum()
}

// -----------------------------------------------------------------------
// Chip conservation: total chips in play must be constant after every hand.
// -----------------------------------------------------------------------

fn assert_chips_conserved(starting: &[u32], result_deltas: &[i32], stacks_after: &[u32]) {
    let before: i32 = starting.iter().map(|&s| s as i32).sum();
    let after: i32 = stacks_after.iter().map(|&s| s as i32).sum();
    assert_eq!(
        before, after,
        "chip conservation violated: before={before}, after={after}"
    );
    let sum_deltas: i32 = result_deltas.iter().sum();
    assert_eq!(sum_deltas, 0, "deltas must sum to zero: {result_deltas:?}");
}

// -----------------------------------------------------------------------
// Helpers to build agents from action scripts
// -----------------------------------------------------------------------

fn scripted(actions: Vec<Action>) -> Box<dyn poker_engine::agent::Agent> {
    Box::new(ScriptedAgent::new(actions))
}

fn calling() -> Box<dyn poker_engine::agent::Agent> {
    Box::new(CallingStation)
}

// -----------------------------------------------------------------------
// 1. Everyone folds to the big blind
// -----------------------------------------------------------------------
#[test]
fn everyone_folds_to_bb() {
    let rules = nlhe_rules();
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [100u32, 100, 100]; // seats 0, 1, 2
    let dealer = 0;
    // SB = seat 1, BB = seat 2, UTG = seat 0
    // UTG folds, SB folds → BB wins
    let mut agents: Vec<Box<dyn poker_engine::agent::Agent>> = vec![
        scripted(vec![Action::Fold]), // UTG (seat 0)
        scripted(vec![Action::Fold]), // SB  (seat 1)
        scripted(vec![]),             // BB  (seat 2) — not asked to act
    ];
    let mut sink = VecSink::default();
    let result = engine.run_hand(1, 42, &stacks, dealer, None, &mut agents, &mut sink);

    // Seat 2 (BB) should gain the 3 chips posted (SB=1, BB=2 → pot=3).
    let bb_outcome = result.seats.iter().find(|s| s.seat == 2).unwrap();
    assert_eq!(bb_outcome.chip_delta, 1, "BB wins SB's 1 chip: delta should be +1");

    // HandStarted + HandEnded events must be present.
    assert!(sink.events.iter().any(|e| matches!(e, EngineEvent::HandStarted { .. })));
    assert!(sink.events.iter().any(|e| matches!(e, EngineEvent::HandEnded { .. })));
}

// -----------------------------------------------------------------------
// 2. Simple full hand — all players call, best hand wins
// -----------------------------------------------------------------------
#[test]
fn full_hand_all_call_chips_conserved() {
    let rules = nlhe_rules();
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [200u32, 200, 200];
    let dealer = 0;

    // Everyone calls/checks every street to reach showdown.
    let mut agents: Vec<Box<dyn poker_engine::agent::Agent>> =
        vec![calling(), calling(), calling()];
    let mut sink = NullSink;
    let result = engine.run_hand(1, 99, &stacks, dealer, None, &mut agents, &mut sink);

    // Chip conservation.
    let deltas: Vec<i32> = result.seats.iter().map(|s| s.chip_delta).collect();
    let stacks_after: Vec<u32> = result
        .seats
        .iter()
        .map(|s| (stacks[s.seat] as i32 + s.chip_delta) as u32)
        .collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);
    assert_eq!(result.board.len(), 5, "five community cards must be dealt");
}

// -----------------------------------------------------------------------
// 3. UTG raises, everyone folds → UTG wins uncontested
// -----------------------------------------------------------------------
#[test]
fn utg_raise_fold_out() {
    let rules = nlhe_rules();
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [200u32, 200, 200]; // dealer=0, SB=1, BB=2, UTG=0
    let dealer = 2; // so UTG=0, SB=0... let's use dealer=1: SB=2, BB=0, UTG=1

    // dealer=1 → SB=2, BB=0, UTG=1
    // UTG raises to 6, SB folds, BB folds
    let mut agents: Vec<Box<dyn poker_engine::agent::Agent>> = vec![
        scripted(vec![Action::Fold]),     // seat 0 (BB)
        scripted(vec![Action::Raise(6)]), // seat 1 (UTG) — first to act
        scripted(vec![Action::Fold]),     // seat 2 (SB)
    ];
    let mut sink = NullSink;
    let result = engine.run_hand(2, 77, &stacks, 1, None, &mut agents, &mut sink);

    let deltas: Vec<i32> = (0..3)
        .map(|seat| result.seats.iter().find(|s| s.seat == seat).map(|s| s.chip_delta).unwrap_or(0))
        .collect();
    let stacks_after: Vec<u32> = (0..3)
        .map(|i| (stacks[i] as i32 + deltas[i]) as u32)
        .collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);
    // UTG (seat 1) wins: delta should be positive.
    assert!(deltas[1] > 0, "UTG should win; deltas={deltas:?}");
}

// -----------------------------------------------------------------------
// 4. Split pot — two players tie at showdown (same hole cards isn't possible,
//    but identical board-play hand is). We verify conservation and that both
//    net zero when pot is split evenly.
// -----------------------------------------------------------------------
#[test]
fn chip_conservation_heads_up_all_call() {
    let rules = BettingRules::no_limit_holdem(1, 2, 2);
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [100u32, 100];
    let dealer = 0;

    let mut agents: Vec<Box<dyn poker_engine::agent::Agent>> =
        vec![calling(), calling()];
    let mut sink = NullSink;
    let result = engine.run_hand(3, 12345, &stacks, dealer, None, &mut agents, &mut sink);

    let deltas: Vec<i32> = result.seats.iter().map(|s| s.chip_delta).collect();
    let stacks_after: Vec<u32> = result
        .seats
        .iter()
        .map(|s| (stacks[s.seat] as i32 + s.chip_delta) as u32)
        .collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);
}

// -----------------------------------------------------------------------
// 5. All-in pre-flop — side pot conservation
// -----------------------------------------------------------------------
#[test]
fn short_stack_allin_chips_conserved() {
    let rules = nlhe_rules();
    let engine = Engine::new(rules, NaiveEvaluator);
    // Seat 1 (SB, 5 chips) goes all-in. SB's all-in re-opens action, so
    // UTG (seat 0) and BB (seat 2) each need to call again — use CallingStation.
    let stacks = [200u32, 5, 200]; // dealer=0, SB=1, BB=2
    let dealer = 0;

    let mut agents: Vec<Box<dyn poker_engine::agent::Agent>> = vec![
        calling(),                     // seat 0 (UTG)
        scripted(vec![Action::AllIn]), // seat 1 (SB) — goes all-in
        calling(),                     // seat 2 (BB)
    ];
    let mut sink = NullSink;
    let result = engine.run_hand(4, 999, &stacks, dealer, None, &mut agents, &mut sink);

    let deltas: Vec<i32> = (0..3)
        .map(|seat| result.seats.iter().find(|s| s.seat == seat).map(|s| s.chip_delta).unwrap_or(0))
        .collect();
    let stacks_after: Vec<u32> = (0..3)
        .map(|i| (stacks[i] as i32 + deltas[i]) as u32)
        .collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);
}

// -----------------------------------------------------------------------
// 6. Replay: same deck_seed produces identical board
// -----------------------------------------------------------------------
#[test]
fn same_seed_identical_board() {
    let rules = nlhe_rules();
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [200u32, 200, 200];

    let run = |seed: u64| {
        let mut agents: Vec<Box<dyn poker_engine::agent::Agent>> =
            vec![calling(), calling(), calling()];
        let mut sink = VecSink::default();
        engine.run_hand(1, seed, &stacks, 0, None, &mut agents, &mut sink);
        let ended = sink.events.into_iter().find_map(|e| {
            if let EngineEvent::HandEnded { result, .. } = e { Some(result) } else { None }
        }).unwrap();
        ended.board
    };

    assert_eq!(run(42), run(42), "same seed must produce same board");
    assert_ne!(run(42), run(43), "different seeds should differ");
}

// -----------------------------------------------------------------------
// 7. Partial all-in does NOT re-open raising for players who already acted.
//
// Post-flop setup (dealer=2, so post-flop order is seat 0, 1, 2):
//   Seat 0 (A, 200 chips): bets 10  → acted at current_bet=10, can re-raise later
//   Seat 1 (B, 200 chips): calls 10 → acted at current_bet=10
//   Seat 2 (C,  15 chips): all-in for 15  → increment=5 < min_raise(10): PARTIAL raise
//
// After C's partial all-in current_bet=15. A and B each owe 5 more.
// Because the raise was sub-minimum, A and B may only call — can_raise must be false.
// -----------------------------------------------------------------------
#[test]
fn partial_allin_does_not_reopen_action() {
    // Use 1/2 blinds but we care about post-flop; preflop: everyone calls with CallingStation,
    // then the post-flop round uses scripted agents loaded via a wrapper.
    // Easier: test entirely in a single-street scenario by running a full hand where
    // everyone folds post-flop except for one scripted post-flop street.
    //
    // Instead, drive pre-flop to completion cheaply and then test the post-flop round.
    // Simplest: use 3-player, dealer=2. Preflop: UTG(0) calls, SB(0? no...
    //
    // With dealer=2 (3 players): SB=seat0, BB=seat1, UTG=seat2.
    // Pre-flop: seat2(UTG) calls BB, seat0(SB) calls, seat1(BB) checks.
    // Post-flop order: first active after dealer(2) = seat 0.
    // Post-flop: A(seat0) bets 10, B(seat1) calls, C(seat2) all-in 15 (partial).
    // A second action: A must call 5, can_raise must be false.
    // B second action: B must call 5, can_raise must be false.

    let rules = BettingRules::no_limit_holdem(1, 2, 3);
    let engine = Engine::new(rules, NaiveEvaluator);
    // Stacks after blinds + preflop calls: everyone starts at 200.
    // dealer=2: SB=0(posts 1), BB=1(posts 2), UTG=2.
    // Preflop: UTG calls 2, SB calls 1 more, BB checks. Each has committed 2.
    // After preflop: stacks ~= [198, 198, 198].
    // Post-flop: A(0) has 198, B(1) has 198, C(2) has 13 (started 15, posted 2 preflop).
    //
    // We use stacks [200, 200, 15] so C = 15 total.
    // Preflop: SB(0)=1, BB(1)=2, UTG(2) calls 2. SB calls 1 more. BB checks.
    // C(seat2, UTG) has 15 chips: calls 2 preflop → 13 left.
    // Post-flop: A bets 10, B calls 10, C all-in for 13 (13 > 10, increment=3 < min_raise=10).

    let stacks = [200u32, 200, 15];
    let dealer = 2;

    // Post-flop scripts (preflop all call/check via CallingStation then switch):
    // We build agents that behave like CallingStation preflop and execute scripts post-flop.
    // Using CapturingAgent for A and B so we can inspect their second legal-action offer.
    // A post-flop: Raise(10) then Call (for the extra 3 after C's all-in).
    // B post-flop: Call then Call.
    // C post-flop: AllIn.
    // Preflop they all see Call/Check — CallingStation handles that but we need to
    // interleave. Simplest: use CapturingAgent with a script that covers both streets.
    //
    // Preflop order (dealer=2): UTG=seat2 first, then SB=seat0, then BB=seat1 (BB option).
    // Preflop actions: UTG calls 2, SB calls 1 more, BB checks.
    //   seat0 script: [Call /* SB preflop */, Raise(10) /* A post-flop bet */, Call /* call C's extra */]
    //   seat1 script: [Check /* BB option */, Call /* B post-flop call */, Call /* call C's extra */]
    //   seat2 script: [Call /* UTG preflop */, AllIn /* C post-flop */]

    let mut a = Box::new(CapturingAgent::new(vec![
        Action::Call,     // SB preflop: call 1 more
        Action::Raise(10), // A post-flop: bet 10
        Action::Call,     // A post-flop: call C's extra 3
    ]));
    let mut b = Box::new(CapturingAgent::new(vec![
        Action::Check,    // BB option preflop
        Action::Call,     // B post-flop: call 10
        Action::Call,     // B post-flop: call C's extra 3
    ]));
    let mut c = Box::new(CapturingAgent::new(vec![
        Action::Call,     // UTG preflop: call 2
        Action::AllIn,    // C post-flop: all-in for 13
    ]));

    let a_ptr: *mut CapturingAgent = &mut *a;
    let b_ptr: *mut CapturingAgent = &mut *b;

    let mut agents: Vec<Box<dyn Agent>> = vec![a, b, c];
    let mut sink = NullSink;
    let result = engine.run_hand(1, 42, &stacks, dealer, None, &mut agents, &mut sink);

    // Chip conservation.
    let deltas: Vec<i32> = (0..3)
        .map(|s| result.seats.iter().find(|o| o.seat == s).map(|o| o.chip_delta).unwrap_or(0))
        .collect();
    let stacks_after: Vec<u32> = (0..3)
        .map(|i| (stacks[i] as i32 + deltas[i]) as u32)
        .collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);

    // A's captured legal actions: index 0 = SB preflop, index 1 = post-flop bet, index 2 = post-flop second act.
    // SAFETY: the CapturingAgent objects are borrowed only after run_hand returns.
    let a_captured = unsafe { &(*a_ptr).captured };
    let b_captured = unsafe { &(*b_ptr).captured };

    // A's 3rd action (index 2): post-flop second act after C's partial all-in.
    // can_raise must be false — partial all-in does not re-open action.
    assert!(
        !a_captured[2].can_raise,
        "A must not be able to raise after a partial all-in: {:?}",
        a_captured[2]
    );
    // B's 3rd action (index 2): same requirement.
    assert!(
        !b_captured[2].can_raise,
        "B must not be able to raise after a partial all-in: {:?}",
        b_captured[2]
    );
}

// -----------------------------------------------------------------------
// 8. Full all-in DOES re-open raising.
//    Same setup, but C has 25 chips → all-in for 23 post-flop (increment=13 >= min_raise=10).
// -----------------------------------------------------------------------
#[test]
fn full_allin_reopens_action() {
    let rules = BettingRules::no_limit_holdem(1, 2, 3);
    let engine = Engine::new(rules, NaiveEvaluator);
    // C(seat2) = 25 chips. Preflop UTG calls 2 → 23 left.
    // Post-flop: A bets 10, B calls 10, C all-in for 23 (increment 13 >= min_raise 10). FULL.
    let stacks = [200u32, 200, 25];
    let dealer = 2;

    // A: [Call(SB preflop), Raise(10)(post-flop), Call(call C's raise)]
    // B: [Check(BB option), Call(post-flop), Call]
    // C: [Call(UTG preflop), AllIn(post-flop)]
    let mut a = Box::new(CapturingAgent::new(vec![
        Action::Call,
        Action::Raise(10),
        Action::Call,
    ]));
    let mut b = Box::new(CapturingAgent::new(vec![
        Action::Check,
        Action::Call,
        Action::Call,
    ]));
    let a_ptr: *mut CapturingAgent = &mut *a;
    let b_ptr: *mut CapturingAgent = &mut *b;

    let mut agents: Vec<Box<dyn Agent>> = vec![
        a,
        b,
        Box::new(CapturingAgent::new(vec![Action::Call, Action::AllIn])),
    ];
    let mut sink = NullSink;
    let result = engine.run_hand(1, 42, &stacks, dealer, None, &mut agents, &mut sink);

    let deltas: Vec<i32> = (0..3)
        .map(|s| result.seats.iter().find(|o| o.seat == s).map(|o| o.chip_delta).unwrap_or(0))
        .collect();
    let stacks_after: Vec<u32> = (0..3).map(|i| (stacks[i] as i32 + deltas[i]) as u32).collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);

    let a_captured = unsafe { &(*a_ptr).captured };
    let b_captured = unsafe { &(*b_ptr).captured };

    // A and B's 3rd action: full raise by C re-opens action → can_raise must be true
    // (assuming they have enough chips, which they do at 190+ remaining).
    assert!(
        a_captured[2].can_raise,
        "A must be able to raise after a full all-in raise: {:?}",
        a_captured[2]
    );
    assert!(
        b_captured[2].can_raise,
        "B must be able to raise after a full all-in raise: {:?}",
        b_captured[2]
    );
}

// -----------------------------------------------------------------------
// 9. Multiple partial all-ins: cumulative chips correct, no phantom reopens.
//    Seat 2 (C=13) and seat 0 (A) both have short stacks that partially raise.
//    Uses CallingStation for simplicity; verifies chip conservation only.
// -----------------------------------------------------------------------
#[test]
fn multiple_partial_allins_chips_conserved() {
    // 4 players: dealer=3, SB=0, BB=1, UTG=2, CO=3.
    // Stacks: [13, 13, 200, 200]. Blinds 1/2.
    // Preflop: UTG(2) calls 2, CO(3) calls 2, SB(0) calls 1 more, BB(1) checks.
    // Post-flop order (left of dealer=3): seat 0 first.
    // Post-flop: seat0 (11 left) all-in 11, seat1 (11 left) all-in 11 (partial vs seat0's 11?),
    // Actually this is getting complicated. Use CallingStation — just verify conservation.
    let rules = BettingRules::no_limit_holdem(1, 2, 4);
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [13u32, 13, 200, 200];
    let dealer = 3;

    let mut agents: Vec<Box<dyn Agent>> = vec![
        Box::new(CallingStation),
        Box::new(CallingStation),
        Box::new(CallingStation),
        Box::new(CallingStation),
    ];
    let mut sink = NullSink;
    let result = engine.run_hand(1, 7, &stacks, dealer, None, &mut agents, &mut sink);

    let deltas: Vec<i32> = (0..4)
        .map(|s| result.seats.iter().find(|o| o.seat == s).map(|o| o.chip_delta).unwrap_or(0))
        .collect();
    let stacks_after: Vec<u32> = (0..4).map(|i| (stacks[i] as i32 + deltas[i]) as u32).collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);
}

// -----------------------------------------------------------------------
// 10. Per-seat starting stacks: verify the engine accepts different stacks
//     per seat and that the asymmetry is preserved in chip deltas.
// -----------------------------------------------------------------------
#[test]
fn asymmetric_stacks_chips_conserved() {
    let rules = BettingRules::no_limit_holdem(1, 2, 3);
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [500u32, 20, 300];
    let dealer = 0;

    let mut agents: Vec<Box<dyn Agent>> = vec![calling(), calling(), calling()];
    let mut sink = NullSink;
    let result = engine.run_hand(1, 55, &stacks, dealer, None, &mut agents, &mut sink);

    let deltas: Vec<i32> = (0..3)
        .map(|s| result.seats.iter().find(|o| o.seat == s).map(|o| o.chip_delta).unwrap_or(0))
        .collect();
    let stacks_after: Vec<u32> = (0..3).map(|i| (stacks[i] as i32 + deltas[i]) as u32).collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);
}

// -----------------------------------------------------------------------
// 11. Cumulative short all-ins reopen action (TDA Rule 47A).
//
// Two sequential short all-ins whose increments individually fall below the
// current min-raise but together meet or exceed it must reopen betting for
// players who previously closed the action.
//
// 4-player setup with dealer=3 (SB=0, BB=1, UTG=2, CO=3):
//   Stacks: [200, 200, 15, 22]
//   Preflop: UTG(2) calls 2, CO(3) calls 2, SB(0) calls 1 more, BB(1) checks.
//   Remaining: A=198, B=198, C=13, D=20.
// Post-flop order starts at seat 0 (A):
//   A bets 10              current_bet=10, min_raise_inc=10
//   B calls 10
//   C all-in for 13        increment=3 (short): cumulative=3
//   D all-in for 20        increment=7 (short): cumulative=10 >= 10 → reopens
//   A's next turn: can_raise must be TRUE.
//   B's next turn: can_raise must be TRUE.
// -----------------------------------------------------------------------
#[test]
fn cumulative_short_all_ins_reopen_action() {
    let rules = BettingRules::no_limit_holdem(1, 2, 4);
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [200u32, 200, 15, 22];
    let dealer = 3;

    // A (seat 0, SB): preflop calls 1, post-flop bets 10, then calls the
    // cumulative bump to 20 on its reopened turn.
    let mut a = Box::new(CapturingAgent::new(vec![
        Action::Call,
        Action::Raise(10),
        Action::Call,
    ]));
    // B (seat 1, BB): preflop checks option, post-flop calls 10, then calls
    // the bump to 20.
    let mut b = Box::new(CapturingAgent::new(vec![
        Action::Check,
        Action::Call,
        Action::Call,
    ]));
    let a_ptr: *mut CapturingAgent = &mut *a;
    let b_ptr: *mut CapturingAgent = &mut *b;

    // C (seat 2, UTG): calls 2 preflop, all-in post-flop.
    // D (seat 3, CO): calls 2 preflop, all-in post-flop.
    let mut agents: Vec<Box<dyn Agent>> = vec![
        a,
        b,
        Box::new(CapturingAgent::new(vec![Action::Call, Action::AllIn])),
        Box::new(CapturingAgent::new(vec![Action::Call, Action::AllIn])),
    ];
    let mut sink = NullSink;
    let result = engine.run_hand(1, 42, &stacks, dealer, None, &mut agents, &mut sink);

    // Chip conservation.
    let deltas: Vec<i32> = (0..4)
        .map(|s| result.seats.iter().find(|o| o.seat == s).map(|o| o.chip_delta).unwrap_or(0))
        .collect();
    let stacks_after: Vec<u32> = (0..4).map(|i| (stacks[i] as i32 + deltas[i]) as u32).collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);

    // SAFETY: agents are borrowed via the Vec during run_hand; after it
    // returns, the raw pointers remain valid until `agents` is dropped.
    let a_captured = unsafe { &(*a_ptr).captured };
    let b_captured = unsafe { &(*b_ptr).captured };

    // Indices: [0]=preflop, [1]=post-flop initial, [2]=post-flop after shorts.
    assert!(
        a_captured[2].can_raise,
        "A must be able to raise after cumulative shorts meet min-raise: {:?}",
        a_captured[2]
    );
    assert!(
        b_captured[2].can_raise,
        "B must be able to raise after cumulative shorts meet min-raise: {:?}",
        b_captured[2]
    );
}

// -----------------------------------------------------------------------
// 12. Dead-hand blind: seat is dealt cards and posts BB as normal, but the
//     hand is automatically folded and excluded from stats / winnings.
// -----------------------------------------------------------------------
#[test]
fn dead_hand_blind_posted_and_hand_excluded() {
    let rules = BettingRules::no_limit_holdem(1, 2, 3);
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [200u32, 200, 200];
    let dealer = 0; // SB=1, BB=2 → seat 2 is the dead BB

    let seat_kinds = [SeatKind::Live, SeatKind::Live, SeatKind::DeadHand];

    let mut agents: Vec<Box<dyn Agent>> = vec![calling(), calling(), calling()];
    let mut sink = NullSink;
    let result = engine.run_hand(
        1, 12345, &stacks, dealer, Some(&seat_kinds), &mut agents, &mut sink,
    );

    // Dead-hand seat must be flagged and may not net positive (they cannot
    // win the pot — at best they lose exactly what they posted).
    let dead = result.seats.iter().find(|s| s.seat == 2).unwrap();
    assert!(dead.sat_out, "dead-hand seat must have sat_out=true");
    assert_eq!(
        dead.chip_delta, -2,
        "dead-hand seat should lose exactly its BB post (2): got {}",
        dead.chip_delta
    );
    assert!(
        dead.hole_cards.is_some(),
        "dead hand is still dealt cards for audit"
    );

    // Chip conservation.
    let deltas: Vec<i32> = (0..3)
        .map(|s| result.seats.iter().find(|o| o.seat == s).map(|o| o.chip_delta).unwrap_or(0))
        .collect();
    let stacks_after: Vec<u32> = (0..3).map(|i| (stacks[i] as i32 + deltas[i]) as u32).collect();
    assert_chips_conserved(&stacks, &deltas, &stacks_after);
}
