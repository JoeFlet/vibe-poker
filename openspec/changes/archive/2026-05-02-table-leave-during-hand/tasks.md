## 1. Protocol version bump

- [x] 1.1 Change `PROTOCOL_VERSION` from `4` to `5` in `crates/poker-engine/src/net/protocol.rs`
- [x] 1.2 Run `cargo test --workspace` and fix any tests that hard-code `4` (expected: some may match the constant directly, which is fine, but any asserting literal `4` need updating)

## 2. Server: force-fold on mid-hand LeaveTable

- [x] 2.1 In `crates/poker-server/src/session.rs::handle_leave`, call `conn.cancel_pending()` before `table.leave(...)` so any in-flight prompt drops and `RemoteAgent::act` returns Fold to the engine
- [ ] 2.2 Verify the existing `conn.try_send(ServerMessage::LeftTable { ... })` still fires immediately (no change needed there)
- [x] 2.3 Update the doc comment on `table.rs::leave` to state: "when mid-hand, caller MUST also call `cancel_pending` on the leaver's Connection to force-fold; `leave_pending=true` alone does not affect the engine"
- [x] 2.4 Re-run `cargo test -p poker-server` — existing tests should pass unchanged (the prompt-timeout pathway stays valid as a fallback)

## 3. Server: integration test for mid-hand leave

- [x] 3.1 Add `tests/mid_hand_leave.rs` (workspace-root `poker-full-flow-tests` crate) with two players, seat both, start a hand, have the non-acting player send `LeaveTable`, assert the acting player's next event sees seat 1 folded within one poll tick (not after `deadline_ms` elapses)
- [x] 3.2 Add a sibling test for the pending-prompt case: prompt goes to seat 0, seat 0 sends LeaveTable, assert the engine receives Fold within one tick and the remaining seat sees HandEnded

## 4. Server: graceful-disconnect (non-superseded) test

- [x] 4.1 Add `tests/mid_hand_disconnect.rs`: seat two players, start hand, prompt seat 0, close seat 0's TCP connection cleanly, assert seat 1 sees `ActionTaken { seat: 0, action: Fold }` within one tick
- [x] 4.2 Add companion test for superseded session handoff: seat 0 connected, hand in progress with prompt pending, open a SECOND connection as the same user, authenticate, assert the old session's cleanup does NOT force-fold (the new session sees the re-issued Prompt and can submit an action normally)

## 5. Client-core: per-seat bet tracking

- [x] 5.1 Add `pub bet_this_street: Vec<(SeatIndex, u32)>` to `CurrentHand` in `crates/poker-client-core/src/view.rs`; initialise empty in `CurrentHand::started`
- [x] 5.2 In `crates/poker-client-core/src/core.rs::project_engine_event`:
    - On `HandStarted`: field is already reset via fresh `CurrentHand::started`
    - On `BoardDealt`: clear `bet_this_street`
    - On `ActionTaken { seat, action, .. }`: apply per-action update rules (Fold no-op, Check set-or-keep-at-max, Call match max, Raise(to) set to `to`, AllIn delegate to following `PlayerAllIn`)
    - On `PlayerAllIn { seat, total_committed }`: set `bet_this_street[seat]` to `total_committed` minus any earlier-street cumulative (approximate: store last-seen `total_committed` per seat in a new hand-local cumulative map, compute delta). If approximation diverges from server in edge cases, the post-hand `TableState` resets it.
- [x] 5.3 Add unit tests in `core.rs` covering all five scenarios from the client-core spec (Reset on new hand, Reset on new street, Raise sets exact, Call matches max, Fold keeps entry)

## 6. TypeScript types mirror

- [x] 6.1 Add `bet_this_street: [SeatIndex, number][]` to `CurrentHand` in `client/src/types.ts`
- [x] 6.2 `pnpm typecheck` clean

## 7. Client UI: render per-seat bet

- [x] 7.1 In `client/src/views/Seated.tsx`, inside the seat-list row render: derive `bet = view.current_hand?.bet_this_street.find(([s]) => s === seat.seat)?.[1] ?? 0`, display "(bet: N)" next to the stack column if `bet > 0`
- [x] 7.2 Visual check: run `pnpm tauri dev` with two profiles, play a hand, confirm bets appear and reset between streets
- [x] 8.2 In `client/src/views/Lobby.tsx`'s Disconnect button, add the same guard (though we shouldn't be in Lobby mid-hand, this is defensive against state desync)
- [x] 8.3 In `client/src/views/Ended.tsx`'s Back-to-login handler, the current `Intent::Disconnect` already triggers the force-fold path server-side via session-close; no new guard needed but add a comment pointing at the server-behavior spec

## 9. Integration test: full client-server flow

- [x] 9.1 Add `tests/mid_hand_leave_via_client.rs` in `poker-full-flow-tests`: use `NativeClient` from `poker-client-transport-native` for two clients, seat both, start a hand, one client issues `Intent::LeaveTable`, assert both clients converge on consistent state (leaver in Lobby, remaining in Seated with opponent seat folded)

## 10. Documentation

- [x] 10.1 Update `client/README.md` "Known limitations" section — mid-hand-leave is no longer a known broken behavior; mention the new confirmation dialog
- [x] 10.2 Update `crates/poker-engine/src/net/PROTOCOL.md` — bump version reference, add a note under `LeaveTable` describing the force-fold semantics
