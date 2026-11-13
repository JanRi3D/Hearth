//! Signing in and out, creating an account with an invite, and managing sessions.

use std::{net::IpAddr, time::Duration};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use hearth_api::{
    ErrorCode,
    auth::{ChangePassword, DeviceInfo, LoginRequest, LoginResponse, Me, RegisterRequest, Session, TotpCode, TotpSetup, User},
    settings::JoinMode,
    setup::Handoff,
};

use crate::{
    auth::{self, ApiJson, Auth, ClientIp, role_from_db},
    db::{self, now},
    error::{ApiError, ApiResult},
    state::AppState,
};

const LOCK_AFTER: i64 = 5;
const LOCK_SECONDS: i64 = 15 * 60;

fn invalid_credentials() -> ApiError {
    ApiError::coded(StatusCode::UNAUTHORIZED, ErrorCode::InvalidCredentials, "The username or password is not right.")
}

async fn load_user(state: &AppState, user_id: i64) -> ApiResult<User> {
    let (id, username, role, created_at): (i64, String, String, i64) =
        sqlx::query_as("SELECT id, username, role, created_at FROM users WHERE id = ?").bind(user_id).fetch_one(&state.db).await?;
    Ok(User { id, username, role: role_from_db(&role), created_at })
}

/// Opens a session and builds the answer. With `cookie`, the token goes into an HttpOnly cookie
/// and stays out of the body, where a script could read it.
async fn sign_in(
    state: &AppState,
    headers: &HeaderMap,
    user_id: i64,
    device: &DeviceInfo,
    ip: IpAddr,
    cookie: bool,
) -> ApiResult<Response> {
    let had_sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = ?").bind(user_id).fetch_one(&state.db).await?;
    let session = auth::create_session(state, user_id, device, ip).await?;
    if session.new_device && had_sessions > 0 {
        // The devices already signed in hear about it, so a stolen password does not go unnoticed.
        let name = if device.name.trim().is_empty() { "a new device" } else { device.name.trim() };
        let title = format!("New sign-in to {} from {name}", state.settings().name);
        let body = "If that was not you, change your password. That signs every other device out.";
        super::social::notify(state, super::social::To::User(user_id), hearth_api::social::NotificationKind::SignIn, title, body, &[("device_id", session.device_id)]).await;
    }
    let user = load_user(state, user_id).await?;
    let totp_enabled: bool = sqlx::query_scalar("SELECT totp_enabled FROM users WHERE id = ?").bind(user_id).fetch_one(&state.db).await?;
    let mut body = LoginResponse {
        token: Some(session.token.clone()),
        expires_at: session.expires_at,
        session_id: session.session_id,
        device_id: session.device_id,
        totp_setup_required: user.role == hearth_api::auth::Role::Admin && state.settings().admin_totp_required && !totp_enabled,
        user,
    };
    if !cookie {
        return Ok(Json(body).into_response());
    }
    body.token = None;
    let cookie = auth::session_cookie(&session.token, session.expires_at, auth::is_https(state, headers));
    Ok(([(header::SET_COOKIE, cookie)], Json(body)).into_response())
}

pub async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(request): ApiJson<LoginRequest>,
) -> ApiResult<Response> {
    state.require_setup()?;
    // Lockout slows guessing at one account. This slows trying one password on many.
    let window = Duration::from_secs(300);
    state.limiter.check("login", ip, 30, window)?;

    type Row = (i64, String, String, i64, Option<i64>, bool, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, username, password_hash, failed_logins, locked_until, totp_enabled, totp_secret
         FROM users WHERE username = ? AND removed_at IS NULL",
    )
    .bind(request.username.trim())
    .fetch_optional(&state.db)
    .await?;
    let Some((user_id, username, password_hash, failed_logins, locked_until, totp_enabled, totp_secret)) = row else {
        auth::verify_password_async(request.password, None).await?;
        state.limiter.fail("login", ip, window);
        return Err(invalid_credentials());
    };

    let now = now();
    if let Some(until) = locked_until.filter(|until| *until > now) {
        let minutes = (until - now + 59) / 60;
        return Err(ApiError::coded(
            StatusCode::FORBIDDEN,
            ErrorCode::AccountLocked,
            format!("Too many wrong passwords. This account unlocks in {minutes} min, or sooner if an admin sends a reset link."),
        ));
    }

    let password_ok = auth::verify_password_async(request.password, Some(password_hash)).await?;
    let totp_ok = match (totp_enabled, &totp_secret, &request.totp) {
        (true, Some(secret), Some(code)) => crate::totp::verify(secret, code, now),
        (true, _, _) => false,
        (false, _, _) => true,
    };
    if password_ok && totp_enabled && request.totp.is_none() {
        // The password was right, so asking for the second step gives nothing away.
        return Err(ApiError::coded(
            StatusCode::UNAUTHORIZED,
            ErrorCode::TotpRequired,
            "Enter the 6-digit code from your authenticator app.",
        ));
    }
    if !password_ok || !totp_ok {
        state.limiter.fail("login", ip, window);
        let failures = failed_logins + 1;
        if state.settings().lockout_enabled && failures >= LOCK_AFTER {
            sqlx::query("UPDATE users SET failed_logins = 0, locked_until = ? WHERE id = ?")
                .bind(now + LOCK_SECONDS)
                .bind(user_id)
                .execute(&state.db)
                .await?;
            db::log_activity(&state.db, format!("{username} was locked for 15 minutes after 5 wrong passwords")).await;
        } else {
            sqlx::query("UPDATE users SET failed_logins = ? WHERE id = ?").bind(failures).bind(user_id).execute(&state.db).await?;
        }
        return Err(if password_ok {
            ApiError::coded(StatusCode::UNAUTHORIZED, ErrorCode::TotpInvalid, "That code is not right. Codes change every 30 seconds.")
        } else {
            invalid_credentials()
        });
    }

    sqlx::query("UPDATE users SET failed_logins = 0, locked_until = NULL WHERE id = ?").bind(user_id).execute(&state.db).await?;
    let response = sign_in(&state, &headers, user_id, &request.device, ip, request.cookie).await?;
    let device = if request.device.name.trim().is_empty() { "a new device" } else { request.device.name.trim() };
    db::log_activity(&state.db, format!("{username} signed in from {device}")).await;
    Ok(response)
}

/// Trades the one-time token from the end of setup for a session cookie.
pub async fn handoff(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(request): ApiJson<Handoff>,
) -> ApiResult<Response> {
    let window = Duration::from_secs(300);
    state.limiter.check("handoff", ip, 10, window)?;
    let Some(user_id) = state.take_handoff(&request.token) else {
        state.limiter.fail("handoff", ip, window);
        return Err(ApiError::unauthorized());
    };
    let device = DeviceInfo { name: "Browser".into(), client: "Web admin".into(), ..Default::default() };
    sign_in(&state, &headers, user_id, &device, ip, true).await
}

pub async fn register(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(request): ApiJson<RegisterRequest>,
) -> ApiResult<Response> {
    state.require_setup()?;
    let window = Duration::from_secs(600);
    state.limiter.check("invite", ip, 10, window)?;
    let username = auth::check_username(&request.username)?;
    auth::check_password(&request.password)?;
    let invite_invalid = |message: &str| {
        state.limiter.fail("invite", ip, window);
        ApiError::coded(StatusCode::FORBIDDEN, ErrorCode::InviteInvalid, message)
    };

    let invite = match request.invite_code.as_deref().map(str::trim).filter(|code| !code.is_empty()) {
        Some(code) => Some(auth::canonical_invite(code).ok_or_else(|| invite_invalid("Invite codes look like BNFR-7Q2K-XM4D."))?),
        None if state.settings().join == JoinMode::Network && auth::is_private(ip) => None,
        None => return Err(invite_invalid("This Hearth needs an invite code. Ask whoever runs it for one.")),
    };
    let password_hash = auth::hash_password_async(request.password).await?;

    let now = now();
    let mut tx = state.db.begin().await?;
    if let Some(code) = &invite {
        // Counting the use and checking the limits is one statement, so a code cannot be overused.
        let used = sqlx::query(
            "UPDATE invites SET uses = uses + 1
             WHERE code = ? AND (max_uses IS NULL OR uses < max_uses) AND (expires_at IS NULL OR expires_at > ?)",
        )
        .bind(code)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if used.rows_affected() != 1 {
            return Err(invite_invalid("That invite code is used up, expired or mistyped. Ask for a new one."));
        }
    }
    let inserted = sqlx::query_scalar::<_, i64>("INSERT INTO users (username, password_hash, created_at) VALUES (?, ?, ?) RETURNING id")
        .bind(&username)
        .bind(password_hash)
        .bind(now)
        .fetch_one(&mut *tx)
        .await;
    let user_id = match inserted {
        Ok(id) => id,
        Err(sqlx::Error::Database(err)) if err.is_unique_violation() => {
            return Err(ApiError::coded(
                StatusCode::CONFLICT,
                ErrorCode::UsernameTaken,
                format!("Someone on this Hearth is already called {username}. Pick another name."),
            ));
        }
        Err(err) => return Err(err.into()),
    };
    tx.commit().await?;

    db::log_activity(&state.db, format!("{username} joined with {}", if invite.is_some() { "an invite" } else { "no invite, from this network" })).await;
    sign_in(&state, &headers, user_id, &request.device, ip, false).await
}

pub async fn logout(State(state): State<AppState>, auth: Auth) -> ApiResult<Response> {
    sqlx::query("DELETE FROM sessions WHERE id = ?").bind(auth.session_id).execute(&state.db).await?;
    state.hub.kick_session(auth.session_id);
    Ok(([(header::SET_COOKIE, auth::clear_cookie())], StatusCode::NO_CONTENT).into_response())
}

pub async fn me(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Me>> {
    Ok(Json(Me {
        user: load_user(&state, auth.user_id).await?,
        session_id: auth.session_id,
        device_id: auth.device_id,
        totp_enabled: auth.totp_enabled,
        totp_setup_required: auth.is_admin() && state.settings().admin_totp_required && !auth.totp_enabled,
    }))
}

/// The caller's own sessions. Admins see everyone's under `/admin/sessions`.
pub async fn sessions(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Session>>> {
    Ok(Json(list_sessions(&state, Some(auth.user_id), auth.session_id).await?))
}

pub async fn list_sessions(state: &AppState, user_id: Option<i64>, current: i64) -> ApiResult<Vec<Session>> {
    type Row = (i64, i64, String, Option<i64>, Option<String>, Option<String>, Option<String>, String, i64, i64, Option<i64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.user_id, u.username, s.device_id, d.name, d.platform, d.client, s.ip, s.created_at, s.last_seen_at, s.expires_at
         FROM sessions s JOIN users u ON u.id = s.user_id LEFT JOIN devices d ON d.id = s.device_id
         WHERE (?1 IS NULL OR s.user_id = ?1) AND (s.expires_at IS NULL OR s.expires_at > ?2)
         ORDER BY s.last_seen_at DESC",
    )
    .bind(user_id)
    .bind(now())
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, user_id, username, device_id, name, platform, client, ip, created_at, last_seen_at, expires_at)| Session {
            id,
            user_id,
            username,
            device_id,
            device_name: name.unwrap_or_else(|| "Unknown device".into()),
            platform: platform.unwrap_or_default(),
            client: client.unwrap_or_default(),
            ip,
            created_at,
            last_seen_at,
            expires_at,
            current: id == current,
        })
        .collect())
}

/// Signs a device out. People may end their own sessions, admins anyone's.
pub async fn revoke_session(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let owner: Option<i64> = sqlx::query_scalar("SELECT user_id FROM sessions WHERE id = ?").bind(id).fetch_optional(&state.db).await?;
    match owner {
        Some(owner) if owner == auth.user_id || auth.is_admin() => {
            sqlx::query("DELETE FROM sessions WHERE id = ?").bind(id).execute(&state.db).await?;
            state.hub.kick_session(id);
            Ok(StatusCode::NO_CONTENT)
        }
        _ => Err(ApiError::not_found("That session does not exist, or it has already ended.")),
    }
}

/// Changing the password signs every other device out.
pub async fn change_password(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<ChangePassword>) -> ApiResult<StatusCode> {
    auth::check_password(&request.new_password)?;
    let current: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?").bind(auth.user_id).fetch_one(&state.db).await?;
    if !auth::verify_password_async(request.current_password, Some(current)).await? {
        return Err(ApiError::coded(StatusCode::FORBIDDEN, ErrorCode::InvalidCredentials, "The current password is not right."));
    }
    let hash = auth::hash_password_async(request.new_password).await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(hash).bind(auth.user_id).execute(&state.db).await?;
    let ended: Vec<i64> = sqlx::query_scalar("DELETE FROM sessions WHERE user_id = ? AND id != ? RETURNING id").bind(auth.user_id).bind(auth.session_id).fetch_all(&state.db).await?;
    ended.into_iter().for_each(|id| state.hub.kick_session(id));
    Ok(StatusCode::NO_CONTENT)
}

// ---------- two-step sign-in ----------

fn totp_invalid() -> ApiError {
    ApiError::coded(StatusCode::FORBIDDEN, ErrorCode::TotpInvalid, "That code is not right. Codes change every 30 seconds.")
}

/// Step one: a fresh secret for the authenticator app. Nothing is switched on yet.
pub async fn totp_setup(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<TotpSetup>> {
    if auth.totp_enabled {
        return Err(ApiError::conflict("Two-step sign-in is already on for this account."));
    }
    let secret = crate::totp::new_secret();
    sqlx::query("UPDATE users SET totp_secret = ? WHERE id = ?").bind(&secret).bind(auth.user_id).execute(&state.db).await?;
    Ok(Json(TotpSetup { uri: crate::totp::uri(&secret, &auth.username, &state.settings().name), secret }))
}

/// Step two: a code from the app proves it took the secret, and two-step sign-in is on.
pub async fn totp_enable(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<TotpCode>) -> ApiResult<StatusCode> {
    let secret: Option<String> = sqlx::query_scalar("SELECT totp_secret FROM users WHERE id = ?").bind(auth.user_id).fetch_one(&state.db).await?;
    let secret = secret.ok_or_else(|| ApiError::bad_request("Start the setup of two-step sign-in first."))?;
    if !crate::totp::verify(&secret, &request.code, now()) {
        return Err(totp_invalid());
    }
    sqlx::query("UPDATE users SET totp_enabled = 1 WHERE id = ?").bind(auth.user_id).execute(&state.db).await?;
    db::log_activity(&state.db, format!("{} switched on two-step sign-in", auth.username)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn totp_disable(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<TotpCode>) -> ApiResult<StatusCode> {
    if auth.is_admin() && state.settings().admin_totp_required {
        return Err(ApiError::bad_request(
            "This Hearth requires two-step sign-in for admins. Switch that off under Settings first.",
        ));
    }
    let secret: Option<String> = sqlx::query_scalar("SELECT totp_secret FROM users WHERE id = ?").bind(auth.user_id).fetch_one(&state.db).await?;
    if !secret.is_some_and(|secret| crate::totp::verify(&secret, &request.code, now())) {
        return Err(totp_invalid());
    }
    sqlx::query("UPDATE users SET totp_enabled = 0, totp_secret = NULL WHERE id = ?").bind(auth.user_id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}
