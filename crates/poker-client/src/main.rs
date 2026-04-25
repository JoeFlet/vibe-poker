//! poker-client: GUI frontend to the poker engine.
//!
//! Current scope: replay viewer. Opens a `FileSink` log and lets the user
//! scrub through events, seeing the game state snapshot at each step. The
//! rendering layer here is also the basis for the future live client that
//! connects to `poker-server`.

mod replay;

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "poker-client",
    about = "Replay viewer for poker engine event logs",
)]
struct Cli {
    /// Path to an event log file produced by `FileSink` (MessagePack framing).
    #[arg(long)]
    replay: Option<PathBuf>,
}

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([800.0, 520.0])
            .with_title("poker-client"),
        ..Default::default()
    };

    eframe::run_native(
        "poker-client",
        native_options,
        Box::new(move |_cc| {
            let app = match cli.replay.as_ref() {
                Some(path) => replay::ReplayApp::open(path)
                    .unwrap_or_else(|e| replay::ReplayApp::error(format!("{e}"))),
                None => replay::ReplayApp::empty(),
            };
            Ok(Box::new(app))
        }),
    )
}
