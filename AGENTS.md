# AGENTS.md

Compact reference for OpenCode sessions in this Rust poker workspace.

## Build & Test

```sh
cargo test --workspace                # all crates + workspace-root integration tests
cargo test -p poker-server --test table_play
cargo test -p poker-full-flow-tests   # real-TCP end-to-end
cargo bench -p poker_engine           # criterion benchmarks
```

Use `--release` for binaries that do real work (`poker_train`, `poker_play`, `poker_report`).

## Crate Map

| Crate | Role | Key Entrypoint | Capability Spec |
|---|---|---|---|
| `poker-engine` | Pure rules library: cards, eval, betting, agents, personas, stats, wire types (`net/`) | `Engine::run_hand` | [`engine-rules`](../openspec/specs/engine-rules/spec.md) |
| `poker-trainer` | MCCFR solver + dataset tools | `poker_train`, `poker_play`, `poker_dataset` binaries | — |
| `poker-server` | Async tokio TCP server, SQLite persistence, Argon2id auth | `cargo run -p poker-server` | [`server-behavior`](../openspec/specs/server-behavior/spec.md), [`server-protocol`](../openspec/specs/server-protocol/spec.md) |
| `poker-client-core` | Synchronous state machine (`Intent` → `Effect`), no I/O | Pure lib; every transition unit-tested | [`client-core`](../openspec/specs/client-core/spec.md) |
| `poker-client-transport-native` | Tokio TCP transport + `NativeClient` facade | Used by Tauri shell in `client/` | [`client-transport`](../openspec/specs/client-transport/spec.md) |
| `poker-client-headless` | Test harness, `InMemoryTransport`, scenario driver + CLI | `cargo run -p poker-client-headless -- <scenario.json>` | [`client-core`](../openspec/specs/client-core/spec.md), [`client-transport`](../openspec/specs/client-transport/spec.md) |
| `tests/` (crate `poker-full-flow-tests`) | Workspace-root real-TCP integration tests | | [`server-behavior`](../openspec/specs/server-behavior/spec.md), [`client-core`](../openspec/specs/client-core/spec.md) |
| `client/` (submodule) | Tauri 2 + SolidJS + Vite shell; **not in parent Cargo workspace** | `pnpm install && pnpm tauri dev` | [`client-core`](../openspec/specs/client-core/spec.md) |

See [`openspec/specs/`](../openspec/specs/) for load-bearing requirements. **Before changing code, read the relevant capability spec.**

## End-to-End Development Loop

```sh
# 1. Train a blueprint
cargo run --release -p poker-trainer --bin poker_train -- --iters 200000 --out bp.mp --seed 7

# 2. Play blueprint vs bot, log to file
cargo run --release -p poker-trainer --bin poker_play -- --blueprint bp.mp --hands 5000 \
    --agents blueprint,calling --log session.mp

# 3. Live server + Tauri client
cargo run -p poker-server                       # 127.0.0.1:7878
cd client && pnpm install && pnpm tauri dev     # separate terminal

# 4. Two local clients against one server (testing only, see client/README.md)
cd client && pnpm tauri:p2                      # second window, identifier com.poker.client.p2
```

## Design Authority

`DESIGN.md` is the authoritative numbered plan. Update it together with the README roadmap table when making nontrivial architectural changes.

`openspec/specs/` carries the normative requirements for each subsystem. When a change modifies behavior (not just implementation), update the relevant spec delta and merge to main specs on archive.
