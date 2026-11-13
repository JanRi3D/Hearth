//! Accounts, sign-in, sessions and invites.

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Player,
    Admin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: Id,
    pub username: String,
    pub role: Role,
    pub created_at: Timestamp,
}

/// The PC or browser a session belongs to. A client makes up `client_id` once and keeps it, so
/// signing in again on the same PC reuses the same device.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    #[serde(default)]
    pub client_id: String,
    /// "Desktop", "Laptop", "Steam Deck".
    pub name: String,
    /// "Windows 11", "Linux".
    #[serde(default)]
    pub platform: String,
    /// "Bonfire 0.1.0".
    #[serde(default)]
    pub client: String,
}

/// `POST /api/v1/auth/login`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    /// The 6-digit code, when the account has two-step sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub totp: Option<String>,
    pub device: DeviceInfo,
    /// The web UI sets this. The token then travels in an HttpOnly cookie and `token` is empty.
    #[serde(default)]
    pub cookie: bool,
}

/// `POST /api/v1/auth/register`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    /// `BNFR-XXXX-XXXX`. May be left out when the Hearth lets anyone on its network join.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_code: Option<String>,
    pub username: String,
    pub password: String,
    pub device: DeviceInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResponse {
    /// Send as `Authorization: Bearer <token>`. Shown once; the Hearth keeps only a hash.
    pub token: Option<String>,
    pub expires_at: Option<Timestamp>,
    pub session_id: Id,
    pub device_id: Id,
    pub user: User,
    /// True when this Hearth requires two-step sign-in for admins and this admin has none yet.
    #[serde(default)]
    pub totp_setup_required: bool,
}

/// `GET /api/v1/me`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Me {
    pub user: User,
    pub session_id: Id,
    pub device_id: Option<Id>,
    pub totp_enabled: bool,
    #[serde(default)]
    pub totp_setup_required: bool,
}

/// One signed-in device. `GET /api/v1/auth/sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Id,
    pub user_id: Id,
    pub username: String,
    pub device_id: Option<Id>,
    pub device_name: String,
    pub platform: String,
    pub client: String,
    pub ip: String,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub expires_at: Option<Timestamp>,
    /// The session this request was made with.
    pub current: bool,
}

/// `POST /api/v1/auth/password`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

/// `POST /api/v1/auth/reset`. The token comes from a reset link an admin created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetPassword {
    pub token: String,
    pub new_password: String,
}

/// `POST /api/v1/auth/totp/setup` answers with the secret to put into an authenticator app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpSetup {
    /// Base32, for typing by hand.
    pub secret: String,
    /// `otpauth://` link, for a QR code.
    pub uri: String,
}

/// `POST /api/v1/auth/totp/enable` and `/disable`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpCode {
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invite {
    pub id: Id,
    pub code: String,
    /// `None` means unlimited.
    pub max_uses: Option<u32>,
    pub uses: u32,
    pub expires_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// `POST /api/v1/admin/invites`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInvite {
    /// 1, 5 or `None` for unlimited.
    pub max_uses: Option<u32>,
    /// Days until the code stops working. Defaults to 7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,
}
