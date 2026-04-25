//! Replay viewer: loads a `FileSink` log and lets the user scrub events.
//!
//! The viewer holds the full event list and a cursor index. At each frame
//! we derive a `Snapshot` from `events[..=cursor]` and render it. This is
//! cheap enough for interactive use: hands produce on the order of tens
//! of events, and even long multi-hand logs stay under a few thousand.

use std::path::{Path, PathBuf};

use eframe::egui;
use poker_engine::core::Card;
use poker_engine::game::{read_event_log, Action, EngineEvent, HandResult, SeatIndex, Street};

pub struct ReplayApp {
    source: Option<PathBuf>,
    events: Vec<EngineEvent>,
    cursor: usize,
    error: Option<String>,
}

impl ReplayApp {
    pub fn empty() -> Self {
        ReplayApp {
            source: None,
            events: Vec::new(),
            cursor: 0,
            error: None,
        }
    }

    pub fn error(msg: String) -> Self {
        ReplayApp {
            source: None,
            events: Vec::new(),
            cursor: 0,
            error: Some(msg),
        }
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        let events = read_event_log(path)?;
        Ok(ReplayApp {
            source: Some(path.to_path_buf()),
            cursor: events.len().saturating_sub(1),
            events,
            error: None,
        })
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot::derive(&self.events[..self.cursor.saturating_add(1).min(self.events.len())])
    }
}

impl eframe::App for ReplayApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("poker-client · replay viewer");
                ui.separator();
                match &self.source {
                    Some(p) => ui.label(format!("{}", p.display())),
                    None => ui.label("(no log loaded — pass --replay <path>)"),
                };
            });
        });

        if let Some(msg) = &self.error {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("error: {msg}"));
            });
            return;
        }

        if self.events.is_empty() {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("No events. Launch with `--replay <path>` to open a log.");
            });
            return;
        }

        egui::TopBottomPanel::top("scrubber").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let n = self.events.len();
                if ui.button("⏮").clicked() {
                    self.cursor = 0;
                }
                if ui.button("◀").clicked() {
                    self.cursor = self.cursor.saturating_sub(1);
                }
                if ui.button("▶").clicked() {
                    self.cursor = (self.cursor + 1).min(n - 1);
                }
                if ui.button("⏭").clicked() {
                    self.cursor = n - 1;
                }
                ui.label(format!("event {}/{}", self.cursor + 1, n));
                ui.add(
                    egui::Slider::new(&mut self.cursor, 0..=(n - 1))
                        .clamping(egui::SliderClamping::Always)
                        .text("cursor"),
                );
            });
        });

        egui::SidePanel::left("events").resizable(true).show(ctx, |ui| {
            ui.heading("Events");
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (i, e) in self.events.iter().enumerate() {
                        let label = describe_event(e);
                        let selected = i == self.cursor;
                        if ui
                            .selectable_label(selected, format!("{i:>4}  {label}"))
                            .clicked()
                        {
                            self.cursor = i;
                        }
                    }
                });
        });

        let snapshot = self.snapshot();
        egui::CentralPanel::default().show(ctx, |ui| {
            render_snapshot(ui, &snapshot);
        });
    }
}

fn describe_event(e: &EngineEvent) -> String {
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

fn fmt_action(a: Action) -> String {
    match a {
        Action::Fold => "Fold".to_string(),
        Action::Check => "Check".to_string(),
        Action::Call => "Call".to_string(),
        Action::Raise(to) => format!("Raise({to})"),
        Action::AllIn => "AllIn".to_string(),
    }
}

// -----------------------------------------------------------------------
// Snapshot: state derived from events[..=cursor].
// -----------------------------------------------------------------------

struct Snapshot {
    hand_id: Option<u64>,
    dealer: Option<SeatIndex>,
    street: Option<Street>,
    board: Vec<Card>,
    pot_total: u32,
    players: Vec<PlayerView>,
    action_log: Vec<String>,
    terminal: Option<HandResult>,
}

#[derive(Default, Clone)]
struct PlayerView {
    hole_cards: Option<[Card; 2]>,
    all_in: bool,
    folded: bool,
    total_committed: u32,
}

impl Snapshot {
    fn derive(events: &[EngineEvent]) -> Self {
        let mut snap = Snapshot {
            hand_id: None,
            dealer: None,
            street: None,
            board: Vec::new(),
            pot_total: 0,
            players: Vec::new(),
            action_log: Vec::new(),
            terminal: None,
        };

        for e in events {
            match e {
                EngineEvent::HandStarted { hand_id, dealer, .. } => {
                    snap.hand_id = Some(*hand_id);
                    snap.dealer = Some(*dealer);
                    snap.street = Some(Street::Preflop);
                    snap.board.clear();
                    snap.pot_total = 0;
                    snap.players.clear();
                    snap.action_log.clear();
                    snap.terminal = None;
                }
                EngineEvent::HoleCardsDealt { seat, cards } => {
                    ensure_seat(&mut snap.players, *seat);
                    snap.players[*seat].hole_cards = Some(*cards);
                }
                EngineEvent::BoardDealt { street, cards } => {
                    snap.street = Some(*street);
                    snap.board.extend_from_slice(cards);
                }
                EngineEvent::ActionTaken { seat, action, pot_total } => {
                    ensure_seat(&mut snap.players, *seat);
                    snap.pot_total = *pot_total;
                    if matches!(action, Action::Fold) {
                        snap.players[*seat].folded = true;
                    }
                    if matches!(action, Action::AllIn) {
                        snap.players[*seat].all_in = true;
                    }
                    snap.action_log.push(format!(
                        "seat {} {} (pot {pot_total})",
                        seat,
                        fmt_action(*action),
                    ));
                }
                EngineEvent::PlayerAllIn { seat, total_committed } => {
                    ensure_seat(&mut snap.players, *seat);
                    snap.players[*seat].all_in = true;
                    snap.players[*seat].total_committed = *total_committed;
                }
                EngineEvent::HandEnded { result, .. } => {
                    snap.terminal = Some(result.clone());
                }
            }
        }

        snap
    }
}

fn ensure_seat(players: &mut Vec<PlayerView>, seat: SeatIndex) {
    while players.len() <= seat {
        players.push(PlayerView::default());
    }
}

fn render_snapshot(ui: &mut egui::Ui, snap: &Snapshot) {
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
            snap.street
                .map(|s| format!("{s:?}"))
                .unwrap_or_else(|| "—".to_string())
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
                    match s.hole_cards {
                        Some([a, b]) => {
                            card_chip(ui, &a);
                            card_chip(ui, &b);
                        }
                        None => {}
                    }
                });
            }
        });
    }
}

fn card_chip(ui: &mut egui::Ui, card: &Card) {
    // Colour by suit: red for hearts/diamonds, white for clubs/spades.
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
