//! CLI scenario replayer.
//!
//! ```text
//! poker-client-headless <scenario.json>
//! ```
//!
//! Loads a [`FileScript`] JSON file, replays each step against a
//! fresh [`HeadlessClient`], and prints the phase plus selected
//! view fields after each step. Non-zero exit on a parse or file
//! I/O error. Expectation failures don't apply here — the file
//! format stores only `Issue` / `Receive` steps.
//!
//! The intended workflow is: a failing integration test captures
//! its effective `Vec<Intent>` + inbound `ServerMessage` sequence
//! (via `FileStep::from_step` + `save_file_script`), commits the
//! `scenario.json`, and contributors re-run the CLI to reproduce
//! the exact state sequence deterministically.

use std::path::PathBuf;
use std::process::ExitCode;

use poker_client_core::{Phase, Intent};
use poker_client_headless::{Driver, FileStep, HeadlessClient, Step, scenario::load_file_script};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        return if args.is_empty() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }

    let path = PathBuf::from(&args[0]);
    let file_script = match load_file_script(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let steps: Vec<Step> = file_script.into_iter().map(FileStep::into_step).collect();
    println!("loaded {} step(s) from {}", steps.len(), path.display());

    let mut client = HeadlessClient::new(None);
    let result = Driver::run(&mut client, &steps);
    for (idx, snap) in result.snapshots.iter().enumerate() {
        let step_desc = step_one_line(&steps[idx]);
        println!(
            "[{idx}] {step_desc}\n     phase={:?} username={:?} tables={} seats={} hand={}",
            snap.phase,
            snap.username,
            snap.tables.len(),
            snap.seats.len(),
            snap.current_hand
                .as_ref()
                .map(|h| format!("#{} street={:?}", h.hand_id, h.street))
                .unwrap_or_else(|| "none".to_string()),
        );
    }
    if let Some((idx, msg)) = result.failure {
        eprintln!("FAILED at step {idx}: {msg}");
        return ExitCode::FAILURE;
    }

    // Brief post-run summary. The file-format Steps contain no
    // expectations, so `result.failure` is always `None` for a
    // well-formed script — but keep the status line for clarity.
    println!(
        "ok — finished at phase {:?}",
        result
            .snapshots
            .last()
            .map(|s| s.phase.clone())
            .unwrap_or(Phase::Disconnected)
    );
    ExitCode::SUCCESS
}

fn step_one_line(step: &Step) -> String {
    match step {
        Step::Issue(Intent::Connect { addr }) => format!("Issue Connect({addr})"),
        Step::Issue(Intent::Register { username, .. }) => {
            format!("Issue Register(username={username})")
        }
        Step::Issue(Intent::AuthenticatePassword { identifier, .. }) => {
            format!("Issue AuthenticatePassword({identifier})")
        }
        Step::Issue(Intent::AuthenticateSession { .. }) => "Issue AuthenticateSession".to_string(),
        Step::Issue(Intent::ListTables) => "Issue ListTables".to_string(),
        Step::Issue(Intent::JoinTable { table_id, buy_in }) => {
            format!("Issue JoinTable({table_id}, buy_in={buy_in})")
        }
        Step::Issue(Intent::LeaveTable { table_id }) => format!("Issue LeaveTable({table_id})"),
        Step::Issue(Intent::SubmitAction { action }) => format!("Issue SubmitAction({action:?})"),
        Step::Issue(Intent::Heartbeat) => "Issue Heartbeat".to_string(),
        Step::Issue(Intent::Disconnect) => "Issue Disconnect".to_string(),
        Step::Issue(Intent::ConnectionOpened) => "Issue ConnectionOpened".to_string(),
        Step::Issue(Intent::ConnectionLost { reason }) => {
            format!("Issue ConnectionLost({reason})")
        }
        Step::Receive(msg) => format!("Receive {:?}", discriminant_name(msg)),
        Step::ExpectPhase(p) => format!("ExpectPhase({p:?})"),
        Step::Expect { label, .. } => format!("Expect({label})"),
    }
}

fn discriminant_name(msg: &poker_engine::net::protocol::ServerMessage) -> &'static str {
    use poker_engine::net::protocol::ServerMessage;
    match msg {
        ServerMessage::Welcome { .. } => "Welcome",
        ServerMessage::Rejected { .. } => "Rejected",
        ServerMessage::Goodbye { .. } => "Goodbye",
        ServerMessage::Heartbeat => "Heartbeat",
        ServerMessage::TableList { .. } => "TableList",
        ServerMessage::JoinedTable { .. } => "JoinedTable",
        ServerMessage::LeftTable { .. } => "LeftTable",
        ServerMessage::TableState { .. } => "TableState",
        ServerMessage::TableEvent { .. } => "TableEvent",
        ServerMessage::Prompt { .. } => "Prompt",
        ServerMessage::ActionRejected { .. } => "ActionRejected",
        ServerMessage::ReplayEvents { .. } => "ReplayEvents",
    }
}

fn print_usage() {
    eprintln!("usage: poker-client-headless <scenario.json>");
    eprintln!();
    eprintln!("Replays a JSON file of `FileStep`s against a fresh");
    eprintln!("`HeadlessClient` and prints the resulting view phase");
    eprintln!("after each step. Used to reproduce bugs deterministically.");
}
