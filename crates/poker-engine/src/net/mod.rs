//! Wire protocol shared by `poker-server` and live clients.
//!
//! `protocol` defines the message types; `frame` defines the
//! `[u32 LE length][msgpack bytes]` codec — the same shape as `FileSink`,
//! so a server can spool its event broadcasts into a log without
//! re-encoding.
//!
//! I/O is intentionally absent from this module: callers wire `frame::encode`
//! / `frame::decode` to whatever transport (tokio, std, sync, async) they
//! prefer, keeping `poker-engine` free of network dependencies.

pub mod frame;
pub mod protocol;

pub use protocol::{
    ClientMessage, LifetimeStats, PlayerId, SeatInfo, ServerMessage, TableId, TableInfo,
    PROTOCOL_VERSION,
};
