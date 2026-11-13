//! Play data: sessions and playtime, achievements, stats, collections and notes. All of it
//! lives on the Hearth, so it follows an account to every PC.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

/// A client sends a heartbeat this often while a game runs. A session without one for
/// [`SESSION_TIMEOUT`] seconds counts as ended at its last heartbeat.
pub const HEARTBEAT_SECONDS: i64 = 60;
pub const SESSION_TIMEOUT: i64 = 180;

/// `POST /api/v1/play/sessions`: a game was started.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartSession {
    pub game_id: Id,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaySession {
    pub id: Id,
    pub game_id: Id,
    pub device_id: Option<Id>,
    pub started_at: Timestamp,
    /// `None` while the game is running.
    pub ended_at: Option<Timestamp>,
    pub seconds: u64,
}

/// How long someone played one game. `GET /api/v1/me/playtime`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playtime {
    pub game_id: Id,
    pub seconds: u64,
    pub sessions: u32,
    pub last_played_at: Timestamp,
}

/// An achievement a game can award. Admins may describe them; games unlock them by `name`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AchievementDef {
    /// The name the game itself uses, for example `iron-throne-1`.
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    /// Not shown to players until they unlock it.
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unlocked {
    pub name: String,
    pub unlocked_at: Timestamp,
}

/// `GET /api/v1/games/{id}/achievements`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Achievements {
    /// Empty when nobody described this game's achievements. Unlocks are still recorded.
    pub defined: Vec<AchievementDef>,
    pub unlocked: Vec<Unlocked>,
}

/// Named numbers a game keeps per player, for example `kills` or `distance`.
/// `GET` and `PUT /api/v1/games/{id}/stats`. A `PUT` sets the stats it names and keeps the rest.
pub type Stats = BTreeMap<String, f64>;

/// `GET /api/v1/collections`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: Id,
    pub name: String,
    pub game_ids: Vec<Id>,
}

/// `POST /api/v1/collections`, and `PATCH /api/v1/collections/{id}` with the fields to change.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Replaces the games in the collection, in this order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_ids: Option<Vec<Id>>,
}

/// `GET` and `PUT /api/v1/games/{id}/note`: free text a player keeps about a game.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Note {
    pub text: String,
    #[serde(default)]
    pub updated_at: Option<Timestamp>,
}

/// `GET /api/v1/users/{id}/profile`: what a friend may see. A section is `None` when the
/// person switched off sharing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub person: crate::people::Person,
    pub playtime: Option<Vec<Playtime>>,
    /// Their latest sessions, newest first. Shared together with playtime.
    pub recent: Option<Vec<PlaySession>>,
    pub achievements: Option<Vec<GameAchievements>>,
    /// The game they are in right now.
    pub now_playing: Option<NowPlaying>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameAchievements {
    pub game_id: Id,
    pub unlocked: Vec<Unlocked>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NowPlaying {
    pub game_id: Id,
    pub since: Timestamp,
}

/// One row of `GET /api/v1/games/{id}/players`: how the people on this Hearth compare in a
/// game. People who do not share playtime are left out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GamePlayer {
    pub user_id: Id,
    pub username: String,
    pub seconds: u64,
    pub last_played_at: Timestamp,
    /// `None` when they do not share achievements.
    pub achievements: Option<u32>,
}
