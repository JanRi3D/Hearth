//! Backups and updates, for the Maintenance group in Settings.

use axum::{Json, extract::State, http::StatusCode};
use hearth_api::admin::{Maintenance, UpdateCheck};

use super::VERSION;
use crate::{
    auth::Admin,
    backup,
    db::{self, now},
    error::{ApiError, ApiResult},
    state::AppState,
    update,
};

async fn view(state: &AppState) -> ApiResult<Maintenance> {
    Ok(Maintenance {
        version: VERSION.into(),
        backup: backup::latest(state).await?,
        backup_folder: state.paths.dir("backups").display().to_string(),
        update: state.update.lock().unwrap().as_ref().map(|(check, _)| check.clone()),
    })
}

pub async fn get(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Maintenance>> {
    Ok(Json(view(&state).await?))
}

pub async fn backup_now(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Maintenance>> {
    backup::run(&state).await?;
    Ok(Json(view(&state).await?))
}

fn unreachable(message: String) -> ApiError {
    ApiError::coded(StatusCode::BAD_GATEWAY, hearth_api::ErrorCode::Internal, format!("Could not look for a newer version. {message}"))
}

/// Looks for a newer release. Only when an admin asks: the Hearth does not call out on its own.
pub async fn check_update(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Maintenance>> {
    let release = update::latest(&state.opts.update_api).await.map_err(unreachable)?;
    let check = UpdateCheck { available: update::newer_than_running(&release.version), latest: release.version.clone(), checked_at: now() };
    *state.update.lock().unwrap() = Some((check, release));
    Ok(Json(view(&state).await?))
}

/// Downloads the release found by the last check, puts it in place and starts the Hearth again.
pub async fn update(State(state): State<AppState>, Admin(admin): Admin) -> ApiResult<StatusCode> {
    let found = state.update.lock().unwrap().clone();
    let Some((check, release)) = found.filter(|(check, _)| check.available) else {
        return Err(ApiError::conflict("There is no newer version to update to. Check for updates first."));
    };
    let folder = std::env::current_exe()?.parent().map(|dir| dir.to_path_buf()).ok_or_else(|| ApiError::conflict("The Hearth cannot find its own folder."))?;
    let file = update::download(&state.opts.update_api, &release, &folder).await.map_err(|message| ApiError::coded(StatusCode::BAD_GATEWAY, hearth_api::ErrorCode::Internal, message))?;
    update::install(&file).map_err(|err| ApiError::conflict(format!("The new version could not be put in place: {err}. The Hearth keeps running as it is.")))?;
    db::log_activity(&state.db, format!("{} updated the Hearth to {}", admin.username, check.latest)).await;
    // Answer first, then restart: the page needs the reply to know it should wait.
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let err = update::restart();
        tracing::error!(error = %err, "the new version is in place but could not be started. Start the Hearth again by hand");
    });
    Ok(StatusCode::ACCEPTED)
}
