//! Per-connection lifecycle.
//!
//! After the [`ClientMessage::Register`] / [`ClientMessage::Authenticate`]
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
//!
//! Session revocation: the registry hands the reader an
//! `Arc<AtomicBool>` that flips to `true` when a newer login
//! supersedes this one. The reader checks the flag on each iteration
//! and tears the connection down with a `Goodbye { "session_revoked" }`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use poker_engine::net::protocol::{
    AuthMode, ClientMessage, PROTOCOL_VERSION, RejectReason, ServerMessage, TableId,
};

use crate::connection::{Connection, writer_task};
use crate::limits::{ConnectionLimits, TokenBucket};
use crate::registry::{AuthSuccess, Registry, RegistryError};
use crate::table::TableManager;
use crate::wire::{WireError, read_message, write_message};

/// Bundle of long-lived shared state passed to every session.
#[derive(Clone)]
pub struct ServerContext {
    pub registry: Arc<Registry>,
    pub tables: TableManager,
    pub limits: ConnectionLimits,
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
pub async fn handle_connection(stream: TcpStream, peer: SocketAddr, ctx: ServerContext) {
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
    // Same idle timeout as the post-handshake reader: a peer that
    // opens a TCP connection and never sends Register can't tie up
    // a session task indefinitely.
    let handshake = timeout(
        ctx.limits.idle_timeout,
        perform_handshake(&mut read, &mut write, &ctx),
    )
    .await;
    let auth = match handshake {
        Ok(Ok(Some(a))) => a,
        Ok(Ok(None)) => return Ok(()), // rejection already sent
        Ok(Err(e)) => return Err(e),
        Err(_elapsed) => {
            debug!(%peer, "handshake idle timeout");
            return Ok(());
        }
    };
    let player_id = auth.record.player_id;
    let session_id = auth.session_id;
    let revoked = auth.revoked.clone();
    info!(%peer, player_id, username = %auth.record.username, session_id, "client authenticated");

    // ---- per-connection plumbing ----
    let (conn_data, out_rx) =
        Connection::new(player_id, auth.record.username.clone(), session_id);
    let conn = Arc::new(conn_data);

    // Welcome rides the outbound queue too — fast-path the first send
    // through the queue so ordering with subsequent messages is sane.
    let welcome = ServerMessage::Welcome {
        protocol_version: PROTOCOL_VERSION,
        player_id,
        username: auth.record.username.clone(),
        session_key: auth.session_key.clone(),
        stats: auth.record.stats.clone(),
    };
    if conn.out_tx.send(welcome).await.is_err() {
        ctx.registry.logout(player_id, session_id).await;
        return Err(SessionError::Disconnected);
    }

    // Writer task drains `out_rx` until either the queue closes
    // (last sender dropped) or the socket errors.
    let writer = tokio::spawn(writer_task(write, out_rx));

    // Step 22c — reconnect takeover. If this player was already seated
    // when their previous session was superseded, hand the seat over
    // to this connection. `reconnect_player` itself pushes the
    // `JoinedTable` notice and any migrated in-flight `Prompt`.
    if let Some(table_id) = ctx.tables.current_table(player_id).await {
        if let Some(table) = ctx.tables.get(table_id).await {
            let _ = table.reconnect_player(Arc::clone(&conn)).await;
        }
    }

    // ---- post-handshake reader loop ----
    let outcome = reader_loop(&mut read, &conn, &ctx, &revoked, peer).await;

    // Cleanup: drop any in-flight prompt, leave any table, log out.
    conn.cancel_pending();
    // If a newer session has already taken over this player's seat
    // (revoked flag set by the registry on subsequent login), do NOT
    // tear the seat down — the new session is now driving it.
    let superseded = revoked.load(Ordering::SeqCst);
    if !superseded {
        if let Some(table_id) = ctx.tables.record_leave(player_id).await {
            if let Some(table) = ctx.tables.get(table_id).await {
                table.force_leave(player_id, Some(session_id)).await;
            }
        }
    }
    ctx.registry.logout(player_id, session_id).await;

    // Closing the outbound queue lets the writer task wind down.
    drop(conn);
    let _ = writer.await;

    info!(%peer, player_id, session_id, "session closed");
    outcome
}

/// Read the first message and resolve it into an [`AuthSuccess`], or
/// send the appropriate `Rejected` and return `Ok(None)`.
async fn perform_handshake<R, W>(
    read: &mut R,
    write: &mut W,
    ctx: &ServerContext,
) -> Result<Option<AuthSuccess>, SessionError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let first: ClientMessage = match read_message(read).await {
        Ok(msg) => msg,
        Err(WireError::Closed) => return Err(SessionError::Disconnected),
        Err(other) => return Err(other.into()),
    };

    let outcome = match first {
        ClientMessage::Register {
            protocol_version,
            email,
            username,
            password,
            device_label,
        } => {
            if protocol_version != PROTOCOL_VERSION {
                send_rejected(write, RejectReason::ProtocolMismatch).await?;
                return Ok(None);
            }
            ctx.registry
                .register(&email, &username, &password, device_label.as_deref())
                .await
        }
        ClientMessage::Authenticate {
            protocol_version,
            mode,
            device_label,
        } => {
            if protocol_version != PROTOCOL_VERSION {
                send_rejected(write, RejectReason::ProtocolMismatch).await?;
                return Ok(None);
            }
            match mode {
                AuthMode::Password { identifier, password } => {
                    ctx.registry
                        .authenticate_password(&identifier, &password, device_label.as_deref())
                        .await
                }
                AuthMode::Session { key } => {
                    ctx.registry.authenticate_session(&key).await
                }
            }
        }
        ClientMessage::Disconnect => return Err(SessionError::Disconnected),
        ClientMessage::Heartbeat => return Err(SessionError::OutOfOrder { got: "Heartbeat" }),
        _ => return Err(SessionError::OutOfOrder { got: "non-handshake" }),
    };

    match outcome {
        Ok(success) => Ok(Some(success)),
        Err(e) => {
            let reason = registry_error_to_reason(&e);
            send_rejected(write, reason).await?;
            Err(SessionError::Registry(e))
        }
    }
}

fn registry_error_to_reason(e: &RegistryError) -> RejectReason {
    match e {
        RegistryError::InvalidUsername => RejectReason::InvalidUsername,
        RegistryError::InvalidEmail => RejectReason::InvalidEmail,
        RegistryError::InvalidPassword => RejectReason::InvalidPassword,
        RegistryError::UsernameInUse => RejectReason::UsernameInUse,
        RegistryError::EmailInUse => RejectReason::EmailInUse,
        RegistryError::BadCredentials => RejectReason::BadCredentials,
        RegistryError::SessionRevoked => RejectReason::SessionRevoked,
        RegistryError::UnknownSession => RejectReason::UnknownSession,
        _ => RejectReason::InternalError,
    }
}

async fn send_rejected<W: AsyncWrite + Unpin>(
    write: &mut W,
    reason: RejectReason,
) -> Result<(), SessionError> {
    let msg = ServerMessage::Rejected {
        protocol_version: PROTOCOL_VERSION,
        reason: reason.as_str().into(),
    };
    write_message(write, &msg).await?;
    Ok(())
}

async fn reader_loop<R>(
    read: &mut R,
    conn: &Arc<Connection>,
    ctx: &ServerContext,
    revoked: &Arc<AtomicBool>,
    peer: SocketAddr,
) -> Result<(), SessionError>
where
    R: AsyncRead + Unpin,
{
    let mut bucket = TokenBucket::new(ctx.limits);
    loop {
        let msg: ClientMessage = match timeout(ctx.limits.idle_timeout, read_message(read)).await {
            Ok(Ok(m)) => m,
            Ok(Err(WireError::Closed)) => return Ok(()),
            Ok(Err(other)) => return Err(other.into()),
            Err(_elapsed) => {
                debug!(%peer, player_id = conn.player_id, "idle timeout");
                let _ = conn
                    .out_tx
                    .send(ServerMessage::Goodbye {
                        reason: "idle timeout".into(),
                    })
                    .await;
                return Ok(());
            }
        };
        // A newer login may have flipped the flag while we were parked
        // in `read_message`. Check after each wake-up so the next
        // inbound byte forces a teardown rather than getting echoed.
        if revoked.load(Ordering::SeqCst) {
            let _ = conn
                .out_tx
                .send(ServerMessage::Goodbye {
                    reason: RejectReason::SessionRevoked.as_str().into(),
                })
                .await;
            return Ok(());
        }
        if !bucket.try_consume() {
            warn!(%peer, player_id = conn.player_id, "rate limit exceeded; closing");
            let _ = conn
                .out_tx
                .send(ServerMessage::Goodbye {
                    reason: "rate limit exceeded".into(),
                })
                .await;
            return Ok(());
        }
        match msg {
            ClientMessage::Register { .. } | ClientMessage::Authenticate { .. } => {
                return Err(SessionError::OutOfOrder { got: "re-handshake" });
            }
            ClientMessage::Heartbeat => {
                conn.try_send(ServerMessage::Heartbeat);
            }
            ClientMessage::Disconnect => {
                let _ = conn
                    .out_tx
                    .send(ServerMessage::Goodbye {
                        reason: "client requested".into(),
                    })
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
            ClientMessage::SubmitAction {
                table_id,
                hand_id,
                action,
            } => {
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
            conn.try_send(ServerMessage::JoinedTable {
                table_id,
                seat,
                seats,
            });
            // Broadcast the new layout to existing players. The
            // joiner already has it via JoinedTable.
            table.broadcast_table_state(Some(conn.player_id)).await;
        }
        Err(reason) => {
            conn.try_send(ServerMessage::ActionRejected { reason });
        }
    }
}

async fn handle_leave(conn: &Arc<Connection>, ctx: &ServerContext, table_id: TableId) {
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
