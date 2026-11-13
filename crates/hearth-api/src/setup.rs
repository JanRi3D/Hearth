//! First-time setup. Every request carries the setup code in the [`crate::SETUP_CODE_HEADER`]
//! header, and every endpoint answers `setup_complete` once an admin exists.

use serde::{Deserialize, Serialize};

use crate::{Timestamp, settings::Settings};

/// `POST /api/v1/setup/begin` checks the code and answers with what the wizard starts from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupDefaults {
    pub settings: Settings,
    pub data_folder: String,
}

/// `POST /api/v1/setup/complete`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupDone {
    /// Where the Hearth answers from now on, for example `https://home.lan:7300`.
    pub url: String,
    /// What friends type into Bonfire, for example `home.lan:7300`.
    pub address: String,
    pub invite_code: String,
    pub invite_max_uses: u32,
    pub invite_expires_at: Timestamp,
    /// True when address, port or encryption changed, so the browser has to open `url` and the
    /// admin signs in there. False when the admin is already signed in on this page.
    pub moved: bool,
    /// When `moved`: a one-time token, good for two minutes, for `POST /api/v1/auth/handoff`
    /// at the new address. It spares the admin typing the password again.
    #[serde(default)]
    pub handoff: Option<String>,
}

/// `POST /api/v1/auth/handoff`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub token: String,
}
