//! Emulators: stored once in the emulators folder and shared by the games that need them,
//! so a player downloads one with the first such game and only game files after that.

use std::path::{Component, Path as FsPath};

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use hearth_api::{
    community::{Emulator, EmulatorInput, EmulatorManifest},
    library::{CHUNK_SIZE, ManifestFile},
};

use crate::{
    auth::{Admin, ApiJson, Auth},
    db::{self, now},
    error::{ApiError, ApiResult},
    library::manifest,
    state::AppState,
    tls::hex,
};

async fn load(state: &AppState) -> ApiResult<Vec<Emulator>> {
    type Row = (i64, String, String, String, String, String, Option<String>, Option<i64>, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT e.id, e.name, e.version, e.systems, e.command, e.folder, m.state, m.bytes, (SELECT COUNT(*) FROM games g WHERE g.emulator_id = e.id)
         FROM emulators e LEFT JOIN manifests m ON m.id = e.manifest_id ORDER BY e.name COLLATE NOCASE",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, version, systems, command, folder, manifest_state, bytes, games)| Emulator {
            id,
            name,
            version,
            systems: serde_json::from_str(&systems).unwrap_or_default(),
            command,
            folder,
            bytes: bytes.unwrap_or(0) as u64,
            ready: manifest_state.as_deref() == Some("ready"),
            games: games as u32,
        })
        .collect())
}

async fn one(state: &AppState, id: i64) -> ApiResult<Emulator> {
    load(state).await?.into_iter().find(|emulator| emulator.id == id).ok_or_else(no_emulator)
}

fn no_emulator() -> ApiError {
    ApiError::not_found("This Hearth has no such emulator.")
}

pub async fn list(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<Emulator>>> {
    Ok(Json(load(&state).await?))
}

/// The manifest of an emulator that is ready to download, with where its files are.
async fn ready_manifest(state: &AppState, id: i64) -> ApiResult<(i64, String)> {
    let row: Option<(Option<i64>, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT m.id, m.root, m.state FROM emulators e LEFT JOIN manifests m ON m.id = e.manifest_id WHERE e.id = ?").bind(id).fetch_optional(&state.db).await?;
    match row.ok_or_else(no_emulator)? {
        (Some(manifest_id), Some(root), Some(state)) if state == "ready" => Ok((manifest_id, root)),
        (_, _, Some(state)) if state == "pending" => Err(ApiError::conflict("The Hearth is still preparing this emulator. Try again in a moment.")),
        _ => Err(ApiError::conflict("The files of this emulator are not on the Hearth right now. Its admin can check the emulators folder.")),
    }
}

pub async fn emulator_manifest(State(state): State<AppState>, _auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<EmulatorManifest>> {
    let (manifest_id, _) = ready_manifest(&state, id).await?;
    let files = manifest::load(&state.db, manifest_id).await?;
    Ok(Json(EmulatorManifest {
        emulator_id: id,
        chunk_size: CHUNK_SIZE,
        bytes: files.iter().map(|file| file.size).sum(),
        files: files.into_iter().map(|file| ManifestFile { path: file.path, size: file.size, hash: hex(&file.hash), chunks: hex(&file.chunks) }).collect(),
    }))
}

pub async fn emulator_file(State(state): State<AppState>, _auth: Auth, Path((id, index)): Path<(i64, i64)>, headers: HeaderMap) -> ApiResult<Response> {
    let (manifest_id, root) = ready_manifest(&state, id).await?;
    super::serve_manifest_file(&state, manifest_id, &root, index, &headers).await
}

/// Checks what an admin typed and returns (name, version, systems as JSON, command, folder, where the files are).
fn check(state: &AppState, input: EmulatorInput) -> ApiResult<(String, String, String, String, String, String)> {
    let name = input.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(ApiError::bad_request("An emulator needs a name of up to 80 characters."));
    }
    let command = input.command.trim().to_string();
    if !command.contains("{rom}") {
        return Err(ApiError::bad_request("The start command must contain {rom}, which stands for the game file."));
    }
    let folder = if input.folder.trim().is_empty() { name.clone() } else { input.folder.trim().to_string() };
    // One plain folder name inside the emulators folder, nothing that could lead elsewhere.
    let mut parts = FsPath::new(&folder).components();
    if !matches!((parts.next(), parts.next()), (Some(Component::Normal(_)), None)) || folder.contains(['/', '\\']) {
        return Err(ApiError::bad_request("The folder is the name of one folder inside the emulators folder."));
    }
    let base = state.settings().emulators_folder.clone();
    let root = FsPath::new(&base).join(&folder);
    if !root.is_dir() {
        return Err(ApiError::bad_request(format!("Put the emulator's files under {} first.", root.display())));
    }
    let systems: Vec<String> = input.systems.iter().map(|system| system.trim().to_string()).filter(|system| !system.is_empty()).take(20).collect();
    Ok((name, input.version.trim().to_string(), serde_json::to_string(&systems)?, command, folder, root.display().to_string()))
}

fn name_taken(err: sqlx::Error) -> ApiError {
    match err {
        sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::conflict("There is already an emulator with that name."),
        other => other.into(),
    }
}

pub async fn create(State(state): State<AppState>, Admin(admin): Admin, ApiJson(input): ApiJson<EmulatorInput>) -> ApiResult<Json<Emulator>> {
    let (name, version, systems, command, folder, root) = check(&state, input)?;
    let now = now();
    let mut tx = state.db.begin().await?;
    let manifest_id: i64 = sqlx::query_scalar("INSERT INTO manifests (root, created_at) VALUES (?, ?) RETURNING id").bind(&root).bind(now).fetch_one(&mut *tx).await?;
    let id: i64 = sqlx::query_scalar("INSERT INTO emulators (name, version, systems, command, folder, manifest_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id")
        .bind(&name)
        .bind(version)
        .bind(systems)
        .bind(command)
        .bind(folder)
        .bind(manifest_id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(name_taken)?;
    tx.commit().await?;
    state.library.hash_wake.notify_one();
    db::log_activity(&state.db, format!("{} added the emulator {name}", admin.username)).await;
    Ok(Json(one(&state, id).await?))
}

pub async fn patch(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>, ApiJson(input): ApiJson<EmulatorInput>) -> ApiResult<Json<Emulator>> {
    let manifest_id: Option<i64> = sqlx::query_scalar("SELECT manifest_id FROM emulators WHERE id = ?").bind(id).fetch_optional(&state.db).await?.ok_or_else(no_emulator)?;
    let (name, version, systems, command, folder, root) = check(&state, input)?;
    sqlx::query("UPDATE emulators SET name = ?, version = ?, systems = ?, command = ?, folder = ? WHERE id = ?")
        .bind(name)
        .bind(version)
        .bind(systems)
        .bind(command)
        .bind(folder)
        .bind(id)
        .execute(&state.db)
        .await
        .map_err(name_taken)?;
    // The files may have moved or changed with the edit. Hashing again is cheap for what did not.
    if let Some(manifest_id) = manifest_id {
        sqlx::query("UPDATE manifests SET root = ?, state = 'pending' WHERE id = ?").bind(root).bind(manifest_id).execute(&state.db).await?;
        state.library.hash_wake.notify_one();
    }
    Ok(Json(one(&state, id).await?))
}

/// Removes the emulator from the Hearth's list. Its files stay where they are, and games
/// that used it go back to having no emulator.
pub async fn delete(State(state): State<AppState>, Admin(admin): Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let emulator = one(&state, id).await?;
    let manifest_id: Option<i64> = sqlx::query_scalar("SELECT manifest_id FROM emulators WHERE id = ?").bind(id).fetch_one(&state.db).await?;
    sqlx::query("DELETE FROM emulators WHERE id = ?").bind(id).execute(&state.db).await?;
    sqlx::query("DELETE FROM manifests WHERE id = ?").bind(manifest_id).execute(&state.db).await?;
    db::log_activity(&state.db, format!("{} removed the emulator {}", admin.username, emulator.name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// After a library scan: emulator files may have changed too, and the emulators folder may
/// have been moved in Settings. Hashing again only reads files that changed.
pub async fn refresh(state: &AppState) -> Result<(), sqlx::Error> {
    let base = state.settings().emulators_folder.clone();
    let emulators: Vec<(String, Option<i64>)> = sqlx::query_as("SELECT folder, manifest_id FROM emulators").fetch_all(&state.db).await?;
    for (folder, manifest_id) in emulators {
        if let Some(manifest_id) = manifest_id {
            sqlx::query("UPDATE manifests SET root = ?, state = 'pending' WHERE id = ?")
                .bind(FsPath::new(&base).join(&folder).display().to_string())
                .bind(manifest_id)
                .execute(&state.db)
                .await?;
        }
    }
    Ok(())
}
