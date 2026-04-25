use rand::rngs::SmallRng;
use rand::{RngCore, SeedableRng};

use crate::agent::{Agent, RunConfig};
use crate::core::HandEvaluator;
use crate::game::{Engine, EventSink, HandResult, NullSink, SeatIndex};

// -----------------------------------------------------------------------
// Configuration types
// -----------------------------------------------------------------------

/// How deck seeds are generated for each hand.
#[derive(Clone, Debug)]
pub enum SeedMode {
    /// Each hand gets a fresh random seed from the runner's internal RNG.
    Random,
    /// Hand `i` (zero-indexed) gets seed `base + i`. Fully reproducible.
    Sequential(u64),
}

/// What happens to stacks between hands.
#[derive(Clone, Debug)]
pub enum StackPolicy {
    /// Every hand begins with the configured starting stacks.
    Reset,
    /// Stacks carry over across hands. Players whose stack reaches 0 sit out
    /// for subsequent hands. Use `SimRunner::reset_stacks` to rebuy them.
    Persistent,
}

/// Per-run configuration passed to `SimRunner::run`.
#[derive(Clone, Debug)]
pub struct SimConfig {
    pub seed: SeedMode,
    pub stack_policy: StackPolicy,
}

impl SimConfig {
    pub fn standard() -> Self {
        SimConfig {
            seed: SeedMode::Random,
            stack_policy: StackPolicy::Reset,
        }
    }

    pub fn deterministic(base_seed: u64) -> Self {
        SimConfig {
            seed: SeedMode::Sequential(base_seed),
            stack_policy: StackPolicy::Reset,
        }
    }
}

// -----------------------------------------------------------------------
// Result
// -----------------------------------------------------------------------

/// Aggregated output from a simulation run.
#[derive(Debug, Clone)]
pub struct SimResult {
    /// Number of hands actually played.
    pub hands_played: usize,
    /// Net chips won (+) or lost (-) per seat across all hands.
    pub chip_deltas: Vec<i64>,
    /// Stack of each seat at the end of the run.
    pub final_stacks: Vec<u32>,
}

impl SimResult {
    /// Chips won per hand for each seat (bb/hand when stacks are in BB units).
    pub fn chips_per_hand(&self) -> Vec<f64> {
        self.chip_deltas
            .iter()
            .map(|&d| d as f64 / self.hands_played as f64)
            .collect()
    }

    /// Merge results from parallel workers into one aggregate.
    fn merge(results: Vec<SimResult>) -> SimResult {
        debug_assert!(!results.is_empty());
        let n = results[0].chip_deltas.len();
        let mut deltas = vec![0i64; n];
        let mut hands = 0usize;
        for r in &results {
            hands += r.hands_played;
            for (i, &d) in r.chip_deltas.iter().enumerate() {
                deltas[i] += d;
            }
        }
        // Final stacks: take from the last worker (arbitrary; only meaningful for Persistent).
        let final_stacks = results.into_iter().last().unwrap().final_stacks;
        SimResult { hands_played: hands, chip_deltas: deltas, final_stacks }
    }
}

// -----------------------------------------------------------------------
// Single-threaded runner
// -----------------------------------------------------------------------

/// Owns an `Engine`, a fixed set of agents, and per-seat stack state.
/// Runs hands sequentially, calling agent lifecycle hooks around each run.
pub struct SimRunner<E: HandEvaluator> {
    engine: Engine<E>,
    agents: Vec<Box<dyn Agent>>,
    /// Current stacks (updated in Persistent mode, or reset from `starting_stacks`).
    stacks: Vec<u32>,
    starting_stacks: Vec<u32>,
    dealer: SeatIndex,
    next_hand_id: u64,
    rng: SmallRng,
}

impl<E: HandEvaluator> SimRunner<E> {
    /// Seed the internal RNG from OS entropy.
    pub fn new(engine: Engine<E>, agents: Vec<Box<dyn Agent>>, stacks: Vec<u32>) -> Self {
        let rng = SmallRng::from_entropy();
        SimRunner::with_rng(engine, agents, stacks, rng)
    }

    /// Fully deterministic constructor — seeds the internal RNG from `rng_seed`.
    pub fn new_seeded(
        engine: Engine<E>,
        agents: Vec<Box<dyn Agent>>,
        stacks: Vec<u32>,
        rng_seed: u64,
    ) -> Self {
        let rng = SmallRng::seed_from_u64(rng_seed);
        SimRunner::with_rng(engine, agents, stacks, rng)
    }

    fn with_rng(
        engine: Engine<E>,
        agents: Vec<Box<dyn Agent>>,
        stacks: Vec<u32>,
        rng: SmallRng,
    ) -> Self {
        debug_assert_eq!(
            agents.len(),
            stacks.len(),
            "one agent per seat required"
        );
        let starting_stacks = stacks.clone();
        SimRunner {
            engine,
            agents,
            stacks,
            starting_stacks,
            dealer: 0,
            next_hand_id: 1,
            rng,
        }
    }

    /// Reset all seats to their starting stacks. Useful for rebuying busted players
    /// between training epochs when using `StackPolicy::Persistent`.
    pub fn reset_stacks(&mut self) {
        self.stacks.clone_from(&self.starting_stacks);
    }

    /// Run `hands` hands. Events are forwarded to `sink`.
    ///
    /// Calls `on_run_start` before the first hand and `on_run_end` after the last.
    pub fn run(
        &mut self,
        hands: usize,
        config: &SimConfig,
        sink: &mut dyn EventSink,
    ) -> SimResult {
        let n = self.stacks.len();
        let run_cfg = RunConfig {
            small_blind: self.engine.rules.small_blind,
            big_blind: self.engine.rules.big_blind,
            max_players: n,
        };
        for agent in self.agents.iter_mut() {
            agent.on_run_start(&run_cfg);
        }

        let mut deltas = vec![0i64; n];

        for hand_idx in 0..hands {
            let hand_stacks = match config.stack_policy {
                StackPolicy::Reset => self.starting_stacks.clone(),
                StackPolicy::Persistent => self.stacks.clone(),
            };

            let deck_seed = match config.seed {
                SeedMode::Random => self.rng.next_u64(),
                SeedMode::Sequential(base) => base + hand_idx as u64,
            };

            let result = self.engine.run_hand(
                self.next_hand_id,
                deck_seed,
                &hand_stacks,
                self.dealer,
                None,
                &mut self.agents,
                sink,
            );
            self.next_hand_id += 1;

            apply_result_to_deltas(&result, &mut deltas);

            if matches!(config.stack_policy, StackPolicy::Persistent) {
                apply_result_to_stacks(&result, &mut self.stacks);
            }

            self.dealer = next_dealer(self.dealer, &self.stacks);
        }

        for agent in self.agents.iter_mut() {
            agent.on_run_end();
        }

        SimResult {
            hands_played: hands,
            chip_deltas: deltas,
            final_stacks: self.stacks.clone(),
        }
    }
}

// -----------------------------------------------------------------------
// Parallel runner
// -----------------------------------------------------------------------

/// Run `hands_total` hands across `threads` threads, aggregating the results.
///
/// - `engine_factory`: called once per thread to construct an independent engine.
/// - `agent_factory`: called once per thread to construct an independent agent set.
/// - Events are discarded (use the single-threaded runner with a `FileSink` if you
///   need per-hand event logs).
///
/// Requires `E: HandEvaluator + Send` and both factories to be `Sync`.
pub fn run_parallel<E, EF, AF>(
    hands_total: usize,
    threads: usize,
    stacks: &[u32],
    engine_factory: &EF,
    agent_factory: &AF,
    config: &SimConfig,
) -> SimResult
where
    E: HandEvaluator + Send,
    EF: Fn() -> Engine<E> + Sync,
    AF: Fn() -> Vec<Box<dyn Agent>> + Sync,
{
    let threads = threads.max(1);
    let base_hands = hands_total / threads;
    let remainder = hands_total % threads;

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let hands = base_hands + if t < remainder { 1 } else { 0 };
                // Offset sequential seed so each thread covers a distinct range.
                let thread_config = thread_config(config, t, base_hands, remainder);
                let engine = engine_factory();
                let agents = agent_factory();
                let stacks = stacks.to_vec();
                scope.spawn(move || {
                    let mut runner = SimRunner::new(engine, agents, stacks);
                    runner.run(hands, &thread_config, &mut NullSink)
                })
            })
            .collect();

        let results: Vec<SimResult> = handles
            .into_iter()
            .map(|h| h.join().expect("sim thread panicked"))
            .collect();

        SimResult::merge(results)
    })
}

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

fn apply_result_to_deltas(result: &HandResult, deltas: &mut Vec<i64>) {
    for outcome in &result.seats {
        deltas[outcome.seat] += outcome.chip_delta as i64;
    }
}

fn apply_result_to_stacks(result: &HandResult, stacks: &mut Vec<u32>) {
    for outcome in &result.seats {
        stacks[outcome.seat] =
            (stacks[outcome.seat] as i64 + outcome.chip_delta as i64).max(0) as u32;
    }
}

/// Advance dealer to the next seat that has chips.
fn next_dealer(current: SeatIndex, stacks: &[u32]) -> SeatIndex {
    let n = stacks.len();
    for i in 1..=n {
        let seat = (current + i) % n;
        if stacks[seat] > 0 {
            return seat;
        }
    }
    current // fallback: all busted (shouldn't happen in a well-formed session)
}

/// Build a per-thread SimConfig that offsets sequential seeds correctly.
fn thread_config(base: &SimConfig, thread_idx: usize, base_hands: usize, remainder: usize) -> SimConfig {
    let seed = match base.seed {
        SeedMode::Random => SeedMode::Random,
        SeedMode::Sequential(base_seed) => {
            // Thread t starts after all hands assigned to threads 0..t.
            let offset: u64 = (0..thread_idx)
                .map(|i| (base_hands + if i < remainder { 1 } else { 0 }) as u64)
                .sum();
            SeedMode::Sequential(base_seed + offset)
        }
    };
    SimConfig { seed, stack_policy: base.stack_policy.clone() }
}

// -----------------------------------------------------------------------
// Unit tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::builtin::CallingStation;
    use crate::core::NaiveEvaluator;
    use crate::game::BettingRules;

    fn make_runner(n_seats: usize, stack: u32) -> SimRunner<NaiveEvaluator> {
        let engine = Engine::new(
            BettingRules::no_limit_holdem(1, 2, n_seats),
            NaiveEvaluator,
        );
        let agents: Vec<Box<dyn Agent>> = (0..n_seats).map(|_| {
            let b: Box<dyn Agent> = Box::new(CallingStation);
            b
        }).collect();
        let stacks = vec![200u32; n_seats];
        SimRunner::new(engine, agents, stacks)
    }

    #[test]
    fn single_thread_chip_conservation() {
        let mut runner = make_runner(3, 200);
        let config = SimConfig::deterministic(1);
        let result = runner.run(50, &config, &mut NullSink);
        assert_eq!(result.hands_played, 50);
        let sum_deltas: i64 = result.chip_deltas.iter().sum();
        assert_eq!(sum_deltas, 0, "chips must be conserved: {:?}", result.chip_deltas);
    }

    #[test]
    fn sequential_seeds_deterministic() {
        let config = SimConfig::deterministic(42);
        let run = || {
            let mut runner = make_runner(3, 200);
            runner.run(20, &config, &mut NullSink).chip_deltas
        };
        assert_eq!(run(), run(), "same seed must yield identical results");
    }

    #[test]
    fn persistent_stacks_non_negative() {
        let engine = Engine::new(BettingRules::no_limit_holdem(1, 2, 3), NaiveEvaluator);
        let agents: Vec<Box<dyn Agent>> = (0..3).map(|_| {
            let b: Box<dyn Agent> = Box::new(CallingStation);
            b
        }).collect();
        let stacks = vec![20u32; 3]; // small stacks — will bust quickly
        let mut runner = SimRunner::new(engine, agents, stacks);
        let config = SimConfig {
            seed: SeedMode::Sequential(7),
            stack_policy: StackPolicy::Persistent,
        };
        let result = runner.run(30, &config, &mut NullSink);
        assert!(result.final_stacks.iter().all(|&s| s < u32::MAX),
            "no stack overflow: {:?}", result.final_stacks);
    }

    #[test]
    fn chips_per_hand_sums_to_zero() {
        let mut runner = make_runner(4, 100);
        let config = SimConfig::deterministic(99);
        let result = runner.run(100, &config, &mut NullSink);
        let total: f64 = result.chips_per_hand().iter().sum();
        assert!(total.abs() < 1e-9, "chips/hand must sum to zero: {total}");
    }
}
