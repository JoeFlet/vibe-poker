//! High-level runtime facade.
//!
//! Pulls [`ClientCore`] + [`Transport`] + [`SessionStore`] together
//! behind a thread-safe synchronous API:
//!
//! - [`NativeClient::issue`] forwards an [`Intent`] to the core.
//! - [`NativeClient::snapshot`] returns the latest [`ClientView`].
//! - [`NativeClient::drain_logs`] pulls buffered log entries.
//!
//! All I/O and state mutation happen on a dedicated worker thread
//! that owns both the core and the transport. The host never
//! awaits, never allocates a runtime — it just polls. This shape
//! is what the Tauri shell wants: Tauri commands can call
//! `snapshot()` and `issue()` synchronously on any thread.
//!
//! Effects are routed internally:
//! - [`Effect::OpenConnection`] → `transport.open`
//! - [`Effect::Send`] → `handle.send`
//! - [`Effect::CloseConnection`] → drop the handle
//! - [`Effect::PersistSessionKey`] → [`SessionStore::save`] / `clear`
//! - [`Effect::Log`] → `tracing` + host log buffer
//! - [`Effect::Schedule`] → **TODO**, see note at bottom of file.

use std::sync::{Arc, Mutex};
use std::thread;

use poker_client_core::{ClientCore, ClientView, Effect, Intent, LogLevel};
use tokio::runtime::Builder;
use tokio::sync::{mpsc, watch};

use crate::native::NativeTransport;
use crate::session::SessionStore;
use crate::transport::{Transport, TransportHandle, TransportIn};

/// One structured log line. Mirrors [`Effect::Log`] so the host can
/// surface it without pulling in `poker-client-core` directly.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    pub fields: Vec<(&'static str, String)>,
}

/// Runtime facade. Construct once, issue intents, read snapshots.
pub struct NativeClient {
    /// `Option` so [`Drop`] can force-close the channel (and thus
    /// the worker loop) before joining the thread.
    intent_tx: Option<mpsc::UnboundedSender<Intent>>,
    view_rx: watch::Receiver<ClientView>,
    log_buffer: Arc<Mutex<Vec<LogEntry>>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl NativeClient {
    /// Default construction: [`NativeTransport`] + a filesystem
    /// session store at `session_path`.
    pub fn new(session_path: std::path::PathBuf) -> std::io::Result<Self> {
        Self::with_transport(NativeTransport::new(), SessionStore::new(session_path))
    }

    /// Build with a caller-supplied [`Transport`] (e.g. a mock in
    /// tests) and [`SessionStore`]. Any persisted session key is
    /// loaded and handed to [`ClientCore::new`]; the caller still
    /// issues an [`Intent::AuthenticateSession`] to actually
    /// reauthenticate against the server.
    pub fn with_transport<T: Transport>(
        transport: T,
        session_store: SessionStore,
    ) -> std::io::Result<Self> {
        let initial_key = session_store.load()?;
        let initial_core = ClientCore::new(initial_key);

        let (intent_tx, intent_rx) = mpsc::unbounded_channel::<Intent>();
        let (view_tx, view_rx) = watch::channel(initial_core.snapshot());

        let log_buffer = Arc::new(Mutex::new(Vec::new()));
        let log_buffer_thread = Arc::clone(&log_buffer);

        let thread = thread::Builder::new()
            .name("poker-client-runtime".into())
            .spawn(move || {
                let rt = Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build tokio runtime");
                rt.block_on(run_loop(
                    initial_core,
                    transport,
                    session_store,
                    intent_rx,
                    view_tx,
                    log_buffer_thread,
                ));
            })?;

        Ok(Self {
            intent_tx: Some(intent_tx),
            view_rx,
            log_buffer,
            thread: Some(thread),
        })
    }

    /// Queue an intent. Silently dropped if the worker has
    /// exited (during shutdown, or if a previous Drop has already
    /// fired).
    pub fn issue(&self, intent: Intent) {
        if let Some(tx) = self.intent_tx.as_ref() {
            let _ = tx.send(intent);
        }
    }

    /// Most recent view. Constant-time (`watch::Receiver::borrow`
    /// + clone of the small `ClientView`).
    pub fn snapshot(&self) -> ClientView {
        self.view_rx.borrow().clone()
    }

    /// Drain accumulated log entries since the previous call. Each
    /// entry was also handed to `tracing` at its native level.
    pub fn drain_logs(&self) -> Vec<LogEntry> {
        std::mem::take(&mut *self.log_buffer.lock().unwrap())
    }
}

impl Drop for NativeClient {
    fn drop(&mut self) {
        // Close the intent channel so the worker's `select!`
        // observes `None` and returns. Only then can we join the
        // thread without risking deadlock.
        drop(self.intent_tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ─── Worker loop ────────────────────────────────────────────────────

async fn run_loop<T: Transport>(
    mut core: ClientCore,
    transport: T,
    session_store: SessionStore,
    mut intent_rx: mpsc::UnboundedReceiver<Intent>,
    view_tx: watch::Sender<ClientView>,
    log_buffer: Arc<Mutex<Vec<LogEntry>>>,
) {
    let mut handle: Option<TransportHandle> = None;

    loop {
        tokio::select! {
            biased;

            // 1. Intents from the host.
            intent = intent_rx.recv() => {
                match intent {
                    Some(intent) => {
                        let effects = core.handle_intent(intent);
                        process_effects(
                            &mut core,
                            effects,
                            &transport,
                            &mut handle,
                            &session_store,
                            &log_buffer,
                        ).await;
                        let _ = view_tx.send(core.snapshot());
                    }
                    None => {
                        // Host dropped. Clean shutdown.
                        return;
                    }
                }
            }

            // 2. Events from the transport (if connected).
            inbound = recv_or_pending(handle.as_mut()),
                if handle.is_some() =>
            {
                match inbound {
                    Some(TransportIn::Connected) => {
                        let effects = core.handle_intent(Intent::ConnectionOpened);
                        process_effects(
                            &mut core, effects, &transport,
                            &mut handle, &session_store, &log_buffer,
                        ).await;
                    }
                    Some(TransportIn::Message(msg)) => {
                        let effects = core.handle_inbound(msg);
                        process_effects(
                            &mut core, effects, &transport,
                            &mut handle, &session_store, &log_buffer,
                        ).await;
                    }
                    Some(TransportIn::Lost { reason }) => {
                        // Drop the dead handle before re-entering
                        // the core; the core will reset its view.
                        handle = None;
                        let effects = core.handle_intent(
                            Intent::ConnectionLost { reason }
                        );
                        process_effects(
                            &mut core, effects, &transport,
                            &mut handle, &session_store, &log_buffer,
                        ).await;
                    }
                    None => {
                        // Pump exited without delivering Lost
                        // (shouldn't happen; defensive).
                        handle = None;
                    }
                }
                let _ = view_tx.send(core.snapshot());
            }
        }
    }
}

/// Wraps `TransportHandle::recv` so a `None` handle blocks forever
/// rather than fast-returning. The outer `select!` guards this
/// branch with `if handle.is_some()`, but that guard is checked
/// before the future is polled, and `select!` needs a well-typed
/// future even in the `None` case.
async fn recv_or_pending(h: Option<&mut TransportHandle>) -> Option<TransportIn> {
    match h {
        Some(handle) => handle.recv().await,
        None => std::future::pending().await,
    }
}

async fn process_effects<T: Transport>(
    core: &mut ClientCore,
    effects: Vec<Effect>,
    transport: &T,
    handle: &mut Option<TransportHandle>,
    session_store: &SessionStore,
    log_buffer: &Arc<Mutex<Vec<LogEntry>>>,
) {
    for effect in effects {
        match effect {
            Effect::OpenConnection { addr } => {
                // Discard any previous handle; the core's
                // Connecting phase assumes a fresh socket.
                *handle = None;
                match transport.open(addr.clone()).await {
                    Ok(new_handle) => {
                        *handle = Some(new_handle);
                        // The pump will emit TransportIn::Connected,
                        // which the outer loop translates to
                        // Intent::ConnectionOpened.
                    }
                    Err(e) => {
                        let reason = format!("open {addr}: {e}");
                        tracing::warn!(%reason, "transport open failed");
                        // Synthesise a ConnectionLost. Any
                        // follow-up effects from the core are
                        // routed recursively below.
                        let tail = core.handle_intent(Intent::ConnectionLost { reason });
                        for t in tail {
                            apply_inert_effect(t, session_store, log_buffer);
                        }
                    }
                }
            }
            Effect::CloseConnection => {
                // Drop the handle; the pump's write half shuts
                // down, peer observes EOF, server-side cleanup.
                *handle = None;
            }
            Effect::Send(msg) => match handle.as_ref() {
                Some(h) => {
                    if h.send(msg).is_err() {
                        tracing::debug!("Send on closed transport; Lost will follow");
                    }
                }
                None => {
                    tracing::warn!("Send with no transport handle; dropping");
                }
            },
            Effect::PersistSessionKey(Some(key)) => {
                if let Err(e) = session_store.save(&key) {
                    tracing::warn!(
                        error=%e,
                        path=?session_store.path(),
                        "session key save failed",
                    );
                }
            }
            Effect::PersistSessionKey(None) => {
                if let Err(e) = session_store.clear() {
                    tracing::warn!(error=%e, "session key clear failed");
                }
            }
            Effect::Schedule { .. } => {
                // TODO: heartbeat + prompt-deadline ticks will land
                // alongside the matching core intents. Until then
                // the server's heartbeat timeout is long enough
                // (60s) that skipping these is harmless for the
                // happy path.
            }
            Effect::Log {
                level,
                message,
                fields,
            } => {
                emit_log(log_buffer, level, &message, &fields);
            }
        }
    }
}

/// Apply only side-effect-free effects: the loop's recursion after
/// a synthesised `ConnectionLost` never needs to re-enter the
/// transport, so I/O effects are ignored.
fn apply_inert_effect(
    effect: Effect,
    session_store: &SessionStore,
    log_buffer: &Arc<Mutex<Vec<LogEntry>>>,
) {
    match effect {
        Effect::PersistSessionKey(Some(k)) => {
            let _ = session_store.save(&k);
        }
        Effect::PersistSessionKey(None) => {
            let _ = session_store.clear();
        }
        Effect::Log {
            level,
            message,
            fields,
        } => {
            emit_log(log_buffer, level, &message, &fields);
        }
        // Any I/O effects here are expected to be impossible
        // because `ConnectionLost` is a pure-state-reset in the
        // core. Explicit arms keep us honest if the core grows.
        Effect::OpenConnection { .. } | Effect::CloseConnection | Effect::Send(_) => {
            tracing::warn!("I/O effect emitted post-ConnectionLost; ignoring");
        }
        Effect::Schedule { .. } => {}
    }
}

fn emit_log(
    buffer: &Arc<Mutex<Vec<LogEntry>>>,
    level: LogLevel,
    message: &str,
    fields: &[(&'static str, String)],
) {
    match level {
        LogLevel::Trace => tracing::trace!(?fields, "{}", message),
        LogLevel::Debug => tracing::debug!(?fields, "{}", message),
        LogLevel::Info => tracing::info!(?fields, "{}", message),
        LogLevel::Warn => tracing::warn!(?fields, "{}", message),
        LogLevel::Error => tracing::error!(?fields, "{}", message),
    }
    buffer.lock().unwrap().push(LogEntry {
        level,
        message: message.to_string(),
        fields: fields.to_vec(),
    });
}
