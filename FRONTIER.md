# FRONTIER

The bleeding edge of the project: what's in flight, what's broken, and what's
next. For the shape of the whole system see [DESIGN.md](DESIGN.md); for
in-depth per-module design see each crate's README.

_Last swept: 2026-09-22._

## Snapshot

Everything in the historical build-out is implemented: the rules engine, the
MCCFR trainer, the live TCP server (auth, persistence, reconnect, hand replay,
force-fold-on-leave, protocol v5), and the client stack (pure core, native
transport, headless harness, Tauri shell). The workspace builds clean and the
full test suite passes **except one deterministic failure** (below) caused by a
real concurrency bug. There are no active (non-archived) OpenSpec change
proposals — no spec work is mid-flight.

## Build & test health

- `cargo build --workspace` — clean (a few dead-code warnings only:
  unused `dealer`, `stack`, `NullSink` import).
- `cargo test --workspace` — all green **except** the failure below.

### 🔴 Concurrent-registration bug — `SQLITE_BUSY`

Failing test: `poker-server` → `table_play::showdown_reveals_both_hole_cards`
(deterministic; its sibling `full_hand_fold_through_wire` passes because it
registers clients *sequentially*).

Root cause: [`open_pool`](crates/poker-server/src/db.rs) opens the SQLite pool
in WAL with `max_connections(8)` but sets **no `busy_timeout`**, so two
concurrent `Register` write transactions collide and one fails immediately with
`SQLITE_BUSY`. That surfaces to the client as
`Rejected { reason: "internal error" }`
([`registry_error_to_reason`](crates/poker-server/src/session.rs)), and the
rejected client then times out waiting to be seated.

This is a production correctness issue, not just a test artifact: two real users
registering at the same instant can hit it.

Fix direction: set a busy timeout on the connect options in `open_pool` (e.g.
`.busy_timeout(Duration::from_secs(5))` or `PRAGMA busy_timeout`), plus a
regression test that registers several users concurrently.

## Next work (nothing below is started)

Roughly in order of how load-bearing they are.

1. **Fix the concurrent-registration DB bug** (above). Small and clear; the
   only red in the suite.

2. **Transport scheduling — `Effect::Schedule` is a no-op.**
   [`runtime.rs`](crates/poker-client-transport-native/src/runtime.rs) drops all
   `Schedule` effects with a TODO, so client-side heartbeat and prompt-deadline
   ticks are unimplemented. Only "harmless on the happy path" because the
   server's 60s heartbeat timeout is generous. This is the main missing piece of
   client resilience.

3. **Solver: postflop card abstraction.** The biggest gap in the trainer. All
   postflop info sets share `bucket = 0`
   ([`info_set.rs`](crates/poker-trainer/src/solver/info_set.rs)), so postflop
   play is weak. Preflop is complete (169-class canonical). Needs
   equity/distribution bucketing.

4. **Solver: exploitability / best-response.** No best-response solver yet;
   convergence is only a head-to-head smoke test. A prerequisite before the
   trainer can be called production-ready.

5. **Richer action abstraction.** Only a single pot-sized raise today; half-pot
   / two-pot / distinct all-in sizings are a future widening of `AbstractAction`.

6. **Exploit layer (planned, not begun).** Opponent profiling on the `StatsSink`
   substrate + best-response mixing over the blueprint, slotting in behind the
   `StrategyAgent` trait. Depends on 3 & 4.

7. **Client polish.** The Tauri/SolidJS shell is a functional-loop MVP (connect
   → register → lobby → seat → act, plus mid-hand-leave confirmation and
   per-seat street-bet display). No card art, no positional layout, no
   animations.

## Deliberately out of scope (context, not TODOs)

- **Multi-player MCCFR.** Heads-up only by design — CFR gives a correlated (not
  Nash) equilibrium at 3+ players. The intended path is a heads-up blueprint +
  real-time depth-limited search in the exploit layer (à la Pluribus).
- **Deprecated frontends.** The old in-tree client and the Flutter experiment
  are gone (Flutter survives only as the `flutter-experiment` tag).
