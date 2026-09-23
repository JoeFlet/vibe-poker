//! SQLite-backed user registry, password auth, and session revocation.
//!
//! The registry owns three concerns:
//!
//! 1. **Persistent identity** — `users`, `user_password`,
//!    `lifetime_stats` rows in `<data_dir>/poker.sqlite`. Reconnecting
//!    with the same credentials returns the same `PlayerId` (= user
//!    row id) and lifetime stats.
//! 2. **Authentication** — Argon2id password hashing, session keys
//!    (32 random bytes hex-encoded), and the rule that issuing a new
//!    session revokes any prior session for the same user in the same
//!    transaction.
//! 3. **Online presence** — an in-memory map keyed by `PlayerId`
//!    holding the *current* session's id + a shared "revoked" flag.
//!    When a new login lands for an already-online player we flip the
//!    prior flag; the session reader loop checks the flag on each
//!    iteration and tears down its connection cleanly.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use sqlx::SqlitePool;
use tokio::fs;
use tokio::sync::Mutex;

use poker_engine::game::SeatIndex;
use poker_engine::net::protocol::{
    LifetimeStats, PlayerId, TableId, is_valid_email, is_valid_password, is_valid_username,
};

use crate::db;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("invalid username")]
    InvalidUsername,
    #[error("invalid email")]
    InvalidEmail,
    #[error("invalid password")]
    InvalidPassword,
    #[error("username already in use")]
    UsernameInUse,
    #[error("email already in use")]
    EmailInUse,
    #[error("bad credentials")]
    BadCredentials,
    #[error("session revoked")]
    SessionRevoked,
    #[error("unknown session")]
    UnknownSession,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("password hash error: {0}")]
    Hash(String),
}

impl From<argon2::password_hash::Error> for RegistryError {
    fn from(e: argon2::password_hash::Error) -> Self {
        Self::Hash(e.to_string())
    }
}

/// In-memory view of one player's persistent record.
#[derive(Debug, Clone)]
pub struct PlayerRecord {
    pub player_id: PlayerId,
    pub username: String,
    /// Seconds since UNIX epoch when the user row was created. Stored
    /// for diagnostics; not load-bearing.
    pub created_unix: u64,
    pub stats: LifetimeStats,
}

/// Successful login result. `revoked` flips to `true` once a newer
/// session for the same user replaces this one — the session reader
/// loop polls it to know when to disconnect.
#[derive(Debug)]
pub struct AuthSuccess {
    pub record: PlayerRecord,
    pub session_key: String,
    pub session_id: i64,
    pub revoked: Arc<AtomicBool>,
}

struct OnlineEntry {
    session_id: i64,
    revoked: Arc<AtomicBool>,
    #[allow(dead_code)]
    username: String,
}

/// One row's worth of `hand_seats` data: who sat where, how their
/// stack changed, and whether they were sat-out for the hand.
#[derive(Debug, Clone)]
pub struct HandSeatRecord {
    pub seat: SeatIndex,
    pub player_id: Option<PlayerId>,
    pub chip_delta: i32,
    pub sat_out: bool,
}

/// Persisted hand: header columns plus the full `FileSink`-compatible
/// event log and per-seat outcome rows. Returned by [`Registry::fetch_hand`].
#[derive(Debug, Clone)]
pub struct HandRecord {
    pub id: i64,
    pub table_id: TableId,
    pub started_at: i64,
    pub ended_at: i64,
    pub log: Vec<u8>,
    pub seats: Vec<HandSeatRecord>,
}

/// Server-side player registry: SQLite-backed records + an online set.
pub struct Registry {
    pool: SqlitePool,
    online: Mutex<HashMap<PlayerId, OnlineEntry>>,
    /// Serialises multi-statement write transactions. sqlx issues a
    /// *deferred* `BEGIN`, so a read-then-write transaction takes a shared
    /// lock on its first `SELECT` and only tries to upgrade to a writer on
    /// its first `INSERT`. Two such transactions running at once each hold a
    /// shared lock and deadlock on the upgrade — SQLite returns `SQLITE_BUSY`
    /// immediately (it won't honour `busy_timeout` for a lock that could
    /// never be granted). SQLite serialises writers anyway, so gating these
    /// transactions here costs no real concurrency and removes the deadlock.
    write_tx: Mutex<()>,
}

impl Registry {
    pub async fn open(data_dir: impl AsRef<Path>) -> Result<Self, RegistryError> {
        let dir = data_dir.as_ref();
        fs::create_dir_all(dir).await?;
        let pool = db::open_pool(&dir.join("poker.sqlite")).await?;
        Ok(Self {
            pool,
            online: Mutex::new(HashMap::new()),
            write_tx: Mutex::new(()),
        })
    }

    pub async fn open_in_memory() -> Result<Self, RegistryError> {
        let pool = db::open_pool_in_memory().await?;
        Ok(Self {
            pool,
            online: Mutex::new(HashMap::new()),
            write_tx: Mutex::new(()),
        })
    }

    pub async fn close(self) {
        self.pool.close().await;
    }

    /// Create a new user with a password credential, then issue a
    /// fresh session. Email and username must both be unused.
    pub async fn register(
        &self,
        email: &str,
        username: &str,
        password: &str,
        device_label: Option<&str>,
    ) -> Result<AuthSuccess, RegistryError> {
        if !is_valid_email(email) {
            return Err(RegistryError::InvalidEmail);
        }
        if !is_valid_username(username) {
            return Err(RegistryError::InvalidUsername);
        }
        if !is_valid_password(password) {
            return Err(RegistryError::InvalidPassword);
        }

        let hash = hash_password(password)?;
        let now = unix_now();

        let _write = self.write_tx.lock().await;
        let mut tx = self.pool.begin().await?;

        let username_taken: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM users WHERE username = ?")
                .bind(username)
                .fetch_optional(&mut *tx)
                .await?;
        if username_taken.is_some() {
            return Err(RegistryError::UsernameInUse);
        }
        let email_taken: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM users WHERE email = ?")
                .bind(email)
                .fetch_optional(&mut *tx)
                .await?;
        if email_taken.is_some() {
            return Err(RegistryError::EmailInUse);
        }

        let res = sqlx::query("INSERT INTO users (email, username, created_at) VALUES (?, ?, ?)")
            .bind(email)
            .bind(username)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        let user_id = res.last_insert_rowid();

        sqlx::query(
            "INSERT INTO user_password (user_id, password_hash, updated_at) VALUES (?, ?, ?)",
        )
        .bind(user_id)
        .bind(&hash)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        sqlx::query("INSERT INTO lifetime_stats (user_id) VALUES (?)")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

        let (session_id, session_key) =
            issue_session_in_tx(&mut tx, user_id, device_label, now).await?;

        tx.commit().await?;

        let record = PlayerRecord {
            player_id: user_id as PlayerId,
            username: username.to_string(),
            created_unix: now as u64,
            stats: LifetimeStats::default(),
        };
        let revoked = self
            .install_online(record.player_id, session_id, &record.username)
            .await;
        Ok(AuthSuccess {
            record,
            session_key,
            session_id,
            revoked,
        })
    }

    /// Verify a username-or-email + password, then issue a fresh
    /// session. Revokes any prior live session for that user.
    pub async fn authenticate_password(
        &self,
        identifier: &str,
        password: &str,
        device_label: Option<&str>,
    ) -> Result<AuthSuccess, RegistryError> {
        let by_email = identifier.contains('@');

        let _write = self.write_tx.lock().await;
        let mut tx = self.pool.begin().await?;

        let row: Option<(i64, String, i64, String)> = if by_email {
            sqlx::query_as(
                r#"SELECT u.id, u.username, u.created_at, p.password_hash
                   FROM users u
                   JOIN user_password p ON p.user_id = u.id
                   WHERE u.email = ?"#,
            )
            .bind(identifier)
            .fetch_optional(&mut *tx)
            .await?
        } else {
            sqlx::query_as(
                r#"SELECT u.id, u.username, u.created_at, p.password_hash
                   FROM users u
                   JOIN user_password p ON p.user_id = u.id
                   WHERE u.username = ?"#,
            )
            .bind(identifier)
            .fetch_optional(&mut *tx)
            .await?
        };

        let Some((user_id, username, created_at, password_hash)) = row else {
            return Err(RegistryError::BadCredentials);
        };

        if !verify_password(password, &password_hash)? {
            return Err(RegistryError::BadCredentials);
        }

        let now = unix_now();
        let (session_id, session_key) =
            issue_session_in_tx(&mut tx, user_id, device_label, now).await?;
        let stats = load_stats_in_tx(&mut tx, user_id).await?;

        tx.commit().await?;

        let record = PlayerRecord {
            player_id: user_id as PlayerId,
            username,
            created_unix: created_at as u64,
            stats,
        };
        let revoked = self
            .install_online(record.player_id, session_id, &record.username)
            .await;
        Ok(AuthSuccess {
            record,
            session_key,
            session_id,
            revoked,
        })
    }

    /// Resume an existing live session by key. Does not rotate the
    /// key; if the key is unknown or revoked the caller gets the
    /// matching error.
    pub async fn authenticate_session(&self, key: &str) -> Result<AuthSuccess, RegistryError> {
        let _write = self.write_tx.lock().await;
        let mut tx = self.pool.begin().await?;
        let row: Option<(i64, i64, Option<i64>, String, i64)> = sqlx::query_as(
            r#"SELECT s.id, s.user_id, s.revoked_at, u.username, u.created_at
               FROM sessions s
               JOIN users u ON u.id = s.user_id
               WHERE s.key = ?"#,
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((session_id, user_id, revoked_at, username, created_at)) = row else {
            return Err(RegistryError::UnknownSession);
        };
        if revoked_at.is_some() {
            return Err(RegistryError::SessionRevoked);
        }

        let stats = load_stats_in_tx(&mut tx, user_id).await?;
        tx.commit().await?;

        let record = PlayerRecord {
            player_id: user_id as PlayerId,
            username,
            created_unix: created_at as u64,
            stats,
        };
        let revoked = self
            .install_online(record.player_id, session_id, &record.username)
            .await;
        Ok(AuthSuccess {
            record,
            session_key: key.to_string(),
            session_id,
            revoked,
        })
    }

    /// Mark a player offline IF the entry in the online map still
    /// belongs to this session — a newer login from another device
    /// will have replaced the entry and we mustn't kick that one.
    pub async fn logout(&self, player_id: PlayerId, session_id: i64) {
        let mut online = self.online.lock().await;
        if let Some(entry) = online.get(&player_id) {
            if entry.session_id == session_id {
                online.remove(&player_id);
            }
        }
    }

    /// Snapshot of the currently-online player count. Diagnostic.
    pub async fn online_count(&self) -> usize {
        self.online.lock().await.len()
    }

    /// Persist updated lifetime stats for `player_id`. No-op if the
    /// player is no longer online.
    pub async fn update_stats(
        &self,
        player_id: PlayerId,
        stats: LifetimeStats,
    ) -> Result<(), RegistryError> {
        {
            let online = self.online.lock().await;
            if !online.contains_key(&player_id) {
                return Ok(());
            }
        }

        sqlx::query(
            r#"UPDATE lifetime_stats
               SET hands = ?, voluntary_pf = ?, raised_pf = ?,
                   aggressive_actions = ?, passive_actions = ?,
                   showdowns = ?, chip_delta = ?
               WHERE user_id = ?"#,
        )
        .bind(stats.hands as i64)
        .bind(stats.voluntary_pf as i64)
        .bind(stats.raised_pf as i64)
        .bind(stats.aggressive_actions as i64)
        .bind(stats.passive_actions as i64)
        .bind(stats.showdowns as i64)
        .bind(stats.chip_delta)
        .bind(player_id as i64)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Persist one finished hand: a row in `hands` carrying the full
    /// `FileSink`-compatible event log, plus one row per participating
    /// seat in `hand_seats`. Returns the inserted `hands.id`.
    ///
    /// INVARIANT (mid-hand atomicity): this MUST only be called after
    /// the engine has emitted `HandEnded`. No partial write is possible
    /// — the entire insert (header row + all seat rows) runs inside one
    /// SQLite transaction, so either every row lands or none do. A
    /// server crash mid-hand (before `HandEnded`) MUST leave the DB
    /// indistinguishable from "the hand never started". Enforced by
    /// the call sequence in `crates/poker-server/src/table.rs::run_table`:
    /// `run_one_hand` returns only after `HandEnded`, and `persist_hand`
    /// (→ this function) is the next call. Regression pinned by
    /// `tests/persistence.rs::abort_mid_hand_leaves_db_clean`.
    pub async fn record_hand(
        &self,
        table_id: TableId,
        started_at: i64,
        ended_at: i64,
        log: Vec<u8>,
        seats: &[HandSeatRecord],
    ) -> Result<i64, RegistryError> {
        let _write = self.write_tx.lock().await;
        let mut tx = self.pool.begin().await?;

        let res = sqlx::query(
            "INSERT INTO hands (table_id, started_at, ended_at, log) VALUES (?, ?, ?, ?)",
        )
        .bind(table_id as i64)
        .bind(started_at)
        .bind(ended_at)
        .bind(log)
        .execute(&mut *tx)
        .await?;
        let hand_id = res.last_insert_rowid();

        for s in seats {
            sqlx::query(
                "INSERT INTO hand_seats (hand_id, seat, user_id, chip_delta, sat_out) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(hand_id)
            .bind(s.seat as i64)
            .bind(s.player_id.map(|id| id as i64))
            .bind(s.chip_delta)
            .bind(s.sat_out as i64)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(hand_id)
    }

    /// Fetch one persisted hand by id, including its event log and
    /// per-seat outcome rows. Returns `None` if no hand has that id.
    pub async fn fetch_hand(&self, id: i64) -> Result<Option<HandRecord>, RegistryError> {
        let header: Option<(i64, i64, i64, Vec<u8>)> = sqlx::query_as(
            "SELECT table_id, started_at, ended_at, log FROM hands WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((table_id, started_at, ended_at, log)) = header else {
            return Ok(None);
        };

        let rows: Vec<(i64, Option<i64>, i64, i64)> = sqlx::query_as(
            "SELECT seat, user_id, chip_delta, sat_out FROM hand_seats WHERE hand_id = ? ORDER BY seat",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;
        let seats = rows
            .into_iter()
            .map(|(seat, user_id, chip_delta, sat_out)| HandSeatRecord {
                seat: seat as SeatIndex,
                player_id: user_id.map(|v| v as PlayerId),
                chip_delta: chip_delta as i32,
                sat_out: sat_out != 0,
            })
            .collect();
        Ok(Some(HandRecord {
            id,
            table_id: table_id as TableId,
            started_at,
            ended_at,
            log,
            seats,
        }))
    }

    /// Total number of persisted hands across all tables. Useful for
    /// tests that need to wait until persistence has caught up.
    pub async fn count_hands(&self) -> Result<i64, RegistryError> {
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM hands")
            .fetch_one(&self.pool)
            .await?;
        Ok(n)
    }

    /// Replace the online entry for `player_id`. If a prior entry
    /// exists, flip its revoke flag so the prior reader loop tears
    /// itself down on its next iteration.
    async fn install_online(
        &self,
        player_id: PlayerId,
        session_id: i64,
        username: &str,
    ) -> Arc<AtomicBool> {
        let revoked = Arc::new(AtomicBool::new(false));
        let mut online = self.online.lock().await;
        if let Some(prev) = online.insert(
            player_id,
            OnlineEntry {
                session_id,
                revoked: revoked.clone(),
                username: username.to_string(),
            },
        ) {
            prev.revoked.store(true, Ordering::SeqCst);
        }
        revoked
    }
}

async fn issue_session_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: i64,
    device_label: Option<&str>,
    now: i64,
) -> Result<(i64, String), RegistryError> {
    // Revoke any currently-live sessions for this user.
    sqlx::query(
        "UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;

    let key = new_session_key();
    let res = sqlx::query(
        "INSERT INTO sessions (user_id, key, device_label, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(&key)
    .bind(device_label)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok((res.last_insert_rowid(), key))
}

async fn load_stats_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: i64,
) -> Result<LifetimeStats, RegistryError> {
    let row: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT hands, voluntary_pf, raised_pf, aggressive_actions,
                  passive_actions, showdowns, chip_delta
           FROM lifetime_stats WHERE user_id = ?"#,
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(LifetimeStats {
        hands: row.0 as u64,
        voluntary_pf: row.1 as u64,
        raised_pf: row.2 as u64,
        aggressive_actions: row.3 as u64,
        passive_actions: row.4 as u64,
        showdowns: row.5 as u64,
        chip_delta: row.6,
    })
}

fn hash_password(password: &str) -> Result<String, RegistryError> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string();
    Ok(hash)
}

fn verify_password(password: &str, hash: &str) -> Result<bool, RegistryError> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

fn new_session_key() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let mut s = String::with_capacity(64);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const PW: &str = "hunter2hunter";

    #[tokio::test]
    async fn register_then_authenticate_password_returns_same_id() {
        let reg = Registry::open_in_memory().await.unwrap();

        let r1 = reg
            .register("alice@example.com", "alice", PW, None)
            .await
            .unwrap();
        let id = r1.record.player_id;
        assert_eq!(r1.record.username, "alice");
        assert!(!r1.session_key.is_empty());

        // logout to clear the online flag, then re-auth.
        reg.logout(id, r1.session_id).await;

        let r2 = reg
            .authenticate_password("alice", PW, None)
            .await
            .unwrap();
        assert_eq!(r2.record.player_id, id);
        assert_ne!(r2.session_key, r1.session_key, "new session each auth");
    }

    // Regression: two clients registering at the same instant used to
    // deadlock on a SQLite write-lock upgrade (deferred `BEGIN` + concurrent
    // read-then-write), surfacing to one client as `Rejected { "internal
    // error" }`. Needs the on-disk pool (max_connections > 1) to reproduce;
    // the in-memory pool is single-connection and serialises anyway.
    #[tokio::test]
    async fn concurrent_registration_of_distinct_users_both_succeed() {
        let dir = tempdir().unwrap();
        let reg = std::sync::Arc::new(Registry::open(dir.path()).await.unwrap());

        let a = {
            let reg = std::sync::Arc::clone(&reg);
            tokio::spawn(async move { reg.register("alice@example.com", "alice", PW, None).await })
        };
        let b = {
            let reg = std::sync::Arc::clone(&reg);
            tokio::spawn(async move { reg.register("bob@example.com", "bob", PW, None).await })
        };

        let ra = a.await.unwrap().expect("alice register");
        let rb = b.await.unwrap().expect("bob register");
        assert_ne!(ra.record.player_id, rb.record.player_id);
    }

    #[tokio::test]
    async fn authenticate_by_email_works() {
        let reg = Registry::open_in_memory().await.unwrap();
        let r1 = reg
            .register("alice@example.com", "alice", PW, None)
            .await
            .unwrap();
        reg.logout(r1.record.player_id, r1.session_id).await;

        let r2 = reg
            .authenticate_password("alice@example.com", PW, None)
            .await
            .unwrap();
        assert_eq!(r2.record.player_id, r1.record.player_id);
    }

    #[tokio::test]
    async fn wrong_password_rejects() {
        let reg = Registry::open_in_memory().await.unwrap();
        reg.register("a@b.co", "alice", PW, None).await.unwrap();
        let err = reg
            .authenticate_password("alice", "wrongpassword", None)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::BadCredentials));
    }

    #[tokio::test]
    async fn unknown_user_rejects_with_bad_credentials() {
        let reg = Registry::open_in_memory().await.unwrap();
        let err = reg
            .authenticate_password("nobody", PW, None)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::BadCredentials));
    }

    #[tokio::test]
    async fn duplicate_username_rejects() {
        let reg = Registry::open_in_memory().await.unwrap();
        reg.register("a@b.co", "alice", PW, None).await.unwrap();
        let err = reg
            .register("c@d.co", "alice", PW, None)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::UsernameInUse));
    }

    #[tokio::test]
    async fn duplicate_email_rejects() {
        let reg = Registry::open_in_memory().await.unwrap();
        reg.register("a@b.co", "alice", PW, None).await.unwrap();
        let err = reg
            .register("a@b.co", "bob", PW, None)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::EmailInUse));
    }

    #[tokio::test]
    async fn session_resume_works_until_replaced() {
        let reg = Registry::open_in_memory().await.unwrap();
        let r1 = reg.register("a@b.co", "alice", PW, None).await.unwrap();
        let key = r1.session_key.clone();
        reg.logout(r1.record.player_id, r1.session_id).await;

        let resumed = reg.authenticate_session(&key).await.unwrap();
        assert_eq!(resumed.record.player_id, r1.record.player_id);
        reg.logout(resumed.record.player_id, resumed.session_id).await;

        // Password auth issues a new session and revokes the old one.
        let _r2 = reg.authenticate_password("alice", PW, None).await.unwrap();

        // The original key is now revoked.
        let err = reg.authenticate_session(&key).await.unwrap_err();
        assert!(matches!(err, RegistryError::SessionRevoked));
    }

    #[tokio::test]
    async fn new_login_kicks_prior_online_session() {
        let reg = Registry::open_in_memory().await.unwrap();
        let r1 = reg.register("a@b.co", "alice", PW, None).await.unwrap();
        let prior_revoked = r1.revoked.clone();

        let r2 = reg
            .authenticate_password("alice", PW, None)
            .await
            .unwrap();

        assert!(prior_revoked.load(Ordering::SeqCst));
        assert!(!r2.revoked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn invalid_email_rejects() {
        let reg = Registry::open_in_memory().await.unwrap();
        let err = reg
            .register("not-an-email", "alice", PW, None)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::InvalidEmail));
    }

    #[tokio::test]
    async fn invalid_username_rejects() {
        let reg = Registry::open_in_memory().await.unwrap();
        let err = reg
            .register("a@b.co", "x", PW, None)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::InvalidUsername));
    }

    #[tokio::test]
    async fn short_password_rejects() {
        let reg = Registry::open_in_memory().await.unwrap();
        let err = reg
            .register("a@b.co", "alice", "short", None)
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::InvalidPassword));
    }

    #[tokio::test]
    async fn ids_persist_across_reopen() {
        let dir = tempdir().unwrap();

        let reg = Registry::open(dir.path()).await.unwrap();
        let r1 = reg.register("a@b.co", "alice", PW, None).await.unwrap();
        let id = r1.record.player_id;
        reg.close().await;

        let reg = Registry::open(dir.path()).await.unwrap();
        let r2 = reg.authenticate_password("alice", PW, None).await.unwrap();
        assert_eq!(r2.record.player_id, id);
        reg.close().await;
    }

    #[tokio::test]
    async fn update_stats_persists() {
        let reg = Registry::open_in_memory().await.unwrap();
        let r = reg.register("a@b.co", "alice", PW, None).await.unwrap();

        let new_stats = LifetimeStats {
            hands: 100,
            chip_delta: -42,
            ..LifetimeStats::default()
        };
        reg.update_stats(r.record.player_id, new_stats).await.unwrap();

        // Stats survive a logout / re-auth cycle.
        reg.logout(r.record.player_id, r.session_id).await;
        let r2 = reg.authenticate_password("alice", PW, None).await.unwrap();
        assert_eq!(r2.record.stats.hands, 100);
        assert_eq!(r2.record.stats.chip_delta, -42);
    }
}
