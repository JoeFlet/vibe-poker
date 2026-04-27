//! poker-client: GUI frontend to the poker engine.
//!
//! Two modes:
//!   * `--replay <path>`        — open a `FileSink` log and scrub events.
//!   * `--connect <addr> --username <name>` — live mode against `poker-server`.
//!
//! Both modes share the rendering layer in [`snapshot`].

mod live;
mod live_net;
mod replay;
mod snapshot;

use std::path::PathBuf;

use clap::Parser;
use poker_engine::net::protocol::is_valid_username;

#[derive(Parser, Debug)]
#[command(
    name = "poker-client",
    about = "GUI frontend for the poker engine (replay + live)",
)]
struct Cli {
    /// Path to an event log file produced by `FileSink` (MessagePack framing).
    #[arg(long, conflicts_with = "connect")]
    replay: Option<PathBuf>,

    /// `host:port` of a poker-server to connect to.
    #[arg(long)]
    connect: Option<String>,

    /// Username to send with the Hello handshake. Required with `--connect`.
    #[arg(long)]
    username: Option<String>,
}

enum Mode {
    Replay(Option<PathBuf>),
    Live { addr: String, username: String },
}

fn pick_mode(cli: Cli) -> Result<Mode, String> {
    match (cli.connect, cli.username, cli.replay) {
        (Some(addr), Some(user), _) => {
            if !is_valid_username(&user) {
                return Err(format!(
                    "invalid username '{user}': must be 3..=24 ASCII chars, [a-z0-9_.-], starting with alnum"
                ));
            }
            Ok(Mode::Live { addr, username: user })
        }
        (Some(_), None, _) => Err("--connect requires --username".into()),
        (None, _, replay) => Ok(Mode::Replay(replay)),
    }
}

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();
    let mode = match pick_mode(cli) {
        Ok(m) => m,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

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
            let app: Box<dyn eframe::App> = match mode {
                Mode::Replay(Some(path)) => match replay::ReplayApp::open(&path) {
                    Ok(a) => Box::new(a),
                    Err(e) => Box::new(replay::ReplayApp::error(format!("{e}"))),
                },
                Mode::Replay(None) => Box::new(replay::ReplayApp::empty()),
                Mode::Live { addr, username } => {
                    Box::new(live::LiveApp::connect(addr, username))
                }
            };
            Ok(app)
        }),
    )
}
