//! What the admin pages read. Everything here needs an admin session.

use serde::{Deserialize, Serialize};

use crate::{Timestamp, library::ScanStatus};

/// `GET /api/v1/admin/overview`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overview {
    pub version: String,
    pub uptime_seconds: u64,
    pub accounts: u32,
    /// People seen within the last five minutes.
    pub online_now: u32,
    /// Signed-in PCs and browsers.
    pub devices: u32,
    pub library: LibraryStats,
    /// People online in each of the last 24 hours, oldest first. The last entry is this hour.
    #[serde(default)]
    pub online_24h: Vec<u32>,
    #[serde(default)]
    pub playing_now: Vec<PlayingNow>,
    /// Open lobbies, and how many of them have traffic going through the relay.
    #[serde(default)]
    pub lobbies: u32,
    #[serde(default)]
    pub relayed_lobbies: u32,
    /// Space all rested saves take, and what is set aside in total: the quota times the number of accounts.
    #[serde(default)]
    pub rest_used_bytes: u64,
    #[serde(default)]
    pub rest_quota_bytes: u64,
    pub services: Vec<ServiceStatus>,
    /// Newest first.
    pub activity: Vec<Activity>,
}

/// The library at a glance, for the Overview.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryStats {
    pub games: u32,
    /// Download size of the current version of every game.
    pub bytes: u64,
    pub hidden: u32,
    pub folders: Vec<String>,
    /// Files waiting in Metadata review.
    pub review: u32,
    pub watching: bool,
    /// Matches at or above this are applied on their own.
    pub threshold: f64,
    pub scan: ScanStatus,
}

/// Someone in a game right now, for the Overview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayingNow {
    pub username: String,
    pub game_title: String,
    pub since: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub detail: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub at: Timestamp,
    pub text: String,
}

/// `GET /api/v1/admin/networking`: Bonfire networking at a glance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Networking {
    pub relay_enabled: bool,
    /// The UDP port the relay is listening on. `None` when it could not bind.
    pub relay_port: Option<u16>,
    /// Traffic through the relay, averaged over the last minute.
    pub relay_bits_per_second: u64,
    pub sessions: u32,
    pub relayed_sessions: u32,
    pub lobbies: Vec<AdminLobby>,
    /// Every shared world on this Hearth.
    #[serde(default)]
    pub worlds: Vec<crate::saves::World>,
    /// Games people play together, with their Bonfire networking switch.
    pub games: Vec<NetworkingGame>,
    pub calls_today: u64,
    /// Calls today that the Hearth answered with an error.
    pub failed_today: u64,
    /// Newest first.
    pub recent_calls: Vec<Call>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminLobby {
    pub id: crate::Id,
    /// The lobby's `name` data, or "<owner>'s lobby".
    pub name: String,
    pub game_title: String,
    pub owner: String,
    pub members: u32,
    pub max_members: u32,
    pub route: crate::multiplayer::Route,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkingGame {
    pub id: crate::Id,
    pub title: String,
    pub networking: bool,
}

/// One request from the Steam API replacement, named the way Steam names it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub at: Timestamp,
    /// For example `ISteamMatchmaking`.
    pub interface: String,
    /// For example `CreateLobby`.
    pub call: String,
    /// The HTTP status the Hearth answered with.
    pub status: u16,
    pub millis: u32,
}

/// `GET /api/v1/admin/maintenance`: backups and updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Maintenance {
    pub version: String,
    /// The newest database backup, if there is one.
    pub backup: Option<Backup>,
    pub backup_folder: String,
    /// What the last look for a newer version found. `None` until an admin asks.
    pub update: Option<UpdateCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backup {
    pub at: Timestamp,
    pub bytes: u64,
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheck {
    /// The newest released version.
    pub latest: String,
    /// True when `latest` is newer than what is running.
    pub available: bool,
    pub checked_at: Timestamp,
}
