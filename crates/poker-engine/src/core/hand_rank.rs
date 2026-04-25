use serde::{Deserialize, Serialize};

/// Opaque hand strength value. Higher = stronger. Directly comparable with `>`.
///
/// Encoding (u32): bits [31:28] = category (0–8), bits [27:0] = tiebreakers (7 × 4-bit nibbles).
/// PHEval produces values in 1..=7462; when that evaluator is wired in the NaiveEvaluator
/// values can be discarded — the trait guarantees only relative ordering.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct HandRank(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum HandCategory {
    HighCard = 0,
    OnePair,
    TwoPair,
    ThreeOfAKind,
    Straight,
    Flush,
    FullHouse,
    FourOfAKind,
    StraightFlush,
}

impl HandRank {
    pub fn category(self) -> HandCategory {
        // Upper 4 bits encode the category (0–8).
        let cat = (self.0 >> 28) as u8;
        unsafe { std::mem::transmute(cat.min(8)) }
    }
}
