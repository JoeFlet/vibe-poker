# poker-server

Live TCP poker server. Wraps `poker_engine::Engine::run_hand` in an async runtime so remote clients can sit at a table and play hands.

This is the active development surface for the workspace going forward. The next-generation playable client lives in a separate sibling repository (planned to land here later as a git submodule) and consumes the wire protocol documented in [crates/poker-engine/src/net/PROTOCOL.md](../poker-engine/src/net/PROTOCOL.md). The in-tree [poker-client](../poker-client/) is a deprecated smoke-test harness only.

## Run it

```sh
cargo run -p poker-server                   # binds 127.0.0.1:7878 by default
cargo run -p poker-server -- --bind 0.0.0.0:7878 --max-seats 6
```

| Flag | Default | Purpose |
|---|---|---|
| `--bind` | `127.0.0.1:7878` | Listen address |
| `--data-dir` | `data` | Persistent state (`<data_dir>/poker.sqlite` — SQLite via `sqlx`, migrations in [migrations/](migrations/)) |
| `--small-blind` / `--big-blind` | `1` / `2` | Default-table blinds |
| `--max-seats` | `2` | Default-table size |
| `--buy-in` | `200` | Default-table suggested buy-in |

Connect the in-tree [poker-client](../poker-client/) (deprecated smoke-test harness) with `--connect 127.0.0.1:7878 --username alice` for an end-to-end smoke test, or point any client built against [`PROTOCOL.md`](../poker-engine/src/net/PROTOCOL.md) at the same address.

## Architecture

The load-bearing trick is the bridge from synchronous `Engine::run_hand` (which calls `Agent::act` synchronously) to async tokio:

- **Per-table actor** ([table.rs](src/table.rs)) waits for quorum on a `Notify`, snapshots seated players, then runs `Engine::run_hand` on `tokio::task::spawn_blocking` with one `RemoteAgent` per seat.
- **`RemoteAgent::act`** ([remote_agent.rs](src/remote_agent.rs)) is sync. It sends a `Prompt` over the connection's outbound queue and `Handle::block_on`s a `oneshot` that the next inbound `SubmitAction` resolves. Missed deadlines auto-fold the seat.
- **`BroadcastSink`** ([table.rs](src/table.rs)) fans `EngineEvent`s out per-recipient with hole-card masking: `HoleCardsDealt` rides only to its owner; `HandEnded` masks all hole cards for non-recipients except at proper showdowns (river dealt + ≥2 non-folded contenders). The visibility tests live in [tests/table_play.rs](tests/table_play.rs) — keep them green.

## Wire protocol

Defined once in [poker_engine::net::protocol](../poker-engine/src/net/protocol.rs) and shared with the client. Frames are `[u32 LE length][rmp-serde bytes]` — same format `FileSink` uses, so server broadcasts can be spooled straight into a replay log.

`PROTOCOL_VERSION` lives at the top of that module and is bumped on any backwards-incompatible change. The long-form spec — framing, handshake, every variant, ordering, error model, reconnect — is at [crates/poker-engine/src/net/PROTOCOL.md](../poker-engine/src/net/PROTOCOL.md).

## Persistence

State lives in `<data_dir>/poker.sqlite`. The schema is embedded from [migrations/](migrations/) via `sqlx::migrate!` and applied at startup. The full table set (users, password, sessions, lifetime stats, hands, hand seats) is provisioned by the initial migration:

- **`Registry`** ([registry.rs](src/registry.rs)) — username → stable `PlayerId` + `LifetimeStats` via `users` and `lifetime_stats`. Reconnecting with the same name returns the same id. The `db` module ([db.rs](src/db.rs)) opens the pool with WAL journaling and foreign-keys ON.
- `user_password` — Argon2id hash per password-credentialed user (OAuth would land in a sibling table without bloating `users`). Written by `Registry::register`.
- `sessions` — append-only; "live session" = newest non-revoked row per user. `register` / `authenticate_password` insert a new row and `UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL` in the same transaction; the prior connection's in-memory `Arc<AtomicBool>` flips so its reader sends `Goodbye { "session_revoked" }` on the next frame.
- `hands` + `hand_seats` — one row per finished hand. The `log` BLOB is the same `[u32 LE length][rmp-serde]` framing `FileSink` produces, accumulated by `BroadcastSink` while it broadcasts to clients, so a server-recorded hand can be replayed unchanged. `hand_seats` carries one row per participating seat with the engine's `chip_delta` and `sat_out` flag. Lookups via `Registry::fetch_hand` / `count_hands`.

## Tests

```sh
cargo test -p poker-server                        # all
cargo test -p poker-server --test handshake       # auth + reconnect
cargo test -p poker-server --test table_play      # full hand over the wire + hole-card masking
```
