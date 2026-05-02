//! Full-client integration test for mid-hand leave.
//!
//! Uses two LiveHarness clients (backed by the same in-process server)
//! to validate the end-to-end behavior described in task 9.1:
//! one client issues Intent::LeaveTable, both clients converge on a
//! consistent post-leave state.
//!
//! The test mirrors what a `NativeClient` would observe; `LiveHarness`
//! drives the same `ClientCore` state machine over real TCP.

use std::time::Duration;

use poker_client_core::{Intent, Phase};
use poker_engine::game::Action;
use poker_full_flow_tests::{LiveHarness, spawn_server};

const T: Duration = Duration::from_secs(5);

async fn seat_client(h: &mut LiveHarness, email: &str, username: &str) {
    h.issue(Intent::Register {
        email: email.into(),
        username: username.into(),
        password: "hunter2hunter".into(),
        device_label: None,
    })
    .await;
    h.wait_for(|v| v.phase == Phase::Lobby, T, &format!("{username} phase=Lobby"))
        .await;
    h.issue(Intent::JoinTable { table_id: 1, buy_in: 200 }).await;
    h.wait_for(
        |v| matches!(v.phase, Phase::Seated { .. }),
        T,
        &format!("{username} phase=Seated"),
    )
    .await;
}

fn table_id(h: &LiveHarness) -> u32 {
    match h.view().phase {
        Phase::Seated { table_id, .. } => table_id,
        _ => panic!("not seated"),
    }
}

/// After one client issues Intent::LeaveTable mid-hand:
/// - Leaver ends in Phase::Lobby
/// - Remaining client ends in Phase::Seated with the leaver's seat empty /
///   hand ended (current_hand is None after the forced fold settles the hand)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_clients_converge_after_mid_hand_leave() {
    let (addr, _guard) = spawn_server().await;

    let mut alice = LiveHarness::connect(addr).await;
    let mut bob = LiveHarness::connect(addr).await;

    seat_client(&mut alice, "alice@example.com", "alice").await;
    seat_client(&mut bob, "bob@example.com", "bob").await;

    alice.wait_for(|v| v.seats.len() == 2, T, "alice sees 2 seats").await;
    bob.wait_for(|v| v.seats.len() == 2, T, "bob sees 2 seats").await;
    alice.wait_for(|v| v.current_hand.is_some(), T, "alice hand started").await;
    bob.wait_for(|v| v.current_hand.is_some(), T, "bob hand started").await;

    // Determine who is acting first.
    let alice_acting = tokio::time::timeout(
        Duration::from_millis(500),
        async {
            alice
                .wait_for(
                    |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
                    T,
                    "alice prompt",
                )
                .await
        },
    )
    .await
    .is_ok();

    if !alice_acting {
        bob.wait_for(
            |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
            T,
            "bob prompt",
        )
        .await;
    }

    // The acting player submits Call so both players get to act, then
    // whoever is NOT acting leaves (or vice-versa). For simplicity:
    // leaver is whoever is NOT currently acting.
    let (leaver, stayer) = if alice_acting {
        // alice is acting → bob leaves (non-acting)
        (&mut bob, &mut alice)
    } else {
        // bob is acting → alice leaves (non-acting)
        (&mut alice, &mut bob)
    };

    let tid = table_id(leaver);

    // Leaver sends Intent::LeaveTable.
    leaver.issue(Intent::LeaveTable { table_id: tid }).await;

    // Leaver should reach Lobby quickly.
    leaver
        .wait_for(|v| v.phase == Phase::Lobby, T, "leaver in Lobby")
        .await;

    // Stayer (still seated) should see the leaver's seat eventually vacate
    // or the hand end. Call the prompt to advance the engine.
    stayer.issue(Intent::SubmitAction { action: Action::Call }).await;

    // After the engine advances and the departed player fast-folds,
    // stayer should see the hand end.
    stayer
        .wait_for(|v| v.current_hand.is_none(), T, "stayer hand ended after leaver departs")
        .await;

    // Final state assertions:
    // 1. Leaver is in Lobby with no current_hand.
    assert_eq!(leaver.view().phase, Phase::Lobby, "leaver must be in Lobby");
    assert!(leaver.view().current_hand.is_none(), "leaver must have no current_hand");

    // 2. Stayer is still Seated with no current_hand (hand is over).
    assert!(
        matches!(stayer.view().phase, Phase::Seated { .. }),
        "stayer must remain Seated"
    );
    assert!(stayer.view().current_hand.is_none(), "stayer must have no current_hand");
}
