## Context

Today's `LeaveTable` during a hand (see `crates/poker-server/src/table.rs::leave` at lines 257-274 and `crates/poker-server/src/session.rs::handle_leave` at lines 431-448):

1. Marks the seat `leave_pending = true`, keeps the seat in `inner.seats[]`, keeps the `SeatLink` live.
2. Sends `LeftTable` back to the client *immediately*.
3. Hand continues normally; engine still expects the leaver's responses.
4. Leaver's prompt (if any) times out after the action-deadline (~30s), at which point the server folds them automatically.
5. On `HandEnded`, the seat vacates via the bottom of `run_one_hand`.

The client's `project_engine_event` (`crates/poker-client-core/src/core.rs:380`) defensively drops `TableEvent`s for non-current tables, so after the immediate `LeftTable` the leaver just sees silence for the rest of the hand — but the server has still been waiting 30s for their dead prompt before folding them, stalling other players.

Per-seat bets are already tracked server-side on `state.players[i].bet_this_street`, but are not broadcast except indirectly via `EngineEvent::ActionTaken { seat, action, pot_total }`. Clients today only get `pot_total` and the action variant; there is no authoritative "alice has 30 chips in the pot this street" signal.

## Goals / Non-Goals

**Goals:**
- Fix the split-brain UX where the UI flips to Lobby but the engine still holds the leaver's seat.
- Eliminate the ~30s dead-prompt stall when someone leaves mid-hand.
- Show per-seat current-street bet next to each player's name in the Seated view.
- Preserve the session-handoff behavior: when a new login supersedes an old connection, the *new* session inherits the seat and plays out the hand normally (no force-fold).
- Ship minimal protocol change (a semantic bump, no new message variants).

**Non-Goals:**
- No change to bust-out semantics (`stack == 0` still vacates at `HandEnded`).
- No change to the prompt deadline timer; it's still the backstop for unresponsive clients that haven't sent `LeaveTable` or disconnected.
- No UI confirmation redesign beyond a simple `window.confirm`-style prompt for this iteration. A polished modal is deferred.
- No new wire message for per-seat bets; client-core reconstructs them from existing `ActionTaken` + `LegalActions` data.

## Decisions

### D1: Force-fold on `LeaveTable` rather than on prompt timeout

**Choice**: when `LeaveTable` arrives mid-hand, call `conn.cancel_pending()` on the leaver's `Connection` before marking `leave_pending`. This is exactly the path session-close already uses (`session.rs:148`). Dropping the pending responder causes `RemoteAgent::act` to receive `RecvError` and return `Fold` to the engine immediately, so the next actor can move.

**Alternatives considered**:
- *New `LeaveTableNow` wire message*: more explicit but doubles the leave codepath and bumps protocol for no semantic reason the user doesn't already have ("I pressed Leave; fold me").
- *Synthesize a `SubmitAction(Fold)`*: works but races `cancel_pending`'s timeout path. Dropping the responder is the established cancellation primitive.

### D2: Keep `LeftTable` reply immediate

The client transitions to `Phase::Lobby` right away (as today); the server's continued `TableEvent` broadcasts for the hand are silently dropped by `project_engine_event`'s non-current-table guard. This is the existing contract and avoids holding the client in a zombie "leaving..." state.

The seat stays in `inner.seats[]` until `HandEnded` so side-pot accounting still sees the leaver's committed chips. `leave_pending = true` plus the post-hand cleanup at `table.rs:604-615` already handles vacating.

### D3: Session-close: non-superseded only

`session.rs:152` already has `superseded = revoked.load(Ordering::SeqCst)` and only calls `force_leave` when false. The existing `conn.cancel_pending()` call at `session.rs:148` runs unconditionally — which means today a session handoff *also* cancels the old connection's prompt.

**Problem**: this conflicts with the handoff contract (the new session should inherit the in-flight prompt). Looking closer at `connection.rs:188-224`, `SeatLink::rebind_to` lifts the pending slot from the old `Connection` to the new one via `migrated = old.pending.lock().take()`. So by the time the OLD session reaches `session.rs:148`, the slot is already `None` — `cancel_pending` is a no-op on a handed-off connection. ✓ No change needed.

**New subtlety**: for the *non-superseded* path, we want `cancel_pending` to *also trigger force-fold broadcast awareness*. Currently it's a pure local drop; the engine sees a RecvError from `RemoteAgent::act` and folds at the engine layer. That path already emits `ActionTaken { action: Fold }`, which broadcasts to remaining seats — so no extra work is needed. We just document that this is the force-fold path.

### D4: Per-seat street bet — client-side reconstruction

**Choice**: extend `CurrentHand` in `poker-client-core::view` with `bet_this_street: Vec<(SeatIndex, u32)>`. On `HandStarted` / `BoardDealt`, reset. On `ActionTaken`, update the leaver's entry:
- `Fold`, `Check`: no change (Fold removes from active, Check matches current table bet or stays 0).
- `Call`: set to `max(bet_this_street)` across currently-active seats — i.e. match the high bet.
- `Raise(to)`: set to `to`.
- `AllIn`: need `total_committed` for the seat, which the server already emits as `PlayerAllIn { seat, total_committed }`. Take `total_committed - (sum of prior streets' committed)`. Since the client can't easily reconstruct prior-street totals without more state, approximate as `bet_this_street[seat] = player's stack-pre + current bet`. **Simpler**: treat AllIn that's a raise as setting bet to `max(bet_this_street) + delta`, treat AllIn-call as matching. For v5 the engine still distinguishes raise-all-in from call-all-in via `ActionTaken`'s successor events; a cleaner solution is for the client to infer "at least the current high bet" and let the server's `TableState` after the hand correct anything that drifted.

**Simplification adopted**: on Raise(to), set `bet[seat] = to`. On Call, set `bet[seat] = max(bet)`. On AllIn, look at the matching `PlayerAllIn` event (which immediately precedes or follows `ActionTaken` per engine code) and set `bet[seat] = min(total_committed - earlier_streets, player's pre-action stack + prior bet)`. In practice, since the UI only needs a visual cue and the pot_total stays authoritative, drift at the edges is tolerable. The Seated view always shows the pot total from the server.

**Alternatives considered**:
- *Server adds `bet_this_street` to `SeatInfo`*: accurate but bumps protocol further and requires a broadcast every action. Overkill for a UI affordance.
- *Don't show bets at all*: user-rejected explicitly.

### D5: Protocol version bump to 5

`LeaveTable` semantics change is observable by any v4 client that relied on "play out the hand after pressing Leave". We bump `PROTOCOL_VERSION` from 4 to 5. No new message variants; the wire shape is identical. Clients compiled against v4 will be rejected at handshake with `Rejected { reason: "protocol mismatch" }`.

## Risks / Trade-offs

**[R1]** *Client-side bet reconstruction can drift on unusual all-in sequences* → Mitigation: the server's `TableState` broadcast after `HandEnded` resets all per-seat bookkeeping (the `bet_this_street` vec is cleared on next `HandStarted`). Drift is bounded to a single hand and does not affect game correctness, only UI presentation. The authoritative `pot_total` from the server always displays.

**[R2]** *A v4 client in the wild breaks on upgrade* → Mitigation: the Tauri client and `poker-client-headless` are the only known clients; both ship from the same repo and upgrade atomically. Third parties would already fail on protocol mismatch per existing spec.

**[R3]** *Mid-hand force-fold surprises observers who expected "they'll play it out"* → Mitigation: the client dialog explicitly says "Leaving will fold your hand. Continue?". Server-side, folded-then-removed is the same end state the prompt-deadline would eventually reach.

**[R4]** *Race: `LeaveTable` arrives *between* a `Prompt` going out and the user seeing it* → Mitigation: `cancel_pending` is atomic on the `pending` mutex; if a `SubmitAction` arrives after, `deliver_action` sees an empty slot and replies `ActionRejected { reason: "no matching pending action" }`. Existing behavior; no new failure modes.

## Migration Plan

1. Ship the server change + `PROTOCOL_VERSION` bump in one release.
2. Ship the client change in the same release. Both crates live in the same repo and versioned lockstep.
3. Clients with stale installs will get `Rejected { reason: "protocol mismatch" }` on the next connect; the UI already routes this to `Phase::Ended` with the reason visible.
4. No database migration needed — no schema changes.

**Rollback**: revert the server commit; clients that auto-updated will again fail handshake (mismatched v5-client-vs-v4-server). Low-risk because the entire v5 change is a semantic tightening — if we need to undo, we revert both sides together.

## Open Questions

None — the user explicitly chose "change existing `LeaveTable` semantics" over "new message variant", and "force-fold on disconnect, except on handoff" over "rely on prompt timeout". Implementation can proceed.
