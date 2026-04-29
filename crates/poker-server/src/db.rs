//! SQLite connection-pool plumbing.
//!
//! The pool is opened once at startup, runs the embedded migrations,
//! then handed to every component (registry, hand recorder, ...) that
//! needs to touch persistent state. Migrations live in
//! `crates/poker-server/migrations/` and are embedded into the binary
//! at compile time via [`sqlx::migrate!`].

use std::path::Path;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

/// Open (or create) the on-disk pool at `path` and apply migrations.
pub async fn open_pool(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
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
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
