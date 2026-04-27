//! Network worker for live mode.
//!
//! egui runs synchronously on the main thread. Tokio I/O runs on a
//! dedicated current-thread runtime in a worker thread. The two are
//! bridged by a pair of `mpsc::UnboundedSender`/`Receiver`s:
//!
//!   gui  ── ClientMessage ──>  worker  ── socket  ──>  server
//!   gui  <── LiveEvent  ──   worker  <── socket  <──  server
//!
//! `LiveClient` is the gui-side handle. It exposes sync `send` /
//! `try_recv` so the egui frame loop never blocks.

use std::thread::JoinHandle;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use poker_engine::net::frame::{self, FrameError, LENGTH_PREFIX_BYTES};
use poker_engine::net::protocol::{ClientMessage, ServerMessage, PROTOCOL_VERSION};

/// Size of the read buffer used when pulling one msgpack frame off
/// the wire. Heap-allocated per-frame so a stray oversized message
/// does not bloat steady-state memory.
const READ_BUF_INIT: usize = 1024;

/// Events delivered from the network worker to the gui thread.
pub enum LiveEvent {
    /// Successfully connected and the handshake is in flight.
    Connecting,
    /// Server sent a `Welcome` (or any other post-Hello message).
    Server(ServerMessage),
    /// Connection closed cleanly or with an error string.
    Disconnected(String),
}

/// Gui-side handle for one live connection.
pub struct LiveClient {
    out_tx: mpsc::UnboundedSender<ClientMessage>,
    in_rx: mpsc::UnboundedReceiver<LiveEvent>,
    /// Joined on drop so the worker exits before egui shuts down.
    worker: Option<JoinHandle<()>>,
}

impl LiveClient {
    /// Spawn a worker thread that connects to `addr`, sends a
    /// `Hello` with `username`, and shuttles messages until either
    /// the gui drops the client or the socket closes.
    pub fn connect(addr: String, username: String) -> Self {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (in_tx, in_rx) = mpsc::unbounded_channel();

        let worker = std::thread::Builder::new()
            .name("poker-client-net".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build tokio runtime");
                rt.block_on(run(addr, username, out_rx, in_tx));
            })
            .expect("spawn network worker");

        LiveClient { out_tx, in_rx, worker: Some(worker) }
    }

    /// Push a message toward the server. Returns `false` if the
    /// worker has exited.
    pub fn send(&self, msg: ClientMessage) -> bool {
        self.out_tx.send(msg).is_ok()
    }

    /// Drain any events the worker has produced since the last call.
    pub fn drain(&mut self, out: &mut Vec<LiveEvent>) {
        while let Ok(ev) = self.in_rx.try_recv() {
            out.push(ev);
        }
    }
}

impl Drop for LiveClient {
    fn drop(&mut self) {
        // Signal the worker by sending Disconnect, then dropping our
        // sender. The worker exits its select loop on either signal.
        let _ = self.out_tx.send(ClientMessage::Disconnect);
        if let Some(h) = self.worker.take() {
            // Don't wait forever; if the worker hangs we let the
            // process exit collect it.
            let _ = h.join();
        }
    }
}

async fn run(
    addr: String,
    username: String,
    mut out_rx: mpsc::UnboundedReceiver<ClientMessage>,
    in_tx: mpsc::UnboundedSender<LiveEvent>,
) {
    let _ = in_tx.send(LiveEvent::Connecting);

    let stream = match TcpStream::connect(&addr).await {
        Ok(s) => s,
        Err(e) => {
            let _ = in_tx.send(LiveEvent::Disconnected(format!("connect {addr}: {e}")));
            return;
        }
    };
    if let Err(e) = stream.set_nodelay(true) {
        // Non-fatal; just keep going.
        let _ = in_tx.send(LiveEvent::Server(ServerMessage::ActionRejected {
            reason: format!("set_nodelay: {e}"),
        }));
    }
    let (mut read, mut write) = stream.into_split();

    // Send Hello.
    let hello = ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        username,
    };
    if let Err(e) = send_one(&mut write, &hello).await {
        let _ = in_tx.send(LiveEvent::Disconnected(format!("send Hello: {e}")));
        return;
    }

    // Pump loop: forward gui→server and server→gui.
    loop {
        tokio::select! {
            res = read_one(&mut read) => match res {
                Ok(msg) => {
                    let close = matches!(msg, ServerMessage::Goodbye { .. });
                    if in_tx.send(LiveEvent::Server(msg)).is_err() {
                        // Gui dropped the receiver; quit.
                        let _ = write.shutdown().await;
                        return;
                    }
                    if close {
                        let _ = in_tx.send(LiveEvent::Disconnected("server closed connection".into()));
                        return;
                    }
                }
                Err(e) => {
                    let _ = in_tx.send(LiveEvent::Disconnected(e));
                    return;
                }
            },
            outbound = out_rx.recv() => match outbound {
                Some(msg) => {
                    let is_disconnect = matches!(msg, ClientMessage::Disconnect);
                    if let Err(e) = send_one(&mut write, &msg).await {
                        let _ = in_tx.send(LiveEvent::Disconnected(format!("send: {e}")));
                        return;
                    }
                    if is_disconnect {
                        // Give the server a moment to flush a Goodbye
                        // before we tear the socket down.
                        let _ = tokio::time::timeout(
                            Duration::from_millis(250),
                            read_one(&mut read),
                        ).await;
                        let _ = in_tx.send(LiveEvent::Disconnected("disconnected".into()));
                        return;
                    }
                }
                None => {
                    // Gui dropped the sender — clean shutdown.
                    let _ = write.shutdown().await;
                    return;
                }
            },
        }
    }
}

async fn send_one(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    msg: &ClientMessage,
) -> Result<(), String> {
    let bytes = frame::encode(msg).map_err(|e| format!("{e}"))?;
    write.write_all(&bytes).await.map_err(|e| format!("{e}"))?;
    write.flush().await.map_err(|e| format!("{e}"))?;
    Ok(())
}

async fn read_one(
    read: &mut tokio::net::tcp::OwnedReadHalf,
) -> Result<ServerMessage, String> {
    let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
    if let Err(e) = read.read_exact(&mut prefix).await {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            return Err("server closed".into());
        }
        return Err(format!("{e}"));
    }
    let len = frame::parse_length_prefix(prefix)
        .map_err(|e: FrameError| format!("{e}"))?;
    let mut payload = vec![0u8; len.max(READ_BUF_INIT.min(len))];
    payload.resize(len, 0);
    read.read_exact(&mut payload).await.map_err(|e| format!("{e}"))?;
    frame::decode::<ServerMessage>(&payload).map_err(|e| format!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::time::Duration;

    use poker_engine::game::BettingRules;
    use poker_server::{
        handle_connection, Registry, ServerContext, Table, TableConfig, TableManager,
    };
    use tempfile::tempdir;
    use tokio::net::TcpListener;

    /// Spin up the same in-process server the poker-server integration
    /// tests use, then drive it through `LiveClient`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_client_handshake_and_lobby() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(Registry::open(dir.path()).await.unwrap());
        let cfg = TableConfig {
            name: "Smoke".into(),
            max_seats: 2,
            min_seats: 2,
            small_blind: 1,
            big_blind: 2,
            default_buy_in: 200,
            action_deadline: Duration::from_secs(5),
            between_hands: Duration::from_millis(50),
        };
        let rules = BettingRules::no_limit_holdem(
            cfg.small_blind,
            cfg.big_blind,
            cfg.max_seats as usize,
        );
        let tables = TableManager::new();
        tables.install(Table::new(7, cfg), rules).await;
        let ctx = ServerContext { registry, tables };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let ctx = ctx.clone();
                tokio::spawn(async move { handle_connection(stream, peer, ctx).await });
            }
        });

        let mut client = LiveClient::connect(addr.to_string(), "alice".into());

        // Wait up to 2s for Welcome.
        let mut welcomed = false;
        let mut buf = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
            client.drain(&mut buf);
            for ev in buf.drain(..) {
                if let LiveEvent::Server(ServerMessage::Welcome { username, .. }) = ev {
                    assert_eq!(username, "alice");
                    welcomed = true;
                    break;
                }
            }
            if welcomed {
                break;
            }
        }
        assert!(welcomed, "did not receive Welcome");

        // Ask for the lobby and confirm we get a TableList containing
        // the table we installed above.
        assert!(client.send(ClientMessage::ListTables));
        let mut listed = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
            client.drain(&mut buf);
            for ev in buf.drain(..) {
                if let LiveEvent::Server(ServerMessage::TableList { tables }) = ev {
                    assert!(tables.iter().any(|t| t.table_id == 7));
                    listed = true;
                    break;
                }
            }
            if listed {
                break;
            }
        }
        assert!(listed, "did not receive TableList");
    }
}
