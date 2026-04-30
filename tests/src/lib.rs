//! Workspace-root full-flow integration tests.
//!
//! This crate's `lib.rs` is the **shared test harness** that every
//! integration test in `tests/` builds on. It encapsulates:
//!
//! - Spinning up a `poker-server` in-process, against a tempdir.
//! - Connecting a [`poker_client_headless::HeadlessClient`] to it
//!   over real TCP loopback.
//! - Pumping the client: every [`poker_client_core::Effect::Send`]
//!   becomes a wire write, every inbound `ServerMessage` is fed back
//!   into the client, and the test waits on `ClientView` predicates
//!   rather than counting messages.
//!
//! Per `docs/CLIENT_PRINCIPLES.md` §1, full-flow tests live here
//! rather than under any individual crate's `tests/` directory.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};

use poker_client_core::{ClientView, Effect, Intent};
use poker_client_headless::HeadlessClient;
use poker_engine::game::BettingRules;
use poker_engine::net::protocol::ServerMessage;
use poker_server::{
    Registry, ServerContext, Table, TableConfig, TableManager, handle_connection, read_message,
    write_message,
};

/// Sensible defaults; tests can override via [`spawn_server_with`].
pub fn default_table_config() -> TableConfig {
    TableConfig {
        name: "Test".into(),
        max_seats: 2,
        min_seats: 2,
        small_blind: 1,
        big_blind: 2,
        default_buy_in: 200,
        action_deadline: Duration::from_secs(5),
        between_hands: Duration::from_millis(50),
    }
}

/// Stand up a `poker-server` listening on `127.0.0.1:0` with a
/// single default heads-up table, returning the bound address and
/// a [`TempDir`] guard the caller must hold for the duration of
/// the test (it owns the SQLite database).
pub async fn spawn_server() -> (SocketAddr, TempDir) {
    spawn_server_with(default_table_config()).await
}

pub async fn spawn_server_with(cfg: TableConfig) -> (SocketAddr, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::open(dir.path()).await.unwrap());
    let rules = BettingRules::no_limit_holdem(
        cfg.small_blind,
        cfg.big_blind,
        cfg.max_seats as usize,
    );
    let tables = TableManager::new();
    tables
        .install(Table::new(1, cfg), rules, Arc::clone(&registry))
        .await;
    let ctx = ServerContext {
        registry,
        tables,
        limits: Default::default(),
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let ctx = ctx.clone();
            tokio::spawn(async move {
                handle_connection(stream, peer, ctx).await;
            });
        }
    });
    (addr, dir)
}

/// Test driver. Wraps a [`HeadlessClient`] with a real TCP
/// connection to a [`spawn_server`] instance. The reader half of
/// the TCP stream is pumped continuously into a channel by a
/// background task, so the test thread can `wait_for(predicate)`
/// without missing any inbound messages.
pub struct LiveHarness {
    client: HeadlessClient,
    writer: OwnedWriteHalf,
    inbound_rx: mpsc::UnboundedReceiver<ServerMessage>,
    /// Held alive for `Drop` cleanup; never read.
    _reader_task: JoinHandle<()>,
}

impl LiveHarness {
    /// Open a TCP connection to `addr` and prime the headless
    /// client to "connection-open" state so subsequent intents
    /// like [`Intent::Register`] flush their wire messages
    /// immediately.
    pub async fn connect(addr: SocketAddr) -> Self {
        let stream = TcpStream::connect(addr).await.expect("TcpStream::connect failed");
        stream.set_nodelay(true).ok();
        let (reader, writer) = stream.into_split();
        let (tx, rx) = mpsc::unbounded_channel::<ServerMessage>();
        let reader_task = tokio::spawn(reader_loop(reader, tx));
        let mut client = HeadlessClient::new(None);
        // The harness owns the actual socket open, so we just tell
        // the core "you're connected": Connect emits an
        // OpenConnection effect we ignore, then ConnectionOpened
        // sets `connection_open=true` so handshake intents send.
        client.intent(Intent::Connect { addr: addr.to_string() });
        client.intent(Intent::ConnectionOpened);
        Self {
            client,
            writer,
            inbound_rx: rx,
            _reader_task: reader_task,
        }
    }

    /// Issue an intent. Any `Effect::Send` produced is written to
    /// the socket immediately. Returns the full effect slice for
    /// assertions on side-effects (logs, persistence, etc.).
    pub async fn issue(&mut self, intent: Intent) -> Vec<Effect> {
        let effects = self.client.intent(intent).to_vec();
        for e in &effects {
            if let Effect::Send(msg) = e {
                write_message(&mut self.writer, msg)
                    .await
                    .expect("write_message failed");
            }
        }
        effects
    }

    /// Pump inbound messages until the predicate holds against the
    /// client view, or `t` elapses. Failure prints the full effect
    /// log so test output explains what the core actually saw.
    pub async fn wait_for(
        &mut self,
        mut p: impl FnMut(&ClientView) -> bool,
        t: Duration,
        label: &str,
    ) {
        if p(&self.client.view()) {
            return;
        }
        let deadline = Instant::now() + t;
        loop {
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(d) => d,
                None => self.fail_wait(label),
            };
            let next = match timeout(remaining, self.inbound_rx.recv()).await {
                Ok(Some(msg)) => msg,
                Ok(None) => self.fail_with(label, "reader channel closed"),
                Err(_) => self.fail_wait(label),
            };
            self.client.inbound(next);
            if p(&self.client.view()) {
                return;
            }
        }
    }

    pub fn view(&self) -> ClientView {
        self.client.view()
    }

    pub fn effect_log(&self) -> &[Effect] {
        self.client.effect_log()
    }

    fn fail_wait(&self, label: &str) -> ! {
        self.fail_with(label, "timed out")
    }

    fn fail_with(&self, label: &str, why: &str) -> ! {
        panic!(
            "wait_for({label}) {why}\n  current phase: {:?}\n  effect log:\n{}",
            self.client.view().phase,
            self.effect_log()
                .iter()
                .map(|e| format!("    {e:?}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
}

async fn reader_loop(mut reader: OwnedReadHalf, tx: mpsc::UnboundedSender<ServerMessage>) {
    loop {
        let msg: Result<ServerMessage, _> = read_message(&mut reader).await;
        match msg {
            Ok(msg) => {
                if tx.send(msg).is_err() {
                    return; // harness dropped
                }
            }
            Err(_) => return, // EOF or framing error; harness will see it via `wait_for` timeout
        }
    }
}
