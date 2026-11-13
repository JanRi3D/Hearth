//! Milestone 4: presence, chat and notifications, live over the WebSocket.

mod common;

use common::{Api, Live, TestHearth, sign_in};
use futures_util::SinkExt;
use hearth_api::{
    auth::{Me, Session},
    people::{Device, QueueDownload, QueuedDownload},
    play::{PlaySession, StartSession},
    social::{ChatMessage, Conversation, Event, MarkRead, Notification, NotificationKind, Presence, SendMessage, SetPresence, Status},
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

async fn id_of(api: &Api) -> i64 {
    api.fetch::<Me>("/me").await.user.id
}

#[tokio::test(flavor = "multi_thread")]
async fn two_clients_see_each_other_online_and_chat() {
    let hearth = TestHearth::start().await;
    std::fs::create_dir_all(hearth.games.join("Factorio")).unwrap();
    std::fs::write(hearth.games.join("Factorio/factorio.exe"), b"game").unwrap();
    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    let game = hearth.settled(&admin, 0).await.games[0].id;
    let robin = hearth.join(&done.invite_code, "robin").await;
    let noor = hearth.join(&done.invite_code, "noor").await;
    let (robin_id, noor_id) = (id_of(&robin).await, id_of(&noor).await);

    // Robin connects first, then sees Noor come online.
    let mut robin_live = Live::connect(&robin).await;
    let everyone: Vec<Presence> = robin.fetch("/presence").await;
    let statuses: Vec<_> = everyone.iter().map(|presence| (presence.user_id, presence.status)).collect();
    assert_eq!(statuses.iter().filter(|(_, status)| *status == Status::Online).map(|(id, _)| *id).collect::<Vec<_>>(), [robin_id]);
    let mut noor_live = Live::connect(&noor).await;
    robin_live.presence_of(noor_id, Status::Online).await;
    let everyone: Vec<Presence> = noor.fetch("/presence").await;
    assert_eq!(everyone.iter().filter(|presence| presence.status == Status::Online).count(), 2);

    // Chat arrives live, on every device of both sides.
    let mut robin_laptop_live = Live::connect(&sign_in(&hearth, "robin", "Laptop").await).await;
    let sent: ChatMessage = robin.ok(Method::POST, &format!("/chats/{noor_id}/messages"), &SendMessage { body: " Server is up. Bring iron. ".into() }).await;
    assert_eq!((sent.body.as_str(), sent.read_at), ("Server is up. Bring iron.", None));
    let pick_chat = |event| match event {
        Event::ChatMessage(message) => Some(message),
        _ => None,
    };
    assert_eq!(noor_live.find(pick_chat).await, sent);
    assert_eq!(robin_laptop_live.find(pick_chat).await, sent, "the sender's other device shows the message too");
    let reply: ChatMessage = noor.ok(Method::POST, &format!("/chats/{robin_id}/messages"), &SendMessage { body: "Give me ten minutes.".into() }).await;
    assert_eq!(robin_live.find(pick_chat).await.id, sent.id);
    assert_eq!(robin_live.find(pick_chat).await, reply);

    // Someone who is offline finds the message when they come back.
    let lea = hearth.join(&done.invite_code, "lea").await;
    let lea_id = id_of(&lea).await;
    let _: ChatMessage = robin.ok(Method::POST, &format!("/chats/{lea_id}/messages"), &SendMessage { body: "Rail Empire restarts tonight.".into() }).await;
    let inbox: Vec<Conversation> = lea.fetch("/chats").await;
    assert_eq!((inbox.len(), inbox[0].peer_id, inbox[0].unread), (1, robin_id, 1));

    // Reading is shared too, and history comes oldest first.
    let (status, _) = noor.post(&format!("/chats/{robin_id}/read"), &json!({ "up_to": sent.id })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let read = robin_live
        .find(|event| match event {
            Event::ChatRead(read) => Some(read),
            _ => None,
        })
        .await;
    assert_eq!((read.reader_id, read.up_to), (noor_id, sent.id));
    let history: Vec<ChatMessage> = robin.fetch(&format!("/chats/{noor_id}/messages")).await;
    assert_eq!(history.iter().map(|message| message.id).collect::<Vec<_>>(), [sent.id, reply.id]);
    assert!(history[0].read_at.is_some() && history[1].read_at.is_none());
    let earlier: Vec<ChatMessage> = robin.fetch(&format!("/chats/{noor_id}/messages?before={}", reply.id)).await;
    assert_eq!(earlier.len(), 1);
    let none: Vec<ChatMessage> = lea.fetch(&format!("/chats/{noor_id}/messages")).await;
    assert!(none.is_empty(), "a conversation is between two people only");
    assert_eq!(robin.fails(Method::POST, &format!("/chats/{robin_id}/messages"), &SendMessage { body: "hi me".into() }, StatusCode::BAD_REQUEST).await, "bad_request");
    assert_eq!(robin.fails(Method::POST, "/chats/9999/messages", &SendMessage { body: "hi".into() }, StatusCode::NOT_FOUND).await, "not_found");
    assert_eq!(robin.fails(Method::POST, &format!("/chats/{noor_id}/messages"), &SendMessage { body: "  ".into() }, StatusCode::BAD_REQUEST).await, "bad_request");

    // Presence follows what Robin does: a game, a detail line, away, and privacy.
    let session: PlaySession = robin.ok(Method::POST, "/play/sessions", &StartSession { game_id: game }).await;
    let playing = noor_live.presence_of(robin_id, Status::Playing).await;
    assert_eq!(playing.game_id, Some(game));
    let change = SetPresence { detail: Some("in Factory Friday".into()), rich: Some([("status".to_string(), "Factory Friday".to_string())].into()), away: None };
    let (status, _) = robin.call(Method::PUT, "/presence", &change).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let detailed = noor_live
        .find(|event| match event {
            Event::Presence(presence) if !presence.detail.is_empty() => Some(presence),
            _ => None,
        })
        .await;
    assert_eq!((detailed.detail.as_str(), detailed.rich["status"].as_str()), ("in Factory Friday", "Factory Friday"));

    let _: Value = robin.ok(Method::PATCH, "/me/privacy", &json!({ "share_now_playing": false })).await;
    let veiled = noor_live.presence_of(robin_id, Status::Online).await;
    assert_eq!((veiled.game_id, veiled.detail.as_str()), (None, ""), "with sharing off, friends see online and nothing more");
    let own: Vec<Presence> = robin.fetch("/presence").await;
    assert_eq!(own.iter().find(|presence| presence.user_id == robin_id).unwrap().status, Status::Playing);
    let (status, _) = robin.post(&format!("/play/sessions/{}/stop", session.id), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = noor.call(Method::PUT, "/presence", &SetPresence { away: Some(true), ..Default::default() }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    robin_live.presence_of(noor_id, Status::Away).await;
    assert_eq!(lea.fails(Method::PUT, "/presence", &SetPresence::default(), StatusCode::CONFLICT).await, "conflict");

    // Closing the connection is going offline.
    noor_live.0.close(None).await.unwrap();
    robin_live.presence_of(noor_id, Status::Offline).await;
    let noor_row: Vec<hearth_api::people::Person> = robin.fetch("/users").await;
    assert!(noor_row.iter().find(|person| person.id == noor_id).unwrap().last_seen_at.is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn notifications_arrive_live_and_a_signed_out_device_is_disconnected() {
    let hearth = TestHearth::start().await;
    std::fs::create_dir_all(hearth.games.join("Factorio")).unwrap();
    std::fs::write(hearth.games.join("Factorio/factorio.exe"), b"game").unwrap();
    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    let game = hearth.settled(&admin, 0).await.games[0].id;
    let desktop = hearth.join(&done.invite_code, "robin").await;
    let mut desktop_live = Live::connect(&desktop).await;

    // A sign-in from a device the Hearth has not seen is announced to the ones already signed in.
    let first = sign_in(&hearth, "robin", "Laptop").await;
    let notification = desktop_live
        .find(|event| match event {
            Event::Notification(notification) => Some(notification),
            _ => None,
        })
        .await;
    assert_eq!(notification.kind, NotificationKind::SignIn);
    assert_eq!(notification.title, "New sign-in to home from Laptop");
    let laptop = sign_in(&hearth, "robin", "Laptop").await;
    let listed: Vec<Notification> = desktop.fetch("/notifications?unread=true").await;
    assert_eq!(listed.len(), 1, "the same laptop signing in again is not news");
    // A device has one session: signing in again ends the one it had.
    assert_eq!(first.fails(Method::GET, "/me", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");
    let (status, _) = desktop.post("/notifications/read", &MarkRead::default()).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let unread: Vec<Notification> = desktop.fetch("/notifications?unread=true").await;
    let all: Vec<Notification> = desktop.fetch("/notifications").await;
    assert_eq!((unread.len(), all.len(), all[0].read_at.is_some()), (0, 1, true));

    // A download queued by the admin reaches the device at once when it is connected.
    let mut laptop_live = Live::connect(&laptop).await;
    let devices: Vec<Device> = laptop.fetch("/devices").await;
    let laptop_device = devices.iter().find(|device| device.name == "Laptop").unwrap();
    assert!(laptop_device.online);
    let queued: QueuedDownload = admin.ok(Method::POST, "/admin/downloads", &QueueDownload { device_id: laptop_device.id, game_id: game }).await;
    let pushed = laptop_live
        .find(|event| match event {
            Event::DownloadQueued(download) => Some(download),
            _ => None,
        })
        .await;
    assert_eq!((pushed.id, pushed.game_id), (queued.id, game));

    // A ping keeps the connection, and gets an answer.
    laptop_live.0.send(tungstenite::Message::Text(r#"{"type":"ping"}"#.into())).await.unwrap();
    laptop_live.find(|event| matches!(event, Event::Pong).then_some(())).await;

    // Signing the laptop out from the admin pages cuts its connection, not the desktop's.
    let laptop_session = laptop.fetch::<Me>("/me").await.session_id;
    let sessions: Vec<Session> = admin.fetch("/admin/sessions").await;
    assert_eq!(sessions.iter().filter(|session| session.device_id == Some(laptop_device.id)).count(), 1, "the laptop signed in twice and still has one session");
    let (status, _) = admin.call(Method::DELETE, &format!("/auth/sessions/{laptop_session}"), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let mut closed = false;
    for _ in 0..20 {
        if laptop_live.next().await.is_none() {
            closed = true;
            break;
        }
    }
    assert!(closed, "the signed-out device's connection should end");
    let (status, _) = desktop.call(Method::GET, "/me", &Value::Null).await;
    assert_eq!(status, StatusCode::OK);

    // Nobody connects without a session, and a page from another site cannot use the admin's cookie.
    let anonymous = format!("{}/api/v1/ws", hearth.base().replace("http", "ws")).into_client_request().unwrap();
    let refused = tokio_tungstenite::connect_async(anonymous).await.unwrap_err();
    assert!(matches!(&refused, tungstenite::Error::Http(response) if response.status() == 401), "{refused}");
    let mut foreign = format!("{}/api/v1/ws", hearth.base().replace("http", "ws")).into_client_request().unwrap();
    foreign.headers_mut().insert("authorization", format!("Bearer {}", desktop.token.as_deref().unwrap()).parse().unwrap());
    foreign.headers_mut().insert("origin", "http://evil.example".parse().unwrap());
    let refused = tokio_tungstenite::connect_async(foreign).await.unwrap_err();
    assert!(matches!(&refused, tungstenite::Error::Http(response) if response.status() == 403), "{refused}");
}
