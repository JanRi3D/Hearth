//! What the Steam API replacement in Bonfire maps onto: session tickets, lobbies, invites
//! and peer-to-peer sessions with a relay to fall back on.
//!
//! Lobbies and peer-to-peer sessions live in the Hearth's memory. They need a live
//! connection (see [`crate::social`]) and are gone when the Hearth restarts, like a game
//! session itself.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Id, Timestamp};

// ---------- tickets ----------

/// `POST /api/v1/tickets`: proof of identity to hand to a game server or another player.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketRequest {
    pub game_id: Id,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    /// Opaque. Whoever receives it asks the Hearth what it stands for.
    pub ticket: String,
    pub expires_at: Timestamp,
}

/// `POST /api/v1/tickets/verify`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyTicket {
    pub ticket: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketInfo {
    pub user_id: Id,
    pub username: String,
    pub game_id: Id,
    /// Whether the game is in the library for this person: the Hearth's answer to "do they own it".
    pub owned: bool,
}

// ---------- lobbies ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LobbyKind {
    /// Joined by invite only. Never found by a search.
    Private,
    /// Found and joined through friends. On a Hearth everyone is a friend, so this behaves like `public`.
    Friends,
    Public,
    /// Not found by a search, but anyone who knows the id may join.
    Invisible,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lobby {
    pub id: Id,
    pub game_id: Id,
    pub owner_id: Id,
    pub kind: LobbyKind,
    pub max_members: u32,
    /// The owner can close a lobby to newcomers without changing its kind.
    pub joinable: bool,
    /// In the order they joined.
    pub members: Vec<LobbyMember>,
    /// Key and value pairs the owner sets. Searches filter on them.
    pub data: BTreeMap<String, String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LobbyMember {
    pub user_id: Id,
    pub username: String,
    /// Pairs each member sets about themselves, for example a chosen character.
    pub data: BTreeMap<String, String>,
}

/// `POST /api/v1/lobbies`. The creator becomes the owner and first member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLobby {
    pub game_id: Id,
    pub kind: LobbyKind,
    pub max_members: u32,
    #[serde(default)]
    pub data: BTreeMap<String, String>,
}

/// `PATCH /api/v1/lobbies/{id}`, by the owner. Only the fields that are present change.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LobbyPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<LobbyKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_members: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joinable: Option<bool>,
    /// Hands the lobby to another member.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<Id>,
    /// Pairs to set. Pairs that are not named keep their value.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    /// Keys to remove.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delete: Vec<String>,
}

/// `POST /api/v1/lobbies/search`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LobbySearch {
    pub game_id: Id,
    /// A lobby must pass every filter.
    #[serde(default)]
    pub filters: Vec<LobbyFilter>,
    /// Sort so that lobbies whose value for a key is closest to the wanted number come first.
    #[serde(default)]
    pub near: Vec<LobbyNear>,
    /// Only lobbies with at least this many free places.
    #[serde(default)]
    pub slots_available: u32,
    /// At most this many results. Defaults to 50.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyFilter {
    pub key: String,
    pub value: String,
    pub compare: Compare,
    /// Compare as numbers instead of text. A value that is not a number fails the filter.
    #[serde(default)]
    pub numeric: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyNear {
    pub key: String,
    pub value: f64,
}

/// `POST /api/v1/lobbies/{id}/chat`: a message to every member. `body` is opaque to the
/// Hearth, so a game can put text or encoded bytes in it. Up to 8 KB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyChat {
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LobbyChatMessage {
    pub lobby_id: Id,
    pub user_id: Id,
    pub body: String,
    pub sent_at: Timestamp,
}

/// `POST /api/v1/invites`: asks someone into a game, and into a lobby when one is named.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendInvite {
    pub user_id: Id,
    pub game_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lobby_id: Option<Id>,
    /// What the invited game needs to connect, for games that do not use lobbies.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub connect: String,
}

/// The event an invited person's clients receive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Invite {
    pub from_id: Id,
    pub from_username: String,
    pub game_id: Id,
    pub lobby_id: Option<Id>,
    pub connect: String,
}

/// `POST /api/v1/users/{id}/join-request`: "let me into your game". The other person's client
/// usually answers with an invite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub game_id: Id,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinRequested {
    pub from_id: Id,
    pub from_username: String,
    pub game_id: Id,
}

// ---------- peer to peer ----------

/// Each relay packet starts with these four bytes, then one byte for its type.
pub const RELAY_MAGIC: [u8; 4] = *b"BNF1";

/// The relay's packet types. All packets are UDP datagrams to the relay port.
///
/// | Type | Layout after the magic and type byte | Meaning |
/// |---|---|---|
/// | `PROBE` | token (16 bytes) | "Which address do you see me at?" Also tells the relay where this side of the session is. |
/// | `PROBE_REPLY` | family (4 or 6), address (4 or 16 bytes), port (2 bytes, big endian) | The answer. |
/// | `DATA` | token (16 bytes), payload | Forwarded to the other side, with the token replaced by the receiver's own. |
/// | `DISCOVER` | nothing | Sent as a broadcast on a local network to find a Hearth. |
/// | `DISCOVER_REPLY` | JSON, see [`Discovered`] | Only sent to private addresses. |
pub mod relay_packet {
    pub const PROBE: u8 = 1;
    pub const PROBE_REPLY: u8 = 2;
    pub const DATA: u8 = 3;
    pub const DISCOVER: u8 = 4;
    pub const DISCOVER_REPLY: u8 = 5;
}

/// The largest payload the relay forwards.
pub const RELAY_MAX_PAYLOAD: usize = 1400;

/// A Hearth's answer to a discovery broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Discovered {
    pub name: String,
    pub version: String,
    /// The HTTP port. The address is where the answer came from.
    pub port: u16,
    pub https: bool,
    pub cert_sha256: Option<String>,
}

/// `POST /api/v1/p2p/sessions`: open a session with another person for a game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenP2p {
    pub peer_id: Id,
    pub game_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lobby_id: Option<Id>,
}

/// One side's view of a session. The two sides try a direct path first, with addresses they
/// exchange through [`P2pSignal`]s, and send through the relay when none works.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct P2pSession {
    pub id: Id,
    pub peer_id: Id,
    pub game_id: Id,
    pub lobby_id: Option<Id>,
    /// This side's relay token, 32 hex characters. It names the session on the wire and is
    /// the only thing the relay accepts packets for.
    pub token: String,
    /// UDP port of the relay. `None` when the owner switched the relay off, which leaves direct paths only.
    pub relay_port: Option<u16>,
    pub route: Route,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// Not settled yet.
    Pending,
    Direct,
    Relay,
}

/// `POST /api/v1/p2p/sessions/{id}/signal`: anything one side wants the other to know while
/// they connect, typically the addresses to try. The Hearth passes it on untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct P2pSignal {
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct P2pSignalled {
    pub session_id: Id,
    pub from_id: Id,
    pub data: serde_json::Value,
}

/// `POST /api/v1/p2p/sessions/{id}/route`: a side reports which path it ended up using.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRoute {
    pub route: Route,
}
