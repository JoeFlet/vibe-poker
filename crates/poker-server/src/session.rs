//! Per-connection lifecycle.
//!
//! After the [`ClientMessage::Hello`] / [`ServerMessage::Welcome`]
//! handshake the connection splits into:
//!
//!   - a **writer task** draining a per-connection `mpsc::Receiver`
//!     into the socket;
//!   - a **reader loop** (this task) parsing client messages and
//!     dispatching them to the [`Registry`] / [`TableManager`].
//!
//! Both halves share an `Arc<Connection>` carrying the player id,
//! username, outbound queue handle, and pending-action slot. Game
//! events from a [`crate::table::Table`] reach the client by being
//! pushed into that same outbound queue from a `BroadcastSink`
//! running on tokio's blocking pool.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use poker_engine::net::protocol::{
    ClientMessage, RejectReason, ServerMessage, TableId, PROTOCOL_VERSION,
};

use crate::connection::{writer_task, Connection};
use crate::registry::{Registry, RegistryError};
use crate::table::TableManager;
use crate::wire::{read_message, write_message, WireError};

/// Bundle of long-lived shared state passed to every session.
#[derive(Clone)]
pub struct ServerContext {
    pub registry: Arc<Registry>,
    pub tables: TableManager,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("wire error: {0}")]
    Wire(#[from] WireError),
    #[error("registry error: {0}")]
    Registry(#[from] RegistryError),
    #[error("client sent {got} before handshake")]
    OutOfOrder { got: &'static str },
    #[error("client disconnected")]
    Disconnected,
}

/// Drive one fully-accepted TCP socket through its session lifecycle.
pub async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    ctx: ServerContext,
) {
    if let Err(e) = stream.set_nodelay(true) {
        warn!(%peer, error=%e, "set_nodelay failed");
    }
    let (read, write) = stream.into_split();
    if let Err(e) = run_session(read, write, peer, ctx).await {
        match e {
            SessionError::Disconnected => debug!(%peer, "client disconnected"),
            other => info!(%peer, error=%other, "session ended"),
        }
    }
}

/// Generic over the reader/writer halves so tests can drive the same
/// state machine over an in-memory duplex pair.
pub async fn run_session<R, W>(
    mut read: R,
    mut write: W,
    peer: SocketAddr,
    ctx: ServerContext,
) -> Result<(), SessionError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // ---- handshake (synchronous on this task) ----
    let hello: ClientMessage = match read_message(&mut read).await {
        Ok(msg) => msg,
        Err(WireError::Closed) => return Err(SessionError::Disconnected),
        Err(other) => return Err(other.into()),
    };

    let username = match hello {
        ClientMessage::Hello { protocol_version, username } => {
            if protocol_version != PROTOCOL_VERSION {
                let reject = ServerMessage::Rejected {
                    protocol_version: PROTOCOL_VERSION,
                    reason: RejectReason::ProtocolMismatch.as_str().into(),
                };
                write_message(&mut write, &reject).await?;
                return Ok(());
            }
            username
        }
        ClientMessage::Heartbeat => return Err(SessionError::OutOfOrder { got: "Heartbeat" }),
        ClientMessage::Disconnect => return Err(SessionError::Disconnected),
        _ => return Err(SessionError::OutOfOrder { got: "non-Hello" }),
    };

    let record = match ctx.registry.login(&username).await {
        Ok(rec) => rec,
        Err(e) => {
            let reason = match e {
                RegistryError::InvalidUsername => RejectReason::InvalidUsername.as_str(),
                RegistryError::AlreadyConnected => RejectReason::AlreadyConnected.as_str(),
                _ => "internal error",
            };
            let reject = ServerMessage::Rejected {
                protocol_version: PROTOCOL_VERSION,
                reason: reason.into(),
            };
            write_message(&mut write, &reject).await?;
            return Err(e.into());
        }
    };
    let player_id = record.player_id;
    info!(%peer, player_id, username = %record.username, "client logged in");

    // ---- per-connection plumbing ----
    let (conn_data, out_rx) = Connection::new(player_id, record.username.clone());
    let conn = Arc::new(conn_data);

    // Welcome rides the outbound queue too — fast-path the first send
    // through the queue so ordering with subsequent messages is sane.
    let welcome = ServerMessage::Welcome {
        protocol_version: PROTOCOL_VERSION,
        player_id,
        username: record.username.clone(),
        stats: record.stats.clone(),
    };
    if conn.out_tx.send(welcome).await.is_err() {
        ctx.registry.logout(player_id).await;
        return Err(SessionError::Disconnected);
    }

    // Writer task drains `out_rx` until either the queue closes
    // (last sender dropped) or the socket errors.
    let writer = tokio::spawn(writer_task(write, out_rx));

    // ---- post-handshake reader loop ----
    let outcome = reader_loop(&mut read, &conn, &ctx).await;

    // Cleanup: drop any in-flight prompt, leave any table, log out.
    conn.cancel_pending();
    if let Some(table_id) = ctx.tables.record_leave(player_id).await {
        if let Some(table) = ctx.tables.get(table_id).await {
            table.force_leave(player_id).await;
        }
    }
    ctx.registry.logout(player_id).await;

    // Closing the outbound queue lets the writer task wind down.
    drop(conn);
    let _ = writer.await;

    info!(%peer, player_id, "session closed");
    outcome
}

async fn reader_loop<R>(
    read: &mut R,
    conn: &Arc<Connection>,
    ctx: &ServerContext,
) -> Result<(), SessionError>
where
    R: AsyncRead + Unpin,
{
    loop {
        let msg: ClientMessage = match read_message(read).await {
            Ok(m) => m,
            Err(WireError::Closed) => return Ok(()),
            Err(other) => return Err(other.into()),
        };
        match msg {
            ClientMessage::Hello { .. } => {
                return Err(SessionError::OutOfOrder { got: "Hello" });
            }
            ClientMessage::Heartbeat => {
                conn.try_send(ServerMessage::Heartbeat);
            }
            ClientMessage::Disconnect => {
                let _ = conn.out_tx
                    .send(ServerMessage::Goodbye { reason: "client requested".into() })
                    .await;
                return Ok(());
            }
            ClientMessage::ListTables => {
                let tables = ctx.tables.list_infos().await;
                conn.try_send(ServerMessage::TableList { tables });
            }
            ClientMessage::JoinTable { table_id, buy_in } => {
                handle_join(conn, ctx, table_id, buy_in).await;
            }
            ClientMessage::LeaveTable { table_id } => {
                handle_leave(conn, ctx, table_id).await;
            }
            ClientMessage::SubmitAction { table_id, hand_id, action } => {
                if !conn.deliver_action(table_id, hand_id, action) {
                    conn.try_send(ServerMessage::ActionRejected {
                        reason: "no matching pending action".into(),
                    });
                }
            }
        }
    }
}

async fn handle_join(
    conn: &Arc<Connection>,
    ctx: &ServerContext,
    table_id: TableId,
    buy_in: u32,
) {
    if ctx.tables.current_table(conn.player_id).await.is_some() {
        conn.try_send(ServerMessage::ActionRejected {
            reason: "already seated at a table".into(),
        });
        return;
    }
    let Some(table) = ctx.tables.get(table_id).await else {
        conn.try_send(ServerMessage::ActionRejected {
            reason: format!("no such table: {table_id}"),
        });
        return;
    };
    match table.sit(Arc::clone(conn), buy_in).await {
        Ok(seat) => {
            ctx.tables.record_join(conn.player_id, table_id).await;
            let seats = table.seat_infos().await;
            conn.try_send(ServerMessage::JoinedTable { table_id, seat, seats });
            // Broadcast the new layout to existing players. The
            // joiner already has it via JoinedTable.
            table.broadcast_table_state(Some(conn.player_id)).await;
        }
        Err(reason) => {
            conn.try_send(ServerMessage::ActionRejected { reason });
        }
    }
}

async fn handle_leave(
    conn: &Arc<Connection>,
    ctx: &ServerContext,
    table_id: TableId,
) {
    let Some(table) = ctx.tables.get(table_id).await else {
        conn.try_send(ServerMessage::ActionRejected {
            reason: format!("no such table: {table_id}"),
        });
        return;
    };
    match table.leave(conn.player_id).await {
        Ok(()) => {
            ctx.tables.record_leave(conn.player_id).await;
            conn.try_send(ServerMessage::LeftTable { table_id });
            table.broadcast_table_state(Some(conn.player_id)).await;
        }
        Err(reason) => {
            conn.try_send(ServerMessage::ActionRejected { reason });
        }
    }
}
