//! Games, their versions, and how a client downloads them.
//!
//! A game has one or more versions. Each version has a manifest: the list of its files with
//! a BLAKE3 hash per file and per [`CHUNK_SIZE`] chunk. A client downloads byte ranges,
//! checks them against the manifest, and can ask which chunks differ between two versions.

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// Files are hashed in chunks of this many bytes. The last chunk of a file is shorter.
pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Players {
    Single,
    Coop,
    /// Online, with dedicated servers.
    Online,
}

/// What a game is on the Hearth's disk, which tells a client what to do after downloading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A folder that is ready to run.
    Folder,
    /// A .zip, .7z or .rar to unpack.
    Archive,
    /// An .exe installer to run.
    Installer,
    /// An .iso disc image.
    Image,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestState {
    /// Waiting to be hashed, or being hashed. Not downloadable yet.
    Pending,
    Ready,
    /// The files could not be read. A rescan tries again.
    Failed,
}

/// One game in `GET /api/v1/games`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameSummary {
    pub id: Id,
    pub title: String,
    pub developer: String,
    pub year: Option<i32>,
    pub genre: String,
    pub players: Players,
    /// Paths on this Hearth, for example `/api/v1/art/12-cover.jpg`. `None` means the client
    /// draws its own stand-in.
    pub cover: Option<String>,
    pub hero: Option<String>,
    pub logo: Option<String>,
    pub kind: SourceKind,
    /// Download size of the current version.
    pub bytes: u64,
    /// The newest version that can be downloaded. `None` while the first one is being prepared.
    pub version: Option<VersionRef>,
    /// Whether this game talks to the Hearth instead of Steam (Bonfire networking).
    pub networking: bool,
    /// Set for a game that runs in a shared emulator.
    pub emulator_id: Option<Id>,
    /// False when an admin hides the game from players. Only admins ever see such a game.
    pub visible: bool,
    pub added_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionRef {
    pub id: Id,
    pub name: String,
}

/// `GET /api/v1/games/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    #[serde(flatten)]
    pub summary: GameSummary,
    pub description: String,
    /// The Steam app id, when known. The Steam replacement uses it to find this game.
    pub steam_app_id: Option<u32>,
    pub launch: Vec<LaunchOption>,
    pub prerequisites: Vec<Prerequisite>,
    /// Newest first.
    pub versions: Vec<Version>,
}

/// One way to start the game. Exactly one is the default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchOption {
    pub name: String,
    /// The executable, relative to the game's folder, with forward slashes.
    pub path: String,
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub default: bool,
}

/// An installer inside the game's files that has to run once per PC before the game starts,
/// for example a Visual C++ runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prerequisite {
    pub name: String,
    /// Relative to the game's folder, with forward slashes.
    pub path: String,
    #[serde(default)]
    pub args: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub id: Id,
    pub name: String,
    /// What the admin wants players to know about this version.
    pub note: String,
    pub state: ManifestState,
    pub bytes: u64,
    pub files: u32,
    /// How much a client on the version before this one has to download. `None` for the first.
    pub update_bytes: Option<u64>,
    pub added_at: Timestamp,
}

/// `GET /api/v1/versions/{id}/manifest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version_id: Id,
    pub game_id: Id,
    pub chunk_size: u64,
    pub bytes: u64,
    /// In a stable order. A file's position in this list is its index in download requests.
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Relative to the game's folder, with forward slashes.
    pub path: String,
    pub size: u64,
    /// BLAKE3 of the whole file, 64 hex characters.
    pub hash: String,
    /// BLAKE3 of each chunk, 64 hex characters apiece, one after the other.
    pub chunks: String,
}

/// `GET /api/v1/versions/{to}/diff?from={from}`: what a client that has `from` needs for `to`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diff {
    pub from: Id,
    pub to: Id,
    /// Files of `from` that `to` no longer has.
    pub delete: Vec<String>,
    pub fetch: Vec<DiffFile>,
    /// Total to download.
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffFile {
    /// Index of the file in the manifest of `to`.
    pub index: u32,
    pub path: String,
    pub size: u64,
    /// The chunks that differ. `None` means the whole file: it is new, or shares nothing.
    pub chunks: Option<Vec<u32>>,
    pub bytes: u64,
}

// ---------- admin ----------

/// `GET /api/v1/admin/library`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub games: Vec<GameSummary>,
    pub scan: ScanStatus,
    /// Files waiting in Metadata review.
    pub review: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanStatus {
    pub running: bool,
    /// Files looked at so far in the running scan.
    pub files: u64,
    pub last_finished_at: Option<Timestamp>,
    /// Scans completed since the Hearth started. Goes up by one each time, so a client that
    /// asked for a scan can tell when it is done.
    pub scans_finished: u64,
    /// Versions still to be hashed before every version can be downloaded.
    pub hashing: u32,
}

/// `PATCH /api/v1/admin/games/{id}`. Only the fields that are present change.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GamePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer: Option<String>,
    /// Send `null` to clear it. Leave the field out to keep it.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::cleared_by_null")]
    pub year: Option<Option<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub players: Option<Players>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Send `null` to clear it. Leave the field out to keep it.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::cleared_by_null")]
    pub steam_app_id: Option<Option<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub networking: Option<bool>,
    /// The emulator this game runs in. Send `null` for a game that needs none.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::cleared_by_null")]
    pub emulator_id: Option<Option<Id>>,
    /// Replaces the whole list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch: Option<Vec<LaunchOption>>,
    /// Replaces the whole list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prerequisites: Option<Vec<Prerequisite>>,
}

/// `PATCH /api/v1/admin/versions/{id}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VersionPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A file or folder the Hearth could not place on its own. `GET /api/v1/admin/review`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: Id,
    /// File or folder name inside the games folder.
    pub name: String,
    pub folder: String,
    pub kind: SourceKind,
    pub bytes: u64,
    /// Best first. Empty when no metadata provider knows anything like it.
    pub candidates: Vec<Candidate>,
    /// Set when this looks like a newer version or patch of a game that is already here.
    pub update_of: Option<VersionRef>,
    /// The version name read from the file name, when there is one.
    pub version_guess: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub title: String,
    pub year: Option<i32>,
    /// 0 to 1. At or above the threshold in Settings a match is applied without asking.
    pub confidence: f64,
    pub igdb_id: Option<u64>,
    pub sgdb_id: Option<u64>,
}

/// `POST /api/v1/admin/review/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ReviewAction {
    /// Add it as a new game with the details of the candidate at this position.
    Accept { candidate: usize },
    /// Add it as a new game named after its file, with no details.
    AddPlain,
    /// Add it as a new version of a game that is already in the library.
    Update {
        game_id: Id,
        version: String,
        #[serde(default)]
        note: String,
    },
    /// Leave it out of the library. A rescan will not bring it back.
    Ignore,
}

/// `POST /api/v1/admin/metadata/test`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataTest {
    pub provider: crate::settings::MetadataProvider,
}
