//! Integration tests for mid-hand leave and force-fold semantics.
//!
//! Covers server-behavior spec: "Force-fold on mid-hand leave".

use std::time::Duration;

use poker_client_core::{Intent, Phase};
use poker_engine::game::Action;
use poker_full_flow_tests::{LiveHarness, spawn_server};

const T: Duration = Duration::from_secs(5);

/// Helper: register, sit, wait for Seated phase.
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

fn is_awaiting(h: &LiveHarness) -> bool {
    h.view()
        .current_hand
        .as_ref()
        .map_or(false, |ch| ch.awaiting_action)
}

/// Wait until one harness has an active prompt; returns `true` if `a` won.
async fn wait_for_any_prompt(a: &mut LiveHarness, b: &mut LiveHarness) -> bool {
    let a_won = tokio::time::timeout(
        Duration::from_millis(500),
        async {
            a.wait_for(
                |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
                T,
                "prompt on a",
            )
            .await
        },
    )
    .await
    .is_ok();

    if !a_won {
        b.wait_for(
            |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
            T,
            "prompt on b",
        )
        .await;
    }
    a_won
}

// ─── Test 3.1 ─────────────────────────────────────────────────────────────────
//
// Two players seated. The non-acting player sends LeaveTable.
// The acting player then calls, which advances the engine to the non-actor's
// seat. The non-actor's connection has `departed=true` so the engine
// fast-folds without waiting for the 5-second deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_acting_player_leave_folds_immediately() {
    let (addr, _guard) = spawn_server().await;

    let mut alice = LiveHarness::connect(addr).await;
    let mut bob = LiveHarness::connect(addr).await;

    seat_client(&mut alice, "alice@example.com", "alice").await;
    seat_client(&mut bob, "bob@example.com", "bob").await;

    alice.wait_for(|v| v.seats.len() == 2, T, "alice sees 2 seats").await;
    bob.wait_for(|v| v.seats.len() == 2, T, "bob sees 2 seats").await;
    alice.wait_for(|v| v.current_hand.is_some(), T, "alice hand started").await;
    bob.wait_for(|v| v.current_hand.is_some(), T, "bob hand started").await;

    let alice_is_acting = wait_for_any_prompt(&mut alice, &mut bob).await;
    let (actor, non_actor) = if alice_is_acting {
        (&mut alice, &mut bob)
    } else {
        (&mut bob, &mut alice)
    };

    assert!(is_awaiting(actor), "actor must have active prompt");
    assert!(!is_awaiting(non_actor), "non-actor must NOT have prompt");

    // Non-acting player leaves.
    let tid = table_id(non_actor);
    non_actor.issue(Intent::LeaveTable { table_id: tid }).await;
    non_actor
        .wait_for(|v| v.phase == Phase::Lobby, T, "non-actor phase=Lobby")
        .await;

    // Actor calls — advances the engine to the non-actor's seat.
    let start = tokio::time::Instant::now();
    actor.issue(Intent::SubmitAction { action: Action::Call }).await;

    // Actor should see the non-actor's seat folded well within the deadline.
    actor
        .wait_for(
            |v| {
                v.current_hand
                    .as_ref()
                    .map_or(false, |h| !h.folded_seats.is_empty())
            },
            T,
            "actor sees non-actor seat folded",
        )
        .await;

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(4),
        "fold took {elapsed:?}; expected fast force-fold via departed flag, not deadline expiry"
    );
}

// ─── Test 3.2 ─────────────────────────────────────────────────────────────────
//
// Two players seated, hand starts, the *acting* player (prompt pending) sends
// LeaveTable. Assert the hand ends quickly (not after deadline) and the
// remaining seat eventually sees HandEnded (current_hand becomes None).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acting_player_leave_folds_and_ends_hand() {
    let (addr, _guard) = spawn_server().await;

    let mut alice = LiveHarness::connect(addr).await;
    let mut bob = LiveHarness::connect(addr).await;

    seat_client(&mut alice, "alice@example.com", "alice").await;
    seat_client(&mut bob, "bob@example.com", "bob").await;

    alice.wait_for(|v| v.seats.len() == 2, T, "alice sees 2 seats").await;
    bob.wait_for(|v| v.seats.len() == 2, T, "bob sees 2 seats").await;
    alice.wait_for(|v| v.current_hand.is_some(), T, "alice hand started").await;
    bob.wait_for(|v| v.current_hand.is_some(), T, "bob hand started").await;

    let alice_is_acting = wait_for_any_prompt(&mut alice, &mut bob).await;
    let (actor, waiter) = if alice_is_acting {
        (&mut alice, &mut bob)
    } else {
        (&mut bob, &mut alice)
    };

    let tid = table_id(actor);
    let start = tokio::time::Instant::now();

    // Acting player sends LeaveTable while holding the prompt.
    actor.issue(Intent::LeaveTable { table_id: tid }).await;
    actor
        .wait_for(|v| v.phase == Phase::Lobby, T, "actor phase=Lobby after leave")
        .await;

    // Waiting player should see the hand end fast (2-player: one fold ends hand).
    waiter
        .wait_for(|v| v.current_hand.is_none(), T, "waiter hand ended")
        .await;

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(4),
        "hand end took {elapsed:?}; expected fast force-fold, not deadline expiry"
    );
}
