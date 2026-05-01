## 1. Remove deprecated `poker-client` crate

- [x] 1.1 Remove `"crates/poker-client"` from `Cargo.toml` workspace members
- [x] 1.2 Delete `crates/poker-client/` directory entirely
- [x] 1.3 Verify `cargo test --workspace` still compiles (poker-client not referenced)

## 2. Consolidate documentation

- [x] 2.1 Delete `CLAUDE.md` — outdated, superseded by `AGENTS.md`
- [x] 2.2 Delete `docs/CLIENT_PRINCIPLES.md` — merge content into `AGENTS.md`
- [x] 2.3 Remove `docs/` directory (now empty)
- [x] 2.4 Update `AGENTS.md` with merged client principles and remove `poker-client` from crate map
- [x] 2.5 Trim `README.md`: remove deprecated replay viewer from quick-start, shrink inter-crate contracts, update roadmap

## 3. Clean per-crate references

- [x] 3.1 Remove `poker-client` from `crates/poker-engine/README.md` dependents list
- [x] 3.2 Update `crates/poker-server/README.md`: remove smoke-test command, update persistence note
- [x] 3.3 Update `crates/poker-trainer/README.md`: remove deprecated replay viewer reference, update source comments in `poker_play.rs` and `poker_dataset.rs`

## 4. Update DESIGN.md

- [x] 4.1 Shorten step 19c to indicate supersession by Tauri client
- [x] 4.2 Remove "deprecated client stays as v3 harness" clause from step 26
- [x] 4.3 Clean `poker-client` references from steps 16, 17, 22
- [x] 4.4 Update docs/CLIENT_PRINCIPLES.md link to point at AGENTS.md

## 5. Verify workspace health

- [x] 5.1 Run `cargo test --workspace` and confirm all tests pass
- [x] 5.2 Run `cargo check --workspace` to confirm clean compile
- [x] 5.3 Confirm `Cargo.lock` no longer references `poker-client` (will prune on next update)
