//! Per-username persistent records + an in-memory connection map.
//!
//! Records live as `<data_dir>/users/<username>.mp` (msgpack) so a
//! returning client gets the same `PlayerId` and lifetime stats it had
//! before. The on-disk format is intentionally not the wire format —
//! `PlayerRecord` carries server-only fields (creation timestamp,
//! schema version) that the client doesn't need to see.
//!
//! Concurrency: the registry holds an in-memory map of who's currently
//! online (keyed by `PlayerId`) so a second connection from the same
//! username can be rejected with `AlreadyConnected`. The map is guarded
//! by a `Mutex`; lookups are cheap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::Mutex;

use poker_engine::net::protocol::{is_valid_username, LifetimeStats, PlayerId};

/// Bumped if `PlayerRecord` ever changes shape on disk.
const RECORD_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("invalid username")]
    InvalidUsername,
    #[error("username already connected")]
    AlreadyConnected,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("encode error: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("schema mismatch: file v{found}, expected v{expected}")]
    SchemaMismatch { found: u32, expected: u32 },
}

/// On-disk record for one player.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerRecord {
    pub schema_version: u32,
    pub player_id: PlayerId,
    pub username: String,
    /// Seconds since UNIX epoch. Stored for diagnostics; not load-bearing.
    pub created_unix: u64,
    pub stats: LifetimeStats,
}

/// Server-side player registry: file-backed records + an online set.
pub struct Registry {
    user_dir: PathBuf,
    /// Monotonically increasing source for new `PlayerId`s. Persisted
    /// implicitly via the records on disk: on startup we scan and seed
    /// this above the current maximum.
    next_id: AtomicU64,
    /// Currently-connected players keyed by `PlayerId`. The value is
    /// the username for diagnostics.
    online: Mutex<HashMap<PlayerId, String>>,
}

impl Registry {
    /// Open or create a registry rooted at `data_dir`. Scans the
    /// `users/` subdirectory to seed the next-id counter.
    pub async fn open(data_dir: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let user_dir = data_dir.as_ref().join("users");
        fs::create_dir_all(&user_dir).await?;

        let mut max_id: PlayerId = 0;
        let mut entries = fs::read_dir(&user_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("mp") {
                continue;
            }
            let bytes = match fs::read(&path).await {
                Ok(b) => b,
                Err(_) => continue, // ignore unreadable stragglers
            };
            if let Ok(rec) = rmp_serde::from_slice::<PlayerRecord>(&bytes) {
                max_id = max_id.max(rec.player_id);
            }
        }

        Ok(Self {
            user_dir,
            next_id: AtomicU64::new(max_id + 1),
            online: Mutex::new(HashMap::new()),
        })
    }

    /// Resolve a username to a [`PlayerRecord`], creating one if this
    /// is a first-time login. Marks the player as online so a second
    /// concurrent `Hello` gets [`RegistryError::AlreadyConnected`].
    pub async fn login(&self, username: &str) -> Result<PlayerRecord, RegistryError> {
        if !is_valid_username(username) {
            return Err(RegistryError::InvalidUsername);
        }

        let record = match self.load_by_username(username).await? {
            Some(rec) => rec,
            None => self.create_record(username).await?,
        };

        let mut online = self.online.lock().await;
        if online.contains_key(&record.player_id) {
            return Err(RegistryError::AlreadyConnected);
        }
        online.insert(record.player_id, record.username.clone());
        Ok(record)
    }

    /// Mark a player offline. Idempotent.
    pub async fn logout(&self, player_id: PlayerId) {
        let mut online = self.online.lock().await;
        online.remove(&player_id);
    }

    /// Snapshot the current online list. Diagnostic helper.
    pub async fn online_count(&self) -> usize {
        self.online.lock().await.len()
    }

    /// Persist updated lifetime stats for `player_id`. No-op if the
    /// record file has been removed externally.
    pub async fn update_stats(
        &self,
        player_id: PlayerId,
        stats: LifetimeStats,
    ) -> Result<(), RegistryError> {
        let username = {
            let online = self.online.lock().await;
            online.get(&player_id).cloned()
        };
        let Some(username) = username else { return Ok(()) };
        let mut record = match self.load_by_username(&username).await? {
            Some(rec) => rec,
            None => return Ok(()),
        };
        record.stats = stats;
        self.save_record(&record).await
    }

    fn record_path(&self, username: &str) -> PathBuf {
        self.user_dir.join(format!("{username}.mp"))
    }

    async fn load_by_username(
        &self,
        username: &str,
    ) -> Result<Option<PlayerRecord>, RegistryError> {
        let path = self.record_path(username);
        let bytes = match fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let record: PlayerRecord = rmp_serde::from_slice(&bytes)?;
        if record.schema_version != RECORD_SCHEMA_VERSION {
            return Err(RegistryError::SchemaMismatch {
                found: record.schema_version,
                expected: RECORD_SCHEMA_VERSION,
            });
        }
        Ok(Some(record))
    }

    async fn create_record(&self, username: &str) -> Result<PlayerRecord, RegistryError> {
        let player_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let created_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let record = PlayerRecord {
            schema_version: RECORD_SCHEMA_VERSION,
            player_id,
            username: username.to_string(),
            created_unix,
            stats: LifetimeStats::default(),
        };
        self.save_record(&record).await?;
        Ok(record)
    }

    async fn save_record(&self, record: &PlayerRecord) -> Result<(), RegistryError> {
        let bytes = rmp_serde::to_vec(record)?;
        let path = self.record_path(&record.username);
        // Write to a tempfile and rename so partial writes never leave
        // a half-baked record on disk.
        let tmp = path.with_extension("mp.tmp");
        fs::write(&tmp, &bytes).await?;
        fs::rename(&tmp, &path).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn login_creates_record_then_returns_same_id() {
        let dir = tempdir().unwrap();
        let reg = Registry::open(dir.path()).await.unwrap();

        let rec1 = reg.login("alice").await.unwrap();
        assert_eq!(rec1.username, "alice");
        assert_eq!(rec1.stats.hands, 0);
        let id = rec1.player_id;

        reg.logout(id).await;

        let rec2 = reg.login("alice").await.unwrap();
        assert_eq!(rec2.player_id, id);
    }

    #[tokio::test]
    async fn second_concurrent_login_rejects() {
        let dir = tempdir().unwrap();
        let reg = Registry::open(dir.path()).await.unwrap();

        let _ = reg.login("alice").await.unwrap();
        let err = reg.login("alice").await.unwrap_err();
        assert!(matches!(err, RegistryError::AlreadyConnected));
    }

    #[tokio::test]
    async fn invalid_username_rejected() {
        let dir = tempdir().unwrap();
        let reg = Registry::open(dir.path()).await.unwrap();
        let err = reg.login("x").await.unwrap_err();
        assert!(matches!(err, RegistryError::InvalidUsername));
    }

    #[tokio::test]
    async fn ids_persist_and_advance_across_reopen() {
        let dir = tempdir().unwrap();

        let reg = Registry::open(dir.path()).await.unwrap();
        let a = reg.login("alice").await.unwrap();
        let b = reg.login("bob").await.unwrap();
        assert_ne!(a.player_id, b.player_id);
        drop(reg);

        let reg = Registry::open(dir.path()).await.unwrap();
        let c = reg.login("carol").await.unwrap();
        assert!(c.player_id > a.player_id.max(b.player_id));
    }

    #[tokio::test]
    async fn update_stats_persists() {
        let dir = tempdir().unwrap();
        let reg = Registry::open(dir.path()).await.unwrap();
        let rec = reg.login("alice").await.unwrap();

        let mut new_stats = LifetimeStats::default();
        new_stats.hands = 100;
        new_stats.chip_delta = -42;
        reg.update_stats(rec.player_id, new_stats).await.unwrap();

        reg.logout(rec.player_id).await;
        let rec2 = reg.login("alice").await.unwrap();
        assert_eq!(rec2.stats.hands, 100);
        assert_eq!(rec2.stats.chip_delta, -42);
    }
}
