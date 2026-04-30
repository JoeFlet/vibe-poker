# poker

A Cargo workspace for a high-performance Texas Hold'em **rules engine**, an **MCCFR agent trainer** built on top, and a **live TCP poker server**. The playable desktop client is split between in-tree Rust crates (state machine + transport + headless test harness) and a SolidJS / Tauri 2 shell in the `client/` git submodule.

Architectural principles for the client live at [docs/CLIENT_PRINCIPLES.md](docs/CLIENT_PRINCIPLES.md).

## Crates at a glance

| Crate | Role | Status |
|---|---|---|
| [crates/poker-engine](crates/poker-engine/) | Pure-Rust rules library: cards, evaluator, game state, betting, agents, sim, stats, wire types | Stable |
| [crates/poker-trainer](crates/poker-trainer/) | MCCFR solver, dataset aggregator, `poker_train` / `poker_play` / `poker_dataset` CLIs | Stable |
| [crates/poker-server](crates/poker-server/) | Async TCP server — `Engine::run_hand` over the wire, SQLite persistence, Argon2id auth, reconnect | Stable |
| [crates/poker-client-core](crates/poker-client-core/) | Pure synchronous state machine — `Intent` → `Effect`, no I/O, every transition unit-tested | Stable |
| [crates/poker-client-transport-native](crates/poker-client-transport-native/) | Tokio TCP transport + filesystem session-key persistence; `NativeClient` runtime facade | Stable |
| [crates/poker-client-headless](crates/poker-client-headless/) | `HeadlessClient`, `InMemoryTransport`, scripted scenario driver, CLI scenario replayer | Stable |
| [client/](client/) (submodule) | Tauri 2 + SolidJS + Vite + TypeScript desktop shell | Active |
| [crates/poker-client](crates/poker-client/) | `egui` replay viewer + deprecated live client | **Deprecated** — frozen at `PROTOCOL_VERSION = 3`, smoke-test harness only |
| [tests/](tests/) | Workspace-root full-flow tests (server in-process + headless client over real TCP) | Active |

Each crate has its own README with the specifics; this document is the cross-cutting view.

## Quick start: play live

Start the server, then launch the desktop client from the submodule:

```sh
# Terminal 1 — server (binds 127.0.0.1:7878 by default)
cargo run -p poker-server

# Terminal 2 — desktop client
cd client
pnpm install        # first time only
pnpm tauri dev      # Vite dev server + Tauri window
```

Enter `127.0.0.1:7878` in the Connect form, register a username, join the table. A second player can connect from another terminal / machine.

## End-to-end training + replay loop

```sh
# 1. Train a blueprint (MCCFR heads-up, 200 000 iterations)
cargo run --release -p poker-trainer --bin poker_train -- \
    --iters 200000 --out blueprint.mp --seed 7

# 2. Play blueprint vs calling-station, write event log
cargo run --release -p poker-trainer --bin poker_play -- \
    --blueprint blueprint.mp --hands 5000 \
    --agents blueprint,calling --log session.mp

# 3. Replay in the deprecated egui viewer (still functional)
cargo run --release -p poker-client -- --replay session.mp
```

For training-specific flags see the [trainer README](crates/poker-trainer/); for server flags the [server README](crates/poker-server/).

## Quick stats run (`poker_report`)

For a fast per-seat stat table over N simulated hands without training a blueprint:

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

Every meaningful moment in a hand emits an event: `HandStarted` / `HoleCardsDealt` / `BoardDealt` / `ActionTaken` / `PlayerAllIn` / `HandEnded`. The engine stores nothing; events flow into whatever `EventSink` the caller supplies.

| Sink | Used by |
|---|---|
| `NullSink` | Bulk training / report runs |
| `VecSink` | Tests, in-memory capture |
| `FileSink` | `poker_play --log`, `poker_dataset --out`, replay viewer input |
| `StatsSink` | `poker_report`, `poker_play` |
| `BroadcastSink` (server-side) | `poker-server` — fans events to each client with hole-card masking |

### `FileSink` log format

`[u32 LE length][rmp-serde EngineEvent bytes]` frames written through `BufWriter`. Round-trip with `read_event_log`. The server uses the same framing for hand-log BLOBs in SQLite, so a recorded hand can be replayed unchanged.

### Wire protocol

Defined once in [crates/poker-engine/src/net/](crates/poker-engine/src/net/) and linked by `poker-server`, `poker-client-core`, and the Tauri glue in `client/src-tauri/`. Same `[u32 LE length][rmp-serde bytes]` framing as `FileSink`, but carrying `ClientMessage` / `ServerMessage` envelopes. `PROTOCOL_VERSION` is currently **3**; bumped on any backwards-incompatible change. Long-form spec at [crates/poker-engine/src/net/PROTOCOL.md](crates/poker-engine/src/net/PROTOCOL.md).

## Workspace-wide invariants

- **Determinism.** A hand's `deck_seed` (in `HandStarted`) is the only value needed to reproduce the deal. Agent seeds are passed at construction. Don't pull entropy inside the engine; thread it through.
- **Integer chips.** Chip values are `u32`, BB-denominated. Split-pot remainders go to the first eligible winner left of the dealer. `SeatOutcome::chip_delta` is `i32`. No floating-point arithmetic.
- **Hole-card visibility.** `HoleCardsDealt` is per-seat private. `HandEnded` reveals hole cards only at a proper showdown (river dealt + ≥2 non-folded contenders). The server's `BroadcastSink` enforces this per-recipient.

## Roadmap

Full step-by-step plan lives in [DESIGN.md](DESIGN.md). Summary:

| Steps | What | Status |
|---|---|---|
| 1–17 | Engine, sim, stats, MCCFR, blueprints, dataset, egui replay viewer | ✅ |
| 18 | Persona bot pool (`Nit`, `Tag`, `Lag`, `Maniac`, `TiltProne`) + `poker_dataset` | ✅ |
| 19a–c | TCP server skeleton, game messages, live client smoke | ✅ |
| 20 | `poker-trainer` split from `poker-engine` | ✅ |
| 21a–c | SQLite persistence — schema, Argon2id auth, per-hand log storage | ✅ |
| 22a–c | Security hardening — wire-layer limits, idle timeout, rate-limit, reconnect mid-hand | ✅ |
| 23 | `PROTOCOL.md` long-form wire spec | ✅ |
| 24 | Deprecate `poker-client`; per-crate READMEs; root README | ✅ |
| 25a | `poker-client-core` — pure synchronous state machine | ✅ |
| 25b | `poker-client-transport-native` — tokio TCP pump + session store + `NativeClient` facade | ✅ |
| 25c | `poker-client-headless` — `InMemoryTransport`, scenario driver, CLI replayer | ✅ |
| 25d | `client/` Tauri 2 + SolidJS shell — MVP functional loop | ✅ |
| 26 | Server-side hand replay verb (`RequestReplay`), protocol v4 | 🔲 |
| 27 | Mid-hand persistence atomicity — verify + regression test | 🔲 |

## Tests

```sh
cargo test --workspace          # all crates + workspace-root full-flow tests
cargo bench -p poker_engine     # criterion benchmarks
```
