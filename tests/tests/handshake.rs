//! Full-flow handshake / lobby tests. The matching state-machine
//! unit tests live in `crates/poker-client-core/src/core.rs`; these
//! tests drive the same transitions over real TCP against an
//! in-process `poker-server`.

use std::time::Duration;

use poker_client_core::{Intent, Phase};
use poker_full_flow_tests::{LiveHarness, spawn_server};

const T: Duration = Duration::from_secs(5);

#[tokio::test]
async fn register_lands_us_in_lobby_with_session_key() {
    let (addr, _guard) = spawn_server().await;
    let mut h = LiveHarness::connect(addr).await;

    h.issue(Intent::Register {
        email: "alice@example.com".into(),
        username: "alice".into(),
        password: "hunter2hunter".into(),
        device_label: Some("test".into()),
    })
    .await;

    h.wait_for(|v| v.phase == Phase::Lobby, T, "phase=Lobby").await;

    let v = h.view();
    assert_eq!(v.username.as_deref(), Some("alice"));
    assert!(v.player_id.is_some(), "player id must be set after Welcome");
    assert!(
        v.lifetime_stats.is_some(),
        "lifetime stats must accompany the welcome"
    );
}

#[tokio::test]
async fn list_tables_populates_view() {
    let (addr, _guard) = spawn_server().await;
    let mut h = LiveHarness::connect(addr).await;

    h.issue(Intent::Register {
        email: "alice@example.com".into(),
        username: "alice".into(),
        password: "hunter2hunter".into(),
        device_label: None,
    })
    .await;
    h.wait_for(|v| v.phase == Phase::Lobby, T, "phase=Lobby").await;

    h.issue(Intent::ListTables).await;
    h.wait_for(|v| !v.tables.is_empty(), T, "tables.len > 0").await;

    let v = h.view();
    assert_eq!(v.tables.len(), 1);
    assert_eq!(v.tables[0].table_id, 1);
    assert_eq!(v.tables[0].max_seats, 2);
}

#[tokio::test]
async fn join_table_advances_to_seated() {
    let (addr, _guard) = spawn_server().await;
    let mut h = LiveHarness::connect(addr).await;

    h.issue(Intent::Register {
        email: "alice@example.com".into(),
        username: "alice".into(),
        password: "hunter2hunter".into(),
        device_label: None,
    })
    .await;
    h.wait_for(|v| v.phase == Phase::Lobby, T, "phase=Lobby").await;

    h.issue(Intent::JoinTable {
        table_id: 1,
        buy_in: 200,
    })
    .await;
    h.wait_for(
        |v| matches!(v.phase, Phase::Seated { .. }),
        T,
        "phase=Seated",
    )
    .await;

    let v = h.view();
    assert_eq!(v.our_seat(), Some(0), "first joiner should land in seat 0");
    assert_eq!(v.seats.len(), 1, "only one player at the table so far");
    assert_eq!(v.seats[0].username, "alice");
    assert_eq!(v.seats[0].stack, 200);
}

/// Heads-up sit-down with two real clients connected to the same
/// server. Validates that the server's broadcast updates land on
/// both clients and that each correctly identifies its own seat.
#[tokio::test]
async fn two_clients_seat_at_heads_up_table() {
    let (addr, _guard) = spawn_server().await;

    let mut alice = LiveHarness::connect(addr).await;
    alice
        .issue(Intent::Register {
            email: "alice@example.com".into(),
            username: "alice".into(),
            password: "hunter2hunter".into(),
            device_label: None,
        })
        .await;
    alice
        .wait_for(|v| v.phase == Phase::Lobby, T, "alice phase=Lobby")
        .await;
    alice
        .issue(Intent::JoinTable {
            table_id: 1,
            buy_in: 200,
        })
        .await;
    alice
        .wait_for(
            |v| matches!(v.phase, Phase::Seated { .. }),
            T,
            "alice phase=Seated",
        )
        .await;

    let mut bob = LiveHarness::connect(addr).await;
    bob.issue(Intent::Register {
        email: "bob@example.com".into(),
        username: "bob".into(),
        password: "hunter2hunter".into(),
        device_label: None,
    })
    .await;
    bob.wait_for(|v| v.phase == Phase::Lobby, T, "bob phase=Lobby")
        .await;
    bob.issue(Intent::JoinTable {
        table_id: 1,
        buy_in: 200,
    })
    .await;
    bob.wait_for(
        |v| matches!(v.phase, Phase::Seated { .. }),
        T,
        "bob phase=Seated",
    )
    .await;

    // Each client knows its own seat; both see two seats total.
    alice
        .wait_for(|v| v.seats.len() == 2, T, "alice sees 2 seats")
        .await;
    let av = alice.view();
    let bv = bob.view();
    assert_eq!(av.our_seat(), Some(0));
    assert_eq!(bv.our_seat(), Some(1));
    assert_eq!(av.seats.len(), 2);
    assert_eq!(bv.seats.len(), 2);
}
