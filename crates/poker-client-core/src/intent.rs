//! Things the host can ask the core to do. Intents are user actions
//! and host events — never wire messages, which arrive via
//! [`super::ClientCore::handle_inbound`].

use poker_engine::game::Action;
use poker_engine::net::protocol::TableId;
use serde::{Deserialize, Serialize};

/// The top-level command the host issues against the core.
///
/// `Serialize` / `Deserialize` are derived so scripted test
/// scenarios (see `poker-client-headless`) can round-trip through
/// JSON / msgpack files. The on-disk format follows serde's default
/// representation for enums; no stability guarantees are made — if
/// you pin a scenario file, re-record it whenever this enum grows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Intent {
    /// Open a TCP / WebSocket connection to `addr`. The core will
    /// emit an [`super::Effect::OpenConnection`] in response so the
    /// transport actually performs the I/O. The core's phase moves
    /// to `Connecting`.
    Connect { addr: String },

    /// Notification from the transport that the connection just
    /// finished opening. Any handshake intent (`Register` /
    /// `AuthenticatePassword` / `AuthenticateSession`) issued
    /// while the core was in `Connecting` is queued and only
    /// flushed on this intent.
    ConnectionOpened,

    /// First-time account creation. Triggers a `ClientMessage::Register`
    /// once the connection is open.
    Register {
        email: String,
        username: String,
        password: String,
        device_label: Option<String>,
    },

    /// Existing-account login by username/email + password.
    AuthenticatePassword {
        identifier: String,
        password: String,
        device_label: Option<String>,
    },

    /// Re-authenticate using a previously persisted session key.
    /// Typically the first intent issued on app launch.
    AuthenticateSession {
        key: String,
        device_label: Option<String>,
    },

    /// Ask the server for the current lobby snapshot.
    ListTables,

    /// Sit down at a table.
    JoinTable { table_id: TableId, buy_in: u32 },

    /// Stand up.
    LeaveTable { table_id: TableId },

    /// Respond to a `Prompt` with an action. The core remembers which
    /// hand the active prompt belongs to, so the host doesn't need
    /// to track `hand_id`.
    SubmitAction { action: Action },

    /// Optional keepalive prod (the transport may also schedule its own).
    Heartbeat,

    /// Close the connection cleanly. Triggers a `ClientMessage::Disconnect`.
    Disconnect,

    /// Notification from the transport that the connection went away
    /// without a clean `Goodbye`. Drops the core back to `Disconnected`
    /// and clears any in-flight prompt.
    ConnectionLost { reason: String },
}
