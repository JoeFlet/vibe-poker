# poker-client-headless

Test-friendly client harness used by the workspace-root full-flow tests and as a scripted unit-testing tier. Three layers:

| Module | What it provides |
|---|---|
| [`harness`](src/harness.rs) | `HeadlessClient` — wraps `ClientCore` with effect + snapshot logs; scripts inject `Intent`s and `ServerMessage`s directly, bypassing any transport |
| [`transport`](src/transport.rs) | `InMemoryTransport` (implements the `Transport` trait), `InMemoryConnections` listener, `InMemoryServer` peer — a complete in-memory channel pair; use `in_memory_pair()` to construct |
| [`scenario`](src/scenario.rs) | `Step::{Issue, Receive, ExpectPhase, Expect}`, `Script = Vec<Step>`, `Driver::run` / `Driver::run_and_assert`, serde-backed `FileStep` / `FileScript` for CLI reproduction files |

## `HeadlessClient`

Drives `ClientCore` with no transport. Use for unit-speed tests of protocol logic:

```rust
let mut h = HeadlessClient::new(None);
h.intent(Intent::Connect { addr: "x".into() });
h.assert_phase(&Phase::Connecting);
h.inbound(ServerMessage::Welcome { ... });
h.assert_phase(&Phase::Lobby);
```

Effect log and snapshot log are retained across every call for failure output.

## `InMemoryTransport`

An in-memory `Transport` that pairs with an `InMemoryServer` — no sockets, no ports, unit speed. Use it with `NativeClient::with_transport` to test the full runtime facade in isolation from the network:

```rust
let (transport, mut conns) = in_memory_pair();
let client = NativeClient::with_transport(transport, session_store).unwrap();
client.issue(Intent::Connect { addr: "test".into() });
// Grab the server side
let mut server = conns.next().await.unwrap();
// Script the server
server.send(ServerMessage::Welcome { ... }).unwrap();
// Assert on the client
wait_for(&client, "Lobby", Duration::from_secs(1), |v| v.phase == Phase::Lobby);
```

## Scenario driver

For linear test scripts without manually calling `intent` / `inbound`:

```rust
let script = vec![
    Step::Issue(Intent::Connect { addr: "x".into() }),
    Step::ExpectPhase(Phase::Connecting),
    Step::Receive(ServerMessage::Welcome { ... }),
    Step::ExpectPhase(Phase::Lobby),
    Step::expect("username set", |v| v.username.is_some()),
];
let mut h = HeadlessClient::new(None);
Driver::run_and_assert(&mut h, &script);
```

## CLI reproduction binary

```sh
cargo run -p poker-client-headless -- scenario.json
```

Loads a `FileScript` JSON file (serde-serialised `Issue` / `Receive` steps), replays it, and prints a one-line summary per step. Use `FileStep::from_step` + `save_file_script` to capture a failing test's exact input sequence as a committed repro file.

## Tests

```sh
cargo test -p poker-client-headless
```

| Location | Coverage |
|---|---|
| `src/harness.rs` | Startup, drive-to-lobby |
| `src/scenario.rs` | Linear success, stop-at-first-failure, panic message, `FileScript` round-trip, predicate discard |
| `src/transport.rs` | `InMemoryTransport` open, send/recv/close, dropped-listener error |
| `tests/in_memory_runtime.rs` | `NativeClient` + `InMemoryTransport`: Register → Lobby + session persistence; server close → `Phase::Ended` |
