## Context

`Registry::record_hand` writes a hand's event log and per-seat chip deltas to SQLite (`hands` + `hand_seats` tables) only after the table actor's `run_one_hand` call returns — which happens only after the engine has emitted `HandEnded`. The call sequence in `table.rs` is:

```
run_one_hand(...)   ← engine runs; HandEnded emitted inside BroadcastSink
apply_hand_result(...)
persist_hand(...)   ← calls registry.record_hand(...)
```

The invariant is believed to hold today, but there is no test that exercises or proves the "crash mid-hand → DB unchanged" branch. A future "incremental stat update" feature could easily break it by moving a write before `HandEnded`.

Note: `update_stats` is not called from `persist_hand`; `lifetime_stats` is written by the session-level `update_stats` path (triggered separately). The atomicity invariant covers `hands` + `hand_seats`; `lifetime_stats` is a known gap in the current design that is **out of scope** for this change.

## Goals / Non-Goals

**Goals:**
- Add a regression test that aborts the table actor mid-hand and asserts no `hands`/`hand_seats` rows appeared.
- Add a code comment in `registry.rs` on `record_hand` and in `table.rs` on `persist_hand` pinning the call-site contract.
- Mark step 27 complete in `DESIGN.md`.

**Non-Goals:**
- Changing when or how stats are written (`update_stats` / `lifetime_stats`).
- Persisting partial-hand state.
- Recovering a crashed hand or replaying it post-crash.
- Coverage of a crash inside `record_hand` itself (SQLite's own WAL handles that).
- Adding new database migrations or schema changes.

## Decisions

### Decision: Abort the spawn_blocking task, not the whole server

The hand runs on `tokio::task::spawn_blocking`. Aborting the `JoinHandle` returned by `spawn_blocking` causes it to return `Err(JoinError)` (the task is cancelled). The table actor already handles this case in `run_one_hand`: the `match outcome { Err(e) => ... }` arm returns an empty `HandResult` with no seats, so `persist_hand` is called with an empty seats list and an empty log.

This means the existing code path already handles a "panic in the hand task" correctly — `record_hand` is still called, but with empty data, which could still write a row. The test needs to abort the **table actor task itself** (before `persist_hand` is called) to prove the DB is untouched.

**Approach**: use `tokio::task::JoinHandle::abort()` on the table-actor task (`run_table`) after a hand starts but before `HandEnded` is received by a polling watcher. This is the cleanest simulation of a mid-hand server crash.

**Alternative considered**: Spawning a Tokio runtime in a separate thread and calling `runtime.shutdown_background()` mid-hand. Rejected: more complex, OS-level cleanup races, and not representative of a clean abort.

### Decision: Test lives in `crates/poker-server/tests/persistence.rs`

The existing `persistence.rs` already covers the normal "hand persists after completion" path. Appending a second test there keeps related concerns co-located and reuses the `spawn_server` helper pattern without duplication.

**Alternative**: New file `tests/atomicity.rs`. Rejected: extra file for a small test that shares helpers with `persistence.rs`.

### Decision: Document the contract with `// INVARIANT:` comments, not rustdoc

The call-site guarantee is an operational contract between `table.rs` and `registry.rs`, not a user-facing API contract. A short `// INVARIANT:` comment at the call site in `persist_hand` and at the function signature in `record_hand` is idiomatic for this pattern in the codebase.

## Risks / Trade-offs

- **Race in the test**: After aborting the table actor, there may be a brief window where the spawned `spawn_blocking` task still completes and calls `persist_hand`. The test must wait long enough (e.g. 100ms) after the abort before checking the DB. → Mitigation: poll `count_hands` for a brief window; if it stays 0 after 200ms the invariant holds.
- **`lifetime_stats` gap**: The spec requirement mentions `lifetime_stats` as part of the atomicity invariant, but the current implementation does not update `lifetime_stats` inside `persist_hand` — it's a separate write path. The test will document this gap with a comment rather than asserting on `lifetime_stats` (which is not touched by `record_hand` at all). A future "incremental stat update" change would need to address this when it moves stats writes into `persist_hand`.
- **Test fragility on slow CI**: `spawn_blocking` tasks are not abortable by tokio's task abort. If the blocking thread has already started executing before the abort arrives, it will run to completion. The test accounts for this by using a synchronisation barrier (wait for `HandStarted` to arrive at a client before aborting) to ensure the abort fires mid-hand rather than before the task starts.

## Migration Plan

No schema changes, no wire-protocol changes, no migration needed. The change is additive: new test + new code comments + DESIGN.md update.

## Open Questions

None. Implementation is straightforward given the existing test harness.
