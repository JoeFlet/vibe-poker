//! Integration tests for graceful-disconnect force-fold and
//! superseded-session handoff behavior.
//!
//! Covers server-behavior spec: "Force-fold on mid-hand leave" §§
//! "Graceful disconnect mid-hand force-folds" and
//! "Session handoff does NOT force-fold".

use std::time::Duration;

use poker_client_core::{Intent, Phase};
use poker_engine::game::{Action, EngineEvent};
use poker_engine::net::protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage};
use poker_full_flow_tests::{LiveHarness, spawn_server};
use poker_server::{read_message, write_message};
use tokio::net::TcpStream;
use tokio::time::timeout;

const T: Duration = Duration::from_secs(5);

async fn seat_via_harness(h: &mut LiveHarness, email: &str, username: &str) {
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

/// Minimal raw TCP player — register, sit, wait for prompts, respond.
struct RawPlayer {
    read: tokio::net::tcp::OwnedReadHalf,
    write: tokio::net::tcp::OwnedWriteHalf,
}

impl RawPlayer {
    async fn connect_and_register(addr: std::net::SocketAddr, email: &str, username: &str) -> Self {
        let stream = TcpStream::connect(addr).await.unwrap();
        stream.set_nodelay(true).ok();
        let (read, mut write) = stream.into_split();
        write_message(
            &mut write,
            &ClientMessage::Register {
                protocol_version: PROTOCOL_VERSION,
                email: email.into(),
                username: username.into(),
                password: "hunter2hunter".into(),
                device_label: None,
            },
        )
        .await
        .unwrap();
        let mut r = read;
        let welcome: ServerMessage = timeout(T, read_message(&mut r)).await.unwrap().unwrap();
        assert!(matches!(welcome, ServerMessage::Welcome { .. }), "expected Welcome");
        let read = r;
        Self { read, write }
    }

    async fn join_table(&mut self) {
        write_message(&mut self.write, &ClientMessage::JoinTable { table_id: 1, buy_in: 200 })
            .await
            .unwrap();
        let resp: ServerMessage =
            timeout(T, read_message(&mut self.read)).await.unwrap().unwrap();
        assert!(
            matches!(resp, ServerMessage::JoinedTable { .. }),
            "expected JoinedTable, got {resp:?}"
        );
    }

    /// Read messages until predicate holds; return matching message.
    async fn read_until<F: Fn(&ServerMessage) -> bool>(
        &mut self,
        pred: F,
        t: Duration,
        label: &str,
    ) -> ServerMessage {
        let deadline = tokio::time::Instant::now() + t;
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or_default();
            let msg: ServerMessage = timeout(remaining, read_message(&mut self.read))
                .await
                .unwrap_or_else(|_| panic!("{label}: timed out"))
                .unwrap_or_else(|e| panic!("{label}: wire error {e:?}"));
            if pred(&msg) {
                return msg;
            }
        }
    }

    /// Drain until a Prompt arrives; return (table_id, hand_id).
    /// Drop the write half cleanly (FIN), simulating a graceful disconnect.
    fn close_write(self) -> tokio::net::tcp::OwnedReadHalf {
        // Drop `write` → sends FIN to server. The read half can still receive
        // in-flight server messages; we just discard them.
        drop(self.write);
        self.read
    }
}

// ─── Test 4.1 ─────────────────────────────────────────────────────────────────
//
// Seat two players. Prompt arrives at seat 0 (raw player). Seat 0 closes its
// TCP connection. Seat 1 (LiveHarness) should see ActionTaken { seat:0, Fold }
// within one tick.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_disconnect_mid_hand_folds() {
    let (addr, _guard) = spawn_server().await;

    // Seat 0: raw TCP player that will disconnect.
    let mut p0 = RawPlayer::connect_and_register(addr, "player0@x.com", "player0").await;
    p0.join_table().await;

    // Seat 1: LiveHarness observer.
    let mut p1 = LiveHarness::connect(addr).await;
    seat_via_harness(&mut p1, "player1@x.com", "player1").await;

    // Wait for p1 to see two seats (hand will start).
    p1.wait_for(|v| v.seats.len() == 2, T, "p1 sees 2 seats").await;
    p1.wait_for(|v| v.current_hand.is_some(), T, "p1 hand started").await;

    // p0 drains until it gets a prompt (it may or may not be first actor).
    // If p1 gets the prompt, we need to handle that. For simplicity, we
    // wait for p0's prompt by draining. If instead p1 gets it, p0 will
    // wait a bit; the test still checks graceful disconnect -> fold.
    //
    // Strategy: run both in parallel. Whichever gets the prompt,
    // we disconnect p0 (even if they don't currently hold a prompt,
    // the departed flag ensures fast-fold when their turn comes).

    // Wait for a hand to start for p0 too (drain until HandStarted).
    p0.read_until(
        |m| matches!(m, ServerMessage::TableEvent { event: EngineEvent::HandStarted { .. }, .. }),
        T,
        "p0 hand started",
    )
    .await;

    // Wait until we know who has the prompt.
    // Try p1 first (with a short window to detect if p1 is acting).
    let p1_has_prompt = tokio::time::timeout(
        Duration::from_millis(500),
        async {
            p1.wait_for(
                |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
                T,
                "p1 prompt check",
            )
            .await
        },
    )
    .await
    .is_ok();

    // Disconnect p0 (regardless of who holds the prompt).
    let start = tokio::time::Instant::now();
    let _p0_read = p0.close_write(); // drops TCP write half → FIN

    if p1_has_prompt {
        // p1 holds the prompt; p1 must call so the engine reaches p0's seat.
        // p0 has departed=true, so p0 fast-folds.
        p1.issue(Intent::SubmitAction { action: Action::Call }).await;
    } else {
        // p0 held the prompt; cancel_pending was triggered by the disconnect.
        // p0 folds immediately. p1 waits to see fold / hand end.
    }

    // In either case, p0 should fold quickly.
    p1.wait_for(
        |v| {
            v.current_hand
                .as_ref()
                .map_or(false, |h| !h.folded_seats.is_empty())
                || v.current_hand.is_none() // or hand ended (p1 was p0's only opponent)
        },
        T,
        "p1 sees p0 fold or hand end",
    )
    .await;

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(4),
        "disconnect fold took {elapsed:?}; expected sub-deadline"
    );
}

// ─── Test 4.2 ─────────────────────────────────────────────────────────────────
//
// Seat p0, start a hand, prompt arrives at p0. Then open a SECOND connection
// as the same user (session handoff). Assert the old session's cleanup does
// NOT force-fold — the new session sees the re-issued Prompt and can submit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn superseded_session_does_not_force_fold() {
    let (addr, _guard) = spawn_server().await;

    // p0 and p1 seat up.
    let mut p0 = LiveHarness::connect(addr).await;
    seat_via_harness(&mut p0, "player0@x.com", "player0").await;

    let mut p1 = LiveHarness::connect(addr).await;
    seat_via_harness(&mut p1, "player1@x.com", "player1").await;

    p0.wait_for(|v| v.seats.len() == 2, T, "p0 sees 2 seats").await;
    p1.wait_for(|v| v.seats.len() == 2, T, "p1 sees 2 seats").await;
    p0.wait_for(|v| v.current_hand.is_some(), T, "p0 hand started").await;
    p1.wait_for(|v| v.current_hand.is_some(), T, "p1 hand started").await;

    // Wait for p0 to have the prompt. If p1 has it, p1 calls first so
    // p0 gets prompted second.
    let p0_awaiting = tokio::time::timeout(
        Duration::from_millis(500),
        async {
            p0.wait_for(
                |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
                T,
                "p0 prompt",
            )
            .await
        },
    )
    .await
    .is_ok();

    if !p0_awaiting {
        // p1 has the prompt; p1 calls so engine advances to p0.
        p1.wait_for(
            |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
            T,
            "p1 prompt (intermediate)",
        )
        .await;
        p1.issue(Intent::SubmitAction { action: Action::Call }).await;
        // Now wait for p0's prompt.
        p0.wait_for(
            |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
            T,
            "p0 prompt after p1 call",
        )
        .await;
    }

    assert!(
        p0.view().current_hand.as_ref().map_or(false, |h| h.awaiting_action),
        "p0 should have active prompt before handoff"
    );

    // Capture session key before superseding.
    let session_key = p0.view().lifetime_stats.as_ref().map(|_| ()).is_some();
    let _ = session_key;

    // Open a SECOND connection as the same user (session handoff).
    // Use a raw TCP client to authenticate with session key.
    // We need p0's session key from the Welcome. Let's use Authenticate instead.
    // Actually, the simplest: just Register under a DIFFERENT username for p1,
    // and re-authenticate p0 by opening a new LiveHarness with the same credentials.

    let mut p0_new = LiveHarness::connect(addr).await;
    // Authenticate as p0 again (same email/password) — this supersedes the old session.
    p0_new
        .issue(Intent::AuthenticatePassword {
            identifier: "player0@x.com".into(),
            password: "hunter2hunter".into(),
            device_label: None,
        })
        .await;
    p0_new
        .wait_for(|v| matches!(v.phase, Phase::Seated { .. }), T, "p0_new reconnected seated")
        .await;

    // The new session should receive the re-issued Prompt (seat was mid-hand).
    p0_new
        .wait_for(
            |v| v.current_hand.as_ref().map_or(false, |h| h.awaiting_action),
            T,
            "p0_new receives re-issued prompt",
        )
        .await;

    // Submit a valid action from the new session — hand should NOT have been
    // force-folded by the old session's cleanup.
    p0_new.issue(Intent::SubmitAction { action: Action::Fold }).await;

    // p1 should see the hand end normally.
    p1.wait_for(|v| v.current_hand.is_none(), T, "p1 hand ended normally").await;
}
