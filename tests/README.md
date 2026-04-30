# Workspace-root full-flow tests

This directory holds end-to-end tests that **cross crate boundaries** — typically `poker-server` (in-process) driven by `poker-client-headless`. They live here, not under any individual crate's `tests/`, so the dependency direction is unambiguous: tests depend on every workspace member they need.

Per-crate `tests/` directories continue to hold tests scoped to a single crate (e.g. [`crates/poker-server/tests/security.rs`](../crates/poker-server/tests/security.rs)).

## Running

Cargo doesn't natively pick up integration tests from the workspace root, so they're wired in as a top-level test crate via `tests/Cargo.toml` (added when the first full-flow test lands in step 25c). Until that lands, this directory is a documented placeholder.

## What goes here

A test belongs at this level if it:

- Spins up `poker-server` in-process and drives one or more headless clients through a real flow, or
- Asserts cross-crate ordering / contract guarantees that no single crate's tests can verify (e.g. "if the server crashes mid-hand, the database state matches a never-started hand" — DESIGN.md step 27).

A test belongs in a per-crate `tests/` directory if it's checking a single crate's behaviour against its public surface (the existing `poker-server` security and table-play tests are good examples).
