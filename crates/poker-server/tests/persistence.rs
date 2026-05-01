//! Step 21c — verify finished hands land in `hands` + `hand_seats`.
//!
//! Boots the same heads-up server `table_play.rs` uses, walks two
//! clients through one fold-to-end hand, and then queries the
//! persisted record:
//!
//!   * `hands` row exists with a non-empty `FileSink`-compatible log
//!     that decodes back into `HandStarted` … `HandEnded`.
//!   * `hand_seats` has one row per participating seat, with chip
//!     deltas summing to zero (the engine's invariant).

use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use poker_engine::game::{Action, BettingRules, EngineEvent};
use poker_engine::net::frame;
use poker_engine::net::protocol::{
    ClientMessage, PROTOCOL_VERSION, ServerMessage,
};
use poker_server::{
    Registry, ServerContext, Table, TableConfig, TableManager, handle_connection, read_message,
    write_message,
};

const T: Duration = Duration::from_secs(10);

async fn spawn_server() -> (std::net::SocketAddr, Arc<Registry>, tempfile::TempDir) {
    let (addr, registry, dir, _handle) = spawn_server_with_handle().await;
    (addr, registry, dir)
}

/// Variant of [`spawn_server`] that also returns the table-actor
/// `JoinHandle`, so tests can abort it to simulate a mid-hand crash.
async fn spawn_server_with_handle() -> (
    std::net::SocketAddr,
    Arc<Registry>,
    tempfile::TempDir,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempdir().unwrap();
    let registry = Arc::new(Registry::open(dir.path()).await.unwrap());

    let cfg = TableConfig {
        name: "Persist".into(),
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
    let handle = tables
        .install(Table::new(1, cfg), rules, Arc::clone(&registry))
        .await;
    let ctx = ServerContext {
        registry: Arc::clone(&registry),
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
            tokio::spawn(async move { handle_connection(stream, peer, ctx).await });
        }
    });
    (addr, registry, dir, handle)
}

async fn register_and_join(addr: std::net::SocketAddr, name: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut r, mut w) = stream.split();
    let register = ClientMessage::Register {
        protocol_version: PROTOCOL_VERSION,
        email: format!("{name}@example.com"),
        username: name.into(),
        password: "hunter2hunter".into(),
        device_label: None,
    };
    write_message(&mut w, &register).await.unwrap();
    let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
    write_message(&mut w, &ClientMessage::JoinTable { table_id: 1, buy_in: 200 })
        .await
        .unwrap();
    let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
    drop((r, w));
    stream
}

async fn fold_until_hand_end(mut stream: TcpStream) {
    let (mut read, mut write) = stream.split();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg: ServerMessage = match timeout(remaining, read_message(&mut read)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => panic!("wire error: {e:?}"),
            Err(_) => panic!("timed out waiting for HandEnded"),
        };
        match msg {
            ServerMessage::TableEvent { event: EngineEvent::HandEnded { .. }, .. } => return,
            ServerMessage::Prompt { table_id, hand_id, .. } => {
                write_message(
                    &mut write,
                    &ClientMessage::SubmitAction {
                        table_id,
                        hand_id,
                        action: Action::Fold,
                    },
                )
                .await
                .unwrap();
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn finished_hand_persists_log_and_seats() {
    let (addr, registry, _guard) = spawn_server().await;

    let alice = register_and_join(addr, "alice").await;
    let bob = register_and_join(addr, "bob").await;

    let a = tokio::spawn(fold_until_hand_end(alice));
    let b = tokio::spawn(fold_until_hand_end(bob));
    a.await.unwrap();
    b.await.unwrap();

    // The table actor writes the row right after applying chip deltas.
    // Poll briefly so we don't race the actor.
    let mut count = 0i64;
    for _ in 0..50 {
        count = registry.count_hands().await.unwrap();
        if count >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(count >= 1, "no hand persisted");

    let hand = registry.fetch_hand(1).await.unwrap().expect("hand 1 missing");
    assert_eq!(hand.table_id, 1);
    assert!(hand.ended_at >= hand.started_at);
    assert!(!hand.log.is_empty(), "log must not be empty");

    // Decode the log back via the same framing FileSink uses.
    let events = decode_log(&hand.log);
    assert!(
        matches!(events.first(), Some(EngineEvent::HandStarted { .. })),
        "log must start with HandStarted, got {:?}",
        events.first(),
    );
    assert!(
        matches!(events.last(), Some(EngineEvent::HandEnded { .. })),
        "log must end with HandEnded, got {:?}",
        events.last(),
    );

    // Two seats, chip deltas sum to zero.
    assert_eq!(hand.seats.len(), 2, "expected 2 seat rows, got {}", hand.seats.len());
    let total: i64 = hand.seats.iter().map(|s| s.chip_delta as i64).sum();
    assert_eq!(total, 0, "chip deltas should sum to zero, got {total}");
    for s in &hand.seats {
        assert!(s.player_id.is_some(), "seat {} missing user_id", s.seat);
    }
}

fn decode_log(bytes: &[u8]) -> Vec<EngineEvent> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
        i += 4;
        let payload = &bytes[i..i + len];
        let ev: EngineEvent = frame::decode(payload).unwrap();
        out.push(ev);
        i += len;
    }
    assert_eq!(i, bytes.len(), "trailing bytes in log");
    out
}

/// Step 27 regression: aborting the table actor mid-hand MUST leave
/// the DB indistinguishable from "the hand never started". This pins
/// the `record_hand` call-site contract — any future optimisation
/// that moves a persistence write ahead of `HandEnded` will trip this.
#[tokio::test]
async fn abort_mid_hand_leaves_db_clean() {
    let (addr, registry, _guard, handle) = spawn_server_with_handle().await;

    // Register + seat both players. After the second joins, quorum is
    // met and the table actor starts a hand.
    let alice = register_and_join(addr, "alice").await;
    let bob = register_and_join(addr, "bob").await;

    // Drive both clients until one of them sees `HandStarted` — proof
    // that the actor is inside `run_one_hand` and no `HandEnded` has
    // been emitted yet. Don't answer any `Prompt` (leave the hand
    // mid-flight). Keep both streams alive until the end of the test
    // so the server's connection-teardown paths don't mask the
    // persistence signal.
    let alice = wait_for_hand_started(alice).await;

    // Simulate a server crash: abort the table-actor task. The
    // `spawn_blocking` hand task is uncancellable and will run to
    // completion, but the `run_one_hand.await` inside `run_table` is
    // dropped along with the rest of the actor's future, so
    // `persist_hand` (the only caller of `Registry::record_hand`) is
    // never reached.
    handle.abort();
    let _ = handle.await; // swallow the JoinError from the abort.

    // Hold both client streams open so the server's connection-
    // teardown paths don't kick in and mask whether the persistence
    // side did its job.
    let _alice_guard = alice;
    let _bob_guard = bob;

    // Give any in-flight blocking task a generous window to finish.
    // The design pins this at 200ms; a full heads-up hand off random
    // cards is well under that on CI, so if `record_hand` were going
    // to be called we'd see it.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let hands = registry
        .count_hands()
        .await
        .expect("count_hands must succeed");
    assert_eq!(
        hands, 0,
        "aborted-mid-hand server must leave `hands` table empty, got {hands} rows",
    );

    // And: a re-authentication of one of the players returns the
    // default (zero) lifetime stats — the mid-hand abort MUST NOT have
    // moved any aggregates. (Current implementation doesn't touch
    // `lifetime_stats` from `record_hand` at all; this assertion
    // future-proofs the invariant against any such change.)
    // `authenticate_password` revokes any prior live session in the
    // same transaction, so alice being "online" from registration
    // doesn't block this.
    let reauth = registry
        .authenticate_password("alice", "hunter2hunter", None)
        .await
        .expect("alice should re-auth cleanly");
    assert_eq!(
        reauth.record.stats.hands, 0,
        "lifetime_stats.hands must be unchanged after mid-hand abort",
    );
    assert_eq!(
        reauth.record.stats.chip_delta, 0,
        "lifetime_stats.chip_delta must be unchanged after mid-hand abort",
    );
}

/// Read from `stream` until a `TableEvent(HandStarted)` arrives, then
/// return the stream so the caller can hold it open. Panics on
/// timeout or wire error.
async fn wait_for_hand_started(mut stream: TcpStream) -> TcpStream {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (mut read, _write) = stream.split();
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg: ServerMessage = match timeout(remaining, read_message(&mut read)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => panic!("wire error waiting for HandStarted: {e:?}"),
            Err(_) => panic!("timed out waiting for HandStarted"),
        };
        if let ServerMessage::TableEvent {
            event: EngineEvent::HandStarted { .. },
            ..
        } = msg
        {
            return stream;
        }
    }
}
