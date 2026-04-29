//! In-house dataset generation: turn `FileSink` event logs into per-hand
//! per-seat stat records, then group them by opponent class or by rolling
//! hand-window for downstream analysis.
//!
//! The flow:
//! 1. Drive a `SimRunner` with a labelled set of personas; `FileSink` writes
//!    a log.
//! 2. `extract_hand_stats(events, seat_class)` decodes that log into one
//!    `HandStats` row per (hand, live seat), with `own_class` and the
//!    opponents-signature already attached so rows from different matchups
//!    can be concatenated and aggregated together.
//! 3. `class_conditional` / `windowed` slice the resulting `Vec<HandStats>`.
//!
//! Class labels are arbitrary strings — future personas slot in without
//! code changes here.

use std::collections::BTreeMap;

use poker_engine::game::{Action, EngineEvent, HandId, SeatIndex, Street};

// -----------------------------------------------------------------------
// Per-hand record
// -----------------------------------------------------------------------

/// One seat's experience of one hand. The atom of the dataset.
#[derive(Clone, Debug)]
pub struct HandStats {
    pub hand_id: HandId,
    pub seat: SeatIndex,
    /// Class label of the seat that produced this row.
    pub own_class: String,
    /// Sorted-multiset signature of the *other* seats' classes, joined by
    /// `+`. Heads-up: just the opponent's class.
    pub opponents_sig: String,
    /// Voluntarily put money in preflop (call or raise, not blind/check).
    pub voluntary_pf: bool,
    /// Raised or shoved preflop.
    pub raised_pf: bool,
    /// Bets + raises + all-ins across all streets.
    pub aggressive_actions: u32,
    /// Calls across all streets.
    pub passive_actions: u32,
    /// Reached showdown (hole cards revealed in the result).
    pub saw_showdown: bool,
    pub chip_delta: i32,
}

#[derive(Default)]
struct Builder {
    voluntary_pf: bool,
    raised_pf: bool,
    aggressive_actions: u32,
    passive_actions: u32,
    folded: bool,
}

/// Walk an event log and emit one `HandStats` per (hand, live seat).
///
/// `seat_class[i]` is the class label for seat `i`. Seats marked `sat_out`
/// in a hand result are skipped.
pub fn extract_hand_stats(events: &[EngineEvent], seat_class: &[String]) -> Vec<HandStats> {
    let mut out = Vec::new();
    let mut hand_id: HandId = 0;
    let mut street = Street::Preflop;
    let mut per_seat: BTreeMap<SeatIndex, Builder> = BTreeMap::new();

    for e in events {
        match e {
            EngineEvent::HandStarted { hand_id: hid, .. } => {
                hand_id = *hid;
                street = Street::Preflop;
                per_seat.clear();
            }
            EngineEvent::BoardDealt { street: s, .. } => {
                street = *s;
            }
            EngineEvent::ActionTaken { seat, action, .. } => {
                let entry = per_seat.entry(*seat).or_default();
                match action {
                    Action::Raise(_) | Action::AllIn => {
                        entry.aggressive_actions += 1;
                        if street == Street::Preflop {
                            entry.voluntary_pf = true;
                            entry.raised_pf = true;
                        }
                    }
                    Action::Call => {
                        entry.passive_actions += 1;
                        if street == Street::Preflop {
                            entry.voluntary_pf = true;
                        }
                    }
                    Action::Fold => {
                        entry.folded = true;
                    }
                    Action::Check => {}
                }
            }
            EngineEvent::HandEnded { result, .. } => {
                for outcome in &result.seats {
                    if outcome.sat_out {
                        continue;
                    }
                    let seat = outcome.seat;
                    if seat >= seat_class.len() {
                        continue;
                    }
                    let b = per_seat.remove(&seat).unwrap_or_default();
                    let opps_sig = opponents_signature(seat_class, seat);
                    out.push(HandStats {
                        hand_id,
                        seat,
                        own_class: seat_class[seat].clone(),
                        opponents_sig: opps_sig,
                        voluntary_pf: b.voluntary_pf,
                        raised_pf: b.raised_pf,
                        aggressive_actions: b.aggressive_actions,
                        passive_actions: b.passive_actions,
                        saw_showdown: !b.folded,
                        chip_delta: outcome.chip_delta,
                    });
                }
                per_seat.clear();
            }
            EngineEvent::HoleCardsDealt { .. } | EngineEvent::PlayerAllIn { .. } => {}
        }
    }
    out
}

fn opponents_signature(seat_class: &[String], own: SeatIndex) -> String {
    let mut opps: Vec<&str> = seat_class
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != own)
        .map(|(_, c)| c.as_str())
        .collect();
    opps.sort_unstable();
    opps.join("+")
}

// -----------------------------------------------------------------------
// Aggregations
// -----------------------------------------------------------------------

/// Rolled-up stats over a set of `HandStats` rows.
#[derive(Clone, Debug, Default)]
pub struct StatRollup {
    pub hands: u64,
    pub vpip_pct: f64,
    pub pfr_pct: f64,
    pub aggression_factor: Option<f64>,
    pub showdown_pct: f64,
    pub chip_ev: f64,
}

impl StatRollup {
    pub fn from_rows<'a, I: IntoIterator<Item = &'a HandStats>>(rows: I) -> Self {
        let mut hands: u64 = 0;
        let mut vpip: u64 = 0;
        let mut pfr: u64 = 0;
        let mut showdown: u64 = 0;
        let mut chips: i64 = 0;
        let mut aggressive: u64 = 0;
        let mut passive: u64 = 0;

        for r in rows {
            hands += 1;
            if r.voluntary_pf { vpip += 1; }
            if r.raised_pf { pfr += 1; }
            if r.saw_showdown { showdown += 1; }
            chips += r.chip_delta as i64;
            aggressive += r.aggressive_actions as u64;
            passive += r.passive_actions as u64;
        }

        let pct = |n: u64| if hands == 0 { 0.0 } else { n as f64 / hands as f64 * 100.0 };
        let af = if passive == 0 { None } else { Some(aggressive as f64 / passive as f64) };

        StatRollup {
            hands,
            vpip_pct: pct(vpip),
            pfr_pct: pct(pfr),
            aggression_factor: af,
            showdown_pct: pct(showdown),
            chip_ev: if hands == 0 { 0.0 } else { chips as f64 / hands as f64 },
        }
    }
}

/// Group rows by `(own_class, opponents_sig)` and roll each group up.
pub fn class_conditional(rows: &[HandStats]) -> BTreeMap<(String, String), StatRollup> {
    let mut groups: BTreeMap<(String, String), Vec<&HandStats>> = BTreeMap::new();
    for row in rows {
        groups
            .entry((row.own_class.clone(), row.opponents_sig.clone()))
            .or_default()
            .push(row);
    }
    groups
        .into_iter()
        .map(|(k, v)| (k, StatRollup::from_rows(v)))
        .collect()
}

/// Roll up each (seat, class) pair's stats over rolling windows of `window`
/// hands stepping by `step` hands. Windows are anchored at the smallest
/// observed `hand_id`.
pub fn windowed(rows: &[HandStats], window: u32, step: u32) -> Vec<WindowRow> {
    assert!(window > 0 && step > 0, "window and step must be positive");
    if rows.is_empty() {
        return Vec::new();
    }
    let min_hand = rows.iter().map(|r| r.hand_id).min().unwrap();
    let max_hand = rows.iter().map(|r| r.hand_id).max().unwrap();

    // Distinct (seat, class) keys present in the input. We bucket by both
    // so a single seat playing different classes across matchups (rare —
    // typically the seat→class map is fixed per matchup) stays separable.
    let mut keys: Vec<(SeatIndex, String)> = rows
        .iter()
        .map(|r| (r.seat, r.own_class.clone()))
        .collect();
    keys.sort();
    keys.dedup();

    let mut out = Vec::new();
    let mut start = min_hand;
    while start <= max_hand {
        let end = start + window as u64;
        for (seat, class) in &keys {
            let in_window: Vec<&HandStats> = rows
                .iter()
                .filter(|r| {
                    r.seat == *seat
                        && r.own_class == *class
                        && r.hand_id >= start
                        && r.hand_id < end
                })
                .collect();
            if in_window.is_empty() {
                continue;
            }
            let stats = StatRollup::from_rows(in_window);
            out.push(WindowRow {
                window_start: start,
                window_end: end,
                seat: *seat,
                class: class.clone(),
                stats,
            });
        }
        start += step as u64;
    }
    out
}

/// One row of windowed output: stats for one (seat, class) over one window.
#[derive(Clone, Debug)]
pub struct WindowRow {
    pub window_start: HandId,
    pub window_end: HandId,
    pub seat: SeatIndex,
    pub class: String,
    pub stats: StatRollup,
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use poker_engine::game::event::{HandResult, SeatOutcome};

    fn ev_action(seat: SeatIndex, action: Action, pot: u32) -> EngineEvent {
        EngineEvent::ActionTaken { seat, action, pot_total: pot }
    }
    fn ev_hand_started(id: HandId) -> EngineEvent {
        EngineEvent::HandStarted { hand_id: id, dealer: 0, deck_seed: 0 }
    }
    fn ev_board(street: Street) -> EngineEvent {
        EngineEvent::BoardDealt { street, cards: vec![] }
    }
    fn ev_hand_ended(id: HandId, deltas: &[(SeatIndex, i32, bool)]) -> EngineEvent {
        EngineEvent::HandEnded {
            hand_id: id,
            result: HandResult {
                hand_id: id,
                board: vec![],
                seats: deltas
                    .iter()
                    .map(|(s, d, sd)| SeatOutcome {
                        seat: *s,
                        hole_cards: if *sd {
                            use poker_engine::core::{Card, Rank, Suit};
                            Some([
                                Card::new(Rank::Two, Suit::Clubs),
                                Card::new(Rank::Two, Suit::Diamonds),
                            ])
                        } else {
                            None
                        },
                        chip_delta: *d,
                        sat_out: false,
                    })
                    .collect(),
            },
        }
    }

    fn classes(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extract_basic_two_player_hand() {
        let events = vec![
            ev_hand_started(1),
            ev_action(0, Action::Raise(6), 6),
            ev_action(1, Action::Call, 12),
            ev_board(Street::Flop),
            ev_action(0, Action::Check, 12),
            ev_action(1, Action::Check, 12),
            ev_action(0, Action::Raise(4), 16),
            ev_action(1, Action::Fold, 16),
            ev_hand_ended(1, &[(0, 5, false), (1, -2, false)]),
        ];
        let rows = extract_hand_stats(&events, &classes(&["lag", "nit"]));
        assert_eq!(rows.len(), 2);

        let s0 = rows.iter().find(|r| r.seat == 0).unwrap();
        assert_eq!(s0.own_class, "lag");
        assert_eq!(s0.opponents_sig, "nit");
        assert!(s0.voluntary_pf);
        assert!(s0.raised_pf);
        assert_eq!(s0.aggressive_actions, 2);
        assert_eq!(s0.passive_actions, 0);
        assert_eq!(s0.chip_delta, 5);

        let s1 = rows.iter().find(|r| r.seat == 1).unwrap();
        assert_eq!(s1.own_class, "nit");
        assert_eq!(s1.opponents_sig, "lag");
        assert!(s1.voluntary_pf);
        assert!(!s1.raised_pf);
        assert_eq!(s1.passive_actions, 1);
    }

    #[test]
    fn rollup_computes_pcts_and_af() {
        let rows = vec![
            HandStats {
                hand_id: 1, seat: 0, own_class: "x".into(), opponents_sig: "y".into(),
                voluntary_pf: true, raised_pf: true,
                aggressive_actions: 1, passive_actions: 0,
                saw_showdown: false, chip_delta: 4,
            },
            HandStats {
                hand_id: 2, seat: 0, own_class: "x".into(), opponents_sig: "y".into(),
                voluntary_pf: true, raised_pf: false,
                aggressive_actions: 0, passive_actions: 1,
                saw_showdown: true, chip_delta: -2,
            },
        ];
        let r = StatRollup::from_rows(rows.iter());
        assert_eq!(r.hands, 2);
        assert!((r.vpip_pct - 100.0).abs() < 1e-9);
        assert!((r.pfr_pct - 50.0).abs() < 1e-9);
        assert_eq!(r.aggression_factor, Some(1.0));
        assert!((r.showdown_pct - 50.0).abs() < 1e-9);
        assert!((r.chip_ev - 1.0).abs() < 1e-9);
    }

    #[test]
    fn class_conditional_buckets_correctly() {
        let mut rows = Vec::new();
        // Matchup A: lag vs nit
        rows.push(HandStats { hand_id: 1, seat: 0, own_class: "lag".into(),
            opponents_sig: "nit".into(), voluntary_pf: true, raised_pf: true,
            aggressive_actions: 1, passive_actions: 0, saw_showdown: false, chip_delta: 1 });
        rows.push(HandStats { hand_id: 1, seat: 1, own_class: "nit".into(),
            opponents_sig: "lag".into(), voluntary_pf: false, raised_pf: false,
            aggressive_actions: 0, passive_actions: 0, saw_showdown: false, chip_delta: -1 });
        // Matchup B: lag vs nit again, contributing to the same bucket
        rows.push(HandStats { hand_id: 2, seat: 0, own_class: "lag".into(),
            opponents_sig: "nit".into(), voluntary_pf: true, raised_pf: false,
            aggressive_actions: 0, passive_actions: 1, saw_showdown: false, chip_delta: -1 });

        let cc = class_conditional(&rows);
        let lag_vs_nit = cc.get(&("lag".to_string(), "nit".to_string())).unwrap();
        assert_eq!(lag_vs_nit.hands, 2);
        assert!((lag_vs_nit.vpip_pct - 100.0).abs() < 1e-9);
        assert!((lag_vs_nit.pfr_pct - 50.0).abs() < 1e-9);
    }

    #[test]
    fn windowed_chunks_by_hand_id() {
        let mk = |hand_id: HandId, vpip: bool| HandStats {
            hand_id, seat: 0, own_class: "x".into(), opponents_sig: "y".into(),
            voluntary_pf: vpip, raised_pf: vpip,
            aggressive_actions: if vpip { 1 } else { 0 },
            passive_actions: 0,
            saw_showdown: false, chip_delta: 0,
        };
        let rows: Vec<HandStats> = (1..=6u64).map(|i| mk(i, i <= 3)).collect();
        let windows = windowed(&rows, 3, 3);
        assert_eq!(windows.len(), 2);
        assert!((windows[0].stats.vpip_pct - 100.0).abs() < 1e-9);
        assert!((windows[1].stats.vpip_pct - 0.0).abs() < 1e-9);
    }
}
