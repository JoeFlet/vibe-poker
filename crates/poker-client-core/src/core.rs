//! The state machine. Synchronous, deterministic, and the sole owner
//! of the client's authoritative state. See
//! [`docs/CLIENT_PRINCIPLES.md`](../../../../docs/CLIENT_PRINCIPLES.md).

use poker_engine::game::{Action, EngineEvent, HandId, LegalActions, SeatIndex, Street};
use poker_engine::net::protocol::{
    AuthMode, ClientMessage, PROTOCOL_VERSION, ServerMessage, TableId,
};

use crate::effect::{Effect, LogLevel};
use crate::intent::Intent;
use crate::view::{ClientView, CurrentHand, Phase};

/// The state machine. Owns everything that is the client's truth.
///
/// Determinism: contains no `Instant`, no `RandomState`, no clock.
/// Anything that needs wall time goes through [`Effect::Schedule`]
/// so tests drive timing manually.
pub struct ClientCore {
    view: ClientView,
    /// Most recent session key from `Welcome` (or supplied via
    /// `AuthenticateSession`). Persisted via
    /// `Effect::PersistSessionKey` whenever it changes.
    session_key: Option<String>,
    /// `true` between `Intent::ConnectionOpened` and any of
    /// `Disconnect` / `ConnectionLost` / `Goodbye` / terminal
    /// `Rejected`. Decoupled from `Phase` so that the post-handshake
    /// phases (`Authenticating`, `Lobby`, `Seated`, …) can be set
    /// without a separate "connected" flag check.
    connection_open: bool,
    /// Cached pending handshake message that needs to fire once
    /// the connection is open. Used by the
    /// `Connect → Register/Authenticate` flow when the client
    /// pre-issues the handshake before the socket is up.
    pending_handshake: Option<ClientMessage>,
    /// Open prompt addressed to us, awaiting a `SubmitAction` intent.
    /// Cleared as soon as the action is sent or the prompt is
    /// superseded.
    active_prompt: Option<ActivePrompt>,
}

/// Handle on the open prompt. Stored on the core (not the view) so
/// the host can't see internals like the bare `(table_id, hand_id)`
/// pair the server demands echoed back — it just toggles
/// `view.current_hand.awaiting_action` and issues a `SubmitAction`
/// intent when the user picks something.
#[derive(Debug, Clone)]
struct ActivePrompt {
    table_id: TableId,
    hand_id: HandId,
    /// Seat the prompt was addressed to. Currently informational —
    /// always equal to our seat — but retained so that a future
    /// "spectator can see open prompts" mode has the data already.
    #[allow(dead_code)]
    seat: SeatIndex,
    legal: LegalActions,
}

impl ClientCore {
    pub fn new(initial_session_key: Option<String>) -> Self {
        Self {
            view: ClientView::empty(),
            session_key: initial_session_key,
            connection_open: false,
            pending_handshake: None,
            active_prompt: None,
        }
    }

    /// Read-only snapshot. Cheap to call (clones the view); the
    /// frontend can call this on every render.
    pub fn snapshot(&self) -> ClientView {
        self.view.clone()
    }

    /// Whatever session key the host should persist. Returned for
    /// hosts that prefer polling over chasing the
    /// `Effect::PersistSessionKey` stream.
    pub fn session_key(&self) -> Option<&str> {
        self.session_key.as_deref()
    }
}

// ─── Intent handling ────────────────────────────────────────────────

impl ClientCore {
    pub fn handle_intent(&mut self, intent: Intent) -> Vec<Effect> {
        let mut out = Vec::new();
        match intent {
            Intent::Connect { addr } => {
                self.view.phase = Phase::Connecting;
                out.push(Effect::OpenConnection { addr: addr.clone() });
                out.push(log(LogLevel::Info, "core: opening connection",
                    &[("addr", addr)]));
            }
            Intent::ConnectionOpened => {
                self.connection_open = true;
                if let Some(msg) = self.pending_handshake.take() {
                    self.view.phase = Phase::Authenticating;
                    out.push(log(LogLevel::Debug,
                        "core: connection opened, flushing pending handshake",
                        &[("msg", kind_of_client_message(&msg).into())]));
                    out.push(Effect::Send(msg));
                } else {
                    out.push(log(LogLevel::Debug,
                        "core: connection opened, awaiting handshake intent", &[]));
                }
            }
            Intent::Register { email, username, password, device_label } => {
                let msg = ClientMessage::Register {
                    protocol_version: PROTOCOL_VERSION,
                    email,
                    username,
                    password,
                    device_label,
                };
                self.queue_or_send_handshake(msg, &mut out);
            }
            Intent::AuthenticatePassword { identifier, password, device_label } => {
                let msg = ClientMessage::Authenticate {
                    protocol_version: PROTOCOL_VERSION,
                    mode: AuthMode::Password { identifier, password },
                    device_label,
                };
                self.queue_or_send_handshake(msg, &mut out);
            }
            Intent::AuthenticateSession { key, device_label } => {
                self.session_key = Some(key.clone());
                let msg = ClientMessage::Authenticate {
                    protocol_version: PROTOCOL_VERSION,
                    mode: AuthMode::Session { key },
                    device_label,
                };
                self.queue_or_send_handshake(msg, &mut out);
            }
            Intent::ListTables => {
                out.push(Effect::Send(ClientMessage::ListTables));
            }
            Intent::JoinTable { table_id, buy_in } => {
                out.push(Effect::Send(ClientMessage::JoinTable { table_id, buy_in }));
            }
            Intent::LeaveTable { table_id } => {
                out.push(Effect::Send(ClientMessage::LeaveTable { table_id }));
            }
            Intent::SubmitAction { action } => {
                self.handle_submit_action(action, &mut out);
            }
            Intent::Heartbeat => {
                out.push(Effect::Send(ClientMessage::Heartbeat));
            }
            Intent::Disconnect => {
                out.push(Effect::Send(ClientMessage::Disconnect));
                out.push(Effect::CloseConnection);
                self.reset_to_disconnected();
            }
            Intent::ConnectionLost { reason } => {
                self.reset_to_ended(reason.clone());
                out.push(log(LogLevel::Warn, "core: connection lost",
                    &[("reason", reason)]));
            }
        }
        out
    }

    /// If the connection isn't open yet, hold the message until
    /// `ConnectionOpened` flushes it. Otherwise send immediately.
    fn queue_or_send_handshake(&mut self, msg: ClientMessage, out: &mut Vec<Effect>) {
        if self.connection_open {
            self.view.phase = Phase::Authenticating;
            out.push(Effect::Send(msg));
        } else {
            self.pending_handshake = Some(msg);
            out.push(log(LogLevel::Debug,
                "core: connection not yet open, queued handshake", &[]));
        }
    }

    fn handle_submit_action(&mut self, action: Action, out: &mut Vec<Effect>) {
        // Two gates: (1) there must be an open prompt to act on,
        // (2) we must not already have submitted against it (the
        // optimistic clear flips `awaiting_action` to false the
        // moment we send). The `active_prompt` stays populated so
        // a subsequent `ActionRejected` can re-arm us.
        let still_awaiting = self
            .view
            .current_hand
            .as_ref()
            .is_some_and(|h| h.awaiting_action);
        if !still_awaiting {
            out.push(log(LogLevel::Warn,
                "core: SubmitAction with no live prompt; ignoring",
                &[("action", format!("{action:?}"))]));
            return;
        }
        let Some(prompt) = self.active_prompt.as_ref() else {
            out.push(log(LogLevel::Warn,
                "core: SubmitAction with no active prompt record; ignoring",
                &[("action", format!("{action:?}"))]));
            return;
        };
        if !prompt.legal.is_legal(action) {
            out.push(log(LogLevel::Warn,
                "core: SubmitAction failed local legality check; ignoring",
                &[("action", format!("{action:?}"))]));
            return;
        }
        let msg = ClientMessage::SubmitAction {
            table_id: prompt.table_id,
            hand_id: prompt.hand_id,
            action,
        };
        out.push(Effect::Send(msg));
        out.push(log(LogLevel::Info, "core: submitted action",
            &[("action", format!("{action:?}"))]));

        // Optimistic: flip out of "awaiting" so the UI hides action
        // buttons. We keep `active_prompt` around so a server-side
        // `ActionRejected` can re-arm us. The prompt is finally
        // cleared when a fresh `Prompt` supersedes it, the hand
        // ends, or we leave the table.
        if let Some(h) = self.view.current_hand.as_mut() {
            h.awaiting_action = false;
            h.legal_actions = None;
            h.action_deadline_ms = None;
        }
    }
}

// ─── Inbound handling ───────────────────────────────────────────────

impl ClientCore {
    pub fn handle_inbound(&mut self, msg: ServerMessage) -> Vec<Effect> {
        let mut out = Vec::new();
        match msg {
            ServerMessage::Welcome {
                player_id,
                username,
                session_key,
                stats,
                ..
            } => {
                self.view.phase = Phase::Lobby;
                self.view.player_id = Some(player_id);
                self.view.username = Some(username.clone());
                self.view.lifetime_stats = Some(stats);
                self.session_key = Some(session_key.clone());
                out.push(Effect::PersistSessionKey(Some(session_key)));
                out.push(log(LogLevel::Info, "core: welcomed",
                    &[("username", username), ("player_id", player_id.to_string())]));
            }
            ServerMessage::Rejected { reason, .. } => {
                self.view.phase = Phase::Ended { reason: reason.clone() };
                out.push(Effect::CloseConnection);
                out.push(log(LogLevel::Warn, "core: handshake rejected",
                    &[("reason", reason)]));
            }
            ServerMessage::Goodbye { reason } => {
                self.reset_to_ended(reason.clone());
                out.push(Effect::CloseConnection);
                out.push(log(LogLevel::Info, "core: goodbye",
                    &[("reason", reason)]));
            }
            ServerMessage::Heartbeat => {
                // No-op; surface for future jitter / RTT tracking.
            }
            ServerMessage::TableList { tables } => {
                let n = tables.len();
                self.view.tables = tables;
                out.push(log(LogLevel::Debug, "core: table list updated",
                    &[("count", n.to_string())]));
            }
            ServerMessage::JoinedTable { table_id, seat, seats } => {
                self.view.phase = Phase::Seated { table_id, seat };
                self.view.seats = seats;
                self.view.button = None; // Will be set by the next TableState.
                self.view.current_hand = None;
                self.active_prompt = None;
                self.view.last_action_rejection = None;
                out.push(log(LogLevel::Info, "core: seated",
                    &[("table_id", table_id.to_string()),
                      ("seat", seat.to_string())]));
            }
            ServerMessage::LeftTable { table_id } => {
                self.view.phase = Phase::Lobby;
                self.view.seats.clear();
                self.view.button = None;
                self.view.current_hand = None;
                self.active_prompt = None;
                out.push(log(LogLevel::Info, "core: left table",
                    &[("table_id", table_id.to_string())]));
            }
            ServerMessage::TableState { table_id, seats, button } => {
                if matches!(self.view.phase, Phase::Seated { table_id: t, .. } if t == table_id) {
                    self.view.seats = seats;
                    self.view.button = Some(button);
                    out.push(log(LogLevel::Debug, "core: table state updated",
                        &[("table_id", table_id.to_string()),
                          ("button", button.to_string())]));
                } else {
                    out.push(log(LogLevel::Warn,
                        "core: TableState for non-current table; ignoring",
                        &[("table_id", table_id.to_string())]));
                }
            }
            ServerMessage::TableEvent { table_id, event } => {
                self.project_engine_event(table_id, event, &mut out);
            }
            ServerMessage::Prompt {
                table_id,
                hand_id,
                seat,
                legal,
                deadline_ms,
            } => {
                self.active_prompt = Some(ActivePrompt {
                    table_id,
                    hand_id,
                    seat,
                    legal,
                });
                if let Some(h) = self.view.current_hand.as_mut() {
                    h.awaiting_action = true;
                    h.legal_actions = Some(legal);
                    h.action_deadline_ms = Some(deadline_ms);
                }
                self.view.last_action_rejection = None;
                out.push(log(LogLevel::Info, "core: prompt received",
                    &[("hand_id", hand_id.to_string()),
                      ("seat", seat.to_string()),
                      ("deadline_ms", deadline_ms.to_string())]));
            }
            ServerMessage::ActionRejected { reason } => {
                // Server bounced our last submission. Re-arm the
                // local prompt so the UI lets the user try again.
                if let Some(prompt) = self.active_prompt.as_ref() {
                    if let Some(h) = self.view.current_hand.as_mut() {
                        h.awaiting_action = true;
                        h.legal_actions = Some(prompt.legal);
                    }
                }
                self.view.last_action_rejection = Some(reason.clone());
                out.push(log(LogLevel::Warn, "core: action rejected",
                    &[("reason", reason)]));
            }
            ServerMessage::ReplayEvents { hand_id, events } => {
                // Mid-hand replay from the server (step 26). Feed each
                // contained message back through `handle_inbound` so
                // the state machine applies them identically to live
                // play. The machine is deterministic and idempotent,
                // so overlap with already-processed live events is
                // harmless — see `client-core` spec "No duplicate
                // processing".
                let n = events.len();
                out.push(log(LogLevel::Info, "core: replay begin",
                    &[("hand_id", hand_id.to_string()),
                      ("events", n.to_string())]));
                for event in events {
                    let nested = self.handle_inbound(event);
                    out.extend(nested);
                }
                out.push(log(LogLevel::Info, "core: replay end",
                    &[("hand_id", hand_id.to_string()),
                      ("events", n.to_string())]));
            }
        }
        out
    }
}

// ─── EngineEvent projection ─────────────────────────────────────────

impl ClientCore {
    fn project_engine_event(
        &mut self,
        table_id: TableId,
        event: EngineEvent,
        out: &mut Vec<Effect>,
    ) {
        // Defensive: events should only arrive for our current table.
        if !matches!(self.view.phase, Phase::Seated { table_id: t, .. } if t == table_id) {
            out.push(log(LogLevel::Warn,
                "core: TableEvent for non-current table; ignoring",
                &[("table_id", table_id.to_string())]));
            return;
        }
        match event {
            EngineEvent::HandStarted { hand_id, dealer, deck_seed } => {
                self.view.current_hand = Some(CurrentHand::started(hand_id, dealer));
                self.view.last_action_rejection = None;
                self.active_prompt = None;
                out.push(log(LogLevel::Info, "core: hand started",
                    &[("hand_id", hand_id.to_string()),
                      ("dealer", dealer.to_string()),
                      ("deck_seed", deck_seed.to_string())]));
            }
            EngineEvent::HoleCardsDealt { seat, cards } => {
                let our_seat = self.view.our_seat();
                if our_seat == Some(seat) {
                    if let Some(h) = self.view.current_hand.as_mut() {
                        h.hole_cards = Some([cards[0].index() as u8, cards[1].index() as u8]);
                    }
                    out.push(log(LogLevel::Debug, "core: hole cards dealt to us", &[]));
                } else {
                    out.push(log(LogLevel::Trace,
                        "core: HoleCardsDealt for another seat (unexpected; server should mask)",
                        &[("seat", seat.to_string())]));
                }
            }
            EngineEvent::BoardDealt { street, cards } => {
                if let Some(h) = self.view.current_hand.as_mut() {
                    h.street = street;
                    for c in &cards {
                        h.board.push(c.index() as u8);
                    }
                    // New street: reset per-seat bet projection.
                    h.bet_this_street.clear();
                    out.push(log(LogLevel::Debug, "core: board dealt",
                        &[("street", format!("{street:?}")),
                          ("cards", cards.len().to_string())]));
                }
            }
            EngineEvent::ActionTaken { seat, action, pot_total } => {
                if let Some(h) = self.view.current_hand.as_mut() {
                    h.pot_total = pot_total;
                    if matches!(action, Action::Fold) && !h.folded_seats.contains(&seat) {
                        h.folded_seats.push(seat);
                    }
                    // Update per-seat street bet projection (UI affordance only).
                    match action {
                        Action::Fold | Action::Check => {
                            // Fold: entry unchanged (spec says Fold keeps entry).
                            // Check: set to current max (or 0 if none).
                            if matches!(action, Action::Check) {
                                let max = h.bet_this_street.iter().map(|&(_, b)| b).max().unwrap_or(0);
                                update_bet(&mut h.bet_this_street, seat, max);
                            }
                        }
                        Action::Call => {
                            // Call: match current maximum bet.
                            let max = h.bet_this_street.iter().map(|&(_, b)| b).max().unwrap_or(0);
                            update_bet(&mut h.bet_this_street, seat, max);
                        }
                        Action::Raise(to) => {
                            // Raise(to): set exact amount.
                            update_bet(&mut h.bet_this_street, seat, to);
                        }
                        Action::AllIn => {
                            // AllIn: delegate to subsequent PlayerAllIn event.
                            // No update here; PlayerAllIn will set total_committed.
                        }
                    }
                }
                out.push(log(LogLevel::Debug, "core: action taken",
                    &[("seat", seat.to_string()),
                      ("action", format!("{action:?}")),
                      ("pot_total", pot_total.to_string())]));
            }
            EngineEvent::PlayerAllIn { seat, total_committed } => {
                if let Some(h) = self.view.current_hand.as_mut() {
                    if !h.all_in_seats.contains(&seat) {
                        h.all_in_seats.push(seat);
                    }
                    // Best-effort: set the seat's street bet to total_committed.
                    // This over-counts if prior streets contributed, but is
                    // acceptable as a UI affordance (see design §D4).
                    update_bet(&mut h.bet_this_street, seat, total_committed);
                }
                out.push(log(LogLevel::Debug, "core: player all-in",
                    &[("seat", seat.to_string()),
                      ("total_committed", total_committed.to_string())]));
            }
            EngineEvent::HandEnded { hand_id, result: _ } => {
                // Per CLIENT_PRINCIPLES.md §3, "current game state"
                // ends at HandEnded. The seat list (with refreshed
                // stacks) arrives via a follow-up TableState.
                if let Some(h) = self.view.current_hand.as_mut() {
                    if h.board.len() >= 5 {
                        h.street = Street::Showdown;
                    }
                }
                self.view.current_hand = None;
                self.active_prompt = None;
                out.push(log(LogLevel::Info, "core: hand ended",
                    &[("hand_id", hand_id.to_string())]));
            }
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────

impl ClientCore {
    fn reset_to_disconnected(&mut self) {
        self.view = ClientView::empty();
        self.connection_open = false;
        self.pending_handshake = None;
        self.active_prompt = None;
    }

    fn reset_to_ended(&mut self, reason: String) {
        self.view = ClientView::empty();
        self.view.phase = Phase::Ended { reason };
        self.connection_open = false;
        self.pending_handshake = None;
        self.active_prompt = None;
    }
}

fn log(level: LogLevel, msg: &str, fields: &[(&'static str, String)]) -> Effect {
    Effect::Log {
        level,
        message: msg.to_string(),
        fields: fields.to_vec(),
    }
}

/// Update or insert the per-seat bet entry in `bet_this_street`.
fn update_bet(bets: &mut Vec<(usize, u32)>, seat: usize, amount: u32) {
    if let Some(entry) = bets.iter_mut().find(|(s, _)| *s == seat) {
        entry.1 = amount;
    } else {
        bets.push((seat, amount));
    }
}

fn kind_of_client_message(msg: &ClientMessage) -> &'static str {
    match msg {
        ClientMessage::Register { .. } => "Register",
        ClientMessage::Authenticate { .. } => "Authenticate",
        ClientMessage::ListTables => "ListTables",
        ClientMessage::JoinTable { .. } => "JoinTable",
        ClientMessage::LeaveTable { .. } => "LeaveTable",
        ClientMessage::SubmitAction { .. } => "SubmitAction",
        ClientMessage::RequestReplay { .. } => "RequestReplay",
        ClientMessage::Heartbeat => "Heartbeat",
        ClientMessage::Disconnect => "Disconnect",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_engine::core::{Card, Rank, Suit};
    use poker_engine::game::{Action, EngineEvent, HandResult, LegalActions, SeatOutcome, Street};
    use poker_engine::net::protocol::{LifetimeStats, SeatInfo};

    fn welcome(core: &mut ClientCore) {
        core.handle_inbound(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: 1,
            username: "alice".into(),
            session_key: "abc".into(),
            stats: LifetimeStats::default(),
        });
    }

    fn seat_alice(core: &mut ClientCore) {
        welcome(core);
        core.handle_inbound(ServerMessage::JoinedTable {
            table_id: 1,
            seat: 0,
            seats: vec![
                SeatInfo { seat: 0, player_id: 1, username: "alice".into(), stack: 200 },
                SeatInfo { seat: 1, player_id: 2, username: "bob".into(), stack: 200 },
            ],
        });
    }

    fn matches_send(eff: &Effect) -> Option<&ClientMessage> {
        if let Effect::Send(m) = eff { Some(m) } else { None }
    }

    fn legal_check_only() -> LegalActions {
        LegalActions {
            can_check: true, can_call: false, call_amount: 0,
            can_raise: false, min_raise: 0, max_raise: 0,
            all_in_amount: 0,
        }
    }

    // ─── Connection lifecycle ───────────────────────────────────

    #[test]
    fn connect_intent_emits_open_connection_and_phase_advances() {
        let mut core = ClientCore::new(None);
        let effects = core.handle_intent(Intent::Connect {
            addr: "127.0.0.1:7878".into(),
        });
        assert_eq!(core.snapshot().phase, Phase::Connecting);
        assert!(effects.iter().any(|e| matches!(
            e, Effect::OpenConnection { addr } if addr == "127.0.0.1:7878"
        )));
    }

    #[test]
    fn handshake_before_connection_open_is_queued_and_flushed_later() {
        let mut core = ClientCore::new(None);
        core.handle_intent(Intent::Connect { addr: "x".into() });
        let effects = core.handle_intent(Intent::AuthenticateSession {
            key: "k".into(),
            device_label: None,
        });
        // Nothing sent yet — connection isn't open.
        assert!(effects.iter().filter_map(matches_send).next().is_none());
        assert_eq!(core.snapshot().phase, Phase::Connecting);

        let effects = core.handle_intent(Intent::ConnectionOpened);
        assert_eq!(core.snapshot().phase, Phase::Authenticating);
        let sent = effects.iter().filter_map(matches_send).next();
        assert!(matches!(sent, Some(ClientMessage::Authenticate { .. })));
    }

    #[test]
    fn handshake_after_connection_open_sends_immediately() {
        // Edge case the boolean `connection_open` flag was added to
        // fix: phase stays `Connecting` until the first handshake
        // message lands, so a Register issued *after* ConnectionOpened
        // has to look at connection-open state, not phase.
        let mut core = ClientCore::new(None);
        core.handle_intent(Intent::Connect { addr: "x".into() });
        core.handle_intent(Intent::ConnectionOpened); // no pending handshake
        assert_eq!(core.snapshot().phase, Phase::Connecting);

        let effects = core.handle_intent(Intent::Register {
            email: "e@x.com".into(),
            username: "alice".into(),
            password: "hunter2hunter".into(),
            device_label: None,
        });
        let sent = effects.iter().filter_map(matches_send).next();
        assert!(
            matches!(sent, Some(ClientMessage::Register { .. })),
            "Register issued after ConnectionOpened must send immediately"
        );
        assert_eq!(core.snapshot().phase, Phase::Authenticating);
    }

    #[test]
    fn welcome_lands_us_in_lobby_and_persists_session_key() {
        let mut core = ClientCore::new(None);
        core.handle_intent(Intent::Connect { addr: "x".into() });
        let effects = core.handle_inbound(ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: 42,
            username: "alice".into(),
            session_key: "abc".into(),
            stats: LifetimeStats::default(),
        });
        assert_eq!(core.snapshot().phase, Phase::Lobby);
        assert_eq!(core.session_key(), Some("abc"));
        assert!(effects.iter().any(|e| matches!(
            e, Effect::PersistSessionKey(Some(k)) if k == "abc"
        )));
    }

    #[test]
    fn rejected_lands_us_in_ended_and_closes() {
        let mut core = ClientCore::new(None);
        let effects = core.handle_inbound(ServerMessage::Rejected {
            protocol_version: PROTOCOL_VERSION,
            reason: "bad credentials".into(),
        });
        assert!(matches!(core.snapshot().phase, Phase::Ended { .. }));
        assert!(effects.iter().any(|e| matches!(e, Effect::CloseConnection)));
    }

    #[test]
    fn connection_lost_clears_view_and_marks_ended() {
        let mut core = ClientCore::new(None);
        welcome(&mut core);
        core.handle_intent(Intent::ConnectionLost { reason: "EOF".into() });
        let v = core.snapshot();
        assert!(matches!(v.phase, Phase::Ended { .. }));
        assert!(v.player_id.is_none(), "view must be cleared on connection loss");
    }

    // ─── Lobby state ────────────────────────────────────────────

    #[test]
    fn list_tables_emits_send() {
        let mut core = ClientCore::new(None);
        let effects = core.handle_intent(Intent::ListTables);
        let sent = effects.iter().filter_map(matches_send).next();
        assert!(matches!(sent, Some(ClientMessage::ListTables)));
    }

    #[test]
    fn table_list_lands_in_view() {
        use poker_engine::net::protocol::TableInfo;
        let mut core = ClientCore::new(None);
        welcome(&mut core);
        core.handle_inbound(ServerMessage::TableList {
            tables: vec![TableInfo {
                table_id: 1, name: "Main".into(),
                small_blind: 1, big_blind: 2, max_seats: 6,
                seated: 0, default_buy_in: 200,
            }],
        });
        let v = core.snapshot();
        assert_eq!(v.tables.len(), 1);
        assert_eq!(v.tables[0].name, "Main");
    }

    #[test]
    fn joined_table_advances_to_seated() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        let v = core.snapshot();
        assert!(matches!(v.phase, Phase::Seated { table_id: 1, seat: 0 }));
        assert_eq!(v.seats.len(), 2);
        assert_eq!(v.our_seat(), Some(0));
    }

    #[test]
    fn left_table_returns_to_lobby() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        core.handle_inbound(ServerMessage::LeftTable { table_id: 1 });
        let v = core.snapshot();
        assert_eq!(v.phase, Phase::Lobby);
        assert!(v.seats.is_empty());
    }

    #[test]
    fn table_state_updates_seats_and_button() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        core.handle_inbound(ServerMessage::TableState {
            table_id: 1,
            seats: vec![
                SeatInfo { seat: 0, player_id: 1, username: "alice".into(), stack: 195 },
                SeatInfo { seat: 1, player_id: 2, username: "bob".into(), stack: 205 },
            ],
            button: 1,
        });
        let v = core.snapshot();
        assert_eq!(v.seats[0].stack, 195);
        assert_eq!(v.button, Some(1));
    }

    // ─── In-hand projection ─────────────────────────────────────

    fn fire_event(core: &mut ClientCore, ev: EngineEvent) -> Vec<Effect> {
        core.handle_inbound(ServerMessage::TableEvent { table_id: 1, event: ev })
    }

    #[test]
    fn hand_started_initialises_current_hand() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 7, dealer: 1, deck_seed: 42,
        });
        let h = core.snapshot().current_hand.expect("current_hand should be set");
        assert_eq!(h.hand_id, 7);
        assert_eq!(h.dealer, 1);
        assert_eq!(h.street, Street::Preflop);
        assert!(h.hole_cards.is_none());
    }

    #[test]
    fn hole_cards_dealt_to_self_lands_in_view() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 1, dealer: 1, deck_seed: 0,
        });
        let cards = [
            Card::new(Rank::Ace, Suit::Spades),
            Card::new(Rank::King, Suit::Hearts),
        ];
        fire_event(&mut core, EngineEvent::HoleCardsDealt { seat: 0, cards });
        let h = core.snapshot().current_hand.unwrap();
        assert_eq!(h.hole_cards, Some([cards[0].index() as u8, cards[1].index() as u8]));
    }

    #[test]
    fn hole_cards_for_other_seat_are_ignored() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 1, dealer: 1, deck_seed: 0,
        });
        // Server normally masks these; defence-in-depth check that
        // we still don't store another seat's cards if one leaks.
        fire_event(&mut core, EngineEvent::HoleCardsDealt {
            seat: 1,
            cards: [Card::new(Rank::Two, Suit::Clubs), Card::new(Rank::Three, Suit::Clubs)],
        });
        assert!(core.snapshot().current_hand.unwrap().hole_cards.is_none());
    }

    #[test]
    fn board_dealt_appends_cards_and_advances_street() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 1, dealer: 1, deck_seed: 0,
        });
        let flop = vec![
            Card::new(Rank::Ace, Suit::Spades),
            Card::new(Rank::King, Suit::Hearts),
            Card::new(Rank::Queen, Suit::Diamonds),
        ];
        fire_event(&mut core, EngineEvent::BoardDealt {
            street: Street::Flop,
            cards: flop.clone(),
        });
        let h = core.snapshot().current_hand.unwrap();
        assert_eq!(h.street, Street::Flop);
        assert_eq!(h.board.len(), 3);
    }

    #[test]
    fn action_taken_updates_pot_and_records_folds() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 1, dealer: 1, deck_seed: 0,
        });
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 1, action: Action::Fold, pot_total: 30,
        });
        let h = core.snapshot().current_hand.unwrap();
        assert_eq!(h.pot_total, 30);
        assert_eq!(h.folded_seats, vec![1]);
    }

    #[test]
    fn hand_ended_clears_current_hand() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 1, dealer: 1, deck_seed: 0,
        });
        fire_event(&mut core, EngineEvent::HandEnded {
            hand_id: 1,
            result: HandResult {
                hand_id: 1, board: Vec::new(),
                seats: vec![SeatOutcome {
                    seat: 0, hole_cards: None, chip_delta: -2, sat_out: false,
                }],
            },
        });
        assert!(core.snapshot().current_hand.is_none());
    }

    // ─── Prompt + SubmitAction ──────────────────────────────────

    #[test]
    fn prompt_marks_us_awaiting_with_legal_actions() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 7, dealer: 1, deck_seed: 0,
        });
        core.handle_inbound(ServerMessage::Prompt {
            table_id: 1, hand_id: 7, seat: 0,
            legal: legal_check_only(), deadline_ms: 5000,
        });
        let h = core.snapshot().current_hand.unwrap();
        assert!(h.awaiting_action);
        assert!(h.legal_actions.unwrap().can_check);
        assert_eq!(h.action_deadline_ms, Some(5000));
    }

    #[test]
    fn submit_action_with_active_prompt_emits_send_and_clears_awaiting() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 7, dealer: 1, deck_seed: 0,
        });
        core.handle_inbound(ServerMessage::Prompt {
            table_id: 1, hand_id: 7, seat: 0,
            legal: legal_check_only(), deadline_ms: 5000,
        });
        let effects = core.handle_intent(Intent::SubmitAction { action: Action::Check });
        let sent = effects.iter().filter_map(matches_send).next();
        assert!(matches!(
            sent,
            Some(ClientMessage::SubmitAction { table_id: 1, hand_id: 7, action: Action::Check })
        ));
        // Optimistic clear: UI should flip out of "awaiting".
        assert!(!core.snapshot().current_hand.unwrap().awaiting_action);
    }

    #[test]
    fn submit_action_with_no_active_prompt_is_a_noop() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        let effects = core.handle_intent(Intent::SubmitAction { action: Action::Fold });
        assert!(effects.iter().filter_map(matches_send).next().is_none(),
            "no Send should fire when no prompt is active");
    }

    #[test]
    fn submit_action_failing_legality_is_a_noop() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 7, dealer: 1, deck_seed: 0,
        });
        // Legal: only Check. Try to Raise — must be rejected client-side.
        core.handle_inbound(ServerMessage::Prompt {
            table_id: 1, hand_id: 7, seat: 0,
            legal: legal_check_only(), deadline_ms: 5000,
        });
        let effects = core.handle_intent(Intent::SubmitAction {
            action: Action::Raise(50),
        });
        assert!(effects.iter().filter_map(matches_send).next().is_none(),
            "illegal action must not reach the wire");
        assert!(core.snapshot().current_hand.unwrap().awaiting_action,
            "prompt should remain open");
    }

    #[test]
    fn action_rejected_re_arms_awaiting_and_records_reason() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        fire_event(&mut core, EngineEvent::HandStarted {
            hand_id: 7, dealer: 1, deck_seed: 0,
        });
        core.handle_inbound(ServerMessage::Prompt {
            table_id: 1, hand_id: 7, seat: 0,
            legal: legal_check_only(), deadline_ms: 5000,
        });
        core.handle_intent(Intent::SubmitAction { action: Action::Check });
        // Optimistic clear has fired; now the server bounces us.
        core.handle_inbound(ServerMessage::ActionRejected {
            reason: "stale prompt".into(),
        });
        let v = core.snapshot();
        assert!(v.current_hand.unwrap().awaiting_action,
            "should re-arm so the user can try again");
        assert_eq!(v.last_action_rejection.as_deref(), Some("stale prompt"));
    }

    // ─── bet_this_street projection ─────────────────────────────

    fn start_hand(core: &mut ClientCore) {
        fire_event(core, EngineEvent::HandStarted { hand_id: 1, dealer: 0, deck_seed: 0 });
    }

    fn bet_entry(h: &crate::view::CurrentHand, seat: usize) -> Option<u32> {
        h.bet_this_street.iter().find(|&&(s, _)| s == seat).map(|&(_, b)| b)
    }

    /// Scenario: Reset on new hand.
    #[test]
    fn bet_this_street_resets_on_new_hand() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        start_hand(&mut core);
        // Populate some bets from a first hand.
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 0, action: Action::Raise(10), pot_total: 10,
        });
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 1, action: Action::Call, pot_total: 20,
        });
        // Verify bets were set.
        assert!(core.snapshot().current_hand.as_ref().unwrap().bet_this_street.len() >= 1);

        // Start a new hand — bet_this_street must be empty.
        fire_event(&mut core, EngineEvent::HandEnded {
            hand_id: 1,
            result: HandResult { hand_id: 1, board: vec![], seats: vec![] },
        });
        start_hand(&mut core);
        let h = core.snapshot().current_hand.unwrap();
        assert!(h.bet_this_street.is_empty(), "bet_this_street must reset on HandStarted");
    }

    /// Scenario: Reset on new street.
    #[test]
    fn bet_this_street_resets_on_new_street() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        start_hand(&mut core);
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 0, action: Action::Raise(10), pot_total: 10,
        });
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 1, action: Action::Call, pot_total: 20,
        });
        // New street.
        fire_event(&mut core, EngineEvent::BoardDealt {
            street: Street::Flop,
            cards: vec![],
        });
        let h = core.snapshot().current_hand.unwrap();
        assert!(h.bet_this_street.is_empty(), "bet_this_street must clear on BoardDealt");
    }

    /// Scenario: Raise sets exact amount.
    #[test]
    fn bet_this_street_raise_sets_exact() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        start_hand(&mut core);
        // Seat 0 raises to 30, seat 1 had 10 from blind.
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 1, action: Action::Raise(10), pot_total: 10,
        });
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 0, action: Action::Raise(30), pot_total: 40,
        });
        let h = core.snapshot().current_hand.unwrap();
        assert_eq!(bet_entry(&h, 0), Some(30), "seat 0 bet should be 30 after Raise(30)");
        assert_eq!(bet_entry(&h, 1), Some(10), "seat 1 bet should remain 10");
    }

    /// Scenario: Call matches current max.
    #[test]
    fn bet_this_street_call_matches_max() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        start_hand(&mut core);
        // Seat 0 raises to 30; seat 1 calls → should match 30.
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 0, action: Action::Raise(30), pot_total: 30,
        });
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 1, action: Action::Call, pot_total: 60,
        });
        let h = core.snapshot().current_hand.unwrap();
        assert_eq!(bet_entry(&h, 1), Some(30), "seat 1 Call should match max (30)");
    }

    /// Scenario: Fold does not remove entry.
    #[test]
    fn bet_this_street_fold_keeps_entry() {
        let mut core = ClientCore::new(None);
        seat_alice(&mut core);
        start_hand(&mut core);
        // Seat 1 has bet 10; seat 1 folds — entry must remain.
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 1, action: Action::Raise(10), pot_total: 10,
        });
        fire_event(&mut core, EngineEvent::ActionTaken {
            seat: 1, action: Action::Fold, pot_total: 10,
        });
        let h = core.snapshot().current_hand.unwrap();
        assert_eq!(bet_entry(&h, 1), Some(10), "Fold must not remove bet_this_street entry");
        assert!(h.folded_seats.contains(&1), "seat 1 should appear in folded_seats");
    }
}
