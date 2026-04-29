# poker-client

> **Status: deprecated reference / smoke-test harness.** Frozen at `PROTOCOL_VERSION = 3`. The next-generation playable client is being built in a separate repository and is expected to land here later as a git submodule (see [DESIGN.md](../../DESIGN.md) step 24). This crate stays in-tree only as a known-good smoke-test counterparty for `poker-server`. **Only protocol-compatibility bug fixes land here** — no UI features, no UX polish, no new modes.

An `egui`/`eframe` desktop app with two modes:

## Replay viewer

```sh
cargo run -p poker-client -- --replay session.mp
```

Loads any `FileSink` log (engine output, server-recorded hands, dataset matchups) and lets you scrub through the events. A cursor walks `events[0..=i]` and each frame derives a [Snapshot](src/snapshot.rs) — hand id / dealer / street / pot, board cards, per-seat hole cards with folded/all-in/committed flags, the action log, and the final `HandResult` block. ⏮◀▶⏭ buttons step the cursor; the slider jumps anywhere; clicking an event in the side panel snaps the cursor to it.

## Live mode

```sh
cargo run -p poker-server &
cargo run -p poker-client -- --connect 127.0.0.1:7878 --username alice
```

Connects to a [poker-server](../poker-server/), handshakes via `Hello`, browses the lobby, sits at a table, and plays. The egui side stays purely synchronous; network I/O lives in [LiveClient](src/live_net.rs), which owns a worker thread running a current-thread tokio runtime and exposes non-blocking `send` / `drain` (paired `mpsc::UnboundedChannel`s).

[LiveApp](src/live.rs) is the egui state machine: Connecting → Lobby → Seated → Ended. Both modes share [snapshot.rs](src/snapshot.rs) — replay slices `events[..=cursor]` and re-derives, live calls `Snapshot::apply(&event)` incrementally as `TableEvent`s arrive.

## Tests

```sh
cargo test -p poker-client                                    # all
cargo test -p poker-client --bin poker-client live_client     # live-mode smoke
```
