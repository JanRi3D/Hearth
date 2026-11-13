-- Play data, devices' installed games, and the remote download queue.

CREATE TABLE play_sessions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    game_id      INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    device_id    INTEGER REFERENCES devices(id) ON DELETE SET NULL,
    started_at   INTEGER NOT NULL,
    -- Moved forward by every heartbeat. A session that stops beating ended here.
    last_beat_at INTEGER NOT NULL,
    ended_at     INTEGER
);
CREATE INDEX play_sessions_user ON play_sessions(user_id, game_id);

-- Described by an admin. A game may unlock names that nobody described.
CREATE TABLE achievements (
    game_id     INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    title       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    hidden      INTEGER NOT NULL DEFAULT 0,
    position    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (game_id, name)
) WITHOUT ROWID;

CREATE TABLE user_achievements (
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    game_id     INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    unlocked_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, game_id, name)
) WITHOUT ROWID;

CREATE TABLE user_stats (
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    game_id INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    name    TEXT NOT NULL,
    value   REAL NOT NULL,
    PRIMARY KEY (user_id, game_id, name)
) WITHOUT ROWID;

CREATE TABLE collections (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id  INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- "Couch co-op" and "couch co-op" are the same collection to a person.
    name     TEXT NOT NULL COLLATE NOCASE,
    UNIQUE (user_id, name)
);

CREATE TABLE collection_games (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    game_id       INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    position      INTEGER NOT NULL,
    PRIMARY KEY (collection_id, game_id)
) WITHOUT ROWID;

CREATE TABLE game_notes (
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    game_id    INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    text       TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, game_id)
) WITHOUT ROWID;

-- What each device says it has kindled. The ready check for events reads this.
CREATE TABLE device_games (
    device_id  INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    game_id    INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version_id INTEGER NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (device_id, game_id)
) WITHOUT ROWID;

CREATE TABLE download_queue (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    device_id    INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    game_id      INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    version_id   INTEGER NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
    state        TEXT NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'started', 'done', 'failed')),
    requested_by INTEGER REFERENCES users(id) ON DELETE SET NULL,
    queued_at    INTEGER NOT NULL
);

-- How many people were online, sampled every ten minutes, for the chart on the Overview.
CREATE TABLE online_samples (
    at     INTEGER PRIMARY KEY,
    people INTEGER NOT NULL
) WITHOUT ROWID;
