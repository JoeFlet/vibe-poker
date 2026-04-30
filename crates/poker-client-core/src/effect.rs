//! Side-effects the core wants the host to perform. The host pumps
//! these in order, and any results that flow back become either
//! [`super::Intent`]s (e.g. `ConnectionLost`) or
//! `ClientCore::handle_inbound(...)` calls.

use poker_engine::net::protocol::ClientMessage;

/// What the core asks the host to do.
#[derive(Debug, Clone)]
pub enum Effect {
    /// Open a connection to `addr`. The host should then pump the
    /// connection: every inbound `ServerMessage` becomes a
    /// `handle_inbound` call.
    OpenConnection { addr: String },

    /// Close the connection. Idempotent.
    CloseConnection,

    /// Serialise and send `msg` over the open connection.
    Send(ClientMessage),

    /// Persist (or, when `None`, clear) the session key. The host
    /// chooses the storage backend (filesystem under
    /// `poker-client-transport-native`, IndexedDB in a browser
    /// transport, etc.).
    PersistSessionKey(Option<String>),

    /// Schedule a future tick. The host calls `handle_intent` with
    /// the matching tick intent after `after_ms` elapses. Used for
    /// heartbeats and the action-deadline countdown.
    Schedule {
        kind: ScheduleKind,
        after_ms: u32,
    },

    /// Structured log line. Hosts route these to `tracing` (native)
    /// or `console` (web). Per principle 5 (verbose, structured
    /// logging) every meaningful state transition emits one.
    Log {
        level: LogLevel,
        message: String,
        fields: Vec<(&'static str, String)>,
    },
}

/// Tick categories. Kept open-ended on purpose so future timing
/// behaviour (e.g. animation budget enforcement) can land here
/// without churn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleKind {
    Heartbeat,
    PromptDeadline,
}

/// `tracing`-style level so the host can route appropriately
/// without importing `tracing` itself into the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}
