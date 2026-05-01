## 1. Wire Protocol (`poker-engine`)

- [x] 1.1 Add `RequestReplay { hand_id: Uuid }` to `ClientMessage` in `crates/poker-engine/src/net/protocol.rs`
- [x] 1.2 Add `ReplayEvents { hand_id: Uuid, events: Vec<ServerMessage> }` to `ServerMessage` in `crates/poker-engine/src/net/protocol.rs`
- [x] 1.3 Bump `PROTOCOL_VERSION` from `3` to `4` in `crates/poker-engine/src/net/protocol.rs`
- [x] 1.4 Update `crates/poker-engine/src/net/PROTOCOL.md`: add §6 sub-section for `RequestReplay`/`ReplayEvents` preconditions, guarantees, and ordering
- [x] 1.5 Update `openspec/config.yaml` to reflect `PROTOCOL_VERSION = 4`
- [x] 1.6 Run `cargo test -p poker-engine` to verify protocol types compile and frame tests pass

## 2. Server Replay Buffering (`poker-server`)

- [x] 2.1 Add `replay_buffer: Vec<ServerMessage>` to `SeatLink` (or per-seat map in `Table`) in `crates/poker-server/src/`
- [x] 2.2 Modify `BroadcastSink::send_event` to append each fanned `ServerMessage` to the target seat's `replay_buffer`
- [x] 2.3 Ensure buffers are dropped/cleared when `HandRunner` is torn down at `HandEnded`
- [x] 2.4 Run `cargo test -p poker-server --test table_play` to confirm normal hand flow still works

## 3. Server RequestReplay Handler (`poker-server`)

- [x] 3.1 In `crates/poker-server/src/session.rs`, add match arm for `ClientMessage::RequestReplay`
- [x] 3.2 Validate that the session user is seated at the table and `hand_id` matches the current hand; else reply `ActionRejected { reason: "not seated at this hand" }`
- [x] 3.3 On valid request, clone the seat's `replay_buffer` and send `ServerMessage::ReplayEvents { hand_id, events: buffer }`
- [x] 3.4 Run `cargo test -p poker-server --test table_play` to confirm no regressions

## 4. Client-Core Ingestion (`poker-client-core`)

- [x] 4.1 In `crates/poker-client-core/src/core.rs`, add match arm for `ServerMessage::ReplayEvents` in `handle_inbound`
- [x] 4.2 Iterate `events` and call the existing inbound-handling logic for each `ServerMessage` in order
- [x] 4.3 Run `cargo test -p poker-client-core` to verify existing tests pass

## 5. Protocol Version Updates in Tests & Docs

- [x] 5.1 Update all hard-coded `PROTOCOL_VERSION` assertions in `crates/poker-server/tests/` from `3` to `4`
- [x] 5.2 Update all hard-coded `PROTOCOL_VERSION` assertions in `crates/poker-client-headless/tests/` from `3` to `4`
- [x] 5.3 Update all hard-coded `PROTOCOL_VERSION` assertions in `crates/poker-client-core/src/core.rs` tests from `3` to `4`
- [x] 5.4 Update `crates/poker-server/README.md` and `crates/poker-engine/README.md` any mentions of version 3
- [x] 5.5 Update `README.md` roadmap to mark step 26 as complete
- [x] 5.6 Run `cargo test --workspace` to verify all updated tests pass

## 6. Integration Tests

- [x] 6.1 Add `poker-server/tests/replay.rs` (or extend `table_play.rs`): test successful `RequestReplay` returns correct event count and ordering
- [x] 6.2 Add integration test: unseated client sending `RequestReplay` gets `ActionRejected`
- [x] 6.3 Add integration test: `RequestReplay` with mismatched `hand_id` gets `ActionRejected`
- [x] 6.4 Add integration test: replayed `HoleCardsDealt` contains only requesting seat's cards
- [x] 6.5 Add integration test: live events after replay arrive in order without duplication
- [x] 6.6 Run `cargo test -p poker-server` and `cargo test --workspace` to verify all new tests pass
