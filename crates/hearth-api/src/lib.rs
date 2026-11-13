//! Wire types for the Hearth API, version 1.
//!
//! Hearth is the server, Bonfire the client. Both depend on this crate so they agree on the
//! JSON that travels between them. `docs/API.md` in the Hearth repository describes the
//! endpoints these types belong to.
//!
//! Conventions that hold everywhere:
//! - Every path starts with [`API_PREFIX`]. Bodies are JSON.
//! - Ids are integers below 2^53, so they survive any JSON parser.
//! - Times are [`Timestamp`]s: Unix seconds, UTC.
//! - Failures are an HTTP status plus an [`ErrorBody`].
//! - Field names are snake_case. Unknown fields must be ignored, so the API can grow.

use serde::{Deserialize, Serialize};

pub mod admin;
pub mod auth;
pub mod community;
pub mod library;
pub mod multiplayer;
pub mod people;
pub mod play;
pub mod saves;
pub mod settings;
pub mod setup;
pub mod social;

pub const API_VERSION: u32 = 1;
pub const API_PREFIX: &str = "/api/v1";

/// Sent with every setup request. The code is printed in the console where Hearth was started.
pub const SETUP_CODE_HEADER: &str = "x-setup-code";
/// The web UI sends this with every request. The server demands it for cookie sessions, which
/// keeps other sites from acting with the admin's cookie.
pub const WEB_UI_HEADER: &str = "x-hearth-ui";
pub const SESSION_COOKIE: &str = "hearth_session";

/// For a patch field that can be cleared: a missing field means "keep" (`None`), an explicit
/// `null` means "clear" (`Some(None)`), and a value means "set" (`Some(Some(value))`).
/// Plain serde reads `null` as a missing field, so such fields name this function.
pub fn cleared_by_null<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

pub type Id = i64;
/// Unix time in seconds, UTC.
pub type Timestamp = i64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub code: ErrorCode,
    /// Written for the person in front of the screen: what happened and what to do next.
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    Internal,
    /// Wrong username or password.
    InvalidCredentials,
    AccountLocked,
    /// The account has two-step sign-in. Send the request again with `totp`.
    TotpRequired,
    TotpInvalid,
    /// This Hearth requires two-step sign-in for admins and this admin has not set it up.
    TotpSetupRequired,
    InviteInvalid,
    UsernameTaken,
    WeakPassword,
    SetupCodeInvalid,
    /// The Hearth has no admin yet. Open it in a browser to run first-time setup.
    SetupRequired,
    /// Setup has already been completed, so the setup endpoints are closed.
    SetupComplete,
    /// The lobby has no free place.
    LobbyFull,
    /// The lobby is closed to newcomers, or needs an invite.
    LobbyLocked,
    /// Another PC rested newer saves for this game. See `saves::GameSaves`.
    SaveConflict,
    /// The saves, or the shared world, do not fit into the space set aside for them.
    QuotaExceeded,
    /// Someone else is hosting the shared world right now.
    WorldLocked,
    /// A code this version of the crate does not know.
    #[serde(other)]
    Unknown,
}

/// `GET /api/v1/hearth`. Public, so a client can show what it is about to join.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HearthInfo {
    pub name: String,
    pub version: String,
    pub api_version: u32,
    pub setup_complete: bool,
    pub join: settings::JoinMode,
    pub tls: settings::TlsMode,
    /// SHA-256 of the certificate in hex, when the Hearth uses its self-signed one. A client
    /// stores it on first connection and refuses a different certificate later.
    pub cert_sha256: Option<String>,
    /// UDP port of the Bonfire relay, when it is switched on.
    pub relay_port: Option<u16>,
    pub time: Timestamp,
}

/// `GET /api/v1/health`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
}

#[cfg(test)]
mod tests {
    use crate::library::GamePatch;

    #[test]
    fn a_patch_tells_clearing_a_field_from_leaving_it_alone() {
        let keep: GamePatch = serde_json::from_str("{}").unwrap();
        let clear: GamePatch = serde_json::from_str(r#"{"year":null,"steam_app_id":null}"#).unwrap();
        let set: GamePatch = serde_json::from_str(r#"{"year":2018,"steam_app_id":427520}"#).unwrap();
        assert_eq!((keep.year, keep.steam_app_id), (None, None));
        assert_eq!((clear.year, clear.steam_app_id), (Some(None), Some(None)));
        assert_eq!((set.year, set.steam_app_id), (Some(Some(2018)), Some(Some(427520))));
        assert_eq!(serde_json::to_string(&clear).unwrap(), r#"{"year":null,"steam_app_id":null}"#, "and it survives the trip back");
    }
}
