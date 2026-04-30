# Client architecture principles

These principles govern the in-tree Rust client (`poker-client-core` + `poker-client-transport-*` + `poker-client-headless`) and the SolidJS / Tauri shell that wraps them. They are load-bearing — work that violates one needs explicit discussion, not a workaround. They also shape what kind of changes are acceptable on the **server** side, since two of the three principles below are only attainable with server cooperation.

## 1. Strict correctness, validated by tests

Every state transition in `poker-client-core` has a unit test. Every end-to-end flow — register → lobby → seat → play → leave, plus all crash/reconnect paths — has an integration test that spins up `poker-server` in-process and drives a `poker-client-headless` instance through the full flow.

Integration tests live at the **workspace root** (`tests/`), not under any individual crate, because they cross crate boundaries. Per-crate `tests/` directories continue to hold tests that are scoped to a single crate (e.g. `poker-server/tests/security.rs`).

UI code is exempt only because it's a thin projection (see §2). Any logic that branches on game state belongs in the core, not the frontend. If you find yourself writing `if (gameState.pot > 0) { … }` in TypeScript, that's a sign the core should be exposing a derived field instead.

## 2. Thin UI

The frontend is a pure function of `ClientView` — the read-only projection emitted by `ClientCore::snapshot()`. The frontend MAY introduce visual delay for animations: a 600 ms card-flip is fine; the underlying state has already advanced. The frontend MUST NOT hold authoritative state of its own. Concretely:

- No "I think the pot is X but the core says Y" — the displayed pot is the core's pot.
- No "let me remember which button the user clicked" — intents flow through the core, which decides legality and emits effects.
- No client-side derived state caches that survive reload — recompute from `ClientView` on every render.

Anything the UI needs to remember either lives in the core or is reconstructable from server events.

### Animation budget

Visual state must not lag true state by more than **1000 ms**. A long animation that would exceed this budget is dynamically sped up or skipped outright. The headless harness exposes the true state directly without any animation layer, so its timing assertions are tight; the frontend is allowed slack within the budget.

## 3. Resilience to interruption

A client crash, force-quit, or device swap is recoverable by reconnecting and resyncing from the server — never by reading local state. The session key is the only persisted client-side artifact required to resume. Everything else (the in-flight hand, your hole cards, the table layout, the pot, betting history for the current hand) is reconstructed from the server's event stream after `Authenticate`.

**Scope of "current game state" is strictly the in-flight hand.** Hand history and lifetime stats are out of scope for the client for now; the only stats we surface are whatever rides on `Welcome.stats`.

This principle has two server-side requirements that need to land alongside the client work:

- **Hand replay verb** (DESIGN.md step 26). The server must be able to replay the current hand's event stream — including the reconnecting player's own hole cards — to a client that takes over a seat mid-hand. The current protocol's "we re-issue the pending Prompt and that's it" is insufficient; protocol bumps to v4.
- **Mid-hand atomicity** (DESIGN.md step 27). A hand's chip movements are durable only at `HandEnded`. If the server crashes mid-hand, the next start-up reads stacks from before the hand began; the partial hand is discarded. This is invariant: in-progress hands MUST NOT leave half-applied chip state in `lifetime_stats` or any other persisted aggregate.

Local caches (e.g. the most recent `TableList` snapshot for instant lobby render) are allowed as **performance optimizations**, but the core must behave identically when they are empty.

## Design rules

These aren't principles but they're how the principles get enforced in practice. Treat them as defaults: deviate only with a clear reason.

### Pure core, isolated transport

`poker-client-core` depends only on `poker-engine` (for the wire protocol types). It contains no `tokio`, no `tauri`, no `tracing-subscriber`, no `rmp-serde` (the transport handles framing). It is fully synchronous and deterministic: same `(Intent | inbound ServerMessage)` sequence → same effect output and same `ClientView`. This is what makes principle 1 cheap — every test is a few lines of "feed it these events, assert the snapshot."

Transports (`poker-client-transport-native`, future `poker-client-transport-browser`) own all I/O, all platform plumbing, and all session-key persistence. They communicate with the core only via `Intent` in / `Effect` out.

### Verbose, structured logging

One of the few things the Flutter prototype got right was that pervasive logging of inbound and outbound events with timestamps and ordering markers was load-bearing for debugging — especially when event order was the bug. The Rust client preserves this:

- The core emits a `Log` effect at every state transition.
- Transports emit structured logs at every wire send / receive with a monotonic sequence number.
- The headless harness records the full effect stream so failed assertions can dump "here's exactly what the core saw and emitted."

Use `tracing` with structured fields, not stringly-typed `format!` lines.

### Tauri 2 + SolidJS + Vite + TypeScript + pnpm

The frontend shell lives in the `client/` git submodule (separate repo). It owns its own toolchain — JS dependencies don't sit on `cargo build`. Tauri 2 because Tauri 1 has no mobile path. SolidJS via Vite + TypeScript. pnpm for package management.

The Tauri Rust glue depends on `poker-client-core` and `poker-client-transport-native`. Its job is to forward intents from the frontend to the core, pump effects, and expose `snapshot()` results back to the frontend over Tauri commands. It is itself thin; non-trivial logic still belongs in the core.

## Lessons from the Flutter prototype

A short post-mortem so the lessons are durable:

- **Don't intertwine UI with state logic.** The Flutter prototype mixed widget state with game state, and bugs surfaced in both layers simultaneously. The Rust core / TS UI split here is precisely intended to make this impossible.
- **Pick a UI framework that fits the application's data flow.** Flutter's stateful-widget model fights an event-stream-driven game. SolidJS's fine-grained reactivity over a top-level store fits much better.
- **Verbose logging pays for itself fast** when the bug class is "events arrived in the wrong order." Keep it on by default during development; gate noisy logs behind a level rather than a feature flag.
