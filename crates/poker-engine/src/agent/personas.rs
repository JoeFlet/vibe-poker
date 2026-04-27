//! Bot personas for in-house dataset generation.
//!
//! Each persona is a hand-tuned action policy keyed on (street, hand strength,
//! whether facing a bet). They are deliberately simple — the goal is to
//! produce *distinct* VPIP / PFR / AF profiles so the aggregator can verify
//! its class-conditional and session-windowed stats against known baselines.
//!
//! Strength buckets are deterministic functions of the observation, so a
//! persona's behaviour is fully determined by its action RNG seed.

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

use super::{Agent, Observation};
use crate::abstraction::PreflopClass;
use crate::core::{Card, HandCategory, HandEvaluator, RsPokerEvaluator};
use crate::game::{Action, HandResult, Street};

/// Coarse hand strength buckets. Same scale across streets: a `Premium`
/// preflop hand and a `Premium` postflop hand are both "act fast" tier for
/// every persona.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Strength {
    Trash,
    Weak,
    Medium,
    Strong,
    Premium,
}

/// Map an observation to a strength bucket. Preflop uses `PreflopClass` index;
/// postflop uses the hand category of the best 5 cards out of the available
/// 5 / 6 / 7.
fn evaluate(obs: &Observation<'_>, eval: &impl HandEvaluator) -> Strength {
    if obs.street == Street::Preflop {
        let idx = PreflopClass::from_hole(obs.hole_cards).index();
        return match idx {
            0..=10 => Strength::Premium,
            11..=30 => Strength::Strong,
            31..=70 => Strength::Medium,
            71..=130 => Strength::Weak,
            _ => Strength::Trash,
        };
    }

    let cat = best_category(eval, obs.hole_cards, obs.board);
    match cat {
        HandCategory::FullHouse | HandCategory::FourOfAKind | HandCategory::StraightFlush => {
            Strength::Premium
        }
        HandCategory::Flush | HandCategory::Straight => Strength::Strong,
        HandCategory::ThreeOfAKind | HandCategory::TwoPair => Strength::Medium,
        HandCategory::OnePair => Strength::Weak,
        HandCategory::HighCard => Strength::Trash,
    }
}

/// Best 5-card category out of 5, 6, or 7 available cards.
fn best_category(eval: &impl HandEvaluator, hole: [Card; 2], board: &[Card]) -> HandCategory {
    // Flop: exactly 5 cards, one combination.
    // Turn: 6 cards; enumerate the 6 ways to pick 5.
    // River: 7 cards; the evaluator's rank_7 picks the best internally.
    match board.len() {
        3 => {
            let five = [hole[0], hole[1], board[0], board[1], board[2]];
            eval.rank_5(five).category()
        }
        4 => {
            let mut best = None;
            let all = [hole[0], hole[1], board[0], board[1], board[2], board[3]];
            for skip in 0..6 {
                let five: [Card; 5] = std::array::from_fn(|i| all[if i < skip { i } else { i + 1 }]);
                let cat = eval.rank_5(five).category();
                best = Some(match best {
                    None => cat,
                    Some(b) if cat > b => cat,
                    Some(b) => b,
                });
            }
            best.unwrap()
        }
        5 => {
            let seven = [
                hole[0], hole[1], board[0], board[1], board[2], board[3], board[4],
            ];
            eval.rank_7(seven).category()
        }
        _ => HandCategory::HighCard, // unreachable in practice
    }
}

// -----------------------------------------------------------------------
// Action helpers
// -----------------------------------------------------------------------

fn fold_or_check(obs: &Observation<'_>) -> Action {
    if obs.legal_actions.can_check {
        Action::Check
    } else {
        Action::Fold
    }
}

fn call_or_check(obs: &Observation<'_>) -> Action {
    if obs.legal_actions.can_check {
        Action::Check
    } else if obs.legal_actions.can_call {
        Action::Call
    } else {
        Action::Fold
    }
}

fn raise_or_call(obs: &Observation<'_>) -> Action {
    let la = &obs.legal_actions;
    if la.can_raise {
        Action::Raise(la.min_raise)
    } else {
        call_or_check(obs)
    }
}

fn shove_or_call(obs: &Observation<'_>) -> Action {
    if obs.legal_actions.all_in_amount > 0 {
        Action::AllIn
    } else {
        call_or_check(obs)
    }
}

// -----------------------------------------------------------------------
// Personas
// -----------------------------------------------------------------------

/// Very tight, mostly passive. Folds everything outside premium/strong;
/// calls premium, occasionally raises premium. Postflop: continues only
/// with two pair or better. Target profile: very low VPIP/PFR, low AF.
pub struct Nit {
    rng: SmallRng,
    eval: RsPokerEvaluator,
}

impl Nit {
    pub fn new(seed: u64) -> Self {
        Nit { rng: SmallRng::seed_from_u64(seed), eval: RsPokerEvaluator }
    }
}

impl Agent for Nit {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let s = evaluate(obs, &self.eval);
        let r: f32 = self.rng.r#gen();
        if obs.street == Street::Preflop {
            return match s {
                Strength::Premium => {
                    if r < 0.7 { raise_or_call(obs) } else { call_or_check(obs) }
                }
                Strength::Strong => call_or_check(obs),
                _ => fold_or_check(obs),
            };
        }
        match s {
            Strength::Premium => {
                if r < 0.5 { raise_or_call(obs) } else { call_or_check(obs) }
            }
            Strength::Strong | Strength::Medium => call_or_check(obs),
            _ => fold_or_check(obs),
        }
    }
}

/// Tight-aggressive: enters with premium/strong, raises most of the time
/// when entering, c-bets postflop with anything medium+, folds weak.
/// Target profile: VPIP ≈ 22 %, PFR ≈ 18 %, AF ≈ 2.5.
pub struct Tag {
    rng: SmallRng,
    eval: RsPokerEvaluator,
}

impl Tag {
    pub fn new(seed: u64) -> Self {
        Tag { rng: SmallRng::seed_from_u64(seed), eval: RsPokerEvaluator }
    }
}

impl Agent for Tag {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let s = evaluate(obs, &self.eval);
        let r: f32 = self.rng.r#gen();
        if obs.street == Street::Preflop {
            return match s {
                Strength::Premium => raise_or_call(obs),
                Strength::Strong => {
                    if r < 0.75 { raise_or_call(obs) } else { call_or_check(obs) }
                }
                Strength::Medium => {
                    if obs.legal_actions.can_check { Action::Check } else { Action::Fold }
                }
                _ => fold_or_check(obs),
            };
        }
        match s {
            Strength::Premium | Strength::Strong => raise_or_call(obs),
            Strength::Medium => {
                if r < 0.6 { raise_or_call(obs) } else { call_or_check(obs) }
            }
            Strength::Weak => call_or_check(obs),
            Strength::Trash => fold_or_check(obs),
        }
    }
}

/// Loose-aggressive: enters with anything medium+, raises most of the time,
/// stays aggressive postflop. Target: VPIP ≈ 35 %, PFR ≈ 28 %, AF ≈ 2.5.
pub struct Lag {
    rng: SmallRng,
    eval: RsPokerEvaluator,
}

impl Lag {
    pub fn new(seed: u64) -> Self {
        Lag { rng: SmallRng::seed_from_u64(seed), eval: RsPokerEvaluator }
    }
}

impl Agent for Lag {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let s = evaluate(obs, &self.eval);
        let r: f32 = self.rng.r#gen();
        if obs.street == Street::Preflop {
            return match s {
                Strength::Premium | Strength::Strong => raise_or_call(obs),
                Strength::Medium => {
                    if r < 0.7 { raise_or_call(obs) } else { call_or_check(obs) }
                }
                Strength::Weak => {
                    if r < 0.3 { raise_or_call(obs) } else { fold_or_check(obs) }
                }
                Strength::Trash => fold_or_check(obs),
            };
        }
        match s {
            Strength::Premium | Strength::Strong => raise_or_call(obs),
            Strength::Medium => {
                if r < 0.6 { raise_or_call(obs) } else { call_or_check(obs) }
            }
            Strength::Weak => {
                if r < 0.35 { raise_or_call(obs) } else { call_or_check(obs) }
            }
            Strength::Trash => {
                if r < 0.15 { raise_or_call(obs) } else { fold_or_check(obs) }
            }
        }
    }
}

/// Raises or shoves nearly everything. High variance, easy to identify.
/// Target: VPIP > 70 %, PFR > 60 %, AF > 4.
pub struct Maniac {
    rng: SmallRng,
}

impl Maniac {
    pub fn new(seed: u64) -> Self {
        Maniac { rng: SmallRng::seed_from_u64(seed) }
    }
}

impl Agent for Maniac {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let r: f32 = self.rng.r#gen();
        // Occasionally fold the absolute worst (trash preflop) to avoid
        // making this a pure all-in bot.
        if obs.street == Street::Preflop {
            let s = PreflopClass::from_hole(obs.hole_cards).index();
            if s > 150 && r < 0.4 {
                return fold_or_check(obs);
            }
        }
        if r < 0.05 {
            shove_or_call(obs)
        } else if r < 0.85 {
            raise_or_call(obs)
        } else {
            call_or_check(obs)
        }
    }
}

/// Switches between TAG and LAG modes based on recent chip swings.
/// After a meaningful loss, opens up (LAG mode) — "running cold = play wider".
/// After a win, contracts back to TAG.
pub struct TiltProne {
    rng: SmallRng,
    eval: RsPokerEvaluator,
    /// Cumulative chip delta over the recent window.
    recent_delta: i64,
    /// Threshold past which the bot switches into loose mode.
    tilt_threshold: i32,
}

impl TiltProne {
    pub fn new(seed: u64) -> Self {
        TiltProne {
            rng: SmallRng::seed_from_u64(seed),
            eval: RsPokerEvaluator,
            recent_delta: 0,
            tilt_threshold: 30,
        }
    }

    fn tilted(&self) -> bool {
        self.recent_delta <= -(self.tilt_threshold as i64)
    }
}

impl Agent for TiltProne {
    fn act(&mut self, obs: &Observation<'_>) -> Action {
        let s = evaluate(obs, &self.eval);
        let r: f32 = self.rng.r#gen();
        let tilted = self.tilted();
        if obs.street == Street::Preflop {
            return match s {
                Strength::Premium | Strength::Strong => raise_or_call(obs),
                Strength::Medium => {
                    let p = if tilted { 0.85 } else { 0.30 };
                    if r < p { raise_or_call(obs) } else if tilted { call_or_check(obs) } else { fold_or_check(obs) }
                }
                Strength::Weak => {
                    if tilted && r < 0.5 { raise_or_call(obs) } else { fold_or_check(obs) }
                }
                Strength::Trash => fold_or_check(obs),
            };
        }
        let aggression = if tilted { 0.8 } else { 0.5 };
        match s {
            Strength::Premium | Strength::Strong => raise_or_call(obs),
            Strength::Medium => {
                if r < aggression { raise_or_call(obs) } else { call_or_check(obs) }
            }
            Strength::Weak => {
                if tilted && r < 0.4 { raise_or_call(obs) } else if r < 0.5 { call_or_check(obs) } else { fold_or_check(obs) }
            }
            Strength::Trash => {
                if tilted && r < 0.2 { raise_or_call(obs) } else { fold_or_check(obs) }
            }
        }
    }

    fn on_hand_end(&mut self, result: &HandResult) {
        // Sum this seat's delta; we don't know which seat we are without
        // tracking it, so update from whichever entry matches the agent's
        // remaining stack swing. The runner calls this for every seat with
        // the full result, so look up our seat by chip_delta sign +
        // magnitude that matches our recent action — too brittle. Instead,
        // average the run: use the most negative delta as a proxy for the
        // table-wide stress level. (Good enough for a "tilt" heuristic
        // without threading seat identity through the trait.)
        if let Some(min_delta) = result.seats.iter().map(|s| s.chip_delta).min() {
            self.recent_delta = (self.recent_delta * 3 + min_delta as i64) / 4;
        }
    }
}
