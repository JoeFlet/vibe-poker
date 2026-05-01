## Why

Client reconnection mid-hand must be able to reconstruct full table state without waiting for the next hand. This is required by the crash-resilience principle: a client that drops and reconnects should see the same state as one that stayed connected. Currently a reconnected client sits blind until `HandEnded`, which breaks usability.

## What Changes

- **BREAKING**: Add `ClientMessage::RequestReplay { hand_id: u64 }` to the wire protocol.
- **BREAKING**: Add `ServerMessage::ReplayEvents { hand_id: u64, events: Vec<ServerMessage> }` containing the ordered `EngineEvent` stream for the requesting seat.
- Bump `PROTOCOL_VERSION` from **3** to **4**.
- Server `session.rs`: validate that the requester is seated at the named hand, then emit the buffered per-seat event stream.
- `poker-client-core`: on receiving `ReplayEvents`, replay the enclosed `ServerMessage` sequence through `ClientCore` to reconstruct `CurrentHand` deterministically.
- Update `PROTOCOL.md` with the new verb's preconditions, guarantees, and ordering rules.

## Capabilities

### New Capabilities
- *(none — no new subsystem introduced)*

### Modified Capabilities
- `server-protocol`: New `ClientMessage`/`ServerMessage` variants; `PROTOCOL_VERSION` bump to 4; new §6 sub-section in `PROTOCOL.md`.
- `server-behavior`: New `RequestReplay` handling requirement (preconditions, per-seat masking guarantees, event ordering relative to live broadcasts).
- `client-core`: Deterministic replay of a `ReplayEvents` payload into `ClientCore` to reconstruct `CurrentHand`.

## Impact

- `crates/poker-engine/src/net/protocol.rs` — add variants, bump version.
- `crates/poker-engine/src/net/PROTOCOL.md` — document new verb.
- `crates/poker-server/src/session.rs` — handle `RequestReplay`, validate, emit replay.
- `crates/poker-client-core/src/core.rs` — handle `ReplayEvents`, feed events into state machine.
- `crates/poker-client-core/src/view.rs` — ensure `CurrentHand` reconstruction is complete.
- All client crates that reference `PROTOCOL_VERSION` for handshake — update expected value in tests.
- Integration tests in `tests/` and `poker-server/tests/` — cover replay success, rejection, and masking.

## Non-goals

- Client-side request initiation UI (the Tauri shell will add a reconnect button later).
- Persisting replay buffers to disk; replay is only for in-flight hands.
- Replay of completed hands (history viewer is a future feature).
