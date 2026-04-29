# poker

A Cargo workspace for a high-performance Texas Hold'em **rules engine**, an **agent trainer** built on top, and a **TCP poker server** that lets remote clients play hands. An `egui` reference client is shipped alongside as a smoke-testing harness; the next-generation playable client is being developed in a separate repository and is planned to land here later as a git submodule.

## The four pieces

| Crate | Role | Status |
|---|---|---|
| [crates/poker-engine](crates/poker-engine/) | Pure-Rust rules library: cards, evaluator, game state, betting, agents, sim, stats, wire types | Stable |
| [crates/poker-trainer](crates/poker-trainer/) | MCCFR solver, dataset aggregator, `poker_train` / `poker_play` / `poker_dataset` CLIs | Stable |
| [crates/poker-server](crates/poker-server/) | Async TCP server wrapping `Engine::run_hand` for live play | **Active focus** |
| [crates/poker-client](crates/poker-client/) | `egui` replay viewer + reference live client | Deprecated; in-tree as a smoke-test harness, frozen at `PROTOCOL_VERSION = 3`. Real client is a sibling repo (planned submodule). |

Each crate has its own README with the specifics. The rest of this document is the cross-cutting view — how the pieces fit together, what contracts they share, and how to drive the workspace from end to end.

## End-to-end loop

```sh
# 1. Train a blueprint
cargo run --release -p poker-trainer --bin poker_train -- \
    --iters 200000 --out blueprint.mp --seed 7

# 2. Play it against bots and capture an event log
cargo run --release -p poker-trainer --bin poker_play -- \
    --blueprint blueprint.mp --hands 5000 \
    --agents blueprint,calling --log session.mp

# 3. Replay the session visually
cargo run --release -p poker-client -- --replay session.mp

# 4. Or play live against a server
cargo run -p poker-server &
cargo run -p poker-client -- --connect 127.0.0.1:7878 --username alice
```

For training-specific flags see the [trainer README](crates/poker-trainer/), for server flags the [server README](crates/poker-server/).

## Quick stats run (`poker_report`)

For a fast "what does this matchup look like over N hands" without bothering with a blueprint, the engine ships a single CLI:

```
cargo run --release -p poker_engine -- [OPTIONS]

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

## Inter-crate contracts

Three shared formats are the load-bearing seams. Touching any of them is a workspace-wide concern.

### `EngineEvent` stream

Every meaningful moment in a hand emits an event: `HandStarted` / `HoleCardsDealt` / `BoardDealt` / `ActionTaken` / `PlayerAllIn` / `HandEnded`. The engine itself stores nothing; events flow into whatever `EventSink` the caller supplies.

| Sink | Used by |
|---|---|
| `NullSink` | Bulk training / report runs |
| `VecSink` | Tests, in-memory replay capture |
| `FileSink` | `poker_play --log`, `poker_dataset --out`, replay viewer input |
| `StatsSink` | `poker_report`, `poker_play` |
| Server-side `BroadcastSink` | `poker-server` (per-recipient hole-card masking) |

The same event stream feeds the [snapshot derivation](crates/poker-client/src/snapshot.rs) used by both replay and live modes of `poker-client`.

### `FileSink` log format

`[u32 LE length][rmp-serde EngineEvent bytes]` frames, written through `BufWriter`, flushed on drop. Round-trip with `read_event_log`. Streaming-friendly: a server can spool live broadcasts straight to disk without re-encoding.

### Wire protocol

Defined once in [crates/poker-engine/src/net/](crates/poker-engine/src/net/) and linked by both `poker-server` and `poker-client`. Same `[u32 LE length][rmp-serde bytes]` framing as `FileSink`, but carrying `ClientMessage` / `ServerMessage` envelopes instead of bare `EngineEvent`s. `PROTOCOL_VERSION` (currently 3) is bumped on any backwards-incompatible change. The long-form spec — sufficient to build a non-Rust client without reading server source — lives at [crates/poker-engine/src/net/PROTOCOL.md](crates/poker-engine/src/net/PROTOCOL.md).

## Workspace-wide invariants

These apply across every crate:

- **Determinism.** A hand's `deck_seed` (in `HandStarted`) is the only value needed to reproduce the deal. Agent seeds are passed at construction and never touched by the engine. Don't pull entropy inside the engine; thread it through.
- **Integer chips.** Chip values are `u32`, denominated in big-blind units by convention (no enforced denomination). Split-pot remainders go to the first eligible winner left of the dealer. `SeatOutcome::chip_delta` is `i32`. No floating-point arithmetic anywhere.
- **Hole-card visibility.** `HoleCardsDealt` is per-seat private. `HandEnded` reveals hole cards only at a proper showdown (river dealt + ≥2 non-folded contenders) and even then only for non-folded seats. The server's `BroadcastSink` enforces this per-recipient; the engine itself never strips them.

## Roadmap

Step plan and status live in [DESIGN.md](DESIGN.md). At a glance:

| Step | Status |
|---|---|
| 1–17 (engine, sim, stats, MCCFR, blueprints, dataset, replay viewer) | ✅ |
| 18 (persona bot pool + dataset aggregator) | ✅ |
| 19a–c (TCP server skeleton + game messages + live client) | ✅ |
| 20 (trainer split — this crate's birth) | ✅ |
| 21a (SQLite schema + migration + Registry replacement) | ✅ |
| 21b (Argon2id auth + session keys, `Authenticate` verb) | ✅ |
| 21c (Per-hand persistence into `hands` + `hand_seats`) | ✅ |
| 22a (wire-layer hardening + LegalActions forgery test) | ✅ |
| 22b (idle timeout + per-connection rate limit) | ✅ |
| 22c (reconnect mid-hand) | ✅ |
| 23 (`PROTOCOL.md` long-form spec) | ✅ |
| 24 (deprecate `poker-client` and refocus docs) | ✅ |

Steps 1–24 cover everything originally planned for the in-tree workspace. Future work — most prominently importing the sibling client repo as a submodule and any post-launch server hardening — will be appended as new steps when the time comes.

## Tests

```sh
cargo test --workspace
cargo bench -p poker_engine
```
