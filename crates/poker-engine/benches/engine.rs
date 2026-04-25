use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use poker_engine::agent::builtin::CallingStation;
use poker_engine::agent::Agent;
use poker_engine::core::{Card, Deck, HandEvaluator, NaiveEvaluator, RsPokerEvaluator};
use poker_engine::game::{BettingRules, Engine, NullSink};
use poker_engine::sim::{run_parallel, SimConfig, SimRunner};

// -----------------------------------------------------------------------
// Core layer
// -----------------------------------------------------------------------

fn bench_hand_eval(c: &mut Criterion) {
    let cards: [Card; 7] = {
        let mut deck = Deck::new(42);
        std::array::from_fn(|_| deck.deal())
    };
    let naive = NaiveEvaluator;
    c.bench_function("naive_eval_7card", |b| {
        b.iter(|| naive.rank_7(black_box(cards)))
    });
    let rs = RsPokerEvaluator;
    c.bench_function("rs_poker_eval_7card", |b| {
        b.iter(|| rs.rank_7(black_box(cards)))
    });
}

fn bench_deck_shuffle(c: &mut Criterion) {
    c.bench_function("deck_new_and_deal_5", |b| {
        b.iter(|| {
            let mut d = Deck::new(black_box(42));
            [d.deal(), d.deal(), d.deal(), d.deal(), d.deal()]
        })
    });
}

// -----------------------------------------------------------------------
// Full hand
// -----------------------------------------------------------------------

fn make_calling_agents(n: usize) -> Vec<Box<dyn Agent>> {
    (0..n).map(|_| -> Box<dyn Agent> { Box::new(CallingStation) }).collect()
}

fn bench_full_hand(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_hand");
    for &n_players in &[2usize, 3, 6] {
        let rules = BettingRules::no_limit_holdem(1, 2, n_players);
        let engine = Engine::new(rules, RsPokerEvaluator);
        let stacks = vec![200u32; n_players];
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{n_players}p")),
            &n_players,
            |b, _| {
                b.iter(|| {
                    let mut agents = make_calling_agents(n_players);
                    engine.run_hand(1, black_box(42), &stacks, 0, None, &mut agents, &mut NullSink)
                })
            },
        );
    }
    group.finish();
}

// -----------------------------------------------------------------------
// SimRunner throughput (hands/second)
// -----------------------------------------------------------------------

fn bench_sim_runner(c: &mut Criterion) {
    let mut group = c.benchmark_group("sim_runner");
    for &hands in &[100usize, 1_000] {
        group.throughput(Throughput::Elements(hands as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{hands}h_3p")),
            &hands,
            |b, &hands| {
                b.iter(|| {
                    let engine = Engine::new(BettingRules::no_limit_holdem(1, 2, 3), RsPokerEvaluator);
                    let agents = make_calling_agents(3);
                    let stacks = vec![200u32; 3];
                    let mut runner = SimRunner::new_seeded(engine, agents, stacks, 42);
                    runner.run(hands, &SimConfig::deterministic(1), &mut NullSink)
                })
            },
        );
    }
    group.finish();
}

// -----------------------------------------------------------------------
// Parallel throughput
// -----------------------------------------------------------------------

fn bench_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel");
    for &threads in &[2usize, 4] {
        let hands = 500;
        group.throughput(Throughput::Elements(hands as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{hands}h_{threads}t")),
            &threads,
            |b, &threads| {
                let stacks = vec![200u32; 3];
                let config = SimConfig::deterministic(1);
                b.iter(|| {
                    run_parallel(
                        hands,
                        threads,
                        &stacks,
                        &|| Engine::new(BettingRules::no_limit_holdem(1, 2, 3), RsPokerEvaluator),
                        &|| make_calling_agents(3),
                        &config,
                    )
                })
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_hand_eval,
    bench_deck_shuffle,
    bench_full_hand,
    bench_sim_runner,
    bench_parallel
);
criterion_main!(benches);
