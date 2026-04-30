//! Scripted test scenarios. A [`Script`] is a plain `Vec<Step>` the
//! [`Driver`] pumps against a [`crate::HeadlessClient`], recording
//! a snapshot after each step. Failed expectations return rather
//! than panic; callers call [`run_and_assert`] if they want
//! panic-on-fail behaviour for integration tests.
//!
//! # File format
//!
//! [`FileScript`] is a serde-serialisable subset of [`Script`] —
//! just [`Step::Issue`] and [`Step::Receive`]. The CLI binary
//! (`poker-client-headless`) loads one of these, replays it, and
//! prints each resulting snapshot. Use it to turn a flaky
//! reproduction into a committed repro file.

use std::sync::Arc;

use poker_client_core::{ClientView, Intent, Phase};
use poker_engine::net::protocol::ServerMessage;
use serde::{Deserialize, Serialize};

use crate::harness::HeadlessClient;

/// Predicate on a [`ClientView`]. Stored as `Arc<dyn Fn>` so
/// [`Step`] stays `Clone`; predicates can close over test-local
/// state (e.g. captured table ids).
pub type ViewPredicate = Arc<dyn Fn(&ClientView) -> bool + Send + Sync>;

/// One step in a scripted scenario.
#[derive(Clone)]
pub enum Step {
    /// Forward an [`Intent`] to the core.
    Issue(Intent),
    /// Inject an inbound [`ServerMessage`] directly — i.e. pretend
    /// the server just sent us this. Bypasses any transport.
    Receive(ServerMessage),
    /// Assert the post-step phase matches exactly.
    ExpectPhase(Phase),
    /// Assert a predicate on the post-step view.
    Expect {
        label: String,
        predicate: ViewPredicate,
    },
}

impl std::fmt::Debug for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Step::Issue(i) => f.debug_tuple("Issue").field(i).finish(),
            Step::Receive(m) => f.debug_tuple("Receive").field(m).finish(),
            Step::ExpectPhase(p) => f.debug_tuple("ExpectPhase").field(p).finish(),
            Step::Expect { label, .. } => {
                f.debug_struct("Expect").field("label", label).finish()
            }
        }
    }
}

impl Step {
    /// Convenience ctor for a predicate-based expectation.
    pub fn expect(
        label: impl Into<String>,
        predicate: impl Fn(&ClientView) -> bool + Send + Sync + 'static,
    ) -> Self {
        Step::Expect {
            label: label.into(),
            predicate: Arc::new(predicate),
        }
    }
}

/// A linear sequence of steps.
pub type Script = Vec<Step>;

/// What [`Driver::run`] returns.
#[derive(Debug)]
pub struct DriverResult {
    /// Snapshot after each applied step, in order. Length equals
    /// the number of steps that ran (including the one that
    /// failed, if any).
    pub snapshots: Vec<ClientView>,
    /// `None` if every step succeeded; otherwise
    /// `(step_index, failure_message)`.
    pub failure: Option<(usize, String)>,
}

impl DriverResult {
    pub fn passed(&self) -> bool {
        self.failure.is_none()
    }
}

/// Runs a [`Script`] against a [`HeadlessClient`]. Stateless —
/// holds no config; exists only as a namespace for the entry
/// points.
pub struct Driver;

impl Driver {
    /// Pump each step. Stops at the first failed expectation and
    /// returns what was seen so far. Never panics.
    pub fn run(h: &mut HeadlessClient, script: &[Step]) -> DriverResult {
        let mut snapshots = Vec::with_capacity(script.len());
        for (idx, step) in script.iter().enumerate() {
            match step {
                Step::Issue(intent) => {
                    h.intent(intent.clone());
                }
                Step::Receive(msg) => {
                    h.inbound(msg.clone());
                }
                Step::ExpectPhase(expected) => {
                    let view = h.view();
                    if &view.phase != expected {
                        snapshots.push(view.clone());
                        return DriverResult {
                            snapshots,
                            failure: Some((
                                idx,
                                format!(
                                    "ExpectPhase: expected {expected:?}, got {:?}",
                                    view.phase
                                ),
                            )),
                        };
                    }
                }
                Step::Expect { label, predicate } => {
                    let view = h.view();
                    if !predicate(&view) {
                        snapshots.push(view);
                        return DriverResult {
                            snapshots,
                            failure: Some((idx, format!("Expect '{label}' failed"))),
                        };
                    }
                }
            }
            snapshots.push(h.view());
        }
        DriverResult {
            snapshots,
            failure: None,
        }
    }

    /// Like [`Driver::run`] but panics on a failed expectation.
    /// The panic message includes the step index, the failure
    /// reason, and the full snapshot trail.
    pub fn run_and_assert(h: &mut HeadlessClient, script: &[Step]) {
        let result = Self::run(h, script);
        if let Some((idx, msg)) = result.failure {
            let trail = result
                .snapshots
                .iter()
                .enumerate()
                .map(|(i, s)| format!("  [{i}] phase={:?}", s.phase))
                .collect::<Vec<_>>()
                .join("\n");
            panic!("scenario failed at step {idx}: {msg}\nsnapshot trail:\n{trail}");
        }
    }
}

// ─── File format ────────────────────────────────────────────────────

/// Serde-serialisable subset of [`Step`]. Only `Issue` and
/// `Receive` round-trip through disk — predicates don't.
/// The CLI uses this to replay recorded scenarios.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileStep {
    Issue { intent: Intent },
    Receive { message: ServerMessage },
}

impl FileStep {
    pub fn into_step(self) -> Step {
        match self {
            FileStep::Issue { intent } => Step::Issue(intent),
            FileStep::Receive { message } => Step::Receive(message),
        }
    }

    pub fn from_step(step: &Step) -> Option<Self> {
        match step {
            Step::Issue(intent) => Some(FileStep::Issue {
                intent: intent.clone(),
            }),
            Step::Receive(msg) => Some(FileStep::Receive {
                message: msg.clone(),
            }),
            Step::ExpectPhase(_) | Step::Expect { .. } => None,
        }
    }
}

/// On-disk scenario — a JSON array of [`FileStep`]s.
pub type FileScript = Vec<FileStep>;

/// Load a [`FileScript`] from a JSON file.
pub fn load_file_script(path: impl AsRef<std::path::Path>) -> std::io::Result<FileScript> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("parse: {e}"))
    })
}

/// Save a [`FileScript`] to a JSON file (pretty-printed).
pub fn save_file_script(
    path: impl AsRef<std::path::Path>,
    script: &FileScript,
) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(script)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}")))?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_engine::net::protocol::{LifetimeStats, PROTOCOL_VERSION};

    fn welcome() -> ServerMessage {
        ServerMessage::Welcome {
            protocol_version: PROTOCOL_VERSION,
            player_id: 1,
            username: "alice".into(),
            session_key: "abc".into(),
            stats: LifetimeStats::default(),
        }
    }

    #[test]
    fn driver_runs_linear_script_to_completion() {
        let script = vec![
            Step::Issue(Intent::Connect {
                addr: "test".into(),
            }),
            Step::ExpectPhase(Phase::Connecting),
            Step::Receive(welcome()),
            Step::ExpectPhase(Phase::Lobby),
            Step::expect("username=alice", |v| {
                v.username.as_deref() == Some("alice")
            }),
        ];

        let mut h = HeadlessClient::new(None);
        let result = Driver::run(&mut h, &script);
        assert!(result.passed(), "script failed: {:?}", result.failure);
        assert_eq!(
            result.snapshots.len(),
            script.len(),
            "one snapshot per step (including expectations)",
        );
    }

    #[test]
    fn driver_stops_at_first_failed_expectation() {
        let script = vec![
            Step::Issue(Intent::Connect {
                addr: "test".into(),
            }),
            Step::ExpectPhase(Phase::Lobby), // wrong: should be Connecting
            Step::Issue(Intent::Disconnect),
        ];

        let mut h = HeadlessClient::new(None);
        let result = Driver::run(&mut h, &script);
        let (idx, msg) = result.failure.expect("expected failure");
        assert_eq!(idx, 1);
        assert!(msg.contains("ExpectPhase"), "message: {msg}");
        assert_eq!(result.snapshots.len(), 2, "stopped after the failing step");
    }

    #[test]
    fn run_and_assert_panics_with_trail_on_failure() {
        let script = vec![Step::ExpectPhase(Phase::Lobby)];
        let mut h = HeadlessClient::new(None);

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Driver::run_and_assert(&mut h, &script);
        }));
        let err = res.expect_err("expected panic");
        let msg = err
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| err.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(
            msg.contains("ExpectPhase"),
            "panic message should mention the failure: {msg}",
        );
    }

    #[test]
    fn file_script_roundtrips_through_json() {
        let script = vec![
            FileStep::Issue {
                intent: Intent::Connect {
                    addr: "addr".into(),
                },
            },
            FileStep::Receive { message: welcome() },
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scenario.json");
        save_file_script(&path, &script).unwrap();
        let loaded = load_file_script(&path).unwrap();
        assert_eq!(loaded.len(), script.len());

        // Convert to Steps and run them — the round-trip should
        // produce a working scenario.
        let steps: Vec<Step> = loaded.into_iter().map(FileStep::into_step).collect();
        let mut h = HeadlessClient::new(None);
        let result = Driver::run(&mut h, &steps);
        assert!(result.passed());
        assert_eq!(h.view().phase, Phase::Lobby);
    }

    #[test]
    fn from_step_discards_predicates() {
        assert!(FileStep::from_step(&Step::Issue(Intent::Heartbeat)).is_some());
        assert!(
            FileStep::from_step(&Step::ExpectPhase(Phase::Lobby)).is_none(),
            "expectations are not file-serialisable",
        );
        assert!(
            FileStep::from_step(&Step::expect("x", |_| true)).is_none(),
        );
    }
}
