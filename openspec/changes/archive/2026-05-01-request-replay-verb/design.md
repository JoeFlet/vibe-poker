## Context

The server (`poker-server`) currently runs hands via `poker_engine::Engine`, broadcasting `EngineEvent`s through a `BroadcastSink` that fans messages per-seat with hole-card masking. A client that reconnects mid-hand receives a fresh `SeatLink`, a re-issued `Prompt` if it's their turn, but no history of prior hand events. Until `HandEnded` the reconnected client has an empty `CurrentHand`, which is a poor user experience and violates the crash-resilience principle (client-core spec: "recoverable solely by reconnecting and resyncing").

There is no in-memory buffer of per-seat events on the server today; the `BroadcastSink` pushes events directly to `SeatLink` senders. The `Table` struct tracks its current `HandRunner`, but the runner does not retain the event stream.

## Goals / Non-Goals

**Goals:**
- Add a `RequestReplay { hand_id }` client verb that a seat can send at any time.
- Server replies with a `ReplayEvents { hand_id, events }` containing the exact ordered `ServerMessage` stream that seat would have received from `HandStarted` through the most recent broadcast.
- The replayed events MUST be byte-identical (same masking) to the original live stream for that seat.
- After replay, live broadcasts continue in order (replay ends before new events).
- Bump `PROTOCOL_VERSION` to 4.
- `poker-client-core` can ingest `ReplayEvents` by feeding each inner `ServerMessage` through `handle_inbound` deterministically.

**Non-Goals:**
- Adding a reconnect/replay button in the Tauri shell (UI is out of scope).
- Persisting replay buffers to SQLite or disk.
- Replaying completed hands (history viewer).
- Changing the engine's `EngineEvent` emission order or semantics.

## Decisions

### 1. Where to buffer per-seat event streams
**Decision**: Add a `Vec<ServerMessage>` buffer inside each seat's `SeatLink` (or a parallel map in `Table`). Each time `BroadcastSink::send_event` fans an `EngineEvent` into a `ServerMessage` for a seat, append that `ServerMessage` to the seat's replay buffer.

**Rationale**: This keeps replay data co-located with the connection state that needs it. It reuses the existing per-seat masking logic without introducing a second fan-out path. When the hand ends, the buffer is dropped with the `HandRunner`.

**Alternative considered**: Buffer `EngineEvent`s globally and re-run `BroadcastSink` logic at replay time. Rejected because it complicates masking (the sink may need knowledge of current seat positions) and would produce logically identical but potentially not byte-identical output.

### 2. Ordering guarantee relative to live broadcasts
**Decision**: `ReplayEvents` is sent synchronously in the session loop when `RequestReplay` is handled. The `Table` actor holds an `RwLock`, and the session holds a read lock during replay retrieval. Live broadcasts from `HandRunner` happen in the table loop, also under the table lock. Therefore, the replay payload is a snapshot taken before any subsequent live events are sent.

**Rationale**: Simple, no extra queues. The client receives all past events, then resumes live flow.

**Risk**: A very long replay payload could briefly block live broadcasts if the table lock is held while cloning the buffer. Mitigation: the buffer is just a `Vec<ServerMessage>` clone; expected size is low hundreds of messages even for long hands.

### 3. Client-side replay ingestion
**Decision**: `poker-client-core` treats `ReplayEvents` like any other `ServerMessage`. Its `handle_inbound` implementation will iterate over `events` and apply each one in sequence, emitting `Effect::UpdateView` after each. No special "replay mode" state is introduced.

**Rationale**: Leverages the existing deterministic state machine. Because `ClientCore` is pure and sync, replaying 50 messages is identical to having received them live.

## Risks / Trade-offs

- **[Risk] Memory growth during long hands**: Each seat accumulates a copy of every broadcast. For 9 seats and 200 events, that's 9×200 `ServerMessage`s. Acceptable for a game server; completed hands already drop the `HandRunner` and buffers.
- **[Risk] Protocol version bump forces client rebuilds**: The Tauri shell and any headless tests must update their expected `PROTOCOL_VERSION`. This is by design (spec requirement).
- **[Risk] ReplayEvents payload could exceed frame size**: In pathological cases (very long hand, many seats), the replay payload could approach `MAX_FRAME_BYTES`. Mitigation: if this becomes an issue, future design can paginate; for now, msgpack compactness keeps it well under limit.

## Migration Plan

1. Land server changes (`protocol.rs`, `session.rs`, `table.rs`) and integration tests.
2. Land `poker-client-core` change to handle `ReplayEvents`.
3. Update all hard-coded `PROTOCOL_VERSION` assertions in tests (server and client).
4. Update `PROTOCOL.md` and crate READMEs that mention version 3.
5. The Tauri shell in `client/` will pick up the new protocol types automatically on next `cargo` build of the Rust glue, but the UI won't invoke `RequestReplay` until a later change.

## Open Questions

- Should `RequestReplay` be rejected with a specific error if the hand is already over? `HandEnded` is technically still "at the named hand" until the next `HandStarted`, so replay should succeed until the next hand begins. (Tentative: yes, allow it; the buffer exists until `HandRunner` is dropped.)
