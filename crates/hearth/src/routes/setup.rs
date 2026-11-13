//! First-time setup. Every request must carry the setup code that was printed in the console,
//! and all of it answers `setup_complete` once an admin exists.

use std::time::Duration;

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use hearth_api::{
    ErrorCode, SETUP_CODE_HEADER,
    auth::DeviceInfo,
    settings::{Browse, FolderScan, FolderScanRequest, TlsMode},
    setup::{SetupDefaults, SetupDone, SetupRequest},
};
use serde::Deserialize;

use crate::{
    auth::{self, ApiJson, ClientIp},
    db::{self, now},
    error::{ApiError, ApiResult},
    fsutil, settings,
    state::AppState,
};

/// Lets a request through only while setup is open and the code is right.
fn guard(state: &AppState, headers: &HeaderMap, ip: std::net::IpAddr) -> ApiResult<()> {
    let Some(expected) = state.setup_code() else {
        return Err(ApiError::coded(
            StatusCode::GONE,
            ErrorCode::SetupComplete,
            "This Hearth is already set up. Sign in as an admin to change its settings.",
        ));
    };
    // Ten wrong codes per ten minutes: the code has eight characters, so guessing it is hopeless.
    let window = Duration::from_secs(600);
    state.limiter.check("setup", ip, 10, window)?;
    let given = headers.get(SETUP_CODE_HEADER).and_then(|value| value.to_str().ok()).unwrap_or_default();
    if auth::normalize_code(given) != auth::normalize_code(&expected) {
        state.limiter.fail("setup", ip, window);
        return Err(ApiError::coded(
            StatusCode::FORBIDDEN,
            ErrorCode::SetupCodeInvalid,
            "That is not the setup code. It is printed in the window where the Hearth was started.",
        ));
    }
    Ok(())
}

pub async fn begin(State(state): State<AppState>, ClientIp(ip): ClientIp, headers: HeaderMap) -> ApiResult<Json<SetupDefaults>> {
    guard(&state, &headers, ip)?;
    Ok(Json(SetupDefaults {
        settings: settings::public_view(&state.settings()),
        data_folder: state.paths.root().display().to_string(),
    }))
}

#[derive(Deserialize)]
pub struct BrowseQuery {
    #[serde(default)]
    pub path: String,
}

pub async fn browse(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Query(query): Query<BrowseQuery>,
) -> ApiResult<Json<Browse>> {
    guard(&state, &headers, ip)?;
    Ok(Json(tokio::task::spawn_blocking(move || fsutil::browse(&query.path)).await??))
}

pub async fn scan(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(request): ApiJson<FolderScanRequest>,
) -> ApiResult<Json<FolderScan>> {
    guard(&state, &headers, ip)?;
    Ok(Json(tokio::task::spawn_blocking(move || fsutil::scan_summary(&request.path)).await?))
}

pub async fn complete(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(request): ApiJson<SetupRequest>,
) -> ApiResult<Response> {
    guard(&state, &headers, ip)?;
    let username = auth::check_username(&request.username)?;
    auth::check_password(&request.password)?;
    let mut new_settings = request.settings;
    settings::validate(&mut new_settings)?;
    if new_settings.games_folders.is_empty() {
        return Err(ApiError::bad_request("Add at least one games folder."));
    }
    let password_hash = auth::hash_password_async(request.password).await?;

    let now = now();
    let invite_code = auth::invite_code();
    let invite_expires_at = now + 7 * 86400;
    let mut tx = state.db.begin().await?;
    // The check and the insert share a transaction, so two browsers cannot both become the owner.
    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&mut *tx).await?;
    if admins > 0 {
        return Err(ApiError::coded(StatusCode::GONE, ErrorCode::SetupComplete, "This Hearth is already set up."));
    }
    let user_id: i64 =
        sqlx::query_scalar("INSERT INTO users (username, password_hash, role, created_at) VALUES (?, ?, 'admin', ?) RETURNING id")
            .bind(&username)
            .bind(password_hash)
            .bind(now)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO invites (code, max_uses, expires_at, created_by, created_at) VALUES (?, 5, ?, ?, ?)")
        .bind(&invite_code)
        .bind(invite_expires_at)
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO settings (id, json) VALUES (1, ?) ON CONFLICT (id) DO UPDATE SET json = excluded.json")
        .bind(serde_json::to_string(&new_settings)?)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO meta (key, value) VALUES ('setup_complete', ?)").bind(now.to_string()).execute(&mut *tx).await?;
    tx.commit().await?;

    // Where the Hearth answers once the listener has bound again with the new settings.
    let was = state.bound();
    let https = new_settings.tls != TlsMode::Proxy;
    let port = state.opts.port.map(|_| was.as_ref().map_or(new_settings.port, |bound| bound.addr.port())).unwrap_or(new_settings.port);
    let moved = was.as_ref().is_none_or(|bound| bound.https != https || bound.addr.port() != port);
    let address = format!("{}:{}", new_settings.address, port);
    let url = format!("{}://{}", if https { "https" } else { "http" }, address);

    state.store_settings(new_settings).await?;
    state.finish_setup();
    db::log_activity(&state.db, "First-time setup finished").await;
    state.library.request_scan();

    let done = SetupDone {
        url,
        address,
        invite_code,
        invite_max_uses: 5,
        invite_expires_at,
        moved,
        handoff: moved.then(|| state.issue_handoff(user_id)),
    };
    if moved {
        // Give this answer time to leave before the listener closes the connection.
        let state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            state.rebind.notify_one();
        });
        return Ok(Json(done).into_response());
    }
    // Same address as before: sign the new admin in right here.
    let device = DeviceInfo { name: "Browser".into(), client: "Web admin".into(), ..Default::default() };
    let session = auth::create_session(&state, user_id, &device, ip).await?;
    let cookie = auth::session_cookie(&session.token, session.expires_at, auth::is_https(&state, &headers));
    Ok(([(header::SET_COOKIE, cookie)], Json(done)).into_response())
}
