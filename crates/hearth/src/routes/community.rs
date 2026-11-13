//! News and game nights, with the ready check that says whose PC still needs the game.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use hearth_api::{
    community::{Answer, GameNight, GuestReady, NewsPatch, NewsPost, PlanEvent, PublishNews, Ready, ReadyCheck, Rsvp, SetRsvp},
    library::VersionRef,
    social::NotificationKind,
};

use super::social::{self, To};
use crate::{
    auth::{Admin, ApiJson, Auth},
    db::{self, now},
    error::{ApiError, ApiResult},
    state::AppState,
};

// ---------- news ----------

pub async fn news(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<NewsPost>>> {
    let rows: Vec<(i64, String, String, bool, Option<String>, i64)> = sqlx::query_as(
        "SELECT n.id, n.title, n.body, n.pinned, u.username, n.published_at FROM news n LEFT JOIN users u ON u.id = n.author_id
         ORDER BY n.pinned DESC, n.published_at DESC, n.id DESC LIMIT 200",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, title, body, pinned, author, published_at)| NewsPost { id, title, body, pinned, author: author.unwrap_or_default(), published_at })
            .collect(),
    ))
}

fn check_post(title: &str, body: &str) -> ApiResult<()> {
    if title.trim().is_empty() || title.chars().count() > 200 {
        return Err(ApiError::bad_request("A post needs a title of up to 200 characters."));
    }
    if body.trim().is_empty() || body.len() > 100_000 {
        return Err(ApiError::bad_request("A post needs some text, up to 100,000 characters."));
    }
    Ok(())
}

pub async fn publish(State(state): State<AppState>, Admin(admin): Admin, ApiJson(post): ApiJson<PublishNews>) -> ApiResult<Json<NewsPost>> {
    check_post(&post.title, &post.body)?;
    let published_at = now();
    let id: i64 = sqlx::query_scalar("INSERT INTO news (title, body, pinned, author_id, published_at) VALUES (?, ?, ?, ?, ?) RETURNING id")
        .bind(post.title.trim())
        .bind(post.body.trim())
        .bind(post.pinned)
        .bind(admin.user_id)
        .bind(published_at)
        .fetch_one(&state.db)
        .await?;
    let hearth = state.settings().name.clone();
    social::notify(&state, To::Everyone(Some(admin.user_id)), NotificationKind::News, format!("New on {hearth}: {}", post.title.trim()), "", &[("news_id", id)]).await;
    db::log_activity(&state.db, format!("{} published \"{}\"", admin.username, post.title.trim())).await;
    Ok(Json(NewsPost { id, title: post.title.trim().into(), body: post.body.trim().into(), pinned: post.pinned, author: admin.username, published_at }))
}

pub async fn patch_news(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>, ApiJson(patch): ApiJson<NewsPatch>) -> ApiResult<StatusCode> {
    let current: Option<(String, String)> = sqlx::query_as("SELECT title, body FROM news WHERE id = ?").bind(id).fetch_optional(&state.db).await?;
    let (title, body) = current.ok_or_else(|| ApiError::not_found("That post does not exist any more."))?;
    let (title, body) = (patch.title.unwrap_or(title), patch.body.unwrap_or(body));
    check_post(&title, &body)?;
    sqlx::query("UPDATE news SET title = ?, body = ?, pinned = COALESCE(?, pinned) WHERE id = ?").bind(title.trim()).bind(body.trim()).bind(patch.pinned).bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_news(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM news WHERE id = ?").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- events ----------

fn rsvp_from_db(rsvp: &str) -> Rsvp {
    match rsvp {
        "going" => Rsvp::Going,
        "maybe" => Rsvp::Maybe,
        _ => Rsvp::No,
    }
}

/// Game nights that have not been over for more than six hours, soonest first.
async fn load_events(state: &AppState, viewer: i64, only: Option<i64>) -> ApiResult<Vec<GameNight>> {
    let rows: Vec<(i64, String, i64, i64, String, Option<String>)> = sqlx::query_as(
        "SELECT e.id, e.title, e.game_id, e.starts_at, e.note, u.username FROM events e LEFT JOIN users u ON u.id = e.created_by
         WHERE (?1 IS NULL AND e.starts_at > ?2 - 6 * 3600) OR e.id = ?1 ORDER BY e.starts_at, e.id",
    )
    .bind(only)
    .bind(now())
    .fetch_all(&state.db)
    .await?;
    let answers: Vec<(i64, i64, String, String)> = sqlx::query_as(
        "SELECT r.event_id, u.id, u.username, r.rsvp FROM event_rsvps r JOIN users u ON u.id = r.user_id WHERE u.removed_at IS NULL ORDER BY u.username COLLATE NOCASE",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, title, game_id, starts_at, note, host)| {
            let answers: Vec<Answer> = answers
                .iter()
                .filter(|(event, _, _, _)| *event == id)
                .map(|(_, user_id, username, rsvp)| Answer { user_id: *user_id, username: username.clone(), rsvp: rsvp_from_db(rsvp) })
                .collect();
            GameNight { id, title, game_id, starts_at, note, host: host.unwrap_or_default(), mine: answers.iter().find(|answer| answer.user_id == viewer).map(|answer| answer.rsvp), answers }
        })
        .collect())
}

fn no_event() -> ApiError {
    ApiError::not_found("That game night does not exist any more.")
}

pub async fn events(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<GameNight>>> {
    Ok(Json(load_events(&state, auth.user_id, None).await?))
}

pub async fn rsvp(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(answer): ApiJson<SetRsvp>) -> ApiResult<Json<GameNight>> {
    load_events(&state, auth.user_id, Some(id)).await?.into_iter().next().ok_or_else(no_event)?;
    match answer.rsvp {
        Some(rsvp) => {
            let text = match rsvp {
                Rsvp::Going => "going",
                Rsvp::Maybe => "maybe",
                Rsvp::No => "no",
            };
            sqlx::query("INSERT INTO event_rsvps (event_id, user_id, rsvp) VALUES (?, ?, ?) ON CONFLICT DO UPDATE SET rsvp = excluded.rsvp")
                .bind(id)
                .bind(auth.user_id)
                .bind(text)
                .execute(&state.db)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM event_rsvps WHERE event_id = ? AND user_id = ?").bind(id).bind(auth.user_id).execute(&state.db).await?;
        }
    }
    load_events(&state, auth.user_id, Some(id)).await?.into_iter().next().map(Json).ok_or_else(no_event)
}

pub async fn plan(State(state): State<AppState>, Admin(admin): Admin, ApiJson(plan): ApiJson<PlanEvent>) -> ApiResult<Json<GameNight>> {
    let title = plan.title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(ApiError::bad_request("A game night needs a name of up to 120 characters."));
    }
    if plan.starts_at <= now() {
        return Err(ApiError::bad_request("Pick a start time that is still ahead."));
    }
    let game: Option<String> = sqlx::query_scalar("SELECT title FROM games WHERE id = ?").bind(plan.game_id).fetch_optional(&state.db).await?;
    let game = game.ok_or_else(|| ApiError::not_found("This Hearth has no such game."))?;
    let id: i64 = sqlx::query_scalar("INSERT INTO events (title, game_id, starts_at, note, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?) RETURNING id")
        .bind(title)
        .bind(plan.game_id)
        .bind(plan.starts_at)
        .bind(plan.note.trim())
        .bind(admin.user_id)
        .bind(now())
        .fetch_one(&state.db)
        .await?;
    // The time is left to the client, which knows the reader's time zone.
    social::notify(&state, To::Everyone(Some(admin.user_id)), NotificationKind::Event, format!("{title} was planned"), format!("A game night for {game}. Say if you are going."), &[("event_id", id), ("game_id", plan.game_id)]).await;
    db::log_activity(&state.db, format!("{} planned {title}", admin.username)).await;
    load_events(&state, admin.user_id, Some(id)).await?.into_iter().next().map(Json).ok_or_else(no_event)
}

pub async fn delete_event(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM events WHERE id = ?").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Sent by the janitor: a nudge to everyone who is going or might, an hour before the start.
pub async fn remind(state: &AppState) -> Result<(), sqlx::Error> {
    let now = now();
    let due: Vec<(i64, String, i64)> =
        sqlx::query_as("SELECT id, title, game_id FROM events WHERE reminded_at IS NULL AND starts_at > ? AND starts_at <= ?").bind(now).bind(now + 3600).fetch_all(&state.db).await?;
    for (id, title, game_id) in due {
        sqlx::query("UPDATE events SET reminded_at = ? WHERE id = ?").bind(now).bind(id).execute(&state.db).await?;
        let guests: Vec<i64> = sqlx::query_scalar("SELECT user_id FROM event_rsvps WHERE event_id = ? AND rsvp IN ('going', 'maybe')").bind(id).fetch_all(&state.db).await?;
        for user_id in guests {
            social::notify(state, To::User(user_id), NotificationKind::Event, format!("{title} starts within the hour"), "", &[("event_id", id), ("game_id", game_id)]).await;
        }
    }
    Ok(())
}

// ---------- the ready check ----------

/// The newest version of a game that can be downloaded.
async fn current_version(state: &AppState, game_id: i64) -> ApiResult<Option<VersionRef>> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT v.id, v.name FROM versions v JOIN manifests m ON m.id = v.manifest_id WHERE v.game_id = ? AND m.state = 'ready' ORDER BY v.id DESC LIMIT 1",
    )
    .bind(game_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(row.map(|(id, name)| VersionRef { id, name }))
}

async fn ready_check(state: &AppState, event_id: i64) -> ApiResult<ReadyCheck> {
    let event = load_events(state, 0, Some(event_id)).await?.into_iter().next().ok_or_else(no_event)?;
    let version = current_version(state, event.game_id).await?;
    let mut guests = Vec::new();
    for answer in event.answers.iter().filter(|answer| answer.rsvp == Rsvp::Going) {
        // What each PC of this guest has of the game, and whether something is on its way.
        let installed: Vec<i64> =
            sqlx::query_scalar("SELECT dg.version_id FROM device_games dg JOIN devices d ON d.id = dg.device_id WHERE d.user_id = ? AND dg.game_id = ?")
                .bind(answer.user_id)
                .bind(event.game_id)
                .fetch_all(&state.db)
                .await?;
        let underway: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM download_queue q JOIN devices d ON d.id = q.device_id WHERE d.user_id = ? AND q.game_id = ? AND q.state IN ('queued', 'started')",
        )
        .bind(answer.user_id)
        .bind(event.game_id)
        .fetch_one(&state.db)
        .await?;
        let ready = match &version {
            Some(version) if installed.contains(&version.id) => Ready::Ready,
            _ if underway > 0 => Ready::GettingReady,
            _ if !installed.is_empty() => Ready::NeedsUpdate,
            _ => Ready::NotKindled,
        };
        guests.push(GuestReady { user_id: answer.user_id, username: answer.username.clone(), ready });
    }
    Ok(ReadyCheck { event_id, game_id: event.game_id, version, guests })
}

pub async fn ready(State(state): State<AppState>, _auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<ReadyCheck>> {
    Ok(Json(ready_check(&state, id).await?))
}

/// Queues the game, or its update, for every guest who is not ready. It goes to the PC that
/// already has the game, or else to the one they used last.
pub async fn get_everyone_ready(State(state): State<AppState>, Admin(admin): Admin, Path(id): Path<i64>) -> ApiResult<Json<ReadyCheck>> {
    let check = ready_check(&state, id).await?;
    for guest in check.guests.iter().filter(|guest| matches!(guest.ready, Ready::NeedsUpdate | Ready::NotKindled)) {
        let device: Option<i64> = sqlx::query_scalar(
            "SELECT d.id FROM devices d LEFT JOIN device_games dg ON dg.device_id = d.id AND dg.game_id = ?1
             WHERE d.user_id = ?2 AND d.client != 'Web admin' AND EXISTS (SELECT 1 FROM sessions s WHERE s.device_id = d.id)
             ORDER BY dg.version_id IS NULL, d.last_seen_at DESC LIMIT 1",
        )
        .bind(check.game_id)
        .bind(guest.user_id)
        .fetch_optional(&state.db)
        .await?;
        // A guest without a PC that is signed in cannot be helped from here.
        if let Some(device_id) = device
            && let Err(err) = super::people::queue_for(&state, device_id, check.game_id, admin.user_id).await
        {
            tracing::warn!(guest = %guest.username, error = %err.message, "could not queue the game for a guest");
        }
    }
    Ok(Json(ready_check(&state, id).await?))
}
