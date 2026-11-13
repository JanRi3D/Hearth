-- Rest: cloud saves and shared worlds. The files are folders in the data folder; these
-- tables say which revisions exist.

CREATE TABLE save_revisions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    game_id      INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    number       INTEGER NOT NULL,
    device_id    INTEGER REFERENCES devices(id) ON DELETE SET NULL,
    device_name  TEXT NOT NULL DEFAULT '',
    bytes        INTEGER NOT NULL,
    files        INTEGER NOT NULL,
    -- Fingerprint of every path and file. Equal hashes mean equal saves.
    hash         TEXT NOT NULL,
    committed_at INTEGER NOT NULL,
    UNIQUE (user_id, game_id, number)
);

CREATE TABLE worlds (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id     INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    max_players INTEGER NOT NULL DEFAULT 4,
    created_by  INTEGER REFERENCES users(id) ON DELETE SET NULL,
    -- 0 until the world has been stored back for the first time.
    revision    INTEGER NOT NULL DEFAULT 0,
    bytes       INTEGER NOT NULL DEFAULT 0,
    -- Set while someone hosts the world. host_beat_at moves with their heartbeat.
    host_id     INTEGER REFERENCES users(id) ON DELETE SET NULL,
    hosted_at   INTEGER,
    host_beat_at INTEGER,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE world_members (
    world_id INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    user_id  INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (world_id, user_id)
) WITHOUT ROWID;

CREATE TABLE world_journal (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    world_id INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    user_id  INTEGER REFERENCES users(id) ON DELETE SET NULL,
    at       INTEGER NOT NULL,
    seconds  INTEGER NOT NULL DEFAULT 0,
    note     TEXT NOT NULL DEFAULT ''
);
