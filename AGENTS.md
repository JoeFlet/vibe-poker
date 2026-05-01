# AGENTS.md

Compact reference for OpenCode sessions in this Rust poker workspace.

## Build & Test

```sh
cargo test --workspace          # everything including workspace-root integration tests
cargo test -p poker-server --test table_play   # server broadcast/hole-card tests
cargo test -p poker-client --bin poker-client live_client  # filter by test name
cargo test -p poker-full-flow-tests            # real-TCP end-to-end in tests/
cargo bench -p poker_engine                    # criterion benchmarks
```

Use `--release` for binaries that do real work (`poker_train`, `poker_play`, `poker_report`).

## Crate Map

| Crate | Role | Key Entrypoint |
|---|---|---|
| `poker-engine` | Pure rules library: cards, eval, betting, agents, personas, stats, wire types (`net/`) | `Engine::run_hand` |
| `poker-trainer` | MCCFR solver + dataset tools | `poker_train`, `poker_play`, `poker_dataset` binaries |
| `poker-server` | Async tokio TCP server, SQLite persistence, Argon2id auth | `cargo run -p poker-server` |
| `poker-client-core` | Synchronous state machine (Intent → Effect) | Pure lib, no I/O |
| `poker-client-transport-native` | Tokio TCP transport + `NativeClient` facade | Used by Tauri shell |
| `poker-client-headless` | Test harness, `InMemoryTransport`, scenario driver + CLI | `cargo run -p poker-client-headless -- <scenario.json>` |
| `poker-client` | **Deprecated** egui viewer + old live client | Frozen at `PROTOCOL_VERSION = 3`; only compat fixes |
| `tests/` (crate `poker-full-flow-tests`) | Workspace-root real-TCP integration tests | | |
| `client/` (submodule) | Tauri 2 + SolidJS + Vite shell; **not in parent Cargo workspace** | `pnpm install && pnpm tauri dev` |

## Critical Invariants

- **Determinism:** `deck_seed` in `HandStarted` is the sole source of deal entropy. Agent seeds are passed at construction. Never pull randomness inside the engine.
- **Integer chips only:** `u32`, BB-denominated. `SeatOutcome::chip_delta` is `i32`. No floating-point in game logic.
- **Hole-card masking:** `HoleCardsDealt` is per-seat private. `HandEnded` reveals hole cards only at proper showdown (river dealt + ≥2 non-folded contenders). The server's `BroadcastSink` enforces this per-recipient; changing broadcast logic without updating `table_play.rs` tests will leak cards.

## Wire Protocol

- Defined once in `crates/poker-engine/src/net/`.
- Framing: `[u32 LE length][rmp-serde bytes]`, same as `FileSink`.
- `PROTOCOL_VERSION = 3`. Bump on any backwards-incompatible change and update `crates/poker-engine/src/net/PROTOCOL.md`.

## Server Details

- SQLite at `<data_dir>/poker.sqlite`; migrations in `crates/poker-server/migrations/` applied at startup via `sqlx::migrate!`.
- `Registry::record_hand` writes one `hands` row (log BLOB) + `hand_seats` rows after each hand.
- Reconnect mid-hand swaps the `SeatLink` in place so the blocked `RemoteAgent::act` resolves on the new socket.

## End-to-End Development Loop

```sh
# 1. Train a blueprint
cargo run --release -p poker-trainer --bin poker_train -- --iters 200000 --out bp.mp --seed 7

# 2. Play blueprint vs bot, log to file
cargo run --release -p poker-trainer --bin poker_play -- --blueprint bp.mp --hands 5000 \
    --agents blueprint,calling --log session.mp

# 3. Replay in deprecated viewer
cargo run --release -p poker-client -- --replay session.mp

# 4. Live server + Tauri client
cargo run -p poker-server                       # 127.0.0.1:7878
cd client && pnpm install && pnpm tauri dev     # separate terminal
```

## Design Authority

`DESIGN.md` is the authoritative numbered plan. Update it together with the README roadmap table when making nontrivial architectural changes.
