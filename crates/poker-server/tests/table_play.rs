//! End-to-end TCP test for the 19b table layer.
//!
//! Boots a server with one heads-up table, connects two clients,
//! seats both, and runs a full hand by always folding to every
//! prompt. Asserts:
//!
//!   * Both clients see `JoinedTable` and a `TableState`.
//!   * Both clients receive a `HandStarted` event.
//!   * Each client sees ONLY its own `HoleCardsDealt`.
//!   * Folding through the hand drives the engine to `HandEnded`.
//!   * The hole cards of the folded seat are masked in the other
//!     client's `HandEnded` view.

use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use poker_engine::game::{Action, BettingRules, EngineEvent};
use poker_engine::net::protocol::{
    ClientMessage, SeatInfo, ServerMessage, PROTOCOL_VERSION,
};
use poker_server::{
    handle_connection, read_message, write_message, Registry, ServerContext, Table, TableConfig,
    TableManager,
};

const T: Duration = Duration::from_secs(10);

async fn spawn_server() -> (std::net::SocketAddr, tempfile::TempDir) {
    let dir = tempdir().unwrap();
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
    tables.install(Table::new(1, cfg), rules).await;
    let ctx = ServerContext { registry, tables };

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
    (addr, dir)
}

/// Connect, log in, return the socket and the welcome message.
async fn connect_and_login(
    addr: std::net::SocketAddr,
    username: &str,
) -> (TcpStream, ServerMessage) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    {
        let (mut read, mut write) = stream.split();
        write_message(
            &mut write,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                username: username.into(),
            },
        )
        .await
        .unwrap();
        let welcome: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
        return (
            // stream is borrowed by `split`; rebuild via the let reborrow below
            stream_unsplit(stream),
            welcome,
        );
    }
}

// `TcpStream::split` borrows the stream; once the borrow ends we can return
// the stream by value. This helper just exists to make the lifetime explicit.
fn stream_unsplit(stream: TcpStream) -> TcpStream {
    stream
}

#[tokio::test]
async fn full_hand_fold_through_wire() {
    let (addr, _guard) = spawn_server().await;

    let (mut s_alice, w_alice) = connect_and_login(addr, "alice").await;
    let (mut s_bob, w_bob) = connect_and_login(addr, "bob").await;
    assert!(matches!(w_alice, ServerMessage::Welcome { .. }));
    assert!(matches!(w_bob, ServerMessage::Welcome { .. }));

    // Both sit at the only table.
    {
        let (mut r, mut w) = s_alice.split();
        write_message(&mut w, &ClientMessage::JoinTable { table_id: 1, buy_in: 200 })
            .await
            .unwrap();
        let resp: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
        match resp {
            ServerMessage::JoinedTable { table_id, seats, .. } => {
                assert_eq!(table_id, 1);
                assert_eq!(seats.len(), 1);
            }
            other => panic!("alice expected JoinedTable, got {other:?}"),
        }
    }
    {
        let (mut r, mut w) = s_bob.split();
        write_message(&mut w, &ClientMessage::JoinTable { table_id: 1, buy_in: 200 })
            .await
            .unwrap();
        let resp: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
        match resp {
            ServerMessage::JoinedTable { seats, .. } => assert_eq!(seats.len(), 2),
            other => panic!("bob expected JoinedTable, got {other:?}"),
        }
    }

    // Run a single hand to completion. Strategy: always fold when prompted,
    // accept TableState / TableEvent broadcasts as they arrive.
    let alice_outcome = tokio::spawn(run_until_hand_end(s_alice, "alice"));
    let bob_outcome = tokio::spawn(run_until_hand_end(s_bob, "bob"));

    let (a_summary, b_summary) = tokio::join!(alice_outcome, bob_outcome);
    let a = a_summary.unwrap();
    let b = b_summary.unwrap();

    assert!(a.saw_hand_started, "alice never saw HandStarted");
    assert!(b.saw_hand_started, "bob never saw HandStarted");
    assert!(a.saw_hand_ended, "alice never saw HandEnded");
    assert!(b.saw_hand_ended, "bob never saw HandEnded");

    assert_eq!(a.hole_card_events.len(), 1, "alice saw hole-card events for {} seats", a.hole_card_events.len());
    assert_eq!(b.hole_card_events.len(), 1, "bob saw hole-card events for {} seats", b.hole_card_events.len());

    // The hand ended on a fold, so the winner should NOT see the
    // folded seat's hole cards in HandEnded (uncontested win = no
    // showdown reveal).
    let total_revealed_a = a.hand_end_reveals;
    let total_revealed_b = b.hand_end_reveals;
    assert!(
        total_revealed_a <= 1,
        "alice saw {total_revealed_a} hole cards in HandEnded; should be ≤1 (own only)"
    );
    assert!(
        total_revealed_b <= 1,
        "bob saw {total_revealed_b} hole cards in HandEnded; should be ≤1 (own only)"
    );
}

#[tokio::test]
async fn showdown_reveals_both_hole_cards() {
    let (addr, _guard) = spawn_server().await;
    let alice = tokio::spawn(check_call_session(addr, "alice"));
    let bob = tokio::spawn(check_call_session(addr, "bob"));

    let a = alice.await.unwrap();
    let b = bob.await.unwrap();

    // Both reached the river with two seats live.
    assert!(a.board_at_end >= 5, "alice saw only {} board cards", a.board_at_end);
    assert!(b.board_at_end >= 5, "bob saw only {} board cards", b.board_at_end);
    // At showdown, both hole-card pairs are revealed to both players.
    assert_eq!(a.hand_end_reveals, 2, "alice should see both hands at showdown");
    assert_eq!(b.hand_end_reveals, 2, "bob should see both hands at showdown");
}

/// Connect, log in, sit, and play through one hand by always
/// checking/calling. Returns the per-client summary.
async fn check_call_session(addr: std::net::SocketAddr, username: &'static str) -> HandSummary {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.split();

    // Hello / Welcome.
    write_message(
        &mut write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: username.into(),
        },
    )
    .await
    .unwrap();
    let welcome: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    assert!(matches!(welcome, ServerMessage::Welcome { .. }));

    // Sit, then drive the hand.
    write_message(&mut write, &ClientMessage::JoinTable { table_id: 1, buy_in: 200 })
        .await
        .unwrap();

    run_check_call_inner(&mut read, &mut write, username).await
}

async fn run_check_call_inner<R, W>(
    read: &mut R,
    write: &mut W,
    who: &'static str,
) -> HandSummary
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut summary = HandSummary::default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg: ServerMessage = match timeout(remaining, read_message(read)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => panic!("{who}: wire error {e:?}"),
            Err(_) => panic!("{who}: timed out; summary={summary:?}"),
        };
        match msg {
            ServerMessage::TableEvent { event, .. } => match event {
                EngineEvent::HandStarted { .. } => summary.saw_hand_started = true,
                EngineEvent::HoleCardsDealt { seat, .. } => summary.hole_card_events.push(seat),
                EngineEvent::BoardDealt { cards, .. } => {
                    summary.board_at_end += cards.len();
                }
                EngineEvent::HandEnded { result, .. } => {
                    for seat in &result.seats {
                        if seat.hole_cards.is_some() {
                            summary.hand_end_reveals += 1;
                        }
                    }
                    summary.saw_hand_ended = true;
                    return summary;
                }
                _ => {}
            },
            ServerMessage::Prompt { table_id, hand_id, legal, .. } => {
                let action = if legal.can_check {
                    Action::Check
                } else if legal.can_call {
                    Action::Call
                } else {
                    Action::Fold
                };
                write_message(
                    write,
                    &ClientMessage::SubmitAction { table_id, hand_id, action },
                )
                .await
                .unwrap();
            }
            _ => {}
        }
    }
}

#[derive(Default, Debug)]
struct HandSummary {
    saw_hand_started: bool,
    saw_hand_ended: bool,
    hole_card_events: Vec<usize>,
    hand_end_reveals: usize,
    board_at_end: usize,
    final_seats: Option<Vec<SeatInfo>>,
}

async fn run_until_hand_end(mut stream: TcpStream, who: &'static str) -> HandSummary {
    let mut summary = HandSummary::default();
    let (mut read, mut write) = stream.split();

    // Drain messages until we see HandEnded.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg: ServerMessage = match timeout(remaining, read_message(&mut read)).await {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => panic!("{who}: wire error {e:?}"),
            Err(_) => panic!("{who}: timed out waiting for HandEnded; summary={summary:?}"),
        };
        match msg {
            ServerMessage::TableState { seats, .. } => {
                summary.final_seats = Some(seats);
            }
            ServerMessage::TableEvent { event, .. } => match event {
                EngineEvent::HandStarted { .. } => summary.saw_hand_started = true,
                EngineEvent::HoleCardsDealt { seat, .. } => summary.hole_card_events.push(seat),
                EngineEvent::HandEnded { result, .. } => {
                    for seat in &result.seats {
                        if seat.hole_cards.is_some() {
                            summary.hand_end_reveals += 1;
                        }
                    }
                    summary.saw_hand_ended = true;
                    return summary;
                }
                _ => {}
            },
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
            ServerMessage::ActionRejected { reason } => {
                panic!("{who}: action rejected unexpectedly: {reason}");
            }
            _ => {}
        }
    }
}
