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
    AuthMode, ClientMessage, PROTOCOL_VERSION, ServerMessage,
};
use poker_server::{
    Registry, ServerContext, TableManager, handle_connection, read_message, write_message,
};

const T: Duration = Duration::from_secs(5);
const PW: &str = "hunter2hunter";

/// Bind an ephemeral listener and spawn an accept loop bound to a
/// fresh, temp-directory-backed registry. Returns the bound address
/// plus a handle that keeps the temp dir alive.
async fn spawn_server() -> (std::net::SocketAddr, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let registry = Arc::new(Registry::open(dir.path()).await.unwrap());
    let ctx = ServerContext {
        registry,
        tables: TableManager::new(),
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

fn register(email: &str, username: &str) -> ClientMessage {
    ClientMessage::Register {
        protocol_version: PROTOCOL_VERSION,
        email: email.into(),
        username: username.into(),
        password: PW.into(),
        device_label: None,
    }
}

fn auth_password(identifier: &str) -> ClientMessage {
    ClientMessage::Authenticate {
        protocol_version: PROTOCOL_VERSION,
        mode: AuthMode::Password {
            identifier: identifier.into(),
            password: PW.into(),
        },
        device_label: None,
    }
}

fn auth_session(key: &str) -> ClientMessage {
    ClientMessage::Authenticate {
        protocol_version: PROTOCOL_VERSION,
        mode: AuthMode::Session { key: key.into() },
        device_label: None,
    }
}

#[tokio::test]
async fn register_yields_welcome_with_session_key() {
    let (addr, _guard) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.split();

    write_message(&mut write, &register("alice@example.com", "alice"))
        .await
        .unwrap();

    let resp: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Welcome {
            protocol_version,
            username,
            session_key,
            stats,
            ..
        } => {
            assert_eq!(protocol_version, PROTOCOL_VERSION);
            assert_eq!(username, "alice");
            assert!(!session_key.is_empty());
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
        &ClientMessage::Register {
            protocol_version: PROTOCOL_VERSION + 999,
            email: "alice@example.com".into(),
            username: "alice".into(),
            password: PW.into(),
            device_label: None,
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

    write_message(&mut write, &register("alice@example.com", "x"))
        .await
        .unwrap();

    let resp: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Rejected { reason, .. } => assert!(reason.contains("username")),
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn duplicate_username_rejects() {
    let (addr, _guard) = spawn_server().await;

    // First registration takes the name.
    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let (mut r, mut w) = s.split();
        write_message(&mut w, &register("a@b.co", "alice")).await.unwrap();
        let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
        write_message(&mut w, &ClientMessage::Disconnect).await.unwrap();
        let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
    }

    let mut s = TcpStream::connect(addr).await.unwrap();
    let (mut r, mut w) = s.split();
    write_message(&mut w, &register("c@d.co", "alice")).await.unwrap();
    let resp: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Rejected { reason, .. } => {
            assert!(reason.contains("username"), "got {reason:?}")
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn wrong_password_rejects() {
    let (addr, _guard) = spawn_server().await;

    {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let (mut r, mut w) = s.split();
        write_message(&mut w, &register("a@b.co", "alice")).await.unwrap();
        let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
        write_message(&mut w, &ClientMessage::Disconnect).await.unwrap();
        let _: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
    }

    let mut s = TcpStream::connect(addr).await.unwrap();
    let (mut r, mut w) = s.split();
    write_message(
        &mut w,
        &ClientMessage::Authenticate {
            protocol_version: PROTOCOL_VERSION,
            mode: AuthMode::Password {
                identifier: "alice".into(),
                password: "wrongpassword".into(),
            },
            device_label: None,
        },
    )
    .await
    .unwrap();
    let resp: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Rejected { reason, .. } => {
            assert!(reason.contains("credentials"), "got {reason:?}")
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn second_login_revokes_first_session() {
    let (addr, _guard) = spawn_server().await;

    // First session: register + receive welcome.
    let mut s1 = TcpStream::connect(addr).await.unwrap();
    let (mut r1, mut w1) = s1.split();
    write_message(&mut w1, &register("a@b.co", "alice")).await.unwrap();
    let welcome1: ServerMessage = timeout(T, read_message(&mut r1)).await.unwrap().unwrap();
    let session_key_1 = match welcome1 {
        ServerMessage::Welcome { session_key, .. } => session_key,
        other => panic!("expected Welcome, got {other:?}"),
    };

    // Second session: authenticate with password from "another device".
    let mut s2 = TcpStream::connect(addr).await.unwrap();
    let (mut r2, mut w2) = s2.split();
    write_message(&mut w2, &auth_password("alice")).await.unwrap();
    let welcome2: ServerMessage = timeout(T, read_message(&mut r2)).await.unwrap().unwrap();
    assert!(matches!(welcome2, ServerMessage::Welcome { .. }));

    // Poke the first connection so the reader notices its revoked
    // flag and disconnects with Goodbye.
    write_message(&mut w1, &ClientMessage::Heartbeat).await.unwrap();
    let bye: ServerMessage = timeout(T, read_message(&mut r1)).await.unwrap().unwrap();
    match bye {
        ServerMessage::Goodbye { reason } => assert!(reason.contains("revoked"), "got {reason}"),
        other => panic!("expected Goodbye, got {other:?}"),
    }

    // The first session's key is also revoked at the DB level.
    let mut s3 = TcpStream::connect(addr).await.unwrap();
    let (mut r3, mut w3) = s3.split();
    write_message(&mut w3, &auth_session(&session_key_1)).await.unwrap();
    let resp: ServerMessage = timeout(T, read_message(&mut r3)).await.unwrap().unwrap();
    match resp {
        ServerMessage::Rejected { reason, .. } => {
            assert!(reason.contains("revoked"), "got {reason:?}")
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
}

#[tokio::test]
async fn heartbeat_round_trip_then_disconnect() {
    let (addr, _guard) = spawn_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = stream.split();

    write_message(&mut write, &register("a@b.co", "alice")).await.unwrap();
    let _: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();

    write_message(&mut write, &ClientMessage::Heartbeat).await.unwrap();
    let echo: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    assert!(matches!(echo, ServerMessage::Heartbeat));

    write_message(&mut write, &ClientMessage::Disconnect).await.unwrap();
    let bye: ServerMessage = timeout(T, read_message(&mut read)).await.unwrap().unwrap();
    assert!(matches!(bye, ServerMessage::Goodbye { .. }));
}

#[tokio::test]
async fn reconnect_via_session_key_recalls_player_id() {
    let (addr, _guard) = spawn_server().await;

    // Register, capture id + session key, disconnect.
    let mut s1 = TcpStream::connect(addr).await.unwrap();
    let (mut r1, mut w1) = s1.split();
    write_message(&mut w1, &register("a@b.co", "alice")).await.unwrap();
    let welcome1: ServerMessage = timeout(T, read_message(&mut r1)).await.unwrap().unwrap();
    let (id1, key) = match welcome1 {
        ServerMessage::Welcome {
            player_id,
            session_key,
            ..
        } => (player_id, session_key),
        other => panic!("expected Welcome, got {other:?}"),
    };
    write_message(&mut w1, &ClientMessage::Disconnect).await.unwrap();
    let _: ServerMessage = timeout(T, read_message(&mut r1)).await.unwrap().unwrap();

    // Reconnect via the session key — same player_id, same key.
    let mut s2 = TcpStream::connect(addr).await.unwrap();
    let (mut r2, mut w2) = s2.split();
    write_message(&mut w2, &auth_session(&key)).await.unwrap();
    let welcome2: ServerMessage = timeout(T, read_message(&mut r2)).await.unwrap().unwrap();
    match welcome2 {
        ServerMessage::Welcome {
            player_id,
            session_key,
            ..
        } => {
            assert_eq!(player_id, id1);
            assert_eq!(session_key, key);
        }
        other => panic!("expected Welcome, got {other:?}"),
    }
}
