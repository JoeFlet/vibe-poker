//! Solver scaffolding for MCCFR and exploit-aware play.
//!
//! See DESIGN.md section "Solver & Exploit Architecture" for the full design.
//! This module defines the shared types — abstract actions, info-set keys, and
//! the `StrategyAgent` trait returning action probabilities. The MCCFR trainer
//! and exploit layer build on top of these.

pub mod action;
pub mod info_set;
pub mod mccfr;
pub mod persistence;
pub mod regret;
pub mod strategy;

pub use action::AbstractAction;
pub use info_set::{InfoSet, Position};
pub use mccfr::MccfrTrainer;
pub use persistence::{
    load_blueprint, save_blueprint, BlueprintLoadError, BlueprintSaveError, ABSTRACTION_TAG,
    BLUEPRINT_SCHEMA_VERSION,
};
pub use regret::{regret_matching, BlueprintStrategy, RegretEntry, RegretTable};
pub use strategy::{ActionProbs, StrategyAdapter, StrategyAgent, UniformRandomStrategy};
