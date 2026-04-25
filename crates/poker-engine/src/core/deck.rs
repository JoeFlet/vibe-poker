use rand::{Rng, SeedableRng};
use rand::rngs::SmallRng;

use super::card::Card;

/// A full 52-card deck with Fisher-Yates shuffle.
/// Uses SmallRng (xoshiro128++) — fast, non-cryptographic.
///
/// Not serialized directly — callers store `seed()` for replay.
#[derive(Clone)]
pub struct Deck {
    cards: [Card; 52],
    cursor: usize,
    seed: u64,
}

impl Deck {
    pub fn new(seed: u64) -> Self {
        let mut deck = Deck {
            cards: std::array::from_fn(|i| Card(i as u8)),
            cursor: 0,
            seed,
        };
        deck.shuffle();
        deck
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Deal the next card. Panics if the deck is exhausted (should never happen in a valid hand).
    #[inline]
    pub fn deal(&mut self) -> Card {
        debug_assert!(self.cursor < 52, "deck exhausted");
        let card = self.cards[self.cursor];
        self.cursor += 1;
        card
    }

    pub fn remaining(&self) -> usize {
        52 - self.cursor
    }

    fn shuffle(&mut self) {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let n = self.cards.len();
        for i in (1..n).rev() {
            let j = rng.gen_range(0..=i);
            self.cards.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn no_duplicate_cards() {
        let mut deck = Deck::new(42);
        let mut seen = HashSet::new();
        for _ in 0..52 {
            assert!(seen.insert(deck.deal().index()));
        }
    }

    #[test]
    fn deterministic_with_same_seed() {
        let order_a: Vec<_> = {
            let mut d = Deck::new(99);
            (0..52).map(|_| d.deal()).collect()
        };
        let order_b: Vec<_> = {
            let mut d = Deck::new(99);
            (0..52).map(|_| d.deal()).collect()
        };
        assert_eq!(order_a, order_b);
    }

    #[test]
    fn different_seeds_differ() {
        let order_a: Vec<_> = { let mut d = Deck::new(1); (0..10).map(|_| d.deal()).collect() };
        let order_b: Vec<_> = { let mut d = Deck::new(2); (0..10).map(|_| d.deal()).collect() };
        assert_ne!(order_a, order_b);
    }
}
