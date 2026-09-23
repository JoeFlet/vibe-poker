# Poker Workspace — Design

A mile-high view of the current and planned design. This is the map, not the
territory: each crate carries its own in-depth README, the wire protocol has a
long-form spec, normative requirements live in OpenSpec, and the bleeding edge
(work in flight, known breakage, next steps) lives in [FRONTIER.md](FRONTIER.md).

| Concern | Authority |
|---|---|
| Shape of the whole project (this doc) | `DESIGN.md` — terse, current + planned |
| In-depth per-module design | each crate's `README.md` |
| Wire protocol | [crates/poker-engine/src/net/PROTOCOL.md](crates/poker-engine/src/net/PROTOCOL.md) |
| Normative subsystem requirements | [openspec/specs/](openspec/specs/) — read before changing behavior |
| Current edge / next work | [FRONTIER.md](FRONTIER.md) |
| Build & test commands, crate map | [AGENTS.md](AGENTS.md) |

## Goals

- **High-throughput simulation** — millions of hands/sec for AI training.
- **Clean AI boundary** — plug-in agents behind a simple action/observation API.
- **Live play** — an authoritative TCP server hosting real hands, plus a
  desktop client.
- **Correctness over cleverness at the boundary** — a fast, pure core with a
  safe, well-tested surface.
- **Texas Hold'em first** — heads-up and 6-max, with a swappable ruleset
  boundary (`BettingRules`, `Street`, action validation) left open to variants.

## Non-Goals

- Real-money handling.
- Exhaustive game-variant support at v1.
- A multi-player *trainer* — CFR gives only correlated (not Nash) equilibria at
  3+ players; multi-player strength is the exploit layer's job (see below).

## Architecture

The workspace is layered: a pure rules library at the bottom, a trainer and a
server built on it, and a client stack (pure state machine + transport +
headless harness + desktop shell) beside the server.

```
        ┌───────────────────────────── client/ (Tauri + SolidJS shell) ──┐
        │  poker-client-headless   poker-client-transport-native          │
        │            └──────────────┬──────────────┘                      │
        │                   poker-client-core (pure state machine)        │
        └───────────────────────────┬────────────────────────────────────┘
                                     │ net wire types
  poker-trainer ── poker-server ────┴──── poker-engine (rules + net + events)
   (MCCFR)          (live host)                 core · game · agent · sim
```

| Crate | Role | In-depth doc |
|---|---|---|
| `poker-engine` | Pure rules: cards, evaluator, game state, betting, agents, sim, stats, wire types | [README](crates/poker-engine/README.md) |
| `poker-trainer` | MCCFR solver, blueprint persistence, dataset tools, CLIs | [README](crates/poker-trainer/README.md) |
| `poker-server` | Async TCP host, SQLite persistence, Argon2id auth, reconnect | [README](crates/poker-server/README.md) |
| `poker-client-core` | Synchronous `Intent → Effect` state machine, no I/O | [README](crates/poker-client-core/README.md) |
| `poker-client-transport-native` | Tokio TCP transport + session-key store + `NativeClient` | [README](crates/poker-client-transport-native/README.md) |
| `poker-client-headless` | `InMemoryTransport`, scripted scenario driver, CLI replayer | [README](crates/poker-client-headless/README.md) |
| `tests/` | Workspace-root real-TCP full-flow tests | [README](tests/README.md) |
| `client/` (submodule) | Tauri 2 + SolidJS desktop shell; not in the parent Cargo workspace | [README](client/README.md) |

## Engine (`poker-engine`)

The pure foundation: no I/O, no async. Deterministic given seeds.

- **Core.** Cards are packed `u8` (`rank<<2 | suit`, 0–51); hands are `u64`
  bitmasks for O(1) set ops. `RsPokerEvaluator` does 7-card evaluation via
  lookup tables (~50 ns); `NaiveEvaluator` cross-checks it. Deck is
  Fisher-Yates over a fast seeded PRNG.
- **Game.** `GameState` is a cheap-to-clone hand snapshot (board, per-seat
  stack/hole/bet/status, `Pot` with explicit side pots, action-on).
  `BettingRules` (variant, blinds, ante, straddle) is passed at construction.
  `Action` is `Fold | Check | Call | Raise(u32) | AllIn`; the engine validates
  and hands agents a `LegalActions`. TDA edge cases (short all-ins, dead-hand
  blinds, canonical BB first-to-act) live here and are inherited everywhere.
- **Agents & sim.** The whole AI contract is `Agent::act(&Observation) ->
  Action`, plus no-op lifecycle hooks (`on_run_start/end`, `on_hand_start/end`)
  for durable/transient state. `Observation` exposes only what a seat may know —
  the information-hiding boundary. `SimRunner` runs N hands single-threaded or
  across a thread pool; built-in agents: `RandomAgent`, `CallingStation`,
  `ScriptedAgent`, `HumanAgent`, and the five personas (`Nit`, `Tag`, `Lag`,
  `Maniac`, `TiltProne`).
- **Events & determinism.** Every meaningful moment emits an `EngineEvent`
  (`HandStarted` / `HoleCardsDealt` / `BoardDealt` / `ActionTaken` /
  `PlayerAllIn` / `HandEnded`). The engine stores nothing — events flow into a
  caller-supplied `EventSink` (`NullSink`, `VecSink`, `FileSink`, `StatsSink`,
  and the server's `BroadcastSink`). `FileSink` framing is
  `[u32 LE length][rmp-serde bytes]`, reused for wire messages and SQLite hand
  logs. `deck_seed` (in `HandStarted`) is the only value needed to reproduce a
  deal; agent seeds are separate and opaque to the engine.
- **GameTree.** A pausable single-hand stepper (Decision → … → Terminal) that
  reuses the same engine helpers as `run_hand`, so it is bug-compatible. Cloning
  the tree clones the deck (chance-through-clone determinism), which is what lets
  MCCFR compare action branches apples-to-apples.

## Trainer (`poker-trainer`)

Split out of the engine so the engine stays a focused rules library. The lower
strategy layer produces a near-GTO blueprint; the upper (planned) layer
exploits opponent models on top of it.

- **MCCFR.** External-sampling Monte Carlo CFR over `GameTree`, **heads-up
  only**. Regret tables are keyed by `InfoSet = (street, relative_position,
  bucket, action_history)`, rotation-invariant so symmetric decisions share
  storage. `BlueprintStrategy` normalises the time-averaged strategy and plugs
  back into the engine's `Agent` via `StrategyAdapter` — the single bridge
  between the solver's abstract world and concrete engine actions.
- **Abstraction.** Cards: preflop is the 169-class suit-isomorphic canonical
  form (complete); postflop buckets are **planned** (currently all share
  `bucket = 0`, so postflop play is weak). Actions: `Fold`, `Call`, pot-sized
  `Raise`, `AllIn` — a `#[repr(u8)]` enum that widens by one dimension when
  richer sizings (half-pot, two-pot) are added.
- **Why heads-up only.** CFR converges to a correlated (not Nash) equilibrium at
  3+ players. Modern systems (Pluribus etc.) train a heads-up blueprint and add
  depth-limited real-time search + opponent modeling at the table. That is the
  shape the exploit layer will take; a multi-player trainer first would be
  wasted motion.
- **Dataset.** `extract_hand_stats` / `class_conditional` / `windowed` turn any
  `FileSink` log (including server-recorded hands) into per-hand stat rows. The
  `poker_dataset` CLI round-robins the persona pool and emits summaries.
- **Exploit layer (planned).** GTO blueprint as the anchor, opponent-specific
  best-response as a correction. A passive `EngineEvent` consumer builds a
  per-seat model (VPIP/PFR/AF + conditionals) on the `StatsSink` substrate;
  at decision time the blueprint's `ActionProbs` are mixed with a best-response
  distribution, the weight trading exploitation against exploitability. Slots in
  as another `StrategyAgent` with no engine change. Requires postflop
  abstraction and an exploitability/best-response solver first.

## Server (`poker-server`)

Authoritative live host wrapping `Engine::run_hand` in tokio.

- **Sync→async bridge.** A per-table actor waits for quorum, snapshots seated
  players, and runs `Engine::run_hand` on `spawn_blocking`. Each seat gets a
  `RemoteAgent` whose sync `act()` sends a `Prompt` and blocks on a `oneshot`
  that the next `SubmitAction` resolves; missed deadlines auto-fold.
- **Broadcast masking.** `BroadcastSink` fans events per-recipient:
  `HoleCardsDealt` rides only to its owner, and `HandEnded` reveals hole cards
  only at a proper showdown (river dealt, ≥2 contenders).
- **Reconnect & leave.** A `SeatLink` indirection lets a second login for the
  same user take over a seat mid-hand, moving any in-flight prompt to the new
  socket. `RequestReplay` re-sends a seat's masked event stream so a reconnecting
  client recovers its hole cards. `LeaveTable` (and graceful close) during a hand
  immediately force-folds the seat rather than stalling on its prompt.
- **Persistence.** SQLite via `sqlx` under `<data_dir>/poker.sqlite` (migrations
  applied at startup): `users` / `user_password` (Argon2id) / `sessions`
  (append-only; new login revokes the prior) / `lifetime_stats` / `hands` (log
  BLOB) / `hand_seats`. Chip movement is persisted only at `HandEnded`, so a
  crash mid-hand leaves the DB indistinguishable from "hand never started."
- **Security.** Frame-size cap before allocation, per-connection idle timeout,
  and a token-bucket rate limit.

## Client stack

A failed Flutter experiment (intertwined UI + state logic) motivated the current
split: a pure Rust core, a platform transport, a headless harness, and a thin
shell. Three load-bearing principles:

1. **Strict correctness.** The core is a deterministic state machine — the same
   `(Intent | ServerMessage)` sequence always yields the same `Vec<Effect>` and
   `ClientView`. No `Instant`, no clock, no randomness inside it; timing goes
   through `Effect::Schedule` so tests drive it. Full-flow tests over real TCP
   guard the contract.
2. **Thin UI.** `ClientView` is the whole authoritative client state; the UI is
   a pure projection of it and keeps no parallel copy.
3. **Resilience to interruption.** Recovery is server-driven resync
   (`RequestReplay`, `TableState`), not client-side reconstruction.

Layers: [`poker-client-core`](crates/poker-client-core/README.md) is the pure
`Intent → Effect` / `ServerMessage → ClientView` machine;
[`poker-client-transport-native`](crates/poker-client-transport-native/README.md)
routes effects to a tokio TCP transport + filesystem session store and exposes
the sync `NativeClient` facade; [`poker-client-headless`](crates/poker-client-headless/README.md)
provides `InMemoryTransport` and a scripted scenario driver for tests; and the
[`client/`](client/README.md) submodule is the Tauri 2 + SolidJS shell that
consumes `NativeClient` and renders `ClientView`.

## Cross-cutting invariants

- **Determinism.** `deck_seed` alone reproduces a deal. Don't pull entropy
  inside the engine; thread it through.
- **Integer chips.** Chip values are `u32`, BB-denominated; `chip_delta` is
  `i32`. Split-pot remainders go to the first eligible winner left of the dealer.
  No floating point anywhere in the game layer.
- **Hole-card visibility.** `HoleCardsDealt` is per-seat private; `HandEnded`
  reveals hole cards only at a proper showdown. The server's `BroadcastSink`
  enforces this per recipient.
- **One wire format.** `[u32 LE length][rmp-serde bytes]` for `FileSink` logs,
  SQLite hand-log BLOBs, and the network protocol alike. Structs serialize as
  positional arrays (no field names on the wire); `PROTOCOL_VERSION` bumps on any
  incompatible change.

## Performance targets

| Operation | Target |
|---|---|
| Hand evaluation (7-card) | < 50 ns |
| Full hand simulation (2 players) | < 500 ns |
| Bulk simulation (parallel, 8 cores) | > 5M hands/sec |
| Game-state clone | < 20 ns |
| Action validation | < 10 ns |

Hot-path rules: no heap allocation during a hand, no virtual dispatch in the
core (generics/monomorphisation), no floating point, no locking on game state
(each sim thread owns its own).

## References

| Project | Role |
|---|---|
| [PokerHandEvaluator](https://github.com/HenryRLee/PokerHandEvaluator) / [rs_poker](https://crates.io/crates/rs_poker) | Hand-evaluation reference / back-end |
| [OpenSpiel](https://github.com/deepmind/open_spiel) | CFR solver + game-theory baselines |
| [RLCard](https://github.com/datamllab/rlcard) | RL environment reference |
| [poker_ai (fedden)](https://github.com/fedden/poker_ai) | Pluribus MCCFR reference |
