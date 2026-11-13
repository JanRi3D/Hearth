-- Accounts, sessions and settings. Times are Unix seconds, UTC.

-- One row. Every option an admin can set, as JSON.
CREATE TABLE settings (
    id   INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);

-- Small facts the Hearth keeps about itself, for example whether setup has been completed.
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

-- AUTOINCREMENT so an id is never handed out twice. Other tables and clients keep them.
CREATE TABLE users (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    username           TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash      TEXT NOT NULL,
    role               TEXT NOT NULL DEFAULT 'player' CHECK (role IN ('player', 'admin')),
    totp_secret        TEXT,
    totp_enabled       INTEGER NOT NULL DEFAULT 0,
    failed_logins      INTEGER NOT NULL DEFAULT 0,
    locked_until       INTEGER,
    share_playtime     INTEGER NOT NULL DEFAULT 1,
    share_achievements INTEGER NOT NULL DEFAULT 1,
    share_now_playing  INTEGER NOT NULL DEFAULT 1,
    created_at         INTEGER NOT NULL,
    last_seen_at       INTEGER,
    -- Set when an admin removes the account. Its saves are purged 30 days later.
    removed_at         INTEGER
);

CREATE TABLE devices (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- Chosen by the client once and kept, so a PC stays the same device across sign-ins.
    client_id    TEXT NOT NULL,
    name         TEXT NOT NULL,
    platform     TEXT NOT NULL DEFAULT '',
    client       TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    UNIQUE (user_id, client_id)
);

CREATE TABLE sessions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    -- SHA-256 of the token. The token itself is shown to the client once and never stored.
    token_hash   BLOB NOT NULL UNIQUE,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id    INTEGER REFERENCES devices(id) ON DELETE SET NULL,
    ip           TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    expires_at   INTEGER
);
CREATE INDEX sessions_user ON sessions(user_id);

CREATE TABLE invites (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    code       TEXT NOT NULL UNIQUE,
    max_uses   INTEGER,
    uses       INTEGER NOT NULL DEFAULT 0,
    expires_at INTEGER,
    created_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE password_resets (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    token_hash BLOB NOT NULL UNIQUE,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER,
    created_at INTEGER NOT NULL
);

-- What the Overview shows under "Recent activity".
CREATE TABLE activity (
    id   INTEGER PRIMARY KEY AUTOINCREMENT,
    at   INTEGER NOT NULL,
    text TEXT NOT NULL
);
