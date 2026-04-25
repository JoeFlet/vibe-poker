//! Multi-player correctness pass: 3-max and 6-max edge cases that the
//! existing `engine_integration` and `sim_integration` suites don't cover.
//!
//! Focus areas:
//! - 6-max chip conservation under randomized play (no panics, no leaks)
//! - Multi-way side-pot distribution at showdown (eligibility correctness)
//! - Multi-way uncontested wins with mid-hand folds
//! - Cascade all-in: every player all-in for a different amount

use poker_engine::agent::builtin::{CallingStation, RandomAgent, ScriptedAgent};
use poker_engine::agent::Agent;
use poker_engine::core::NaiveEvaluator;
use poker_engine::game::{Action, BettingRules, Engine, NullSink, SeatIndex, VecSink};
use poker_engine::sim::{SeedMode, SimConfig, SimRunner, StackPolicy};

fn nlhe(seats: usize) -> BettingRules {
    BettingRules::no_limit_holdem(1, 2, seats)
}

fn assert_chips_conserved(starting: &[u32], deltas: &[i32]) {
    let before: i64 = starting.iter().map(|&s| s as i64).sum();
    let total_delta: i64 = deltas.iter().map(|&d| d as i64).sum();
    let after: i64 = before + total_delta;
    assert_eq!(before, after, "chip conservation broken: before={before}, after={after}, deltas={deltas:?}");
}

// -----------------------------------------------------------------------
// 1. 6-max chip conservation over many hands with mixed agents.
//
// The existing sim suite stops at 3-max. Six seats stress-tests the
// dealer/blind rotation logic, persistent-stack handling across more
// players, and the side-pot path under deeper action sequences.
// -----------------------------------------------------------------------
#[test]
fn six_max_mixed_agents_chip_conservation_long_run() {
    let starting = vec![300u32; 6];
    let mut agents: Vec<Box<dyn Agent>> = vec![
        Box::new(RandomAgent::new(1)),
        Box::new(CallingStation),
        Box::new(RandomAgent::new(3)),
        Box::new(CallingStation),
        Box::new(RandomAgent::new(5)),
        Box::new(CallingStation),
    ];
    let mut runner = SimRunner::new(
        Engine::new(nlhe(6), NaiveEvaluator),
        std::mem::take(&mut agents),
        starting.clone(),
    );

    let result = runner.run(500, &SimConfig::deterministic(2026), &mut NullSink);
    assert_eq!(result.hands_played, 500);

    let total: i64 = result.chip_deltas.iter().sum();
    assert_eq!(total, 0, "6-max chip total drifted: deltas={:?}", result.chip_deltas);
    // Note: with `StackPolicy::Reset`, per-seat cumulative deltas are
    // unbounded across many hands (each hand begins fresh at 300). The
    // single-stack floor check belongs in the persistent-stacks test.
}

// -----------------------------------------------------------------------
// 2. Property fuzz: many short runs with varied seat counts and seeds.
//
// We cover 2/3/4/6 seats with random agents across many seeds, asserting
// no panic and chip conservation per seed. This is the catch-all for
// silent breakage in dealer rotation, dead-button edges, and the
// uncontested path under random action sequences.
// -----------------------------------------------------------------------
#[test]
fn fuzz_random_play_chip_conservation_across_seat_counts() {
    for &seats in &[2usize, 3, 4, 6] {
        let starting = vec![200u32; seats];
        for base_seed in 0..40u64 {
            let agents: Vec<Box<dyn Agent>> = (0..seats)
                .map(|i| -> Box<dyn Agent> {
                    Box::new(RandomAgent::new(base_seed.wrapping_mul(31) + i as u64))
                })
                .collect();
            let mut runner = SimRunner::new(
                Engine::new(nlhe(seats), NaiveEvaluator),
                agents,
                starting.clone(),
            );
            let cfg = SimConfig {
                seed: SeedMode::Sequential(base_seed.wrapping_mul(101).wrapping_add(7)),
                stack_policy: StackPolicy::Reset,
            };
            let result = runner.run(60, &cfg, &mut NullSink);
            let total: i64 = result.chip_deltas.iter().sum();
            assert_eq!(
                total, 0,
                "fuzz {seats}-max seed={base_seed} drifted: deltas={:?}",
                result.chip_deltas
            );
        }
    }
}

// -----------------------------------------------------------------------
// 3. Three-way all-in cascade with three different stacks.
//
// Configures stacks 30/60/100 and forces all three players to commit
// their full stack preflop. Side-pot eligibility:
//   - Main pot: 30 × 3 = 90, all three eligible
//   - Side 1:   30 × 2 = 60, the two larger stacks eligible
//   - Side 2:   40 × 1 = 40, only the largest stack eligible (uncontested
//                 — they get their excess back)
// We don't pin a specific winner (deck is randomized), but we assert that
// every chip stays accounted for and that the largest stack at minimum
// gets back its uncontested 40 chips beyond any matched commitment.
// -----------------------------------------------------------------------
#[test]
fn three_way_allin_cascade_distributes_correctly() {
    let rules = nlhe(3);
    let engine = Engine::new(rules, NaiveEvaluator);
    let stacks = [30u32, 60, 100];
    let dealer: SeatIndex = 0;
    // All three go all-in preflop. SB=1, BB=2 (already posted), UTG=0
    // acts first preflop.
    //
    // Action order preflop heads-up-to-3-max is UTG, SB, BB.
    // Each player goes all-in on their first action. After all three are
    // all-in, the engine should run out the board and resolve.
    let mut agents: Vec<Box<dyn Agent>> = vec![
        Box::new(ScriptedAgent::new(vec![Action::AllIn])),
        Box::new(ScriptedAgent::new(vec![Action::AllIn])),
        Box::new(ScriptedAgent::new(vec![Action::AllIn])),
    ];
    let mut sink = VecSink::default();
    let result = engine.run_hand(1, 99, &stacks, dealer, None, &mut agents, &mut sink);

    let deltas: Vec<i32> = (0..3)
        .map(|s| {
            result
                .seats
                .iter()
                .find(|o| o.seat == s)
                .map(|o| o.chip_delta)
                .unwrap_or(0)
        })
        .collect();
    assert_chips_conserved(&stacks, &deltas);

    // Largest stack put 100 in. The most they can lose is everything they
    // could have been called for: 30 (main level) + 30 (mid level) = 60.
    // Their 40 chips of overcommitment had no contestants, so their delta
    // can never be worse than -60.
    assert!(
        deltas[2] >= -60,
        "seat 2 (largest stack) delta {} worse than the worst-case matched loss of -60",
        deltas[2]
    );

    // Main-level players (seat 0 and 1) cannot win more chips than the
    // total non-self contributions to the pots they can win. Concretely:
    // - seat 0 (30 stack) can win the main pot (90 total), so delta ≤ +60
    //   (they keep their own 30 plus opponents' 60).
    // - seat 1 (60 stack) can win main + side1 (90+60=150), so delta ≤ +90.
    assert!(
        deltas[0] <= 60,
        "seat 0 (30 stack) delta {} exceeds max possible +60",
        deltas[0]
    );
    assert!(
        deltas[1] <= 90,
        "seat 1 (60 stack) delta {} exceeds max possible +90",
        deltas[1]
    );
}

// -----------------------------------------------------------------------
// 4. Persistent stacks across many hands at 6-max: busted seats should
//    sit out cleanly, total chips remain conserved, and the runner
//    handles dealer rotation past sat-out players without panicking.
// -----------------------------------------------------------------------
#[test]
fn six_max_busted_seats_handled_across_hands() {
    // Asymmetric starting stacks so at least one will likely bust early
    // under random play.
    let starting = vec![20u32, 40, 60, 80, 100, 200];
    let agents: Vec<Box<dyn Agent>> = (0..6)
        .map(|i| -> Box<dyn Agent> { Box::new(RandomAgent::new(100 + i as u64)) })
        .collect();
    let mut runner = SimRunner::new(
        Engine::new(nlhe(6), NaiveEvaluator),
        agents,
        starting.clone(),
    );
    let cfg = SimConfig {
        seed: SeedMode::Sequential(7),
        stack_policy: StackPolicy::Persistent,
    };
    let result = runner.run(300, &cfg, &mut NullSink);

    let total: i64 = result.chip_deltas.iter().sum();
    assert_eq!(
        total, 0,
        "6-max chip total drifted with busted seats: deltas={:?}",
        result.chip_deltas
    );

    // No seat ended below zero stack.
    for (i, &delta) in result.chip_deltas.iter().enumerate() {
        let final_stack = starting[i] as i64 + delta as i64;
        assert!(
            final_stack >= 0,
            "seat {i} ended at stack {final_stack} (delta={delta}, start={})",
            starting[i]
        );
    }
}
