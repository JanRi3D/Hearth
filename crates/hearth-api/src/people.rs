//! People on a Hearth: who they are to each other, their devices, and what an admin sees.

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp, auth::Role};

/// Someone on this Hearth, as everyone else sees them. `GET /api/v1/users`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    pub id: Id,
    pub username: String,
    pub role: Role,
    pub last_seen_at: Option<Timestamp>,
}

/// The three privacy switches. `GET` and `PATCH /api/v1/me/privacy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Privacy {
    /// Friends see how long you played what.
    pub share_playtime: bool,
    pub share_achievements: bool,
    /// Friends see what you are playing right now.
    pub share_now_playing: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrivacyPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_playtime: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_achievements: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_now_playing: Option<bool>,
}

/// A PC, or a browser, someone signed in on. `GET /api/v1/devices` lists the caller's own.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: Id,
    pub user_id: Id,
    pub username: String,
    pub name: String,
    pub platform: String,
    pub client: String,
    pub last_seen_at: Timestamp,
    /// Connected to the Hearth right now.
    pub online: bool,
    /// What the device last said it has kindled.
    pub installed: Vec<Installed>,
}

/// A game on a device, and which version of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub game_id: Id,
    pub version_id: Id,
}

/// A download an admin queued for a device. It starts when that device comes online.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedDownload {
    pub id: Id,
    pub device_id: Id,
    pub game_id: Id,
    /// The version to fetch. Always the newest at the time of queueing.
    pub version_id: Id,
    pub state: DownloadState,
    pub queued_at: Timestamp,
    /// For the admin's list. Clients know their own device and look the game up themselves.
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub game_title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Queued,
    Started,
    Done,
    Failed,
}

/// `POST /api/v1/admin/downloads`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueDownload {
    pub device_id: Id,
    pub game_id: Id,
}

/// `POST /api/v1/devices/current/queue/{id}`: the device says how the download is going.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub state: DownloadState,
}

// ---------- admin ----------

/// One row of People and invites. `GET /api/v1/admin/users`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUser {
    pub id: Id,
    pub username: String,
    pub role: Role,
    pub created_at: Timestamp,
    pub last_seen_at: Option<Timestamp>,
    pub online: bool,
    pub playtime_seconds: u64,
    pub devices: u32,
    pub totp_enabled: bool,
    pub locked_until: Option<Timestamp>,
    /// Space their cloud saves take.
    pub save_bytes: u64,
}

/// `PATCH /api/v1/admin/users/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPatch {
    pub role: Role,
}

/// A password reset link. Single use, valid for 24 hours.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetLink {
    pub id: Id,
    pub user_id: Id,
    pub username: String,
    pub expires_at: Timestamp,
    /// The path to append to the Hearth's address, for example `/reset/pA3…`. Only present in
    /// the answer that creates the link: the Hearth keeps a hash and cannot show it again.
    pub path: Option<String>,
}

/// `GET /api/v1/auth/reset/{token}`: who a reset link is for, so the page can say so.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetInfo {
    pub username: String,
}
