//! Firelink: the registry of dedicated game servers. An admin adds servers, which the
//! Hearth checks every 30 seconds, and servers can list themselves with a heartbeat.

use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use axum::{
    Json,
    extract::{FromRequestParts, Path, Query, State},
    http::{StatusCode, request::Parts},
};
use hearth_api::community::{AddGameServer, GameServer, HEARTBEAT_SECONDS, Heartbeat, Registry};
use serde::Deserialize;
use tokio::net::{TcpStream, UdpSocket};

use crate::{
    auth::{self, Admin, ApiJson, Auth, ClientIp},
    db::{self, now},
    error::{ApiError, ApiResult},
    state::AppState,
};

// ---------- the registry token ----------

/// The one token servers use to list themselves. Made on first use, kept until an admin rotates it.
pub async fn heartbeat_token(state: &AppState) -> ApiResult<String> {
    if let Some(token) = db::meta_get(&state.db, "heartbeat_token").await? {
        return Ok(token);
    }
    new_heartbeat_token(state).await
}

async fn new_heartbeat_token(state: &AppState) -> ApiResult<String> {
    let token = format!("hb_{}", &auth::new_token()[..24]);
    db::meta_set(&state.db, "heartbeat_token", &token).await?;
    Ok(token)
}

async fn is_registry_token(state: &AppState, token: &str) -> ApiResult<bool> {
    Ok(token.starts_with("hb_") && token == heartbeat_token(state).await?)
}

/// A game server holding the registry token, or anyone signed in. Used where a dedicated
/// server has to ask the Hearth something, like who a ticket stands for.
pub struct ServerOrUser;

impl FromRequestParts<AppState> for ServerOrUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        if let Some(token) = auth::bearer(&parts.headers).filter(|token| token.starts_with("hb_")) {
            return if is_registry_token(state, &token).await? { Ok(Self) } else { Err(ApiError::unauthorized()) };
        }
        Auth::from_request_parts(parts, state).await.map(|_| Self)
    }
}

// ---------- reading ----------

async fn load(state: &AppState, game_id: Option<i64>) -> ApiResult<Vec<GameServer>> {
    type Row = (i64, Option<i64>, String, String, i64, i64, String, String, bool, Option<i64>, Option<i64>, bool, Option<String>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT s.id, s.game_id, s.name, s.address, s.players, s.max_players, s.map, s.version, s.online, s.ping_ms, s.last_seen_at, s.self_listed, u.username
         FROM game_servers s LEFT JOIN users u ON u.id = s.added_by WHERE ?1 IS NULL OR s.game_id = ?1 ORDER BY s.name COLLATE NOCASE",
    )
    .bind(game_id)
    .fetch_all(&state.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, game_id, name, address, players, max_players, map, version, online, ping_ms, last_seen_at, self_listed, added_by)| GameServer {
            id,
            game_id,
            name,
            address,
            players: players as u32,
            max_players: max_players as u32,
            map,
            version,
            online,
            ping_ms: ping_ms.map(|ms| ms as u32),
            last_seen_at,
            self_listed,
            added_by: added_by.unwrap_or_default(),
        })
        .collect())
}

#[derive(Deserialize)]
pub struct ListQuery {
    game_id: Option<i64>,
}

pub async fn list(State(state): State<AppState>, _auth: Auth, Query(query): Query<ListQuery>) -> ApiResult<Json<Vec<GameServer>>> {
    Ok(Json(load(&state, query.game_id).await?))
}

pub async fn registry(State(state): State<AppState>, _admin: Admin) -> ApiResult<Json<Registry>> {
    Ok(Json(Registry { servers: load(&state, None).await?, heartbeat_token: heartbeat_token(&state).await? }))
}

/// Makes a new token. Servers that list themselves need it before their next heartbeat.
pub async fn rotate_token(State(state): State<AppState>, Admin(admin): Admin) -> ApiResult<Json<Registry>> {
    let heartbeat_token = new_heartbeat_token(&state).await?;
    db::log_activity(&state.db, format!("{} made a new Firelink token", admin.username)).await;
    Ok(Json(Registry { servers: load(&state, None).await?, heartbeat_token }))
}

// ---------- adding and removing ----------

/// `host:port`, with a host that is a name or an address and nothing else.
fn check_address(address: &str) -> ApiResult<String> {
    let address = address.trim();
    let ok = address.parse::<SocketAddr>().is_ok()
        || address.rsplit_once(':').is_some_and(|(host, port)| {
            !host.is_empty() && host.len() <= 253 && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') && port.parse::<u16>().is_ok_and(|port| port > 0)
        });
    if !ok {
        return Err(ApiError::bad_request("The address is host:port, for example home.lan:34197."));
    }
    Ok(address.to_string())
}

fn check_name(name: &str) -> ApiResult<String> {
    let name: String = name.trim().chars().filter(|c| !c.is_control()).collect();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(ApiError::bad_request("A server needs a name of up to 80 characters."));
    }
    Ok(name)
}

pub async fn add(State(state): State<AppState>, Admin(admin): Admin, ApiJson(request): ApiJson<AddGameServer>) -> ApiResult<Json<GameServer>> {
    let (name, address) = (check_name(&request.name)?, check_address(&request.address)?);
    let game: Option<i64> = sqlx::query_scalar("SELECT id FROM games WHERE id = ?").bind(request.game_id).fetch_optional(&state.db).await?;
    game.ok_or_else(|| ApiError::not_found("This Hearth has no such game."))?;
    let inserted = sqlx::query_scalar::<_, i64>("INSERT INTO game_servers (game_id, name, address, added_by, created_at) VALUES (?, ?, ?, ?, ?) RETURNING id")
        .bind(request.game_id)
        .bind(&name)
        .bind(&address)
        .bind(admin.user_id)
        .bind(now())
        .fetch_one(&state.db)
        .await;
    let id = match inserted {
        Ok(id) => id,
        Err(sqlx::Error::Database(err)) if err.is_unique_violation() => return Err(ApiError::conflict("A server with that name and address is already in the registry.")),
        Err(err) => return Err(err.into()),
    };
    db::log_activity(&state.db, format!("{} added the server {name} to Firelink", admin.username)).await;
    // Check it right away, so the admin sees at once whether the address answers.
    check_one(&state, id, &address).await;
    load(&state, None).await?.into_iter().find(|server| server.id == id).map(Json).ok_or_else(|| ApiError::not_found("The server was added but could not be read back."))
}

pub async fn remove(State(state): State<AppState>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM game_servers WHERE id = ?").bind(id).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- heartbeats ----------

/// A server lists itself, or says it is still there. It needs the registry token.
pub async fn heartbeat(State(state): State<AppState>, ClientIp(ip): ClientIp, headers: axum::http::HeaderMap, ApiJson(beat): ApiJson<Heartbeat>) -> ApiResult<StatusCode> {
    let window = Duration::from_secs(600);
    state.limiter.check("heartbeat", ip, 10, window)?;
    let token = auth::bearer(&headers).unwrap_or_default();
    if !is_registry_token(&state, &token).await? {
        state.limiter.fail("heartbeat", ip, window);
        return Err(ApiError::unauthorized());
    }
    let name = check_name(&beat.name)?;
    let host = match beat.host.as_deref().map(str::trim).filter(|host| !host.is_empty()) {
        Some(host) => host.to_string(),
        None => match ip {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        },
    };
    let address = check_address(&format!("{host}:{}", beat.port))?;
    let game_id: Option<i64> = match (beat.game_id, beat.steam_app_id) {
        (Some(id), _) => sqlx::query_scalar("SELECT id FROM games WHERE id = ?").bind(id).fetch_optional(&state.db).await?,
        (None, Some(app_id)) => sqlx::query_scalar("SELECT id FROM games WHERE steam_app_id = ? ORDER BY id LIMIT 1").bind(app_id).fetch_optional(&state.db).await?,
        (None, None) => return Err(ApiError::bad_request("Say which game the server runs, with game_id or steam_app_id.")),
    };
    let game_id = game_id.ok_or_else(|| ApiError::not_found("This Hearth has no such game."))?;
    let clip = |text: &str| text.chars().filter(|c| !c.is_control()).take(80).collect::<String>();
    sqlx::query(
        "INSERT INTO game_servers (game_id, name, address, self_listed, players, max_players, map, version, online, last_seen_at, created_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, 1, ?8, ?8)
         ON CONFLICT (self_listed, name, address) DO UPDATE SET game_id = ?1, players = ?4, max_players = ?5, map = ?6, version = ?7, online = 1, last_seen_at = ?8",
    )
    .bind(game_id)
    .bind(name)
    .bind(address)
    .bind(beat.players.min(100_000))
    .bind(beat.max_players.min(100_000))
    .bind(clip(&beat.map))
    .bind(clip(&beat.version))
    .bind(now())
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- checking ----------

/// What a check found out about a server.
#[derive(Debug, Default, PartialEq)]
struct Found {
    ping_ms: u32,
    /// Players, places and map, when the server speaks a query protocol the Hearth knows.
    info: Option<(u32, u32, String)>,
}

/// Runs until shutdown: every 30 seconds, checks the servers an admin added and drops
/// self-listed ones whose heartbeat stopped.
pub async fn checker(state: AppState) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(HEARTBEAT_SECONDS as u64)) => {}
            _ = state.shutdown.cancelled() => return,
        }
        if let Err(err) = check_all(&state).await {
            tracing::warn!(error = %err, "could not check the Firelink servers");
        }
    }
}

pub async fn check_all(state: &AppState) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM game_servers WHERE self_listed = 1 AND last_seen_at < ?").bind(now() - 3 * HEARTBEAT_SECONDS).execute(&state.db).await?;
    let servers: Vec<(i64, String)> = sqlx::query_as("SELECT id, address FROM game_servers WHERE self_listed = 0").fetch_all(&state.db).await?;
    // All at once: a server that does not answer must not hold up the ones that do.
    futures_util::future::join_all(servers.iter().map(|(id, address)| check_one(state, *id, address))).await;
    Ok(())
}

async fn check_one(state: &AppState, id: i64, address: &str) {
    let found = probe(address).await;
    let saved = match &found {
        Some(Found { ping_ms, info: Some((players, max_players, map)) }) => {
            sqlx::query("UPDATE game_servers SET online = 1, ping_ms = ?, players = ?, max_players = ?, map = ?, last_seen_at = ? WHERE id = ?")
                .bind(ping_ms)
                .bind(players)
                .bind(max_players)
                .bind(map)
                .bind(now())
                .bind(id)
                .execute(&state.db)
                .await
        }
        Some(Found { ping_ms, info: None }) => sqlx::query("UPDATE game_servers SET online = 1, ping_ms = ?, last_seen_at = ? WHERE id = ?").bind(ping_ms).bind(now()).bind(id).execute(&state.db).await,
        None => sqlx::query("UPDATE game_servers SET online = 0, ping_ms = NULL WHERE id = ?").bind(id).execute(&state.db).await,
    };
    if let Err(err) = saved {
        tracing::warn!(server = id, error = %err, "could not store the result of a server check");
    }
}

/// Asks a server whether it is there: first the query many game servers answer over UDP,
/// which also tells players and map, then a plain TCP connection.
///
/// ponytail: a server that only speaks its own UDP protocol answers neither and shows as
/// not answering. Such a server can list itself with the heartbeat endpoint instead.
async fn probe(address: &str) -> Option<Found> {
    let mut targets: Vec<SocketAddr> = tokio::time::timeout(Duration::from_secs(3), tokio::net::lookup_host(address)).await.ok()?.ok()?.collect();
    // A name can stand for several addresses while the server listens on one of them only.
    // `localhost` is such a name. Game servers are mostly IPv4, so that is tried first.
    targets.sort_by_key(SocketAddr::is_ipv6);
    for target in targets.into_iter().take(2) {
        if let Some(found) = probe_one(target).await {
            return Some(found);
        }
    }
    None
}

async fn probe_one(target: SocketAddr) -> Option<Found> {
    if let Some(found) = query_a2s(target).await {
        return Some(found);
    }
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(target)).await.ok()?.ok()?;
    Some(Found { ping_ms: started.elapsed().as_millis() as u32, info: None })
}

const A2S_INFO: &[u8] = b"\xff\xff\xff\xffTSource Engine Query\0";

/// The server query protocol of Source engine games, which many other dedicated servers speak too.
async fn query_a2s(target: SocketAddr) -> Option<Found> {
    let socket = UdpSocket::bind(if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).await.ok()?;
    socket.connect(target).await.ok()?;
    let started = Instant::now();
    let mut request = A2S_INFO.to_vec();
    let mut buffer = [0u8; 1400];
    // A server may first answer with a challenge number that has to be sent back.
    for _ in 0..2 {
        socket.send(&request).await.ok()?;
        let length = tokio::time::timeout(Duration::from_millis(1500), socket.recv(&mut buffer)).await.ok()?.ok()?;
        let reply = &buffer[..length];
        match reply.get(4) {
            Some(b'A') if reply.len() >= 9 => {
                request = A2S_INFO.to_vec();
                request.extend_from_slice(&reply[5..9]);
            }
            Some(b'I') => return Some(Found { ping_ms: started.elapsed().as_millis() as u32, info: parse_a2s_info(reply) }),
            _ => return None,
        }
    }
    None
}

/// Reads players, places and map out of an A2S_INFO answer.
fn parse_a2s_info(reply: &[u8]) -> Option<(u32, u32, String)> {
    // Header (4), type (1), protocol (1), then four zero-terminated strings: name, map, folder, game.
    let mut rest = reply.get(6..)?;
    let mut strings = Vec::new();
    for _ in 0..4 {
        let end = rest.iter().position(|byte| *byte == 0)?;
        strings.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = &rest[end + 1..];
    }
    // App id (2), then players and places, one byte each.
    let (players, max_players) = (*rest.get(2)?, *rest.get(3)?);
    Some((players as u32, max_players as u32, strings.swap_remove(1).chars().filter(|c| !c.is_control()).take(80).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_server_query_answer_and_checks_addresses() {
        let mut reply = b"\xff\xff\xff\xffI\x11Sunday factory\0nauvis\0factorio\0Factorio\0".to_vec();
        reply.extend_from_slice(&[0x34, 0x12, 3, 16, 0, b'd', b'l', 0, 1]);
        assert_eq!(parse_a2s_info(&reply), Some((3, 16, "nauvis".to_string())));
        assert_eq!(parse_a2s_info(b"\xff\xff\xff\xffI\x11cut off"), None);

        for good in ["home.lan:34197", "192.168.1.20:27015", "[::1]:27015"] {
            assert!(check_address(good).is_ok(), "{good}");
        }
        for bad in ["home.lan", "home.lan:0", "http://home.lan:80", "home lan:80", ":80", "home.lan:port"] {
            assert!(check_address(bad).is_err(), "{bad}");
        }
    }
}
