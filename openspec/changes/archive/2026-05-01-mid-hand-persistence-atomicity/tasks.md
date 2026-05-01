## 1. Code Comments

- [x] 1.1 Add `// INVARIANT:` comment to `Registry::record_hand` in `crates/poker-server/src/registry.rs` documenting that it is only called after `HandEnded` and that no partial write occurs. Verify with `cargo test -p poker-server`.
- [x] 1.2 Add `// INVARIANT:` comment to the `persist_hand` call-site in `crates/poker-server/src/table.rs` (inside `run_table`) documenting that `persist_hand` is only reached after `run_one_hand` returns (which only happens after `HandEnded` is emitted by `BroadcastSink`). Verify with `cargo test -p poker-server`.

## 2. Regression Test

- [x] 2.1 Add `abort_mid_hand_leaves_db_clean` test to `crates/poker-server/tests/persistence.rs`. The test shall: boot the server with `spawn_server`, register two players and have them both join the table, wait until a `HandStarted` event arrives at one of the client connections (to confirm the hand is in flight), abort the `run_table` task handle, wait 200ms, then assert `registry.count_hands() == 0`. Verify with `cargo test -p poker-server --test persistence`.
- [x] 2.2 Confirm that `cargo test -p poker-server --test persistence` passes (all tests in `persistence.rs` green), including both the new test and the existing `finished_hand_persists_log_and_seats` test.

## 3. Spec Update

- [x] 3.1 Verify the delta spec at `openspec/changes/mid-hand-persistence-atomicity/specs/server-behavior/spec.md` compiles under `openspec` (status shows `specs: done`). The live `openspec/specs/server-behavior/spec.md` merge happens at archive time via the sync step — no manual edit here.

## 4. Documentation

- [x] 4.1 Mark step 27 complete (✅) in `DESIGN.md` and run `cargo test --workspace` to confirm nothing regressed.
