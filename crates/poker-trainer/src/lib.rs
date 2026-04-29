//! Agent-training scaffolding split out of `poker-engine` in step 20.
//!
//! `poker-engine` is the rules library; this crate layers on the
//! optimisation tooling that the engine intentionally doesn't carry
//! (MCCFR solver, blueprint persistence, dataset aggregator) plus
//! the `poker_train` / `poker_play` / `poker_dataset` binaries that
//! drive them.

pub mod dataset;
pub mod solver;
