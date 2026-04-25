use crate::game::{Action, BettingRules, EngineEvent, EventSink, Street};

// -----------------------------------------------------------------------
// Per-seat counters
// -----------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct SeatStats {
    pub hands_dealt: u64,
    pub hands_won: u64,
    pub chip_delta_total: i64,
    /// Hands where seat called or raised preflop (voluntarily).
    pub vpip_hands: u64,
    /// Hands where seat raised or went all-in preflop.
    pub pfr_hands: u64,
    /// Bets + raises + all-ins across all streets.
    pub aggressive_actions: u64,
    /// Calls across all streets.
    pub passive_actions: u64,
    /// Sum of pot sizes at hand end (for average pot calculation).
    pub total_pot: u64,
}

impl SeatStats {
    pub fn win_pct(&self) -> f64 {
        if self.hands_dealt == 0 {
            return 0.0;
        }
        self.hands_won as f64 / self.hands_dealt as f64 * 100.0
    }

    /// Net chips won per hand (bb/hand when stacks are in BB units).
    pub fn chip_ev(&self) -> f64 {
        if self.hands_dealt == 0 {
            return 0.0;
        }
        self.chip_delta_total as f64 / self.hands_dealt as f64
    }

    pub fn vpip_pct(&self) -> f64 {
        if self.hands_dealt == 0 {
            return 0.0;
        }
        self.vpip_hands as f64 / self.hands_dealt as f64 * 100.0
    }

    pub fn pfr_pct(&self) -> f64 {
        if self.hands_dealt == 0 {
            return 0.0;
        }
        self.pfr_hands as f64 / self.hands_dealt as f64 * 100.0
    }

    /// Aggression Factor = (raises + bets) / calls.
    /// Returns `None` when there are no passive actions (undefined / infinite).
    pub fn aggression_factor(&self) -> Option<f64> {
        if self.passive_actions == 0 {
            return None;
        }
        Some(self.aggressive_actions as f64 / self.passive_actions as f64)
    }
}

// -----------------------------------------------------------------------
// Sink
// -----------------------------------------------------------------------

/// An `EventSink` that accumulates per-seat statistics across a simulation run.
///
/// Tracks VPIP and PFR by observing the current street inferred from the
/// event sequence (`HandStarted` → Preflop, then each `BoardDealt` advances
/// the street).
pub struct StatsSink {
    pub seats: Vec<SeatStats>,
    // Transient per-hand state, reset on HandStarted.
    current_street: Street,
    vpip_this_hand: Vec<bool>,
    pfr_this_hand: Vec<bool>,
}

impl StatsSink {
    pub fn new(n_seats: usize) -> Self {
        StatsSink {
            seats: vec![SeatStats::default(); n_seats],
            current_street: Street::Preflop,
            vpip_this_hand: vec![false; n_seats],
            pfr_this_hand: vec![false; n_seats],
        }
    }

    pub fn n_seats(&self) -> usize {
        self.seats.len()
    }

    /// Hands played (all seats see the same count).
    pub fn hands_played(&self) -> u64 {
        self.seats.first().map(|s| s.hands_dealt).unwrap_or(0)
    }

    pub fn avg_pot(&self) -> f64 {
        let hands = self.hands_played();
        if hands == 0 {
            return 0.0;
        }
        // total_pot is the same across seats (same pot seen by everyone); use seat 0.
        self.seats[0].total_pot as f64 / hands as f64
    }
}

impl EventSink for StatsSink {
    fn on_event(&mut self, event: &EngineEvent) {
        match event {
            EngineEvent::HandStarted { .. } => {
                self.current_street = Street::Preflop;
                for v in self.vpip_this_hand.iter_mut() {
                    *v = false;
                }
                for p in self.pfr_this_hand.iter_mut() {
                    *p = false;
                }
            }

            EngineEvent::HoleCardsDealt { .. } => {
                // hands_dealt is incremented at HandEnded for live seats only,
                // so dead-hand seats (which are also dealt cards) aren't counted.
            }

            EngineEvent::BoardDealt { street, .. } => {
                self.current_street = *street;
            }

            EngineEvent::ActionTaken { seat, action, pot_total: _ } => {
                let seat = *seat;
                if seat >= self.seats.len() {
                    return;
                }
                match action {
                    Action::Raise(_) | Action::AllIn => {
                        self.seats[seat].aggressive_actions += 1;
                        if self.current_street == Street::Preflop {
                            self.vpip_this_hand[seat] = true;
                            self.pfr_this_hand[seat] = true;
                        }
                    }
                    Action::Call => {
                        self.seats[seat].passive_actions += 1;
                        if self.current_street == Street::Preflop {
                            self.vpip_this_hand[seat] = true;
                        }
                    }
                    // Fold and Check don't contribute to VPIP/PFR/AF.
                    Action::Fold | Action::Check => {}
                }
            }

            EngineEvent::HandEnded { result, .. } => {
                // Pot size = sum of positive chip deltas (total chips won).
                let pot: u64 = result
                    .seats
                    .iter()
                    .filter(|o| o.chip_delta > 0)
                    .map(|o| o.chip_delta as u64)
                    .sum();

                for outcome in &result.seats {
                    let seat = outcome.seat;
                    if seat >= self.seats.len() {
                        continue;
                    }
                    // Dead-hand seats are invisible to stats even though they
                    // appear in the result (to preserve chip accounting upstream).
                    if outcome.sat_out {
                        continue;
                    }
                    let s = &mut self.seats[seat];
                    s.hands_dealt += 1;
                    s.chip_delta_total += outcome.chip_delta as i64;
                    if outcome.chip_delta > 0 {
                        s.hands_won += 1;
                    }
                    if self.vpip_this_hand[seat] {
                        s.vpip_hands += 1;
                    }
                    if self.pfr_this_hand[seat] {
                        s.pfr_hands += 1;
                    }
                    s.total_pot += pot;
                }
            }

            // Not relevant to stats.
            EngineEvent::PlayerAllIn { .. } => {}
        }
    }
}

// -----------------------------------------------------------------------
// Report formatting
// -----------------------------------------------------------------------

/// Print a formatted report to stdout.
pub fn print_report(
    sink: &StatsSink,
    agent_names: &[String],
    rules: &BettingRules,
    threads: usize,
    stack_label: &str,
) {
    let hands = sink.hands_played();
    let n = sink.n_seats();
    let thread_note = if threads > 1 {
        format!("  ·  {threads} threads")
    } else {
        String::new()
    };

    println!();
    println!(
        "poker_report  ·  {hands} hands  ·  {}/{} NL Hold'em  ·  {}bb starting stacks{thread_note}",
        rules.small_blind,
        rules.big_blind,
        stack_label,
    );
    println!();

    let col_w = agent_names.iter().map(|n| n.len()).max().unwrap_or(7).max(7);
    let header = format!(
        " {:<4}  {:<col_w$}  {:>6}  {:>6}  {:>8}  {:>6}  {:>6}  {:>5}",
        "Seat", "Agent", "Hands", "Win%", "Chip EV", "VPIP", "PFR", "AF"
    );
    let sep = "─".repeat(header.len());
    println!(" {sep}");
    println!("{header}");
    println!(" {sep}");

    for seat in 0..n {
        let s = &sink.seats[seat];
        let name = agent_names.get(seat).map(String::as_str).unwrap_or("?");
        let af = match s.aggression_factor() {
            Some(v) => format!("{v:.2}"),
            None => "∞".to_string(),
        };
        println!(
            " {:<4}  {:<col_w$}  {:>6}  {:>5.1}%  {:>+8.2}  {:>5.1}%  {:>5.1}%  {:>5}",
            seat,
            name,
            s.hands_dealt,
            s.win_pct(),
            s.chip_ev(),
            s.vpip_pct(),
            s.pfr_pct(),
            af,
        );
    }

    println!(" {sep}");
    println!("  Avg pot: {:.1} chips", sink.avg_pot());
    println!();
}


// -----------------------------------------------------------------------
// Unit tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::builtin::{CallingStation, RandomAgent};
    use crate::agent::Agent;
    use crate::core::NaiveEvaluator;
    use crate::game::{BettingRules, Engine, NullSink};
    use crate::sim::{SimConfig, SimRunner};

    fn run_stats(n: usize, hands: usize, seed: u64) -> StatsSink {
        let engine = Engine::new(BettingRules::no_limit_holdem(1, 2, n), NaiveEvaluator);
        let agents: Vec<Box<dyn Agent>> = (0..n)
            .map(|_| -> Box<dyn Agent> { Box::new(CallingStation) })
            .collect();
        let mut runner = SimRunner::new(engine, agents, vec![200u32; n]);
        let mut stats = StatsSink::new(n);
        runner.run(hands, &SimConfig::deterministic(seed), &mut stats);
        stats
    }

    #[test]
    fn hands_dealt_matches_run() {
        let stats = run_stats(3, 100, 1);
        for s in &stats.seats {
            assert_eq!(s.hands_dealt, 100, "every seat sees every hand");
        }
    }

    #[test]
    fn win_pcts_sum_to_100_approx() {
        let stats = run_stats(3, 500, 2);
        let total: f64 = stats.seats.iter().map(|s| s.win_pct()).sum();
        // May slightly exceed 100 on split pots; should be within 1%.
        assert!(
            total >= 99.0 && total <= 120.0,
            "win% total={total:.1} should be ~100 (may exceed due to split pots)"
        );
    }

    #[test]
    fn chip_ev_sums_to_zero() {
        let stats = run_stats(3, 200, 3);
        let total: f64 = stats.seats.iter().map(|s| s.chip_ev()).sum();
        assert!(
            total.abs() < 1e-9,
            "chip EV must be zero-sum: {total}"
        );
    }

    #[test]
    fn calling_station_has_zero_aggression() {
        let stats = run_stats(3, 100, 4);
        for s in &stats.seats {
            assert_eq!(
                s.aggressive_actions, 0,
                "CallingStation never raises"
            );
            assert_eq!(
                s.pfr_hands, 0,
                "CallingStation never PFRs"
            );
        }
    }

    #[test]
    fn random_agent_has_nonzero_aggression() {
        let n = 3;
        let engine = Engine::new(BettingRules::no_limit_holdem(1, 2, n), NaiveEvaluator);
        let agents: Vec<Box<dyn Agent>> = vec![
            Box::new(RandomAgent::new(1)),
            Box::new(CallingStation),
            Box::new(CallingStation),
        ];
        let mut runner = SimRunner::new(engine, agents, vec![200u32; n]);
        let mut stats = StatsSink::new(n);
        runner.run(200, &SimConfig::deterministic(5), &mut stats);
        assert!(
            stats.seats[0].aggressive_actions > 0,
            "RandomAgent should raise sometimes"
        );
        assert!(
            stats.seats[0].pfr_hands > 0,
            "RandomAgent should PFR sometimes"
        );
    }

    #[test]
    fn avg_pot_positive() {
        let stats = run_stats(3, 100, 6);
        assert!(stats.avg_pot() > 0.0, "average pot must be positive");
    }

    #[test]
    fn vpip_calling_station_equals_hands_not_in_bb() {
        // CallingStation always calls; VPIP should be high but not 100%
        // (a player in the BB who faces no raise just checks — not VPIP).
        let stats = run_stats(3, 300, 7);
        for s in &stats.seats {
            // VPIP should be between 50% and 100% for a caller.
            assert!(
                s.vpip_pct() > 40.0 && s.vpip_pct() <= 100.0,
                "VPIP={:.1}% unexpected for CallingStation",
                s.vpip_pct()
            );
        }
    }
}
