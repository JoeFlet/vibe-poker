//! Integration tests driving [`NativeClient`] with
//! [`InMemoryTransport`] + [`InMemoryServer`]. These exercise the
//! full runtime facade without any TCP — scripts pick server
//! messages out of thin air, verify the client responds correctly.
//!
//! The point of this tier is **unit-speed tests of protocol logic**
//! without standing up `poker-server`. For full-flow tests that
//! exercise real server behaviour, see `tests/` at the workspace
//! root.

use std::time::{Duration, Instant};

use poker_client_core::{Intent, Phase};
use poker_client_headless::{InMemoryServer, in_memory_pair};
use poker_client_transport_native::{NativeClient, SessionStore};
use poker_engine::net::protocol::{LifetimeStats, PROTOCOL_VERSION, ServerMessage};

/// Poll the client's snapshot until `predicate` holds, with a
/// timeout. Mirrors the helper in the transport-native runtime
/// tests; kept local so this crate doesn't take a wider dep.
fn wait_for(
    client: &NativeClient,
    label: &str,
    t: Duration,
    mut predicate: impl FnMut(&poker_client_core::ClientView) -> bool,
) {
    let deadline = Instant::now() + t;
    loop {
        if predicate(&client.snapshot()) {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "wait_for({label}) timed out at phase {:?}",
                client.snapshot().phase,
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Pull the (first) server-side handle out of the connections
/// listener, spinning briefly so the runtime's open-connection
/// effect has time to reach the transport.
async fn take_first_connection(
    conns: &mut poker_client_headless::InMemoryConnections,
) -> InMemoryServer {
    tokio::time::timeout(Duration::from_secs(2), conns.next())
        .await
        .expect("no connection opened within timeout")
        .expect("connections channel closed before open")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_client_register_flow_over_in_memory_transport() {
    let (transport, mut conns) = in_memory_pair();
    let session_dir = tempfile::tempdir().unwrap();
    let session_path = session_dir.path().join("session.key");
    let client =
        NativeClient::with_transport(transport, SessionStore::new(session_path.clone())).unwrap();

    client.issue(Intent::Connect {
        addr: "in-memory".into(),
    });
    client.issue(Intent::Register {
        email: "alice@example.com".into(),
        username: "alice".into(),
        password: "hunter2hunter".into(),
        device_label: None,
    });

    // The runtime calls `transport.open()` asynchronously from
    // Effect::OpenConnection → our connection listener gets the
    // server side shortly after.
    let mut server = take_first_connection(&mut conns).await;

    // First ClientMessage off the wire must be the Register.
    match server.recv().await {
        Some(poker_engine::net::protocol::ClientMessage::Register { username, .. }) => {
            assert_eq!(username, "alice");
        }
        other => panic!("expected Register, got {other:?}"),
    }

    // Script the server's Welcome.
    server
        .send(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: 42,
            username: "alice".into(),
            session_key: "scripted-key".into(),
            stats: LifetimeStats::default(),
        })
        .unwrap();

    wait_for(&client, "phase=Lobby", Duration::from_secs(2), |v| {
        v.phase == Phase::Lobby
    });

    let v = client.snapshot();
    assert_eq!(v.player_id, Some(42));
    assert_eq!(v.username.as_deref(), Some("alice"));

    // Session key must have been persisted.
    let persisted = std::fs::read_to_string(&session_path).expect("session file exists");
    assert_eq!(persisted, "scripted-key");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_close_surfaces_as_connection_lost() {
    let (transport, mut conns) = in_memory_pair();
    let dir = tempfile::tempdir().unwrap();
    let client = NativeClient::with_transport(
        transport,
        SessionStore::new(dir.path().join("session.key")),
    )
    .unwrap();

    client.issue(Intent::Connect {
        addr: "in-memory".into(),
    });
    client.issue(Intent::Heartbeat); // just to have something to send

    let mut server = take_first_connection(&mut conns).await;
    // Drain the Heartbeat so the test's recv doesn't block.
    let _ = server.recv().await;

    // Abrupt server close.
    server.close("scripted-disconnect");

    wait_for(
        &client,
        "phase=Ended",
        Duration::from_secs(2),
        |v| matches!(v.phase, Phase::Ended { .. }),
    );

    match client.snapshot().phase {
        Phase::Ended { reason } => assert!(
            reason.contains("scripted-disconnect"),
            "expected reason to carry server close reason; got {reason:?}",
        ),
        other => panic!("unexpected phase {other:?}"),
    }
}
