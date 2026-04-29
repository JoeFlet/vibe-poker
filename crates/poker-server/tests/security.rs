//! Step 22a — wire-layer hardening + server-side action validation.
//!
//! Three concerns covered here:
//!
//!   * An oversized length prefix is rejected at the framing layer
//!     before the server ever allocates the payload buffer (no
//!     "attacker announces 4 GiB and we OOM").
//!   * A truncated frame (prefix announces N bytes, peer sends
//!     fewer and closes) tears the session down cleanly rather than
//!     hanging the read.
//!   * A forged `SubmitAction` whose chosen action falls outside
//!     `LegalActions` is rejected by the server, even though the
//!     client signed a valid `Prompt` for the same hand. The check
//!     lives in `Connection::deliver_action`.

use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use poker_engine::game::{Action, BettingRules, EngineEvent};
use poker_engine::net::frame::MAX_FRAME_BYTES;
use poker_engine::net::protocol::{
    AuthMode, ClientMessage, PROTOCOL_VERSION, ServerMessage,
};
use poker_server::{
    ConnectionLimits, Registry, ServerContext, Table, TableConfig, TableManager,
    handle_connection, read_message, write_message,
};

const T: Duration = Duration::from_secs(10);

async fn spawn_server() -> (std::net::SocketAddr, tempfile::TempDir) {
    spawn_server_with_limits(Default::default()).await
}

async fn spawn_server_with_limits(
    limits: ConnectionLimits,
) -> (std::net::SocketAddr, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let registry = Arc::new(Registry::open(dir.path()).await.unwrap());

    let cfg = TableConfig {
        name: "Sec".into(),
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
        registry: Arc::clone(&registry),
        tables,
        limits,
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
    (addr, dir)
}

#[tokio::test]
async fn oversized_frame_prefix_drops_connection() {
    let (addr, _guard) = spawn_server().await;
    let mut s = TcpStream::connect(addr).await.unwrap();

    // Announce a payload one byte over the cap. The server must
    // refuse to allocate it; the connection should drop without
    // sending any payload from us.
    let bad_len = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes();
    s.write_all(&bad_len).await.unwrap();
    s.flush().await.unwrap();

    // The server tears the connection down. `read` returns 0 once
    // the peer half-closes; we just need to confirm we don't hang
    // and the server didn't try to read 4 GiB.
    let mut scratch = [0u8; 64];
    let read = timeout(T, s.read(&mut scratch))
        .await
        .expect("server should drop the connection promptly");
    let n = read.expect("read should not error");
    assert_eq!(n, 0, "expected EOF from server, got {n} bytes");
}

#[tokio::test]
async fn truncated_frame_drops_connection() {
    let (addr, _guard) = spawn_server().await;
    let mut s = TcpStream::connect(addr).await.unwrap();

    // Announce 1024 bytes then send only 4. The server's
    // `read_exact` on the payload should yield UnexpectedEof when
    // we close, and the session should exit without panicking.
    let len = 1024u32.to_le_bytes();
    s.write_all(&len).await.unwrap();
    s.write_all(&[0u8; 4]).await.unwrap();
    s.shutdown().await.unwrap();

    let mut scratch = [0u8; 64];
    let read = timeout(T, s.read(&mut scratch))
        .await
        .expect("server should not hang");
    // Either a clean EOF (0 bytes) or an error is acceptable; what
    // matters is we don't time out.
    let _ = read;
}

#[tokio::test]
async fn forged_raise_is_server_rejected() {
    let (addr, _guard) = spawn_server().await;

    // Two players join the heads-up table.
    let mut alice = register_and_join(addr, "alice").await;
    let mut bob = register_and_join(addr, "bob").await;

    // Drive Bob with a check/call loop in the background so the hand
    // doesn't immediately end — alice needs to actually receive a
    // prompt to forge against. Folding here would let bob (heads-up
    // dealer/SB on some hands) end the hand before alice ever acts.
    let bob_task = tokio::spawn(async move {
        let (mut r, mut w) = bob.split();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg: ServerMessage = match timeout(remaining, read_message(&mut r)).await {
                Ok(Ok(m)) => m,
                _ => return,
            };
            if let ServerMessage::Prompt { table_id, hand_id, ref legal, .. } = msg {
                let action = if legal.can_check { Action::Check } else { Action::Call };
                let _ = write_message(
                    &mut w,
                    &ClientMessage::SubmitAction { table_id, hand_id, action },
                )
                .await;
            }
            if matches!(
                msg,
                ServerMessage::TableEvent { event: EngineEvent::HandEnded { .. }, .. }
            ) {
                return;
            }
        }
    });

    // Wait for Alice's first prompt so we can forge against it.
    let (mut a_read, mut a_write) = alice.split();
    let got_rejection;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg: ServerMessage = timeout(remaining, read_message(&mut a_read))
            .await
            .expect("timed out waiting for prompt")
            .unwrap();
        match msg {
            ServerMessage::Prompt { table_id, hand_id, legal, .. } => {
                // Build a raise that's outside the legal range. If a
                // raise is allowed at all we use max_raise + 1; if
                // it isn't we just use any nonzero amount, since
                // is_legal will reject Raise outright.
                let bogus = legal.max_raise.saturating_add(1).max(1);
                write_message(
                    &mut a_write,
                    &ClientMessage::SubmitAction {
                        table_id,
                        hand_id,
                        action: Action::Raise(bogus),
                    },
                )
                .await
                .unwrap();
                // The server rejects, which means our pending prompt
                // is still live — submit a real fold so the hand can
                // actually finish (otherwise we'd time out on the
                // action deadline and get auto-folded anyway).
                let resp: ServerMessage = timeout(T, read_message(&mut a_read))
                    .await
                    .unwrap()
                    .unwrap();
                match resp {
                    ServerMessage::ActionRejected { .. } => {
                        got_rejection = true;
                        let _ = write_message(
                            &mut a_write,
                            &ClientMessage::SubmitAction {
                                table_id,
                                hand_id,
                                action: Action::Fold,
                            },
                        )
                        .await;
                        break;
                    }
                    other => panic!("expected ActionRejected, got {other:?}"),
                }
            }
            ServerMessage::TableEvent { event: EngineEvent::HandEnded { .. }, .. } => {
                panic!("hand ended before alice ever got a prompt");
            }
            _ => {}
        }
    }
    assert!(got_rejection, "server never rejected the forged raise");
    bob_task.abort();
}

/// 22b — a peer that opens a TCP connection but never sends a frame
/// must be reaped by the handshake idle timeout. The client should
/// see a clean EOF without us sending anything.
#[tokio::test]
async fn idle_handshake_drops_connection() {
    let limits = ConnectionLimits {
        idle_timeout: Duration::from_millis(150),
        ..Default::default()
    };
    let (addr, _guard) = spawn_server_with_limits(limits).await;
    let mut s = TcpStream::connect(addr).await.unwrap();

    // Sit silent. The server should give up on us within idle_timeout
    // and drop the socket without us sending a single byte.
    let mut scratch = [0u8; 64];
    let read = timeout(T, s.read(&mut scratch))
        .await
        .expect("server should reap idle connection");
    let n = read.expect("read should not error");
    assert_eq!(n, 0, "expected EOF, got {n} bytes");
}

/// 22b — a misbehaving peer that floods frames past the token bucket
/// must get a `Goodbye { reason: "rate limit exceeded" }` and have
/// its connection torn down. Tight bucket so the test is quick.
#[tokio::test]
async fn flood_triggers_rate_limit_goodbye() {
    let limits = ConnectionLimits {
        idle_timeout: Duration::from_secs(30),
        rate_burst: 4,
        rate_refill_per_sec: 1,
    };
    let (addr, _guard) = spawn_server_with_limits(limits).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut r, mut w) = stream.split();

    // Register so we get past the handshake.
    write_message(
        &mut w,
        &ClientMessage::Register {
            protocol_version: PROTOCOL_VERSION,
            email: "flood@example.com".into(),
            username: "flooder".into(),
            password: "hunter2hunter".into(),
            device_label: None,
        },
    )
    .await
    .unwrap();
    // Welcome.
    let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();

    // Burst Heartbeats well past the bucket capacity. The server should
    // accept the first few and then close us out with a Goodbye.
    for _ in 0..32 {
        if write_message(&mut w, &ClientMessage::Heartbeat).await.is_err() {
            break;
        }
    }

    // Drain inbound until we see the Goodbye or the socket closes.
    let mut got_goodbye = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next: Result<Result<ServerMessage, _>, _> =
            timeout(remaining, read_message(&mut r)).await;
        match next {
            Ok(Ok(ServerMessage::Goodbye { reason })) => {
                assert!(
                    reason.contains("rate"),
                    "expected rate-limit reason, got {reason:?}"
                );
                got_goodbye = true;
                break;
            }
            Ok(Ok(_)) => continue, // Heartbeat replies before the cap fires
            Ok(Err(_)) => break,    // EOF — server has closed; that's fine too
            Err(_) => panic!("server never closed the flooded connection"),
        }
    }
    assert!(got_goodbye, "expected ServerMessage::Goodbye");
}

/// 22c — a fresh login while the player is mid-hand must take over
/// the existing seat: the new socket sees `JoinedTable` and a
/// re-issued `Prompt` matching the in-flight action, and submitting
/// against that prompt resolves the engine's blocked `act()` so the
/// hand keeps progressing.
#[tokio::test]
async fn reconnect_takes_over_seat_mid_hand() {
    let (addr, _guard) = spawn_server().await;

    let mut alice = register_and_join(addr, "alice").await;
    let bob = register_and_join(addr, "bob").await;

    // Bob keeps the hand alive by checking/calling on every prompt.
    let bob_task = tokio::spawn(async move {
        let mut bob = bob;
        let (mut r, mut w) = bob.split();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg: ServerMessage = match timeout(remaining, read_message(&mut r)).await {
                Ok(Ok(m)) => m,
                _ => return,
            };
            if let ServerMessage::Prompt { table_id, hand_id, ref legal, .. } = msg {
                let action = if legal.can_check { Action::Check } else { Action::Call };
                let _ = write_message(
                    &mut w,
                    &ClientMessage::SubmitAction { table_id, hand_id, action },
                )
                .await;
            }
            if matches!(
                msg,
                ServerMessage::TableEvent { event: EngineEvent::HandEnded { .. }, .. }
            ) {
                return;
            }
        }
    });

    // Wait for Alice's first Prompt on the original socket. Once we
    // see it, the engine's `act()` is blocked on the oneshot — that's
    // exactly when the reconnect handover has to work.
    {
        let (mut r, _w) = alice.split();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let msg: ServerMessage = timeout(remaining, read_message(&mut r))
                .await
                .expect("timed out waiting for alice's prompt")
                .unwrap();
            if matches!(msg, ServerMessage::Prompt { .. }) {
                break;
            }
            if matches!(
                msg,
                ServerMessage::TableEvent { event: EngineEvent::HandEnded { .. }, .. }
            ) {
                panic!("hand ended before alice was prompted");
            }
        }
    }

    // New device authenticates as alice. The registry will revoke the
    // first session, and the session machinery should swap the seat's
    // SeatLink to this connection and re-issue the migrated Prompt.
    let mut alice2 = TcpStream::connect(addr).await.unwrap();
    let (mut r2, mut w2) = alice2.split();
    write_message(
        &mut w2,
        &ClientMessage::Authenticate {
            protocol_version: PROTOCOL_VERSION,
            mode: AuthMode::Password {
                identifier: "alice".into(),
                password: "hunter2hunter".into(),
            },
            device_label: None,
        },
    )
    .await
    .unwrap();

    // Expect, in order: Welcome, JoinedTable, then a re-issued Prompt.
    let mut got_joined = false;
    let mut prompt_ctx: Option<(_, _, Action)> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while prompt_ctx.is_none() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg: ServerMessage = timeout(remaining, read_message(&mut r2))
            .await
            .expect("timed out waiting for new-device prompt")
            .unwrap();
        match msg {
            ServerMessage::Welcome { .. } => {}
            ServerMessage::JoinedTable { .. } => got_joined = true,
            ServerMessage::Prompt { table_id, hand_id, ref legal, .. } => {
                let action = if legal.can_check { Action::Check } else { Action::Call };
                prompt_ctx = Some((table_id, hand_id, action));
            }
            _ => {}
        }
    }
    assert!(got_joined, "new device never received JoinedTable");
    let (table_id, hand_id, action) = prompt_ctx.unwrap();

    // Resolve the prompt on the new socket. If the seat handover
    // worked, the engine's blocked `act()` returns this action and
    // the hand continues — we should see at least one TableEvent
    // back before the test exits.
    write_message(
        &mut w2,
        &ClientMessage::SubmitAction { table_id, hand_id, action },
    )
    .await
    .unwrap();

    let mut saw_progress = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !saw_progress {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let msg: ServerMessage = match timeout(remaining, read_message(&mut r2)).await {
            Ok(Ok(m)) => m,
            _ => break,
        };
        if matches!(msg, ServerMessage::TableEvent { .. } | ServerMessage::Prompt { .. }) {
            saw_progress = true;
        }
    }
    assert!(
        saw_progress,
        "engine never moved past alice's reconnected action"
    );
    bob_task.abort();
}

async fn register_and_join(addr: std::net::SocketAddr, name: &str) -> TcpStream {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut r, mut w) = stream.split();
    write_message(
        &mut w,
        &ClientMessage::Register {
            protocol_version: PROTOCOL_VERSION,
            email: format!("{name}@example.com"),
            username: name.into(),
            password: "hunter2hunter".into(),
            device_label: None,
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
    write_message(&mut w, &ClientMessage::JoinTable { table_id: 1, buy_in: 200 })
        .await
        .unwrap();
    let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
    drop((r, w));
    stream
}
