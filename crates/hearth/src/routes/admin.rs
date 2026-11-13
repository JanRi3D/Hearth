//! Settings and the folder tools behind them. Admins only.

use axum::{
    Json,
    extract::{Query, State},
};
use hearth_api::{
    admin::{Activity, AdminLobby, LibraryStats, Networking, NetworkingGame, Overview, PlayingNow, ServiceStatus},
    library::Players,
    multiplayer::Route,
    settings::{Browse, FolderScan, FolderScanRequest, SettingsPatch, SettingsView},
};

use super::{setup::BrowseQuery, system::VERSION};
use crate::{
    auth::{Admin, ApiJson},
    db::{self, now},
    error::{ApiError, ApiResult},
    fsutil, settings,
    state::AppState,
};

fn view(state: &AppState) -> SettingsView {
    SettingsView {
        settings: settings::public_view(&state.settings()),
        data_folder: state.paths.root().display().to_string(),
        version: VERSION.into(),
    }
}

pub async fn get_settings(State(state): State<AppState>, _admin: Admin) -> Json<SettingsView> {
    Json(view(&state))
}

pub async fn patch_settings(
    State(state): State<AppState>,
    Admin(admin): Admin,
    ApiJson(patch): ApiJson<SettingsPatch>,
) -> ApiResult<Json<SettingsView>> {
    let before = state.settings();
    let mut after = (*before).clone();
    settings::apply_patch(&mut after, patch);
    settings::validate(&mut after)?;
    if after.games_folders.is_empty() {
        return Err(ApiError::bad_request("Keep at least one games folder."));
    }
    // Requiring two-step sign-in while having none yourself would lock you out on the spot.
    if after.admin_totp_required && !before.admin_totp_required && !admin.totp_enabled {
        return Err(ApiError::bad_request("Set up two-step sign-in for your own account first, then require it for admins."));
    }
    if after == *before {
        return Ok(Json(view(&state)));
    }
    let rebind = (after.port, after.tls, &after.address) != (before.port, before.tls, &before.address);
    // New folders or new keys can change what the library holds.
    let rescan = after.games_folders != before.games_folders
        || (after.igdb_enabled, &after.igdb_client_id, &after.igdb_client_secret)
            != (before.igdb_enabled, &before.igdb_client_id, &before.igdb_client_secret)
        || (after.sgdb_enabled, &after.sgdb_api_key) != (before.sgdb_enabled, &before.sgdb_api_key);
    state.store_settings(after).await?;
    if rescan {
        state.library.request_scan();
    }
    db::log_activity(&state.db, format!("{} changed the settings", admin.username)).await;
    if rebind {
        // Answer first, then move: the browser needs the reply to know where to go next.
        let state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            state.rebind.notify_one();
        });
    }
    Ok(Json(view(&state)))
}

pub async fn browse(_admin: Admin, Query(query): Query<BrowseQuery>) -> ApiResult<Json<Browse>> {
    Ok(Json(tokio::task::spawn_blocking(move || fsutil::browse(&query.path)).await??))
}

pub async fn scan(_admin: Admin, ApiJson(request): ApiJson<FolderScanRequest>) -> ApiResult<Json<FolderScan>> {
    Ok(Json(tokio::task::spawn_blocking(move || fsutil::scan_summary(&request.path)).await?))
}

pub async fn overview(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Overview>> {
    let now = now();
    let count = |sql: &'static str| sqlx::query_scalar::<_, i64>(sql).bind(now).fetch_one(&state.db);
    let accounts = count("SELECT COUNT(*) FROM users WHERE removed_at IS NULL AND ?1 > 0").await?;
    let online_now = count("SELECT COUNT(*) FROM users WHERE removed_at IS NULL AND last_seen_at > ?1 - 300").await?;
    let devices = count("SELECT COUNT(*) FROM sessions WHERE expires_at IS NULL OR expires_at > ?1").await?;
    let activity: Vec<(i64, String)> = sqlx::query_as("SELECT at, text FROM activity ORDER BY id DESC LIMIT 8").fetch_all(&state.db).await?;

    let settings = state.settings();
    let games = crate::library::load_games(&state.db, true).await?;
    let review = count("SELECT COUNT(*) FROM sources WHERE state = 'review' AND ?1 > 0").await?;
    let library = LibraryStats {
        games: games.len() as u32,
        bytes: games.iter().map(|game| game.bytes).sum(),
        hidden: games.iter().filter(|game| !game.visible).count() as u32,
        folders: settings.games_folders.clone(),
        review: review as u32,
        watching: settings.watch_folders,
        threshold: settings.match_threshold,
        scan: state.library.status(&state.db).await,
    };

    // The chart: the busiest sample of each of the last 24 hours.
    let hour = now - now % 3600;
    let samples: Vec<(i64, i64)> = sqlx::query_as("SELECT at, people FROM online_samples WHERE at >= ?").bind(hour - 23 * 3600).fetch_all(&state.db).await?;
    let mut online_24h = vec![0u32; 24];
    for (at, people) in samples {
        let slot = ((at - (hour - 23 * 3600)) / 3600).clamp(0, 23) as usize;
        online_24h[slot] = online_24h[slot].max(people as u32);
    }
    online_24h[23] = online_24h[23].max(online_now as u32);

    let playing: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT u.username, g.title, p.started_at FROM play_sessions p JOIN users u ON u.id = p.user_id JOIN games g ON g.id = p.game_id
         WHERE p.ended_at IS NULL AND p.last_beat_at >= ?1 - 180 AND u.removed_at IS NULL ORDER BY p.started_at",
    )
    .bind(now)
    .fetch_all(&state.db)
    .await?;

    let mut services = Vec::new();
    if let Some(bound) = state.bound() {
        services.push(ServiceStatus {
            name: "API".into(),
            detail: format!("{} · port {}", if bound.https { "https" } else { "http" }, bound.addr.port()),
            ok: true,
        });
    }
    services.push(match state.multiplayer.relay_port().filter(|_| settings.relay_enabled) {
        Some(port) => ServiceStatus { name: "Bonfire relay".into(), detail: format!("udp · port {port}"), ok: true },
        None if settings.relay_enabled => ServiceStatus { name: "Bonfire relay".into(), detail: "could not listen on its port".into(), ok: false },
        None => ServiceStatus { name: "Bonfire relay".into(), detail: "switched off".into(), ok: false },
    });
    let rest_used = count("SELECT COALESCE(SUM(bytes), 0) FROM save_revisions WHERE ?1 > 0").await? as u64;
    services.push(ServiceStatus {
        name: "Rest storage".into(),
        detail: format!("{:.1} GB used", rest_used as f64 / (1024.0 * 1024.0 * 1024.0)),
        ok: true,
    });
    services.push(ServiceStatus {
        name: "Library watcher".into(),
        detail: if settings.watch_folders { settings.games_folders.join(", ") } else { "switched off".into() },
        ok: settings.watch_folders,
    });
    let providers: Vec<&str> = [
        crate::library::metadata::Providers::igdb_ready(&settings).then_some("IGDB"),
        crate::library::metadata::Providers::sgdb_ready(&settings).then_some("SteamGridDB"),
    ]
    .into_iter()
    .flatten()
    .collect();
    services.push(ServiceStatus {
        name: "Metadata".into(),
        detail: if providers.is_empty() { "no keys yet".into() } else { providers.join(", ") },
        ok: !providers.is_empty(),
    });
    let backup = crate::backup::latest(&state).await?;
    services.push(ServiceStatus {
        name: "Database backup".into(),
        detail: match (&backup, settings.backup_nightly) {
            (Some(backup), _) => format!("{:.1} MB, {}", backup.bytes as f64 / (1024.0 * 1024.0), backup.file),
            (None, true) => format!("first one at {}", settings.backup_time),
            (None, false) => "switched off".into(),
        },
        ok: backup.is_some() || settings.backup_nightly,
    });
    Ok(Json(Overview {
        version: VERSION.into(),
        uptime_seconds: state.started.elapsed().as_secs(),
        accounts: accounts as u32,
        online_now: online_now as u32,
        devices: devices as u32,
        library,
        online_24h,
        playing_now: playing.into_iter().map(|(username, game_title, since)| PlayingNow { username, game_title, since }).collect(),
        rest_used_bytes: rest_used,
        rest_quota_bytes: super::saves::quota_bytes(&state) * accounts as u64,
        lobbies: state.multiplayer.lobbies().len() as u32,
        relayed_lobbies: {
            let routes = state.multiplayer.routes();
            let mut relayed: Vec<i64> = routes.iter().filter(|(_, route)| *route == Route::Relay).filter_map(|(lobby, _)| *lobby).collect();
            relayed.sort_unstable();
            relayed.dedup();
            relayed.len() as u32
        },
        services,
        activity: activity.into_iter().map(|(at, text)| Activity { at, text }).collect(),
    }))
}

pub async fn networking(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Networking>> {
    let settings = state.settings();
    let games = crate::library::load_games(&state.db, true).await?;
    let usernames: Vec<(i64, String)> = sqlx::query_as("SELECT id, username FROM users").fetch_all(&state.db).await?;
    let routes = state.multiplayer.routes();
    let mut lobbies: Vec<AdminLobby> = state
        .multiplayer
        .lobbies()
        .into_iter()
        .map(|lobby| {
            let owner = usernames.iter().find(|(id, _)| *id == lobby.owner_id).map_or(String::new(), |(_, name)| name.clone());
            // A lobby counts as relayed as soon as one pair in it talks through the relay.
            let of_lobby = |route| routes.iter().any(|(lobby_id, found)| *lobby_id == Some(lobby.id) && *found == route);
            AdminLobby {
                id: lobby.id,
                name: lobby.data.get("name").filter(|name| !name.is_empty()).cloned().unwrap_or_else(|| format!("{owner}'s lobby")),
                game_title: games.iter().find(|game| game.id == lobby.game_id).map_or(String::new(), |game| game.title.clone()),
                owner,
                members: lobby.members.len() as u32,
                max_members: lobby.max_members,
                route: if of_lobby(Route::Relay) {
                    Route::Relay
                } else if of_lobby(Route::Direct) {
                    Route::Direct
                } else {
                    Route::Pending
                },
                created_at: lobby.created_at,
            }
        })
        .collect();
    lobbies.sort_by_key(|lobby| lobby.created_at);
    let (sessions, relayed_sessions) = state.multiplayer.p2p_count();
    let (calls_today, failed_today, recent_calls) = state.multiplayer.calls(now());
    Ok(Json(Networking {
        relay_enabled: settings.relay_enabled,
        relay_port: state.multiplayer.relay_port(),
        relay_bits_per_second: state.multiplayer.relay_rate(),
        sessions: sessions as u32,
        relayed_sessions: relayed_sessions as u32,
        lobbies,
        worlds: super::worlds::load(&state, None).await?,
        games: games
            .into_iter()
            .filter(|game| game.players != Players::Single)
            .map(|game| NetworkingGame { id: game.id, title: game.title, networking: game.networking })
            .collect(),
        calls_today,
        failed_today,
        recent_calls,
    }))
}
