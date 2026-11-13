//! The library as an admin runs it: rescans, the game editor, Metadata review and uploads.

use std::path::{Component, Path as FsPath};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::StatusCode,
};
use futures_util::StreamExt;
use hearth_api::library::{Game, GamePatch, Library, MetadataTest, ReviewAction, ReviewItem, VersionPatch, VersionRef};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use crate::{
    auth::{Admin, ApiJson},
    db::{self, now},
    error::{ApiError, ApiResult},
    fsutil,
    library::{
        self, kind_from_db,
        metadata::Meta,
        names, players_to_db,
        scan::{self, Source},
    },
    state::AppState,
};

pub async fn overview(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Library>> {
    let review: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sources WHERE state = 'review'").fetch_one(&state.db).await?;
    Ok(Json(Library {
        games: library::load_games(&state.db, true).await?,
        scan: state.library.status(&state.db).await,
        review: review as u32,
    }))
}

pub async fn rescan(State(state): State<AppState>, Admin(admin): Admin) -> StatusCode {
    tracing::info!(by = %admin.username, "rescan requested");
    state.library.request_scan();
    StatusCode::ACCEPTED
}

pub async fn game(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<Json<Game>> {
    library::load_game(&state.db, id).await?.map(Json).ok_or_else(|| ApiError::not_found("This Hearth has no such game."))
}

/// A path inside a game's folder, as an admin types it: relative, and never leading out.
fn game_path(path: &str) -> ApiResult<String> {
    let cleaned = path.trim().replace('\\', "/");
    let inside = !cleaned.is_empty() && FsPath::new(&cleaned).components().all(|part| matches!(part, Component::Normal(_)));
    if !inside {
        return Err(ApiError::bad_request(format!(
            "\"{path}\" is not a path inside the game's folder. Write it like bin/game.exe, without a drive or \"..\"."
        )));
    }
    Ok(cleaned)
}

pub async fn patch_game(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<i64>,
    ApiJson(patch): ApiJson<GamePatch>,
) -> ApiResult<Json<Game>> {
    let current = library::load_game(&state.db, id).await?.ok_or_else(|| ApiError::not_found("This Hearth has no such game."))?;
    let title = patch.title.map(|title| title.trim().to_string()).unwrap_or(current.summary.title.clone());
    if title.is_empty() || title.chars().count() > 200 {
        return Err(ApiError::bad_request("A game needs a title of up to 200 characters."));
    }
    let launch = patch
        .launch
        .map(|options| {
            let mut options = options
                .into_iter()
                .map(|mut option| {
                    option.path = game_path(&option.path)?;
                    option.name = option.name.trim().to_string();
                    if option.name.is_empty() {
                        return Err(ApiError::bad_request("Every way to start the game needs a name."));
                    }
                    Ok(option)
                })
                .collect::<ApiResult<Vec<_>>>()?;
            // Exactly one default: the first one marked, or simply the first.
            let default = options.iter().position(|option| option.default).unwrap_or(0);
            for (index, option) in options.iter_mut().enumerate() {
                option.default = index == default;
            }
            Ok::<_, ApiError>(options)
        })
        .transpose()?;
    let prerequisites = patch
        .prerequisites
        .map(|list| {
            list.into_iter()
                .map(|mut item| {
                    item.path = game_path(&item.path)?;
                    item.name = item.name.trim().to_string();
                    Ok(item)
                })
                .collect::<ApiResult<Vec<_>>>()
        })
        .transpose()?;

    if let Some(Some(emulator_id)) = patch.emulator_id {
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM emulators WHERE id = ?").bind(emulator_id).fetch_optional(&state.db).await?;
        exists.ok_or_else(|| ApiError::not_found("This Hearth has no such emulator."))?;
    }
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE games SET title = ?, developer = ?, year = ?, genre = ?, players = ?, description = ?, steam_app_id = ?,
         visible = ?, networking = ?, emulator_id = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&title)
    .bind(patch.developer.unwrap_or(current.summary.developer).trim())
    .bind(patch.year.unwrap_or(current.summary.year))
    .bind(patch.genre.unwrap_or(current.summary.genre).trim())
    .bind(players_to_db(patch.players.unwrap_or(current.summary.players)))
    .bind(patch.description.unwrap_or(current.description).trim())
    .bind(patch.steam_app_id.unwrap_or(current.steam_app_id))
    .bind(patch.visible.unwrap_or(current.summary.visible))
    .bind(patch.networking.unwrap_or(current.summary.networking))
    .bind(patch.emulator_id.unwrap_or(current.summary.emulator_id))
    .bind(now())
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if let Some(options) = launch {
        sqlx::query("DELETE FROM launch_options WHERE game_id = ?").bind(id).execute(&mut *tx).await?;
        for (position, option) in options.iter().enumerate() {
            sqlx::query("INSERT INTO launch_options (game_id, position, name, path, args, is_default) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(id)
                .bind(position as i64)
                .bind(&option.name)
                .bind(&option.path)
                .bind(option.args.trim())
                .bind(option.default)
                .execute(&mut *tx)
                .await?;
        }
    }
    if let Some(list) = prerequisites {
        sqlx::query("DELETE FROM prerequisites WHERE game_id = ?").bind(id).execute(&mut *tx).await?;
        for (position, item) in list.iter().enumerate() {
            sqlx::query("INSERT INTO prerequisites (game_id, position, name, path, args) VALUES (?, ?, ?, ?, ?)")
                .bind(id)
                .bind(position as i64)
                .bind(&item.name)
                .bind(&item.path)
                .bind(item.args.trim())
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    tracing::info!(game = id, by = %admin.username, "game edited");
    game(State(state), Admin(admin), Path(id)).await
}

pub async fn patch_version(
    State(state): State<AppState>,
    _admin: Admin,
    Path(id): Path<i64>,
    ApiJson(patch): ApiJson<VersionPatch>,
) -> ApiResult<StatusCode> {
    if patch.name.as_deref().is_some_and(|name| name.trim().is_empty()) {
        return Err(ApiError::bad_request("A version needs a name, for example 2.0.16."));
    }
    let changed = sqlx::query("UPDATE versions SET name = COALESCE(?, name), note = COALESCE(?, note) WHERE id = ?")
        .bind(patch.name.map(|name| name.trim().to_string()))
        .bind(patch.note.map(|note| note.trim().to_string()))
        .bind(id)
        .execute(&state.db)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::not_found("This Hearth has no such version."));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- Metadata review ----------

pub async fn review(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Vec<ReviewItem>>> {
    type Row = (i64, String, String, String, i64, String, Option<i64>, Option<String>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.name, s.folder, s.kind, s.bytes, s.candidates, g.id, g.title
         FROM sources s LEFT JOIN games g ON g.id = s.update_of WHERE s.state = 'review' ORDER BY s.name COLLATE NOCASE",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, folder, kind, bytes, candidates, game_id, game_title)| ReviewItem {
                id,
                version_guess: names::parse(&name).version,
                name,
                folder,
                kind: kind_from_db(&kind),
                bytes: bytes as u64,
                candidates: serde_json::from_str::<Vec<Meta>>(&candidates).unwrap_or_default().into_iter().map(|meta| meta.candidate).collect(),
                update_of: game_id.zip(game_title).map(|(id, name)| VersionRef { id, name }),
            })
            .collect(),
    ))
}

pub async fn decide(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<i64>,
    ApiJson(action): ApiJson<ReviewAction>,
) -> ApiResult<StatusCode> {
    let source: Option<Source> = sqlx::query_as("SELECT id, path, name, kind, signature, state FROM sources WHERE id = ? AND state = 'review'")
        .bind(id)
        .fetch_optional(&state.db)
        .await?;
    let source = source.ok_or_else(|| ApiError::not_found("That file is no longer waiting for review."))?;
    match action {
        ReviewAction::Accept { candidate } => {
            let stored: String = sqlx::query_scalar("SELECT candidates FROM sources WHERE id = ?").bind(id).fetch_one(&state.db).await?;
            let candidates: Vec<Meta> = serde_json::from_str(&stored).unwrap_or_default();
            let meta = candidates.get(candidate).ok_or_else(|| ApiError::bad_request("Pick one of the listed matches."))?;
            scan::create_game(&state, &source, Some(meta)).await?;
            db::log_activity(&state.db, format!("{} added {} from Metadata review", admin.username, meta.candidate.title)).await;
        }
        ReviewAction::AddPlain => {
            scan::create_game(&state, &source, None).await?;
            db::log_activity(&state.db, format!("{} added {} without details", admin.username, source.name)).await;
        }
        ReviewAction::Update { game_id, version, note } => {
            let title: Option<String> = sqlx::query_scalar("SELECT title FROM games WHERE id = ?").bind(game_id).fetch_optional(&state.db).await?;
            let title = title.ok_or_else(|| ApiError::not_found("This Hearth has no such game."))?;
            if version.trim().is_empty() {
                return Err(ApiError::bad_request("Name the version, for example 2.0.16."));
            }
            scan::add_version(&state, &source, game_id, &version, &note).await?;
            db::log_activity(&state.db, format!("{title} {} added. It is published once its files are hashed", version.trim())).await;
        }
        ReviewAction::Ignore => {
            sqlx::query("UPDATE sources SET state = 'ignored', candidates = '[]' WHERE id = ?").bind(id).execute(&state.db).await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Behind the Test button next to a key. Uses the keys as they are stored.
pub async fn test_metadata(State(state): State<AppState>, _admin: Admin, ApiJson(request): ApiJson<MetadataTest>) -> ApiResult<StatusCode> {
    state.library.providers.test(&state.settings(), request.provider).await.map_err(ApiError::bad_request)?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- uploads ----------

#[derive(Deserialize)]
pub struct UploadQuery {
    /// One of the games folders. Empty means the first.
    #[serde(default)]
    folder: String,
    name: String,
}

/// Takes one file into a games folder. The body is the file itself, streamed to disk, so a
/// game of many gigabytes never sits in memory.
pub async fn upload(State(state): State<AppState>, Admin(admin): Admin, Query(query): Query<UploadQuery>, body: Body) -> ApiResult<StatusCode> {
    let settings = state.settings();
    // The target is always one of the configured folders, never a path from the request.
    let folder = match query.folder.as_str() {
        "" => settings.games_folders.first(),
        wanted => settings.games_folders.iter().find(|folder| folder.as_str() == wanted),
    }
    .ok_or_else(|| ApiError::bad_request("Uploads go into one of the games folders from Settings."))?;

    let name = query.name.trim();
    let plain_name = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        && FsPath::new(name).file_name().and_then(|file| file.to_str()) == Some(name);
    if !plain_name || !fsutil::is_game_file(FsPath::new(name)) {
        return Err(ApiError::bad_request("Upload a .zip, .7z, .rar, .exe or .iso file with a plain file name."));
    }
    let target = FsPath::new(folder).join(name);
    if target.exists() {
        return Err(ApiError::conflict(format!("{name} is already in {folder}. Rename the file or remove the old one first.")));
    }

    // Written under a hidden name the scanner skips, then renamed, so a half-sent file is never listed.
    let partial = FsPath::new(folder).join(format!(".{name}.part"));
    let written: ApiResult<u64> = async {
        let mut out = tokio::fs::File::create(&partial).await?;
        let mut stream = body.into_data_stream();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ApiError::bad_request("The upload was cut off. Send the file again."))?;
            out.write_all(&chunk).await?;
            total += chunk.len() as u64;
        }
        out.flush().await?;
        Ok(total)
    }
    .await;
    let total = match written {
        Ok(total) if total > 0 => total,
        other => {
            let _ = tokio::fs::remove_file(&partial).await;
            other?;
            return Err(ApiError::bad_request("The upload was empty."));
        }
    };
    tokio::fs::rename(&partial, &target).await?;
    db::log_activity(&state.db, format!("{} uploaded {name}, {} MB", admin.username, total / (1024 * 1024))).await;
    state.library.request_scan();
    Ok(StatusCode::CREATED)
}
