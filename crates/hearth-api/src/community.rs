//! News, game nights with their ready check, emulators, and Firelink, the registry of
//! dedicated game servers.

use serde::{Deserialize, Serialize};

use crate::{
    Id, Timestamp,
    library::{ManifestFile, VersionRef},
};

// ---------- news ----------

/// `GET /api/v1/news`: pinned posts first, then newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsPost {
    pub id: Id,
    pub title: String,
    /// Markdown.
    pub body: String,
    pub pinned: bool,
    pub author: String,
    pub published_at: Timestamp,
}

/// `POST /api/v1/admin/news`. Publishing notifies everyone on the Hearth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishNews {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub pinned: bool,
}

/// `PATCH /api/v1/admin/news/{id}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewsPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
}

// ---------- events ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rsvp {
    Going,
    Maybe,
    /// Can't make it.
    No,
}

/// A game night. `GET /api/v1/events` lists the upcoming ones, soonest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameNight {
    pub id: Id,
    pub title: String,
    pub game_id: Id,
    pub starts_at: Timestamp,
    pub note: String,
    /// Who planned it.
    pub host: String,
    pub answers: Vec<Answer>,
    /// The caller's own answer.
    pub mine: Option<Rsvp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub user_id: Id,
    pub username: String,
    pub rsvp: Rsvp,
}

/// `PUT /api/v1/events/{id}/rsvp`. `null` takes the answer back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetRsvp {
    pub rsvp: Option<Rsvp>,
}

/// `POST /api/v1/admin/events`. Adding one notifies everyone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEvent {
    pub title: String,
    pub game_id: Id,
    pub starts_at: Timestamp,
    #[serde(default)]
    pub note: String,
}

/// Whether a guest's PC is ready for a game night.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ready {
    /// A PC of theirs has the game in the newest version.
    Ready,
    /// They have the game, but an older version.
    NeedsUpdate,
    /// None of their PCs has the game.
    NotKindled,
    /// A download is on its way to one of their PCs.
    GettingReady,
}

/// `GET /api/v1/events/{id}/ready`: the ready check for everyone who said they are going.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyCheck {
    pub event_id: Id,
    pub game_id: Id,
    /// The version everyone needs. `None` while the game has no version ready.
    pub version: Option<VersionRef>,
    pub guests: Vec<GuestReady>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuestReady {
    pub user_id: Id,
    pub username: String,
    pub ready: Ready,
}

// ---------- emulators ----------

/// An emulator, stored once on the Hearth and shared by every game that needs it.
/// `GET /api/v1/emulators`.
///
/// A game that names an emulator ships only its own files. Its default launch option's
/// `path` is the game file, and the client starts `command` with `{rom}` replaced by it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Emulator {
    pub id: Id,
    pub name: String,
    pub version: String,
    /// The systems it plays, for example "PlayStation".
    pub systems: Vec<String>,
    /// How to start a game, with `{rom}` standing for the game file.
    pub command: String,
    /// Its folder inside the Hearth's emulators folder.
    pub folder: String,
    pub bytes: u64,
    /// False while the Hearth is still hashing its files.
    pub ready: bool,
    /// How many games use it.
    pub games: u32,
}

/// `POST /api/v1/admin/emulators`, and `PATCH /api/v1/admin/emulators/{id}` with the same fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmulatorInput {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub systems: Vec<String>,
    pub command: String,
    /// The emulator's folder inside the emulators folder. Left empty, the folder named like the emulator.
    #[serde(default)]
    pub folder: String,
}

/// `GET /api/v1/emulators/{id}/manifest`. Files are fetched from
/// `GET /api/v1/emulators/{id}/files/{index}`, exactly like a game version's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmulatorManifest {
    pub emulator_id: Id,
    pub chunk_size: u64,
    pub bytes: u64,
    pub files: Vec<ManifestFile>,
}

// ---------- Firelink ----------

/// A dedicated game server in the registry. `GET /api/v1/game-servers`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameServer {
    pub id: Id,
    pub game_id: Option<Id>,
    pub name: String,
    /// `host:port`.
    pub address: String,
    pub players: u32,
    pub max_players: u32,
    /// The map or world it runs, when the server says.
    pub map: String,
    pub version: String,
    /// Whether the last check, or the last heartbeat, found it up.
    pub online: bool,
    /// Round trip of the last check, for servers the Hearth checks itself.
    pub ping_ms: Option<u32>,
    pub last_seen_at: Option<Timestamp>,
    /// True for a server that lists itself with heartbeats. It drops off when they stop.
    pub self_listed: bool,
    /// The admin who added it. Empty for a self-listed server.
    pub added_by: String,
}

/// `POST /api/v1/admin/game-servers`. The Hearth checks it every 30 seconds from then on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddGameServer {
    pub game_id: Id,
    pub name: String,
    /// `host:port`.
    pub address: String,
}

/// How often a self-listed server posts a heartbeat. It drops off after three missed ones.
pub const HEARTBEAT_SECONDS: i64 = 30;

/// `POST /api/v1/game-servers/heartbeat`, with the registry token as a bearer token.
/// The token comes from the Firelink page of the admin UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub name: String,
    /// The game on this Hearth, by id or by Steam app id. One of the two is needed.
    #[serde(default)]
    pub game_id: Option<Id>,
    #[serde(default)]
    pub steam_app_id: Option<u32>,
    /// The port players connect to.
    pub port: u16,
    /// The host players connect to. Left out, the address the heartbeat came from.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub players: u32,
    #[serde(default)]
    pub max_players: u32,
    #[serde(default)]
    pub map: String,
    #[serde(default)]
    pub version: String,
}

/// `GET /api/v1/admin/game-servers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub servers: Vec<GameServer>,
    /// What a server sends to list itself. Rotate it with `POST /api/v1/admin/game-servers/token`.
    pub heartbeat_token: String,
}
