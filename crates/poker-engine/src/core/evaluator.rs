use rs_poker::core::{Card as RsCard, Rankable, Suit as RsSuit, Value as RsValue};

use super::card::{Card, Suit};
use super::hand_rank::HandRank;

/// Trait for hand strength evaluation. Swap implementations without touching game logic.
pub trait HandEvaluator: Send + Sync {
    fn rank_7(&self, cards: [Card; 7]) -> HandRank;
    fn rank_5(&self, cards: [Card; 5]) -> HandRank;
}

/// Fast lookup-table-ish evaluator via the `rs_poker` crate (~25 ns/7-card).
///
/// Default choice for bulk simulation. Produces a `HandRank(u32)` whose
/// ordering agrees with `NaiveEvaluator` on every hand, but whose numeric
/// encoding differs (it packs the rs_poker variant index into bits [31:28]
/// and the variant's inner u32 tiebreaker into bits [27:0]).
#[derive(Default, Clone, Copy)]
pub struct RsPokerEvaluator;

impl HandEvaluator for RsPokerEvaluator {
    fn rank_7(&self, cards: [Card; 7]) -> HandRank {
        let rs: [RsCard; 7] = std::array::from_fn(|i| to_rs_card(cards[i]));
        encode_rs_rank(rs.as_slice().rank())
    }

    fn rank_5(&self, cards: [Card; 5]) -> HandRank {
        let rs: [RsCard; 5] = std::array::from_fn(|i| to_rs_card(cards[i]));
        encode_rs_rank(rs.as_slice().rank())
    }
}

#[inline]
fn to_rs_card(c: Card) -> RsCard {
    // Our Rank (Two=0..Ace=12) matches rs_poker::Value numerically.
    let value = RsValue::from_u8(c.rank() as u8);
    // Suit encodings differ — explicit mapping required.
    let suit = match c.suit() {
        Suit::Clubs => RsSuit::Club,
        Suit::Diamonds => RsSuit::Diamond,
        Suit::Hearts => RsSuit::Heart,
        Suit::Spades => RsSuit::Spade,
    };
    RsCard::new(value, suit)
}

#[inline]
fn encode_rs_rank(r: rs_poker::core::Rank) -> HandRank {
    use rs_poker::core::Rank as R;
    // Variant index matches our HandCategory ordering (HighCard=0..StraightFlush=8)
    // so (cat << 28) | inner preserves ordering across categories.
    let (cat, inner) = match r {
        R::HighCard(v) => (0u32, v),
        R::OnePair(v) => (1, v),
        R::TwoPair(v) => (2, v),
        R::ThreeOfAKind(v) => (3, v),
        R::Straight(v) => (4, v),
        R::Flush(v) => (5, v),
        R::FullHouse(v) => (6, v),
        R::FourOfAKind(v) => (7, v),
        R::StraightFlush(v) => (8, v),
    };
    debug_assert!(inner < (1 << 28), "rs_poker tiebreaker overflowed 28 bits: {inner}");
    HandRank((cat << 28) | (inner & 0x0FFF_FFFF))
}

/// Naive evaluator — correct but not fast. Used as a cross-check oracle
/// for the lookup evaluator and for tests that don't need speed.
pub struct NaiveEvaluator;

impl HandEvaluator for NaiveEvaluator {
    fn rank_7(&self, cards: [Card; 7]) -> HandRank {
        best_of_7(&cards)
    }

    fn rank_5(&self, cards: [Card; 5]) -> HandRank {
        rank_5_naive(cards)
    }
}

fn best_of_7(cards: &[Card; 7]) -> HandRank {
    let mut best = HandRank(0);
    // Try all C(7,5) = 21 five-card combinations.
    for i in 0..7 {
        for j in (i + 1)..7 {
            let five: [Card; 5] = {
                let mut buf = [cards[0]; 5];
                let mut k = 0;
                for (idx, &c) in cards.iter().enumerate() {
                    if idx != i && idx != j {
                        buf[k] = c;
                        k += 1;
                    }
                }
                buf
            };
            let rank = rank_5_naive(five);
            if rank > best {
                best = rank;
            }
        }
    }
    best
}

fn rank_5_naive(mut cards: [Card; 5]) -> HandRank {
    use super::card::Suit;

    // Sort by rank descending for tiebreak encoding.
    cards.sort_unstable_by(|a, b| b.rank().cmp(&a.rank()));

    let ranks: [u8; 5] = std::array::from_fn(|i| cards[i].rank() as u8);
    let suits: [Suit; 5] = std::array::from_fn(|i| cards[i].suit());

    let is_flush = suits.iter().all(|&s| s == suits[0]);
    let is_straight = is_straight_ranks(&ranks);

    // Wheel (A-2-3-4-5): ace counts low.
    let is_wheel = ranks == [12, 3, 2, 1, 0];

    // Category occupies bits [31:28]; tiebreakers fit in [27:0] (7 × 4-bit nibbles).
    if is_flush && (is_straight || is_wheel) {
        let high = if is_wheel { 3u32 } else { ranks[0] as u32 };
        return HandRank(0x8000_0000 | high);
    }

    let counts = rank_counts(&ranks);

    if let Some(r) = four_of_a_kind(&counts) {
        let kicker = ranks.iter().find(|&&x| x != r).copied().unwrap_or(0) as u32;
        return HandRank(0x7000_0000 | ((r as u32) << 4) | kicker);
    }

    if let Some((three, pair)) = full_house(&counts) {
        return HandRank(0x6000_0000 | ((three as u32) << 4) | pair as u32);
    }

    if is_flush {
        let tiebreak = tiebreak_kickers(&ranks, 5);
        return HandRank(0x5000_0000 | tiebreak);
    }

    if is_straight || is_wheel {
        let high = if is_wheel { 3u32 } else { ranks[0] as u32 };
        return HandRank(0x4000_0000 | high);
    }

    if let Some(r) = three_of_a_kind(&counts) {
        let kickers: Vec<u8> = ranks.iter().filter(|&&x| x != r).copied().collect();
        let tb = (kickers[0] as u32) << 4 | kickers[1] as u32;
        return HandRank(0x3000_0000 | ((r as u32) << 8) | tb);
    }

    if let Some((hi, lo)) = two_pair(&counts) {
        let kicker = ranks.iter().find(|&&x| x != hi && x != lo).copied().unwrap_or(0) as u32;
        return HandRank(0x2000_0000 | ((hi as u32) << 8) | ((lo as u32) << 4) | kicker);
    }

    if let Some(r) = one_pair(&counts) {
        let kickers: Vec<u8> = ranks.iter().filter(|&&x| x != r).copied().collect();
        let tb = ((kickers[0] as u32) << 8) | ((kickers[1] as u32) << 4) | kickers[2] as u32;
        return HandRank(0x1000_0000 | ((r as u32) << 12) | tb);
    }

    HandRank(tiebreak_kickers(&ranks, 5))
}

fn is_straight_ranks(ranks: &[u8; 5]) -> bool {
    ranks[0] == ranks[4] + 4
        && ranks[0] != ranks[1] && ranks[1] != ranks[2]
        && ranks[2] != ranks[3] && ranks[3] != ranks[4]
}

fn rank_counts(ranks: &[u8; 5]) -> [u8; 13] {
    let mut counts = [0u8; 13];
    for &r in ranks {
        counts[r as usize] += 1;
    }
    counts
}

fn four_of_a_kind(counts: &[u8; 13]) -> Option<u8> {
    counts.iter().rposition(|&c| c == 4).map(|i| i as u8)
}

fn full_house(counts: &[u8; 13]) -> Option<(u8, u8)> {
    let three = counts.iter().rposition(|&c| c == 3).map(|i| i as u8)?;
    let pair = counts.iter().rposition(|&c| c == 2).map(|i| i as u8)?;
    Some((three, pair))
}

fn three_of_a_kind(counts: &[u8; 13]) -> Option<u8> {
    counts.iter().rposition(|&c| c == 3).map(|i| i as u8)
}

fn two_pair(counts: &[u8; 13]) -> Option<(u8, u8)> {
    let pairs: Vec<u8> = counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == 2)
        .map(|(i, _)| i as u8)
        .collect();
    if pairs.len() >= 2 {
        Some((pairs[pairs.len() - 1], pairs[pairs.len() - 2]))
    } else {
        None
    }
}

fn one_pair(counts: &[u8; 13]) -> Option<u8> {
    counts.iter().rposition(|&c| c == 2).map(|i| i as u8)
}

fn tiebreak_kickers(ranks: &[u8; 5], n: usize) -> u32 {
    ranks[..n]
        .iter()
        .fold(0u32, |acc, &r| (acc << 4) | r as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::card::{Rank, Suit};

    fn card(r: Rank, s: Suit) -> Card {
        Card::new(r, s)
    }

    #[test]
    fn royal_flush_beats_straight_flush() {
        let eval = NaiveEvaluator;
        let royal = [
            card(Rank::Ace, Suit::Spades), card(Rank::King, Suit::Spades),
            card(Rank::Queen, Suit::Spades), card(Rank::Jack, Suit::Spades),
            card(Rank::Ten, Suit::Spades), card(Rank::Two, Suit::Hearts),
            card(Rank::Three, Suit::Hearts),
        ];
        let sf = [
            card(Rank::Nine, Suit::Spades), card(Rank::Eight, Suit::Spades),
            card(Rank::Seven, Suit::Spades), card(Rank::Six, Suit::Spades),
            card(Rank::Five, Suit::Spades), card(Rank::Two, Suit::Hearts),
            card(Rank::Three, Suit::Hearts),
        ];
        assert!(eval.rank_7(royal) > eval.rank_7(sf));
    }

    #[test]
    fn pair_beats_high_card() {
        let eval = NaiveEvaluator;
        let pair = [
            card(Rank::Ace, Suit::Spades), card(Rank::Ace, Suit::Hearts),
            card(Rank::Two, Suit::Clubs), card(Rank::Three, Suit::Diamonds),
            card(Rank::Four, Suit::Spades), card(Rank::Five, Suit::Hearts),
            card(Rank::Seven, Suit::Clubs),
        ];
        let high = [
            card(Rank::King, Suit::Spades), card(Rank::Queen, Suit::Hearts),
            card(Rank::Jack, Suit::Clubs), card(Rank::Nine, Suit::Diamonds),
            card(Rank::Eight, Suit::Spades), card(Rank::Six, Suit::Hearts),
            card(Rank::Four, Suit::Clubs),
        ];
        assert!(eval.rank_7(pair) > eval.rank_7(high));
    }

    #[test]
    fn rs_poker_agrees_with_naive_category() {
        // On a modest sample, the rs_poker evaluator must classify every hand
        // into the same HandCategory as the naive evaluator.
        use crate::core::Deck;
        let naive = NaiveEvaluator;
        let rs = RsPokerEvaluator;
        for seed in 0..100u64 {
            let mut d = Deck::new(seed);
            let cards: [Card; 7] = std::array::from_fn(|_| d.deal());
            let n = naive.rank_7(cards);
            let r = rs.rank_7(cards);
            assert_eq!(
                n.category(),
                r.category(),
                "category mismatch on seed {seed} cards={cards:?}: naive={n:?} rs={r:?}"
            );
        }
    }

    #[test]
    fn rs_poker_pairwise_ordering_matches_naive() {
        // For random pairs of hands, rs_poker and naive must agree on the
        // relative ordering (who wins, or whether they tie).
        use crate::core::Deck;
        let naive = NaiveEvaluator;
        let rs = RsPokerEvaluator;
        for seed in 0..200u64 {
            let mut d = Deck::new(seed);
            let a: [Card; 7] = std::array::from_fn(|_| d.deal());
            let b: [Card; 7] = std::array::from_fn(|_| d.deal());
            let naive_ord = naive.rank_7(a).cmp(&naive.rank_7(b));
            let rs_ord = rs.rank_7(a).cmp(&rs.rank_7(b));
            assert_eq!(
                naive_ord, rs_ord,
                "ordering mismatch seed={seed}: a={a:?} b={b:?}"
            );
        }
    }

    #[test]
    fn wheel_straight_flush() {
        let eval = NaiveEvaluator;
        let wheel = [
            card(Rank::Ace, Suit::Hearts), card(Rank::Two, Suit::Hearts),
            card(Rank::Three, Suit::Hearts), card(Rank::Four, Suit::Hearts),
            card(Rank::Five, Suit::Hearts), card(Rank::King, Suit::Spades),
            card(Rank::Queen, Suit::Spades),
        ];
        // Wheel straight flush should beat a plain flush but lose to a higher straight flush.
        let plain_flush = [
            card(Rank::Ace, Suit::Spades), card(Rank::King, Suit::Spades),
            card(Rank::Queen, Suit::Spades), card(Rank::Jack, Suit::Spades),
            card(Rank::Nine, Suit::Spades), card(Rank::Two, Suit::Hearts),
            card(Rank::Three, Suit::Hearts),
        ];
        assert!(eval.rank_7(wheel) > eval.rank_7(plain_flush));
    }
}
