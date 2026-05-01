## Context

The workspace currently contains `crates/poker-client` — an `egui`/`eframe`-based replay viewer and live client that was deprecated in DESIGN.md step 24 and frozen at `PROTOCOL_VERSION = 3`. The replacement architecture (step 25a–d) — `poker-client-core`, `poker-client-transport-native`, `poker-client-headless`, and the Tauri shell in `client/` — is stable. The old crate is pure dead weight: it pulls in egui/eframe/tokio, clutters the workspace graph, and its README and source comments create confusion about which client is canonical.

Documentation sprawl is a secondary problem. `CLAUDE.md`, `docs/CLIENT_PRINCIPLES.md`, `AGENTS.md`, `README.md`, and `DESIGN.md` overlap in coverage. `CLAUDE.md` contains stale commands. `docs/CLIENT_PRINCIPLES.md` duplicates content that belongs in `AGENTS.md`.

## Goals / Non-Goals

**Goals:**
- Remove `crates/poker-client` entirely (directory, Cargo.toml membership, all references).
- Consolidate client architecture principles into `AGENTS.md`.
- Trim `README.md` and `DESIGN.md` of deprecated-client references and historical bloat.
- Update per-crate READMEs (`poker-engine`, `poker-server`, `poker-trainer`) to stop referencing the removed crate.

**Non-Goals:**
- No functional changes to any remaining crate.
- No changes to the `client/` Tauri shell submodule.
- No changes to the wire protocol.
- No migration of replay viewer functionality (step 26+ may add a new replay feature to the Tauri client).

## Decisions

1. **Delete rather than archive.** The crate is fully superseded; there is no intent to revive it. The git history preserves the code if needed.
2. **Merge CLIENT_PRINCIPLES into AGENTS.md.** AGENTS.md is already the canonical agent reference; splitting principles into a separate file in `docs/` was a historical accident. The three principles (strict correctness, thin UI, resilience) and design rules (pure core, verbose logging) move verbatim.
3. **Delete CLAUDE.md entirely.** It was created for a different tool and contains stale commands; AGENTS.md is the single source of truth for agent guidance.
4. **Soft-update DESIGN.md historical steps.** Steps 16, 17, 19c, 26 still mention `poker-client` because they describe historical work. Rather than rewrite history, shorten the references to indicate supersession and remove forward-looking clauses that assume the deprecated crate will persist.

## Risks / Trade-offs

- [Risk] Someone may still expect `cargo run -p poker-client -- --replay session.mp` to work.
  → Mitigation: README no longer documents this command. The log format is unchanged, so any future replay tool can consume the same `FileSink` files.
- [Risk] `Cargo.lock` stale entries for egui/eframe.
  → Mitigation: `cargo update` after workspace build will prune the lockfile automatically.
