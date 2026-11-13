//! Multiplayer services: tickets, lobbies, invites and peer-to-peer sessions. This is what
//! the Steam API replacement in Bonfire calls instead of Steam.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use hearth_api::{
    ErrorCode,
    library::GameSummary,
    multiplayer::{
        CreateLobby, Invite, JoinRequest, JoinRequested, Lobby, LobbyChat, LobbyChatMessage, LobbyKind, LobbyMember, LobbyPatch, LobbySearch, OpenP2p,
        P2pSession, P2pSignal, P2pSignalled, ReportRoute, SendInvite, Ticket, TicketInfo, TicketRequest, VerifyTicket,
    },
    social::{Event, LobbyRef, NotificationKind, P2pRef},
};

use super::social::{self, To};
use crate::{
    auth::{ApiJson, Auth},
    db::now,
    error::{ApiError, ApiResult},
    multiplayer,
    state::AppState,
};

async fn game_title(state: &AppState, auth: &Auth, game_id: i64) -> ApiResult<String> {
    let row: Option<(String, bool)> = sqlx::query_as("SELECT title, visible FROM games WHERE id = ?").bind(game_id).fetch_optional(&state.db).await?;
    match row {
        Some((title, visible)) if visible || auth.is_admin() => Ok(title),
        _ => Err(ApiError::not_found("This Hearth has no such game.")),
    }
}

async fn person(state: &AppState, user_id: i64) -> ApiResult<String> {
    let username: Option<String> = sqlx::query_scalar("SELECT username FROM users WHERE id = ? AND removed_at IS NULL").bind(user_id).fetch_optional(&state.db).await?;
    username.ok_or_else(|| ApiError::not_found("This Hearth has no such person."))
}

/// Lobbies and sessions belong to a live connection: without one, nobody could tell the
/// client what happens in them, or notice when it is gone.
fn connected(state: &AppState, auth: &Auth) -> ApiResult<()> {
    if state.hub.is_online(auth.user_id) {
        return Ok(());
    }
    Err(ApiError::conflict("Connect to the Hearth's WebSocket first. Lobbies and sessions belong to a live connection."))
}

fn no_lobby() -> ApiError {
    ApiError::not_found("That lobby does not exist any more.")
}

// ---------- tickets and ownership ----------

pub async fn issue_ticket(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<TicketRequest>) -> ApiResult<Json<Ticket>> {
    game_title(&state, &auth, request.game_id).await?;
    let (ticket, expires_at) = state.multiplayer.issue_ticket(auth.user_id, auth.username.clone(), request.game_id, now());
    Ok(Json(Ticket { ticket, expires_at }))
}

/// Whoever was handed a ticket asks here who it stands for.
pub async fn verify_ticket(State(state): State<AppState>, _caller: super::firelink::ServerOrUser, ApiJson(request): ApiJson<VerifyTicket>) -> ApiResult<Json<TicketInfo>> {
    let mut info = state
        .multiplayer
        .verify_ticket(&request.ticket, now())
        .ok_or_else(|| ApiError::not_found("That ticket is not valid. It may have expired or been cancelled."))?;
    // Ownership is the library's answer: the game is there, and visible to players.
    let visible: Option<bool> = sqlx::query_scalar("SELECT visible FROM games WHERE id = ?").bind(info.game_id).fetch_optional(&state.db).await?;
    info.owned = visible == Some(true);
    Ok(Json(info))
}

pub async fn cancel_ticket(State(state): State<AppState>, auth: Auth, Path(ticket): Path<String>) -> StatusCode {
    state.multiplayer.cancel_ticket(&ticket, auth.user_id);
    StatusCode::NO_CONTENT
}

/// Finds a game by its Steam app id. A game that is found is a game the caller owns.
pub async fn game_by_steam_app(State(state): State<AppState>, auth: Auth, Path(app_id): Path<u32>) -> ApiResult<Json<GameSummary>> {
    let id: Option<i64> = sqlx::query_scalar("SELECT id FROM games WHERE steam_app_id = ? AND (visible OR ?) ORDER BY id LIMIT 1")
        .bind(app_id)
        .bind(auth.is_admin())
        .fetch_optional(&state.db)
        .await?;
    let games = crate::library::load_games(&state.db, true).await?;
    games.into_iter().find(|game| Some(game.id) == id).map(Json).ok_or_else(|| ApiError::not_found("No game on this Hearth has that Steam app id."))
}

// ---------- lobbies ----------

/// Tells every member what the lobby looks like now.
fn broadcast(state: &AppState, lobby: &Lobby) {
    for member in &lobby.members {
        state.hub.send(member.user_id, &Event::LobbyUpdated(lobby.clone()));
    }
}

fn is_member(lobby: &Lobby, user_id: i64) -> bool {
    lobby.members.iter().any(|member| member.user_id == user_id)
}

pub async fn create_lobby(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<CreateLobby>) -> ApiResult<Json<Lobby>> {
    connected(&state, &auth)?;
    game_title(&state, &auth, request.game_id).await?;
    if !(1..=250).contains(&request.max_members) {
        return Err(ApiError::bad_request("A lobby holds 1 to 250 people."));
    }
    let owner = LobbyMember { user_id: auth.user_id, username: auth.username.clone(), data: BTreeMap::new() };
    let lobby = state.multiplayer.create_lobby(request.game_id, owner, request.kind, request.max_members, request.data, now());
    social::announce(&state, auth.user_id).await;
    Ok(Json(lobby))
}

pub async fn lobby(State(state): State<AppState>, _auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<Lobby>> {
    state.multiplayer.lobby(id).map(Json).ok_or_else(no_lobby)
}

/// The lobbies the caller is in.
pub async fn my_lobbies(State(state): State<AppState>, auth: Auth) -> Json<Vec<Lobby>> {
    Json(state.multiplayer.lobbies_of(auth.user_id).into_iter().filter_map(|id| state.multiplayer.lobby(id)).collect())
}

pub async fn search_lobbies(State(state): State<AppState>, auth: Auth, ApiJson(search): ApiJson<LobbySearch>) -> ApiResult<Json<Vec<Lobby>>> {
    game_title(&state, &auth, search.game_id).await?;
    Ok(Json(state.multiplayer.search(&search)))
}

pub async fn join_lobby(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<Lobby>> {
    connected(&state, &auth)?;
    let member = LobbyMember { user_id: auth.user_id, username: auth.username.clone(), data: BTreeMap::new() };
    let (joined, lobby) = state
        .multiplayer
        .with_lobby(id, |lobby_state| {
            let lobby = &mut lobby_state.lobby;
            if is_member(lobby, member.user_id) {
                return Ok(false);
            }
            let invited = lobby_state.invited.contains(&member.user_id);
            if !lobby.joinable || (lobby.kind == LobbyKind::Private && !invited) {
                return Err(ApiError::coded(StatusCode::FORBIDDEN, ErrorCode::LobbyLocked, "This lobby is not open. Ask someone in it for an invite."));
            }
            if lobby.members.len() as u32 >= lobby.max_members {
                return Err(ApiError::coded(StatusCode::CONFLICT, ErrorCode::LobbyFull, "This lobby is full."));
            }
            lobby.members.push(member);
            lobby_state.invited.remove(&auth.user_id);
            Ok(true)
        })
        .ok_or_else(no_lobby)?;
    if joined? {
        broadcast(&state, &lobby);
        social::announce(&state, auth.user_id).await;
    }
    Ok(Json(lobby))
}

/// Takes someone out of a lobby and tells everyone concerned. Used when they leave and when they go offline.
pub async fn leave(state: &AppState, lobby_id: i64, user_id: i64) -> bool {
    let Some(after) = state.multiplayer.leave_lobby(lobby_id, user_id) else { return false };
    state.hub.send(user_id, &Event::LobbyLeft(LobbyRef { lobby_id }));
    if let Some(lobby) = after {
        broadcast(state, &lobby);
        // A lobby that got room again shows up in its members' presence as joinable.
        for member in &lobby.members {
            social::announce(state, member.user_id).await;
        }
    }
    social::announce(state, user_id).await;
    true
}

pub async fn leave_lobby(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    if !leave(&state, id, auth.user_id).await {
        return Err(ApiError::not_found("You are not in that lobby."));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn patch_lobby(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(patch): ApiJson<LobbyPatch>) -> ApiResult<Json<Lobby>> {
    let (changed, lobby) = state
        .multiplayer
        .with_lobby(id, |lobby_state| {
            if lobby_state.lobby.owner_id != auth.user_id {
                return Err(ApiError::forbidden("Only the lobby's owner can change it."));
            }
            // Work on a copy, so a patch that is refused halfway leaves the lobby as it was.
            let mut next = lobby_state.lobby.clone();
            multiplayer::patch_lobby(&mut next, patch).map_err(ApiError::bad_request)?;
            lobby_state.lobby = next;
            Ok(())
        })
        .ok_or_else(no_lobby)?;
    changed?;
    broadcast(&state, &lobby);
    for member in &lobby.members {
        social::announce(&state, member.user_id).await;
    }
    Ok(Json(lobby))
}

/// A member sets pairs about themselves. Pairs that are not named keep their value.
pub async fn set_member_data(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    ApiJson(pairs): ApiJson<BTreeMap<String, String>>,
) -> ApiResult<Json<Lobby>> {
    let (found, lobby) = state
        .multiplayer
        .with_lobby(id, |lobby_state| match lobby_state.lobby.members.iter_mut().find(|member| member.user_id == auth.user_id) {
            Some(member) => {
                member.data.extend(multiplayer::clip_member_data(pairs));
                member.data.len() <= 100
            }
            None => false,
        })
        .ok_or_else(no_lobby)?;
    if !found {
        return Err(ApiError::forbidden("Join the lobby first. A member holds up to 100 pairs of data."));
    }
    broadcast(&state, &lobby);
    Ok(Json(lobby))
}

pub async fn lobby_chat(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(chat): ApiJson<LobbyChat>) -> ApiResult<StatusCode> {
    let lobby = state.multiplayer.lobby(id).ok_or_else(no_lobby)?;
    if !is_member(&lobby, auth.user_id) {
        return Err(ApiError::forbidden("Join the lobby first."));
    }
    if chat.body.is_empty() || chat.body.len() > 8192 {
        return Err(ApiError::bad_request("A lobby message holds 1 to 8192 bytes."));
    }
    let message = LobbyChatMessage { lobby_id: id, user_id: auth.user_id, body: chat.body, sent_at: now() };
    // Everyone gets it, the sender included: games expect their own messages echoed back.
    for member in &lobby.members {
        state.hub.send(member.user_id, &Event::LobbyChat(message.clone()));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- invites and join requests ----------

pub async fn invite(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<SendInvite>) -> ApiResult<StatusCode> {
    let title = game_title(&state, &auth, request.game_id).await?;
    person(&state, request.user_id).await?;
    if request.user_id == auth.user_id {
        return Err(ApiError::bad_request("You cannot invite yourself."));
    }
    if request.connect.len() > 1024 {
        return Err(ApiError::bad_request("The connect string holds up to 1024 bytes."));
    }
    if let Some(lobby_id) = request.lobby_id {
        // An invite from a member is what lets someone into a private lobby.
        let (member, _) = state
            .multiplayer
            .with_lobby(lobby_id, |lobby_state| {
                let member = is_member(&lobby_state.lobby, auth.user_id);
                if member {
                    lobby_state.invited.insert(request.user_id);
                }
                member
            })
            .ok_or_else(no_lobby)?;
        if !member {
            return Err(ApiError::forbidden("Only someone in the lobby can invite to it."));
        }
    }
    let invite = Invite { from_id: auth.user_id, from_username: auth.username.clone(), game_id: request.game_id, lobby_id: request.lobby_id, connect: request.connect };
    state.hub.send(request.user_id, &Event::Invite(invite));
    let mut data = vec![("game_id", request.game_id), ("user_id", auth.user_id)];
    if let Some(lobby_id) = request.lobby_id {
        data.push(("lobby_id", lobby_id));
    }
    social::notify(&state, To::User(request.user_id), NotificationKind::Invite, format!("{} invited you to {title}", auth.username), "", &data).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn join_request(State(state): State<AppState>, auth: Auth, Path(user_id): Path<i64>, ApiJson(request): ApiJson<JoinRequest>) -> ApiResult<StatusCode> {
    game_title(&state, &auth, request.game_id).await?;
    person(&state, user_id).await?;
    if !state.hub.is_online(user_id) {
        return Err(ApiError::conflict("That person is not online right now."));
    }
    state.hub.send(user_id, &Event::JoinRequested(JoinRequested { from_id: auth.user_id, from_username: auth.username.clone(), game_id: request.game_id }));
    Ok(StatusCode::NO_CONTENT)
}

// ---------- peer to peer ----------

/// The relay port to hand out: the one that is bound, if the owner has the relay switched on.
fn relay_port(state: &AppState) -> Option<u16> {
    state.multiplayer.relay_port().filter(|_| state.settings().relay_enabled)
}

fn no_session() -> ApiError {
    ApiError::not_found("That session does not exist any more. Open a new one.")
}

pub async fn open_p2p(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<OpenP2p>) -> ApiResult<Json<P2pSession>> {
    connected(&state, &auth)?;
    game_title(&state, &auth, request.game_id).await?;
    person(&state, request.peer_id).await?;
    if request.peer_id == auth.user_id {
        return Err(ApiError::bad_request("A session needs two different people."));
    }
    if !state.hub.is_online(request.peer_id) {
        return Err(ApiError::conflict("That person is not online right now."));
    }
    let port = relay_port(&state);
    let (session, new) = state.multiplayer.open_p2p(auth.user_id, request.peer_id, request.game_id, request.lobby_id, port);
    if new && let Some(theirs) = state.multiplayer.p2p(session.id, request.peer_id, port) {
        state.hub.send(request.peer_id, &Event::P2pOffer(theirs));
    }
    Ok(Json(session))
}

pub async fn p2p(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<P2pSession>> {
    state.multiplayer.p2p(id, auth.user_id, relay_port(&state)).map(Json).ok_or_else(no_session)
}

/// Passes a message to the other side untouched: the addresses to try, usually.
pub async fn signal(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(signal): ApiJson<P2pSignal>) -> ApiResult<StatusCode> {
    let session = state.multiplayer.p2p(id, auth.user_id, None).ok_or_else(no_session)?;
    if serde_json::to_string(&signal.data).map_or(true, |json| json.len() > 16 * 1024) {
        return Err(ApiError::bad_request("A signal holds up to 16 KB."));
    }
    state.hub.send(session.peer_id, &Event::P2pSignal(P2pSignalled { session_id: id, from_id: auth.user_id, data: signal.data }));
    Ok(StatusCode::NO_CONTENT)
}

pub async fn report_route(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>, ApiJson(report): ApiJson<ReportRoute>) -> ApiResult<StatusCode> {
    if !state.multiplayer.set_route(id, auth.user_id, report.route) {
        return Err(no_session());
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn close_p2p(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let peer = state.multiplayer.close_p2p(id, auth.user_id).ok_or_else(no_session)?;
    state.hub.send(peer, &Event::P2pClosed(P2pRef { session_id: id }));
    Ok(StatusCode::NO_CONTENT)
}

/// Tells both people of each closed session that it is gone.
pub fn announce_closed(state: &AppState, closed: Vec<(i64, [i64; 2])>) {
    for (session_id, people) in closed {
        for user_id in people {
            state.hub.send(user_id, &Event::P2pClosed(P2pRef { session_id }));
        }
    }
}

/// Someone's last connection closed and did not come back: they leave their lobbies and sessions.
pub async fn went_offline(state: &AppState, user_id: i64) {
    for lobby_id in state.multiplayer.lobbies_of(user_id) {
        leave(state, lobby_id, user_id).await;
    }
    announce_closed(state, state.multiplayer.close_p2p_of(user_id));
    state.multiplayer.cancel_tickets_of(user_id);
}

// ---------- a record of calls, for the admin ----------

/// What a request is called in the Steam API it stands in for. `None` for everything else.
fn steam_name(method: &axum::http::Method, path: &str) -> Option<(&'static str, &'static str)> {
    Some(match (method.as_str(), path) {
        ("POST", "/tickets") => ("ISteamUser", "GetAuthSessionTicket"),
        ("POST", "/tickets/verify") => ("ISteamUser", "BeginAuthSession"),
        ("DELETE", "/tickets/{ticket}") => ("ISteamUser", "CancelAuthTicket"),
        ("GET", "/games/by-steam-app/{app_id}") => ("ISteamApps", "BIsSubscribedApp"),
        ("PUT", "/presence") => ("ISteamFriends", "SetRichPresence"),
        ("POST", "/invites") => ("ISteamFriends", "InviteUserToGame"),
        ("POST", "/users/{id}/join-request") => ("ISteamFriends", "RequestJoin"),
        ("POST", "/lobbies") => ("ISteamMatchmaking", "CreateLobby"),
        ("POST", "/lobbies/search") => ("ISteamMatchmaking", "RequestLobbyList"),
        ("GET", "/lobbies/{id}") => ("ISteamMatchmaking", "RequestLobbyData"),
        ("PATCH", "/lobbies/{id}") => ("ISteamMatchmaking", "SetLobbyData"),
        ("POST", "/lobbies/{id}/join") => ("ISteamMatchmaking", "JoinLobby"),
        ("POST", "/lobbies/{id}/leave") => ("ISteamMatchmaking", "LeaveLobby"),
        ("PUT", "/lobbies/{id}/members/me") => ("ISteamMatchmaking", "SetLobbyMemberData"),
        ("POST", "/lobbies/{id}/chat") => ("ISteamMatchmaking", "SendLobbyChatMsg"),
        ("POST", "/p2p/sessions") => ("ISteamNetworkingSockets", "ConnectP2P"),
        ("POST", "/p2p/sessions/{id}/signal") => ("ISteamNetworkingSockets", "SendSignal"),
        ("DELETE", "/p2p/sessions/{id}") => ("ISteamNetworkingSockets", "CloseConnection"),
        ("PUT", "/games/{id}/achievements/{name}") => ("ISteamUserStats", "SetAchievement"),
        ("DELETE", "/games/{id}/achievements/{name}") => ("ISteamUserStats", "ClearAchievement"),
        ("PUT", "/games/{id}/stats") => ("ISteamUserStats", "StoreStats"),
        _ => return None,
    })
}

/// Notes each multiplayer request with its Steam name, status and duration.
pub async fn record_calls(State(state): State<AppState>, request: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let name = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .and_then(|path| steam_name(request.method(), path.as_str().strip_prefix(hearth_api::API_PREFIX).unwrap_or(path.as_str())));
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    if let Some((interface, call)) = name {
        state.multiplayer.record_call(interface, call, response.status().as_u16(), started.elapsed().as_millis() as u32, now());
    }
    response
}
