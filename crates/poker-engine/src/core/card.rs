use std::fmt;
use serde::{Deserialize, Serialize};

/// 6-bit packed card: bits [5:2] = rank (0–12), bits [1:0] = suit (0–3).
/// Value range 0–51, fits in a u8.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Card(pub(crate) u8);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum Rank {
    Two = 0,
    Three,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Ten,
    Jack,
    Queen,
    King,
    Ace,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[repr(u8)]
pub enum Suit {
    Clubs = 0,
    Diamonds,
    Hearts,
    Spades,
}

impl Card {
    #[inline]
    pub fn new(rank: Rank, suit: Suit) -> Self {
        Card((rank as u8) << 2 | (suit as u8))
    }

    #[inline]
    pub fn rank(self) -> Rank {
        unsafe { std::mem::transmute(self.0 >> 2) }
    }

    #[inline]
    pub fn suit(self) -> Suit {
        unsafe { std::mem::transmute(self.0 & 0b11) }
    }

    /// Index 0–51, stable for lookup tables.
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }

    /// Bit-mask position in a u64 hand mask.
    #[inline]
    pub fn mask(self) -> u64 {
        1u64 << self.0
    }
}

impl fmt::Debug for Card {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", rank_char(self.rank()), suit_char(self.suit()))
    }
}

impl fmt::Display for Card {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

fn rank_char(r: Rank) -> char {
    match r {
        Rank::Two => '2', Rank::Three => '3', Rank::Four => '4',
        Rank::Five => '5', Rank::Six => '6', Rank::Seven => '7',
        Rank::Eight => '8', Rank::Nine => '9', Rank::Ten => 'T',
        Rank::Jack => 'J', Rank::Queen => 'Q', Rank::King => 'K',
        Rank::Ace => 'A',
    }
}

fn suit_char(s: Suit) -> char {
    match s {
        Suit::Clubs => 'c', Suit::Diamonds => 'd',
        Suit::Hearts => 'h', Suit::Spades => 's',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for rank in [Rank::Two, Rank::Ace, Rank::King, Rank::Ten] {
            for suit in [Suit::Clubs, Suit::Spades, Suit::Hearts, Suit::Diamonds] {
                let c = Card::new(rank, suit);
                assert_eq!(c.rank(), rank);
                assert_eq!(c.suit(), suit);
            }
        }
    }

    #[test]
    fn unique_indices() {
        use std::collections::HashSet;
        let all: HashSet<usize> = (0u8..52)
            .map(|i| Card(i).index())
            .collect();
        assert_eq!(all.len(), 52);
    }

    #[test]
    fn mask_no_overlap() {
        let combined = (0u8..52).fold(0u64, |acc, i| acc | Card(i).mask());
        assert_eq!(combined.count_ones(), 52);
    }
}
