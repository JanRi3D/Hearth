-- The library: what is on disk (sources), what players see (games and versions), and what a
-- client downloads (manifests).

CREATE TABLE games (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    title         TEXT NOT NULL,
    developer     TEXT NOT NULL DEFAULT '',
    year          INTEGER,
    genre         TEXT NOT NULL DEFAULT '',
    players       TEXT NOT NULL DEFAULT 'single' CHECK (players IN ('single', 'coop', 'online')),
    description   TEXT NOT NULL DEFAULT '',
    igdb_id       INTEGER,
    sgdb_id       INTEGER,
    steam_app_id  INTEGER,
    -- File names inside <data>/art.
    cover         TEXT,
    hero          TEXT,
    logo          TEXT,
    visible       INTEGER NOT NULL DEFAULT 1,
    -- Bonfire networking: the game talks to this Hearth instead of Steam.
    networking    INTEGER NOT NULL DEFAULT 0,
    added_at      INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

-- The files of one version, hashed. Built in the background after a scan.
CREATE TABLE manifests (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Folder, or single file, that the paths below are relative to.
    root         TEXT NOT NULL,
    state        TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'ready', 'failed')),
    bytes        INTEGER NOT NULL DEFAULT 0,
    files        INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL,
    ready_at     INTEGER
);

CREATE TABLE manifest_files (
    manifest_id INTEGER NOT NULL REFERENCES manifests(id) ON DELETE CASCADE,
    idx         INTEGER NOT NULL,
    -- Relative to the manifest's root, with forward slashes.
    path        TEXT NOT NULL,
    size        INTEGER NOT NULL,
    mtime       INTEGER NOT NULL,
    -- BLAKE3 of the whole file, then of every 4 MiB chunk, 32 bytes each.
    hash        BLOB NOT NULL,
    chunks      BLOB NOT NULL,
    PRIMARY KEY (manifest_id, idx)
) WITHOUT ROWID;

-- An entry at the top of a games folder: a folder, an archive, an installer or a disc image.
CREATE TABLE sources (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    path        TEXT NOT NULL UNIQUE,
    folder      TEXT NOT NULL,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('folder', 'archive', 'installer', 'image')),
    bytes       INTEGER NOT NULL DEFAULT 0,
    files       INTEGER NOT NULL DEFAULT 0,
    -- File count, size and newest change. A different signature means the files changed.
    signature   TEXT NOT NULL DEFAULT '',
    -- new: just found. review: waiting for an admin. matched: belongs to a version.
    -- ignored: an admin said to leave it out. missing: gone from disk.
    state       TEXT NOT NULL DEFAULT 'new' CHECK (state IN ('new', 'review', 'matched', 'ignored', 'missing')),
    -- JSON list of candidates for Metadata review.
    candidates  TEXT NOT NULL DEFAULT '[]',
    update_of   INTEGER REFERENCES games(id) ON DELETE SET NULL,
    found_at    INTEGER NOT NULL
);

CREATE TABLE versions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    game_id     INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    source_id   INTEGER REFERENCES sources(id) ON DELETE SET NULL,
    manifest_id INTEGER REFERENCES manifests(id) ON DELETE SET NULL,
    name        TEXT NOT NULL,
    note        TEXT NOT NULL DEFAULT '',
    -- What a client on the version before this one has to download. Filled in by the hasher.
    update_bytes INTEGER,
    added_at    INTEGER NOT NULL
);
CREATE INDEX versions_game ON versions(game_id);

CREATE TABLE launch_options (
    game_id    INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    position   INTEGER NOT NULL,
    name       TEXT NOT NULL,
    path       TEXT NOT NULL,
    args       TEXT NOT NULL DEFAULT '',
    is_default INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (game_id, position)
) WITHOUT ROWID;

CREATE TABLE prerequisites (
    game_id  INTEGER NOT NULL REFERENCES games(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    name     TEXT NOT NULL,
    path     TEXT NOT NULL,
    args     TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (game_id, position)
) WITHOUT ROWID;
