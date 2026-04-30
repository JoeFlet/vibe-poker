//! Wire-protocol message types for the live poker server.
//!
//! Kept in `poker-engine` so both ends — the server and an eventual
//! live client — link against one canonical definition. Transport is
//! deliberately abstract: messages are serialized as msgpack and framed
//! by [`super::frame`]; whether they ride TCP, WebSockets, or an
//! in-process channel is the caller's choice.
//!
//! Handshake (PROTOCOL_VERSION 3): the first message on a fresh
//! connection is either [`ClientMessage::Register`] (account creation)
//! or [`ClientMessage::Authenticate`] (existing account, by password
//! or session key). Both succeed with [`ServerMessage::Welcome`]
//! carrying a `session_key` that survives reconnects until either the
//! user authenticates from a fresh device (which revokes prior
//! sessions) or the server explicitly revokes it.
//!
//! Email is server-side state only; broadcasts (`SeatInfo`, etc.) ship
//! username only.

use serde::{Deserialize, Serialize};

use crate::game::{Action, EngineEvent, HandId, LegalActions, SeatIndex};

/// Bumped on any backwards-incompatible change to message shapes.
/// The server sends its version in `Welcome` / `Rejected`; clients
/// SHOULD refuse to proceed against a mismatched major version.
pub const PROTOCOL_VERSION: u32 = 3;

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

/// One half of [`ClientMessage::Authenticate`]: how the client is
/// proving its identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AuthMode {
    /// Email or username + plaintext password. The server hashes with
    /// Argon2id and compares against `user_password.password_hash`.
    Password { identifier: String, password: String },
    /// Reuse a `session_key` previously issued by a `Welcome`. Valid
    /// until the user authenticates from another device or the server
    /// revokes it.
    Session { key: String },
}

/// Messages a client sends to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Create a new account. Email and username must both be unused.
    /// On success the server replies with [`ServerMessage::Welcome`]
    /// carrying a fresh `session_key`. On failure
    /// [`ServerMessage::Rejected`].
    Register {
        protocol_version: u32,
        email: String,
        username: String,
        password: String,
        device_label: Option<String>,
    },

    /// Authenticate an existing account by password or by a previously
    /// issued session key. On success the server replies with
    /// [`ServerMessage::Welcome`]. Issuing a new password-mode session
    /// revokes any prior session for the same user.
    Authenticate {
        protocol_version: u32,
        mode: AuthMode,
        device_label: Option<String>,
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
    /// Handshake accepted. Carries the player's stable id, the session
    /// key the client should retain for subsequent
    /// [`AuthMode::Session`] reconnects, and any stats accumulated in
    /// prior sessions.
    Welcome {
        protocol_version: u32,
        player_id: PlayerId,
        username: String,
        session_key: String,
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

/// Reasons the server may reject `Register` / `Authenticate`.
/// Stringified into `ServerMessage::Rejected.reason` so the wire
/// format stays simple, but kept as an enum here for callers that
/// want to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    ProtocolMismatch,
    InvalidUsername,
    InvalidEmail,
    InvalidPassword,
    UsernameInUse,
    EmailInUse,
    BadCredentials,
    SessionRevoked,
    UnknownSession,
    InternalError,
}

impl RejectReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProtocolMismatch => "protocol version mismatch",
            Self::InvalidUsername => "invalid username",
            Self::InvalidEmail => "invalid email",
            Self::InvalidPassword => "invalid password",
            Self::UsernameInUse => "username already in use",
            Self::EmailInUse => "email already in use",
            Self::BadCredentials => "bad credentials",
            Self::SessionRevoked => "session revoked",
            Self::UnknownSession => "unknown session",
            Self::InternalError => "internal error",
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

/// Lightweight email shape check — non-empty local part, a single `@`,
/// a domain with at least one `.`. We deliberately don't pretend to
/// implement RFC-5321; this catches typos without forbidding any sane
/// address. Length cap matches the SMTP envelope limit (254).
pub fn is_valid_email(email: &str) -> bool {
    if !(3..=254).contains(&email.len()) {
        return false;
    }
    let Some(at) = email.find('@') else { return false };
    if email.matches('@').count() != 1 {
        return false;
    }
    let (local, domain_part) = email.split_at(at);
    let domain = &domain_part[1..];
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    if !domain.contains('.') {
        return false;
    }
    !email
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
}

/// Password length window. The server hashes the password with Argon2id
/// before storage, but enforces a minimum length here so trivially weak
/// passwords are caught at the boundary and clients can pre-check.
pub fn is_valid_password(password: &str) -> bool {
    let len = password.len();
    (8..=128).contains(&len)
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
    fn email_validation() {
        assert!(is_valid_email("alice@example.com"));
        assert!(is_valid_email("a@b.co"));
        assert!(!is_valid_email("plain-text"));            // no @
        assert!(!is_valid_email("a@b"));                   // no dot in domain
        assert!(!is_valid_email("@example.com"));          // empty local
        assert!(!is_valid_email("alice@"));                // empty domain
        assert!(!is_valid_email("a@@b.co"));               // two ats
        assert!(!is_valid_email("a b@example.com"));       // whitespace
    }

    #[test]
    fn password_validation() {
        assert!(is_valid_password("hunter2hunter"));
        assert!(!is_valid_password("short"));              // < 8
        assert!(!is_valid_password(&"x".repeat(129)));     // > 128
    }

    /// Wire-format invariants the long-form `PROTOCOL.md` relies on.
    /// If anyone ever switches `rmp_serde::to_vec` for the named-map
    /// variant (`to_vec_named`) this test fails loudly — silently
    /// flipping the format would break every client built against the
    /// spec.
    #[test]
    fn structs_serialize_as_positional_arrays_not_maps() {
        // Welcome with 5 fields → outer 1-entry map (variant tag) +
        // inner fixarray of 5. Critically: no field-name strings on
        // the wire (which is what the named-map variant would emit).
        let bytes = rmp_serde::to_vec(&ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: 1,
            username: "alice".into(),
            session_key: "k".into(),
            stats: LifetimeStats::default(),
        })
        .unwrap();
        // 0x81 = fixmap with 1 entry (the {"Welcome": …} envelope)
        // 0xa7 0x57 0x65 ... = fixstr len 7 "Welcome"
        // 0x95 = fixarray with 5 elements (the struct fields)
        let expected_prefix = [
            0x81, 0xa7, b'W', b'e', b'l', b'c', b'o', b'm', b'e', 0x95,
        ];
        assert_eq!(
            &bytes[..expected_prefix.len()],
            &expected_prefix,
            "Welcome must serialize as {{\"Welcome\": [<5-array>]}} \
             (positional fields, no field names on the wire)",
        );
        // Sanity: no field name from the struct should appear as a
        // bare string in the payload — that would mean someone
        // switched to `to_vec_named`.
        let payload = String::from_utf8_lossy(&bytes);
        for forbidden in ["protocol_version", "player_id", "session_key"] {
            assert!(
                !payload.contains(forbidden),
                "field name {forbidden:?} leaked into wire bytes — \
                 has someone switched the encoder to named maps?",
            );
        }
    }

    /// Single-field tuple variants (`Action::Raise(u32)`) are encoded
    /// as `{"Raise": <u32>}` — the inner value is bare, not wrapped
    /// in a 1-element array. PROTOCOL.md documents this shape.
    #[test]
    fn newtype_variant_inner_is_not_array_wrapped() {
        use crate::game::Action;
        let bytes = rmp_serde::to_vec(&Action::Raise(50)).unwrap();
        // 0x81 fixmap-1, 0xa5 "Raise", 0x32 positive fixint 50.
        // If the inner had been wrapped in [50] we'd see 0x91 (fixarray-1)
        // before the 0x32.
        assert_eq!(
            bytes,
            [0x81, 0xa5, b'R', b'a', b'i', b's', b'e', 0x32],
            "Raise(50) must be {{\"Raise\": 50}}, not {{\"Raise\": [50]}}",
        );
    }

    #[test]
    fn message_roundtrip() {
        let auth = ClientMessage::Authenticate {
            protocol_version: PROTOCOL_VERSION,
            mode: AuthMode::Password {
                identifier: "alice".into(),
                password: "hunter2hunter".into(),
            },
            device_label: Some("test".into()),
        };
        let bytes = rmp_serde::to_vec(&auth).unwrap();
        let back: ClientMessage = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(auth, back);

        let welcome = ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: 42,
            username: "alice".into(),
            session_key: "abc123".into(),
            stats: LifetimeStats { hands: 17, ..Default::default() },
        };
        let bytes = rmp_serde::to_vec(&welcome).unwrap();
        let back: ServerMessage = rmp_serde::from_slice(&bytes).unwrap();
        match back {
            ServerMessage::Welcome { player_id, username, session_key, stats, .. } => {
                assert_eq!(player_id, 42);
                assert_eq!(username, "alice");
                assert_eq!(session_key, "abc123");
                assert_eq!(stats.hands, 17);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
    }
}
