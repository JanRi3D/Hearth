//! Presence, chat and notifications, and the events that deliver them live.
//!
//! A client opens one WebSocket at `GET /api/v1/ws`, authenticated like any request. The
//! Hearth sends [`Event`]s as JSON text frames; the client may send [`ClientMessage`]s.
//! Everything an event announces can also be fetched with a plain request, so a client that
//! was offline catches up by asking.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    Id, Timestamp,
    multiplayer::{Invite, JoinRequested, Lobby, LobbyChatMessage, P2pSession, P2pSignalled},
    people::QueuedDownload,
    saves::World,
};

/// A client sends a `ping` (or a WebSocket ping frame) at least this often. The Hearth
/// closes a connection that stays silent for three times as long.
pub const PING_SECONDS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Offline,
    Online,
    Away,
    Playing,
}

/// What someone is doing right now. `GET /api/v1/presence` lists everyone's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub user_id: Id,
    pub status: Status,
    /// Set while `status` is `playing`, unless the person hides what they play.
    pub game_id: Option<Id>,
    /// A line the client or the game provides, for example "in Factory Friday".
    pub detail: String,
    /// When the current status began.
    pub since: Timestamp,
    /// Key and value pairs a game publishes about its state (Steam calls this rich presence).
    pub rich: BTreeMap<String, String>,
    /// A lobby friends can join through this person.
    pub lobby_id: Option<Id>,
}

/// `PUT /api/v1/presence`. Only the fields that are present change. Being online, offline or
/// in a game is worked out by the Hearth from the connection and the play session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetPresence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub away: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Replaces all rich presence pairs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rich: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: Id,
    pub sender_id: Id,
    pub recipient_id: Id,
    pub body: String,
    pub sent_at: Timestamp,
    pub read_at: Option<Timestamp>,
}

/// `POST /api/v1/chats/{user_id}/messages`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessage {
    pub body: String,
}

/// One conversation in `GET /api/v1/chats`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub peer_id: Id,
    pub last: ChatMessage,
    /// Messages from the peer the caller has not read.
    pub unread: u32,
}

/// `POST /api/v1/chats/{user_id}/read`, and the event both sides get: everything the peer
/// sent up to and including `up_to` has been read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRead {
    pub reader_id: Id,
    pub peer_id: Id,
    pub up_to: Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// Someone invited you into a lobby or game.
    Invite,
    /// A new version of a game is on the Hearth.
    Update,
    /// A game night was planned or is about to start.
    Event,
    /// An admin published a news post.
    News,
    /// Your account was signed in on a new device.
    SignIn,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: Id,
    pub kind: NotificationKind,
    pub title: String,
    pub body: String,
    /// What the notification is about, for the client to open: `game_id`, `version_id`,
    /// `lobby_id`, `event_id`, `news_id`, `user_id`, whichever apply.
    pub data: BTreeMap<String, Id>,
    pub created_at: Timestamp,
    pub read_at: Option<Timestamp>,
}

/// `POST /api/v1/notifications/read`. An empty list marks everything read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MarkRead {
    #[serde(default)]
    pub ids: Vec<Id>,
}

/// The first event on every connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub user_id: Id,
    pub time: Timestamp,
    pub ping_seconds: u64,
}

/// What the Hearth sends over the WebSocket: `{ "type": "chat_message", "data": { … } }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    Hello(Hello),
    /// Someone's presence changed.
    Presence(Presence),
    /// A message to you, or one you sent from another device.
    ChatMessage(ChatMessage),
    ChatRead(ChatRead),
    Notification(Notification),
    /// An admin queued a download for this device.
    DownloadQueued(QueuedDownload),
    /// A lobby you are in changed: members, owner, data or settings. Carries the whole lobby.
    LobbyUpdated(Lobby),
    /// You are no longer in this lobby.
    LobbyLeft(LobbyRef),
    LobbyChat(LobbyChatMessage),
    /// Someone invited you into a game or lobby.
    Invite(Invite),
    /// Someone asks to join your game.
    JoinRequested(JoinRequested),
    /// Someone opened a peer-to-peer session with you. Carries your side of it.
    P2pOffer(P2pSession),
    P2pSignal(P2pSignalled),
    /// The other side closed the session, or it timed out.
    P2pClosed(P2pRef),
    /// A shared world you are a member of changed: its host, its members or its stored state.
    WorldUpdated(World),
    /// Answer to a `ping`.
    Pong,
    /// An event this version of the crate does not know. Ignore it.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LobbyRef {
    pub lobby_id: Id,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct P2pRef {
    pub session_id: Id,
}

/// What a client may send over the WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ClientMessage {
    Ping,
    #[serde(other)]
    Unknown,
}
