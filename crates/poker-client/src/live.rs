//! Live mode: connect to `poker-server`, sit at a table, play hands.
//!
//! The egui side stays purely synchronous. Network I/O lives in
//! [`crate::live_net::LiveClient`], which exposes a non-blocking
//! `send` / `drain` pair. Each frame we drain whatever events the
//! worker has produced and fold them into our local view:
//!
//! * lobby state (`Vec<TableInfo>`) for the table picker
//! * a [`Snapshot`] accumulated from `TableEvent`s for the in-hand view
//! * a `PendingPrompt` while it's our turn to act

use std::time::Instant;

use eframe::egui;

use poker_engine::game::{Action, HandId, LegalActions, SeatIndex};
use poker_engine::net::protocol::{
    ClientMessage, PlayerId, SeatInfo, ServerMessage, TableId, TableInfo,
};

use crate::live_net::{LiveClient, LiveEvent, LoginRequest};
use crate::snapshot::{render_snapshot, Snapshot};

/// What the client is currently doing.
enum Phase {
    /// Hello sent; waiting for `Welcome` (or `Rejected`).
    Connecting,
    /// Handshake done; the user is browsing the lobby.
    Lobby,
    /// Sitting at a table.
    Seated {
        table_id: TableId,
        my_seat: SeatIndex,
        seats: Vec<SeatInfo>,
        button: Option<SeatIndex>,
        snapshot: Snapshot,
    },
    /// Connection closed.
    Ended(String),
}

struct PendingPrompt {
    table_id: TableId,
    hand_id: HandId,
    seat: SeatIndex,
    legal: LegalActions,
    received_at: Instant,
    deadline_ms: u32,
    /// Slider state for raise sizing.
    raise_to: u32,
}

pub struct LiveApp {
    client: LiveClient,
    addr: String,
    username: String,
    me: Option<PlayerId>,
    phase: Phase,
    tables: Vec<TableInfo>,
    pending: Option<PendingPrompt>,
    /// Most recent server-rejection message, shown as a toast.
    last_error: Option<(Instant, String)>,
    /// Lobby buy-in input (per table).
    buy_in_input: u32,
    /// Reusable scratch buffer so we don't reallocate per frame.
    scratch: Vec<LiveEvent>,
}

impl LiveApp {
    pub fn connect(addr: String, username: String, login: LoginRequest) -> Self {
        let client = LiveClient::connect(addr.clone(), login);
        LiveApp {
            client,
            addr,
            username,
            me: None,
            phase: Phase::Connecting,
            tables: Vec::new(),
            pending: None,
            last_error: None,
            buy_in_input: 200,
            scratch: Vec::new(),
        }
    }

    fn pump(&mut self) {
        self.scratch.clear();
        self.client.drain(&mut self.scratch);
        // `take` so we can loop without a double mutable borrow.
        let events = std::mem::take(&mut self.scratch);
        for ev in &events {
            self.handle_event(ev);
        }
        self.scratch = events;
    }

    fn handle_event(&mut self, ev: &LiveEvent) {
        match ev {
            LiveEvent::Connecting => {
                // No-op; we're already in Connecting until Welcome arrives.
            }
            LiveEvent::Disconnected(reason) => {
                self.phase = Phase::Ended(reason.clone());
                self.pending = None;
            }
            LiveEvent::Server(msg) => self.handle_server(msg),
        }
    }

    fn handle_server(&mut self, msg: &ServerMessage) {
        match msg {
            ServerMessage::Welcome { player_id, .. } => {
                self.me = Some(*player_id);
                self.phase = Phase::Lobby;
                // Ask for the table list immediately.
                self.client.send(ClientMessage::ListTables);
            }
            ServerMessage::Rejected { reason, .. } => {
                self.phase = Phase::Ended(format!("rejected: {reason}"));
            }
            ServerMessage::TableList { tables } => {
                self.tables = tables.clone();
            }
            ServerMessage::JoinedTable { table_id, seat, seats } => {
                self.phase = Phase::Seated {
                    table_id: *table_id,
                    my_seat: *seat,
                    seats: seats.clone(),
                    button: None,
                    snapshot: Snapshot::new(),
                };
            }
            ServerMessage::LeftTable { .. } => {
                self.phase = Phase::Lobby;
                self.pending = None;
                self.client.send(ClientMessage::ListTables);
            }
            ServerMessage::TableState { seats: new_seats, button: btn, .. } => {
                if let Phase::Seated { seats, button, .. } = &mut self.phase {
                    *seats = new_seats.clone();
                    *button = Some(*btn);
                }
            }
            ServerMessage::TableEvent { event, .. } => {
                if let Phase::Seated { snapshot, .. } = &mut self.phase {
                    snapshot.apply(event);
                }
            }
            ServerMessage::Prompt { table_id, hand_id, seat, legal, deadline_ms } => {
                let raise_default = if legal.can_raise {
                    legal.min_raise
                } else {
                    0
                };
                self.pending = Some(PendingPrompt {
                    table_id: *table_id,
                    hand_id: *hand_id,
                    seat: *seat,
                    legal: *legal,
                    received_at: Instant::now(),
                    deadline_ms: *deadline_ms,
                    raise_to: raise_default,
                });
            }
            ServerMessage::ActionRejected { reason } => {
                self.last_error = Some((Instant::now(), reason.clone()));
            }
            ServerMessage::Heartbeat => {}
            ServerMessage::Goodbye { reason } => {
                self.phase = Phase::Ended(format!("server goodbye: {reason}"));
            }
        }
    }

    fn submit(&mut self, action: Action) {
        let Some(p) = self.pending.take() else { return };
        self.client.send(ClientMessage::SubmitAction {
            table_id: p.table_id,
            hand_id: p.hand_id,
            action,
        });
    }
}

impl eframe::App for LiveApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain any pending events before rendering.
        self.pump();
        // Keep the gui ticking so prompt countdowns update smoothly.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("poker-client · live");
                ui.separator();
                ui.label(format!("server: {}", self.addr));
                ui.separator();
                ui.label(format!("user: {}", self.username));
                if let Some(id) = self.me {
                    ui.weak(format!("(id={id})"));
                }
                ui.separator();
                let phase = match &self.phase {
                    Phase::Connecting => "connecting…".to_string(),
                    Phase::Lobby => "lobby".to_string(),
                    Phase::Seated { table_id, my_seat, .. } => {
                        format!("table {table_id} · seat {my_seat}")
                    }
                    Phase::Ended(_) => "disconnected".to_string(),
                };
                ui.label(phase);
            });
        });

        // Recent error toast, dismissed after a few seconds.
        if let Some((at, _)) = &self.last_error {
            if at.elapsed() > std::time::Duration::from_secs(6) {
                self.last_error = None;
            }
        }
        if let Some((_, msg)) = &self.last_error {
            egui::TopBottomPanel::top("toast").show(ctx, |ui| {
                ui.colored_label(egui::Color32::LIGHT_RED, format!("server: {msg}"));
            });
        }

        match &self.phase {
            Phase::Connecting => self.render_connecting(ctx),
            Phase::Lobby => self.render_lobby(ctx),
            Phase::Seated { .. } => self.render_seated(ctx),
            Phase::Ended(reason) => {
                let reason = reason.clone();
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.colored_label(egui::Color32::LIGHT_RED, "Disconnected");
                    ui.label(reason);
                });
            }
        }
    }
}

impl LiveApp {
    fn render_connecting(&self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.heading("Connecting…");
                ui.label(format!("→ {}", self.addr));
            });
        });
    }

    fn render_lobby(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Lobby");
                if ui.button("Refresh").clicked() {
                    self.client.send(ClientMessage::ListTables);
                }
                ui.separator();
                ui.label("Buy-in:");
                ui.add(
                    egui::DragValue::new(&mut self.buy_in_input)
                        .range(1..=u32::MAX)
                        .speed(10.0),
                );
            });
            ui.add_space(8.0);
            if self.tables.is_empty() {
                ui.weak("(no tables yet — click Refresh)");
                return;
            }
            egui::Grid::new("tables")
                .striped(true)
                .num_columns(6)
                .show(ui, |ui| {
                    ui.strong("id");
                    ui.strong("name");
                    ui.strong("blinds");
                    ui.strong("seats");
                    ui.strong("default buy-in");
                    ui.strong("");
                    ui.end_row();
                    let buy_in = self.buy_in_input;
                    for t in &self.tables {
                        ui.monospace(format!("{}", t.table_id));
                        ui.label(&t.name);
                        ui.monospace(format!("{}/{}", t.small_blind, t.big_blind));
                        ui.monospace(format!("{}/{}", t.seated, t.max_seats));
                        ui.monospace(format!("{}", t.default_buy_in));
                        if ui.button("Sit").clicked() {
                            self.client.send(ClientMessage::JoinTable {
                                table_id: t.table_id,
                                buy_in,
                            });
                        }
                        ui.end_row();
                    }
                });
        });
    }

    fn render_seated(&mut self, ctx: &egui::Context) {
        // Snapshot fields are inside the Seated arm; we render in two
        // passes to keep borrow scopes short.
        let (table_id, my_seat, seats_clone, button, snap_clone) =
            if let Phase::Seated { table_id, my_seat, seats, button, snapshot } = &self.phase {
                (*table_id, *my_seat, seats.clone(), *button, snapshot.clone())
            } else {
                return;
            };

        egui::TopBottomPanel::top("seat_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Table {table_id}"));
                ui.separator();
                ui.label(format!("you: seat {my_seat}"));
                if let Some(b) = button {
                    ui.separator();
                    ui.label(format!("button: seat {b}"));
                }
                ui.separator();
                if ui.button("Leave table").clicked() {
                    self.client.send(ClientMessage::LeaveTable { table_id });
                }
            });
        });

        egui::SidePanel::right("seats").resizable(true).show(ctx, |ui| {
            ui.heading("Seats");
            for s in &seats_clone {
                let me = s.seat == my_seat;
                ui.horizontal(|ui| {
                    let label = if me {
                        format!("seat {} · {} (you)", s.seat, s.username)
                    } else {
                        format!("seat {} · {}", s.seat, s.username)
                    };
                    ui.monospace(label);
                    ui.weak(format!("stack {}", s.stack));
                });
            }
            ui.separator();
            self.render_action_panel(ui, my_seat);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            render_snapshot(ui, &snap_clone);
        });
    }

    fn render_action_panel(&mut self, ui: &mut egui::Ui, my_seat: SeatIndex) {
        let Some(p) = self.pending.as_mut() else {
            ui.heading("Action");
            ui.weak("(waiting…)");
            return;
        };
        if p.seat != my_seat {
            ui.heading("Action");
            ui.label(format!("waiting on seat {}", p.seat));
            return;
        }
        ui.heading("Your turn");
        let elapsed = p.received_at.elapsed().as_millis() as u32;
        let remaining = p.deadline_ms.saturating_sub(elapsed);
        ui.weak(format!("hand {} · {:.1}s left", p.hand_id, remaining as f32 / 1000.0));

        let legal = p.legal;
        let mut chosen: Option<Action> = None;

        ui.horizontal_wrapped(|ui| {
            if ui.button("Fold").clicked() {
                chosen = Some(Action::Fold);
            }
            if ui.add_enabled(legal.can_check, egui::Button::new("Check")).clicked() {
                chosen = Some(Action::Check);
            }
            let call_label = format!("Call {}", legal.call_amount);
            if ui.add_enabled(legal.can_call, egui::Button::new(call_label)).clicked() {
                chosen = Some(Action::Call);
            }
            if ui.add_enabled(legal.all_in_amount > 0, egui::Button::new(format!("All-in ({})", legal.all_in_amount))).clicked() {
                chosen = Some(Action::AllIn);
            }
        });

        if legal.can_raise {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Raise to:");
                ui.add(
                    egui::Slider::new(&mut p.raise_to, legal.min_raise..=legal.max_raise)
                        .clamping(egui::SliderClamping::Always),
                );
                if ui.button("Raise").clicked() {
                    chosen = Some(Action::Raise(p.raise_to));
                }
            });
        }

        if let Some(action) = chosen {
            self.submit(action);
        }
    }
}

