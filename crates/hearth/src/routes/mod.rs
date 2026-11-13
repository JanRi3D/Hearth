//! Every HTTP route. The API lives under `/api/v1`; everything else is the web UI.

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{delete, get, patch, post, put},
};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::{error::ApiError, state::AppState, web};

mod admin;
mod auth;
pub mod community;
pub mod emulators;
pub mod firelink;
mod games;
mod library;
mod maintenance;
pub mod multiplayer;
pub mod people;
mod play;
pub mod saves;
pub mod social;
mod setup;
mod system;
pub mod worlds;

pub use games::{parse_range, serve_manifest_file};
pub use system::VERSION;

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(system::health))
        .route("/hearth", get(system::info))
        // First-time setup. Closed for good once an admin exists.
        .route("/setup/begin", post(setup::begin))
        .route("/setup/browse", get(setup::browse))
        .route("/setup/scan", post(setup::scan))
        .route("/setup/complete", post(setup::complete))
        // Accounts and sessions.
        .route("/auth/login", post(auth::login))
        .route("/auth/handoff", post(auth::handoff))
        .route("/auth/register", post(auth::register))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/password", post(auth::change_password))
        .route("/auth/sessions", get(auth::sessions))
        .route("/auth/sessions/{id}", delete(auth::revoke_session))
        .route("/auth/totp/setup", post(auth::totp_setup))
        .route("/auth/totp/enable", post(auth::totp_enable))
        .route("/auth/totp/disable", post(auth::totp_disable))
        .route("/auth/reset", post(people::reset_password))
        .route("/auth/reset/{token}", get(people::reset_info))
        .route("/me", get(auth::me))
        .route("/me/privacy", get(people::privacy).patch(people::patch_privacy))
        .route("/me/playtime", get(play::my_playtime))
        .route("/me/sessions", get(play::my_sessions))
        // People and their devices.
        .route("/users", get(people::people))
        .route("/users/{id}/profile", get(play::profile))
        .route("/devices", get(people::devices))
        .route("/devices/current/installed", put(people::report_installed))
        .route("/devices/current/queue", get(people::my_queue))
        .route("/devices/current/queue/{id}", post(people::report_download))
        // Presence, chat and notifications, delivered live over one WebSocket.
        .route("/ws", get(social::ws))
        .route("/presence", get(social::presence).put(social::set_presence))
        .route("/chats", get(social::conversations))
        .route("/chats/{user_id}/messages", get(social::messages).post(social::send_message))
        .route("/chats/{user_id}/read", post(social::mark_chat_read))
        .route("/notifications", get(social::notifications))
        .route("/notifications/read", post(social::mark_notifications_read))
        // Multiplayer services: what the Steam API replacement talks to.
        .route("/tickets", post(multiplayer::issue_ticket))
        .route("/tickets/verify", post(multiplayer::verify_ticket))
        .route("/tickets/{ticket}", delete(multiplayer::cancel_ticket))
        .route("/games/by-steam-app/{app_id}", get(multiplayer::game_by_steam_app))
        .route("/lobbies", get(multiplayer::my_lobbies).post(multiplayer::create_lobby))
        .route("/lobbies/search", post(multiplayer::search_lobbies))
        .route("/lobbies/{id}", get(multiplayer::lobby).patch(multiplayer::patch_lobby))
        .route("/lobbies/{id}/join", post(multiplayer::join_lobby))
        .route("/lobbies/{id}/leave", post(multiplayer::leave_lobby))
        .route("/lobbies/{id}/members/me", put(multiplayer::set_member_data))
        .route("/lobbies/{id}/chat", post(multiplayer::lobby_chat))
        .route("/invites", post(multiplayer::invite))
        .route("/users/{id}/join-request", post(multiplayer::join_request))
        .route("/p2p/sessions", post(multiplayer::open_p2p))
        .route("/p2p/sessions/{id}", get(multiplayer::p2p).delete(multiplayer::close_p2p))
        .route("/p2p/sessions/{id}/signal", post(multiplayer::signal))
        .route("/p2p/sessions/{id}/route", post(multiplayer::report_route))
        // News, game nights, emulators and the Firelink server registry.
        .route("/news", get(community::news))
        .route("/events", get(community::events))
        .route("/events/{id}/rsvp", put(community::rsvp))
        .route("/events/{id}/ready", get(community::ready))
        .route("/emulators", get(emulators::list))
        .route("/emulators/{id}/manifest", get(emulators::emulator_manifest))
        .route("/emulators/{id}/files/{index}", get(emulators::emulator_file))
        .route("/game-servers", get(firelink::list))
        .route("/game-servers/heartbeat", post(firelink::heartbeat))
        // Rest: cloud saves and shared worlds. Their files travel over WebDAV at /dav.
        .route("/saves", get(saves::list))
        .route("/saves/{game_id}", get(saves::game).delete(saves::delete))
        .route("/saves/{game_id}/commit", post(saves::commit))
        .route("/saves/{game_id}/restore", post(saves::restore))
        .route("/worlds", get(worlds::list).post(worlds::create))
        .route("/worlds/{id}", get(worlds::get).delete(worlds::delete))
        .route("/worlds/{id}/members", post(worlds::add_member))
        .route("/worlds/{id}/members/{user_id}", delete(worlds::remove_member))
        .route("/worlds/{id}/host", post(worlds::host))
        .route("/worlds/{id}/heartbeat", post(worlds::heartbeat))
        .route("/worlds/{id}/release", post(worlds::release))
        .route("/worlds/{id}/journal", get(worlds::journal).post(worlds::add_note))
        // Play data.
        .route("/play/sessions", post(play::start_session))
        .route("/play/sessions/{id}/heartbeat", post(play::heartbeat))
        .route("/play/sessions/{id}/stop", post(play::stop_session))
        .route("/games/{id}/achievements", get(play::achievements))
        .route("/games/{id}/achievements/{name}", put(play::unlock).delete(play::clear_achievement))
        .route("/games/{id}/stats", get(play::stats).put(play::set_stats))
        .route("/games/{id}/note", get(play::note).put(play::set_note))
        .route("/games/{id}/players", get(play::game_players))
        .route("/collections", get(play::collections).post(play::create_collection))
        .route("/collections/{id}", patch(play::patch_collection).delete(play::delete_collection))
        // The library, as clients see it.
        .route("/games", get(games::list))
        .route("/games/{id}", get(games::detail))
        .route("/versions/{id}/manifest", get(games::version_manifest))
        .route("/versions/{id}/diff", get(games::version_diff))
        .route("/versions/{id}/files/{index}", get(games::version_file))
        .route("/art/{name}", get(games::art))
        // Admin.
        .route("/admin/library", get(library::overview))
        .route("/admin/library/scan", post(library::rescan))
        // An upload is a whole game, so the usual size limit for request bodies is lifted here.
        .route("/admin/library/upload", put(library::upload).layer(DefaultBodyLimit::disable()))
        .route("/admin/games/{id}", get(library::game).patch(library::patch_game))
        .route("/admin/versions/{id}", patch(library::patch_version))
        .route("/admin/review", get(library::review))
        .route("/admin/review/{id}", post(library::decide))
        .route("/admin/metadata/test", post(library::test_metadata))
        .route("/admin/games/{id}/achievements", put(play::define_achievements))
        .route("/admin/users", get(people::users))
        .route("/admin/users/{id}", patch(people::patch_user).delete(people::remove_user))
        .route("/admin/users/{id}/reset-link", post(people::create_reset_link))
        .route("/admin/reset-links", get(people::reset_links))
        .route("/admin/reset-links/{id}", delete(people::delete_reset_link))
        .route("/admin/invites", get(people::invites).post(people::create_invite))
        .route("/admin/invites/{id}", delete(people::delete_invite))
        .route("/admin/sessions", get(people::all_sessions))
        .route("/admin/devices", get(people::all_devices))
        .route("/admin/downloads", get(people::queue).post(people::queue_download))
        .route("/admin/downloads/{id}", delete(people::cancel_download))
        .route("/admin/overview", get(admin::overview))
        .route("/admin/networking", get(admin::networking))
        .route("/admin/storage", get(saves::admin_storage))
        .route("/admin/maintenance", get(maintenance::get))
        .route("/admin/backup", post(maintenance::backup_now))
        .route("/admin/update/check", post(maintenance::check_update))
        .route("/admin/update", post(maintenance::update))
        .route("/admin/news", post(community::publish))
        .route("/admin/news/{id}", patch(community::patch_news).delete(community::delete_news))
        .route("/admin/events", post(community::plan))
        .route("/admin/events/{id}", delete(community::delete_event))
        .route("/admin/events/{id}/ready", post(community::get_everyone_ready))
        .route("/admin/emulators", post(emulators::create))
        .route("/admin/emulators/{id}", patch(emulators::patch).delete(emulators::delete))
        .route("/admin/game-servers", get(firelink::registry).post(firelink::add))
        .route("/admin/game-servers/token", post(firelink::rotate_token))
        .route("/admin/game-servers/{id}", delete(firelink::remove))
        .route("/admin/settings", get(admin::get_settings).patch(admin::patch_settings))
        .route("/admin/folders/browse", get(admin::browse))
        .route("/admin/folders/scan", post(admin::scan))
        .fallback(|| async { ApiError::not_found("The Hearth has no such endpoint.") })
        .layer(axum::middleware::from_fn_with_state(state.clone(), multiplayer::record_calls));

    let mut app = Router::new().nest("/api/v1", api).merge(crate::dav::router()).merge(web::router());
    for (name, value) in web::security_headers() {
        app = app.layer(SetResponseHeaderLayer::if_not_present(name, value));
    }
    app.with_state(state)
}
