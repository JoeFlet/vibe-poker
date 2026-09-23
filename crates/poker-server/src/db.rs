//! SQLite connection-pool plumbing.
//!
//! The pool is opened once at startup, runs the embedded migrations,
//! then handed to every component (registry, hand recorder, ...) that
//! needs to touch persistent state. Migrations live in
//! `crates/poker-server/migrations/` and are embedded into the binary
//! at compile time via [`sqlx::migrate!`].

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// How long a connection waits for a competing writer to release the
/// database lock before giving up with `SQLITE_BUSY`. Without this,
/// concurrent writers (e.g. two clients registering at the same instant)
/// fail immediately instead of serialising. SQLite only permits one
/// writer at a time even in WAL mode, so this is the mechanism that turns
/// write contention into a short wait rather than an error.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Open (or create) the on-disk pool at `path` and apply migrations.
pub async fn open_pool(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

/// Open a fresh in-memory pool. Used by tests.
///
/// Limited to one connection because each fresh connection to
/// `:memory:` would otherwise see a different empty database.
pub async fn open_pool_in_memory() -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::new()
        .filename(":memory:")
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
