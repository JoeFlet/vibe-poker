use serde::{Deserialize, Serialize};

use crate::game::Action;

/// Discrete action set for solver policies. Concrete engine `Action`s are
/// mapped into this space via a concretizer (see `StrategyAdapter`).
///
/// The current set is intentionally minimal — enough to represent preflop
/// trees in early MCCFR experiments. Additional raise sizes (e.g. half-pot,
/// two-pot) will be added here when training scale demands it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum AbstractAction {
    Fold = 0,
    /// Call when facing a bet; Check when nothing is owed.
    Call = 1,
    /// Pot-sized raise (total bet = current_bet + pot_after_call).
    Raise = 2,
    AllIn = 3,
}

impl AbstractAction {
    pub const COUNT: usize = 4;

    pub const ALL: [AbstractAction; Self::COUNT] = [
        AbstractAction::Fold,
        AbstractAction::Call,
        AbstractAction::Raise,
        AbstractAction::AllIn,
    ];

    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }

    /// Classify a concrete engine `Action` into its abstract bucket.
    ///
    /// Lossy by design: `Raise(n)` collapses to `Raise` regardless of size.
    /// This is sound for self-play against our own concretizer (which always
    /// emits pot-sized raises), but an exploit layer observing arbitrary
    /// opponents will need richer sizing buckets to track their ranges.
    pub fn from_action(action: Action) -> Self {
        match action {
            Action::Fold => AbstractAction::Fold,
            Action::Check | Action::Call => AbstractAction::Call,
            Action::Raise(_) => AbstractAction::Raise,
            Action::AllIn => AbstractAction::AllIn,
        }
    }
}
