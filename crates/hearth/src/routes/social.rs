//! Presence, chat and notifications, and the WebSocket that delivers them live.

use std::{collections::BTreeMap, time::Duration};

use axum::{
    Json,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::Response,
};
use hearth_api::social::{
    ChatMessage, ChatRead, ClientMessage, Conversation, Event, Hello, MarkRead, Notification, NotificationKind, PING_SECONDS, Presence, SendMessage,
    SetPresence, Status,
};
use serde::Deserialize;

use super::play;
use crate::{
    auth::{ApiJson, Auth},
    db::now,
    error::{ApiError, ApiResult},
    state::AppState,
};

// ---------- the WebSocket ----------

pub async fn ws(State(state): State<AppState>, auth: Auth, headers: HeaderMap, upgrade: WebSocketUpgrade) -> ApiResult<Response> {
    // A browser sends the session cookie with a WebSocket request from any site. The Origin
    // header says which site asked, and only the Hearth's own pages may.
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|value| value.to_str().ok()) {
        let host = headers.get(header::HOST).and_then(|value| value.to_str().ok()).unwrap_or_default();
        if origin.split_once("://").map(|(_, rest)| rest) != Some(host) {
            return Err(ApiError::forbidden("This connection did not come from the Hearth's own pages."));
        }
    }
    Ok(upgrade.max_message_size(64 * 1024).on_upgrade(move |socket| connection(state, auth, socket)))
}

async fn connection(state: AppState, auth: Auth, mut socket: WebSocket) {
    let (conn_id, mut events, came_online) = state.hub.connect(auth.user_id, auth.session_id, auth.device_id, now());
    let hello = Event::Hello(Hello { user_id: auth.user_id, time: now(), ping_seconds: PING_SECONDS });
    let _ = socket.send(Message::Text(serde_json::to_string(&hello).expect("events are plain data").into())).await;
    if came_online {
        announce(&state, auth.user_id).await;
    }

    let silence = Duration::from_secs(PING_SECONDS * 3);
    let mut recheck = tokio::time::interval(Duration::from_secs(30));
    recheck.tick().await;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(json) => {
                    if socket.send(Message::Text(json.as_ref().into())).await.is_err() {
                        break;
                    }
                }
                // The hub dropped this connection: signed out, or too far behind.
                None => break,
            },
            incoming = tokio::time::timeout(silence, socket.recv()) => match incoming {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(ClientMessage::Ping) = serde_json::from_str::<ClientMessage>(&text) {
                        let pong = serde_json::to_string(&Event::Pong).expect("events are plain data");
                        if socket.send(Message::Text(pong.into())).await.is_err() {
                            break;
                        }
                    }
                }
                // Ping frames are answered by the library. Anything else arriving counts as a sign of life.
                Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => break,
                Ok(Some(Ok(_))) => {}
            },
            // A session can end without a request telling us: it expired, or the password was reset on the host.
            _ = recheck.tick() => {
                let alive: Result<Option<i64>, _> = sqlx::query_scalar("SELECT id FROM sessions WHERE id = ? AND (expires_at IS NULL OR expires_at > ?)")
                    .bind(auth.session_id)
                    .bind(now())
                    .fetch_optional(&state.db)
                    .await;
                if matches!(alive, Ok(None)) {
                    break;
                }
            }
            _ = state.shutdown.cancelled() => break,
        }
    }
    let _ = socket.send(Message::Close(None)).await;
    let went_offline = state.hub.disconnect(auth.user_id, conn_id);
    // "Last seen" is the moment the connection ended, not the last request before it.
    let _ = sqlx::query("UPDATE users SET last_seen_at = ? WHERE id = ?").bind(now()).bind(auth.user_id).execute(&state.db).await;
    if let Some(device_id) = auth.device_id {
        let _ = sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?").bind(now()).bind(device_id).execute(&state.db).await;
    }
    if went_offline {
        announce(&state, auth.user_id).await;
        // A dropped connection usually comes right back. Only someone who stays away leaves
        // their lobbies and sessions.
        let user_id = auth.user_id;
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(state.opts.offline_grace) => {}
                _ = state.shutdown.cancelled() => return,
            }
            if !state.hub.is_online(user_id) {
                super::multiplayer::went_offline(&state, user_id).await;
            }
        });
    }
}

// ---------- presence ----------

/// A person's presence as they see it themselves, plus whether others may see the game.
async fn presence_of(state: &AppState, user_id: i64) -> ApiResult<(Presence, bool)> {
    let shares: Option<bool> = sqlx::query_scalar("SELECT share_now_playing FROM users WHERE id = ?").bind(user_id).fetch_optional(&state.db).await?;
    let Some(told) = state.hub.told(user_id) else {
        let offline = Presence { user_id, status: Status::Offline, game_id: None, detail: String::new(), since: now(), rich: BTreeMap::new(), lobby_id: None };
        return Ok((offline, true));
    };
    let playing = play::now_playing(state, user_id).await?;
    let presence = Presence {
        user_id,
        status: match (&playing, told.away) {
            (Some(_), _) => Status::Playing,
            (None, true) => Status::Away,
            (None, false) => Status::Online,
        },
        game_id: playing.as_ref().map(|playing| playing.game_id),
        since: playing.as_ref().map_or(told.since, |playing| playing.since),
        detail: told.detail,
        rich: told.rich,
        lobby_id: state.multiplayer.joinable_lobby_of(user_id),
    };
    Ok((presence, shares.unwrap_or(true)))
}

/// What everyone else sees of someone who keeps their game to themselves: just "online".
fn veiled(mut presence: Presence) -> Presence {
    if presence.status == Status::Playing {
        presence.status = Status::Online;
        presence.game_id = None;
        presence.detail.clear();
        presence.rich.clear();
        presence.lobby_id = None;
    }
    presence
}

/// Tells everyone connected that a person's presence changed.
pub async fn announce(state: &AppState, user_id: i64) {
    match presence_of(state, user_id).await {
        Ok((presence, shares)) => {
            state.hub.send(user_id, &Event::Presence(presence.clone()));
            let public = if shares { presence } else { veiled(presence) };
            state.hub.send_all(&Event::Presence(public), Some(user_id));
        }
        Err(err) => tracing::warn!(user_id, error = %err.message, "could not work out a presence"),
    }
}

/// Everyone's presence, people who are offline included.
pub async fn presence(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Presence>>> {
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM users WHERE removed_at IS NULL ORDER BY id").fetch_all(&state.db).await?;
    let mut all = Vec::with_capacity(ids.len());
    for id in ids {
        let (presence, shares) = presence_of(&state, id).await?;
        all.push(if shares || id == auth.user_id { presence } else { veiled(presence) });
    }
    Ok(Json(all))
}

pub async fn set_presence(State(state): State<AppState>, auth: Auth, ApiJson(change): ApiJson<SetPresence>) -> ApiResult<StatusCode> {
    if !state.hub.is_online(auth.user_id) {
        return Err(ApiError::conflict("Connect to the Hearth's WebSocket first. Presence belongs to a live connection."));
    }
    let clip = |text: String, max: usize| text.chars().filter(|c| !c.is_control()).take(max).collect::<String>();
    if change.rich.as_ref().is_some_and(|rich| rich.len() > 30) {
        return Err(ApiError::bad_request("Rich presence holds up to 30 pairs."));
    }
    state.hub.tell(auth.user_id, |told| {
        if let Some(away) = change.away {
            told.away = away;
        }
        if let Some(detail) = change.detail {
            told.detail = clip(detail, 120);
        }
        if let Some(rich) = change.rich {
            told.rich = rich.into_iter().map(|(key, value)| (clip(key, 64), clip(value, 256))).collect();
        }
    });
    announce(&state, auth.user_id).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- chat ----------

type MessageRow = (i64, i64, i64, String, i64, Option<i64>);

fn message((id, sender_id, recipient_id, body, sent_at, read_at): MessageRow) -> ChatMessage {
    ChatMessage { id, sender_id, recipient_id, body, sent_at, read_at }
}

async fn peer(state: &AppState, auth: &Auth, user_id: i64) -> ApiResult<()> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = ? AND removed_at IS NULL").bind(user_id).fetch_optional(&state.db).await?;
    match exists {
        Some(_) if user_id != auth.user_id => Ok(()),
        Some(_) => Err(ApiError::bad_request("You cannot chat with yourself.")),
        None => Err(ApiError::not_found("This Hearth has no such person.")),
    }
}

pub async fn send_message(State(state): State<AppState>, auth: Auth, Path(to): Path<i64>, ApiJson(request): ApiJson<SendMessage>) -> ApiResult<Json<ChatMessage>> {
    peer(&state, &auth, to).await?;
    let body = request.body.trim();
    if body.is_empty() || body.chars().count() > 4000 {
        return Err(ApiError::bad_request("A message has 1 to 4000 characters."));
    }
    let row: MessageRow = sqlx::query_as(
        "INSERT INTO messages (sender_id, recipient_id, body, sent_at) VALUES (?, ?, ?, ?) RETURNING id, sender_id, recipient_id, body, sent_at, read_at",
    )
    .bind(auth.user_id)
    .bind(to)
    .bind(body)
    .bind(now())
    .fetch_one(&state.db)
    .await?;
    let sent = message(row);
    // The recipient gets it live, and so do the sender's other devices.
    state.hub.send(to, &Event::ChatMessage(sent.clone()));
    state.hub.send(auth.user_id, &Event::ChatMessage(sent.clone()));
    Ok(Json(sent))
}

#[derive(Deserialize)]
pub struct MessagesQuery {
    /// Only messages older than this id, for paging back.
    before: Option<i64>,
    limit: Option<i64>,
}

/// One conversation, oldest first, ending at the newest message or just before `before`.
pub async fn messages(State(state): State<AppState>, auth: Auth, Path(with): Path<i64>, Query(query): Query<MessagesQuery>) -> ApiResult<Json<Vec<ChatMessage>>> {
    let mut rows: Vec<MessageRow> = sqlx::query_as(
        "SELECT id, sender_id, recipient_id, body, sent_at, read_at FROM messages
         WHERE ((sender_id = ?1 AND recipient_id = ?2) OR (sender_id = ?2 AND recipient_id = ?1)) AND (?3 IS NULL OR id < ?3)
         ORDER BY id DESC LIMIT ?4",
    )
    .bind(auth.user_id)
    .bind(with)
    .bind(query.before)
    .bind(query.limit.unwrap_or(50).clamp(1, 200))
    .fetch_all(&state.db)
    .await?;
    rows.reverse();
    Ok(Json(rows.into_iter().map(message).collect()))
}

pub async fn conversations(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Vec<Conversation>>> {
    let latest: Vec<MessageRow> = sqlx::query_as(
        "SELECT id, sender_id, recipient_id, body, sent_at, read_at FROM messages WHERE id IN (
            SELECT MAX(id) FROM messages WHERE sender_id = ?1 OR recipient_id = ?1
            GROUP BY CASE WHEN sender_id = ?1 THEN recipient_id ELSE sender_id END)
         ORDER BY id DESC",
    )
    .bind(auth.user_id)
    .fetch_all(&state.db)
    .await?;
    let unread: Vec<(i64, i64)> = sqlx::query_as("SELECT sender_id, COUNT(*) FROM messages WHERE recipient_id = ? AND read_at IS NULL GROUP BY sender_id")
        .bind(auth.user_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(
        latest
            .into_iter()
            .map(message)
            .map(|last| {
                let peer_id = if last.sender_id == auth.user_id { last.recipient_id } else { last.sender_id };
                Conversation { peer_id, unread: unread.iter().find(|(sender, _)| *sender == peer_id).map_or(0, |(_, count)| *count as u32), last }
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct ReadUpTo {
    up_to: i64,
}

pub async fn mark_chat_read(State(state): State<AppState>, auth: Auth, Path(with): Path<i64>, ApiJson(request): ApiJson<ReadUpTo>) -> ApiResult<StatusCode> {
    let changed = sqlx::query("UPDATE messages SET read_at = ? WHERE recipient_id = ? AND sender_id = ? AND id <= ? AND read_at IS NULL")
        .bind(now())
        .bind(auth.user_id)
        .bind(with)
        .bind(request.up_to)
        .execute(&state.db)
        .await?;
    if changed.rows_affected() > 0 {
        let read = Event::ChatRead(ChatRead { reader_id: auth.user_id, peer_id: with, up_to: request.up_to });
        state.hub.send(with, &read);
        state.hub.send(auth.user_id, &read);
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- notifications ----------

fn kind_to_db(kind: NotificationKind) -> &'static str {
    match kind {
        NotificationKind::Invite => "invite",
        NotificationKind::Update => "update",
        NotificationKind::Event => "event",
        NotificationKind::News => "news",
        NotificationKind::SignIn => "sign_in",
        NotificationKind::Other => "other",
    }
}

fn kind_from_db(kind: &str) -> NotificationKind {
    match kind {
        "invite" => NotificationKind::Invite,
        "update" => NotificationKind::Update,
        "event" => NotificationKind::Event,
        "news" => NotificationKind::News,
        "sign_in" => NotificationKind::SignIn,
        _ => NotificationKind::Other,
    }
}

/// Who a notification goes to.
pub enum To {
    User(i64),
    /// Everyone on the Hearth, except this person when one is named.
    Everyone(Option<i64>),
}

/// Stores a notification for each recipient and pushes it to those who are connected.
pub async fn notify(state: &AppState, to: To, kind: NotificationKind, title: impl Into<String>, body: impl Into<String>, data: &[(&str, i64)]) {
    let (title, body) = (title.into(), body.into());
    let data: BTreeMap<String, i64> = data.iter().map(|(key, id)| (key.to_string(), *id)).collect();
    let sent: Result<(), sqlx::Error> = async {
        let recipients: Vec<i64> = match to {
            To::User(id) => vec![id],
            To::Everyone(except) => sqlx::query_scalar("SELECT id FROM users WHERE removed_at IS NULL AND id IS NOT ?").bind(except).fetch_all(&state.db).await?,
        };
        let created_at = now();
        let json = serde_json::to_string(&data).unwrap_or_else(|_| "{}".into());
        for user_id in recipients {
            let id: i64 = sqlx::query_scalar("INSERT INTO notifications (user_id, kind, title, body, data, created_at) VALUES (?, ?, ?, ?, ?, ?) RETURNING id")
                .bind(user_id)
                .bind(kind_to_db(kind))
                .bind(&title)
                .bind(&body)
                .bind(&json)
                .bind(created_at)
                .fetch_one(&state.db)
                .await?;
            let notification = Notification { id, kind, title: title.clone(), body: body.clone(), data: data.clone(), created_at, read_at: None };
            state.hub.send(user_id, &Event::Notification(notification));
        }
        Ok(())
    }
    .await;
    if let Err(err) = sent {
        tracing::warn!(error = %err, %title, "could not store a notification");
    }
}

#[derive(Deserialize)]
pub struct NotificationsQuery {
    #[serde(default)]
    unread: bool,
}

/// The caller's notifications, newest first. At most the latest hundred.
pub async fn notifications(State(state): State<AppState>, auth: Auth, Query(query): Query<NotificationsQuery>) -> ApiResult<Json<Vec<Notification>>> {
    type Row = (i64, String, String, String, String, i64, Option<i64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, kind, title, body, data, created_at, read_at FROM notifications WHERE user_id = ? AND (read_at IS NULL OR NOT ?) ORDER BY id DESC LIMIT 100",
    )
    .bind(auth.user_id)
    .bind(query.unread)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, kind, title, body, data, created_at, read_at)| Notification {
                id,
                kind: kind_from_db(&kind),
                title,
                body,
                data: serde_json::from_str(&data).unwrap_or_default(),
                created_at,
                read_at,
            })
            .collect(),
    ))
}

pub async fn mark_notifications_read(State(state): State<AppState>, auth: Auth, ApiJson(request): ApiJson<MarkRead>) -> ApiResult<StatusCode> {
    let now = now();
    if request.ids.is_empty() {
        sqlx::query("UPDATE notifications SET read_at = ? WHERE user_id = ? AND read_at IS NULL").bind(now).bind(auth.user_id).execute(&state.db).await?;
    }
    for id in request.ids.iter().take(500) {
        sqlx::query("UPDATE notifications SET read_at = ? WHERE id = ? AND user_id = ? AND read_at IS NULL").bind(now).bind(id).bind(auth.user_id).execute(&state.db).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
