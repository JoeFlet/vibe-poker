# poker

A high-performance Texas Hold'em engine in Rust, plus the tools that ride on top of it: a stats reporter, an MCCFR blueprint trainer, a head-to-head play harness, and an `egui` replay viewer for stepping through recorded hands.

The repository is a Cargo workspace:

| Crate | Role |
|---|---|
| [crates/poker-engine](crates/poker-engine/) | Core library (game state, betting, evaluator, sim runner, MCCFR solver) and CLI binaries: `poker_report`, `poker_train`, `poker_play` |
| [crates/poker-client](crates/poker-client/) | `egui`/`eframe` desktop app — currently the replay viewer; will host the live client |

## Goals

- **Fast simulation** — no heap allocation in the hot path, integer chip arithmetic throughout, lookup-table hand evaluation via `rs_poker`
- **Clean AI interface** — implement one trait (`Agent`) to plug in any decision-making logic
- **Observable & replayable** — structured event stream with separate deck and agent seeds so any hand can be replayed exactly; logs are durable on disk via `FileSink`
- **Correct pot logic** — side pots, split pots, and dead money from folds verified by multi-player property tests

---

## Architecture

```
┌──────────────────────────────────────────────────────┐
│  poker-client (egui)   │   bots / scripts            │
│  replay viewer · live  │   train / play / report     │
├──────────────────────────────────────────────────────┤
│              MCCFR solver · blueprints               │
├──────────────────────────────────────────────────────┤
│  Agent trait  ·  Observation  ·  built-in agents     │
├──────────────────────────────────────────────────────┤
│  Engine  ·  BettingRules  ·  EventSink  ·  SimRunner │
├──────────────────────────────────────────────────────┤
│  Card / Deck  ·  HandEvaluator  ·  HandRank          │
└──────────────────────────────────────────────────────┘
```

---

## Normal usage: train → play → replay

The shippable end-to-end loop is three commands. Run them from the workspace root.

### 1. Train an MCCFR blueprint

```sh
cargo run --release --bin poker_train -- \
    --iters 200000 \
    --out blueprint.mp \
    --seed 7
```

`poker_train` runs external-sampling Monte Carlo CFR over the engine's abstract game tree. Output is a versioned MessagePack file (schema-tagged with the abstraction so loads fail loudly if it changes). A small training run (a few thousand iterations) finishes in well under a second and is enough to smoke-test the loop; production-quality play wants ≥1e6 iterations.

Key flags:

```
--iters    <N>        MCCFR iterations
--out      <path>     Blueprint output path
--seed     <N>        Base seed for the sampling RNG
--stacks   <list>     Per-seat stacks (default 200,200)
--blinds   <sb>/<bb>  Blind levels      (default 1/2)
--progress            Print iter/sec and info-set growth
```

### 2. Play the blueprint against bots

```sh
cargo run --release --bin poker_play -- \
    --blueprint blueprint.mp \
    --hands 5000 \
    --agents blueprint,calling \
    --log session.mp
```

`poker_play` loads a blueprint, wraps it in a `StrategyAdapter`, and runs head-to-head against built-in bots. Per-seat **VPIP**, **PFR**, **AF**, win-rate, and chip-EV print at the end via the same `print_report` table as `poker_report`. With `--log <path>`, the live run is teed into a `FileSink` so you get both the stats report and a durable event log from one pass.

Agent specs (one per seat, comma-separated):

| Spec | Behaviour |
|---|---|
| `blueprint` | The trained policy from `--blueprint` |
| `calling` | `CallingStation` — checks free, otherwise calls |
| `random:<seed>` | `RandomAgent` seeded at `<seed>` |

### 3. Replay the session visually

```sh
cargo run --release --bin poker-client -- --replay session.mp
```

The `egui` viewer loads the event log and lets you scrub through it: a cursor walks `events[0..=i]` and each frame derives a snapshot — hand id / dealer / street / pot, board cards, per-seat hole cards with folded/all-in/committed flags, the action log, and the final `HandResult` block when the hand terminates. ⏮◀▶⏭ buttons step the cursor; the slider jumps anywhere; clicking an event in the side panel snaps the cursor to it.

Launch with no arguments for a stub window that prompts for `--replay`.

---

## Quick benchmark / stats run

For "what does this matchup look like over N hands" without bothering with a blueprint, use `poker_report`:

```
cargo run --release --bin poker_report -- [OPTIONS]

Options:
  --hands   <N>           Hands to simulate            [default: 1000]
  --agents  <list>        Comma-separated agent specs  [default: random:1,random:2]
                            calling          CallingStation
                            random:<seed>    RandomAgent seeded at <seed>
                            human            HumanAgent (interactive)
                            nit:<seed>       Nit persona
                            tag:<seed>       TAG persona
                            lag:<seed>       LAG persona
                            maniac:<seed>    Maniac persona
                            tilt:<seed>      TiltProne persona
  --stack   <N>           Starting stack per seat      [default: 200]
  --stacks  <list>        Per-seat stacks (overrides --stack)
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

---

## Library: running a single hand

```rust
use poker_engine::{
    agent::builtin::RandomAgent,
    core::RsPokerEvaluator,
    game::{BettingRules, Engine, NullSink},
};

fn main() {
    let rules = BettingRules::no_limit_holdem(1, 2, 4);
    let engine = Engine::new(rules, RsPokerEvaluator);

    let stacks = [200u32; 4];
    let dealer = 0;

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

Use `RsPokerEvaluator` for production — it's the lookup-table 7-card evaluator from `rs_poker`. `NaiveEvaluator` (`C(7,5)` brute force) is kept for cross-checking.

---

## Writing an Agent

Implement the `Agent` trait. Only `act` is required; the lifecycle hooks all default to no-ops.

```rust
use poker_engine::agent::{Agent, Observation, RunConfig};
use poker_engine::game::{Action, HandId, HandResult};

struct MyAgent { /* state */ }

impl Agent for MyAgent {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        // obs.hole_cards     — your two private cards
        // obs.board          — community cards dealt so far (0–5)
        // obs.legal_actions  — what moves are currently valid
        // obs.players        — public state of all seats
        // obs.pot            — current pot total
        // obs.street         — Preflop / Flop / Turn / River
        // obs.position       — your seat index

        if obs.legal_actions.can_check { Action::Check } else { Action::Call }
    }

    fn on_run_start(&mut self, _config: &RunConfig) {}   // load durable state here
    fn on_run_end(&mut self) {}                          // persist durable state here
    fn on_hand_start(&mut self, _hand_id: HandId) {}     // reset transient state
    fn on_hand_end(&mut self, _result: &HandResult) {}   // update opponent models
}
```

### Action variants

| Variant | When legal |
|---|---|
| `Action::Fold` | Always |
| `Action::Check` | `legal_actions.can_check` — no bet to face |
| `Action::Call` | `legal_actions.can_call` — there is a bet to face and you have chips |
| `Action::Raise(amount)` | `legal_actions.can_raise` — `amount` is the **total** bet this street, must be in `[min_raise, max_raise]` |
| `Action::AllIn` | Always when you have chips — pushes your entire remaining stack |

```rust
pub struct LegalActions {
    pub can_check:    bool,
    pub can_call:     bool,
    pub call_amount:  u32,
    pub can_raise:    bool,
    pub min_raise:    u32,
    pub max_raise:    u32,
    pub all_in_amount: u32,
}
```

---

## Events & observability

Pass any `EventSink` to `run_hand` (or to `SimRunner::run`) to capture the event stream. The engine emits events for every meaningful moment without storing anything itself.

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
| `VecSink` | Collects events into `events: Vec<EngineEvent>` — for tests and replay capture. |
| `FileSink` | Streams events to disk as `[u32 LE length][rmp-serde bytes]` frames. Flushed on drop. |
| `StatsSink` | Aggregates per-seat VPIP / PFR / AF / win% / chip EV in real time. |

Round-trip a `FileSink` log with `read_event_log`:

```rust
use poker_engine::game::{FileSink, read_event_log};

let mut sink = FileSink::create("session.mp")?;
runner.run(1_000, &config, &mut sink);
drop(sink); // flushes

let events = read_event_log("session.mp")?;
println!("recorded {} events", events.len());
```

### Replay & determinism

The `deck_seed` from `HandStarted` is the only value needed to reproduce the exact board and deal order. Agent seeds are passed at construction and are fully independent — the engine never touches them. To replay a hand, pass the same `deck_seed` to `run_hand`.

---

## Built-in agents

| Agent | Behaviour |
|---|---|
| `RandomAgent::new(seed)` | Picks uniformly at random from all legal actions |
| `CallingStation` | Checks when free, otherwise calls. Never raises. |
| `ScriptedAgent::new(actions)` | Follows a fixed action script in order. Useful for unit tests. |
| `HumanAgent::new()` | Reads moves from stdin. Used by `poker_report --agents human,…` |
| `StrategyAdapter` | Wraps a `Strategy` (e.g. `BlueprintStrategy::from_table(...)`) as an `Agent` |

### Personas (in [agent::personas](crates/poker-engine/src/agent/personas.rs))

Hand-tuned policies that gate actions on a shared `Strength` bucket. Distinct enough to produce clean per-class stat profiles for opponent-modelling work.

| Persona | Profile |
|---|---|
| `Nit::new(seed)` | Very tight, mostly passive |
| `Tag::new(seed)` | Tight-aggressive (TAG) |
| `Lag::new(seed)` | Loose-aggressive (LAG) |
| `Maniac::new(seed)` | Raises or shoves nearly everything |
| `TiltProne::new(seed)` | TAG → LAG mode after losses; tracks recent chip swings |

CLI specs match the struct names: `nit:<seed>`, `tag:<seed>`, `lag:<seed>`, `maniac:<seed>`, `tilt:<seed>`. Use them anywhere `--agents` is accepted.

---

## Opponent dataset (`poker_dataset`)

Round-robin every persona heads-up against every other persona, then print three views over the resulting `HandStats` rows.

```sh
cargo run --release --bin poker_dataset -- \
    --personas nit,tag,lag,maniac,tilt \
    --hands 2000 \
    --window 500 \
    --out logs/
```

Outputs:

1. **Per-persona summary** — pooled VPIP / PFR / AF / WTSD% / chip EV across every matchup the persona played in.
2. **Class-conditional matrix** — rows for each `(own, opponent)` pair: how does TAG behave against a Maniac vs against a Nit? Where does LAG bleed money?
3. **Windowed time series** — rolling N-hand windows per persona, surfacing time-correlated drift (the `tilt` persona is the canonical example).

`--out <dir>` writes a `<a>_vs_<b>.mp` `FileSink` log per matchup so any session can be replayed in `poker-client --replay <path>` or re-aggregated offline. The aggregator API (`extract_hand_stats`, `class_conditional`, `windowed`) is in [dataset/](crates/poker-engine/src/dataset/) and works on any event log — your own bots and future client/server traffic slot in unchanged.

---

## Persistence

Two flavours, both MessagePack:

| What | API | Format |
|---|---|---|
| Trained MCCFR blueprint | `solver::save_blueprint` / `load_blueprint` | Schema-versioned + abstraction-tagged. Load fails if either differs. |
| Event log | `FileSink::create` / `read_event_log` | Length-prefixed `[u32 LE][msgpack]` frames. Streaming-friendly. |

Custom agents can manage their own persistence via the lifecycle hooks:

```rust
fn on_run_start(&mut self, _: &RunConfig) {
    if let Ok(bytes) = std::fs::read("model.mp") {
        if let Ok(s) = rmp_serde::from_slice(&bytes) { self.model = s; }
    }
}

fn on_run_end(&mut self) {
    let bytes = rmp_serde::to_vec(&self.model).unwrap();
    std::fs::write("model.mp", bytes).unwrap();
}
```

---

## Table configuration

```rust
let rules = BettingRules::no_limit_holdem(1, 2, 6); // sb, bb, max_players

// Custom (e.g. with ante)
let rules = BettingRules {
    variant: BetVariant::NoLimit,
    small_blind: 5,
    big_blind: 10,
    ante: 10,
    max_players: 9,
};
```

## Chip conventions

- Chip values are `u32`, denominated in big-blind units by convention (no enforced denomination).
- No floating-point arithmetic. Split pots divide evenly; the remainder chip goes to the first eligible winner left of the dealer.
- `SeatOutcome::chip_delta` is `i32`: positive = net gain, negative = net loss for that hand.

---

## Bulk simulation

### Single-threaded

```rust
use poker_engine::sim::{SimConfig, SimRunner};

let mut runner = SimRunner::new(engine, agents, stacks);
let result = runner.run(10_000, &SimConfig::deterministic(42), &mut NullSink);

for (seat, cpg) in result.chips_per_hand().iter().enumerate() {
    println!("seat {seat}: {cpg:+.3} chips/hand");
}
```

### Parallel

Each thread builds its own engine and agents from the supplied factory closures.

```rust
use poker_engine::sim::run_parallel;

let result = run_parallel(
    1_000_000,
    8,
    &stacks,
    &|| Engine::new(BettingRules::no_limit_holdem(1, 2, 3), RsPokerEvaluator),
    &|| vec![
        Box::new(MyAgent::new()) as Box<dyn Agent>,
        Box::new(CallingStation),
        Box::new(RandomAgent::new(0)),
    ],
    &SimConfig::deterministic(1),
);
```

### `SimConfig`

```rust
SimConfig { seed: SeedMode::Random,        stack_policy: StackPolicy::Reset      } // default
SimConfig::deterministic(42);                                                       // hand i → seed base+i
SimConfig { seed: SeedMode::Random,        stack_policy: StackPolicy::Persistent } // stacks carry over; busts sit out
```

After a `Persistent` run, `runner.reset_stacks()` rebuys everyone to starting stacks.

---

## Project layout

```
crates/
  poker-engine/
    src/
      core/         Card, Deck, HandEvaluator (RsPokerEvaluator + NaiveEvaluator), HandRank
      game/         Engine, BettingRules, EngineEvent + sinks, pot_calc, state
      agent/        Agent trait, Observation, builtin (Random/Calling/Scripted/Human)
      sim/          SimRunner, run_parallel, SimConfig, SeedMode, StackPolicy
      solver/       MCCFR trainer, info-set keying, BlueprintStrategy, save/load_blueprint
      abstraction/  Card-bucketing for solver keys (PreflopClass + postflop placeholder)
      stats/        StatsSink, print_report
      bin/
        poker_train.rs   MCCFR training loop → blueprint file
        poker_play.rs    Blueprint vs bots, optional --log
      main.rs            poker_report binary
    tests/          Integration + multi-player correctness tests
    benches/        Criterion benchmarks
  poker-client/
    src/
      main.rs       eframe entry, --replay CLI
      replay.rs     ReplayApp + Snapshot derivation + egui rendering
DESIGN.md           Architecture decisions and step-by-step build plan
```

---

## Roadmap

| Step | Status |
|---|---|
| Card / Deck / HandEvaluator (`RsPokerEvaluator` + `NaiveEvaluator`) | ✅ |
| `GameState`, `BettingRules`, action validation | ✅ |
| Pot / showdown resolution (side pots, splits) | ✅ |
| Event system (`EngineEvent`, sinks, `FileSink` + `read_event_log`) | ✅ |
| Agent lifecycle hooks + MessagePack persistence | ✅ |
| `SimRunner` (single-threaded and parallel) | ✅ |
| `StatsSink` + `poker_report` CLI | ✅ |
| MCCFR solver + abstraction layer + `BlueprintStrategy` | ✅ |
| Blueprint persistence (schema-versioned, abstraction-tagged) | ✅ |
| Multi-player correctness pass (3- and 6-max property tests) | ✅ |
| `poker_train` + `poker_play` CLI loop | ✅ |
| `egui` replay viewer (`poker-client --replay`) | ✅ |
| Persona bot pool + dataset aggregator + `poker_dataset` driver | ✅ |
| Exploit layer: opponent profiler + best-response mixing | 🔲 Next |
| `poker-server` + live `poker-client` | 🔲 Planned |

The full plan with rationale lives in [DESIGN.md](DESIGN.md).

---

## Running tests / benchmarks

```sh
cargo test --workspace
cargo bench -p poker_engine
```
