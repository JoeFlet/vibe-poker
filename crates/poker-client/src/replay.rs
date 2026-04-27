//! Replay viewer: loads a `FileSink` log and lets the user scrub events.
//!
//! The viewer holds the full event list and a cursor index. At each frame
//! we derive a `Snapshot` from `events[..=cursor]` and render it. This is
//! cheap enough for interactive use: hands produce on the order of tens
//! of events, and even long multi-hand logs stay under a few thousand.

use std::path::{Path, PathBuf};

use eframe::egui;
use poker_engine::game::{read_event_log, EngineEvent};

use crate::snapshot::{describe_event, render_snapshot, Snapshot};

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
