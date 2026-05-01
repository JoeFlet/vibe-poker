## Why

The `server-behavior` spec declares that `Registry::record_hand` must write atomically on `HandEnded` and that a server crash mid-hand must leave the database as if the hand never started. That invariant is believed to hold today but has no regression test anchoring it — a future "incremental stat update" optimisation could silently break it. This change pins the invariant with a targeted stress test so any violation is caught immediately.

## What Changes

- Add a stress test that starts a hand, crashes the server task mid-hand (via `tokio::task::abort` or panicking the table actor), restarts, and asserts no SQLite mutation occurred (`hands`, `hand_seats`, `lifetime_stats` unchanged).
- Verify and document in code comments that `record_hand` is called exactly once per hand, only after `HandEnded` arrives, within the same SQLite transaction as the `lifetime_stats` upsert.
- Update `DESIGN.md` to mark step 27 complete (✅).

## Capabilities

### New Capabilities

_None — no new capability is introduced._

### Modified Capabilities

- `server-behavior`: The existing **Hand persistence atomicity** requirement gains a new scenario covering the stress-test regression contract (no mutation after mid-hand abort).

## Impact

- **`crates/poker-server/tests/`** — new integration test file (or addition to existing `table_play` or `security.rs` suite).
- **`crates/poker-server/src/registry.rs`** — code-comment audit; no functional change expected.
- **`DESIGN.md`** — step 27 marked ✅.
- No wire-protocol change; `PROTOCOL_VERSION` stays at 4.
- No new crate dependencies.

## Non-goals

- Incremental stat updates or any change to when stats are written.
- Full hand-replay or state-reconstruction after crash.
- Coverage of crashes during `record_hand` itself (partial-write inside the SQLite transaction is handled by SQLite's own atomicity).
