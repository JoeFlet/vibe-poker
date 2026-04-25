# poker_engine

A high-performance Texas Hold'em engine written in Rust, designed as a foundation for AI agent development and bulk simulation.

## Goals

- **Fast simulation** — no heap allocation in the hot path, integer chip arithmetic throughout, lookup-table hand evaluation (PHEval integration planned)
- **Clean AI interface** — implement one trait (`Agent`) to plug in any decision-making logic
- **Observable & replayable** — structured event stream with separate deck and agent seeds so any hand can be replayed exactly
- **Correct pot logic** — side pots, split pots, and dead money from folds handled correctly at showdown

---

## Architecture

```
┌──────────────────────────────────────────────────────┐
│                    AI / Test Layer                   │
│   Agent trait  ·  Observation  ·  Built-in agents   │
├──────────────────────────────────────────────────────┤
│                   Game Layer                         │
│   Engine  ·  BettingRules  ·  EventSink              │
├──────────────────────────────────────────────────────┤
│                   Core Layer                         │
│   Card / Deck  ·  HandEvaluator  ·  HandRank         │
└──────────────────────────────────────────────────────┘
```

---

## Quick Start

```rust
use poker_engine::{
    agent::builtin::RandomAgent,
    core::NaiveEvaluator,
    game::{BettingRules, Engine, NullSink},
};

fn main() {
    let rules = BettingRules::no_limit_holdem(1, 2, 6);
    let engine = Engine::new(rules, NaiveEvaluator);

    let stacks = [200u32; 4];   // four players, 200 chips each
    let dealer  = 0;            // button seat

    // One agent per seat, independently seeded.
    let mut agents: Vec<Box<dyn poker_engine::agent::Agent>> = vec![
        Box::new(RandomAgent::new(1)),
        Box::new(RandomAgent::new(2)),
        Box::new(RandomAgent::new(3)),
        Box::new(RandomAgent::new(4)),
    ];

    let result = engine.run_hand(
        /*hand_id*/  1,
        /*deck_seed*/ 42,
        &stacks,
        dealer,
        &mut agents,
        &mut NullSink,
    );

    for outcome in &result.seats {
        println!("seat {}: {:+} chips", outcome.seat, outcome.chip_delta);
    }
}
```

---

## Writing an Agent

Implement the `Agent` trait. Only `act` is required; the lifecycle hooks all have default no-op implementations.

```rust
use poker_engine::agent::{Agent, Observation, RunConfig};
use poker_engine::game::{Action, HandId, HandResult};

struct MyAgent {
    // whatever state you need
}

impl Agent for MyAgent {
    /// Called on every decision point. Return one of the legal actions.
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        // obs.hole_cards     — your two private cards
        // obs.board          — community cards dealt so far (0–5)
        // obs.legal_actions  — what moves are currently valid
        // obs.players        — public state of all seats (stacks, bets, fold/all-in status)
        // obs.pot            — current pot total
        // obs.street         — Preflop / Flop / Turn / River
        // obs.position       — your seat index

        let la = &obs.legal_actions;
        if la.can_check {
            Action::Check
        } else {
            Action::Call
        }
    }

    // --- Optional lifecycle hooks ---

    /// Called once before the first hand of a run. Load trained strategies here.
    fn on_run_start(&mut self, config: &RunConfig) { let _ = config; }

    /// Called after the last hand. Flush anything that should persist to disk here.
    fn on_run_end(&mut self) {}

    /// Called before each hand. Reset per-hand transient state (e.g. reads, notes).
    fn on_hand_start(&mut self, hand_id: HandId) { let _ = hand_id; }

    /// Called after each hand with the full outcome. Update opponent models here.
    fn on_hand_end(&mut self, result: &HandResult) { let _ = result; }
}
```

### Action variants

| Variant | When legal |
|---|---|
| `Action::Fold` | Always |
| `Action::Check` | `legal_actions.can_check` — no bet to face |
| `Action::Call` | `legal_actions.can_call` — there is a bet to face and you have enough chips |
| `Action::Raise(amount)` | `legal_actions.can_raise` — `amount` is the **total** bet this street, must be in `[min_raise, max_raise]` |
| `Action::AllIn` | Always when you have chips — pushes your entire remaining stack |

`LegalActions` fields:

```rust
pub struct LegalActions {
    pub can_check:    bool,
    pub can_call:     bool,
    pub call_amount:  u32,   // chips needed to call (may be less than owed if short-stacked)
    pub can_raise:    bool,
    pub min_raise:    u32,   // total bet size for minimum legal raise
    pub max_raise:    u32,   // total bet size for maximum legal raise
    pub all_in_amount: u32,  // your remaining stack
}
```

---

## Events & Observability

Pass any `EventSink` to `run_hand` to capture the event stream. The engine emits events for every meaningful moment without storing anything itself.

```rust
use poker_engine::game::{EngineEvent, VecSink};

let mut sink = VecSink::default();
engine.run_hand(1, 42, &stacks, 0, &mut agents, &mut sink);

for event in &sink.events {
    match event {
        EngineEvent::HandStarted { deck_seed, .. } => {
            println!("hand started, deck_seed={deck_seed}");
        }
        EngineEvent::ActionTaken { seat, action, pot_total } => {
            println!("seat {seat} did {action:?}, pot now {pot_total}");
        }
        EngineEvent::HandEnded { result, .. } => {
            println!("board: {:?}", result.board);
        }
        _ => {}
    }
}
```

### Event types

| Event | Payload |
|---|---|
| `HandStarted` | `hand_id`, `dealer`, `deck_seed` |
| `HoleCardsDealt` | `seat`, `cards: [Card; 2]` |
| `BoardDealt` | `street`, `cards` |
| `ActionTaken` | `seat`, `action`, `pot_total` |
| `PlayerAllIn` | `seat`, `total_committed` |
| `HandEnded` | `hand_id`, `result: HandResult` |

### Built-in sinks

| Sink | Behaviour |
|---|---|
| `NullSink` | Discards all events. Zero overhead for bulk simulation. |
| `VecSink` | Collects events into `events: Vec<EngineEvent>`. Use for tests and replay capture. |

### Replay & determinism

The `deck_seed` from `HandStarted` is the only value needed to reproduce the exact same board and deal order. Agent seeds are passed at construction and are fully independent — the engine never touches them.

To replay a hand, pass the same `deck_seed` to `run_hand`. The board and hole cards will be identical.

---

## Built-in Agents

| Agent | Behaviour |
|---|---|
| `RandomAgent::new(seed)` | Picks uniformly at random from all legal actions. |
| `CallingStation` | Always checks when free, otherwise calls. Never raises. |
| `ScriptedAgent::new(actions)` | Follows a fixed action script in order. Panics if the script runs out. Useful for deterministic unit tests. |

---

## Persistence

AI agents manage their own persistence via the lifecycle hooks. The recommended format is **MessagePack** via the `rmp-serde` crate (already a dependency of `poker_engine`).

There are two categories of state:

| Category | Examples | When to persist |
|---|---|---|
| **Durable** | trained strategy profiles, hand history logs | Flush in `on_run_end`, load in `on_run_start` |
| **Transient** | per-opponent reads, session chip counts | Reset in `on_hand_start`, update in `on_hand_end` |

```rust
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Strategy { /* ... */ }

struct MyAgent {
    strategy: Strategy,      // durable
    session_notes: Vec<u8>,  // transient
}

impl Agent for MyAgent {
    fn act(&mut self, obs: &Observation<'_>) -> Action { todo!() }

    fn on_run_start(&mut self, _: &RunConfig) {
        if let Ok(bytes) = std::fs::read("strategy.msgpack") {
            if let Ok(s) = rmp_serde::from_slice(&bytes) {
                self.strategy = s;
            }
        }
    }

    fn on_run_end(&mut self) {
        let bytes = rmp_serde::to_vec(&self.strategy).unwrap();
        std::fs::write("strategy.msgpack", bytes).unwrap();
    }

    fn on_hand_start(&mut self, _: HandId) {
        self.session_notes.clear();  // reset transient state
    }
}
```

---

## Table Configuration

```rust
// No-limit Hold'em, 1/2 blinds, up to 6 players
let rules = BettingRules::no_limit_holdem(1, 2, 6);

// Custom (e.g. with ante)
let rules = BettingRules {
    variant: BetVariant::NoLimit,
    small_blind: 5,
    big_blind: 10,
    ante: 10,
    max_players: 9,
};
```

---

## Chip Conventions

- All chip values are `u32`, denominated in **big-blind units** by convention (there is no enforced denomination — pick a unit and be consistent).
- No floating-point arithmetic anywhere in the engine. Split pots divide evenly; the remainder chip goes to the first eligible winner left of the dealer.
- `HandResult::SeatOutcome::chip_delta` is `i32`: positive = net gain, negative = net loss for that hand.

---

## CLI Reporter

`poker_report` is a binary that runs a simulation and prints per-seat statistics.

```
cargo run --bin poker_report -- [OPTIONS]

Options:
  --hands   <N>           Hands to simulate            [default: 1000]
  --agents  <list>        Comma-separated agent specs  [default: random:1,random:2]
                            calling          CallingStation
                            random:<seed>    RandomAgent seeded at <seed>
  --stack   <N>           Starting stack per seat      [default: 200]
  --blinds  <sb>/<bb>     Blind levels                 [default: 1/2]
  --seed    <N>           Base deck seed (omit = random)
  --threads <N>           Parallel threads             [default: 1]
```

Example output:

```
poker_report  ·  1000 hands  ·  1/2 NL Hold'em  ·  200bb starting stacks

 Seat  Agent            Hands   Win%    Chip EV   VPIP    PFR     AF
 ────  ───────────────  ──────  ──────  ────────  ──────  ──────  ─────
 0     calling          1000    36.2%   +0.84     45.3%   0.0%    0.00
 1     random:1         1000    31.4%   -1.21     52.1%   18.4%   0.43
 2     random:2         1000    32.4%   +0.37     45.8%   16.1%   0.38

 Total hands: 1000  ·  Avg pot: 8.4 chips
```

The stats are computed by `StatsSink`, an `EventSink` you can embed in your own code:

```rust
use poker_engine::stats::StatsSink;

let mut stats = StatsSink::new(n_seats);
runner.run(1000, &config, &mut stats);
stats.print_report(&agent_names, &rules);
```

---

## Bulk Simulation

### Single-threaded

```rust
use poker_engine::sim::{SimConfig, SimRunner};

let engine = Engine::new(BettingRules::no_limit_holdem(1, 2, 6), NaiveEvaluator);
let stacks  = vec![200u32; 6];
let agents  = /* ... your agents ... */;

let mut runner = SimRunner::new(engine, agents, stacks);
let result = runner.run(10_000, &SimConfig::deterministic(42), &mut NullSink);

println!("{} hands played", result.hands_played);
for (seat, cpg) in result.chips_per_hand().iter().enumerate() {
    println!("seat {seat}: {cpg:+.3} chips/hand");
}
```

### Parallel

Each thread gets its own engine and agent set constructed by the provided factory closures.

```rust
use poker_engine::sim::{run_parallel, SimConfig};

let stacks = vec![200u32; 3];
let result = run_parallel(
    1_000_000,          // total hands
    8,                  // threads
    &stacks,
    &|| Engine::new(BettingRules::no_limit_holdem(1, 2, 3), NaiveEvaluator),
    &|| vec![
        Box::new(MyAgent::new()) as Box<dyn Agent>,
        Box::new(CallingStation),
        Box::new(RandomAgent::new(0)),
    ],
    &SimConfig::deterministic(1),
);
```

### SimConfig options

```rust
// Random deck seeds each hand (default for production runs)
SimConfig { seed: SeedMode::Random, stack_policy: StackPolicy::Reset }

// Fully reproducible: hand i gets seed base+i
SimConfig::deterministic(42)

// Stacks carry over across hands; busted players sit out
SimConfig { seed: SeedMode::Random, stack_policy: StackPolicy::Persistent }
```

After a Persistent run, call `runner.reset_stacks()` to rebuy all players back to starting stacks.

### FileSink: streaming event log

```rust
use poker_engine::game::FileSink;

let mut sink = FileSink::create("session.msgpack")?;
runner.run(1_000, &config, &mut sink);
// file is flushed on drop

// Read back: each event is a [u32-LE length][msgpack bytes] frame.
```

---

## Project Layout

```
src/
  core/
    card.rs          Card (u8), Rank, Suit — packed 6-bit representation
    deck.rs          Deck — Fisher-Yates shuffle with SmallRng
    evaluator.rs     HandEvaluator trait + NaiveEvaluator (all C(7,5) combos)
    hand_rank.rs     HandRank (u32) — opaque, directly comparable with >
  game/
    engine.rs        Engine<E> — run_hand, betting round, street loop
    action.rs        Action enum, LegalActions
    event.rs         EngineEvent, EventSink, NullSink, VecSink
    pot_calc.rs      collect_street_bets, build_showdown_pots
    rules.rs         BettingRules, BetVariant
    state.rs         GameState, PlayerState, Pot, Street, …
  agent/
    mod.rs           Agent trait, Observation, PublicPlayerState, RunConfig
    builtin.rs       RandomAgent, CallingStation, ScriptedAgent
  sim/
    mod.rs           SimRunner, run_parallel, SimConfig, SimResult, SeedMode, StackPolicy
tests/
  engine_integration.rs   Integration tests: fold-win, showdown, side pots, replay
benches/
  engine.rs          Criterion benchmarks
DESIGN.md            Full design document with architecture decisions
```

---

## Roadmap

| Milestone | Status |
|---|---|
| Card, Deck, HandEvaluator (NaiveEvaluator) | ✅ Done |
| GameState, BettingRules, Action validation | ✅ Done |
| Pot / showdown resolution (side pots, splits) | ✅ Done |
| Event system (EngineEvent, EventSink, VecSink, FileSink) | ✅ Done |
| Agent lifecycle hooks + MessagePack persistence | ✅ Done |
| SimRunner (single-threaded and parallel) | ✅ Done |
| `StatsSink` + `poker_report` CLI | 🔲 Next |
| PHEval FFI (fast lookup-table evaluator) | 🔲 Planned |
| CFRAgent / OpenSpiel bridge | 🔲 Planned |
| Card abstraction layer | 🔲 Planned |
| `egui` replay viewer (load FileSink logs, step through hands) | 🔲 Planned |

---

## Running Tests

```sh
cargo test
```

## Running Benchmarks

```sh
cargo bench
```
