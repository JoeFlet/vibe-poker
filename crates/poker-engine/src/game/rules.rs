use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BetVariant {
    NoLimit,
    PotLimit,
    FixedLimit,
}

/// Table configuration. Passed at game construction; immutable during a session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BettingRules {
    pub variant: BetVariant,
    pub small_blind: u32,
    pub big_blind: u32,
    /// Posted before blinds (0 = no ante).
    pub ante: u32,
    pub max_players: usize,
}

impl BettingRules {
    pub fn no_limit_holdem(small_blind: u32, big_blind: u32, max_players: usize) -> Self {
        BettingRules {
            variant: BetVariant::NoLimit,
            small_blind,
            big_blind,
            ante: 0,
            max_players,
        }
    }
}
