-- Initial schema for poker-server persistent state.
--
-- Step 21a only populates `users` and `lifetime_stats` (Registry replacement).
-- Step 21b adds rows to `user_password` and `sessions` (Argon2id auth + session
-- keys). Step 21c adds rows to `hands` and `hand_seats` (per-hand persistence).
-- Tables are introduced together so later steps don't need new migrations.

CREATE TABLE users (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    email      TEXT,
    username   TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_users_username ON users(username);
CREATE UNIQUE INDEX idx_users_email ON users(email) WHERE email IS NOT NULL;

-- One row per user that has a password credential. OAuth would land in a
-- sibling table without bloating `users` or nulling password_hash.
CREATE TABLE user_password (
    user_id       INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    password_hash TEXT NOT NULL,
    updated_at    INTEGER NOT NULL
);

-- Append-only session log. The "current" session for a user is the newest
-- non-revoked row; issuing a new session revokes the previous one in the
-- same transaction (step 21b).
CREATE TABLE sessions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    key          TEXT NOT NULL UNIQUE,
    device_label TEXT,
    created_at   INTEGER NOT NULL,
    revoked_at   INTEGER
);
CREATE INDEX idx_sessions_user ON sessions(user_id);

CREATE TABLE lifetime_stats (
    user_id            INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    hands              INTEGER NOT NULL DEFAULT 0,
    voluntary_pf       INTEGER NOT NULL DEFAULT 0,
    raised_pf          INTEGER NOT NULL DEFAULT 0,
    aggressive_actions INTEGER NOT NULL DEFAULT 0,
    passive_actions    INTEGER NOT NULL DEFAULT 0,
    showdowns          INTEGER NOT NULL DEFAULT 0,
    chip_delta         INTEGER NOT NULL DEFAULT 0
);

-- `log` is the same length-prefixed msgpack frame format `FileSink` writes,
-- so a server-recorded hand can be replayed by the client unchanged.
CREATE TABLE hands (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    table_id   INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at   INTEGER NOT NULL,
    log        BLOB NOT NULL
);

CREATE TABLE hand_seats (
    hand_id    INTEGER NOT NULL REFERENCES hands(id) ON DELETE CASCADE,
    seat       INTEGER NOT NULL,
    user_id    INTEGER REFERENCES users(id),
    chip_delta INTEGER NOT NULL,
    sat_out    INTEGER NOT NULL,
    PRIMARY KEY (hand_id, seat)
);
