use serde::{Deserialize, Serialize};
use thiserror::Error;

/// All actions an agent may submit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Fold,
    Check,
    Call,
    /// Raise to this total bet size (must satisfy min-raise rules).
    Raise(u32),
    AllIn,
}

/// The set of actions that are currently legal for the acting player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalActions {
    pub can_check: bool,
    pub can_call: bool,
    pub call_amount: u32,
    pub can_raise: bool,
    pub min_raise: u32,
    pub max_raise: u32,
    /// AllIn is always legal when a player has chips.
    pub all_in_amount: u32,
}

impl LegalActions {
    pub fn is_legal(&self, action: Action) -> bool {
        match action {
            Action::Fold => true,
            Action::Check => self.can_check,
            Action::Call => self.can_call,
            Action::Raise(amount) => {
                self.can_raise && amount >= self.min_raise && amount <= self.max_raise
            }
            Action::AllIn => self.all_in_amount > 0,
        }
    }
}

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("illegal action {0:?}: {1}")]
    Illegal(Action, &'static str),
    #[error("no player to act")]
    NoActingPlayer,
}
