## Why

The deprecated `crates/poker-client` (egui replay viewer + old live client) is dead weight in the workspace. It was frozen at `PROTOCOL_VERSION = 3` with a note that it would be removed when v4 lands (DESIGN.md step 26). The new client architecture (step 25a–d: `poker-client-core`, transport-native, headless, plus the Tauri shell in `client/`) has been stable for weeks. The old crate pulls in `egui`/`eframe`/`tokio` dependencies for no reason, slows compilations, and its continued presence creates confusion about which client is canonical.

Documentation has also grown bloated and duplicated across `AGENTS.md`, `CLAUDE.md`, `DESIGN.md`, `README.md`, `docs/CLIENT_PRINCIPLES.md`, and per-crate READMEs. Some files contain contradictory commands (e.g., `CLAUDE.md` still lists `poker-client --replay`), and the client principles are misplaced in a standalone `docs/` file when they belong in `AGENTS.md`.

## What Changes

- **Remove `crates/poker-client`** from the workspace and delete its directory entirely.
- **Remove all references** to `poker-client` from `Cargo.toml`, `DESIGN.md`, `README.md`, per-crate READMEs, and source comments.
- **Delete `CLAUDE.md`** — outdated and superseded by `AGENTS.md`.
- **Delete `docs/CLIENT_PRINCIPLES.md`** — merge its load-bearing content into `AGENTS.md`.
- **Trim `README.md`** — remove the deprecated replay viewer block from the quick-start, shrink inter-crate contract explanations, clean up the roadmap.
- **Update `DESIGN.md`** — remove historical `poker-client` references, update step 19c to indicate supersession, update step 26 to remove the "deprecated client stays as v3 harness" clause.
- **Clean per-crate READMEs** — remove deprecated viewer references from `poker-engine`, `poker-server`, and `poker-trainer` readmes. Update `poker-trainer` source comments.

## Capabilities

### New Capabilities
*None — this is a cleanup/removal change with no new user-facing functionality.*

### Modified Capabilities
*None — existing behavior unchanged; only dead code and documentation are removed.*

## Impact

- `Cargo.toml`: removes `"crates/poker-client"` from `members`.
- `Cargo.lock`: drops `poker-client` and its `egui`/`eframe` dependency tree on next `cargo update`.
- Compile times improve (fewer crates in the workspace graph, no egui/eframe slow path).
- No functional change to any remaining crate or binary.
- `client/` submodule (Tauri shell) is unaffected.
