//! Play data: sessions and playtime, achievements, stats, collections, notes, and what
//! friends may see of all that.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use hearth_api::{
    people::Person,
    play::{
        AchievementDef, Achievements, Collection, CollectionPatch, GameAchievements, GamePlayer, Note, NowPlaying, PlaySession, Playtime, Profile,
        SESSION_TIMEOUT, StartSession, Stats, Unlocked,
    },
};
use serde::Deserialize;

use crate::{
    auth::{Admin, ApiJson, Auth, role_from_db},
    db::now,
    error::{ApiError, ApiResult},
    state::AppState,
};

/// Fails unless the game exists and the caller may see it.
async fn visible_game(state: &AppState, auth: &Auth, game_id: i64) -> ApiResult<()> {
    let visible: Option<bool> = sqlx::query_scalar("SELECT visible FROM games WHERE id = ?").bind(game_id).fetch_optional(&state.db).await?;
    match visible {
        Some(visible) if visible || auth.is_admin() => Ok(()),
        _ => Err(ApiError::not_found("This Hearth has no such game.")),
    }
}

// ---------- sessions and playtime ----------

/// When a session ended, in SQL: explicitly, or at its last heartbeat once that is too old.
const ENDED: &str = "COALESCE(ended_at, CASE WHEN last_beat_at < ?1 - 180 THEN last_beat_at END)";
const _: () = assert!(SESSION_TIMEOUT == 180, "the SQL above spells the timeout out");

fn session_row((id, game_id, device_id, started_at, last_beat_at, ended_at): (i64, i64, Option<i64>, i64, i64, Option<i64>)) -> PlaySession {
    let now = now();
    let ended_at = ended_at.or((last_beat_at < now - SESSION_TIMEOUT).then_some(last_beat_at));
    PlaySession { id, game_id, device_id, started_at, ended_at, seconds: (ended_at.unwrap_or(now) - started_at).max(0) as u64 }
}

pub async fn start_session(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<StartSession>) -> ApiResult<Json<PlaySession>> {
    visible_game(&state, &auth, request.game_id).await?;
    let now = now();
    // One game at a time per device: starting another closes what was running there.
    sqlx::query("UPDATE play_sessions SET ended_at = last_beat_at WHERE user_id = ? AND device_id IS ? AND ended_at IS NULL")
        .bind(auth.user_id)
        .bind(auth.device_id)
        .execute(&state.db)
        .await?;
    let row = sqlx::query_as(
        "INSERT INTO play_sessions (user_id, game_id, device_id, started_at, last_beat_at) VALUES (?, ?, ?, ?, ?)
         RETURNING id, game_id, device_id, started_at, last_beat_at, ended_at",
    )
    .bind(auth.user_id)
    .bind(request.game_id)
    .bind(auth.device_id)
    .bind(now)
    .bind(now)
    .fetch_one(&state.db)
    .await?;
    super::social::announce(&state, auth.user_id).await;
    Ok(Json(session_row(row)))
}

async fn touch_session(state: &AppState, auth: &Auth, id: i64, stop: bool) -> ApiResult<StatusCode> {
    let now = now();
    let changed = sqlx::query("UPDATE play_sessions SET last_beat_at = ?1, ended_at = CASE WHEN ?2 THEN ?1 END WHERE id = ?3 AND user_id = ?4 AND ended_at IS NULL")
        .bind(now)
        .bind(stop)
        .bind(id)
        .bind(auth.user_id)
        .execute(&state.db)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found("That play session is not running. Start a new one."));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Sent about once a minute while the game runs, so playtime is right even if the PC loses power.
pub async fn heartbeat(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    touch_session(&state, &auth, id, false).await
}

pub async fn stop_session(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let stopped = touch_session(&state, &auth, id, true).await?;
    super::social::announce(&state, auth.user_id).await;
    Ok(stopped)
}

async fn playtime_of(state: &AppState, user_id: i64) -> ApiResult<Vec<Playtime>> {
    let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT game_id, SUM(COALESCE(ended_at, CASE WHEN last_beat_at < ?1 - 180 THEN last_beat_at ELSE ?1 END) - started_at), COUNT(*), MAX(started_at)
         FROM play_sessions WHERE user_id = ?2 GROUP BY game_id ORDER BY MAX(started_at) DESC",
    )
    .bind(now())
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(|(game_id, seconds, sessions, last_played_at)| Playtime { game_id, seconds: seconds.max(0) as u64, sessions: sessions as u32, last_played_at }).collect())
}

async fn sessions_of(state: &AppState, user_id: i64, game_id: Option<i64>, limit: i64) -> ApiResult<Vec<PlaySession>> {
    let rows = sqlx::query_as(
        "SELECT id, game_id, device_id, started_at, last_beat_at, ended_at FROM play_sessions
         WHERE user_id = ?1 AND (?2 IS NULL OR game_id = ?2) ORDER BY started_at DESC LIMIT ?3",
    )
    .bind(user_id)
    .bind(game_id)
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    Ok(rows.into_iter().map(session_row).collect())
}

pub async fn my_playtime(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Playtime>>> {
    Ok(Json(playtime_of(&state, auth.user_id).await?))
}

#[derive(Deserialize)]
pub struct SessionsQuery {
    game_id: Option<i64>,
    limit: Option<i64>,
}

pub async fn my_sessions(State(state): State<AppState>, auth: Auth, Query(query): Query<SessionsQuery>) -> ApiResult<Json<Vec<PlaySession>>> {
    Ok(Json(sessions_of(&state, auth.user_id, query.game_id, query.limit.unwrap_or(50).clamp(1, 500)).await?))
}

// ---------- achievements and stats ----------

/// Names come from games, so anything goes as long as it is short and printable.
fn key(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request("A name has 1 to 128 printable characters."));
    }
    Ok(name)
}

async fn unlocked_of(state: &AppState, user_id: i64, game_id: i64) -> ApiResult<Vec<Unlocked>> {
    let rows: Vec<(String, i64)> = sqlx::query_as("SELECT name, unlocked_at FROM user_achievements WHERE user_id = ? AND game_id = ? ORDER BY unlocked_at, name")
        .bind(user_id)
        .bind(game_id)
        .fetch_all(&state.db)
        .await?;
    Ok(rows.into_iter().map(|(name, unlocked_at)| Unlocked { name, unlocked_at }).collect())
}

pub async fn achievements(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>) -> ApiResult<Json<Achievements>> {
    visible_game(&state, &auth, game_id).await?;
    let defined: Vec<(String, String, String, bool)> =
        sqlx::query_as("SELECT name, title, description, hidden FROM achievements WHERE game_id = ? ORDER BY position").bind(game_id).fetch_all(&state.db).await?;
    Ok(Json(Achievements {
        defined: defined.into_iter().map(|(name, title, description, hidden)| AchievementDef { name, title, description, hidden }).collect(),
        unlocked: unlocked_of(&state, auth.user_id, game_id).await?,
    }))
}

/// Unlocking twice keeps the first time, as players expect.
pub async fn unlock(State(state): State<AppState>, auth: Auth, Path((game_id, name)): Path<(i64, String)>) -> ApiResult<StatusCode> {
    visible_game(&state, &auth, game_id).await?;
    sqlx::query("INSERT OR IGNORE INTO user_achievements (user_id, game_id, name, unlocked_at) VALUES (?, ?, ?, ?)")
        .bind(auth.user_id)
        .bind(game_id)
        .bind(key(&name)?)
        .bind(now())
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn clear_achievement(State(state): State<AppState>, auth: Auth, Path((game_id, name)): Path<(i64, String)>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM user_achievements WHERE user_id = ? AND game_id = ? AND name = ?").bind(auth.user_id).bind(game_id).bind(name.trim()).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// An admin describes a game's achievements. The list replaces the one before.
pub async fn define_achievements(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(game_id): Path<i64>,
    ApiJson(list): ApiJson<Vec<AchievementDef>>,
) -> ApiResult<StatusCode> {
    visible_game(&state, &admin, game_id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM achievements WHERE game_id = ?").bind(game_id).execute(&mut *tx).await?;
    for (position, def) in list.iter().enumerate() {
        sqlx::query("INSERT OR REPLACE INTO achievements (game_id, name, title, description, hidden, position) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(game_id)
            .bind(key(&def.name)?)
            .bind(def.title.trim())
            .bind(def.description.trim())
            .bind(def.hidden)
            .bind(position as i64)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn stats(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>) -> ApiResult<Json<Stats>> {
    visible_game(&state, &auth, game_id).await?;
    let rows: Vec<(String, f64)> = sqlx::query_as("SELECT name, value FROM user_stats WHERE user_id = ? AND game_id = ?").bind(auth.user_id).bind(game_id).fetch_all(&state.db).await?;
    Ok(Json(rows.into_iter().collect()))
}

pub async fn set_stats(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>, ApiJson(values): ApiJson<Stats>) -> ApiResult<Json<Stats>> {
    visible_game(&state, &auth, game_id).await?;
    if values.len() > 1000 {
        return Err(ApiError::bad_request("Send at most 1000 stats at a time."));
    }
    let mut tx = state.db.begin().await?;
    for (name, value) in &values {
        if !value.is_finite() {
            return Err(ApiError::bad_request(format!("The stat {name} is not a number.")));
        }
        sqlx::query("INSERT INTO user_stats (user_id, game_id, name, value) VALUES (?, ?, ?, ?) ON CONFLICT DO UPDATE SET value = excluded.value")
            .bind(auth.user_id)
            .bind(game_id)
            .bind(key(name)?)
            .bind(value)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    stats(State(state), auth, Path(game_id)).await
}

// ---------- collections and notes ----------

async fn load_collections(state: &AppState, user_id: i64) -> ApiResult<Vec<Collection>> {
    let collections: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM collections WHERE user_id = ? ORDER BY name COLLATE NOCASE").bind(user_id).fetch_all(&state.db).await?;
    let games: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT cg.collection_id, cg.game_id FROM collection_games cg JOIN collections c ON c.id = cg.collection_id WHERE c.user_id = ? ORDER BY cg.position",
    )
    .bind(user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(collections
        .into_iter()
        .map(|(id, name)| Collection { id, name, game_ids: games.iter().filter(|(collection, _)| *collection == id).map(|(_, game)| *game).collect() })
        .collect())
}

pub async fn collections(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Collection>>> {
    Ok(Json(load_collections(&state, auth.user_id).await?))
}

fn collection_name(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(ApiError::bad_request("A collection needs a name of up to 60 characters."));
    }
    Ok(name)
}

async fn write_collection(state: &AppState, user_id: i64, id: Option<i64>, patch: CollectionPatch) -> ApiResult<Collection> {
    let mut tx = state.db.begin().await?;
    let taken = |err: sqlx::Error| match err {
        sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::conflict("You already have a collection with that name."),
        other => other.into(),
    };
    let id = match id {
        None => {
            let name = collection_name(patch.name.as_deref().unwrap_or_default())?;
            sqlx::query_scalar("INSERT INTO collections (user_id, name) VALUES (?, ?) RETURNING id").bind(user_id).bind(name).fetch_one(&mut *tx).await.map_err(taken)?
        }
        Some(id) => {
            let owned: Option<i64> = sqlx::query_scalar("SELECT id FROM collections WHERE id = ? AND user_id = ?").bind(id).bind(user_id).fetch_optional(&mut *tx).await?;
            owned.ok_or_else(|| ApiError::not_found("You have no such collection."))?;
            if let Some(name) = &patch.name {
                sqlx::query("UPDATE collections SET name = ? WHERE id = ?").bind(collection_name(name)?).bind(id).execute(&mut *tx).await.map_err(taken)?;
            }
            id
        }
    };
    if let Some(game_ids) = patch.game_ids {
        sqlx::query("DELETE FROM collection_games WHERE collection_id = ?").bind(id).execute(&mut *tx).await?;
        for (position, game_id) in game_ids.iter().enumerate() {
            // Games that are gone are dropped quietly: a collection outlives what was in it.
            sqlx::query("INSERT OR IGNORE INTO collection_games (collection_id, game_id, position) SELECT ?, id, ? FROM games WHERE id = ?")
                .bind(id)
                .bind(position as i64)
                .bind(game_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    load_collections(state, user_id).await?.into_iter().find(|collection| collection.id == id).ok_or_else(|| ApiError::not_found("You have no such collection."))
}

pub async fn create_collection(State(state): State<AppState>, auth: Auth, ApiJson(patch): ApiJson<CollectionPatch>) -> ApiResult<Json<Collection>> {
    Ok(Json(write_collection(&state, auth.user_id, None, patch).await?))
}

pub async fn patch_collection(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(patch): ApiJson<CollectionPatch>) -> ApiResult<Json<Collection>> {
    Ok(Json(write_collection(&state, auth.user_id, Some(id), patch).await?))
}

pub async fn delete_collection(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM collections WHERE id = ? AND user_id = ?").bind(id).bind(auth.user_id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn note(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>) -> ApiResult<Json<Note>> {
    let row: Option<(String, i64)> = sqlx::query_as("SELECT text, updated_at FROM game_notes WHERE user_id = ? AND game_id = ?").bind(auth.user_id).bind(game_id).fetch_optional(&state.db).await?;
    Ok(Json(row.map(|(text, updated_at)| Note { text, updated_at: Some(updated_at) }).unwrap_or_default()))
}

pub async fn set_note(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>, ApiJson(note): ApiJson<Note>) -> ApiResult<Json<Note>> {
    visible_game(&state, &auth, game_id).await?;
    if note.text.len() > 100_000 {
        return Err(ApiError::bad_request("A note holds up to 100,000 characters."));
    }
    if note.text.trim().is_empty() {
        sqlx::query("DELETE FROM game_notes WHERE user_id = ? AND game_id = ?").bind(auth.user_id).bind(game_id).execute(&state.db).await?;
        return Ok(Json(Note::default()));
    }
    let updated_at = now();
    sqlx::query("INSERT INTO game_notes (user_id, game_id, text, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT DO UPDATE SET text = excluded.text, updated_at = excluded.updated_at")
        .bind(auth.user_id)
        .bind(game_id)
        .bind(&note.text)
        .bind(updated_at)
        .execute(&state.db)
        .await?;
    Ok(Json(Note { text: note.text, updated_at: Some(updated_at) }))
}

// ---------- what friends see ----------

/// The game someone is in right now, if any.
pub async fn now_playing(state: &AppState, user_id: i64) -> ApiResult<Option<NowPlaying>> {
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT game_id, started_at FROM play_sessions WHERE user_id = ?2 AND ended_at IS NULL AND last_beat_at >= ?1 - 180 ORDER BY started_at DESC LIMIT 1",
    )
    .bind(now())
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(game_id, since)| NowPlaying { game_id, since }))
}

/// A person's profile. Everyone sees their own in full; others see what the privacy switches allow.
pub async fn profile(State(state): State<AppState>, auth: Auth, Path(user_id): Path<i64>) -> ApiResult<Json<Profile>> {
    type Row = (i64, String, String, Option<i64>, bool, bool, bool);
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, username, role, last_seen_at, share_playtime, share_achievements, share_now_playing FROM users WHERE id = ? AND removed_at IS NULL",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?;
    let (id, username, role, last_seen_at, share_playtime, share_achievements, share_now_playing) =
        row.ok_or_else(|| ApiError::not_found("This Hearth has no such person."))?;
    let own = auth.user_id == user_id;

    let (playtime, recent) = if own || share_playtime {
        (Some(playtime_of(&state, user_id).await?), Some(sessions_of(&state, user_id, None, 10).await?))
    } else {
        (None, None)
    };
    let achievements = if own || share_achievements {
        let rows: Vec<(i64, String, i64)> =
            sqlx::query_as("SELECT game_id, name, unlocked_at FROM user_achievements WHERE user_id = ? ORDER BY game_id, unlocked_at").bind(user_id).fetch_all(&state.db).await?;
        let mut games: Vec<GameAchievements> = Vec::new();
        for (game_id, name, unlocked_at) in rows {
            match games.last_mut().filter(|game| game.game_id == game_id) {
                Some(game) => game.unlocked.push(Unlocked { name, unlocked_at }),
                None => games.push(GameAchievements { game_id, unlocked: vec![Unlocked { name, unlocked_at }] }),
            }
        }
        Some(games)
    } else {
        None
    };
    let now_playing = if own || share_now_playing { now_playing(&state, user_id).await? } else { None };
    Ok(Json(Profile { person: Person { id, username, role: role_from_db(&role), last_seen_at }, playtime, recent, achievements, now_playing }))
}

/// How everyone on the Hearth compares in one game, for those who share their playtime.
pub async fn game_players(State(state): State<AppState>, auth: Auth, Path(game_id): Path<i64>) -> ApiResult<Json<Vec<GamePlayer>>> {
    visible_game(&state, &auth, game_id).await?;
    let rows: Vec<(i64, String, i64, i64, bool, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT u.id, u.username, SUM(COALESCE({ENDED}, ?1) - p.started_at), MAX(p.started_at), u.share_achievements,
            (SELECT COUNT(*) FROM user_achievements a WHERE a.user_id = u.id AND a.game_id = ?2)
         FROM play_sessions p JOIN users u ON u.id = p.user_id
         WHERE p.game_id = ?2 AND u.removed_at IS NULL AND (u.share_playtime OR u.id = ?3)
         GROUP BY u.id ORDER BY 3 DESC"
    )))
    .bind(now())
    .bind(game_id)
    .bind(auth.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(user_id, username, seconds, last_played_at, shares, unlocked)| GamePlayer {
                user_id,
                username,
                seconds: seconds.max(0) as u64,
                last_played_at,
                achievements: (shares || user_id == auth.user_id).then_some(unlocked as u32),
            })
            .collect(),
    ))
}
