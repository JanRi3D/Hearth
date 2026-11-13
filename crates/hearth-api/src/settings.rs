//! Everything an admin can configure. Stored in the Hearth's database, never in a file.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// HTTPS with a certificate the Hearth makes itself.
    SelfSigned,
    /// HTTPS with a Let's Encrypt certificate for a public domain name.
    LetsEncrypt,
    /// Plain HTTP. The owner's own proxy adds HTTPS.
    Proxy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinMode {
    /// A new account needs an invite code.
    Invite,
    /// Anyone connecting from a private network address may create an account.
    Network,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: "home".into(),
            address: String::new(),
            port: 7300,
            tls: TlsMode::SelfSigned,
            join: JoinMode::Invite,
            games_folders: Vec::new(),
            emulators_folder: String::new(),
            watch_folders: true,
            match_threshold: 0.85,
            igdb_enabled: true,
            igdb_client_id: String::new(),
            igdb_client_secret: String::new(),
            igdb_client_secret_set: false,
            sgdb_enabled: true,
            sgdb_api_key: String::new(),
            sgdb_api_key_set: false,
            relay_enabled: true,
            relay_port: 7301,
            networking_default: true,
            save_quota_gb: 5,
            admin_totp_required: false,
            session_days: 30,
            lockout_enabled: true,
            backup_nightly: true,
            backup_time: "04:00".into(),
        }
    }
}

/// The settings record. Secrets are never sent back: see the `*_set` fields. A field that is
/// missing from JSON takes its default, so older and newer versions read each other's records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub name: String,
    /// Host name or IP that people type to reach this Hearth, without scheme or port.
    pub address: String,
    pub port: u16,
    pub tls: TlsMode,
    pub join: JoinMode,

    pub games_folders: Vec<String>,
    pub emulators_folder: String,
    pub watch_folders: bool,
    /// Matches at or above this confidence are applied on their own: 0.85, 0.90 or 0.95.
    pub match_threshold: f64,

    pub igdb_enabled: bool,
    pub igdb_client_id: String,
    /// Write-only. Empty in responses; `igdb_client_secret_set` says whether one is stored.
    pub igdb_client_secret: String,
    pub igdb_client_secret_set: bool,
    pub sgdb_enabled: bool,
    /// Write-only, like `igdb_client_secret`.
    pub sgdb_api_key: String,
    pub sgdb_api_key_set: bool,

    pub relay_enabled: bool,
    pub relay_port: u16,
    /// Whether games added from now on use Bonfire networking.
    pub networking_default: bool,
    /// Cloud save space per person in GB: 2, 5 or 10.
    pub save_quota_gb: u32,

    pub admin_totp_required: bool,
    /// Sessions end this many days after sign-in: 7, 30 or 90. 0 means never.
    pub session_days: u32,
    /// Lock an account for 15 minutes after 5 wrong passwords.
    pub lockout_enabled: bool,

    pub backup_nightly: bool,
    /// Local time of the nightly backup, "HH:MM".
    pub backup_time: String,
}

/// `PATCH /api/v1/admin/settings`. Only the fields that are present change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub join: Option<JoinMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub games_folders: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emulators_folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_folders: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_threshold: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub igdb_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub igdb_client_id: Option<String>,
    /// An empty string removes the stored secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub igdb_client_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sgdb_enabled: Option<bool>,
    /// An empty string removes the stored key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sgdb_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub networking_default: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_quota_gb: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admin_totp_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lockout_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_nightly: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_time: Option<String>,
}

/// `GET /api/v1/admin/settings` wraps the settings with what the admin cannot change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsView {
    pub settings: Settings,
    /// Fixed when the Hearth starts, with `--data`.
    pub data_folder: String,
    pub version: String,
}

/// What a quick look into a folder found. `POST /api/v1/admin/folders/scan` and the setup twin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderScan {
    pub path: String,
    pub exists: bool,
    pub readable: bool,
    /// Entries that look like games: folders, archives, installers and disc images.
    pub games: u32,
    pub bytes: u64,
    /// False when the folder was too large to add up in time, so `bytes` is a lower bound.
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderScanRequest {
    pub path: String,
}

/// One level of the server's own directory tree. Folders only, never files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Browse {
    /// Empty at the top, where `dirs` are the drives (Windows) or `/` (elsewhere).
    pub path: String,
    pub parent: Option<String>,
    pub dirs: Vec<BrowseDir>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowseDir {
    pub name: String,
    pub path: String,
}

/// Which metadata provider a key test is for. `POST /api/v1/admin/metadata/test`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataProvider {
    Igdb,
    Sgdb,
}
