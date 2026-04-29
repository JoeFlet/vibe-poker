use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use poker_engine::abstraction::PreflopClass;
use poker_engine::agent::Observation;
use poker_engine::game::Street;
use crate::solver::action::AbstractAction;

/// Rotation-invariant seat position. `0` = dealer (button), `1` = small blind,
/// `2` = big blind, and so on around the table.
pub type Position = u8;

/// A solver information set: the rotation-invariant key that identifies a
/// decision point for regret/strategy storage.
///
/// Two decisions share an InfoSet iff they are interchangeable from the acting
/// player's perspective — same street, same relative position, same card
/// bucket, and same abstract action history.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct InfoSet {
    pub street: Street,
    pub position: Position,
    /// Card abstraction bucket. Preflop uses `PreflopClass::index()` (0..169);
    /// postflop is a placeholder (0) until the postflop abstraction lands.
    pub bucket: u16,
    /// Abstract action history from the start of the hand through the
    /// decision point (exclusive). Fixed-size inline up to 12 steps —
    /// larger histories spill to the heap but are rare.
    pub history: SmallVec<[AbstractAction; 12]>,
}

impl InfoSet {
    /// Build an InfoSet from the current observation and an abstract action
    /// history collected by the caller (typically by a `StrategyAdapter`).
    pub fn from_observation(obs: &Observation<'_>, history: &[AbstractAction]) -> Self {
        let n = obs.players.len();
        let position = relative_position(obs.position, obs.dealer, n);
        let bucket = match obs.street {
            Street::Preflop => PreflopClass::from_hole(obs.hole_cards).index(),
            // Postflop abstraction is deferred to a later stage; all postflop
            // decisions currently collapse into a single bucket.
            _ => 0,
        };
        InfoSet {
            street: obs.street,
            position,
            bucket,
            history: SmallVec::from_slice(history),
        }
    }
}

#[inline]
fn relative_position(seat: usize, dealer: usize, n: usize) -> Position {
    debug_assert!(n > 0 && seat < n && dealer < n);
    ((seat + n - dealer) % n) as Position
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_position_wraps_cleanly() {
        assert_eq!(relative_position(0, 0, 6), 0);
        assert_eq!(relative_position(1, 0, 6), 1);
        assert_eq!(relative_position(0, 2, 6), 4);
        assert_eq!(relative_position(5, 5, 6), 0);
        assert_eq!(relative_position(2, 5, 6), 3);
    }
}
