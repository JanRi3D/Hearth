//! Rest: cloud saves per account, and shared worlds that live on the Hearth.
//!
//! Files travel over WebDAV at `/dav`, which rclone speaks. The JSON endpoints here say what
//! is stored and turn an upload into a revision.
//!
//! **Saves of one game** live at `/dav/saves/{game_id}/`:
//! - `upload/` is the caller's working area. Sync the local saves into it, then commit.
//!   After a commit it holds the committed files again, so the next sync sends only changes.
//! - `current/` is the newest revision, read-only. Sync from it to restore.
//! - `rev/{number}/` is one kept revision, read-only.
//!
//! Configure rclone with `type = webdav`, `vendor = rclone`, `url = https://<hearth>/dav` and
//! the session token as `bearer_token`. With that vendor rclone sends modification times, so
//! a sync compares size and time and skips what did not change.

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// How many revisions of a game's saves are kept per account. Older ones are dropped.
pub const KEPT_REVISIONS: u32 = 5;

/// One committed set of saves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    /// Counts up from 1 per account and game.
    pub number: u32,
    /// The PC it came from.
    pub device_id: Option<Id>,
    pub device_name: String,
    pub committed_at: Timestamp,
    pub bytes: u64,
    pub files: u32,
    /// A fingerprint of all files and their paths. Two revisions with the same hash hold
    /// the same saves.
    pub hash: String,
}

/// `GET /api/v1/saves/{game_id}`.
///
/// **Telling a conflict.** A client remembers the `number` and `hash` of the revision it last
/// synced with on this PC. Before a game starts: if `head` is newer than that, another PC
/// rested saves in between. If the local saves are unchanged since the last sync, restore
/// `head`; if they changed too, that is a conflict for the person to decide. After a game:
/// commit with `base` set to the remembered number, and the Hearth refuses with
/// `save_conflict` if `head` has moved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameSaves {
    pub game_id: Id,
    /// The newest revision. `None` when nothing was ever rested for this game.
    pub head: Option<Revision>,
    /// Newest first.
    pub revisions: Vec<Revision>,
}

/// `GET /api/v1/saves`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Saves {
    pub used_bytes: u64,
    pub quota_bytes: u64,
    pub games: Vec<GameSaves>,
}

/// `POST /api/v1/saves/{game_id}/commit`: turn what is in `upload/` into a new revision.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommitSave {
    /// The revision number this PC last synced with, or `None` if it never has.
    #[serde(default)]
    pub base: Option<u32>,
    /// Commit even though `head` moved since `base`. Nothing is lost: the other revision is kept.
    #[serde(default)]
    pub force: bool,
}

/// `POST /api/v1/saves/{game_id}/restore`: make an older revision the newest again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreSave {
    pub revision: u32,
}

// ---------- shared worlds ----------

/// A multiplayer save that lives on the Hearth, so whoever has time can host it.
///
/// Its files are at `/dav/worlds/{id}/current/` (read-only, for members) and
/// `/dav/worlds/{id}/upload/` (for the person hosting it right now).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct World {
    pub id: Id,
    pub game_id: Id,
    pub name: String,
    pub max_players: u32,
    pub created_by: Option<Id>,
    pub members: Vec<WorldMember>,
    /// Counts up each time the world is stored back. 0 means it has no files yet.
    pub revision: u32,
    pub bytes: u64,
    /// Who is hosting it right now. While set, nobody else can.
    pub host: Option<WorldHost>,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldMember {
    pub user_id: Id,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldHost {
    pub user_id: Id,
    pub username: String,
    pub since: Timestamp,
}

/// `POST /api/v1/worlds`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWorld {
    pub game_id: Id,
    pub name: String,
    pub max_players: u32,
}

/// `POST /api/v1/worlds/{id}/members`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddWorldMember {
    pub user_id: Id,
}

/// `POST /api/v1/worlds/{id}/release`: the host stops hosting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseWorld {
    /// Store what is in `upload/` as the world's new state. False gives the world back unchanged.
    #[serde(default)]
    pub store: bool,
    /// A line for the journal, for whoever hosts next.
    #[serde(default)]
    pub note: String,
}

/// One line of a world's journal: a hosting session, or a note on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub id: Id,
    pub user_id: Option<Id>,
    pub username: String,
    pub at: Timestamp,
    /// How long the world was hosted. 0 for a note on its own.
    pub seconds: u64,
    pub note: String,
}

/// `POST /api/v1/worlds/{id}/journal`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalNote {
    pub note: String,
}

/// How often the host says it is still there. A world whose host has been silent for
/// [`WORLD_HOST_TIMEOUT`] seconds can be taken over.
pub const WORLD_HEARTBEAT_SECONDS: i64 = 60;
pub const WORLD_HOST_TIMEOUT: i64 = 300;

// ---------- admin ----------

/// `GET /api/v1/admin/storage`: Rest storage per account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Storage {
    pub quota_bytes: u64,
    pub accounts: Vec<AccountStorage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountStorage {
    pub user_id: Id,
    pub username: String,
    pub used_bytes: u64,
    /// Games with rested saves.
    pub games: u32,
    pub last_rested_at: Option<Timestamp>,
}
