# poker-server

Live TCP poker server. Wraps `poker_engine::Engine::run_hand` in a tokio runtime so remote clients can sit at a table and play hands against each other (or against future bot seats).

## Run it

```sh
cargo run -p poker-server                   # binds 127.0.0.1:7878 by default
cargo run -p poker-server -- --bind 0.0.0.0:7878 --max-seats 6
```

| Flag | Default | Purpose |
|---|---|---|
| `--bind` | `127.0.0.1:7878` | Listen address |
| `--data-dir` | `data` | Persistent state (`<data_dir>/poker.sqlite`) |
| `--small-blind` / `--big-blind` | `1` / `2` | Default-table blinds |
| `--max-seats` | `2` | Default-table size |
| `--buy-in` | `200` | Default-table suggested buy-in |

## Connecting a client

The primary client is the Tauri 2 + SolidJS app in [client/](../../client/):

```sh
cd client
pnpm install   # first time
pnpm tauri dev # opens the desktop window
```

Enter `127.0.0.1:7878` on the Connect screen, register a username, and join the table.

Any client built against [PROTOCOL.md](../poker-engine/src/net/PROTOCOL.md) will work — the spec is complete enough to implement a client in any language.

## Architecture

The load-bearing trick is the bridge from synchronous `Engine::run_hand` (which calls `Agent::act` synchronously) to async tokio:

- **Per-table actor** ([table.rs](src/table.rs)) — waits for quorum on a `Notify`, snapshots seated players, then runs `Engine::run_hand` on `tokio::task::spawn_blocking` with one `RemoteAgent` per seat.
- **`RemoteAgent::act`** ([remote_agent.rs](src/remote_agent.rs)) — sync (must be, because `Agent` is sync). Sends a `Prompt` over the connection's outbound queue and `Handle::block_on`s a `oneshot` that the next inbound `SubmitAction` resolves. Missed deadlines auto-fold.
- **`BroadcastSink`** ([table.rs](src/table.rs)) — fans `EngineEvent`s per-recipient with hole-card masking: `HoleCardsDealt` rides only to its owner; `HandEnded` reveals hole cards only at a proper showdown. Tests for this are in [tests/table_play.rs](tests/table_play.rs) — keep them green.
- **`SeatLink`** ([connection.rs](src/connection.rs)) — an `Arc<RwLock<Arc<Connection>>>` indirection shared by the table seat, `RemoteAgent`, and `BroadcastSink`. On a second login for the same user, `Table::reconnect_player` swaps the link in place and re-issues any in-flight `Prompt` so the hand keeps going.

## Wire protocol

Defined once in [poker_engine::net::protocol](../poker-engine/src/net/protocol.rs) and linked by client and server. Frames are `[u32 LE length][rmp-serde bytes]` — same format `FileSink` uses. `PROTOCOL_VERSION` (currently **3**) is bumped on any backwards-incompatible change. Full spec at [crates/poker-engine/src/net/PROTOCOL.md](../poker-engine/src/net/PROTOCOL.md).

## Persistence

State lives in `<data_dir>/poker.sqlite`. Schema is embedded from [migrations/](migrations/) via `sqlx::migrate!` and applied at startup.

| Table | Purpose |
|---|---|
| `users` | username → stable `PlayerId`; `email` nullable |
| `user_password` | Argon2id hash, one row per password-credentialed user |
| `sessions` | Append-only; "live session" = newest non-revoked row. A new login revokes the prior session in the same transaction |
| `lifetime_stats` | Single-row-per-user aggregate (VPIP / PFR / AF / chip delta), updated at `HandEnded` |
| `hands` | One row per finished hand; `log` BLOB is the same `[u32 LE length][rmp-serde]` framing `FileSink` uses |
| `hand_seats` | One row per participating seat per hand; chip delta + `sat_out` flag |

## Security / limits

[limits.rs](src/limits.rs) — `ConnectionLimits { idle_timeout: 60s, rate_burst: 30, rate_refill: 20/s }`. Both the handshake read and the post-handshake reader loop are wrapped in `tokio::time::timeout`; over-budget peers receive `Goodbye { "rate limit exceeded" }` and are torn down. Tests in [tests/security.rs](tests/security.rs).

## Tests

```sh
cargo test -p poker-server                          # all
cargo test -p poker-server --test handshake         # auth + reconnect
cargo test -p poker-server --test table_play        # full hand + hole-card masking
cargo test -p poker-server --test security          # wire hardening + rate limit
```
