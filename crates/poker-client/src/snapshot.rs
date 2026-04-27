//! Per-hand `Snapshot` derived from a stream of `EngineEvent`s.
//!
//! Both the replay viewer and the live client feed events through
//! the same accumulator: the live client appends each `TableEvent`
//! as it arrives, the replay viewer slices `events[..=cursor]`. The
//! resulting `Snapshot` is the one source of truth for the table
//! rendering layer.

use eframe::egui;

use poker_engine::core::Card;
use poker_engine::game::{Action, EngineEvent, HandResult, SeatIndex, Street};

#[derive(Default, Clone)]
pub struct Snapshot {
    pub hand_id: Option<u64>,
    pub dealer: Option<SeatIndex>,
    pub street: Option<Street>,
    pub board: Vec<Card>,
    pub pot_total: u32,
    pub players: Vec<PlayerView>,
    pub action_log: Vec<String>,
    pub terminal: Option<HandResult>,
}

#[derive(Default, Clone)]
pub struct PlayerView {
    pub hole_cards: Option<[Card; 2]>,
    pub all_in: bool,
    pub folded: bool,
    pub total_committed: u32,
}

impl Snapshot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn derive(events: &[EngineEvent]) -> Self {
        let mut snap = Snapshot::default();
        for e in events {
            snap.apply(e);
        }
        snap
    }

    /// Fold one event into this snapshot. `HandStarted` resets the
    /// per-hand fields so a long-running live client can keep one
    /// `Snapshot` across hands.
    pub fn apply(&mut self, e: &EngineEvent) {
        match e {
            EngineEvent::HandStarted { hand_id, dealer, .. } => {
                self.hand_id = Some(*hand_id);
                self.dealer = Some(*dealer);
                self.street = Some(Street::Preflop);
                self.board.clear();
                self.pot_total = 0;
                // Keep the seat slots; just clear per-hand fields.
                for p in self.players.iter_mut() {
                    *p = PlayerView::default();
                }
                self.action_log.clear();
                self.terminal = None;
            }
            EngineEvent::HoleCardsDealt { seat, cards } => {
                ensure_seat(&mut self.players, *seat);
                self.players[*seat].hole_cards = Some(*cards);
            }
            EngineEvent::BoardDealt { street, cards } => {
                self.street = Some(*street);
                self.board.extend_from_slice(cards);
            }
            EngineEvent::ActionTaken { seat, action, pot_total } => {
                ensure_seat(&mut self.players, *seat);
                self.pot_total = *pot_total;
                if matches!(action, Action::Fold) {
                    self.players[*seat].folded = true;
                }
                if matches!(action, Action::AllIn) {
                    self.players[*seat].all_in = true;
                }
                self.action_log.push(format!(
                    "seat {} {} (pot {pot_total})",
                    seat,
                    fmt_action(*action),
                ));
            }
            EngineEvent::PlayerAllIn { seat, total_committed } => {
                ensure_seat(&mut self.players, *seat);
                self.players[*seat].all_in = true;
                self.players[*seat].total_committed = *total_committed;
            }
            EngineEvent::HandEnded { result, .. } => {
                self.terminal = Some(result.clone());
            }
        }
    }
}

fn ensure_seat(players: &mut Vec<PlayerView>, seat: SeatIndex) {
    while players.len() <= seat {
        players.push(PlayerView::default());
    }
}

pub fn describe_event(e: &EngineEvent) -> String {
    match e {
        EngineEvent::HandStarted { hand_id, dealer, deck_seed } => {
            format!("HandStarted · hand={hand_id} dealer={dealer} seed={deck_seed}")
        }
        EngineEvent::HoleCardsDealt { seat, cards } => {
            format!("HoleCardsDealt · seat={seat} [{} {}]", cards[0], cards[1])
        }
        EngineEvent::BoardDealt { street, cards } => {
            let joined = cards
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            format!("BoardDealt · {street:?} [{joined}]")
        }
        EngineEvent::ActionTaken { seat, action, pot_total } => {
            format!("ActionTaken · seat={seat} {} pot={pot_total}", fmt_action(*action))
        }
        EngineEvent::PlayerAllIn { seat, total_committed } => {
            format!("PlayerAllIn · seat={seat} total={total_committed}")
        }
        EngineEvent::HandEnded { hand_id, result } => {
            format!(
                "HandEnded · hand={hand_id} deltas=[{}]",
                result
                    .seats
                    .iter()
                    .map(|s| format!("{}:{:+}", s.seat, s.chip_delta))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

pub fn fmt_action(a: Action) -> String {
    match a {
        Action::Fold => "Fold".to_string(),
        Action::Check => "Check".to_string(),
        Action::Call => "Call".to_string(),
        Action::Raise(to) => format!("Raise({to})"),
        Action::AllIn => "AllIn".to_string(),
    }
}

// ─── Rendering ──────────────────────────────────────────────────────────────

pub fn render_snapshot(ui: &mut egui::Ui, snap: &Snapshot) {
    ui.horizontal(|ui| {
        ui.heading(match snap.hand_id {
            Some(h) => format!("Hand #{h}"),
            None => "No hand started".to_string(),
        });
        ui.separator();
        ui.label(format!("dealer: {}", fmt_opt(snap.dealer)));
        ui.separator();
        ui.label(format!(
            "street: {}",
            snap.street.map(|s| format!("{s:?}")).unwrap_or_else(|| "—".to_string())
        ));
        ui.separator();
        ui.label(format!("pot: {}", snap.pot_total));
    });

    ui.add_space(8.0);
    ui.group(|ui| {
        ui.label("Board");
        if snap.board.is_empty() {
            ui.weak("(none)");
        } else {
            ui.horizontal(|ui| {
                for c in &snap.board {
                    card_chip(ui, c);
                }
            });
        }
    });

    ui.add_space(8.0);
    ui.group(|ui| {
        ui.label("Players");
        for (i, p) in snap.players.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.monospace(format!("seat {i:>2}"));
                match p.hole_cards {
                    Some([a, b]) => {
                        card_chip(ui, &a);
                        card_chip(ui, &b);
                    }
                    None => {
                        ui.weak("(no cards)");
                    }
                }
                if p.folded {
                    ui.colored_label(egui::Color32::GRAY, "folded");
                }
                if p.all_in {
                    ui.colored_label(egui::Color32::LIGHT_YELLOW, "all-in");
                }
                if p.total_committed > 0 {
                    ui.weak(format!("committed {}", p.total_committed));
                }
            });
        }
    });

    ui.add_space(8.0);
    ui.group(|ui| {
        ui.label("Action log");
        egui::ScrollArea::vertical()
            .max_height(200.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if snap.action_log.is_empty() {
                    ui.weak("(no actions yet)");
                } else {
                    for line in &snap.action_log {
                        ui.monospace(line);
                    }
                }
            });
    });

    if let Some(result) = &snap.terminal {
        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label("Hand result");
            for s in &result.seats {
                let chip = if s.chip_delta >= 0 {
                    egui::Color32::LIGHT_GREEN
                } else {
                    egui::Color32::LIGHT_RED
                };
                ui.horizontal(|ui| {
                    ui.monospace(format!("seat {}", s.seat));
                    ui.colored_label(chip, format!("{:+}", s.chip_delta));
                    if s.sat_out {
                        ui.weak("sat out");
                    }
                    if let Some([a, b]) = s.hole_cards {
                        card_chip(ui, &a);
                        card_chip(ui, &b);
                    }
                });
            }
        });
    }
}

pub fn card_chip(ui: &mut egui::Ui, card: &Card) {
    let s = card.to_string();
    let red = s.ends_with('h') || s.ends_with('d');
    let colour = if red {
        egui::Color32::from_rgb(240, 120, 120)
    } else {
        egui::Color32::WHITE
    };
    ui.label(
        egui::RichText::new(s)
            .monospace()
            .size(18.0)
            .color(colour)
            .background_color(egui::Color32::from_gray(32)),
    );
}

fn fmt_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    match v {
        Some(x) => x.to_string(),
        None => "—".to_string(),
    }
}
