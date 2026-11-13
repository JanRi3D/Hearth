//! Rest: cloud saves. The files travel over WebDAV (see `crate::dav`); these endpoints say
//! what is stored and turn an upload into a revision.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use hearth_api::{
    ErrorCode,
    saves::{AccountStorage, CommitSave, GameSaves, KEPT_REVISIONS, RestoreSave, Revision, Saves, Storage},
};

use crate::{
    auth::{Admin, ApiJson, Auth},
    db::{self, now},
    error::{ApiError, ApiResult},
    state::AppState,
    storage::Store,
};

pub fn store(state: &AppState, user_id: i64, game_id: i64) -> Store {
    Store::new(state.paths.dir("saves").join(user_id.to_string()).join(game_id.to_string()))
}

pub fn quota_bytes(state: &AppState) -> u64 {
    state.opts.save_quota.unwrap_or(state.settings().save_quota_gb as u64 * 1024 * 1024 * 1024)
}

/// Fails unless the game exists and the caller may see it.
pub async fn check_game(state: &AppState, auth: &Auth, game_id: i64) -> ApiResult<()> {
    let visible: Option<bool> = sqlx::query_scalar("SELECT visible FROM games WHERE id = ?").bind(game_id).fetch_optional(&state.db).await?;
    match visible {
        Some(visible) if visible || auth.is_admin() => Ok(()),
        _ => Err(ApiError::not_found("This Hearth has no such game.")),
    }
}

async fn revisions(state: &AppState, user_id: i64, game_id: i64) -> ApiResult<Vec<Revision>> {
    type Row = (i64, Option<i64>, String, i64, i64, i64, String);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT number, device_id, device_name, committed_at, bytes, files, hash FROM save_revisions WHERE user_id = ? AND game_id = ? ORDER BY number DESC",
    )
    .bind(user_id)
    .bind(game_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(number, device_id, device_name, committed_at, bytes, files, hash)| Revision {
            number: number as u32,
            device_id,
            device_name,
            committed_at,
            bytes: bytes as u64,
            files: files as u32,
            hash,
        })
        .collect())
}

pub async fn revision_numbers(state: &AppState, user_id: i64, game_id: i64) -> ApiResult<Vec<u32>> {
    Ok(revisions(state, user_id, game_id).await?.iter().map(|revision| revision.number).collect())
}

pub async fn head_number(state: &AppState, user_id: i64, game_id: i64) -> ApiResult<Option<u32>> {
    Ok(revision_numbers(state, user_id, game_id).await?.first().copied())
}

/// Games the person has saves for, rested or still being uploaded.
pub async fn games_with_saves(state: &AppState, user_id: i64) -> ApiResult<Vec<i64>> {
    let mut games: Vec<i64> = sqlx::query_scalar("SELECT DISTINCT game_id FROM save_revisions WHERE user_id = ?").bind(user_id).fetch_all(&state.db).await?;
    if let Ok(entries) = std::fs::read_dir(state.paths.dir("saves").join(user_id.to_string())) {
        games.extend(entries.flatten().filter_map(|entry| entry.file_name().to_str()?.parse::<i64>().ok()));
    }
    games.sort_unstable();
    games.dedup();
    Ok(games)
}

pub async fn used_bytes(state: &AppState, user_id: i64) -> ApiResult<u64> {
    let used: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(bytes), 0) FROM save_revisions WHERE user_id = ?").bind(user_id).fetch_one(&state.db).await?;
    Ok(used as u64)
}

pub async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Saves>> {
    let game_ids: Vec<i64> = sqlx::query_scalar("SELECT DISTINCT game_id FROM save_revisions WHERE user_id = ? ORDER BY game_id").bind(auth.user_id).fetch_all(&state.db).await?;
    let mut games = Vec::with_capacity(game_ids.len());
    for game_id in game_ids {
        let revisions = revisions(&state, auth.user_id, game_id).await?;
        games.push(GameSaves { game_id, head: revisions.first().cloned(), revisions });
    }
    Ok(Json(Saves { used_bytes: used_bytes(&state, auth.user_id).await?, quota_bytes: quota_bytes(&state), games }))
}

pub async fn game(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>) -> ApiResult<Json<GameSaves>> {
    check_game(&state, &auth, game_id).await?;
    let revisions = revisions(&state, auth.user_id, game_id).await?;
    Ok(Json(GameSaves { game_id, head: revisions.first().cloned(), revisions }))
}

async fn device_name(state: &AppState, auth: &Auth) -> ApiResult<String> {
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM devices WHERE id = ?").bind(auth.device_id).fetch_optional(&state.db).await?;
    Ok(name.unwrap_or_default())
}

async fn insert_revision(state: &AppState, auth: &Auth, game_id: i64, number: u32, summary: &crate::storage::Summary) -> ApiResult<Revision> {
    let revision = Revision {
        number,
        device_id: auth.device_id,
        device_name: device_name(state, auth).await?,
        committed_at: now(),
        bytes: summary.bytes,
        files: summary.files,
        hash: summary.hash.clone(),
    };
    sqlx::query("INSERT INTO save_revisions (user_id, game_id, number, device_id, device_name, bytes, files, hash, committed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(auth.user_id)
        .bind(game_id)
        .bind(number)
        .bind(revision.device_id)
        .bind(&revision.device_name)
        .bind(revision.bytes as i64)
        .bind(revision.files)
        .bind(&revision.hash)
        .bind(revision.committed_at)
        .execute(&state.db)
        .await?;
    Ok(revision)
}

/// Drops old revisions of a game: beyond the number kept, and then, oldest first, whatever
/// it takes to bring the account back under its quota. The newest revision always stays.
async fn prune(state: &AppState, user_id: i64, game_id: i64) -> ApiResult<()> {
    let store = store(state, user_id, game_id);
    let quota = quota_bytes(state);
    let mut kept = revisions(state, user_id, game_id).await?;
    let mut used = used_bytes(state, user_id).await?;
    while kept.len() > 1 && (kept.len() as u32 > KEPT_REVISIONS || used > quota) {
        let oldest = kept.pop().expect("more than one revision");
        sqlx::query("DELETE FROM save_revisions WHERE user_id = ? AND game_id = ? AND number = ?").bind(user_id).bind(game_id).bind(oldest.number).execute(&state.db).await?;
        store.remove_revision(oldest.number);
        used = used.saturating_sub(oldest.bytes);
    }
    Ok(())
}

fn too_big() -> ApiError {
    ApiError::coded(
        StatusCode::INSUFFICIENT_STORAGE,
        ErrorCode::QuotaExceeded,
        "These saves are larger than the space set aside per person. An admin can raise it under Settings.",
    )
}

pub async fn commit(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>, ApiJson(request): ApiJson<CommitSave>) -> ApiResult<Json<Revision>> {
    check_game(&state, &auth, game_id).await?;
    let head = revisions(&state, auth.user_id, game_id).await?.into_iter().next();
    if let Some(head) = &head
        && request.base != Some(head.number)
        && !request.force
    {
        let from = if head.device_name.is_empty() { "another PC".to_string() } else { head.device_name.clone() };
        return Err(ApiError::coded(
            StatusCode::CONFLICT,
            ErrorCode::SaveConflict,
            format!("Newer saves for this game were rested from {from}. Choose which saves to keep, then rest again."),
        ));
    }
    let number = head.as_ref().map_or(1, |head| head.number + 1);
    let store = store(&state, auth.user_id, game_id);
    let committing = store.clone();
    let summary = tokio::task::spawn_blocking(move || committing.commit(number))
        .await??
        .ok_or_else(|| ApiError::bad_request("There is nothing to rest. Sync the saves into upload/ first."))?;

    // The same saves as last time are not a new revision.
    if let Some(head) = head.filter(|head| head.hash == summary.hash) {
        store.remove_revision(number);
        return Ok(Json(head));
    }
    if summary.bytes > quota_bytes(&state) {
        store.remove_revision(number);
        return Err(too_big());
    }
    let revision = insert_revision(&state, &auth, game_id, number, &summary).await?;
    prune(&state, auth.user_id, game_id).await?;
    let megabytes = summary.bytes as f64 / (1024.0 * 1024.0);
    db::log_activity(&state.db, format!("{} rested {} files, {megabytes:.1} MB", auth.username, summary.files)).await;
    Ok(Json(revision))
}

/// Makes an older revision the newest. The revisions in between are kept.
pub async fn restore(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>, ApiJson(request): ApiJson<RestoreSave>) -> ApiResult<Json<Revision>> {
    check_game(&state, &auth, game_id).await?;
    let all = revisions(&state, auth.user_id, game_id).await?;
    let head = all.first().ok_or_else(|| ApiError::not_found("There are no saves for this game."))?;
    if !all.iter().any(|revision| revision.number == request.revision) {
        return Err(ApiError::not_found("That revision is not kept any more."));
    }
    if head.number == request.revision {
        return Ok(Json(head.clone()));
    }
    let number = head.number + 1;
    let store = store(&state, auth.user_id, game_id);
    let summary = tokio::task::spawn_blocking(move || store.copy_revision(request.revision, number)).await??;
    let revision = insert_revision(&state, &auth, game_id, number, &summary).await?;
    prune(&state, auth.user_id, game_id).await?;
    Ok(Json(revision))
}

/// Deletes every rested save of one game for the caller.
pub async fn delete(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM save_revisions WHERE user_id = ? AND game_id = ?").bind(auth.user_id).bind(game_id).execute(&state.db).await?;
    let store = store(&state, auth.user_id, game_id);
    tokio::task::spawn_blocking(move || store.remove_all()).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn admin_storage(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Storage>> {
    let rows: Vec<(i64, String, i64, i64, Option<i64>)> = sqlx::query_as(
        "SELECT u.id, u.username, COALESCE(SUM(r.bytes), 0), COUNT(DISTINCT r.game_id), MAX(r.committed_at)
         FROM users u LEFT JOIN save_revisions r ON r.user_id = u.id WHERE u.removed_at IS NULL GROUP BY u.id ORDER BY u.username COLLATE NOCASE",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Storage {
        quota_bytes: quota_bytes(&state),
        accounts: rows
            .into_iter()
            .map(|(user_id, username, used, games, last_rested_at)| AccountStorage { user_id, username, used_bytes: used as u64, games: games as u32, last_rested_at })
            .collect(),
    }))
}
