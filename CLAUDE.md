# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common commands

Run from the workspace root.

```sh
cargo build --workspace
cargo test  --workspace
cargo bench -p poker_engine                                 # criterion suite
cargo test  -p poker-server --test table_play               # one integration file
cargo test  -p poker-client --bin poker-client live_client  # filter by test name
```

End-to-end loop the project is built around:

```sh
cargo run --release --bin poker_train -- --iters 200000 --out blueprint.mp --seed 7
cargo run --release --bin poker_play  -- --blueprint blueprint.mp --hands 5000 \
    --agents blueprint,calling --log session.mp
cargo run --release --bin poker-client -- --replay session.mp
```

Live mode against `poker-server` (step 19c):

```sh
cargo run -p poker-server                                                # 127.0.0.1:7878
cargo run -p poker-client -- --connect 127.0.0.1:7878 --username alice
```

Other engine binaries: `poker_report` (default `cargo run -p poker_engine`) and `poker_dataset` (round-robin persona simulation, `--out logs/` writes per-matchup `FileSink` files).

## Architecture

Four-crate workspace. `poker_engine` is the rules library; the others build on it.

### `poker-engine` — pure library, no I/O

Layered as `core` (cards, deck, evaluators) → `game` (`Engine`, `BettingRules`, `EngineEvent`, sinks, pot calc) → `agent` (`Agent` trait + builtins + personas) → `sim`, `stats`, `abstraction`, `net`. `Engine::run_hand(hand_id, deck_seed, stacks, dealer, agents, sink)` is the single entry point: it drives the hand synchronously, calling `Agent::act` on each prompt, and emits an `EngineEvent` stream into the supplied sink.

Two cross-cutting design rules to honour when extending it:

- **Determinism via `deck_seed`** — the seed in `HandStarted` is the only value needed to reproduce the exact deal. Agent seeds are passed at construction and never touched by the engine. Don't pull entropy inside the engine; thread it through.
- **Chips are `u32`, BB-denominated, no floats anywhere.** Split-pot remainders go to the first eligible winner left of the dealer. `SeatOutcome::chip_delta` is `i32`.

The wire protocol lives in [crates/poker-engine/src/net/](crates/poker-engine/src/net/) so client and server link against one canonical definition. Frames are `[u32 LE length][rmp-serde bytes]` — the same format `FileSink` uses, so server broadcasts can be spooled straight into a replay log. Bump `PROTOCOL_VERSION` on any backwards-incompatible change.

### `poker-trainer` — agent training scaffolding

Split out of `poker-engine` in step 20 so the engine stays a focused rules library. Carries `solver/` (MCCFR + blueprint persistence), `dataset/` (HandStats aggregator + class-conditional + windowed views), and the `poker_train` / `poker_play` / `poker_dataset` binaries. Depends on `poker-engine`. Personas and `StatsSink` deliberately stay in the engine because the server uses personas as future table-fillers.

Long-term, serious AI development is expected to migrate to a separate (likely Python) project consuming `poker-engine`; this crate is the in-tree reference trainer.

### `poker-server` — async wrapper around the sync engine

The bridge from synchronous `Engine::run_hand` to async tokio is the load-bearing trick. A per-table actor ([table.rs](crates/poker-server/src/table.rs)) waits for quorum, snapshots seated players, then runs `Engine::run_hand` on `tokio::task::spawn_blocking` with one `RemoteAgent` per seat. `RemoteAgent::act` ([remote_agent.rs](crates/poker-server/src/remote_agent.rs)) is sync (it has to be — `Agent` is sync), so it sends a `Prompt` over the connection's outbound queue and `Handle::block_on`s a `oneshot` that the next inbound `SubmitAction` resolves. Missed deadlines auto-fold.

`BroadcastSink` fans `EngineEvent`s out per-recipient with hole-card masking: `HoleCardsDealt` ride only to their owner; `HandEnded` masks all hole cards for non-recipients except at proper showdowns (river dealt + ≥2 non-folded contenders). When you touch event broadcast, preserve this — there are dedicated tests in `tests/table_play.rs` that fail loudly on leaks.

`Registry` ([registry.rs](crates/poker-server/src/registry.rs)) is a SQLite-backed player record (`<data_dir>/poker.sqlite`, schema embedded from [migrations/](crates/poker-server/migrations/) via `sqlx::migrate!`) that maps username → stable `PlayerId` + `LifetimeStats`. The first message must be `Register { email, username, password, … }` or `Authenticate { mode: Password { identifier, password } | Session { key }, … }`; success returns `Welcome { session_key, … }` and revokes the prior live session for that user. Argon2id hashes live in `user_password`; sessions are append-only in `sessions`. After each hand the table actor calls `Registry::record_hand` to write one `hands` row (with the full `FileSink`-compatible event log accumulated by `BroadcastSink`) plus one `hand_seats` row per participating seat.

### `poker-client` — deprecated reference client

Both replay and live modes derive a single [Snapshot](crates/poker-client/src/snapshot.rs) from the same `EngineEvent` stream and feed it through `render_snapshot`. Replay mode slices `events[..=cursor]` and re-derives; live mode calls `Snapshot::apply(&event)` incrementally.

Live mode keeps egui purely synchronous. [LiveClient](crates/poker-client/src/live_net.rs) owns a worker thread running a current-thread tokio runtime; the gui calls non-blocking `send` / `drain` (paired `mpsc::UnboundedChannel`s). [LiveApp](crates/poker-client/src/live.rs) is the egui state machine: Connecting → Lobby → Seated → Ended.

**Status: deprecated** as of step 20. Stays in-tree pinned to the current `PROTOCOL_VERSION` so the server has something to test against during the step-21–23 hardening pass. Don't add features here — only protocol-compatibility fixes. The next-generation client will be a separate project.

## DESIGN.md is authoritative

[DESIGN.md](DESIGN.md) carries the numbered build plan. As of 2026-04-28 the project pivoted from "engine for AI training" to "poker server with engine + trainer as deps and a deprecated reference client." Active work is steps 21–24 (SQLite + auth → security audit → `PROTOCOL.md` → client deprecation finalised). When making nontrivial architectural changes, update DESIGN.md and the README roadmap table together.
