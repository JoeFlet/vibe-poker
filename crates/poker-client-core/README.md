# poker-client-core

Pure synchronous client state machine. No `tokio`, no I/O, no platform code — depends only on `poker-engine` for wire types.

This is the single source of truth for the client's authoritative state. The same determinism guarantee that makes the engine testable applies here: given the same `(Intent | inbound ServerMessage)` sequence, the core always produces the same `Vec<Effect>` and `ClientView`. That property is what makes every state transition cheaply unit-testable.

## Public API

```
ClientCore::new(initial_session_key: Option<String>) -> ClientCore
ClientCore::handle_intent(Intent) -> Vec<Effect>
ClientCore::handle_inbound(ServerMessage) -> Vec<Effect>
ClientCore::snapshot() -> ClientView   // cheap clone of current view
ClientCore::session_key() -> Option<&str>
```

## Key types

| Type | What it is |
|---|---|
| [`Intent`](src/intent.rs) | Things the host asks the core to do — `Connect`, `Register`, `AuthenticatePassword`, `AuthenticateSession`, `ListTables`, `JoinTable`, `LeaveTable`, `SubmitAction`, `Heartbeat`, `Disconnect`, `ConnectionOpened`, `ConnectionLost` |
| [`Effect`](src/effect.rs) | What the core asks the host to do — `OpenConnection`, `CloseConnection`, `Send(ClientMessage)`, `PersistSessionKey`, `Schedule`, `Log` |
| [`ClientView`](src/view.rs) | Read-only snapshot — `Phase`, `player_id`, `username`, `lifetime_stats`, `tables`, `seats`, `button`, `current_hand`, `last_action_rejection` |
| [`Phase`](src/view.rs) | `Disconnected` → `Connecting` → `Authenticating` → `Lobby` → `Seated { table_id, seat }` → `Ended { reason }` |

## Hosts

The core is consumed by two hosts in this workspace:

- **[poker-client-transport-native](../poker-client-transport-native/)** — wraps the core in `NativeClient`, which routes every `Effect` to a tokio TCP transport and a filesystem `SessionStore`.
- **[poker-client-headless](../poker-client-headless/)** — wraps the core in `HeadlessClient`, which records effect and snapshot logs for test assertions, with no transport at all.

The Tauri glue in [client/src-tauri/](../../client/src-tauri/) drives the core indirectly through `NativeClient`.

## Design rules (from [CLIENT_PRINCIPLES.md](../../../../docs/CLIENT_PRINCIPLES.md))

- The core contains no `Instant`, no randomness, no clock. Anything that needs wall time goes through `Effect::Schedule` so tests drive timing manually.
- `ClientView` is the whole authoritative client state. Hosts MUST NOT keep a parallel copy.
- The core emits a `Log` effect at every meaningful state transition. Hosts route these to `tracing` (native) or `console` (web).

## Tests

```sh
cargo test -p poker-client-core
```

The 22 unit tests in `src/core.rs` cover every state transition: connection lifecycle, lobby state, in-hand event projection, prompt + `SubmitAction`, `ActionRejected` re-arm.
