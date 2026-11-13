//! People: what an admin does with accounts, invites, reset links and devices, and what
//! everyone may see of each other.

use std::time::Duration;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use hearth_api::{
    auth::{CreateInvite, Invite, ResetPassword, Role, Session},
    people::{
        AdminUser, Device, DownloadProgress, DownloadState, Installed, Person, Privacy, PrivacyPatch, QueueDownload, QueuedDownload, ResetInfo,
        ResetLink, UserPatch,
    },
};

use super::auth::list_sessions;
use crate::{
    auth::{self, Admin, ApiJson, Auth, ClientIp, role_from_db, role_to_db},
    db::{self, now},
    error::{ApiError, ApiResult},
    state::AppState,
};

const RESET_SECONDS: i64 = 24 * 3600;
/// Someone seen this recently counts as online.
pub const ONLINE_SECONDS: i64 = 300;

// ---------- everyone ----------

pub async fn people(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<Person>>> {
    let rows: Vec<(i64, String, String, Option<i64>)> =
        sqlx::query_as("SELECT id, username, role, last_seen_at FROM users WHERE removed_at IS NULL ORDER BY username COLLATE NOCASE")
            .fetch_all(&state.db)
            .await?;
    Ok(Json(rows.into_iter().map(|(id, username, role, last_seen_at)| Person { id, username, role: role_from_db(&role), last_seen_at }).collect()))
}

pub async fn privacy(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Privacy>> {
    let (share_playtime, share_achievements, share_now_playing): (bool, bool, bool) =
        sqlx::query_as("SELECT share_playtime, share_achievements, share_now_playing FROM users WHERE id = ?").bind(auth.user_id).fetch_one(&state.db).await?;
    Ok(Json(Privacy { share_playtime, share_achievements, share_now_playing }))
}

pub async fn patch_privacy(State(state): State<AppState>, auth: Auth, ApiJson(patch): ApiJson<PrivacyPatch>) -> ApiResult<Json<Privacy>> {
    let hides_or_shows_game = patch.share_now_playing.is_some();
    sqlx::query(
        "UPDATE users SET share_playtime = COALESCE(?, share_playtime), share_achievements = COALESCE(?, share_achievements),
         share_now_playing = COALESCE(?, share_now_playing) WHERE id = ?",
    )
    .bind(patch.share_playtime)
    .bind(patch.share_achievements)
    .bind(patch.share_now_playing)
    .bind(auth.user_id)
    .execute(&state.db)
    .await?;
    if hides_or_shows_game {
        super::social::announce(&state, auth.user_id).await;
    }
    privacy(State(state), auth).await
}

// ---------- devices ----------

async fn load_devices(state: &AppState, user_id: Option<i64>) -> ApiResult<Vec<Device>> {
    type Row = (i64, i64, String, String, String, String, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT d.id, d.user_id, u.username, d.name, d.platform, d.client, d.last_seen_at FROM devices d JOIN users u ON u.id = d.user_id
         WHERE u.removed_at IS NULL AND (?1 IS NULL OR d.user_id = ?1)
           AND EXISTS (SELECT 1 FROM sessions s WHERE s.device_id = d.id)
         ORDER BY u.username COLLATE NOCASE, d.last_seen_at DESC",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    let installed: Vec<(i64, i64, i64)> = sqlx::query_as("SELECT device_id, game_id, version_id FROM device_games").fetch_all(&state.db).await?;
    let now = now();
    Ok(rows
        .into_iter()
        .map(|(id, user_id, username, name, platform, client, last_seen_at)| Device {
            id,
            user_id,
            username,
            name,
            platform,
            client,
            last_seen_at,
            online: state.hub.device_online(id) || now - last_seen_at < ONLINE_SECONDS,
            installed: installed
                .iter()
                .filter(|(device_id, _, _)| *device_id == id)
                .map(|&(_, game_id, version_id)| Installed { game_id, version_id })
                .collect(),
        })
        .collect())
}

/// The caller's own signed-in devices.
pub async fn devices(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Device>>> {
    Ok(Json(load_devices(&state, Some(auth.user_id)).await?))
}

pub async fn all_devices(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Vec<Device>>> {
    Ok(Json(load_devices(&state, None).await?))
}

fn this_device(auth: &Auth) -> ApiResult<i64> {
    auth.device_id.ok_or_else(|| ApiError::bad_request("This session is not tied to a device. Sign in again."))
}

/// The device reports everything it has kindled. The list replaces what was known before.
pub async fn report_installed(State(state): State<AppState>, auth: Auth, ApiJson(list): ApiJson<Vec<Installed>>) -> ApiResult<StatusCode> {
    let device_id = this_device(&auth)?;
    let now = now();
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM device_games WHERE device_id = ?").bind(device_id).execute(&mut *tx).await?;
    for item in list {
        // A version that does not belong to the game, or is gone, is skipped rather than refused:
        // the device may hold something the Hearth has since dropped.
        sqlx::query(
            "INSERT OR REPLACE INTO device_games (device_id, game_id, version_id, updated_at)
             SELECT ?, v.game_id, v.id, ? FROM versions v WHERE v.id = ? AND v.game_id = ?",
        )
        .bind(device_id)
        .bind(now)
        .bind(item.version_id)
        .bind(item.game_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- remote downloads ----------

fn state_from_db(state: &str) -> DownloadState {
    match state {
        "started" => DownloadState::Started,
        "done" => DownloadState::Done,
        "failed" => DownloadState::Failed,
        _ => DownloadState::Queued,
    }
}

async fn load_queue(state: &AppState, device_id: Option<i64>) -> ApiResult<Vec<QueuedDownload>> {
    type Row = (i64, i64, i64, i64, String, i64, String, String, String);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT q.id, q.device_id, q.game_id, q.version_id, q.state, q.queued_at, d.name, u.username, g.title
         FROM download_queue q JOIN devices d ON d.id = q.device_id JOIN users u ON u.id = d.user_id JOIN games g ON g.id = q.game_id
         WHERE q.state IN ('queued', 'started') AND (?1 IS NULL OR q.device_id = ?1) ORDER BY q.id",
    )
    .bind(device_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, device_id, game_id, version_id, state, queued_at, device_name, username, game_title)| QueuedDownload {
            id,
            device_id,
            game_id,
            version_id,
            state: state_from_db(&state),
            queued_at,
            device_name,
            username,
            game_title,
        })
        .collect())
}

/// What is waiting for the device this request comes from. A client asks when it comes online.
pub async fn my_queue(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<QueuedDownload>>> {
    Ok(Json(load_queue(&state, Some(this_device(&auth)?)).await?))
}

pub async fn report_download(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    ApiJson(progress): ApiJson<DownloadProgress>,
) -> ApiResult<StatusCode> {
    let next = match progress.state {
        DownloadState::Started => "started",
        DownloadState::Done => "done",
        DownloadState::Failed => "failed",
        DownloadState::Queued => "queued",
    };
    let changed = sqlx::query("UPDATE download_queue SET state = ? WHERE id = ? AND device_id = ?")
        .bind(next)
        .bind(id)
        .bind(this_device(&auth)?)
        .execute(&state.db)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found("That download is not queued for this device."));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn queue(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Vec<QueuedDownload>>> {
    Ok(Json(load_queue(&state, None).await?))
}

/// Queues the newest version of a game for a device. `queue_for` is shared with the ready check.
pub async fn queue_download(State(state): State<AppState>, Admin(admin): Admin, ApiJson(request): ApiJson<QueueDownload>) -> ApiResult<Json<QueuedDownload>> {
    let id = queue_for(&state, request.device_id, request.game_id, admin.user_id).await?;
    let queued = load_queue(&state, Some(request.device_id)).await?.into_iter().find(|item| item.id == id);
    queued.map(Json).ok_or_else(|| ApiError::not_found("The download was queued but could not be read back."))
}

/// Tells the device about a queued download if it is connected. Otherwise it finds the
/// download in its queue when it next comes online.
async fn push_queued(state: &AppState, id: i64) -> ApiResult<()> {
    let owner: Option<(i64, i64)> =
        sqlx::query_as("SELECT d.user_id, d.id FROM download_queue q JOIN devices d ON d.id = q.device_id WHERE q.id = ?").bind(id).fetch_optional(&state.db).await?;
    if let Some((user_id, device_id)) = owner
        && let Some(item) = load_queue(state, Some(device_id)).await?.into_iter().find(|item| item.id == id)
    {
        state.hub.send_device(user_id, device_id, &hearth_api::social::Event::DownloadQueued(item));
    }
    Ok(())
}

pub async fn queue_for(state: &AppState, device_id: i64, game_id: i64, requested_by: i64) -> ApiResult<i64> {
    let device: Option<i64> = sqlx::query_scalar("SELECT id FROM devices WHERE id = ?").bind(device_id).fetch_optional(&state.db).await?;
    device.ok_or_else(|| ApiError::not_found("This Hearth knows no such device."))?;
    let version: Option<i64> = sqlx::query_scalar(
        "SELECT v.id FROM versions v JOIN manifests m ON m.id = v.manifest_id WHERE v.game_id = ? AND m.state = 'ready' ORDER BY v.id DESC LIMIT 1",
    )
    .bind(game_id)
    .fetch_optional(&state.db)
    .await?;
    let version_id = version.ok_or_else(|| ApiError::conflict("That game has no version ready to download yet."))?;
    let installed: Option<i64> =
        sqlx::query_scalar("SELECT version_id FROM device_games WHERE device_id = ? AND game_id = ?").bind(device_id).bind(game_id).fetch_optional(&state.db).await?;
    if installed == Some(version_id) {
        return Err(ApiError::conflict("That device already has the newest version of this game."));
    }
    // Asking twice for the same thing gives the same queue entry.
    let waiting: Option<i64> = sqlx::query_scalar("SELECT id FROM download_queue WHERE device_id = ? AND game_id = ? AND state IN ('queued', 'started')")
        .bind(device_id)
        .bind(game_id)
        .fetch_optional(&state.db)
        .await?;
    if let Some(id) = waiting {
        sqlx::query("UPDATE download_queue SET version_id = ? WHERE id = ?").bind(version_id).bind(id).execute(&state.db).await?;
        return Ok(id);
    }
    let id = sqlx::query_scalar("INSERT INTO download_queue (device_id, game_id, version_id, requested_by, queued_at) VALUES (?, ?, ?, ?, ?) RETURNING id")
        .bind(device_id)
        .bind(game_id)
        .bind(version_id)
        .bind(requested_by)
        .bind(now())
        .fetch_one(&state.db)
        .await?;
    push_queued(state, id).await?;
    Ok(id)
}

pub async fn cancel_download(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM download_queue WHERE id = ?").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- accounts, for admins ----------

pub async fn users(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Vec<AdminUser>>> {
    type Row = (i64, String, String, i64, Option<i64>, bool, Option<i64>, i64, i64, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT u.id, u.username, u.role, u.created_at, u.last_seen_at, u.totp_enabled, u.locked_until,
            (SELECT COALESCE(SUM(COALESCE(p.ended_at, p.last_beat_at) - p.started_at), 0) FROM play_sessions p WHERE p.user_id = u.id),
            (SELECT COUNT(DISTINCT s.device_id) FROM sessions s WHERE s.user_id = u.id),
            (SELECT COALESCE(SUM(r.bytes), 0) FROM save_revisions r WHERE r.user_id = u.id)
         FROM users u WHERE u.removed_at IS NULL ORDER BY u.id",
    )
    .fetch_all(&state.db)
    .await?;
    let now = now();
    Ok(Json(
        rows.into_iter()
            .map(|(id, username, role, created_at, last_seen_at, totp_enabled, locked_until, playtime, devices, saves)| AdminUser {
                id,
                username,
                role: role_from_db(&role),
                created_at,
                last_seen_at,
                online: state.hub.is_online(id) || last_seen_at.is_some_and(|seen| now - seen < ONLINE_SECONDS),
                playtime_seconds: playtime.max(0) as u64,
                devices: devices as u32,
                totp_enabled,
                locked_until: locked_until.filter(|until| *until > now),
                save_bytes: saves.max(0) as u64,
            })
            .collect(),
    ))
}

async fn admin_count(state: &AppState) -> ApiResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin' AND removed_at IS NULL").fetch_one(&state.db).await?)
}

async fn username_of(state: &AppState, id: i64) -> ApiResult<(String, Role)> {
    let row: Option<(String, String)> = sqlx::query_as("SELECT username, role FROM users WHERE id = ? AND removed_at IS NULL").bind(id).fetch_optional(&state.db).await?;
    row.map(|(username, role)| (username, role_from_db(&role))).ok_or_else(|| ApiError::not_found("This Hearth has no such account."))
}

pub async fn patch_user(State(state): State<AppState>, Admin(admin): Admin, Path(id): Path<i64>, ApiJson(patch): ApiJson<UserPatch>) -> ApiResult<StatusCode> {
    let (username, role) = username_of(&state, id).await?;
    if role == Role::Admin && patch.role == Role::Player && admin_count(&state).await? <= 1 {
        return Err(ApiError::conflict("A Hearth needs at least one admin. Make someone else an admin first."));
    }
    sqlx::query("UPDATE users SET role = ? WHERE id = ?").bind(role_to_db(patch.role)).bind(id).execute(&state.db).await?;
    if role != patch.role {
        db::log_activity(&state.db, format!("{} made {username} {}", admin.username, if patch.role == Role::Admin { "an admin" } else { "a player" })).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Removing someone signs them out everywhere at once. Their saves stay for 30 days.
pub async fn remove_user(State(state): State<AppState>, Admin(admin): Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let (username, role) = username_of(&state, id).await?;
    if id == admin.user_id {
        return Err(ApiError::conflict("You cannot remove your own account. Another admin can."));
    }
    if role == Role::Admin && admin_count(&state).await? <= 1 {
        return Err(ApiError::conflict("A Hearth needs at least one admin."));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("UPDATE users SET removed_at = ? WHERE id = ?").bind(now()).bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM password_resets WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    state.hub.kick_user(id);
    db::log_activity(&state.db, format!("{} removed {username}. Their saves are kept for 30 days", admin.username)).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn all_sessions(State(state): State<AppState>, Admin(admin): Admin) -> ApiResult<Json<Vec<Session>>> {
    Ok(Json(list_sessions(&state, None, admin.session_id).await?))
}

// ---------- invites ----------

fn invite_row((id, code, max_uses, uses, expires_at, created_at): (i64, String, Option<i64>, i64, Option<i64>, i64)) -> Invite {
    Invite { id, code, max_uses: max_uses.map(|max| max as u32), uses: uses as u32, expires_at, created_at }
}

pub async fn invites(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Vec<Invite>>> {
    // Codes that can still be used. Spent and expired ones only clutter the list.
    let rows = sqlx::query_as(
        "SELECT id, code, max_uses, uses, expires_at, created_at FROM invites
         WHERE (max_uses IS NULL OR uses < max_uses) AND (expires_at IS NULL OR expires_at > ?) ORDER BY id DESC",
    )
    .bind(now())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(invite_row).collect()))
}

pub async fn create_invite(State(state): State<AppState>, Admin(admin): Admin, ApiJson(request): ApiJson<CreateInvite>) -> ApiResult<Json<Invite>> {
    if !matches!(request.max_uses, None | Some(1) | Some(5)) {
        return Err(ApiError::bad_request("An invite works once, five times, or without a limit."));
    }
    let days = request.expires_in_days.unwrap_or(7);
    if !(1..=365).contains(&days) {
        return Err(ApiError::bad_request("An invite expires after 1 to 365 days."));
    }
    let now = now();
    let row = sqlx::query_as(
        "INSERT INTO invites (code, max_uses, expires_at, created_by, created_at) VALUES (?, ?, ?, ?, ?)
         RETURNING id, code, max_uses, uses, expires_at, created_at",
    )
    .bind(auth::invite_code())
    .bind(request.max_uses)
    .bind(now + days as i64 * 86400)
    .bind(admin.user_id)
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(invite_row(row)))
}

pub async fn delete_invite(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM invites WHERE id = ?").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- password reset links ----------

/// Makes a link the admin passes on by hand. It also lifts a lock from wrong passwords.
pub async fn create_reset_link(State(state): State<AppState>, Admin(admin): Admin, Path(user_id): Path<i64>) -> ApiResult<Json<ResetLink>> {
    let (username, _) = username_of(&state, user_id).await?;
    let token = auth::new_token();
    let expires_at = now() + RESET_SECONDS;
    let id: i64 = sqlx::query_scalar("INSERT INTO password_resets (token_hash, user_id, expires_at, created_at) VALUES (?, ?, ?, ?) RETURNING id")
        .bind(auth::token_hash(&token))
        .bind(user_id)
        .bind(expires_at)
        .bind(now())
        .fetch_one(&state.db)
        .await?;
    sqlx::query("UPDATE users SET failed_logins = 0, locked_until = NULL WHERE id = ?").bind(user_id).execute(&state.db).await?;
    db::log_activity(&state.db, format!("{} made a password reset link for {username}", admin.username)).await;
    Ok(Json(ResetLink { id, user_id, username, expires_at, path: Some(format!("/reset/{token}")) }))
}

pub async fn reset_links(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Vec<ResetLink>>> {
    let rows: Vec<(i64, i64, String, i64)> = sqlx::query_as(
        "SELECT r.id, r.user_id, u.username, r.expires_at FROM password_resets r JOIN users u ON u.id = r.user_id
         WHERE r.used_at IS NULL AND r.expires_at > ? ORDER BY r.id DESC",
    )
    .bind(now())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(rows.into_iter().map(|(id, user_id, username, expires_at)| ResetLink { id, user_id, username, expires_at, path: None }).collect()))
}

pub async fn delete_reset_link(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM password_resets WHERE id = ?").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn reset_invalid() -> ApiError {
    ApiError::not_found("This reset link does not work. It may have been used already or be older than 24 hours. Ask an admin for a new one.")
}

async fn pending_reset(state: &AppState, token: &str, ip: std::net::IpAddr) -> ApiResult<(i64, i64, String)> {
    let window = Duration::from_secs(600);
    state.limiter.check("reset", ip, 10, window)?;
    let row: Option<(i64, i64, String)> = sqlx::query_as(
        "SELECT r.id, r.user_id, u.username FROM password_resets r JOIN users u ON u.id = r.user_id
         WHERE r.token_hash = ? AND r.used_at IS NULL AND r.expires_at > ? AND u.removed_at IS NULL",
    )
    .bind(auth::token_hash(token))
    .bind(now())
    .fetch_optional(&state.db)
    .await?;
    row.ok_or_else(|| {
        state.limiter.fail("reset", ip, window);
        reset_invalid()
    })
}

/// Tells the reset page whose password it is about to change.
pub async fn reset_info(State(state): State<AppState>, ClientIp(ip): ClientIp, Path(token): Path<String>) -> ApiResult<Json<ResetInfo>> {
    let (_, _, username) = pending_reset(&state, &token, ip).await?;
    Ok(Json(ResetInfo { username }))
}

/// Sets the new password. The link is spent, and every device of the account is signed out.
pub async fn reset_password(State(state): State<AppState>, ClientIp(ip): ClientIp, ApiJson(request): ApiJson<ResetPassword>) -> ApiResult<StatusCode> {
    let (reset_id, user_id, username) = pending_reset(&state, &request.token, ip).await?;
    auth::check_password(&request.new_password)?;
    let hash = auth::hash_password_async(request.new_password).await?;
    let mut tx = state.db.begin().await?;
    // Marking it used is the gate: of two requests with the same link, one changes a row and wins.
    let spent = sqlx::query("UPDATE password_resets SET used_at = ? WHERE id = ? AND used_at IS NULL").bind(now()).bind(reset_id).execute(&mut *tx).await?;
    if spent.rows_affected() != 1 {
        return Err(reset_invalid());
    }
    sqlx::query("UPDATE users SET password_hash = ?, failed_logins = 0, locked_until = NULL WHERE id = ?").bind(hash).bind(user_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    state.hub.kick_user(user_id);
    db::log_activity(&state.db, format!("{username} set a new password with a reset link")).await;
    Ok(StatusCode::NO_CONTENT)
}
