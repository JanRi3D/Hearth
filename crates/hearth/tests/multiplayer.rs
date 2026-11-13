//! Milestone 5: tickets, lobbies, invites, and peer-to-peer sessions over a direct path and the relay.

mod common;

use std::{net::SocketAddr, time::Duration};

use common::{Api, Live, TestHearth};
use hearth_api::{
    HearthInfo,
    auth::Me,
    library::GameSummary,
    multiplayer::{
        Compare, CreateLobby, Discovered, JoinRequest, Lobby, LobbyChat, LobbyFilter, LobbyKind, LobbyPatch, LobbySearch, OpenP2p, P2pSession, P2pSignal,
        RELAY_MAGIC, RELAY_MAX_PAYLOAD, ReportRoute, Route, SendInvite, Ticket, TicketInfo, TicketRequest, VerifyTicket, relay_packet,
    },
    social::{Event, Notification, NotificationKind, Presence, Status},
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::net::UdpSocket;

struct World {
    hearth: TestHearth,
    admin: Api,
    game: i64,
    invite: String,
}

async fn world() -> World {
    let hearth = TestHearth::start().await;
    std::fs::create_dir_all(hearth.games.join("Factorio")).unwrap();
    std::fs::write(hearth.games.join("Factorio/factorio.exe"), b"game").unwrap();
    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    let game = hearth.settled(&admin, 0).await.games[0].id;
    World { hearth, admin, game, invite: done.invite_code }
}

async fn id_of(api: &Api) -> i64 {
    api.fetch::<Me>("/me").await.user.id
}

fn pairs(list: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    list.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()
}

async fn next_lobby(live: &mut Live, id: i64) -> Lobby {
    live.find(|event| match event {
        Event::LobbyUpdated(lobby) if lobby.id == id => Some(lobby),
        _ => None,
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_open_and_join_a_lobby() {
    let World { hearth, admin, game, invite } = world().await;
    let (robin, noor, lea) = (hearth.join(&invite, "robin").await, hearth.join(&invite, "noor").await, hearth.join(&invite, "lea").await);
    let (robin_id, noor_id, lea_id) = (id_of(&robin).await, id_of(&noor).await, id_of(&lea).await);

    // A lobby belongs to a live connection.
    let create = CreateLobby { game_id: game, kind: LobbyKind::Friends, max_members: 2, data: pairs(&[("name", "Factory Friday"), ("map", "Nauvis")]) };
    assert_eq!(robin.fails(Method::POST, "/lobbies", &create, StatusCode::CONFLICT).await, "conflict");
    let (mut robin_live, mut noor_live, mut lea_live) = (Live::connect(&robin).await, Live::connect(&noor).await, Live::connect(&lea).await);

    let lobby: Lobby = robin.ok(Method::POST, "/lobbies", &create).await;
    assert_eq!((lobby.owner_id, lobby.members.len(), lobby.data["name"].as_str()), (robin_id, 1, "Factory Friday"));

    // Friends see through Robin's presence that there is a lobby to join, and a search finds it.
    let presence = noor_live
        .find(|event| match event {
            Event::Presence(presence) if presence.user_id == robin_id && presence.lobby_id.is_some() => Some(presence),
            _ => None,
        })
        .await;
    assert_eq!(presence.lobby_id, Some(lobby.id));
    let filter = |value: &str| LobbyFilter { key: "map".into(), value: value.into(), compare: Compare::Equal, numeric: false };
    let found: Vec<Lobby> = noor.ok(Method::POST, "/lobbies/search", &LobbySearch { game_id: game, filters: vec![filter("nauvis")], ..Default::default() }).await;
    assert_eq!(found.iter().map(|lobby| lobby.id).collect::<Vec<_>>(), [lobby.id]);
    let found: Vec<Lobby> = noor.ok(Method::POST, "/lobbies/search", &LobbySearch { game_id: game, filters: vec![filter("vulcanus")], ..Default::default() }).await;
    assert!(found.is_empty());

    // Noor joins. Both learn the new member list. The lobby is full now.
    let joined: Lobby = noor.ok(Method::POST, &format!("/lobbies/{}/join", lobby.id), &Value::Null).await;
    assert_eq!(joined.members.iter().map(|member| member.username.as_str()).collect::<Vec<_>>(), ["robin", "noor"]);
    assert_eq!(next_lobby(&mut robin_live, lobby.id).await.members.len(), 2);
    assert_eq!(lea.fails(Method::POST, &format!("/lobbies/{}/join", lobby.id), &Value::Null, StatusCode::CONFLICT).await, "lobby_full");
    let everyone: Vec<Presence> = lea.fetch("/presence").await;
    assert_eq!(everyone.iter().find(|presence| presence.user_id == robin_id).unwrap().lobby_id, None, "a full lobby is not offered for joining");

    // The owner changes the lobby. Members set data about themselves. Everyone hears of both.
    let patch = LobbyPatch { max_members: Some(3), set: pairs(&[("map", "Vulcanus")]), delete: vec!["name".into()], ..Default::default() };
    assert_eq!(noor.fails(Method::PATCH, &format!("/lobbies/{}", lobby.id), &patch, StatusCode::FORBIDDEN).await, "forbidden");
    let changed: Lobby = robin.ok(Method::PATCH, &format!("/lobbies/{}", lobby.id), &patch).await;
    assert_eq!((changed.max_members, changed.data.clone()), (3, pairs(&[("map", "Vulcanus")])));
    // Noor's connection may still hold the update from her own joining. The one with the new data follows.
    let wanted = changed.data.clone();
    noor_live
        .find(|event| match event {
            Event::LobbyUpdated(updated) if updated.id == lobby.id && updated.data == wanted && updated.max_members == 3 => Some(()),
            _ => None,
        })
        .await;
    let _: Lobby = noor.ok(Method::PUT, &format!("/lobbies/{}/members/me", lobby.id), &pairs(&[("ready", "1")])).await;
    let seen = robin_live
        .find(|event| match event {
            Event::LobbyUpdated(lobby) if lobby.members.iter().any(|member| !member.data.is_empty()) => Some(lobby),
            _ => None,
        })
        .await;
    assert_eq!(seen.members[1].data, pairs(&[("ready", "1")]));

    // Lobby chat reaches every member, the sender included.
    let (status, _) = noor.post(&format!("/lobbies/{}/chat", lobby.id), &LobbyChat { body: "gl hf".into() }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for live in [&mut robin_live, &mut noor_live] {
        let message = live
            .find(|event| match event {
                Event::LobbyChat(message) => Some(message),
                _ => None,
            })
            .await;
        assert_eq!((message.user_id, message.body.as_str()), (noor_id, "gl hf"));
    }
    assert_eq!(lea.fails(Method::POST, &format!("/lobbies/{}/chat", lobby.id), &LobbyChat { body: "let me in".into() }, StatusCode::FORBIDDEN).await, "forbidden");

    // A private lobby opens only for the invited. The invite arrives live and as a notification.
    let private: Lobby = robin.ok(Method::POST, "/lobbies", &CreateLobby { game_id: game, kind: LobbyKind::Private, max_members: 4, data: Default::default() }).await;
    assert_eq!(lea.fails(Method::POST, &format!("/lobbies/{}/join", private.id), &Value::Null, StatusCode::FORBIDDEN).await, "lobby_locked");
    let invite = SendInvite { user_id: lea_id, game_id: game, lobby_id: Some(private.id), connect: String::new() };
    assert_eq!(noor.fails(Method::POST, "/invites", &invite, StatusCode::FORBIDDEN).await, "forbidden", "only a member invites into a lobby");
    let (status, _) = robin.post("/invites", &invite).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let received = lea_live
        .find(|event| match event {
            Event::Invite(invite) => Some(invite),
            _ => None,
        })
        .await;
    assert_eq!((received.from_username.as_str(), received.lobby_id), ("robin", Some(private.id)));
    let inbox: Vec<Notification> = lea.fetch("/notifications?unread=true").await;
    assert_eq!((inbox[0].kind, inbox[0].title.as_str(), inbox[0].data["lobby_id"]), (NotificationKind::Invite, "robin invited you to Factorio", private.id));
    let _: Lobby = lea.ok(Method::POST, &format!("/lobbies/{}/join", private.id), &Value::Null).await;

    // "Let me into your game."
    let (status, _) = lea.post(&format!("/users/{robin_id}/join-request"), &JoinRequest { game_id: game }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let asked = robin_live
        .find(|event| match event {
            Event::JoinRequested(request) => Some(request),
            _ => None,
        })
        .await;
    assert_eq!((asked.from_id, asked.game_id), (lea_id, game));

    // The owner leaves: the lobby passes on. The last one leaves: it is gone.
    let mine: Vec<Lobby> = robin.fetch("/lobbies").await;
    assert_eq!(mine.len(), 2);

    // The admin's networking page sees the lobbies, and what the games asked for in Steam's own words.
    let networking: hearth_api::admin::Networking = admin.fetch("/admin/networking").await;
    assert_eq!(networking.lobbies.iter().map(|lobby| (lobby.name.as_str(), lobby.members)).collect::<Vec<_>>(), [("robin's lobby", 2), ("robin's lobby", 2)]);
    assert!(networking.recent_calls.iter().any(|call| (call.interface.as_str(), call.call.as_str(), call.status) == ("ISteamMatchmaking", "JoinLobby", 409)));
    assert!(networking.recent_calls.iter().any(|call| (call.interface.as_str(), call.call.as_str()) == ("ISteamFriends", "InviteUserToGame")));
    assert!(networking.calls_today >= 10 && networking.failed_today >= 3 && networking.relay_enabled);
    let (status, _) = robin.post(&format!("/lobbies/{}/leave", lobby.id), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let after = noor_live
        .find(|event| match event {
            Event::LobbyUpdated(updated) if updated.id == lobby.id && updated.members.len() == 1 => Some(updated),
            _ => None,
        })
        .await;
    assert_eq!(after.owner_id, noor_id);
    robin_live
        .find(|event| match event {
            Event::LobbyLeft(left) if left.lobby_id == lobby.id => Some(()),
            _ => None,
        })
        .await;
    assert_eq!(robin.fails(Method::POST, &format!("/lobbies/{}/leave", lobby.id), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
    let (status, _) = noor.post(&format!("/lobbies/{}/leave", lobby.id), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(noor.fails(Method::GET, &format!("/lobbies/{}", lobby.id), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");

    // Someone whose connection drops and stays away leaves the lobby on their own.
    lea_live.0.close(None).await.unwrap();
    let after = robin_live
        .find(|event| match event {
            Event::LobbyUpdated(updated) if updated.id == private.id && updated.members.len() == 1 => Some(updated),
            _ => None,
        })
        .await;
    assert_eq!(after.members[0].user_id, robin_id);

    // Tickets: Robin proves to Noor who he is, and the library answers for ownership.
    let ticket: Ticket = robin.ok(Method::POST, "/tickets", &TicketRequest { game_id: game }).await;
    let info: TicketInfo = noor.ok(Method::POST, "/tickets/verify", &VerifyTicket { ticket: ticket.ticket.clone() }).await;
    assert_eq!(info, TicketInfo { user_id: robin_id, username: "robin".into(), game_id: game, owned: true });
    let (status, _) = noor.call(Method::DELETE, &format!("/tickets/{}", ticket.ticket), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let _: TicketInfo = noor.ok(Method::POST, "/tickets/verify", &VerifyTicket { ticket: ticket.ticket.clone() }).await;
    let (status, _) = robin.call(Method::DELETE, &format!("/tickets/{}", ticket.ticket), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(noor.fails(Method::POST, "/tickets/verify", &VerifyTicket { ticket: ticket.ticket }, StatusCode::NOT_FOUND).await, "not_found");

    // A game that expects Steam finds itself by its app id.
    let _: Value = admin.ok(Method::PATCH, &format!("/admin/games/{game}"), &json!({ "steam_app_id": 427520 })).await;
    let found: GameSummary = robin.fetch("/games/by-steam-app/427520").await;
    assert_eq!(found.id, game);
    assert_eq!(robin.fails(Method::GET, "/games/by-steam-app/1", &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
}

// ---------- the relay's side of things ----------

fn token_bytes(hex: &str) -> [u8; 16] {
    std::array::from_fn(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap())
}

fn packet(kind: u8, token: &[u8], payload: &[u8]) -> Vec<u8> {
    [&RELAY_MAGIC[..], &[kind], token, payload].concat()
}

/// The next datagram, or `None` when nothing arrives, which is how the relay answers strangers.
async fn receive(socket: &UdpSocket) -> Option<(Vec<u8>, SocketAddr)> {
    let mut buffer = [0u8; 2048];
    let received = tokio::time::timeout(Duration::from_millis(400), socket.recv_from(&mut buffer)).await.ok()?;
    let (length, from) = received.unwrap();
    Some((buffer[..length].to_vec(), from))
}

/// Asks the relay which address it sees this socket at.
async fn probe(socket: &UdpSocket, relay: SocketAddr, token: &[u8; 16]) -> Option<SocketAddr> {
    socket.send_to(&packet(relay_packet::PROBE, token, &[]), relay).await.unwrap();
    let (reply, _) = receive(socket).await?;
    assert_eq!((&reply[..4], reply[4], reply[5]), (&RELAY_MAGIC[..], relay_packet::PROBE_REPLY, 4));
    Some(SocketAddr::from(([reply[6], reply[7], reply[8], reply[9]], u16::from_be_bytes([reply[10], reply[11]]))))
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_exchange_data_directly_and_through_the_relay() {
    let World { hearth, admin, game, invite } = world().await;
    let (robin, noor) = (hearth.join(&invite, "robin").await, hearth.join(&invite, "noor").await);
    let (robin_id, noor_id) = (id_of(&robin).await, id_of(&noor).await);
    let (mut robin_live, mut noor_live) = (Live::connect(&robin).await, Live::connect(&noor).await);

    // Robin opens a session. Noor is told, and gets her own token for it.
    let open = OpenP2p { peer_id: noor_id, game_id: game, lobby_id: None };
    let robins: P2pSession = robin.ok(Method::POST, "/p2p/sessions", &open).await;
    let noors = noor_live
        .find(|event| match event {
            Event::P2pOffer(session) => Some(session),
            _ => None,
        })
        .await;
    assert_eq!((noors.id, noors.peer_id, noors.route), (robins.id, robin_id, Route::Pending));
    assert_ne!(noors.token, robins.token);
    let again: P2pSession = noor.ok(Method::POST, "/p2p/sessions", &OpenP2p { peer_id: robin_id, game_id: game, lobby_id: None }).await;
    assert_eq!(again, noors, "opening from the other side finds the same session");
    let info: HearthInfo = robin.fetch("/hearth").await;
    assert_eq!(info.relay_port, robins.relay_port);
    let relay = SocketAddr::from(([127, 0, 0, 1], robins.relay_port.expect("the relay is on by default")));
    let (robin_token, noor_token) = (token_bytes(&robins.token), token_bytes(&noors.token));

    // Each side learns its public address from the relay, and tells the other through the Hearth.
    let (robin_socket, noor_socket) = (UdpSocket::bind("127.0.0.1:0").await.unwrap(), UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let robin_addr = probe(&robin_socket, relay, &robin_token).await.expect("a probe with a valid token is answered");
    let noor_addr = probe(&noor_socket, relay, &noor_token).await.unwrap();
    assert_eq!((robin_addr, noor_addr), (robin_socket.local_addr().unwrap(), noor_socket.local_addr().unwrap()));
    let (status, _) = robin.post(&format!("/p2p/sessions/{}/signal", robins.id), &P2pSignal { data: json!({ "candidates": [robin_addr.to_string()] }) }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let signal = noor_live
        .find(|event| match event {
            Event::P2pSignal(signal) => Some(signal),
            _ => None,
        })
        .await;
    assert_eq!((signal.session_id, signal.from_id), (robins.id, robin_id));
    let robin_candidate: SocketAddr = signal.data["candidates"][0].as_str().unwrap().parse().unwrap();

    // The direct path: Noor sends straight to the address Robin signalled.
    noor_socket.send_to(b"hello robin, directly", robin_candidate).await.unwrap();
    let (data, from) = receive(&robin_socket).await.expect("the direct packet arrives");
    assert_eq!((data.as_slice(), from), (&b"hello robin, directly"[..], noor_addr));
    let (status, _) = robin.post(&format!("/p2p/sessions/{}/route", robins.id), &ReportRoute { route: Route::Direct }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let session: P2pSession = noor.fetch(&format!("/p2p/sessions/{}", robins.id)).await;
    assert_eq!(session.route, Route::Direct);

    // The relay path, for when no direct path works: each side sends with its own token and
    // receives with its own token, so neither learns the other's.
    robin_socket.send_to(&packet(relay_packet::DATA, &robin_token, b"hello noor, via the relay"), relay).await.unwrap();
    let (data, from) = receive(&noor_socket).await.expect("the relayed packet arrives");
    assert_eq!(from, relay);
    assert_eq!(data, packet(relay_packet::DATA, &noor_token, b"hello noor, via the relay"));
    noor_socket.send_to(&packet(relay_packet::DATA, &noor_token, b"and back"), relay).await.unwrap();
    let (data, _) = receive(&robin_socket).await.unwrap();
    assert_eq!(data, packet(relay_packet::DATA, &robin_token, b"and back"));
    let session: P2pSession = robin.fetch(&format!("/p2p/sessions/{}", robins.id)).await;
    assert_eq!(session.route, Route::Relay, "traffic through the relay marks the session as relayed");

    // The relay is not an open one: strangers, wrong tokens and oversized packets get nothing.
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    assert_eq!(probe(&stranger, relay, &[7; 16]).await, None);
    stranger.send_to(&packet(relay_packet::DATA, &[7; 16], b"spam"), relay).await.unwrap();
    stranger.send_to(b"not even the right magic", relay).await.unwrap();
    robin_socket.send_to(&packet(relay_packet::DATA, &robin_token, &vec![0; RELAY_MAX_PAYLOAD + 1]), relay).await.unwrap();
    assert!(receive(&noor_socket).await.is_none() && receive(&stranger).await.is_none());
    let lea = hearth.join(&invite, "lea").await;
    assert_eq!(lea.fails(Method::GET, &format!("/p2p/sessions/{}", robins.id), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
    assert_eq!(lea.fails(Method::POST, &format!("/p2p/sessions/{}/signal", robins.id), &P2pSignal { data: json!({}) }, StatusCode::NOT_FOUND).await, "not_found");

    // Finding the Hearth on the local network: a broadcast packet, answered with name and port.
    stranger.send_to(&packet(relay_packet::DISCOVER, &[], &[]), relay).await.unwrap();
    let (reply, _) = receive(&stranger).await.expect("a neighbour's discovery is answered");
    assert_eq!((&reply[..4], reply[4]), (&RELAY_MAGIC[..], relay_packet::DISCOVER_REPLY));
    let found: Discovered = serde_json::from_slice(&reply[5..]).unwrap();
    assert_eq!((found.name.as_str(), found.port, found.https), ("home", hearth.running().addr.port(), false));

    // Closing ends it for both, and the tokens stop working.
    let (status, _) = noor.call(Method::DELETE, &format!("/p2p/sessions/{}", robins.id), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    robin_live
        .find(|event| match event {
            Event::P2pClosed(closed) if closed.session_id == robins.id => Some(()),
            _ => None,
        })
        .await;
    assert_eq!(probe(&robin_socket, relay, &robin_token).await, None);

    // With the relay switched off in Settings, sessions offer direct paths only. Discovery keeps working.
    let _: Value = admin.ok(Method::PATCH, "/admin/settings", &json!({ "relay_enabled": false })).await;
    let direct_only: P2pSession = robin.ok(Method::POST, "/p2p/sessions", &open).await;
    assert_eq!(direct_only.relay_port, None);
    assert_eq!(probe(&robin_socket, relay, &token_bytes(&direct_only.token)).await, None);
    stranger.send_to(&packet(relay_packet::DISCOVER, &[], &[]), relay).await.unwrap();
    assert!(receive(&stranger).await.is_some());

    // Someone who goes offline takes their sessions with them.
    robin_live.0.close(None).await.unwrap();
    noor_live
        .find(|event| match event {
            Event::P2pClosed(closed) if closed.session_id == direct_only.id => Some(()),
            _ => None,
        })
        .await;
    let everyone: Vec<Presence> = noor.fetch("/presence").await;
    assert_eq!(everyone.iter().find(|presence| presence.user_id == robin_id).unwrap().status, Status::Offline);
}
