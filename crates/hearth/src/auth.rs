//! Passwords, tokens and sessions, and the extractors handlers use to ask "who is this?".

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::LazyLock,
};

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use axum::{
    extract::{ConnectInfo, FromRequest, FromRequestParts, Request},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hearth_api::{
    ErrorCode, SESSION_COOKIE, WEB_UI_HEADER,
    auth::{DeviceInfo, Role},
    settings::TlsMode,
};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::{
    db::now,
    error::{ApiError, ApiResult},
    state::AppState,
};

// ---------- passwords ----------

/// Argon2id with the crate's defaults. Slow on purpose: call it from `spawn_blocking`.
pub fn hash_password(password: &str) -> String {
    Argon2::default().hash_password(password.as_bytes()).expect("argon2 accepts any password").to_string()
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}

/// Checked against when the username is unknown, so a wrong name takes as long as a wrong password.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| hash_password("no such account"));

pub async fn hash_password_async(password: String) -> ApiResult<String> {
    Ok(tokio::task::spawn_blocking(move || hash_password(&password)).await?)
}

/// `hash` is `None` for an unknown account. The work done is the same either way.
pub async fn verify_password_async(password: String, hash: Option<String>) -> ApiResult<bool> {
    Ok(tokio::task::spawn_blocking(move || match hash {
        Some(hash) => verify_password(&password, &hash),
        None => {
            verify_password(&password, &DUMMY_HASH);
            false
        }
    })
    .await?)
}

pub fn check_username(username: &str) -> ApiResult<String> {
    let username = username.trim();
    let ok = (2..=24).contains(&username.len())
        && username.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !ok {
        return Err(ApiError::bad_request(
            "A username has 2 to 24 characters: letters, digits, dots, dashes and underscores.",
        ));
    }
    Ok(username.to_string())
}

pub fn check_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < 8 {
        return Err(ApiError::coded(
            StatusCode::BAD_REQUEST,
            ErrorCode::WeakPassword,
            "Use at least 8 characters for the password.",
        ));
    }
    if password.len() > 256 {
        return Err(ApiError::bad_request("That password is too long. Keep it under 256 characters."));
    }
    Ok(())
}

// ---------- tokens and codes ----------

/// 256 random bits, as text that is safe in headers and links.
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// No 0, O, 1, I or L, so a code survives being read out loud.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

fn code_group() -> String {
    (0..4).map(|_| CODE_ALPHABET[rand::random_range(0..CODE_ALPHABET.len())] as char).collect()
}

/// `7Q2K-XM4D`
pub fn setup_code() -> String {
    format!("{}-{}", code_group(), code_group())
}

/// `BNFR-7Q2K-XM4D`
pub fn invite_code() -> String {
    format!("BNFR-{}-{}", code_group(), code_group())
}

/// Codes are compared without case, dashes or spaces.
pub fn normalize_code(code: &str) -> String {
    code.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// An invite as stored and shown: `BNFR-XXXX-XXXX`, whatever the person typed around it.
pub fn canonical_invite(code: &str) -> Option<String> {
    let code = normalize_code(code);
    (code.len() == 12 && code.starts_with("BNFR")).then(|| format!("BNFR-{}-{}", &code[4..8], &code[8..12]))
}

// ---------- who is asking ----------

pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => is_private(IpAddr::V4(ip)),
            None => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
        },
    }
}

/// The address a request came from. Behind the owner's proxy that is the last hop the proxy
/// reports, and only a proxy on a private address is believed.
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        if state.settings().tls == TlsMode::Proxy && is_private(peer) {
            let forwarded = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.rsplit(',').next())
                .and_then(|value| value.trim().parse().ok());
            if let Some(ip) = forwarded {
                return Ok(Self(ip));
            }
        }
        Ok(Self(peer))
    }
}

/// True when the browser reached the Hearth over HTTPS, directly or through the proxy.
pub fn is_https(state: &AppState, headers: &HeaderMap) -> bool {
    state.bound().is_some_and(|bound| bound.https)
        || headers.get("x-forwarded-proto").is_some_and(|proto| proto.as_bytes().eq_ignore_ascii_case(b"https"))
}

pub fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim().to_string())
}

fn cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, token)| token.to_string())
}

pub fn session_cookie(token: &str, expires_at: Option<i64>, secure: bool) -> HeaderValue {
    // Ten years stands in for "never": a cookie without Max-Age would end with the browser window.
    let max_age = expires_at.map(|at| (at - now()).max(0)).unwrap_or(10 * 365 * 86400);
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{SESSION_COOKIE}={token}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict{secure}"))
        .expect("a token is plain ASCII")
}

pub fn clear_cookie() -> HeaderValue {
    HeaderValue::from_str(&format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict")).unwrap()
}

/// A signed-in person. Taking this as a handler argument is what makes an endpoint private.
#[derive(Debug, Clone)]
pub struct Auth {
    pub user_id: i64,
    pub username: String,
    pub role: Role,
    pub session_id: i64,
    pub device_id: Option<i64>,
    pub totp_enabled: bool,
}

impl Auth {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

pub fn role_from_db(role: &str) -> Role {
    if role == "admin" { Role::Admin } else { Role::Player }
}

pub fn role_to_db(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Player => "player",
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let (token, via_cookie) = match bearer(&parts.headers) {
            Some(token) => (token, false),
            None => (cookie(&parts.headers).ok_or_else(ApiError::unauthorized)?, true),
        };
        // A browser sends the cookie along with requests another site triggers. Only the web UI
        // can add this header, so a cookie alone never changes anything.
        let changes_something = !matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
        if via_cookie && changes_something && !parts.headers.contains_key(WEB_UI_HEADER) {
            return Err(ApiError::forbidden("This request did not come from the Hearth's own pages."));
        }

        type Row = (i64, i64, Option<i64>, Option<i64>, i64, String, String, bool, Option<i64>);
        let row: Option<Row> = sqlx::query_as(
            "SELECT s.id, s.user_id, s.device_id, s.expires_at, s.last_seen_at, u.username, u.role, u.totp_enabled, u.removed_at
             FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ?",
        )
        .bind(token_hash(&token))
        .fetch_optional(&state.db)
        .await?;
        let Some((session_id, user_id, device_id, expires_at, last_seen_at, username, role, totp_enabled, removed_at)) = row
        else {
            return Err(ApiError::unauthorized());
        };
        let now = now();
        if removed_at.is_some() || expires_at.is_some_and(|at| at <= now) {
            sqlx::query("DELETE FROM sessions WHERE id = ?").bind(session_id).execute(&state.db).await?;
            return Err(ApiError::unauthorized());
        }

        // "Last seen" is good to the minute. Writing it on every request would be wasted work.
        if now - last_seen_at >= 60 {
            let ClientIp(ip) = ClientIp::from_request_parts(parts, state).await?;
            sqlx::query("UPDATE sessions SET last_seen_at = ?, ip = ? WHERE id = ?")
                .bind(now)
                .bind(ip.to_string())
                .bind(session_id)
                .execute(&state.db)
                .await?;
            sqlx::query("UPDATE users SET last_seen_at = ? WHERE id = ?").bind(now).bind(user_id).execute(&state.db).await?;
            if let Some(device_id) = device_id {
                sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?").bind(now).bind(device_id).execute(&state.db).await?;
            }
        }

        Ok(Self { user_id, username, role: role_from_db(&role), session_id, device_id, totp_enabled })
    }
}

/// A signed-in admin. When the Hearth requires two-step sign-in for admins, one without it
/// can reach nothing but the endpoints that set it up, which take [`Auth`].
#[derive(Debug, Clone)]
pub struct Admin(pub Auth);

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let auth = Auth::from_request_parts(parts, state).await?;
        if !auth.is_admin() {
            return Err(ApiError::forbidden("Only an admin of this Hearth can do that."));
        }
        if state.settings().admin_totp_required && !auth.totp_enabled {
            return Err(ApiError::coded(
                StatusCode::FORBIDDEN,
                ErrorCode::TotpSetupRequired,
                "This Hearth requires two-step sign-in for admins. Set it up to continue.",
            ));
        }
        Ok(Self(auth))
    }
}

// ---------- sessions ----------

pub struct NewSession {
    pub token: String,
    pub session_id: i64,
    pub device_id: i64,
    pub expires_at: Option<i64>,
    /// The Hearth had not seen this device on this account before.
    pub new_device: bool,
}

fn clip(text: &str, max: usize) -> String {
    text.trim().chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Signs a person in on a device and returns the token, which is not stored anywhere.
pub async fn create_session(state: &AppState, user_id: i64, device: &DeviceInfo, ip: IpAddr) -> ApiResult<NewSession> {
    let now = now();
    let mut client_id = clip(&device.client_id, 64);
    if client_id.is_empty() {
        client_id = new_token();
    }
    let mut name = clip(&device.name, 48);
    if name.is_empty() {
        name = "Unnamed device".into();
    }
    let known: Option<i64> =
        sqlx::query_scalar("SELECT id FROM devices WHERE user_id = ? AND client_id = ?").bind(user_id).bind(&client_id).fetch_optional(&state.db).await?;
    let device_id: i64 = sqlx::query_scalar(
        "INSERT INTO devices (user_id, client_id, name, platform, client, created_at, last_seen_at) VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (user_id, client_id) DO UPDATE SET name = excluded.name, platform = excluded.platform,
             client = excluded.client, last_seen_at = excluded.last_seen_at
         RETURNING id",
    )
    .bind(user_id)
    .bind(client_id)
    .bind(name)
    .bind(clip(&device.platform, 48))
    .bind(clip(&device.client, 48))
    .bind(now)
    .bind(now)
    .fetch_one(&state.db)
    .await?;

    // One session per device: signing in again on a device ends the session it had.
    let ended: Vec<i64> = sqlx::query_scalar("DELETE FROM sessions WHERE user_id = ? AND device_id = ? RETURNING id").bind(user_id).bind(device_id).fetch_all(&state.db).await?;
    ended.into_iter().for_each(|id| state.hub.kick_session(id));

    let days = state.settings().session_days as i64;
    let expires_at = (days > 0).then(|| now + days * 86400);
    let token = new_token();
    let session_id: i64 = sqlx::query_scalar(
        "INSERT INTO sessions (token_hash, user_id, device_id, ip, created_at, last_seen_at, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(token_hash(&token))
    .bind(user_id)
    .bind(device_id)
    .bind(ip.to_string())
    .bind(now)
    .bind(now)
    .bind(expires_at)
    .fetch_one(&state.db)
    .await?;
    sqlx::query("UPDATE users SET last_seen_at = ? WHERE id = ?").bind(now).bind(user_id).execute(&state.db).await?;
    Ok(NewSession { token, session_id, device_id, expires_at, new_device: known.is_none() })
}

// ---------- JSON bodies ----------

/// `axum::Json`, except that a body the Hearth cannot read is answered in the API's own error format.
pub struct ApiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::bad_request(rejection.body_text())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_hash_to_argon2id_and_verify() {
        let hash = hash_password("correct horse");
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong horse", &hash));
        assert!(!verify_password("correct horse", "not a hash"));
    }

    #[test]
    fn codes_have_the_documented_shape() {
        let setup = setup_code();
        assert_eq!(setup.len(), 9);
        assert_eq!(setup.as_bytes()[4], b'-');
        let invite = invite_code();
        assert!(invite.starts_with("BNFR-") && invite.len() == 14);
        assert_eq!(canonical_invite(" bnfr 7q2k xm4d ").as_deref(), Some("BNFR-7Q2K-XM4D"));
        assert_eq!(canonical_invite("7Q2K-XM4D"), None);
        assert_eq!(normalize_code("7q2k-xm4d"), "7Q2KXM4D");
    }

    #[test]
    fn tokens_are_long_and_never_repeat() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 43);
        assert_ne!(a, b);
        assert_eq!(token_hash(&a).len(), 32);
    }

    #[test]
    fn usernames_and_passwords_are_checked() {
        assert_eq!(check_username(" mika ").unwrap(), "mika");
        assert!(check_username("m").is_err());
        assert!(check_username("mi ka").is_err());
        assert!(check_password("1234567").is_err());
        assert!(check_password("12345678").is_ok());
    }

    #[test]
    fn private_addresses_are_recognised() {
        for ip in ["127.0.0.1", "192.168.1.20", "10.0.0.5", "::1", "fd00::1", "::ffff:192.168.1.2"] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "2001:4860:4860::8888"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }
}
