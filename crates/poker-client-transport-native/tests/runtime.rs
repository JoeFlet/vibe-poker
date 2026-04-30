//! Integration tests for [`NativeClient`] — the runtime facade
//! that stitches [`poker_client_core::ClientCore`] together with
//! [`poker_client_transport_native::NativeTransport`] and a
//! filesystem session store.
//!
//! These tests stand up an in-process `poker-server` (same way
//! `tests/` at the workspace root does) and drive the real
//! TCP transport end-to-end.

use std::sync::Arc;
use std::time::{Duration, Instant};

use poker_client_core::{Intent, Phase};
use poker_client_transport_native::{NativeClient, NativeTransport, SessionStore};
use poker_engine::game::BettingRules;
use poker_server::{
    Registry, ServerContext, Table, TableConfig, TableManager, handle_connection,
};
use tokio::net::TcpListener;

/// Stand up a heads-up server. Mirrors `tests/src/lib.rs` so the
/// two harnesses exercise the same server config.
async fn spawn_server() -> (std::net::SocketAddr, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Registry::open(dir.path()).await.unwrap());
    let cfg = TableConfig {
        name: "Test".into(),
        max_seats: 2,
        min_seats: 2,
        small_blind: 1,
        big_blind: 2,
        default_buy_in: 200,
        action_deadline: Duration::from_secs(5),
        between_hands: Duration::from_millis(50),
    };
    let rules = BettingRules::no_limit_holdem(cfg.small_blind, cfg.big_blind, cfg.max_seats as usize);
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

/// Poll the client until `predicate(snapshot)` holds, or the
/// deadline elapses. `NativeClient::snapshot` is sync, so we
/// cannot `await` on the watch channel directly without moving
/// it; the short poll is fine for tests.
fn wait_for(
    client: &NativeClient,
    label: &str,
    t: Duration,
    mut predicate: impl FnMut(&poker_client_core::ClientView) -> bool,
) {
    let deadline = Instant::now() + t;
    loop {
        let snap = client.snapshot();
        if predicate(&snap) {
            return;
        }
        if Instant::now() >= deadline {
            let logs = client.drain_logs();
            panic!(
                "wait_for({label}) timed out\n  phase: {:?}\n  logs:\n{}",
                snap.phase,
                logs.iter()
                    .map(|l| format!("    [{:?}] {} {:?}", l.level, l.message, l.fields))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_lands_us_in_lobby_via_native_client() {
    let (addr, _dir) = spawn_server().await;
    let session_dir = tempfile::tempdir().unwrap();
    let session_path = session_dir.path().join("session.key");

    let client = NativeClient::with_transport(
        NativeTransport::new(),
        SessionStore::new(session_path.clone()),
    )
    .unwrap();

    client.issue(Intent::Connect {
        addr: addr.to_string(),
    });
    client.issue(Intent::Register {
        email: "alice@example.com".into(),
        username: "alice".into(),
        password: "hunter2hunter".into(),
        device_label: Some("native-client-test".into()),
    });

    wait_for(&client, "phase=Lobby", Duration::from_secs(5), |v| {
        v.phase == Phase::Lobby
    });

    let v = client.snapshot();
    assert_eq!(v.username.as_deref(), Some("alice"));
    assert!(v.player_id.is_some());
    assert!(v.lifetime_stats.is_some());

    // The session key the server minted must have been persisted
    // via Effect::PersistSessionKey → SessionStore::save.
    let persisted = std::fs::read_to_string(&session_path)
        .expect("session key file must exist after Welcome");
    assert!(!persisted.is_empty(), "persisted session key must be non-empty");

    // And the runtime must have buffered at least one Info log
    // (the "core: welcomed" line from the core).
    let logs = client.drain_logs();
    assert!(
        logs.iter().any(|l| l.message.contains("welcomed")),
        "expected a welcomed log; got {logs:?}",
    );
}

/// Session-key reuse path: persist a key from one NativeClient,
/// construct a second one against the same SessionStore, and
/// confirm the key is picked up on load. (We don't actually hand
/// the key to the server — the server would reject it since it's
/// fabricated — but the client-side load path is what this
/// exercises.)
#[test]
fn initial_session_key_is_loaded_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.key");
    std::fs::write(&path, "preseeded-key\n").unwrap();

    // Use a transport that never connects. `issue` just drops
    // intents into the runtime; snapshot shows the core's initial
    // state which reflects the loaded session key indirectly (it
    // does not expose the key itself but we can observe that the
    // core received it by inspecting the runtime's load step).
    // Concretely: we assert SessionStore::load returns the key.
    let store = SessionStore::new(path.clone());
    assert_eq!(
        store.load().unwrap().as_deref(),
        Some("preseeded-key"),
        "SessionStore must pick up the preseeded key",
    );
    // NativeClient::with_transport goes through SessionStore::load
    // and feeds the result to ClientCore::new; the integration
    // path above covers the write side. We don't need to spin up
    // an actual NativeClient here — doing so would create a
    // runtime thread with no connection for the duration of the
    // test.
}
