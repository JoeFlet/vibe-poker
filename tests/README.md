# Workspace-root full-flow tests

End-to-end tests that **cross crate boundaries** — `poker-server` running in-process driven by `poker-client-headless` over real TCP loopback. They live here, not under any individual crate's `tests/`, so the dependency direction is unambiguous and the test binary can import from every workspace member it needs.

Per-crate `tests/` directories hold tests scoped to a single crate (e.g. [`crates/poker-server/tests/security.rs`](../crates/poker-server/tests/security.rs) for wire-layer hardening, `crates/poker-client-transport-native/tests/runtime.rs` for the `NativeClient` integration tests).

## Running

```sh
cargo test --workspace          # includes these tests alongside all per-crate suites
cargo test -p poker-full-flow-tests  # just the cross-crate tests
```

## Current tests

| File | What it covers |
|---|---|
| `tests/handshake.rs` | Register → lobby, list tables, join table, two clients seated at a heads-up table |

## Shared harness (`src/lib.rs`)

`LiveHarness` wraps a [`HeadlessClient`](../crates/poker-client-headless/) with a real `TcpStream` to a `spawn_server()` instance. The reader half is pumped continuously into an `mpsc` channel by a background task so tests block on `wait_for(predicate, timeout, label)` rather than counting messages. Failed waits print the phase and the full effect log.

`spawn_server()` / `spawn_server_with(TableConfig)` stand up a single-table `poker-server` against a `tempdir` SQLite database on a random ephemeral port.

## What belongs here

A test belongs at this level if it:

- Spins up `poker-server` in-process and drives one or more headless clients through a real flow, **or**
- Asserts cross-crate ordering / contract guarantees that no single crate can verify alone (e.g. DESIGN.md step 27: "if the server crashes mid-hand, the database state matches a never-started hand").

Single-crate behaviour tests belong in that crate's own `tests/` directory.
