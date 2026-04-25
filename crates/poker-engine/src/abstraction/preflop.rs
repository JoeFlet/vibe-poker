use crate::core::{Card, Rank};

/// A canonical (suit-isomorphic) preflop hand class.
///
/// Every starting hand maps to exactly one of `COUNT = 169` classes:
/// 13 pocket pairs, 78 suited non-pairs, and 78 offsuit non-pairs.
/// Invariant: the first `Rank` of a non-pair is strictly higher than the second.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum PreflopClass {
    Pair(Rank),
    Suited(Rank, Rank),
    Offsuit(Rank, Rank),
}

impl PreflopClass {
    /// Total number of distinct preflop classes.
    pub const COUNT: u16 = 169;

    /// Canonicalize two hole cards into their suit-isomorphic class.
    pub fn from_hole(hole: [Card; 2]) -> Self {
        let (high, low) = if hole[0].rank() >= hole[1].rank() {
            (hole[0], hole[1])
        } else {
            (hole[1], hole[0])
        };
        if high.rank() == low.rank() {
            PreflopClass::Pair(high.rank())
        } else if high.suit() == low.suit() {
            PreflopClass::Suited(high.rank(), low.rank())
        } else {
            PreflopClass::Offsuit(high.rank(), low.rank())
        }
    }

    /// Stable bucket index in `0..COUNT`.
    ///
    /// Layout: pairs first (AA=0, KK=1, …, 22=12), then suited non-pairs in
    /// descending high/low order (AKs=13, AQs=14, …, 32s=90), then offsuit
    /// non-pairs in the same order (AKo=91, …, 32o=168).
    pub fn index(self) -> u16 {
        match self {
            PreflopClass::Pair(r) => 12 - (r as u16),
            PreflopClass::Suited(h, l) => 13 + non_pair_offset(h, l),
            PreflopClass::Offsuit(h, l) => 91 + non_pair_offset(h, l),
        }
    }
}

fn non_pair_offset(high: Rank, low: Rank) -> u16 {
    debug_assert!(high > low, "non_pair_offset requires high > low");
    let h = high as u16;
    let l = low as u16;
    // Rows are indexed by high rank descending. Row for high h starts after
    // every row with a higher high; the number of entries in rows above is
    // sum_{h' = h+1..=12} h' = 78 - h*(h+1)/2.
    let row_start = 78 - h * (h + 1) / 2;
    row_start + (h - 1 - l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Card, Suit};

    fn c(r: Rank, s: Suit) -> Card {
        Card::new(r, s)
    }

    #[test]
    fn known_hands_map_to_expected_indices() {
        assert_eq!(PreflopClass::from_hole([c(Rank::Ace, Suit::Spades), c(Rank::Ace, Suit::Hearts)]).index(), 0);
        assert_eq!(PreflopClass::from_hole([c(Rank::King, Suit::Spades), c(Rank::King, Suit::Hearts)]).index(), 1);
        assert_eq!(PreflopClass::from_hole([c(Rank::Two, Suit::Clubs), c(Rank::Two, Suit::Diamonds)]).index(), 12);

        // AKs / AKo
        assert_eq!(PreflopClass::from_hole([c(Rank::Ace, Suit::Spades), c(Rank::King, Suit::Spades)]).index(), 13);
        assert_eq!(PreflopClass::from_hole([c(Rank::Ace, Suit::Spades), c(Rank::King, Suit::Hearts)]).index(), 91);

        // 32s / 32o (bottom of each non-pair block)
        assert_eq!(PreflopClass::from_hole([c(Rank::Three, Suit::Clubs), c(Rank::Two, Suit::Clubs)]).index(), 90);
        assert_eq!(PreflopClass::from_hole([c(Rank::Three, Suit::Clubs), c(Rank::Two, Suit::Diamonds)]).index(), 168);
    }

    #[test]
    fn order_independent() {
        let a = c(Rank::Ace, Suit::Hearts);
        let k = c(Rank::King, Suit::Hearts);
        assert_eq!(
            PreflopClass::from_hole([a, k]),
            PreflopClass::from_hole([k, a])
        );
    }

    #[test]
    fn every_hole_maps_into_one_of_169_classes() {
        // Enumerate all C(52, 2) = 1326 unordered hole pairs, bucket them,
        // and verify the combo distribution:
        //   13 pair classes   × 6 combos  = 78
        //   78 suited classes × 4 combos  = 312
        //   78 offsuit classes × 12 combos = 936
        //   total = 1326
        let mut counts = [0u32; PreflopClass::COUNT as usize];
        for a in 0u8..52 {
            for b in (a + 1)..52 {
                let hole = [Card(a), Card(b)];
                let idx = PreflopClass::from_hole(hole).index();
                counts[idx as usize] += 1;
            }
        }

        assert_eq!(counts.iter().sum::<u32>(), 1326);
        for (i, &count) in counts.iter().enumerate() {
            let expected = if i < 13 {
                6 // pair: C(4,2)
            } else if i < 91 {
                4 // suited: one per suit
            } else {
                12 // offsuit: 4 × 3
            };
            assert_eq!(
                count, expected,
                "class {i} has {count} combos, expected {expected}"
            );
        }
    }

    #[test]
    fn indices_are_dense_and_unique() {
        // Every class that can be constructed by enumerating all suited +
        // offsuit pairs plus all pocket pairs must produce a unique index,
        // and those indices must cover 0..169 exactly.
        let mut seen = [false; PreflopClass::COUNT as usize];
        for r in 0u8..13 {
            let rank = Card(r << 2).rank();
            let idx = PreflopClass::Pair(rank).index() as usize;
            assert!(!seen[idx], "duplicate index {idx} for pair {rank:?}");
            seen[idx] = true;
        }
        for h in 1u8..13 {
            for l in 0u8..h {
                let hi = Card(h << 2).rank();
                let lo = Card(l << 2).rank();
                for (variant, _) in [
                    (PreflopClass::Suited(hi, lo), "s"),
                    (PreflopClass::Offsuit(hi, lo), "o"),
                ] {
                    let idx = variant.index() as usize;
                    assert!(!seen[idx], "duplicate index {idx} for {variant:?}");
                    seen[idx] = true;
                }
            }
        }
        assert!(seen.iter().all(|&b| b), "some indices were never produced");
    }
}
