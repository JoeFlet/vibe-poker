//! Wire-protocol message types for the live poker server.
//!
//! Kept in `poker-engine` so both ends — the server and an eventual
//! live client — link against one canonical definition. Transport is
//! deliberately abstract: messages are serialized as msgpack and framed
//! by [`super::frame`]; whether they ride TCP, WebSockets, or an
//! in-process channel is the caller's choice.
//!
//! For now the protocol covers only the **handshake layer** — enough
//! for a client to identify itself with a username, receive a stable
//! `PlayerId`, and let the server attach prior lifetime statistics.
//! Game-bearing messages (table list, sit/leave, action prompts, event
//! broadcasts) will land in a follow-up step.

use serde::{Deserialize, Serialize};

use crate::game::{Action, EngineEvent, HandId, LegalActions, SeatIndex};

/// Bumped on any backwards-incompatible change to message shapes.
/// The server sends its version in `Welcome` / `Rejected`; clients
/// SHOULD refuse to proceed against a mismatched major version.
pub const PROTOCOL_VERSION: u32 = 2;

/// Stable identifier for a poker table. Allocated by the server.
pub type TableId = u32;

/// Public summary of a table for the lobby view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableInfo {
    pub table_id: TableId,
    pub name: String,
    pub small_blind: u32,
    pub big_blind: u32,
    pub max_seats: u8,
    pub seated: u8,
    pub default_buy_in: u32,
}

/// One seat's public state in a `TableSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatInfo {
    pub seat: SeatIndex,
    pub player_id: PlayerId,
    pub username: String,
    pub stack: u32,
}

/// Stable identifier for a registered player. Allocated by the server
/// the first time a username is seen and persisted in its registry, so
/// reconnects pick up the same id (and thus the same stat history).
pub type PlayerId = u64;

/// Long-running aggregate stats kept per player across sessions.
///
/// Mirrors a subset of the fields produced by [`crate::dataset`] so the
/// server can hand a returning client a quick "you've played N hands,
/// here's your VPIP" without having to ship the full event log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LifetimeStats {
    pub hands: u64,
    pub voluntary_pf: u64,
    pub raised_pf: u64,
    pub aggressive_actions: u64,
    pub passive_actions: u64,
    pub showdowns: u64,
    pub chip_delta: i64,
}

/// Messages a client sends to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// First message on a fresh connection. The server replies with
    /// either [`ServerMessage::Welcome`] or [`ServerMessage::Rejected`].
    Hello {
        protocol_version: u32,
        username: String,
    },

    /// Ask the server for the current table list. Server replies with
    /// [`ServerMessage::TableList`].
    ListTables,

    /// Take an open seat at `table_id`. The server replies with
    /// [`ServerMessage::JoinedTable`] on success or
    /// [`ServerMessage::ActionRejected`] otherwise. The seated player
    /// is added to the table broadcast set immediately.
    JoinTable { table_id: TableId, buy_in: u32 },

    /// Stand up from a table the player currently occupies. Effective
    /// at the next hand boundary; the current hand (if any) plays out.
    LeaveTable { table_id: TableId },

    /// Response to a [`ServerMessage::Prompt`]. `hand_id` echoes the
    /// prompt so a stale or duplicate response is dropped.
    SubmitAction {
        table_id: TableId,
        hand_id: HandId,
        action: Action,
    },

    /// Optional keepalive. The server echoes a `Heartbeat` back.
    Heartbeat,
    /// Graceful close. The server may flush stats and reply with
    /// [`ServerMessage::Goodbye`] before dropping the connection.
    Disconnect,
}

/// Messages the server sends to a client.
///
/// Not `PartialEq` — `TableEvent` carries the full `EngineEvent`
/// which transitively contains floats and other non-eq leaves. Use
/// pattern matching for assertions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Handshake accepted. Carries the player's stable id and any
    /// stats accumulated in prior sessions.
    Welcome {
        protocol_version: u32,
        player_id: PlayerId,
        username: String,
        stats: LifetimeStats,
    },
    /// Handshake refused. `reason` is a short, human-readable string.
    Rejected {
        protocol_version: u32,
        reason: String,
    },
    /// Snapshot of the lobby. Sent in response to `ListTables`.
    TableList { tables: Vec<TableInfo> },

    /// Confirms a successful sit. The full seat layout follows in a
    /// subsequent `TableState` so the client can render before the
    /// next hand starts.
    JoinedTable {
        table_id: TableId,
        seat: SeatIndex,
        seats: Vec<SeatInfo>,
    },

    /// Confirms a stand-up.
    LeftTable { table_id: TableId },

    /// One engine event from a table the player is sat at or
    /// observing. The same `EngineEvent` shape `FileSink` writes to
    /// disk, so a server can spool broadcasts straight into a log.
    TableEvent { table_id: TableId, event: EngineEvent },

    /// Periodic seat snapshot — sent on join, after each hand, and
    /// whenever the seat layout changes outside of a hand.
    TableState {
        table_id: TableId,
        seats: Vec<SeatInfo>,
        button: SeatIndex,
    },

    /// Server is asking the client for an action. The matching reply
    /// is `SubmitAction { hand_id, action }`. If `deadline_ms` elapses
    /// before a reply arrives the server folds the seat for the player.
    Prompt {
        table_id: TableId,
        hand_id: HandId,
        seat: SeatIndex,
        legal: LegalActions,
        deadline_ms: u32,
    },

    /// Sent in response to a request the server could not satisfy
    /// (sit failed, action illegal, table missing, ...). Always
    /// non-fatal; the connection stays open.
    ActionRejected { reason: String },

    /// Echo of a client heartbeat.
    Heartbeat,
    /// Server is closing this connection cleanly.
    Goodbye {
        reason: String,
    },
}

/// Reasons the server may reject a `Hello`. Stringified into
/// `ServerMessage::Rejected.reason` so the wire format stays simple,
/// but kept as an enum here for callers that want to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    ProtocolMismatch,
    InvalidUsername,
    AlreadyConnected,
}

impl RejectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolMismatch => "protocol version mismatch",
            Self::InvalidUsername => "invalid username",
            Self::AlreadyConnected => "username already connected",
        }
    }
}

/// Server-side username validation. Kept here so client-side UIs can
/// pre-validate without a round-trip and stay in lockstep with the
/// server's rules.
///
/// Rules: 3..=24 chars, ASCII letters/digits/`_`/`-`/`.`, must start
/// with a letter or digit.
pub fn is_valid_username(name: &str) -> bool {
    let bytes = name.as_bytes();
    if !(3..=24).contains(&bytes.len()) {
        return false;
    }
    let first = bytes[0];
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    bytes.iter().all(|&c| {
        c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.'
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_validation() {
        assert!(is_valid_username("alice"));
        assert!(is_valid_username("bob_42"));
        assert!(is_valid_username("a-b.c"));
        assert!(!is_valid_username("ab"));               // too short
        assert!(!is_valid_username(""));                 // empty
        assert!(!is_valid_username("_alice"));           // leading underscore
        assert!(!is_valid_username("alice!"));           // illegal char
        assert!(!is_valid_username(&"x".repeat(25)));    // too long
    }

    #[test]
    fn message_roundtrip() {
        let hello = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            username: "alice".into(),
        };
        let bytes = rmp_serde::to_vec(&hello).unwrap();
        let back: ClientMessage = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(hello, back);

        let welcome = ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: 42,
            username: "alice".into(),
            stats: LifetimeStats { hands: 17, ..Default::default() },
        };
        let bytes = rmp_serde::to_vec(&welcome).unwrap();
        let back: ServerMessage = rmp_serde::from_slice(&bytes).unwrap();
        match back {
            ServerMessage::Welcome { player_id, username, stats, .. } => {
                assert_eq!(player_id, 42);
                assert_eq!(username, "alice");
                assert_eq!(stats.hands, 17);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
    }
}
