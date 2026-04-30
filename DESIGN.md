# Poker Engine Design Document

## Goals

- **High-throughput simulation**: capable of millions of hands/second for AI training (MCCFR, RL)
- **Clean AI interface**: plug-in agents with a simple action/observation API
- **Texas Hold'em first**: 6-max and heads-up, with architecture open to other variants
- **Correctness over cleverness at the boundary**: fast core, safe surface API

## Non-Goals

- Real-money handling
- Networking and UI live in **companion crates** (see "Companion Projects"
  below), not in this engine crate. The engine stays a pure library.
- Exhaustive game variant support at v1 — MVP is Texas Hold'em only, but the ruleset boundary (`BettingRules`, `Street`, action validation) is designed to be swappable

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────┐
│                    AI / Test Layer                   │
│   AgentInterface  ·  Evaluators  ·  Training Loops   │
├──────────────────────────────────────────────────────┤
│                   Game Layer                         │
│   GameState  ·  ActionSpace  ·  BettingRules         │
├──────────────────────────────────────────────────────┤
│                   Core Layer                         │
│   Card / Deck  ·  HandEvaluator  ·  Pot / Equity     │
└──────────────────────────────────────────────────────┘
```

**Language**: Rust for all engine layers. Python bindings via PyO3 for AI agent code if needed.

---

## Core Layer

### Card Representation

Use a packed 8-bit integer per card: `rank (4 bits) | suit (2 bits)`.

```
Rank: 2=0 … A=12   Suit: clubs=0, diamonds=1, hearts=2, spades=3
Card: u8  (0–51)
Hand: u64 bit-mask  (one bit per card in the 52-card deck)
```

A `u64` hand mask enables O(1) set operations (union, intersection, complement) via bitwise ops. This representation is directly compatible with lookup-table evaluators.

### Hand Evaluator

Integrate **PokerHandEvaluator (PHEval)** as the evaluation back-end:
- 7-card evaluation in ~30 ns via precomputed lookup tables
- MIT licensed, C/C++ with available bindings

Wrap it behind a trait/interface so the implementation can be swapped:

```rust
pub trait HandEvaluator {
    fn rank_7(&self, cards: [Card; 7]) -> HandRank;
    fn rank_5(&self, cards: [Card; 5]) -> HandRank;
}
```

`HandRank` is a `u16` where higher = better, enabling direct comparison.

### Deck

A Fisher-Yates shuffle over a `[Card; 52]` array. Use a fast PRNG (e.g., `SmallRng` / xoshiro256**) — `rand::thread_rng` is fine for correctness but too slow for bulk simulation.

---

## Game Layer

### GameState

Immutable-ish struct representing a complete hand snapshot. Cloning must be cheap for tree search.

```rust
pub struct GameState {
    pub street: Street,           // Preflop, Flop, Turn, River, Showdown
    pub board: [Option<Card>; 5],
    pub players: [PlayerState; MAX_PLAYERS],
    pub pot: Pot,
    pub action_on: SeatIndex,
    pub deck: Deck,               // remaining cards (for deal-out)
}

pub struct PlayerState {
    pub stack: u32,               // chips in big blind units (integer arithmetic only)
    pub hole_cards: Option<[Card; 2]>,
    pub bet_this_street: u32,
    pub status: PlayerStatus,     // Active, Folded, AllIn
}
```

**Integer chip arithmetic only** — no floats anywhere in the game layer. Use big-blind units as the base denomination.

### Pot

Track side pots explicitly from the start. This avoids expensive reconstruction at showdown.

```rust
pub struct Pot {
    pub main: u32,
    pub side: SmallVec<[SidePot; 4]>,  // rare; most hands have 0-1 side pots
}
```

### Action Space

```rust
pub enum Action {
    Fold,
    Check,
    Call,
    Raise(u32),   // total bet size, validated against min-raise rules
    AllIn,
}
```

The engine validates actions and returns `Err` for illegal moves. Agents always receive a `LegalActions` struct listing what is currently valid.

### Betting Rules

Encapsulated in a `BettingRules` struct (passed at game construction):

```rust
pub struct BettingRules {
    pub variant: BetVariant,       // NoLimit, PotLimit, FixedLimit
    pub small_blind: u32,
    pub big_blind: u32,
    pub ante: u32,
    pub max_players: usize,
    pub allow_straddle: bool,
}
```

---

## AI / Test Layer

### AgentInterface

The only contract an AI must satisfy:

```rust
pub trait Agent {
    fn act(&mut self, obs: &Observation) -> Action;
}
```

`Observation` exposes what the agent is *allowed to know*:

```rust
pub struct Observation {
    pub hole_cards: [Card; 2],
    pub board: &[Card],
    pub pot: &Pot,
    pub legal_actions: LegalActions,
    pub players: &[PublicPlayerState],  // stacks, bets — no private info
    pub street: Street,
    pub position: SeatIndex,
}
```

This boundary enforces perfect information hiding: agents cannot read opponent hole cards unless it's showdown.

### Simulation Runner

High-throughput batch runner for training loops:

```rust
pub struct SimRunner {
    pub config: SimConfig,
    pub agents: Vec<Box<dyn Agent>>,
}

impl SimRunner {
    /// Run N hands, return per-agent chip deltas
    pub fn run(&mut self, hands: usize) -> Vec<i64>;

    /// Parallel run across thread pool (agents must be Send)
    pub fn run_parallel(&mut self, hands: usize, threads: usize) -> Vec<i64>;
}
```

For RL/CFR training, expose a lower-level `GameTree` iterator that streams `(state, legal_actions)` pairs without materialising full game objects.

### Built-in Test Agents

| Agent | Description |
|---|---|
| `RandomAgent` | Uniform random over legal actions |
| `CallingStation` | Always calls |
| `ScriptedAgent` | Follows a provided action script — deterministic for unit tests |
| `CFRAgent` | Wraps a pre-trained blueprint strategy (see below) |

---

## Solver & Exploit Architecture

The AI strategy stack is built in two layers. The lower layer produces a near-GTO
blueprint via native **Monte Carlo CFR** trained on this engine. The upper layer
observes opponent play over time and mixes **best-response** adjustments against
inferred opponent models on top of the blueprint to maximize expected value.

Using OpenSpiel was considered as a shortcut to a solver, but rejected: owning the
trainer natively means the solver sees the same rule edge cases (TDA 43 / 47A,
dead-hand blinds, side pots) that the engine already implements, and the exploit
layer can share the engine's `Observation` / info-set representation directly
rather than bridging across an external process.

### Modules

- `crate::abstraction` — card abstractions (Stage A: preflop 169-class canonical
  form, done). Postflop buckets are deferred until the equity / distribution work
  lands.
- `crate::solver::action` — `AbstractAction` enum, the discrete action set the
  solver operates on.
- `crate::solver::info_set` — `InfoSet` key: `(street, relative_position, bucket,
  history)`. Rotation-invariant so symmetric decisions share regret storage.
- `crate::solver::strategy` — `StrategyAgent` trait, `ActionProbs` distribution,
  `StrategyAdapter` (engine `Agent` impl), and `UniformRandomStrategy` baseline.
- `crate::game::tree` — `GameTree` pausable stepper over a single hand, the
  substrate the trainer iterates over. See below.

### GameTree — pausable hand stepper

`Engine::run_hand` plays a full hand against fixed agents. CFR needs the
inverse: pause at a decision, clone the state, try different actions, measure
terminal utility across branches. `GameTree` is that inverse.

Shape:

```rust
pub enum NodeKind {
    Decision { seat: SeatIndex },
    Terminal,
}

pub struct GameTree {
    pub state: GameState,
    pub deck: Deck,
    // private: Phase::{Decision{ current, has_responded }, Terminal{ chip_deltas }}
}

impl GameTree {
    pub fn new<E: HandEvaluator>(engine: &Engine<E>, ..., sink: &mut dyn EventSink) -> Self;
    pub fn current(&self) -> NodeKind;
    pub fn legal_actions<E>(&self, engine: &Engine<E>) -> Option<LegalActions>;
    pub fn observation_owned<E>(&self, engine: &Engine<E>) -> Option<OwnedObservation>;
    pub fn apply_action<E>(&mut self, engine: &Engine<E>, action: Action, sink: &mut dyn EventSink);
    pub fn utilities(&self) -> Option<&[i64]>;
    pub fn into_result(self, hand_id: HandId) -> HandResult;
}
// Plus `#[derive(Clone)]`.
```

Invariants the trainer relies on:

- **Chance-through-clone determinism.** The `Deck` lives inside the tree.
  Cloning the tree clones the deck, so two branches from the same decision
  point produce the same subsequent board and hole-card deals. This is what
  makes external-sampling MCCFR's branch comparisons apples-to-apples without
  having to pre-deal cards into a separate structure.
- **No chance nodes in the public surface.** Board deals and showdown
  resolution happen inside `apply_action` between one decision and the next.
  The consumer sees only Decision → Decision → ... → Terminal.
- **Bug-compatible with `run_hand`.** The stepper reuses the same
  `Engine::{post_blinds, deal_hole_cards, apply_action, compute_legal_actions,
  resolve_showdown, ...}` helpers (now `pub(crate)`). TDA rule handling —
  cumulative short all-ins, dead-hand blinds, canonical BB first-to-act — is
  inherited rather than reimplemented.

The MCCFR trainer (step 12) will call `apply_action` in the outer path, and
`clone` + `apply_action` on interior nodes for branch evaluation.

### MCCFR trainer

`crate::solver::mccfr::MccfrTrainer` is the native external-sampling CFR
trainer. It is heads-up only for now; multi-player is deferred to the
exploit layer (see rationale below).

Per training iteration:

1. For each traverser seat (0 and 1), construct a fresh `GameTree` with a
   seed-determined deal.
2. Walk the tree recursively. At a Decision node for the traverser,
   enumerate every legal `AbstractAction`, clone the tree, and recurse each
   branch to get a per-action utility. At a Decision node for the opponent,
   sample one action from the opponent's current (regret-matched) strategy.
   At a Terminal, return the traverser's net chip P/L.
3. At each traverser info set, accumulate counterfactual regret
   `regret[a] += branch_value[a] - node_value` and strategy-sum
   `strategy_sum[a] += current_strategy[a]`.

Storage lives in `RegretTable`: `HashMap<InfoSet, RegretEntry>`. Each entry
holds `regret: [f32; 4]`, `strategy_sum: [f32; 4]`, and a visit counter.
Regret matching (`regret_matching`) turns regret into the next iteration's
strategy; normalising `strategy_sum` (via `BlueprintStrategy`) yields the
time-averaged strategy, which is what converges to equilibrium.

`BlueprintStrategy` implements `StrategyAgent`, so trained policies plug
directly into the same `StrategyAdapter` pipeline used by
`UniformRandomStrategy`. Info sets unvisited during training fall back to
uniform over the legal subset.

#### Why heads-up only

CFR in 3+ player games converges only to a correlated equilibrium, not a
Nash equilibrium — and in poker, opponents rarely play correlated
strategies. Modern multi-player systems (Pluribus etc.) train a heads-up
blueprint and use depth-limited real-time search plus opponent modeling at
the table. That is the shape the exploit layer will take on top of this
trainer; building the multi-player trainer before the exploit layer would
be wasted motion.

#### Known limitations (tracked, not blocking)

- **Coarse postflop buckets**: postflop info sets all share `bucket = 0`
  until the postflop card-abstraction work (step 14+ material) lands, so
  the trainer's postflop play will be weak. Preflop buckets (169-class
  canonical) are complete and provide the meaningful training signal.
- **Single raise size**: the action abstraction exposes only a pot-sized
  raise. Richer sizing (half-pot, two-pot, all-in as distinct sizes) is a
  future widening of `AbstractAction`; the trainer code generalises over
  any finite set.
- **Convergence diagnostics**: no exploitability computation yet. The
  current convergence test is a head-to-head smoke test (blueprint vs
  uniform random, 1000 iters ⇒ positive chip EV over 2000 hands). A
  best-response solver will be added before the trainer is declared
  production-ready.

### Action abstraction

Stage 1 uses 4 abstract actions: `Fold`, `Call` (includes Check when nothing is
owed), `Raise` (pot-sized), and `AllIn`. This is the minimum viable set for
preflop trees and small experiments. Additional raise sizings (half-pot,
two-pot) will be added when training scale requires finer granularity — the
enum is `#[repr(u8)]` and indexed by `AbstractAction::index()` so regret tables
extend by widening a single dimension.

`legal_abstract_actions(&LegalActions)` computes the legal subset for a given
decision point. Fold is excluded when the player can check: burning probability
on a strictly-dominated action would waste regret-storage capacity during
training.

### Concretization

`concretize(AbstractAction, &Observation) -> Action` maps an abstract choice to
a concrete engine `Action`. Pot-sized raise target:

```
pot_after_call = pot.total() + sum(player.bet_this_street)
target         = clamp(min_raise + pot_after_call, min_raise, max_raise)
```

Illegal abstract actions fall back to the safest legal alternative (Call or
Check). Trained strategies should assign probability 0 to illegal actions; the
fallback is a defensive last line, not the primary correctness mechanism.

### InfoSet key

```rust
pub struct InfoSet {
    pub street: Street,
    pub position: Position,   // 0 = dealer, 1 = SB, 2 = BB, ... rotation-invariant
    pub bucket: u16,          // PreflopClass::index() preflop; 0 postflop (TODO)
    pub history: SmallVec<[AbstractAction; 12]>,
}
```

Two decisions share an `InfoSet` iff they are interchangeable from the acting
player's perspective. Rotation-invariance means a button-vs-BB decision with
the same cards and action sequence reuses regret regardless of which absolute
seats are involved — this is the entire reason `Observation` now carries a
`dealer` field.

The `history` field is a `SmallVec` with 12 inline slots; deeper trees spill to
the heap but are rare. Tuning this capacity is a performance knob, not a
correctness concern.

### StrategyAgent and the adapter

```rust
pub trait StrategyAgent: Send + Sync {
    fn strategy(&self, info: &InfoSet, legal: &LegalActions) -> ActionProbs;
}
```

`StrategyAdapter<S: StrategyAgent>` wraps any `StrategyAgent` into an engine
`Agent`, threading an abstract-action history across the hand and clearing it
on `on_hand_start`. This is the single bridge between the solver's abstract
world and the engine's concrete world: trainers, blueprints, and exploit
policies all share it.

`UniformRandomStrategy` is a trivial `StrategyAgent` used as a baseline and as
a smoke test — it also serves as the starting policy for MCCFR regret
minimization.

### Exploit layer (planned)

Long-term vision: combine the GTO blueprint with opponent-specific deviations.

- **Opponent profiling** — a passive observer (`EngineEvent` consumer) builds a
  running model per seat: VPIP / PFR / AF baseline, plus conditional stats
  (fold-to-3bet, c-bet frequency, etc.). The existing `StatsSink`
  infrastructure is the obvious substrate.
- **Best-response mixing** — at decision time, combine the blueprint's
  `ActionProbs` with a best-response distribution computed against the opponent
  model. The mixing weight trades off exploitation against exploitability:
  blueprint-only is unexploitable but untuned; pure best-response is maximally
  exploitative but blows up against a balanced opponent.
- **Attack surface** — because every decision flows through the `StrategyAgent`
  trait, exploitative strategies slot in as another implementor without any
  engine change. The `Observation` already carries the full public state the
  exploit layer needs to index its opponent model.

This layering is deliberate: GTO as the anchor, exploit as a correction. It
matches how strong human players think about the game and keeps the solver
component pure (it never needs to know about opponent models).

---

## Performance Targets

| Operation | Target |
|---|---|
| Hand evaluation (7-card) | < 50 ns |
| Full hand simulation (2 players) | < 500 ns |
| Bulk simulation (parallel, 8 cores) | > 5M hands/sec |
| Game state clone | < 20 ns |
| Action validation | < 10 ns |

These are achievable with lookup-table eval + integer arithmetic + no heap allocation in the hot path.

### Hot Path Rules

1. No heap allocation during a hand (pre-allocate; reuse game state structs)
2. No virtual dispatch in the core layer (use generics / monomorphisation)
3. No floating point (stacks are integer chip counts)
4. No locking on the game state (each simulation thread owns its state)

---

## Open Source References

| Project | Role | URL |
|---|---|---|
| **PokerHandEvaluator (PHEval)** | Hand evaluation back-end | https://github.com/HenryRLee/PokerHandEvaluator |
| **OpenSpiel** | CFR solvers + game-theory baselines | https://github.com/deepmind/open_spiel |
| **RLCard** | RL environment reference + datasets | https://github.com/datamllab/rlcard |
| **treys** | Python prototyping / sanity checks | https://github.com/ihendley/treys |
| **poker_ai (fedden)** | Pluribus MCCFR reference implementation | https://github.com/fedden/poker_ai |

---

## Persistence

AIs may persist data to disk using **MessagePack** (`rmp-serde` crate) for both performance and simplicity.

Two categories of AI state with different lifetimes:

| Category | Examples | Lifetime |
|---|---|---|
| **Durable** | trained strategy profiles, hand history logs | persists across runs |
| **Transient** | per-opponent player profiles, session reads | reset between runs, retained between hands |

The `Agent` trait exposes optional lifecycle hooks to support this:

```rust
pub trait Agent {
    fn act(&mut self, obs: &Observation) -> Action;

    /// Called once before the first hand of a run. Load durable state here.
    fn on_run_start(&mut self, config: &RunConfig) {}

    /// Called after the last hand of a run. Flush durable state here.
    fn on_run_end(&mut self) {}

    /// Called between hands. Transient per-hand state should be reset here.
    fn on_hand_start(&mut self, hand_id: HandId) {}

    /// Called at showdown / hand conclusion with full outcome.
    fn on_hand_end(&mut self, result: &HandResult) {}
}
```

All hooks have default no-op implementations so simple agents don't need to implement them.

---

## Observability & Replay

The engine emits a structured event stream during every hand. Consumers subscribe via a channel or callback — the engine itself does not log or store anything.

### Event Types

```rust
pub enum EngineEvent {
    HandStarted   { hand_id: HandId, dealer: SeatIndex, deck_seed: u64 },
    CardsDealt    { seat: SeatIndex, hole_cards: [Card; 2] },
    BoardDealt    { street: Street, cards: Vec<Card> },
    ActionTaken   { seat: SeatIndex, action: Action, pot_after: u32 },
    PlayerAllIn   { seat: SeatIndex, amount: u32 },
    HandEnded     { result: HandResult },
}
```

`HandResult` includes the full board, each player's hole cards, and per-seat chip deltas.

### Replay & Determinism

Every hand records two seeds at `HandStarted`:
- **`deck_seed`** — seeds the shuffler RNG exclusively
- Agents are seeded independently at construction and their seeds are opaque to the engine

This separation means:
1. Replaying a hand with the same `deck_seed` always produces the same board and deal, regardless of agent behavior
2. Agent RNG cannot accidentally influence (or be reverse-engineered from) the deck order
3. Bugs can be reproduced by pinning `deck_seed` without needing to replay the full agent state

The `SimRunner` accepts an optional `deck_seed` override for deterministic replay:

```rust
pub struct SimConfig {
    pub hands: usize,
    pub deck_seed: Option<u64>,   // None = random per hand; Some(n) = fixed seed for all hands
    pub event_sink: Option<Box<dyn EventSink>>,
}

pub trait EventSink: Send {
    fn on_event(&mut self, event: &EngineEvent);
}
```

Built-in sinks: `NullSink` (default, zero overhead), `VecSink` (collects to memory for tests), `FileSink` (streams to MessagePack file for post-hoc analysis).

---

## Tooling: CLI Reporter

The primary user-facing tool for comparing AIs is a CLI binary (`poker_report`) that drives `SimRunner` and prints per-seat statistics.

### Stats collected (`StatsSink`)

`StatsSink` implements `EventSink` and accumulates per-seat counters across a run.

| Stat | Definition |
|---|---|
| **Win%** | Hands with a positive chip delta / hands dealt |
| **Chip EV** | Total chip delta / hands dealt (bb/hand when stacks are in BB units) |
| **VPIP** | Voluntarily Put money In Pot — % of hands where seat called or raised preflop |
| **PFR** | PreFlop Raise % — % of hands where seat raised or went all-in preflop |
| **AF** | Aggression Factor — (raises + all-ins) / calls across all streets |

VPIP and PFR are computed by tracking `ActionTaken` events while on the Preflop street (inferred from `HandStarted` / `BoardDealt` event ordering).

### Planned CLI flags

```
poker_report [OPTIONS]

Options:
  --hands  N               Number of hands to simulate [default: 1000]
  --agents <list>          Comma-separated agent specs: calling, random:<seed>
  --stack  N               Starting stack per seat [default: 200]
  --blinds small/big       Blind levels [default: 1/2]
  --seed   N               Base deck seed (omit for random)
  --threads N              Parallel threads [default: 1]
```

### Planned GUI (deferred)

A graphical viewer (`egui`/`eframe`) will be added after the CLI reporter is stable. It will load `FileSink` logs and step through hands event-by-event, rendering a card table with hole cards, board, pot sizes, and action history.

---

## Companion Projects

The engine crate (`poker`) is a pure library: deterministic, dependency-light,
no I/O beyond the optional `FileSink` codec. Anything user-facing lives in a
sibling crate so the engine stays clean and the boundary stays testable.

Planned siblings (Cargo workspace, separate crates):

- **`poker-server`** — authoritative game host. Owns the `Engine`, accepts
  client connections (TCP / WebSocket), routes `Action`s in and broadcasts
  `EngineEvent`s out. Same wire types as the in-process API; serialisation
  via the existing `serde` derives. Connected clients can be humans or bots
  written against the same `Agent` trait.
- **`poker-client`** — desktop GUI (egui/eframe). Renders a card table from
  an `EngineEvent` stream, whether the stream comes from a `FileSink` log
  (replay) or a live `poker-server` connection (play). Replay viewer and
  live client share the same rendering layer.
- **`poker-bots`** — concrete bot implementations beyond the test agents in
  `poker::agent`. Houses the diverse opponent set used by the in-house
  dataset run (step 17). Kept separate so iteration on bots doesn't churn
  the engine crate.

Wire format and protocol details TBD when `poker-server` lands; default
plan is MessagePack over WebSocket with the engine's existing event types
serving as the schema source of truth.

---

## Suggested Build Order

1. **Card + Deck + HandEvaluator** — unit-test against known hands ✅
2. **GameState + BettingRules** — property-test with `RandomAgent` vs `RandomAgent` ✅
3. **Pot + showdown resolution** — side-pot cases exhaustively tested ✅
4. **Event system + `VecSink` / `FileSink`** — wired in from the start ✅
5. **SimRunner single-threaded + parallel** — benchmarked ✅
6. **Agent lifecycle hooks + MessagePack persistence** ✅
7. **`StatsSink` + CLI reporter** (`poker_report` binary) ✅
8. **Fast hand evaluator** — `RsPokerEvaluator` via the `rs_poker` crate
   (pure Rust, ~50 ns / 7-card, ~36× faster than the naive reference) ✅
9. **Card abstraction Stage A** — 169-class preflop canonical form
   (`crate::abstraction::PreflopClass`). Postflop equity buckets deferred. ✅
10. **Solver scaffolding** — `AbstractAction`, `InfoSet`, `StrategyAgent`,
    `StrategyAdapter`, `UniformRandomStrategy` ✅
11. **Game-tree iterator** — `GameTree` stepper with cheap cloning and
    deterministic chance-through-clone, for CFR branch comparisons ✅
12. **Native MCCFR trainer** — regret tables keyed by `InfoSet`, external
    sampling over the `GameTree` ✅
13. **Blueprint persistence** — serde-derive `InfoSet` / `RegretEntry` /
    `RegretTable`; on-disk envelope (`solver::persistence::BlueprintFile`)
    carries a schema version and an abstraction tag, both checked on load
    so stale tables fail loudly. MessagePack codec (`rmp-serde`) reused
    from `FileSink` rather than introducing a second binary format. ✅
14. **Multi-player correctness pass** — fuzz + targeted tests at 2/3/4/6
    seats covering chip conservation under randomized play, 3-way all-in
    side-pot eligibility bounds, persistent-stack busting at 6-max, and
    a 6-max mixed-agent long-run. Lives in
    `tests/multi_player_correctness.rs`. ✅
15. **CLI training binary** — `poker_train` runs MCCFR for N iterations and
    writes a versioned blueprint; `poker_play` loads one and plays it
    against bots (spec: `blueprint`, `calling`, `random:<seed>`), printing
    the same `print_report` table as `poker_report`. Both live in
    `src/bin/`. Train→save→load→play is a shippable end-to-end loop. ✅
16. **`egui` replay viewer** — `poker-client --replay <path>` loads a
    `FileSink` log (via the new `read_event_log` on the engine) and lets the
    user scrub events with a cursor. Each frame renders a `Snapshot` derived
    from `events[..=cursor]`: hand id / dealer / street / pot, board cards,
    per-seat hole cards + folded/all-in/committed, action log, and the
    `HandResult` block. Ships the first rendering layer; will be reused by
    `poker-client` for live play. `poker_play --log <path>` fans the live
    run through a `TeeSink` so stats and a durable log come from one pass. ✅
17. **In-house opponent dataset** — five-persona bot pool (`Nit`, `Tag`,
    `Lag`, `Maniac`, `TiltProne`) in [agent/personas.rs](crates/poker-engine/src/agent/personas.rs),
    each gating actions on a shared `Strength` bucket (preflop class for
    preflop; best-5-of-7 hand category for postflop). The `dataset` module
    provides `extract_hand_stats(events, seat_class) -> Vec<HandStats>` —
    one row per (hand, live seat) tagged with `own_class` and a sorted
    `opponents_sig`, so rows from different matchups concatenate cleanly.
    `class_conditional` and `windowed` slice that slab. The
    `poker_dataset` binary runs a heads-up round-robin over the pool, prints
    a per-persona summary, the (own × opponent) class-conditional matrix
    (VPIP / PFR / AF / WTSD / chip EV), and rolling-window time series for
    each persona; with `--out <dir>` it also dumps the raw `FileSink` logs
    so every matchup can be replayed in `poker-client`. Future client/server
    traffic captured to the same `FileSink` format slots in unchanged.
    Importing external hand histories is a later option, not a prerequisite. ✅
18. **Exploit layer** — opponent profiling consumer on `EngineEvent`
    using the stat schema validated in step 17, plus best-response mixing
    with the MCCFR blueprint. Mixing weight trades exploitation against
    exploitability. Deferred behind the live client/server work.
19. **`poker-server` + `poker-client`** — companion crates (see above).
    Server hosts hands; client renders them via the step-16 layer. Bots
    connect via the same `Agent` trait, exercised over the wire.
    - **19a — server skeleton.** Wire types in
      `poker_engine::net` (msgpack messages + a sync `[u32 LE len][bytes]`
      framer matching `FileSink`). New `poker-server` crate with a tokio
      TCP listener, async wire helpers, and a per-connection session
      driving the `Hello{username, version}` → `Welcome{player_id, stats}`
      handshake. A `Registry` persists one `<data_dir>/users/<name>.mp`
      record per player so reconnects pick up the same `PlayerId` and
      `LifetimeStats`; an in-memory online set rejects duplicate logins.
      Heartbeat / Disconnect close the loop. Six TCP integration tests
      (handshake, protocol mismatch, invalid name, double-login, heartbeat,
      reconnect-recalls-id) plus five registry unit tests. ✅
    - **19b — game messages.** `ListTables` /
      `JoinTable` / `LeaveTable` / `SubmitAction` from client; matching
      `TableList` / `JoinedTable` / `TableState` / `TableEvent` /
      `Prompt` / `ActionRejected` from server. Per-table actor task
      runs hands sequentially: waits for quorum, snapshots seated
      players, runs `Engine::run_hand` on `spawn_blocking` with one
      `RemoteAgent` per seat. The agent issues a `Prompt` over the
      connection's outbound queue and `block_on`s a `oneshot` that
      `SubmitAction` resolves; missed deadlines fold for the player.
      A `BroadcastSink` fans events out per-recipient: hole cards
      ride to their owner only, and `HandEnded` masks all hole cards
      for non-recipients except at proper showdowns (river dealt with
      ≥2 contenders). Two TCP integration tests cover fold-through
      (no leaks) and check/call-to-showdown (both hands revealed). ✅
    - **19c — live client.** `poker-client` grew a tokio-backed
      `LiveClient` worker thread bridged to egui via paired mpsc
      channels. New `LiveApp` egui state machine: Connecting → Lobby
      (table picker with refresh + buy-in input) → Seated (snapshot
      view, seat list with stacks, action panel that lights up on
      `Prompt`). The seated view reuses the step-16 `Snapshot` /
      `render_snapshot` pipeline, fed event-by-event from inbound
      `TableEvent`s. Action panel issues `SubmitAction` for
      fold/check/call/all-in, plus a min/max-bounded raise slider.
      One in-process smoke test drives the worker through Hello →
      Welcome → ListTables → TableList against the same `ServerContext`
      the 19b TCP tests use. ✅

22. **Project pivot — 2026-04-28.** With the live-client smoke working,
    the project's centre of gravity has shifted from "engine for
    training" to **"poker server, with engine + agent training as
    library dependencies, and a deprecated reference client."** The
    next-generation client will be built as a separate (likely
    non-Rust) project and consumes a documented wire protocol. This
    repo's remaining work is therefore a refactor + server hardening
    pass, captured in steps 20–24 below. Step 19d (per-player stat
    persistence) is subsumed by the broader DB work in step 21c.

23. **Step 20 — Trainer split.** Promote agent-training code out of
    `poker-engine` into a new `poker-trainer` crate so the engine stays
    a focused rules library. Files moved: `solver/` (MCCFR + blueprint
    persistence), `abstraction/` (preflop/postflop bucketing for
    solver keys), `dataset/` (HandStats aggregator + class-conditional
    + windowed views), and the `poker_train` / `poker_play` /
    `poker_dataset` binaries. `agent::personas` and `stats::StatsSink`
    stay in the engine — the server can use personas as table-fillers
    later without pulling in the solver. After the split,
    `poker-engine` re-exports stay unchanged for downstream use of
    rules + sim, and the workspace gains one more member.

24. **Step 21 — Persistence (SQLite via `sqlx`).** Replace the
    file-backed `Registry` with a SQLite-backed user/session/hand
    store under `<data_dir>/poker.sqlite`. Migrations live in
    `crates/poker-server/migrations/` and are run at startup.
    - **21a — Schema + migration.** ✅ Done (2026-04-27).
      Aggressively normalised so OAuth can land later without nulling
      columns. Initial migration provisions every table the later
      sub-steps need so 21b/c don't add new migrations:
        - `users(id PK, email UNIQUE NULL, username UNIQUE NOT NULL,
          created_at)` — identifying info only. `email` is nullable in
          21a (Registry-equivalent flow has no email yet); 21b's
          password-registration path requires it.
        - `user_password(user_id PK FK, password_hash, updated_at)` —
          one row only when the user has a password credential. OAuth
          would land in a sibling table (e.g. `user_oauth(user_id,
          provider, provider_user_id, ...)`).
        - `sessions(id PK, user_id FK, key UNIQUE, device_label,
          created_at, revoked_at NULL)` — append-only; "live session
          for user" = newest non-revoked row. Issuing a new session
          revokes the previous one in the same transaction.
        - `lifetime_stats(user_id PK FK, hands, voluntary_pf,
          raised_pf, aggressive_actions, passive_actions, showdowns,
          chip_delta)` — single-row-per-user aggregate, mapped 1:1 to
          `LifetimeStats`. Replaces the old `<data_dir>/users/<name>.mp`
          file format.
        - `hands(id PK, table_id, started_at, ended_at, log BLOB)` —
          one row per finished hand, payload is the same
          length-prefixed msgpack frames `FileSink` writes. Wired up
          in 21c.
        - `hand_seats(hand_id FK, seat, user_id FK NULL, chip_delta,
          sat_out)` — for analytical queries without re-parsing the
          log. Wired up in 21c.
    - **21b — Authentication.** ✅ Done (2026-04-28). Argon2id
      password hashing (`argon2` crate). New protocol verbs `Register
      { email, username, password, device_label }` and `Authenticate
      { mode: Password { identifier, password } | Session { key },
      device_label }` returning `Welcome { session_key, player_id,
      username, stats }` or `Rejected { reason }`. `identifier`
      accepts email or username. A new login revokes the prior live
      session in the same transaction; the in-memory revoke flag
      (`Arc<AtomicBool>`) flips so the prior connection sends
      `Goodbye { "session_revoked" }` on its next received frame.
      Email is server-side state only — `SeatInfo` and every other
      broadcast continues to ship username only. Bumped
      `PROTOCOL_VERSION` to 3, dropped `ClientMessage::Hello`.
    - **21c — Hand persistence.** ✅ Done (2026-04-28).
      `BroadcastSink` accumulates every emitted `EngineEvent` as a
      `FileSink`-compatible `[u32 LE len][rmp-serde]` byte log
      alongside its per-recipient broadcast. After `Engine::run_hand`
      returns, the table actor calls `Registry::record_hand` which
      writes one `hands` row (table_id, started_at, ended_at, log
      BLOB) and one `hand_seats` row per participating seat
      (chip_delta from `SeatOutcome`, `sat_out` flag forwarded from
      the engine). Subsumes the original step 19d. Lookups via
      `Registry::fetch_hand` / `count_hands`.

25. **Step 22 — Security audit + hardening.** A dedicated pass once
    the persistence layer is live. Sub-steps:
    - **22a — Wire-layer hardening.** ✅ Done (2026-04-28). Three
      concerns covered by [crates/poker-server/tests/security.rs](crates/poker-server/tests/security.rs)
      and [crates/poker-engine/src/net/frame.rs](crates/poker-engine/src/net/frame.rs):
      (1) `parse_length_prefix` rejects any prefix > `MAX_FRAME_BYTES`
      before the server allocates a payload buffer (boundary test
      covers cap, cap+1, and `u32::MAX`); (2) a poor-man's fuzz
      hammers `frame::decode::<ClientMessage>` and
      `frame::decode::<ServerMessage>` with 2000 deterministic random
      payloads — must always return Ok or Err, never panic; (3) the
      server's pre-existing `LegalActions::is_legal` re-validation in
      `Connection::deliver_action` is now exercised end-to-end by a
      forged-raise test that submits `Raise(max_raise + 1)` over the
      wire and asserts `ActionRejected`.
    - **22b — Connection lifecycle.** ✅ Done (2026-04-28).
      [crates/poker-server/src/limits.rs](crates/poker-server/src/limits.rs)
      defines `ConnectionLimits { idle_timeout, rate_burst,
      rate_refill_per_sec }` (defaults: 60s / 30 burst / 20 per sec)
      and a `TokenBucket`. The handshake read and the reader loop are
      both wrapped in `tokio::time::timeout(idle_timeout, …)`; on
      idle the post-handshake side queues
      `Goodbye { reason: "idle timeout" }`. Each post-handshake
      frame consumes one token; over-budget peers get
      `Goodbye { reason: "rate limit exceeded" }` and a teardown.
      Tests: `idle_handshake_drops_connection` and
      `flood_triggers_rate_limit_goodbye` in
      [crates/poker-server/tests/security.rs](crates/poker-server/tests/security.rs).
    - **22c — Reconnect / duplicate-session handling.** ✅ Done
      (2026-04-28). The seat now holds an `Arc<SeatLink>` (a
      `RwLock<Arc<Connection>>` indirection in
      [crates/poker-server/src/connection.rs](crates/poker-server/src/connection.rs))
      which is shared by `Table::Seat`, the per-hand snapshot,
      `RemoteAgent`, and `BroadcastSink`. On a second login for the
      same user, `Table::reconnect_player` swaps the link in place,
      moves any in-flight `PendingAction` (with its `oneshot::Sender`)
      from the old connection to the new one, and re-issues a
      matching `Prompt` over the new socket — so the engine's blocked
      `act()` resolves on the new device and the hand keeps going.
      Each `Connection` carries its `session_id`; the cleanup path on
      a superseded session skips `force_leave`/`record_leave` so it
      can't kick the seat out from under the new owner. Test:
      `reconnect_takes_over_seat_mid_hand` in
      [crates/poker-server/tests/security.rs](crates/poker-server/tests/security.rs).
      Known limitation: the current hand's `HoleCardsDealt` is not
      replayed to the reconnecting client, so they play the rest of
      the hand with cards face-down on their UI; full state-replay
      is deferred to step 23 alongside the protocol spec.

26. **Step 23 — `PROTOCOL.md`.** ✅ Done (2026-04-29). Long-form
    functional spec at
    [crates/poker-engine/src/net/PROTOCOL.md](crates/poker-engine/src/net/PROTOCOL.md),
    sufficient for a non-Rust client to be built without reading
    server source. Covers: framing (`[u32 LE length][msgpack bytes]`,
    `MAX_FRAME_BYTES` cap), handshake state machine, every
    `ClientMessage` / `ServerMessage` variant with field-by-field
    semantics, ordering guarantees (e.g. `JoinedTable` always precedes
    the first `TableState`; per-recipient hole-card masking on
    `HandEnded`), error model (`Rejected` vs `ActionRejected` vs
    `Goodbye`), idle/rate-limit policy, reconnect / mid-hand seat
    takeover, and a msgpack schema appendix per variant. Versioned in
    lockstep with `PROTOCOL_VERSION` (currently 3).

27. **Step 24 — Deprecate `poker-client` & refocus docs.** ✅ Done
    (2026-04-29). `crates/poker-client/README.md` is now explicitly
    flagged "frozen at `PROTOCOL_VERSION = 3`, only protocol-
    compatibility bug fixes" with a pointer to the sibling client
    repo. Per-crate READMEs (engine, trainer, server, client) own
    their own specifics; the root `README.md` carries cross-cutting
    concerns (end-to-end loop, inter-crate contracts, workspace
    invariants, roadmap). The next-generation playable client is
    being developed in a **separate sibling repository** and is
    expected to land here later as a git **submodule** — at which
    point a new step will be added covering the wiring (location
    under the workspace, build hooks, `cargo`/sibling-toolchain
    interop). Until then, references to "the client" in this repo
    mean the deprecated in-tree harness.

This concludes the originally-planned step list. Steps 25+ below
were added 2026-04-29 after a project-direction shift on the client
side.

## Phase 2 — Rust client rewrite + supporting server work

Decided 2026-04-29: the Flutter client experiment in the
`ssh://joscode.com/home/joseph/git/poker-client` repo is being
treated as a failed experiment (intertwined UI + state logic, poor
framework fit, bug-prone, slow progress). Lessons learned applied
to a new architecture: **Rust state machine core + transport layer
+ headless test harness in-tree**, **SolidJS / Tauri 2 shell**
back in the `client/` submodule (which gets force-reset; the old
state lives on as the `flutter-experiment` tag in that repo).

The three load-bearing principles that shape this phase live at
[docs/CLIENT_PRINCIPLES.md](docs/CLIENT_PRINCIPLES.md): strict
correctness with full-flow tests, thin UI projecting state, and
resilience to interruption via server-driven resync.

28. **Step 25 — Client architecture rewrite.** Multi-crate setup:
    - **25a — `poker-client-core`.** Pure synchronous state machine.
      Inputs: `Intent` (user actions) + inbound `ServerMessage`.
      Outputs: `Effect` stream (`Send`, `PersistSessionKey`,
      `OpenConnection`, `Log`, …) and a queryable `ClientView`
      projection. No `tokio`, no I/O, no platform code. Depends only
      on `poker-engine` for wire types. Every state transition unit-
      tested.
    - **25b — `poker-client-transport-native`.** ✅ Done (2026-04-29).
      Tokio TCP transport + filesystem session-key persistence.
      Three layers inside the crate:
      [transport.rs](crates/poker-client-transport-native/src/transport.rs)
      defines a small factory-style `Transport` trait plus the
      channel-backed `TransportHandle` (`mpsc` for both directions,
      inbound carries a `TransportIn::{Connected, Message, Lost}`
      enum); a future `poker-client-transport-browser` implements
      the same trait and returns the same handle shape, keeping
      the core agnostic of platform.
      [native.rs](crates/poker-client-transport-native/src/native.rs)
      is the `NativeTransport` impl — `TcpStream::connect` +
      `set_nodelay` + a pump task that forwards framed messages
      both ways until either side EOFs.
      [session.rs](crates/poker-client-transport-native/src/session.rs)
      is `SessionStore` — atomic write-then-rename session-key
      persistence, creates parent dirs lazily, empty-file-is-None
      semantics on load.
      [runtime.rs](crates/poker-client-transport-native/src/runtime.rs)
      is the `NativeClient` facade the Tauri shell consumes: spawns
      a dedicated worker thread with its own current-thread tokio
      runtime, owns the `ClientCore` + `Transport` + `SessionStore`,
      exposes sync `issue(Intent)` / `snapshot() -> ClientView` /
      `drain_logs()` to any thread. All effects are routed
      internally (`OpenConnection`/`CloseConnection`/`Send` →
      transport, `PersistSessionKey` → store, `Log` → `tracing` +
      host buffer). `Effect::Schedule` is a stub for now — the
      server's 60s idle timeout leaves slack; heartbeat / prompt
      deadline ticks will land with a follow-up. Tests: loopback
      roundtrip (`roundtrip_against_echo_server`), connect failure
      (`open_failure_reports_connect_error`), graceful handle drop
      (`handle_drop_tears_down_pump`), session store roundtrip /
      empty-file / missing-parent, and an end-to-end integration
      test at [tests/runtime.rs](crates/poker-client-transport-native/tests/runtime.rs)
      that stands up an in-process `poker-server` and drives a
      `NativeClient` through `Connect → Register → Welcome` with
      assertions on view phase, buffered logs, and the persisted
      session-key file.
    - **25c — `poker-client-headless`.** ✅ Done (2026-04-29).
      Three layers inside the crate:
      [harness.rs](crates/poker-client-headless/src/harness.rs) is
      `HeadlessClient` — wraps [`ClientCore`] with effect and
      snapshot logs; scripts inject intents and inbound
      `ServerMessage`s directly, bypassing any transport. The pure-
      state-machine tier.
      [transport.rs](crates/poker-client-headless/src/transport.rs)
      is `InMemoryTransport` — implements
      `poker_client_transport_native::Transport`, so
      `NativeClient::with_transport(transport, session)` works
      against fully in-memory channels. The paired
      `InMemoryConnections` listener hands out one `InMemoryServer`
      per `open()`; tests use that to script the other side of the
      wire (`server.send(ServerMessage::...)`, `server.close(reason)`,
      `server.recv()` for outbound `ClientMessage`s). Required a new
      `TransportHandle::from_channels` constructor in
      `poker-client-transport-native` so out-of-crate `Transport`
      implementors can build a handle.
      [scenario.rs](crates/poker-client-headless/src/scenario.rs)
      is the scripted-scenario DSL: `Step::{Issue, Receive,
      ExpectPhase, Expect{label, predicate}}`, a plain `Vec<Step>`
      `Script`, a `Driver::run` that pumps + records snapshots and
      stops at the first failed expectation (returning a
      `DriverResult { snapshots, failure }`), and
      `Driver::run_and_assert` for panic-on-fail tests. Predicates
      are `Arc<dyn Fn(&ClientView) -> bool + Send + Sync>` so tests
      can close over local state. For CLI reproduction files the
      serde-derived `FileStep` is a subset (`Issue` + `Receive`
      only) with `load_file_script` / `save_file_script` helpers.
      `Intent` in `poker-client-core` gained `Serialize` +
      `Deserialize` derives to make that file format round-trip.
      [main.rs](crates/poker-client-headless/src/main.rs) is the
      CLI: `poker-client-headless <scenario.json>` replays a
      `FileScript`, printing a one-line summary per step (issued
      intent / received message name, resulting phase, seat /
      hand / table state). Tests: 9 unit tests covering
      `HeadlessClient`, the scenario driver (linear success,
      stop-at-first-failure, panic-message shape, file-script
      round-trip, predicate discard), and `InMemoryTransport`
      pair behaviour; 2 integration tests at
      [tests/in_memory_runtime.rs](crates/poker-client-headless/tests/in_memory_runtime.rs)
      drive `NativeClient` + `InMemoryTransport` end-to-end
      through a scripted Register → Welcome → Lobby and an
      abrupt-server-close → `Phase::Ended` flow.
    - **25d — `client/` Tauri shell.** ✅ Scaffold done (2026-04-29).
      The `client/` submodule (reset to a blank repo by the user
      earlier in the day; the Flutter experiment lives on as the
      `flutter-experiment` tag) is now a Tauri 2 + SolidJS + Vite +
      TypeScript + pnpm app. Layout:
        - `client/src-tauri/` — Rust glue. Standalone Cargo project
          (opt-out of the parent workspace via an empty `[workspace]`
          table) with path deps on `poker-client-core` and
          `poker-client-transport-native`. `src/state.rs` owns a
          single `AppState { client: Arc<NativeClient> }`, built
          against a session key file under Tauri's
          `app_data_dir` / `session.key`. A background tokio task
          (`spawn_event_pump`) polls `NativeClient::snapshot` on a
          20 ms cadence and emits `snapshot_changed` +  `log_entry`
          Tauri events — the 20 ms interval is the animation
          substrate (principle 2 permits up to a 1 s stale-state
          budget), well above UI frame time. `src/commands.rs`
          exposes three `#[tauri::command]`s:
          `get_snapshot` → `ClientView` (initial seed),
          `issue_intent(Intent)` → `Result<(), String>`,
          `drain_logs() -> Vec<LogEvent>`. Required adding serde
          derives on `Phase`, `CurrentHand`, and `ClientView` in
          `poker-client-core` so Tauri's IPC can ship them directly.
          Integration test at
          [client/src-tauri/tests/link.rs](client/src-tauri/tests/link.rs)
          is a one-liner smoke that the path deps resolve (`cargo
          test --manifest-path client/src-tauri/Cargo.toml` runs
          green).
        - `client/src/` — SolidJS frontend. `types.ts` carries
          hand-maintained TS mirrors of `ClientView` / `Intent` /
          `Action` / `Phase` (serde's default enum representation:
          unit variants are bare strings, payload variants are
          single-key objects). `bridge.ts` wraps the three Tauri
          commands + two events. `store.ts` is a SolidJS
          `createStore<ClientView>` with a bounded inbound queue
          (cap 50 — equal to the 1 s @ 20 ms budget) and a 500-
          entry log ring. `views/` has four screens that project
          `view`: `Connect` (register / password form),
          `Lobby` (table list + join), `Seated` (seat list, board +
          hole cards rendered from raw `Card` byte indices, action
          panel that lights up only on `awaiting_action`), `Ended`.
          No local game state — every render is `ClientView` → DOM.
        - `client/README.md` spells out the run loop
          (`pnpm install` → `pnpm tauri dev` + `cargo run -p
          poker-server` in the parent).
      This is the functional-loop MVP: connect, register, lobby,
      seat, submit actions. No card animations, no polish, no
      mid-hand replay (that lands with DESIGN step 26). TS
      typechecks; Vite builds a 30 KB gzipped bundle; Rust side
      compiles + test passes; parent workspace's 250+ tests still
      green.

29. **Step 26 — Server-side hand replay (protocol v4).** Required by
    principle 3. The server gets a new client verb, working title
    `RequestReplay { hand_id }`, that replies with the full ordered
    `EngineEvent` stream for the in-flight hand as the requesting
    seat would have seen it (their hole cards visible, others
    masked). Bumps `PROTOCOL_VERSION` to 4 and invalidates v3
    clients; the in-tree deprecated `crates/poker-client` doesn't
    get updated — it's a v3 harness for the server's v3 codepath
    until that codepath is removed. PROTOCOL.md gets a new
    sub-section under §6 covering the verb's preconditions
    (recipient must be seated at the named hand), guarantees
    (events are byte-identical to what the seat originally received),
    and ordering relative to subsequent live broadcasts.

30. **Step 27 — Mid-hand persistence atomicity.** Verify and document
    that `Registry::record_hand` only fires at `HandEnded`, that no
    chip movement is reflected in `lifetime_stats` or `hands` /
    `hand_seats` until then, and that a server restart mid-hand
    leaves the database in a state indistinguishable from "the hand
    never started." Add a stress test that crashes the server-task
    mid-hand and asserts no SQLite mutation occurred. Likely a
    documentation-only step (the current implementation already
    holds the invariant), but pinned with a regression test so a
    future "incremental stat update" optimisation can't silently
    break it.
