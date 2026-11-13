//! Milestone 7: news, game nights with the ready check, emulators and the Firelink registry.

mod common;

use common::{Api, TestHearth};
use hearth_api::{
    auth::Me,
    community::{AddGameServer, Emulator, EmulatorInput, EmulatorManifest, GameNight, GameServer, Heartbeat, NewsPost, PlanEvent, PublishNews, Ready, ReadyCheck, Registry, Rsvp, SetRsvp},
    library::{Game, Library, ReviewAction, ReviewItem},
    multiplayer::{Ticket, TicketInfo, TicketRequest, VerifyTicket},
    people::{Device, DownloadProgress, DownloadState, Installed, QueuedDownload},
    social::{Notification, NotificationKind},
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn setup(folders: &[(&str, &[u8])]) -> (TestHearth, Api, String, Library) {
    let hearth = TestHearth::start().await;
    for (path, bytes) in folders {
        let file = hearth.games.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    let library = hearth.settled(&admin, 0).await;
    (hearth, admin, done.invite_code, library)
}

async fn expect(api: &Api, method: Method, path: &str, body: &impl serde::Serialize, status: StatusCode) {
    let (got, value) = api.call(method.clone(), path, body).await;
    assert_eq!(got, status, "{method} {path} answered {value}");
}

async fn unread(api: &Api, kind: NotificationKind) -> Vec<Notification> {
    let all: Vec<Notification> = api.fetch("/notifications?unread=true").await;
    all.into_iter().filter(|notification| notification.kind == kind).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn news_reach_everyone_and_a_game_night_shows_who_is_ready() {
    let (hearth, admin, invite, library) = setup(&[("Factorio/factorio.exe", b"version one")]).await;
    let game = library.games[0].clone();
    let v1 = game.version.clone().unwrap().id;
    let (robin, noor, lea) = (hearth.join(&invite, "robin").await, hearth.join(&invite, "noor").await, hearth.join(&invite, "lea").await);

    // News: published by an admin, announced to everyone, pinned posts on top.
    let post = PublishNews { title: "Stardew Valley 1.6.15 is on the Hearth".into(), body: "What changed, and **what to do**.".into(), pinned: false };
    assert_eq!(robin.fails(Method::POST, "/admin/news", &post, StatusCode::FORBIDDEN).await, "forbidden");
    assert_eq!(admin.fails(Method::POST, "/admin/news", &PublishNews { title: " ".into(), ..post.clone() }, StatusCode::BAD_REQUEST).await, "bad_request");
    let first: NewsPost = admin.ok(Method::POST, "/admin/news", &post).await;
    let pinned: NewsPost = admin.ok(Method::POST, "/admin/news", &PublishNews { title: "House rules".into(), body: "Be kind.".into(), pinned: true }).await;
    let feed: Vec<NewsPost> = robin.fetch("/news").await;
    assert_eq!(feed.iter().map(|post| (post.id, post.author.as_str())).collect::<Vec<_>>(), [(pinned.id, "mika"), (first.id, "mika")]);
    let told = unread(&robin, NotificationKind::News).await;
    assert_eq!((told.len(), told[1].title.as_str(), told[1].data["news_id"]), (2, "New on home: Stardew Valley 1.6.15 is on the Hearth", first.id));
    expect(&admin, Method::PATCH, &format!("/admin/news/{}", pinned.id), &json!({ "pinned": false }), StatusCode::NO_CONTENT).await;
    expect(&admin, Method::DELETE, &format!("/admin/news/{}", first.id), &Value::Null, StatusCode::NO_CONTENT).await;
    let feed: Vec<NewsPost> = noor.fetch("/news").await;
    assert_eq!(feed.iter().map(|post| (post.id, post.pinned)).collect::<Vec<_>>(), [(pinned.id, false)]);

    // A game night: planned by an admin, answered by players.
    let soon = hearth::db::now() + 2 * 86400;
    let plan = PlanEvent { title: "Factory Friday".into(), game_id: game.id, starts_at: soon, note: "Bring iron.".into() };
    assert_eq!(admin.fails(Method::POST, "/admin/events", &PlanEvent { starts_at: 1000, ..plan.clone() }, StatusCode::BAD_REQUEST).await, "bad_request");
    let night: GameNight = admin.ok(Method::POST, "/admin/events", &plan).await;
    assert_eq!(unread(&lea, NotificationKind::Event).await[0].title, "Factory Friday was planned");
    for (api, rsvp) in [(&robin, Rsvp::Going), (&noor, Rsvp::Maybe), (&lea, Rsvp::Going)] {
        let answered: GameNight = api.ok(Method::PUT, &format!("/events/{}/rsvp", night.id), &SetRsvp { rsvp: Some(rsvp) }).await;
        assert_eq!(answered.mine, Some(rsvp));
    }
    let nights: Vec<GameNight> = noor.fetch("/events").await;
    assert_eq!(nights[0].answers.iter().map(|answer| (answer.username.as_str(), answer.rsvp)).collect::<Vec<_>>(), [("lea", Rsvp::Going), ("noor", Rsvp::Maybe), ("robin", Rsvp::Going)]);
    let cleared: GameNight = noor.ok(Method::PUT, &format!("/events/{}/rsvp", night.id), &SetRsvp { rsvp: None }).await;
    assert_eq!((cleared.mine, cleared.answers.len()), (None, 2));

    // The ready check. Robin has the game on his PC. Lea does not.
    expect(&robin, Method::PUT, "/devices/current/installed", &vec![Installed { game_id: game.id, version_id: v1 }], StatusCode::NO_CONTENT).await;
    let states = |check: &ReadyCheck| check.guests.iter().map(|guest| (guest.username.clone(), guest.ready)).collect::<Vec<_>>();
    let check: ReadyCheck = lea.fetch(&format!("/events/{}/ready", night.id)).await;
    assert_eq!(states(&check), [("lea".to_string(), Ready::NotKindled), ("robin".to_string(), Ready::Ready)]);

    // A new version comes out before the night. Now Robin needs an update.
    std::fs::create_dir_all(hearth.games.join("Factorio 2.0")).unwrap();
    std::fs::write(hearth.games.join("Factorio 2.0/factorio.exe"), b"version two").unwrap();
    hearth.rescan(&admin).await;
    let review: Vec<ReviewItem> = admin.fetch("/admin/review").await;
    expect(&admin, Method::POST, &format!("/admin/review/{}", review[0].id), &ReviewAction::Update { game_id: game.id, version: "2.0".into(), note: String::new() }, StatusCode::NO_CONTENT).await;
    let before: Library = admin.fetch("/admin/library").await;
    hearth.settled(&admin, before.scan.scans_finished - 1).await;
    let v2 = admin.fetch::<Game>(&format!("/games/{}", game.id)).await.summary.version.unwrap();
    assert_eq!(v2.name, "2.0");
    assert_eq!(unread(&robin, NotificationKind::Update).await[0].title, "Factorio 2.0 is on home", "a new version is announced to everyone");
    let check: ReadyCheck = robin.fetch(&format!("/events/{}/ready", night.id)).await;
    assert_eq!((check.version.clone(), states(&check)), (Some(v2.clone()), vec![("lea".to_string(), Ready::NotKindled), ("robin".to_string(), Ready::NeedsUpdate)]));

    // The admin has the Hearth fetch it for everyone who is not ready.
    assert_eq!(robin.fails(Method::POST, &format!("/admin/events/{}/ready", night.id), &Value::Null, StatusCode::FORBIDDEN).await, "forbidden");
    let check: ReadyCheck = admin.ok(Method::POST, &format!("/admin/events/{}/ready", night.id), &Value::Null).await;
    assert_eq!(states(&check), [("lea".to_string(), Ready::GettingReady), ("robin".to_string(), Ready::GettingReady)]);
    let queue: Vec<QueuedDownload> = robin.fetch("/devices/current/queue").await;
    assert_eq!(queue.iter().map(|item| (item.game_id, item.version_id)).collect::<Vec<_>>(), [(game.id, v2.id)]);
    expect(&robin, Method::POST, &format!("/devices/current/queue/{}", queue[0].id), &DownloadProgress { state: DownloadState::Done }, StatusCode::NO_CONTENT).await;
    expect(&robin, Method::PUT, "/devices/current/installed", &vec![Installed { game_id: game.id, version_id: v2.id }], StatusCode::NO_CONTENT).await;
    let check: ReadyCheck = robin.fetch(&format!("/events/{}/ready", night.id)).await;
    assert_eq!(states(&check), [("lea".to_string(), Ready::GettingReady), ("robin".to_string(), Ready::Ready)]);
    let devices: Vec<Device> = lea.fetch("/devices").await;
    assert_eq!(devices.len(), 1);

    // An hour before the start, everyone who is going gets one reminder.
    let state = &hearth.running().state;
    sqlx::query("UPDATE events SET starts_at = ? WHERE id = ?").bind(hearth::db::now() + 1800).bind(night.id).execute(&state.db).await.unwrap();
    hearth::jobs::tidy(state).await.unwrap();
    hearth::jobs::tidy(state).await.unwrap();
    let reminders: Vec<Notification> = unread(&lea, NotificationKind::Event).await.into_iter().filter(|notification| notification.title.contains("within the hour")).collect();
    assert_eq!(reminders.len(), 1);
    assert!(unread(&noor, NotificationKind::Event).await.iter().all(|notification| !notification.title.contains("within the hour")), "Noor took her answer back");

    expect(&admin, Method::DELETE, &format!("/admin/events/{}", night.id), &Value::Null, StatusCode::NO_CONTENT).await;
    let nights: Vec<GameNight> = robin.fetch("/events").await;
    assert!(nights.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_emulator_is_stored_once_and_games_point_at_it() {
    let (hearth, admin, invite, library) = setup(&[("Crash Bandicoot/crash.bin", b"a game for an older system")]).await;
    let game = library.games[0].id;
    let robin = hearth.join(&invite, "robin").await;
    let emulators = hearth.running().state.paths.dir("emulators");
    std::fs::create_dir_all(emulators.join("DuckStation/bios")).unwrap();
    std::fs::write(emulators.join("DuckStation/duckstation.exe"), vec![7u8; 5000]).unwrap();
    std::fs::write(emulators.join("DuckStation/bios/readme.txt"), b"put a bios here").unwrap();

    let input = |name: &str, command: &str, folder: &str| EmulatorInput { name: name.into(), version: "0.1".into(), systems: vec!["PlayStation".into()], command: command.into(), folder: folder.into() };
    let good = input("DuckStation", "duckstation.exe -fullscreen \"{rom}\"", "");
    assert_eq!(robin.fails(Method::POST, "/admin/emulators", &good, StatusCode::FORBIDDEN).await, "forbidden");
    assert_eq!(admin.fails(Method::POST, "/admin/emulators", &input("DuckStation", "duckstation.exe", ""), StatusCode::BAD_REQUEST).await, "bad_request");
    assert_eq!(admin.fails(Method::POST, "/admin/emulators", &input("PCSX2", "pcsx2.exe {rom}", ""), StatusCode::BAD_REQUEST).await, "bad_request", "its files are not there yet");
    assert_eq!(admin.fails(Method::POST, "/admin/emulators", &input("Sneaky", "x {rom}", "../../games"), StatusCode::BAD_REQUEST).await, "bad_request");
    let emulator: Emulator = admin.ok(Method::POST, "/admin/emulators", &good).await;
    assert_eq!((emulator.folder.as_str(), emulator.games), ("DuckStation", 0));
    assert_eq!(admin.fails(Method::POST, "/admin/emulators", &input("duckstation", "x {rom}", "DuckStation"), StatusCode::CONFLICT).await, "conflict");

    // The Hearth hashes its files in the background. Then any player can fetch it, once.
    let mut ready = None;
    for _ in 0..200 {
        let list: Vec<Emulator> = robin.fetch("/emulators").await;
        if list[0].ready {
            ready = Some(list[0].clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let ready = ready.expect("the emulator should become ready");
    assert_eq!(ready.bytes, 5015);
    let manifest: EmulatorManifest = robin.fetch(&format!("/emulators/{}/manifest", emulator.id)).await;
    assert_eq!(manifest.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["bios/readme.txt", "duckstation.exe"]);
    let (status, bytes) = robin.bytes(&format!("/emulators/{}/files/1", emulator.id), Some((0, 99))).await;
    assert_eq!((status, bytes.len()), (StatusCode::PARTIAL_CONTENT, 100));
    let (_, whole) = robin.bytes(&format!("/emulators/{}/files/1", emulator.id), None).await;
    assert_eq!(hearth::tls::hex(blake3::hash(&whole).as_bytes()), manifest.files[1].hash);

    // The game names the emulator instead of carrying a copy.
    let edited: Game = admin.ok(Method::PATCH, &format!("/admin/games/{game}"), &json!({ "emulator_id": emulator.id })).await;
    assert_eq!(edited.summary.emulator_id, Some(emulator.id));
    assert_eq!(admin.fails(Method::PATCH, &format!("/admin/games/{game}"), &json!({ "emulator_id": 999 }), StatusCode::NOT_FOUND).await, "not_found");
    let list: Vec<Emulator> = robin.fetch("/emulators").await;
    assert_eq!(list[0].games, 1);
    let cleared: Game = admin.ok(Method::PATCH, &format!("/admin/games/{game}"), &json!({ "emulator_id": null })).await;
    assert_eq!(cleared.summary.emulator_id, None);
    let _: Game = admin.ok(Method::PATCH, &format!("/admin/games/{game}"), &json!({ "emulator_id": emulator.id })).await;

    // Editing, and removing: the files stay, the games simply have no emulator any more.
    let renamed: Emulator = admin.ok(Method::PATCH, &format!("/admin/emulators/{}", emulator.id), &input("DuckStation", "duckstation.exe -batch \"{rom}\"", "DuckStation")).await;
    assert!(renamed.command.contains("-batch"));
    expect(&admin, Method::DELETE, &format!("/admin/emulators/{}", emulator.id), &Value::Null, StatusCode::NO_CONTENT).await;
    assert!(emulators.join("DuckStation/duckstation.exe").exists());
    assert_eq!(robin.fetch::<Game>(&format!("/games/{game}")).await.summary.emulator_id, None);
    assert_eq!(robin.fails(Method::GET, &format!("/emulators/{}/manifest", emulator.id), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
}

/// A stand-in dedicated server that answers the server query many games speak, challenge included.
async fn query_server() -> u16 {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut buffer = [0u8; 256];
        while let Ok((length, from)) = socket.recv_from(&mut buffer).await {
            let request = &buffer[..length];
            let reply = if request.ends_with(&[1, 2, 3, 4]) {
                let mut info = b"\xff\xff\xff\xffI\x11Sunday factory\0nauvis\0factorio\0Factorio\0".to_vec();
                info.extend_from_slice(&[0, 0, 3, 16, 0, b'd', b'w', 0, 1]);
                info
            } else {
                b"\xff\xff\xff\xffA\x01\x02\x03\x04".to_vec()
            };
            let _ = socket.send_to(&reply, from).await;
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
async fn firelink_lists_servers_the_admin_adds_and_servers_that_report_in() {
    let (hearth, admin, invite, library) = setup(&[("Factorio/factorio.exe", b"game")]).await;
    let game = library.games[0].id;
    let robin = hearth.join(&invite, "robin").await;

    // Three servers: one that answers the server query, one that only accepts connections, one that is down.
    let query_port = query_server().await;
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_port = tcp.local_addr().unwrap().port();
    let dead_port = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
    let add = |name: &str, port: u16| AddGameServer { game_id: game, name: name.into(), address: format!("127.0.0.1:{port}") };

    assert_eq!(robin.fails(Method::POST, "/admin/game-servers", &add("Mine", tcp_port), StatusCode::FORBIDDEN).await, "forbidden");
    assert_eq!(admin.fails(Method::POST, "/admin/game-servers", &AddGameServer { address: "not an address".into(), ..add("Bad", 1) }, StatusCode::BAD_REQUEST).await, "bad_request");
    let queried: GameServer = admin.ok(Method::POST, "/admin/game-servers", &add("Sunday factory", query_port)).await;
    assert_eq!((queried.online, queried.players, queried.max_players, queried.map.as_str()), (true, 3, 16, "nauvis"), "players and map come from the server itself");
    let plain: GameServer = admin.ok(Method::POST, "/admin/game-servers", &add("Rail Empire", tcp_port)).await;
    assert!(plain.online && plain.ping_ms.is_some() && plain.added_by == "mika");
    let down: GameServer = admin.ok(Method::POST, "/admin/game-servers", &add("Sleeping", dead_port)).await;
    assert!(!down.online);
    assert_eq!(admin.fails(Method::POST, "/admin/game-servers", &add("Rail Empire", tcp_port), StatusCode::CONFLICT).await, "conflict");

    // A server that comes back is noticed by the next check.
    let revived = tokio::net::TcpListener::bind(("127.0.0.1", dead_port)).await.unwrap();
    hearth::routes::firelink::check_all(&hearth.running().state).await.unwrap();
    let servers: Vec<GameServer> = robin.fetch(&format!("/game-servers?game_id={game}")).await;
    assert_eq!(servers.iter().map(|server| (server.name.as_str(), server.online)).collect::<Vec<_>>(), [("Rail Empire", true), ("Sleeping", true), ("Sunday factory", true)]);
    drop((tcp, revived));

    // A server lists itself with the registry token, and drops off when it stops reporting.
    let registry: Registry = admin.fetch("/admin/game-servers").await;
    assert!(registry.heartbeat_token.starts_with("hb_"));
    let mut server = hearth.client();
    server.token = Some(registry.heartbeat_token.clone());
    let beat = Heartbeat { name: "Noor's cabin".into(), game_id: Some(game), steam_app_id: None, port: 24642, host: None, players: 2, max_players: 4, map: "Farm".into(), version: "1.6".into() };
    expect(&server, Method::POST, "/game-servers/heartbeat", &beat, StatusCode::NO_CONTENT).await;
    expect(&server, Method::POST, "/game-servers/heartbeat", &Heartbeat { players: 3, ..beat.clone() }, StatusCode::NO_CONTENT).await;
    let listed: Vec<GameServer> = robin.fetch("/game-servers").await;
    let cabin = listed.iter().find(|server| server.name == "Noor's cabin").unwrap();
    assert_eq!((cabin.address.as_str(), cabin.players, cabin.self_listed, cabin.online), ("127.0.0.1:24642", 3, true, true));
    assert_eq!(listed.len(), 4, "a second heartbeat updates the entry instead of adding one");
    assert_eq!(server.fails(Method::POST, "/game-servers/heartbeat", &Heartbeat { game_id: None, ..beat.clone() }, StatusCode::BAD_REQUEST).await, "bad_request");
    assert_eq!(server.fails(Method::POST, "/game-servers/heartbeat", &Heartbeat { game_id: Some(999), ..beat.clone() }, StatusCode::NOT_FOUND).await, "not_found");
    let mut stranger = hearth.client();
    stranger.token = Some("hb_guessed".into());
    assert_eq!(stranger.fails(Method::POST, "/game-servers/heartbeat", &beat, StatusCode::UNAUTHORIZED).await, "unauthorized");

    // A dedicated server checks a player's ticket with the same token.
    let ticket: Ticket = robin.ok(Method::POST, "/tickets", &TicketRequest { game_id: game }).await;
    let info: TicketInfo = server.ok(Method::POST, "/tickets/verify", &VerifyTicket { ticket: ticket.ticket.clone() }).await;
    assert_eq!((info.username.as_str(), info.owned), ("robin", true));
    assert_eq!(stranger.fails(Method::POST, "/tickets/verify", &VerifyTicket { ticket: ticket.ticket }, StatusCode::UNAUTHORIZED).await, "unauthorized");
    assert_eq!(server.fails(Method::GET, "/games", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized", "the token lists servers and checks tickets, nothing more");

    let state = &hearth.running().state;
    sqlx::query("UPDATE game_servers SET last_seen_at = last_seen_at - 100 WHERE self_listed = 1").execute(&state.db).await.unwrap();
    hearth::routes::firelink::check_all(state).await.unwrap();
    let listed: Vec<GameServer> = robin.fetch("/game-servers").await;
    assert!(!listed.iter().any(|server| server.self_listed), "three missed heartbeats and the server is off the list");

    // Rotating the token shuts out whoever had the old one.
    let rotated: Registry = admin.ok(Method::POST, "/admin/game-servers/token", &Value::Null).await;
    assert_ne!(rotated.heartbeat_token, registry.heartbeat_token);
    assert_eq!(server.fails(Method::POST, "/game-servers/heartbeat", &beat, StatusCode::UNAUTHORIZED).await, "unauthorized");
    expect(&admin, Method::DELETE, &format!("/admin/game-servers/{}", plain.id), &Value::Null, StatusCode::NO_CONTENT).await;
    let me: Me = robin.fetch("/me").await;
    assert_eq!(me.user.username, "robin");
}
