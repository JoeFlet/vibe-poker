//! End-to-end handshake tests over a real TCP socket pair.
//!
//! Spawns the listener on `127.0.0.1:0` (ephemeral port), connects a
//! client, and walks the protocol through the cases defined in
//! `poker_engine::net::protocol`.

use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use poker_engine::net::protocol::{
    ClientMessage, ServerMessage, PROTOCOL_VERSION,
};
use poker_server::{
    handle_connection, read_message, write_message, Registry, ServerContext, TableManager,
};

const T: Duration = Duration::from_secs(5);

/// Bind an ephemeral listener and spawn an accept loop bound to a
/// fresh, temp-directory-backed registry. Returns the bound address
/// plus a handle that keeps the temp dir alive.
async fn spawn_server() -> (std::net::SocketAddr, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let registry = Arc::new(Registry::open(dir.path()).await.unwrap());
    let ctx = ServerContext { registry, tables: TableManager::new() };
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

#[tokio::test]
async fn hello_yields_welcome_with_fresh_id() {
    let (addr, _guard) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.split();

    write_message(
        &mut write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: "alice".into(),
        },
    )
    .await
    .unwrap();

    let resp: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Welcome { protocol_version, username, stats, .. } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(username, "alice");
            assert_eq!(stats.hands, 0);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}

#[tokio::test]
async fn protocol_mismatch_rejects() {
    let (addr, _guard) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.split();

    write_message(
        &mut write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION + 999,
            username: "alice".into(),
        },
    )
    .await
    .unwrap();

    let resp: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    assert!(matches!(resp, ServerMessage::Rejected { .. }));
}

#[tokio::test]
async fn invalid_username_rejects() {
    let (addr, _guard) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.split();

    write_message(
        &mut write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: "x".into(),
        },
    )
    .await
    .unwrap();

    let resp: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Rejected { reason, .. } => assert!(reason.contains("username")),
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn double_login_rejects_second() {
    let (addr, _guard) = spawn_server().await;

    let mut s1 = TcpStream::connect(addr).await.unwrap();
    let (mut r1, mut w1) = s1.split();
    write_message(
        &mut w1,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: "alice".into(),
        },
    )
    .await
    .unwrap();
    let welcome: ServerMessage = timeout(T, read_message(&mut r1)).await.unwrap().unwrap();
    assert!(matches!(welcome, ServerMessage::Welcome { .. }));

    // Second connection with the same name must be rejected while the
    // first is still online.
    let mut s2 = TcpStream::connect(addr).await.unwrap();
    let (mut r2, mut w2) = s2.split();
    write_message(
        &mut w2,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: "alice".into(),
        },
    )
    .await
    .unwrap();
    let resp: ServerMessage = timeout(T, read_message(&mut r2)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Rejected { reason, .. } => {
            assert!(reason.contains("already"), "got {reason:?}")
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn heartbeat_round_trip_then_disconnect() {
    let (addr, _guard) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.split();

    write_message(
        &mut write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: "alice".into(),
        },
    )
    .await
    .unwrap();
    let _: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();

    write_message(&mut write, &ClientMessage::Heartbeat).await.unwrap();
    let echo: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    assert!(matches!(echo, ServerMessage::Heartbeat));

    write_message(&mut write, &ClientMessage::Disconnect).await.unwrap();
    let bye: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    assert!(matches!(bye, ServerMessage::Goodbye { .. }));
}

#[tokio::test]
async fn reconnect_recalls_player_id_and_stats() {
    let (addr, _guard) = spawn_server().await;

    let id1 = {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let (mut read, mut write) = stream.split();
        write_message(
            &mut write,
            &ClientMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                username: "alice".into(),
            },
        )
        .await
        .unwrap();
        let resp: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
        let id = match resp {
            ServerMessage::Welcome { player_id, .. } => player_id,
            other => panic!("expected Welcome, got {other:?}"),
        };
        write_message(&mut write, &ClientMessage::Disconnect).await.unwrap();
        // Drain the goodbye so the server-side logout completes cleanly
        // before we try to reconnect under the same name.
        let _: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
        id
    };

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.split();
    write_message(
        &mut write,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: "alice".into(),
        },
    )
    .await
    .unwrap();
    let resp: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Welcome { player_id, .. } => assert_eq!(player_id, id1),
        other => panic!("expected Welcome, got {other:?}"),
    }
}
