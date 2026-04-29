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
use poker_engine::net::protocol::{is_valid_email, is_valid_password, is_valid_username};

use crate::live_net::LoginRequest;

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

    /// Username for login or registration. Required with `--connect`.
    #[arg(long)]
    username: Option<String>,

    /// Password for login or registration. Required with `--connect`.
    #[arg(long)]
    password: Option<String>,

    /// Register a new account instead of authenticating an existing one.
    /// Requires `--email`.
    #[arg(long)]
    register: bool,

    /// Email address (only used with `--register`).
    #[arg(long)]
    email: Option<String>,
}

enum Mode {
    Replay(Option<PathBuf>),
    Live {
        addr: String,
        username: String,
        login: LoginRequest,
    },
}

fn pick_mode(cli: Cli) -> Result<Mode, String> {
    if cli.connect.is_none() {
        return Ok(Mode::Replay(cli.replay));
    }
    let addr = cli.connect.unwrap();
    let username = cli.username.ok_or("--connect requires --username")?;
    let password = cli.password.ok_or("--connect requires --password")?;
    if !is_valid_username(&username) {
        return Err(format!(
            "invalid username '{username}': must be 3..=24 ASCII chars, [a-z0-9_.-], starting with alnum"
        ));
    }
    if !is_valid_password(&password) {
        return Err("invalid password: must be 8..=128 chars".into());
    }
    let login = if cli.register {
        let email = cli.email.ok_or("--register requires --email")?;
        if !is_valid_email(&email) {
            return Err(format!("invalid email '{email}'"));
        }
        LoginRequest::Register {
            email,
            username: username.clone(),
            password,
        }
    } else {
        if cli.email.is_some() {
            return Err("--email is only meaningful with --register".into());
        }
        LoginRequest::Password {
            identifier: username.clone(),
            password,
        }
    };
    Ok(Mode::Live { addr, username, login })
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
                Mode::Live { addr, username, login } => {
                    Box::new(live::LiveApp::connect(addr, username, login))
                }
            };
            Ok(app)
        }),
    )
}
