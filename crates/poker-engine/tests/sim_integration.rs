use poker_engine::agent::builtin::{CallingStation, RandomAgent};
use poker_engine::agent::Agent;
use poker_engine::core::NaiveEvaluator;
use poker_engine::game::{BettingRules, Engine, FileSink, NullSink, VecSink, EngineEvent};
use poker_engine::sim::{run_parallel, SeedMode, SimConfig, SimRunner, StackPolicy};

fn calling_agents(n: usize) -> Vec<Box<dyn Agent>> {
    (0..n).map(|_| -> Box<dyn Agent> { Box::new(CallingStation) }).collect()
}

fn random_agents(n: usize, base_seed: u64) -> Vec<Box<dyn Agent>> {
    (0..n)
        .map(|i| -> Box<dyn Agent> { Box::new(RandomAgent::new(base_seed + i as u64)) })
        .collect()
}

fn nlhe3() -> BettingRules {
    BettingRules::no_limit_holdem(1, 2, 3)
}

// -----------------------------------------------------------------------
// 1. Chip conservation over many hands
// -----------------------------------------------------------------------
#[test]
fn chip_conservation_100_hands() {
    let mut runner = SimRunner::new(
        Engine::new(nlhe3(), NaiveEvaluator),
        calling_agents(3),
        vec![500u32; 3],
    );
    let result = runner.run(100, &SimConfig::deterministic(1), &mut NullSink);
    let sum: i64 = result.chip_deltas.iter().sum();
    assert_eq!(sum, 0, "chips must be conserved over 100 hands: {:?}", result.chip_deltas);
    assert_eq!(result.hands_played, 100);
}

// -----------------------------------------------------------------------
// 2. Determinism: identical config → identical deltas
// -----------------------------------------------------------------------
#[test]
fn deterministic_runs_are_identical() {
    let config = SimConfig::deterministic(42);
    let run = || {
        let mut r = SimRunner::new_seeded(
            Engine::new(nlhe3(), NaiveEvaluator),
            calling_agents(3),
            vec![200u32; 3],
            0,
        );
        r.run(50, &config, &mut NullSink).chip_deltas
    };
    assert_eq!(run(), run());
}

// -----------------------------------------------------------------------
// 3. Different seeds produce different boards (not just different deltas)
// -----------------------------------------------------------------------
#[test]
fn different_seeds_produce_different_boards() {
    let board_from_seed = |seed: u64| {
        let mut sink = VecSink::default();
        let mut r = SimRunner::new_seeded(
            Engine::new(nlhe3(), NaiveEvaluator),
            calling_agents(3),
            vec![200u32; 3],
            0,
        );
        r.run(1, &SimConfig::deterministic(seed), &mut sink);
        sink.events.into_iter().find_map(|e| {
            if let EngineEvent::HandEnded { result, .. } = e { Some(result.board) } else { None }
        }).unwrap()
    };
    assert_ne!(board_from_seed(1), board_from_seed(2));
}

// -----------------------------------------------------------------------
// 4. Persistent stacks carry over — running total reflects actual play
// -----------------------------------------------------------------------
#[test]
fn persistent_stacks_carry_over() {
    let starting = vec![50u32; 3];
    let mut runner = SimRunner::new(
        Engine::new(nlhe3(), NaiveEvaluator),
        calling_agents(3),
        starting.clone(),
    );
    let config = SimConfig { seed: SeedMode::Sequential(10), stack_policy: StackPolicy::Persistent };
    let result = runner.run(20, &config, &mut NullSink);

    // Final stacks + cumulative losses = starting total chips in play
    let total_start: u32 = starting.iter().sum();
    let total_final: u32 = result.final_stacks.iter().sum();
    assert_eq!(
        total_start, total_final,
        "total chips in play must be conserved: start={total_start}, final={total_final}"
    );
}

// -----------------------------------------------------------------------
// 5. reset_stacks restores starting stacks
// -----------------------------------------------------------------------
#[test]
fn reset_stacks_restores_starting() {
    let starting = vec![100u32; 3];
    let mut runner = SimRunner::new(
        Engine::new(nlhe3(), NaiveEvaluator),
        calling_agents(3),
        starting.clone(),
    );
    let config = SimConfig { seed: SeedMode::Sequential(1), stack_policy: StackPolicy::Persistent };
    runner.run(30, &config, &mut NullSink);
    runner.reset_stacks();
    let result = runner.run(1, &SimConfig::deterministic(999), &mut NullSink);
    // After reset, starting stacks are used for the next hand.
    assert_eq!(result.final_stacks.iter().sum::<u32>(), starting.iter().sum::<u32>());
}

// -----------------------------------------------------------------------
// 6. Parallel runner: aggregate chip conservation
// -----------------------------------------------------------------------
#[test]
fn parallel_chip_conservation() {
    let stacks = vec![200u32; 3];
    let config = SimConfig::deterministic(77);
    let result = run_parallel(
        200,
        4,
        &stacks,
        &|| Engine::new(nlhe3(), NaiveEvaluator),
        &|| calling_agents(3),
        &config,
    );
    let sum: i64 = result.chip_deltas.iter().sum();
    assert_eq!(sum, 0, "parallel: chips must be conserved: {:?}", result.chip_deltas);
    assert_eq!(result.hands_played, 200);
}

// -----------------------------------------------------------------------
// 7. Parallel vs sequential: same total hands played
// -----------------------------------------------------------------------
#[test]
fn parallel_correct_hand_count() {
    let stacks = vec![200u32; 3];
    let config = SimConfig::deterministic(1);
    for threads in [1, 2, 3, 5, 7] {
        let result = run_parallel(
            100, threads, &stacks,
            &|| Engine::new(nlhe3(), NaiveEvaluator),
            &|| calling_agents(3),
            &config,
        );
        assert_eq!(result.hands_played, 100, "threads={threads}");
    }
}

// -----------------------------------------------------------------------
// 8. Random agents also conserve chips (not a calling-station artifact)
// -----------------------------------------------------------------------
#[test]
fn random_agents_chip_conservation() {
    let mut runner = SimRunner::new(
        Engine::new(nlhe3(), NaiveEvaluator),
        random_agents(3, 100),
        vec![200u32; 3],
    );
    let result = runner.run(100, &SimConfig::deterministic(5), &mut NullSink);
    let sum: i64 = result.chip_deltas.iter().sum();
    assert_eq!(sum, 0, "random agents: {:?}", result.chip_deltas);
}

// -----------------------------------------------------------------------
// 9. chips_per_hand sums to zero
// -----------------------------------------------------------------------
#[test]
fn chips_per_hand_zero_sum() {
    let mut runner = SimRunner::new_seeded(
        Engine::new(nlhe3(), NaiveEvaluator),
        calling_agents(3),
        vec![200u32; 3],
        0,
    );
    let result = runner.run(200, &SimConfig::deterministic(1), &mut NullSink);
    let total: f64 = result.chips_per_hand().iter().sum();
    assert!(total.abs() < 1e-9, "chips/hand must sum to 0: {total}");
}

// -----------------------------------------------------------------------
// 10. FileSink: events written and readable back
// -----------------------------------------------------------------------
#[test]
fn file_sink_writes_events() {
    let path = std::env::temp_dir().join("poker_test_sink.msgpack");
    {
        let mut sink = FileSink::create(&path).expect("create sink");
        let mut runner = SimRunner::new_seeded(
            Engine::new(nlhe3(), NaiveEvaluator),
            calling_agents(3),
            vec![200u32; 3],
            0,
        );
        runner.run(5, &SimConfig::deterministic(1), &mut sink);
        // sink flushes on drop
    }
    let data = std::fs::read(&path).expect("read back file");
    assert!(!data.is_empty(), "file should not be empty");

    // Parse events back: length-prefixed frames.
    let mut events: Vec<EngineEvent> = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + len > data.len() { break; }
        let event: EngineEvent = rmp_serde::from_slice(&data[pos..pos + len])
            .expect("deserialize event");
        events.push(event);
        pos += len;
    }

    let hand_starts = events.iter().filter(|e| matches!(e, EngineEvent::HandStarted { .. })).count();
    let hand_ends   = events.iter().filter(|e| matches!(e, EngineEvent::HandEnded { .. })).count();
    assert_eq!(hand_starts, 5, "5 HandStarted events expected");
    assert_eq!(hand_ends,   5, "5 HandEnded events expected");

    let _ = std::fs::remove_file(&path);
}
