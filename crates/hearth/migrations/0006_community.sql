-- News, game nights, emulators and the Firelink server registry.

CREATE TABLE news (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,
    pinned       INTEGER NOT NULL DEFAULT 0,
    author_id    INTEGER REFERENCES users(id) ON DELETE SET NULL,
    published_at INTEGER NOT NULL
);

CREATE TABLE events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    title       TEXT NOT NULL,
    game_id     INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    starts_at   INTEGER NOT NULL,
    note        TEXT NOT NULL DEFAULT '',
    created_by  INTEGER REFERENCES users(id) ON DELETE SET NULL,
    -- Set once the "starts in an hour" notification went out.
    reminded_at INTEGER,
    created_at  INTEGER NOT NULL
);

CREATE TABLE event_rsvps (
    event_id INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    user_id  INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    rsvp     TEXT NOT NULL CHECK (rsvp IN ('going', 'maybe', 'no')),
    PRIMARY KEY (event_id, user_id)
) WITHOUT ROWID;

CREATE TABLE emulators (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    version     TEXT NOT NULL DEFAULT '',
    -- JSON list of system names.
    systems     TEXT NOT NULL DEFAULT '[]',
    -- How to start a game. {rom} stands for the game file.
    command     TEXT NOT NULL,
    -- Its folder inside the emulators folder.
    folder      TEXT NOT NULL,
    manifest_id INTEGER REFERENCES manifests(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL
);

-- A game for an older system points at an emulator instead of carrying its own copy.
ALTER TABLE games ADD COLUMN emulator_id INTEGER REFERENCES emulators(id) ON DELETE SET NULL;

CREATE TABLE game_servers (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id      INTEGER REFERENCES games(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    address      TEXT NOT NULL,
    -- NULL for a server that lists itself with heartbeats.
    added_by     INTEGER REFERENCES users(id) ON DELETE SET NULL,
    self_listed  INTEGER NOT NULL DEFAULT 0,
    players      INTEGER NOT NULL DEFAULT 0,
    max_players  INTEGER NOT NULL DEFAULT 0,
    map          TEXT NOT NULL DEFAULT '',
    version      TEXT NOT NULL DEFAULT '',
    online       INTEGER NOT NULL DEFAULT 0,
    ping_ms      INTEGER,
    last_seen_at INTEGER,
    created_at   INTEGER NOT NULL,
    UNIQUE (self_listed, name, address)
);
