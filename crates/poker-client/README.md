# poker-client

> **Status: deprecated — smoke-test harness only.** Frozen at `PROTOCOL_VERSION = 3`.
> The production client is the Tauri 2 + SolidJS app in [client/](../../client/).
> This crate stays in-tree as a known-good test counterparty for `poker-server`
> and as a functional replay viewer for `FileSink` logs. **Only protocol-compatibility
> bug fixes land here.** It will be removed when `PROTOCOL_VERSION` bumps to 4
> (DESIGN.md step 26).

An `egui`/`eframe` desktop app with two modes:

## Replay viewer

```sh
cargo run -p poker-client -- --replay session.mp
```

Loads any `FileSink` log (engine output, server-recorded hands, dataset matchups) and lets you scrub through the events. ⏮◀▶⏭ buttons step the cursor; the slider jumps anywhere; clicking an event in the side panel snaps to it.

Each frame derives a [Snapshot](src/snapshot.rs): hand id / dealer / street / pot, board cards, per-seat hole cards with folded / all-in / committed flags, action log, and the final `HandResult` block.

## Live mode (smoke test only)

```sh
cargo run -p poker-server &
cargo run -p poker-client -- --connect 127.0.0.1:7878 --username alice
```

Connects to [poker-server](../poker-server/), browses the lobby, sits at a table, and plays. The egui side stays purely synchronous; network I/O lives in [LiveClient](src/live_net.rs) (worker thread + current-thread tokio runtime, paired `mpsc` channels). [LiveApp](src/live.rs) is the state machine: Connecting → Lobby → Seated → Ended.

For a full-featured live client, use the [client/](../../client/) Tauri shell instead.

## Tests

```sh
cargo test -p poker-client                                   # all
cargo test -p poker-client --bin poker-client live_client    # live-mode smoke
```
