use smallvec::SmallVec;

use crate::game::state::{PlayerState, PlayerStatus, Pot, SeatIndex};

/// Move each player's `bet_this_street` into the pot and reset it to 0.
/// `total_committed` is NOT changed here — it accumulates over the whole hand
/// and is used for side-pot calculation at showdown.
pub fn collect_street_bets(players: &mut Vec<PlayerState>, pot: &mut Pot) {
    let street_total: u32 = players.iter().map(|p| p.bet_this_street).sum();
    pot.main += street_total;
    for p in players.iter_mut() {
        p.bet_this_street = 0;
    }
}

/// Build the ordered list of pots for showdown distribution.
///
/// Returns `(amount, eligible_seats)` slices from smallest commitment level upward.
/// Eligible = non-folded players who committed at least that level.
///
/// Dead money from folded players contributes to pot amounts but not to eligibility.
/// Excess committed by a player that no opponent could match is returned to them as
/// an uncontested single-eligible slice.
pub fn build_showdown_pots(
    players: &[PlayerState],
) -> SmallVec<[(u32, SmallVec<[SeatIndex; 6]>); 5]> {
    let mut result: SmallVec<[(u32, SmallVec<[SeatIndex; 6]>); 5]> = SmallVec::new();

    // Unique commitment levels, ascending.
    let mut levels: Vec<u32> = players
        .iter()
        .map(|p| p.total_committed)
        .filter(|&c| c > 0)
        .collect();
    levels.sort_unstable();
    levels.dedup();

    let mut prev = 0u32;

    for level in levels {
        // All players (folded or not) who contributed at least this level.
        let n_contributors = players.iter().filter(|p| p.total_committed >= level).count();
        let pot_slice = (level - prev) * n_contributors as u32;

        if pot_slice == 0 {
            prev = level;
            continue;
        }

        // Only non-folded players are eligible to win this slice.
        let eligible: SmallVec<[SeatIndex; 6]> = players
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                if p.total_committed >= level && p.status != PlayerStatus::Folded {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();

        if eligible.is_empty() {
            // TDA: a side pot with no live hands merges into the last created pot.
            if let Some(last) = result.last_mut() {
                last.0 += pot_slice;
            }
            // If there is no previous pot, the chips are orphaned — the engine
            // is expected to have short-circuited via the uncontested-winner
            // path before we get here. Leave them out to avoid silent loss.
            prev = level;
            continue;
        }

        result.push((pot_slice, eligible));
        prev = level;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::state::PlayerStatus;

    fn player(committed: u32, status: PlayerStatus) -> PlayerState {
        PlayerState {
            stack: 0,
            hole_cards: None,
            bet_this_street: 0,
            total_committed: committed,
            status,
        }
    }

    #[test]
    fn no_side_pots_all_equal() {
        let players = vec![
            player(100, PlayerStatus::Active),
            player(100, PlayerStatus::Active),
            player(100, PlayerStatus::Active),
        ];
        let pots = build_showdown_pots(&players);
        assert_eq!(pots.len(), 1);
        assert_eq!(pots[0].0, 300);
        assert_eq!(pots[0].1.as_slice(), &[0, 1, 2]);
    }

    #[test]
    fn one_short_all_in() {
        // P0 all-in for 50, P1 and P2 each put in 100.
        let players = vec![
            player(50, PlayerStatus::AllIn),
            player(100, PlayerStatus::Active),
            player(100, PlayerStatus::Active),
        ];
        let pots = build_showdown_pots(&players);
        assert_eq!(pots.len(), 2);
        // Main: 50 * 3 = 150, eligible: all three
        assert_eq!(pots[0].0, 150);
        assert_eq!(pots[0].1.as_slice(), &[0, 1, 2]);
        // Side: 50 * 2 = 100, eligible: P1 and P2 only
        assert_eq!(pots[1].0, 100);
        assert_eq!(pots[1].1.as_slice(), &[1, 2]);
    }

    #[test]
    fn dead_money_from_fold() {
        // P0 commits 100, P1 commits 200, P2 folds after committing 50.
        let players = vec![
            player(100, PlayerStatus::Active),
            player(200, PlayerStatus::Active),
            player(50, PlayerStatus::Folded),
        ];
        let pots = build_showdown_pots(&players);
        // Level 50: 3 contributors × 50 = 150, eligible: P0 and P1 (P2 folded)
        // Level 100: 2 contributors (P0, P1) × 50 = 100, eligible: P0 and P1
        // Level 200: 1 contributor (P1) × 100 = 100, eligible: P1 only
        assert_eq!(pots.len(), 3);
        assert_eq!(pots[0].0, 150); // 50 * 3
        assert_eq!(pots[0].1.as_slice(), &[0, 1]);
        assert_eq!(pots[1].0, 100); // 50 * 2
        assert_eq!(pots[1].1.as_slice(), &[0, 1]);
        assert_eq!(pots[2].0, 100); // 100 * 1
        assert_eq!(pots[2].1.as_slice(), &[1]);
    }

    #[test]
    fn uncontested_excess_returned_via_pot() {
        // P0 bets 100, P1 goes all-in for 50 only. P0's excess 50 is uncontested.
        let players = vec![
            player(100, PlayerStatus::Active),
            player(50, PlayerStatus::AllIn),
        ];
        let pots = build_showdown_pots(&players);
        assert_eq!(pots.len(), 2);
        // Main: 50 * 2 = 100, both eligible
        assert_eq!(pots[0].0, 100);
        assert_eq!(pots[0].1.len(), 2);
        // Uncontested: 50 * 1 = 50, only P0 eligible (P1 ran out)
        assert_eq!(pots[1].0, 50);
        assert_eq!(pots[1].1.as_slice(), &[0]);
    }

    #[test]
    fn collect_street_bets_moves_to_pot() {
        let mut players = vec![
            PlayerState { stack: 900, hole_cards: None, bet_this_street: 50, total_committed: 50, status: PlayerStatus::Active },
            PlayerState { stack: 900, hole_cards: None, bet_this_street: 50, total_committed: 50, status: PlayerStatus::Active },
        ];
        let mut pot = Pot::default();
        collect_street_bets(&mut players, &mut pot);
        assert_eq!(pot.main, 100);
        assert!(players.iter().all(|p| p.bet_this_street == 0));
        // total_committed untouched
        assert!(players.iter().all(|p| p.total_committed == 50));
    }
}
