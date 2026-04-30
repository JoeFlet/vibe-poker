# poker-client-transport-native

Tokio TCP transport + filesystem session-key persistence for [`poker-client-core`](../poker-client-core/). Implements the `Transport` trait so the same interface can be satisfied by a future `poker-client-transport-browser` (WASM + WebSocket + IndexedDB) without touching the core.

## Modules

| Module | What it provides |
|---|---|
| [`transport`](src/transport.rs) | `Transport` trait (factory: `open(addr) -> TransportHandle`), `TransportHandle` (channel-backed: `send(ClientMessage)` / `recv() -> TransportIn`), `TransportIn::{Connected, Message, Lost}`, `Closed` error |
| [`native`](src/native.rs) | `NativeTransport` — `TcpStream::connect` + `set_nodelay` + a pump task (`tokio::select!` on reader / writer halves) that forwards framed messages until EOF |
| [`session`](src/session.rs) | `SessionStore` — atomic write-then-rename session-key persistence, lazy parent-dir creation, empty-file-is-`None` semantics |
| [`runtime`](src/runtime.rs) | `NativeClient` — the facade the Tauri shell and integration tests consume |

## `NativeClient`

The primary deliverable. Owns a `ClientCore`, a `Transport`, and a `SessionStore` behind a thread-safe synchronous API:

```rust
// Construct (loads any persisted session key from disk)
let client = NativeClient::new(session_path)?;
// or with a custom transport (e.g. InMemoryTransport from poker-client-headless)
let client = NativeClient::with_transport(transport, session_store)?;

// All calls are sync, callable from any thread
client.issue(Intent::Connect { addr: "127.0.0.1:7878".into() });
client.issue(Intent::Register { ... });

let view: ClientView = client.snapshot();    // latest view, O(clone)
let logs: Vec<LogEntry> = client.drain_logs(); // buffered log entries
```

State transitions happen on a dedicated worker thread running its own current-thread tokio runtime. `Effect::OpenConnection` spawns the TCP pump task; `Effect::PersistSessionKey` calls `SessionStore::save` / `clear` atomically; `Effect::Log` routes to `tracing` and the host log buffer.

## `Transport` trait

```rust
pub trait Transport: Clone + Send + 'static {
    type OpenError: std::error::Error + Send + Sync + 'static;
    async fn open(&self, addr: String) -> Result<TransportHandle, Self::OpenError>;
}
```

`TransportHandle::from_channels(outbound_tx, inbound_rx)` is the constructor for out-of-crate implementors (used by [`InMemoryTransport`](../poker-client-headless/src/transport.rs)).

## Tests

```sh
cargo test -p poker-client-transport-native
```

| Test file | Coverage |
|---|---|
| `src/native.rs` | Loopback roundtrip, connect-failure path, handle-drop pump teardown |
| `src/session.rs` | Save / load roundtrip, missing parent dir, empty-file-is-None |
| `tests/runtime.rs` | `NativeClient` + real `poker-server` in-process: Register → Welcome → Lobby, session key persisted to disk |
