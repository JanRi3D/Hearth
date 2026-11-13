//! Shared worlds: a multiplayer save that lives on the Hearth. Whoever starts it becomes
//! the host and holds it until they give it back, with a journal for the next host.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use hearth_api::{
    ErrorCode,
    saves::{AddWorldMember, CreateWorld, JournalEntry, JournalNote, ReleaseWorld, WORLD_HOST_TIMEOUT, World, WorldHost, WorldMember},
    social::Event,
};
use serde::Deserialize;

use crate::{
    auth::{ApiJson, Auth},
    db::{self, now},
    error::{ApiError, ApiResult},
    state::AppState,
    storage::Store,
};

pub fn store(state: &AppState, world_id: i64) -> Store {
    Store::new(state.paths.dir("worlds").join(world_id.to_string()))
}

/// What a member may do with a world's files right now.
pub struct Access {
    pub revision: u32,
    /// The caller is the one hosting it, so they may write.
    pub hosting: bool,
}

/// `None` for someone who is not a member.
pub async fn access(state: &AppState, world_id: i64, user_id: i64) -> ApiResult<Option<Access>> {
    let row: Option<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT w.revision, w.host_id, w.host_beat_at FROM worlds w JOIN world_members m ON m.world_id = w.id WHERE w.id = ? AND m.user_id = ?",
    )
    .bind(world_id)
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(revision, host_id, beat)| Access { revision: revision as u32, hosting: host_id == Some(user_id) && beat.is_some_and(|beat| beat > now() - WORLD_HOST_TIMEOUT) }))
}

pub async fn member_of(state: &AppState, user_id: i64) -> ApiResult<Vec<i64>> {
    Ok(sqlx::query_scalar("SELECT world_id FROM world_members WHERE user_id = ? ORDER BY world_id").bind(user_id).fetch_all(&state.db).await?)
}

/// Worlds by a filter on their id: one, those of a member, or all.
pub async fn load(state: &AppState, ids: Option<&[i64]>) -> ApiResult<Vec<World>> {
    type Row = (i64, i64, String, i64, Option<i64>, i64, i64, Option<i64>, Option<i64>, Option<i64>, i64, Option<String>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT w.id, w.game_id, w.name, w.max_players, w.created_by, w.revision, w.bytes, w.host_id, w.hosted_at, w.host_beat_at, w.updated_at, u.username
         FROM worlds w LEFT JOIN users u ON u.id = w.host_id ORDER BY w.name COLLATE NOCASE",
    )
    .fetch_all(&state.db)
    .await?;
    let members: Vec<(i64, i64, String)> =
        sqlx::query_as("SELECT m.world_id, u.id, u.username FROM world_members m JOIN users u ON u.id = m.user_id WHERE u.removed_at IS NULL ORDER BY u.username COLLATE NOCASE")
            .fetch_all(&state.db)
            .await?;
    let now = now();
    Ok(rows
        .into_iter()
        .filter(|row| ids.is_none_or(|ids| ids.contains(&row.0)))
        .map(|(id, game_id, name, max_players, created_by, revision, bytes, host_id, hosted_at, beat, updated_at, host_name)| World {
            id,
            game_id,
            name,
            max_players: max_players as u32,
            created_by,
            members: members.iter().filter(|(world, _, _)| *world == id).map(|(_, user_id, username)| WorldMember { user_id: *user_id, username: username.clone() }).collect(),
            revision: revision as u32,
            bytes: bytes as u64,
            // A host who stopped answering no longer holds the world.
            host: host_id
                .zip(hosted_at)
                .filter(|_| beat.is_some_and(|beat| beat > now - WORLD_HOST_TIMEOUT))
                .map(|(user_id, since)| WorldHost { user_id, username: host_name.unwrap_or_default(), since }),
            updated_at,
        })
        .collect())
}

async fn one(state: &AppState, id: i64) -> ApiResult<World> {
    load(state, Some(&[id])).await?.into_iter().next().ok_or_else(no_world)
}

fn no_world() -> ApiError {
    ApiError::not_found("There is no such world, or you are not a member of it.")
}

/// The world, if the caller is a member of it or an admin.
async fn mine(state: &AppState, auth: &Auth, id: i64) -> ApiResult<World> {
    let world = one(state, id).await?;
    if world.members.iter().any(|member| member.user_id == auth.user_id) || auth.is_admin() {
        return Ok(world);
    }
    Err(no_world())
}

/// Tells every member who is connected what the world looks like now.
fn broadcast(state: &AppState, world: &World) {
    for member in &world.members {
        state.hub.send(member.user_id, &Event::WorldUpdated(world.clone()));
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    game_id: Option<i64>,
}

/// The worlds the caller is a member of.
pub async fn list(State(state): State<AppState>, auth: Auth, Query(query): Query<ListQuery>) -> ApiResult<Json<Vec<World>>> {
    let ids = member_of(&state, auth.user_id).await?;
    Ok(Json(load(&state, Some(&ids)).await?.into_iter().filter(|world| query.game_id.is_none_or(|game| game == world.game_id)).collect()))
}

pub async fn create(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<CreateWorld>) -> ApiResult<Json<World>> {
    super::saves::check_game(&state, &auth, request.game_id).await?;
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(ApiError::bad_request("A world needs a name of up to 60 characters."));
    }
    if !(1..=250).contains(&request.max_players) {
        return Err(ApiError::bad_request("A world takes 1 to 250 players."));
    }
    let now = now();
    let mut tx = state.db.begin().await?;
    let id: i64 = sqlx::query_scalar("INSERT INTO worlds (game_id, name, max_players, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?) RETURNING id")
        .bind(request.game_id)
        .bind(name)
        .bind(request.max_players)
        .bind(auth.user_id)
        .bind(now)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO world_members (world_id, user_id) VALUES (?, ?)").bind(id).bind(auth.user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(one(&state, id).await?))
}

pub async fn get(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<World>> {
    Ok(Json(mine(&state, &auth, id).await?))
}

/// Deletes a world and its files. For the person who made it, or an admin.
pub async fn delete(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let world = mine(&state, &auth, id).await?;
    if world.created_by != Some(auth.user_id) && !auth.is_admin() {
        return Err(ApiError::forbidden("Only the person who made this world, or an admin, can delete it."));
    }
    if world.host.as_ref().is_some_and(|host| host.user_id != auth.user_id) {
        return Err(locked(&world));
    }
    sqlx::query("DELETE FROM worlds WHERE id = ?").bind(id).execute(&state.db).await?;
    let store = store(&state, id);
    tokio::task::spawn_blocking(move || store.remove_all()).await?;
    db::log_activity(&state.db, format!("{} deleted the shared world {}", auth.username, world.name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Any member can bring someone in.
pub async fn add_member(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(request): ApiJson<AddWorldMember>) -> ApiResult<Json<World>> {
    mine(&state, &auth, id).await?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = ? AND removed_at IS NULL").bind(request.user_id).fetch_optional(&state.db).await?;
    exists.ok_or_else(|| ApiError::not_found("This Hearth has no such person."))?;
    sqlx::query("INSERT OR IGNORE INTO world_members (world_id, user_id) VALUES (?, ?)").bind(id).bind(request.user_id).execute(&state.db).await?;
    let world = one(&state, id).await?;
    broadcast(&state, &world);
    Ok(Json(world))
}

/// People leave on their own. The maker of the world, or an admin, can take others out.
pub async fn remove_member(State(state): State<AppState>, auth: Auth, Path((id, user_id)): Path<(i64, i64)>) -> ApiResult<StatusCode> {
    let world = mine(&state, &auth, id).await?;
    if user_id != auth.user_id && world.created_by != Some(auth.user_id) && !auth.is_admin() {
        return Err(ApiError::forbidden("Only the person who made this world, or an admin, can take someone else out."));
    }
    if world.host.as_ref().is_some_and(|host| host.user_id == user_id) {
        return Err(ApiError::conflict("That person is hosting the world right now. They have to stop first."));
    }
    sqlx::query("DELETE FROM world_members WHERE world_id = ? AND user_id = ?").bind(id).bind(user_id).execute(&state.db).await?;
    broadcast(&state, &one(&state, id).await?);
    Ok(StatusCode::NO_CONTENT)
}

fn locked(world: &World) -> ApiError {
    let host = world.host.as_ref().map_or("Someone", |host| host.username.as_str());
    ApiError::coded(StatusCode::CONFLICT, ErrorCode::WorldLocked, format!("{host} is hosting this world right now. It opens again when they stop."))
}

/// Takes the world. From here until `release`, only the caller can write to it.
pub async fn host(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<World>> {
    let world = mine(&state, &auth, id).await?;
    if !world.members.iter().any(|member| member.user_id == auth.user_id) {
        return Err(ApiError::forbidden("Join the world before hosting it."));
    }
    if world.host.as_ref().is_some_and(|host| host.user_id == auth.user_id) {
        return Ok(Json(world));
    }
    let now = now();
    // Taking the world is one statement, so of two people asking at once only one gets it.
    let taken = sqlx::query("UPDATE worlds SET host_id = ?1, hosted_at = ?2, host_beat_at = ?2 WHERE id = ?3 AND (host_id IS NULL OR host_beat_at IS NULL OR host_beat_at <= ?4)")
        .bind(auth.user_id)
        .bind(now)
        .bind(id)
        .bind(now - WORLD_HOST_TIMEOUT)
        .execute(&state.db)
        .await?;
    if taken.rows_affected() != 1 {
        return Err(locked(&one(&state, id).await?));
    }
    // The upload area starts as the world's current files, whatever an earlier host left there.
    let (store, revision) = (store(&state, id), world.revision);
    tokio::task::spawn_blocking(move || if revision > 0 { store.refill_upload(revision) } else { store.clear_upload() }).await?;
    let world = one(&state, id).await?;
    broadcast(&state, &world);
    Ok(Json(world))
}

/// The host says it is still there. Without this for five minutes the world can be taken over.
pub async fn heartbeat(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let beat = sqlx::query("UPDATE worlds SET host_beat_at = ? WHERE id = ? AND host_id = ?").bind(now()).bind(id).bind(auth.user_id).execute(&state.db).await?;
    if beat.rows_affected() != 1 {
        return Err(ApiError::conflict("You are not hosting this world."));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The host gives the world back, usually storing what they uploaded as its new state.
pub async fn release(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(request): ApiJson<ReleaseWorld>) -> ApiResult<Json<World>> {
    let world = mine(&state, &auth, id).await?;
    let Some(host) = world.host.as_ref().filter(|host| host.user_id == auth.user_id) else {
        return Err(ApiError::conflict("You are not hosting this world."));
    };
    let now = now();
    if request.store {
        let (store, number) = (store(&state, id), world.revision + 1);
        let committing = store.clone();
        let summary = tokio::task::spawn_blocking(move || committing.commit(number))
            .await??
            .ok_or_else(|| ApiError::bad_request("There is nothing to store. Sync the world's files into upload/ first, or give it back unchanged."))?;
        if summary.bytes > crate::dav::WORLD_LIMIT {
            store.remove_revision(number);
            return Err(ApiError::coded(StatusCode::INSUFFICIENT_STORAGE, ErrorCode::QuotaExceeded, "This world is larger than a shared world may be."));
        }
        sqlx::query("UPDATE worlds SET revision = ?, bytes = ?, updated_at = ? WHERE id = ?").bind(number).bind(summary.bytes as i64).bind(now).bind(id).execute(&state.db).await?;
        // The state before this one stays as a safety net. Anything older goes.
        if number > 2 {
            store.remove_revision(number - 2);
        }
    }
    sqlx::query("INSERT INTO world_journal (world_id, user_id, at, seconds, note) VALUES (?, ?, ?, ?, ?)")
        .bind(id)
        .bind(auth.user_id)
        .bind(host.since)
        .bind((now - host.since).max(0))
        .bind(request.note.trim())
        .execute(&state.db)
        .await?;
    sqlx::query("UPDATE worlds SET host_id = NULL, hosted_at = NULL, host_beat_at = NULL WHERE id = ?").bind(id).execute(&state.db).await?;
    let world = one(&state, id).await?;
    broadcast(&state, &world);
    Ok(Json(world))
}

/// Newest first.
pub async fn journal(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<Vec<JournalEntry>>> {
    mine(&state, &auth, id).await?;
    let rows: Vec<(i64, Option<i64>, String, i64, i64, String)> = sqlx::query_as(
        "SELECT j.id, j.user_id, COALESCE(u.username, ''), j.at, j.seconds, j.note FROM world_journal j LEFT JOIN users u ON u.id = j.user_id WHERE j.world_id = ? ORDER BY j.at DESC, j.id DESC LIMIT 200",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, user_id, username, at, seconds, note)| JournalEntry { id, user_id, username, at, seconds: seconds.max(0) as u64, note })
            .collect(),
    ))
}

/// A note on its own, for whoever hosts next.
pub async fn add_note(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(request): ApiJson<JournalNote>) -> ApiResult<StatusCode> {
    mine(&state, &auth, id).await?;
    let note = request.note.trim();
    if note.is_empty() || note.chars().count() > 2000 {
        return Err(ApiError::bad_request("A journal note has 1 to 2000 characters."));
    }
    sqlx::query("INSERT INTO world_journal (world_id, user_id, at, note) VALUES (?, ?, ?, ?)").bind(id).bind(auth.user_id).bind(now()).bind(note).execute(&state.db).await?;
    Ok(StatusCode::CREATED)
}
