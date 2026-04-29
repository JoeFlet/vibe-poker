# poker-engine

Pure-Rust Texas Hold'em rules library. No I/O, no tokio, no I/O-bound dependencies — just cards, deck, evaluator, game state, betting rules, the `Agent` trait, the event sink hierarchy, and a single-threaded / parallel `SimRunner`.

This crate is the foundation every other crate in the workspace builds on:

- **[poker-trainer](../poker-trainer/)** consumes it for MCCFR training and dataset aggregation.
- **[poker-server](../poker-server/)** runs `Engine::run_hand` on `spawn_blocking` to drive remote players.
- **[poker-client](../poker-client/)** uses it for `EngineEvent` shapes and the `Snapshot` derivation.

## Public surface

| Module | What it provides |
|---|---|
| `core` | `Card`, `Deck`, `RsPokerEvaluator` (lookup-table 7-card eval), `NaiveEvaluator` (cross-check), `HandRank` |
| `game` | `Engine`, `BettingRules`, `EngineEvent`, `EventSink`/`NullSink`/`VecSink`/`FileSink`, `read_event_log`, `Action`, `LegalActions`, `HandResult`, `tree::GameTree` |
| `agent` | `Agent` trait, `Observation`, `RunConfig`, `builtin::{RandomAgent, CallingStation, ScriptedAgent}`, `human::HumanAgent`, `personas::{Nit, Tag, Lag, Maniac, TiltProne}` |
| `sim` | `SimRunner`, `run_parallel`, `SimConfig`, `SeedMode`, `StackPolicy` |
| `stats` | `StatsSink`, `print_report` |
| `abstraction` | `PreflopClass` (suit-isomorphic 169-class bucketing) |
| `net` | `frame::{encode, decode, parse_length_prefix}`, `protocol::{ClientMessage, ServerMessage, ...}` shared with the server |

## CLI

`poker_report` is the engine's only binary — a quick "what does this matchup look like over N hands" stat printer. Usage and agent specs are in the [root README](../../README.md#poker_report).

## Design rules

Two cross-cutting invariants every change must respect:

- **Determinism via `deck_seed`.** The seed in `HandStarted` is the only value needed to reproduce the deal. Agent seeds are passed at construction and never touched by the engine. Don't pull entropy inside the engine; thread it through.
- **Chips are `u32`, BB-denominated, no floats anywhere.** Split-pot remainders go to the first eligible winner left of the dealer.

## Running a single hand

```rust
use poker_engine::{
    agent::builtin::RandomAgent,
    core::RsPokerEvaluator,
    game::{BettingRules, Engine, NullSink},
};

let rules = BettingRules::no_limit_holdem(1, 2, 4);
let engine = Engine::new(rules, RsPokerEvaluator);
let stacks = [200u32; 4];

let mut agents: Vec<Box<dyn poker_engine::agent::Agent>> = (0..4)
    .map(|i| Box::new(RandomAgent::new(i + 1)) as _)
    .collect();

let result = engine.run_hand(1, 42, &stacks, 0, &mut agents, &mut NullSink);
for outcome in &result.seats {
    println!("seat {}: {:+}", outcome.seat, outcome.chip_delta);
}
```

For `Agent` implementation, event sinks, and bulk simulation, see the [root README](../../README.md).

## Tests / benches

```sh
cargo test  -p poker_engine
cargo bench -p poker_engine
```
