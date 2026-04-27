//! Live poker server.
//!
//! Step 19b extends the handshake-only server with table state, a
//! per-table game loop, and the wire-side `Prompt` / `SubmitAction`
//! exchange that lets the synchronous engine drive a remote player.

pub mod connection;
pub mod registry;
pub mod remote_agent;
pub mod session;
pub mod table;
pub mod wire;

pub use connection::Connection;
pub use registry::{PlayerRecord, Registry, RegistryError};
pub use session::{handle_connection, run_session, ServerContext, SessionError};
pub use table::{run_table, shutdown_all, Table, TableConfig, TableManager};
pub use wire::{read_message, write_message, WireError};
