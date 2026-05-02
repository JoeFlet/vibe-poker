## Why

Pressing **Leave table** or closing the client mid-hand currently puts the UI into a "visually in the lobby, actually still in a hand" split-brain state: the server marks the seat `leave_pending` and keeps the player active in the engine until `HandEnded`, while the client immediately transitions to `Phase::Lobby`. The player is unaware they still hold cards and can be seen by opponents. Mid-hand leaves also do not fold the seat immediately, so the engine stalls on the leaver's prompt until the server's action-deadline timer fires.

## What Changes

- **Server**: `LeaveTable` during a hand now force-folds the leaver (cancels any in-flight prompt — `RemoteAgent` treats a dropped responder as Fold) before replying `LeftTable`. The seat stays in `inner.seats` with `leave_pending = true` so the engine can settle side-pots, but is removed at `HandEnded` as today.
- **Server**: Graceful session close (non-superseded disconnect) also force-folds via the same path. Superseded sessions (handoff to a newer login) are unchanged — the new session keeps driving the seat through the hand.
- **Server**: Broadcasts a `TableState` snapshot (with the folded seat) to remaining seats so they see the leave promptly.
- **Client**: Mid-hand `LeaveTable` shows a confirmation dialog explaining the hand will be folded.
- **Client**: Per-seat current-street bet is tracked by the client-core state machine (no wire change) by watching `ActionTaken` events and rendered next to each player's name in the Seated view.
- **BREAKING (v4→v5)**: `LeaveTable`'s semantics change — previously "leave at next hand boundary, hand plays out", now "fold and leave immediately". Clients written against v4 will still compile but see different behavior. `PROTOCOL_VERSION` bumps to 5.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `server-behavior`: new requirement "Force-fold on mid-hand leave" covering both `LeaveTable` and graceful session-close; existing "Session revocation on new login" unchanged.
- `server-protocol`: `PROTOCOL_VERSION` bump to 5; `LeaveTable` semantics change documented.
- `client-core`: new requirement "Track per-seat street bet" covering the client-side bet reconstruction from `ActionTaken` events.

## Impact

- `crates/poker-server/src/table.rs`: `leave()` return type gains an `ImmediateOrPending` discriminator; mid-hand path cancels the leaver's prompt.
- `crates/poker-server/src/session.rs`: `handle_leave` calls `cancel_pending()` on the leaver's connection when the hand is in progress; unchanged superseded-disconnect path.
- `crates/poker-engine/src/net/protocol.rs`: `PROTOCOL_VERSION` → 5.
- `crates/poker-client-core/src/view.rs`: `CurrentHand` gains `bet_this_street: Vec<(SeatIndex, u32)>`.
- `crates/poker-client-core/src/core.rs`: `project_engine_event` populates per-seat street bets; resets on `HandStarted` / `BoardDealt`.
- `client/src/views/Seated.tsx`: renders bet chips next to each seat; confirmation dialog on Leave-during-hand.
- `client/src/views/Lobby.tsx`: "Disconnect" button gains the same mid-hand guard.
- `tests/`: new workspace-root integration test covering mid-hand leave and reconnect after force-fold.

## Non-goals

- No protocol-level message for the client to "finish the hand then leave" — that distinction was implicit today and collapses into the new force-fold semantics. Players who want to finish the hand just don't press Leave.
- No change to the "bust-out at `HandEnded`" logic (stack == 0 still vacates at hand boundary).
- No rendering changes to the Lobby view beyond the Disconnect guard.
