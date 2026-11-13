//! What anyone may ask: is the Hearth up, and what is it called.

use axum::{Json, extract::State};
use hearth_api::{API_VERSION, Health, HearthInfo};

use crate::{db::now, state::AppState};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health { status: "ok".into(), version: VERSION.into(), uptime_seconds: state.started.elapsed().as_secs() })
}

pub async fn info(State(state): State<AppState>) -> Json<HearthInfo> {
    let settings = state.settings();
    let bound = state.bound();
    Json(HearthInfo {
        name: settings.name.clone(),
        version: VERSION.into(),
        api_version: API_VERSION,
        setup_complete: state.setup_complete(),
        join: settings.join,
        tls: settings.tls,
        cert_sha256: bound.and_then(|bound| bound.cert_sha256),
        relay_port: state.multiplayer.relay_port().filter(|_| settings.relay_enabled),
        time: now(),
    })
}
