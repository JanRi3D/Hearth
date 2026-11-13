//! Milestone 3: accounts as an admin runs them, devices and remote downloads, and play data.

mod common;

use common::{ADMIN, Api, PASSWORD, TestHearth, device, login_request};
use hearth_api::{
    admin::Overview,
    auth::{CreateInvite, Invite, LoginResponse, RegisterRequest, ResetPassword, Role, Session},
    library::Library,
    people::{AdminUser, Device, DownloadProgress, DownloadState, Installed, Person, Privacy, QueueDownload, QueuedDownload, ResetInfo, ResetLink},
    play::{AchievementDef, Achievements, Collection, CollectionPatch, GamePlayer, Note, PlaySession, Playtime, Profile, StartSession, Stats},
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// A Hearth with one game, its admin, and an invite.
async fn hearth_with_a_game() -> (TestHearth, Api, String, Library) {
    let hearth = TestHearth::start().await;
    std::fs::create_dir_all(hearth.games.join("Factorio")).unwrap();
    std::fs::write(hearth.games.join("Factorio/factorio.exe"), b"a small stand-in for a game").unwrap();
    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    let library = hearth.settled(&admin, 0).await;
    (hearth, admin, done.invite_code, library)
}

async fn sign_in(hearth: &TestHearth, username: &str, password: &str, device_name: &str) -> Api {
    let mut api = hearth.client();
    let mut request = login_request(username, password);
    request.device = device(device_name);
    let login: LoginResponse = api.ok(Method::POST, "/auth/login", &request).await;
    api.token = login.token;
    api
}

async fn expect(api: &Api, method: Method, path: &str, body: &impl serde::Serialize, status: StatusCode) {
    let (got, value) = api.call(method.clone(), path, body).await;
    assert_eq!(got, status, "{method} {path} answered {value}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_runs_invites_roles_reset_links_and_removals() {
    let (hearth, admin, first_invite, _) = hearth_with_a_game().await;
    let robin = hearth.join(&first_invite, "robin").await;

    // Invites: once, five times or unlimited, and gone when deleted.
    let single: Invite = admin.ok(Method::POST, "/admin/invites", &CreateInvite { max_uses: Some(1), expires_in_days: None }).await;
    assert!(single.code.starts_with("BNFR-") && single.max_uses == Some(1) && single.uses == 0);
    let open: Invite = admin.ok(Method::POST, "/admin/invites", &CreateInvite { max_uses: None, expires_in_days: Some(30) }).await;
    assert_eq!(admin.fails(Method::POST, "/admin/invites", &json!({ "max_uses": 3 }), StatusCode::BAD_REQUEST).await, "bad_request");
    assert_eq!(robin.fails(Method::POST, "/admin/invites", &json!({ "max_uses": 1 }), StatusCode::FORBIDDEN).await, "forbidden");
    let noor = hearth.join(&single.code, "noor").await;
    let listed: Vec<Invite> = admin.fetch("/admin/invites").await;
    assert_eq!(listed.iter().map(|invite| invite.code.as_str()).collect::<Vec<_>>(), [open.code.as_str(), first_invite.as_str()], "a spent invite leaves the list");
    expect(&admin, Method::DELETE, &format!("/admin/invites/{}", open.id), &Value::Null, StatusCode::NO_CONTENT).await;
    let late = RegisterRequest { invite_code: Some(open.code), username: "late".into(), password: PASSWORD.into(), device: device("PC") };
    assert_eq!(hearth.client().fails(Method::POST, "/auth/register", &late, StatusCode::FORBIDDEN).await, "invite_invalid");

    // Everyone on a Hearth sees who else is on it. Only admins see the account details.
    let people: Vec<Person> = noor.fetch("/users").await;
    assert_eq!(people.iter().map(|person| person.username.as_str()).collect::<Vec<_>>(), [ADMIN, "noor", "robin"]);
    let users: Vec<AdminUser> = admin.fetch("/admin/users").await;
    let robin_row = users.iter().find(|user| user.username == "robin").unwrap().clone();
    assert_eq!((robin_row.role, robin_row.devices, robin_row.online), (Role::Player, 1, true));
    assert_eq!(noor.fails(Method::GET, "/admin/users", &Value::Null, StatusCode::FORBIDDEN).await, "forbidden");

    // Roles. A Hearth never loses its last admin.
    let me = users.iter().find(|user| user.username == ADMIN).unwrap().id;
    assert_eq!(admin.fails(Method::PATCH, &format!("/admin/users/{me}"), &json!({ "role": "player" }), StatusCode::CONFLICT).await, "conflict");
    expect(&admin, Method::PATCH, &format!("/admin/users/{}", robin_row.id), &json!({ "role": "admin" }), StatusCode::NO_CONTENT).await;
    let _: Vec<AdminUser> = robin.fetch("/admin/users").await;
    expect(&admin, Method::PATCH, &format!("/admin/users/{}", robin_row.id), &json!({ "role": "player" }), StatusCode::NO_CONTENT).await;
    assert_eq!(robin.fails(Method::GET, "/admin/users", &Value::Null, StatusCode::FORBIDDEN).await, "forbidden");

    // Robin forgets the password and locks the account. A reset link fixes both.
    for _ in 0..5 {
        hearth.client().fails(Method::POST, "/auth/login", &login_request("robin", "not it at all"), StatusCode::UNAUTHORIZED).await;
    }
    assert_eq!(hearth.client().fails(Method::POST, "/auth/login", &login_request("robin", PASSWORD), StatusCode::FORBIDDEN).await, "account_locked");
    let link: ResetLink = admin.ok(Method::POST, &format!("/admin/users/{}/reset-link", robin_row.id), &Value::Null).await;
    let token = link.path.as_deref().unwrap().strip_prefix("/reset/").unwrap().to_string();
    let pending: Vec<ResetLink> = admin.fetch("/admin/reset-links").await;
    assert_eq!((pending.len(), pending[0].path.clone()), (1, None), "the link itself is shown once and never stored");

    let anyone = hearth.client();
    let info: ResetInfo = anyone.fetch(&format!("/auth/reset/{token}")).await;
    assert_eq!(info.username, "robin");
    assert_eq!(anyone.fails(Method::GET, "/auth/reset/not-a-token", &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
    let weak = ResetPassword { token: token.clone(), new_password: "short".into() };
    assert_eq!(anyone.fails(Method::POST, "/auth/reset", &weak, StatusCode::BAD_REQUEST).await, "weak_password");
    let fresh = ResetPassword { token: token.clone(), new_password: "a brand new password".into() };
    expect(&anyone, Method::POST, "/auth/reset", &fresh, StatusCode::NO_CONTENT).await;
    assert_eq!(anyone.fails(Method::POST, "/auth/reset", &fresh, StatusCode::NOT_FOUND).await, "not_found", "a link works once");
    assert_eq!(robin.fails(Method::GET, "/me", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized", "using it signs every device out");
    let robin = sign_in(&hearth, "robin", "a brand new password", "Desktop").await;

    // Signed-in devices of everyone, and signing one out.
    let sessions: Vec<Session> = admin.fetch("/admin/sessions").await;
    let robins = sessions.iter().find(|session| session.username == "robin").unwrap();
    expect(&admin, Method::DELETE, &format!("/auth/sessions/{}", robins.id), &Value::Null, StatusCode::NO_CONTENT).await;
    assert_eq!(robin.fails(Method::GET, "/me", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");

    // Removing an account signs it out everywhere at once.
    assert_eq!(admin.fails(Method::DELETE, &format!("/admin/users/{me}"), &Value::Null, StatusCode::CONFLICT).await, "conflict");
    let noor_id = users.iter().find(|user| user.username == "noor").unwrap().id;
    expect(&admin, Method::DELETE, &format!("/admin/users/{noor_id}"), &Value::Null, StatusCode::NO_CONTENT).await;
    assert_eq!(noor.fails(Method::GET, "/me", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");
    assert_eq!(hearth.client().fails(Method::POST, "/auth/login", &login_request("noor", PASSWORD), StatusCode::UNAUTHORIZED).await, "invalid_credentials");
    let people: Vec<Person> = admin.fetch("/users").await;
    assert!(!people.iter().any(|person| person.username == "noor"));

    // Thirty days later the account and its saves are deleted for good.
    let state = &hearth.running().state;
    let saves = state.paths.dir("saves").join(noor_id.to_string());
    std::fs::create_dir_all(&saves).unwrap();
    hearth::jobs::tidy(state).await.unwrap();
    assert!(saves.exists(), "within 30 days the saves are kept");
    sqlx::query("UPDATE users SET removed_at = removed_at - 31 * 86400 WHERE id = ?").bind(noor_id).execute(&state.db).await.unwrap();
    hearth::jobs::tidy(state).await.unwrap();
    assert!(!saves.exists());
    let overview: Overview = admin.fetch("/admin/overview").await;
    assert_eq!(overview.accounts, 2);
    assert!(overview.online_24h.len() == 24 && overview.online_24h[23] >= 1, "the chart has this hour in it");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admin_queues_a_download_for_a_device_that_fetches_it_when_it_comes_online() {
    let (hearth, admin, invite, library) = hearth_with_a_game().await;
    let game = library.games[0].clone();
    let version = game.version.clone().unwrap().id;
    let desktop = hearth.join(&invite, "robin").await;
    let laptop = sign_in(&hearth, "robin", PASSWORD, "Laptop").await;

    // The desktop has the game and says so.
    expect(&desktop, Method::PUT, "/devices/current/installed", &vec![Installed { game_id: game.id, version_id: version }], StatusCode::NO_CONTENT).await;
    let mine: Vec<Device> = laptop.fetch("/devices").await;
    let desktop_id = mine.iter().find(|device| device.name == "Desktop").unwrap().id;
    let laptop_id = mine.iter().find(|device| device.name == "Laptop").unwrap().id;
    assert_eq!(mine.iter().find(|device| device.id == desktop_id).unwrap().installed, [Installed { game_id: game.id, version_id: version }]);
    let all: Vec<Device> = admin.fetch("/admin/devices").await;
    assert!(all.len() >= 3 && all.iter().any(|device| device.username == ADMIN));

    // Kindle on the laptop from afar. Asking twice is one download.
    let request = QueueDownload { device_id: laptop_id, game_id: game.id };
    let queued: QueuedDownload = admin.ok(Method::POST, "/admin/downloads", &request).await;
    let again: QueuedDownload = admin.ok(Method::POST, "/admin/downloads", &request).await;
    assert_eq!((queued.id, queued.version_id, queued.game_title.as_str(), queued.device_name.as_str()), (again.id, version, "Factorio", "Laptop"));
    assert_eq!(desktop.fails(Method::POST, "/admin/downloads", &request, StatusCode::FORBIDDEN).await, "forbidden");
    let ready = QueueDownload { device_id: desktop_id, game_id: game.id };
    assert_eq!(admin.fails(Method::POST, "/admin/downloads", &ready, StatusCode::CONFLICT).await, "conflict", "the desktop already has it");

    // The laptop comes online, finds its queue, and reports as it goes. The desktop's queue is empty.
    let waiting: Vec<QueuedDownload> = laptop.fetch("/devices/current/queue").await;
    assert_eq!(waiting.iter().map(|item| (item.id, item.state)).collect::<Vec<_>>(), [(queued.id, DownloadState::Queued)]);
    let none: Vec<QueuedDownload> = desktop.fetch("/devices/current/queue").await;
    assert!(none.is_empty());
    let report = |state| DownloadProgress { state };
    assert_eq!(desktop.fails(Method::POST, &format!("/devices/current/queue/{}", queued.id), &report(DownloadState::Done), StatusCode::NOT_FOUND).await, "not_found");
    expect(&laptop, Method::POST, &format!("/devices/current/queue/{}", queued.id), &report(DownloadState::Started), StatusCode::NO_CONTENT).await;
    let shown: Vec<QueuedDownload> = admin.fetch("/admin/downloads").await;
    assert_eq!(shown[0].state, DownloadState::Started);
    expect(&laptop, Method::POST, &format!("/devices/current/queue/{}", queued.id), &report(DownloadState::Done), StatusCode::NO_CONTENT).await;
    let shown: Vec<QueuedDownload> = admin.fetch("/admin/downloads").await;
    assert!(shown.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn play_data_follows_the_account_and_friends_see_what_privacy_allows() {
    let (hearth, admin, invite, library) = hearth_with_a_game().await;
    let game = library.games[0].id;
    let robin = hearth.join(&invite, "robin").await;
    let noor = hearth.join(&invite, "noor").await;
    let db = &hearth.running().state.db;
    let robin_id = robin.fetch::<hearth_api::auth::Me>("/me").await.user.id;

    // An hour of Factorio: start, heartbeats, stop.
    let session: PlaySession = robin.ok(Method::POST, "/play/sessions", &StartSession { game_id: game }).await;
    assert_eq!(session.ended_at, None);
    sqlx::query("UPDATE play_sessions SET started_at = started_at - 3600 WHERE id = ?").bind(session.id).execute(db).await.unwrap();
    expect(&robin, Method::POST, &format!("/play/sessions/{}/heartbeat", session.id), &Value::Null, StatusCode::NO_CONTENT).await;
    let profile: Profile = noor.fetch(&format!("/users/{robin_id}/profile")).await;
    assert_eq!(profile.now_playing.as_ref().map(|playing| playing.game_id), Some(game), "friends see what robin is playing");
    assert_eq!(noor.fails(Method::POST, &format!("/play/sessions/{}/stop", session.id), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
    expect(&robin, Method::POST, &format!("/play/sessions/{}/stop", session.id), &Value::Null, StatusCode::NO_CONTENT).await;
    assert_eq!(robin.fails(Method::POST, &format!("/play/sessions/{}/heartbeat", session.id), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");

    // A PC that lost power: the session ends at its last heartbeat.
    let crashed: PlaySession = robin.ok(Method::POST, "/play/sessions", &StartSession { game_id: game }).await;
    sqlx::query("UPDATE play_sessions SET started_at = started_at - 1800, last_beat_at = last_beat_at - 600 WHERE id = ?").bind(crashed.id).execute(db).await.unwrap();
    let playtime: Vec<Playtime> = robin.fetch("/me/playtime").await;
    assert_eq!(playtime.len(), 1);
    assert!((4795..=4810).contains(&playtime[0].seconds), "an hour plus twenty minutes, got {}", playtime[0].seconds);
    assert_eq!(playtime[0].sessions, 2);
    let sessions: Vec<PlaySession> = robin.fetch(&format!("/me/sessions?game_id={game}")).await;
    assert!(sessions.iter().all(|session| session.ended_at.is_some()));
    let users: Vec<hearth_api::people::AdminUser> = admin.fetch("/admin/users").await;
    assert!(users.iter().find(|user| user.id == robin_id).unwrap().playtime_seconds >= 4795);

    // Achievements and stats, the way a game reports them.
    for _ in 0..2 {
        expect(&robin, Method::PUT, &format!("/games/{game}/achievements/iron-throne-1"), &Value::Null, StatusCode::NO_CONTENT).await;
    }
    expect(&robin, Method::PUT, &format!("/games/{game}/achievements/lazy-bastard"), &Value::Null, StatusCode::NO_CONTENT).await;
    expect(&robin, Method::DELETE, &format!("/games/{game}/achievements/lazy-bastard"), &Value::Null, StatusCode::NO_CONTENT).await;
    let defs = vec![AchievementDef { name: "iron-throne-1".into(), title: "Iron throne 1".into(), description: "Produce 20k iron plates per hour.".into(), hidden: false }];
    assert_eq!(robin.fails(Method::PUT, &format!("/admin/games/{game}/achievements"), &defs, StatusCode::FORBIDDEN).await, "forbidden");
    expect(&admin, Method::PUT, &format!("/admin/games/{game}/achievements"), &defs, StatusCode::NO_CONTENT).await;
    let achievements: Achievements = robin.fetch(&format!("/games/{game}/achievements")).await;
    assert_eq!(achievements.defined, defs);
    assert_eq!(achievements.unlocked.iter().map(|unlocked| unlocked.name.as_str()).collect::<Vec<_>>(), ["iron-throne-1"]);

    let _: Stats = robin.ok(Method::PUT, &format!("/games/{game}/stats"), &json!({ "rockets": 3, "hours": 41.5 })).await;
    let stats: Stats = robin.ok(Method::PUT, &format!("/games/{game}/stats"), &json!({ "rockets": 4 })).await;
    assert_eq!(stats, Stats::from([("hours".to_string(), 41.5), ("rockets".to_string(), 4.0)]), "a stat that is not named keeps its value");
    let noors: Stats = noor.fetch(&format!("/games/{game}/stats")).await;
    assert!(noors.is_empty());

    // Collections and notes are kept on the Hearth, so they follow the account to another PC.
    let couch: Collection = robin.ok(Method::POST, "/collections", &CollectionPatch { name: Some("Couch co-op".into()), game_ids: Some(vec![game, 9999]) }).await;
    assert_eq!(couch.game_ids, [game], "a game that does not exist is left out");
    assert_eq!(robin.fails(Method::POST, "/collections", &json!({ "name": "couch co-op" }), StatusCode::CONFLICT).await, "conflict");
    assert_eq!(noor.fails(Method::PATCH, &format!("/collections/{}", couch.id), &json!({ "name": "Mine now" }), StatusCode::NOT_FOUND).await, "not_found");
    let note: Note = robin.ok(Method::PUT, &format!("/games/{game}/note"), &json!({ "text": "Bring iron. We are out again." })).await;
    assert!(note.updated_at.is_some());
    let laptop = sign_in(&hearth, "robin", PASSWORD, "Laptop").await;
    let collections: Vec<Collection> = laptop.fetch("/collections").await;
    assert_eq!(collections[0].name, "Couch co-op");
    let note: Note = laptop.fetch(&format!("/games/{game}/note")).await;
    assert_eq!(note.text, "Bring iron. We are out again.");
    let renamed: Collection = laptop.ok(Method::PATCH, &format!("/collections/{}", couch.id), &json!({ "name": "Sofa", "game_ids": [] })).await;
    assert_eq!((renamed.name.as_str(), renamed.game_ids.len()), ("Sofa", 0));
    expect(&laptop, Method::DELETE, &format!("/collections/{}", couch.id), &Value::Null, StatusCode::NO_CONTENT).await;
    let _: Note = laptop.ok(Method::PUT, &format!("/games/{game}/note"), &json!({ "text": "  " })).await;
    let note: Note = robin.fetch(&format!("/games/{game}/note")).await;
    assert_eq!((note.text.as_str(), note.updated_at), ("", None), "an empty note is no note");

    // What friends see: everything by default, and nothing of what is switched off.
    let profile: Profile = noor.fetch(&format!("/users/{robin_id}/profile")).await;
    assert_eq!(profile.playtime.as_ref().map(Vec::len), Some(1));
    assert_eq!(profile.achievements.as_ref().map(|games| games[0].unlocked.len()), Some(1));
    let board: Vec<GamePlayer> = noor.fetch(&format!("/games/{game}/players")).await;
    assert_eq!(board.iter().map(|player| (player.username.as_str(), player.achievements)).collect::<Vec<_>>(), [("robin", Some(1))]);

    let privacy: Privacy = robin.ok(Method::PATCH, "/me/privacy", &json!({ "share_playtime": false, "share_achievements": false, "share_now_playing": false })).await;
    assert_eq!(privacy, Privacy { share_playtime: false, share_achievements: false, share_now_playing: false });
    let _: PlaySession = robin.ok(Method::POST, "/play/sessions", &StartSession { game_id: game }).await;
    let profile: Profile = noor.fetch(&format!("/users/{robin_id}/profile")).await;
    assert!(profile.playtime.is_none() && profile.recent.is_none() && profile.achievements.is_none() && profile.now_playing.is_none());
    let board: Vec<GamePlayer> = noor.fetch(&format!("/games/{game}/players")).await;
    assert!(board.is_empty());
    let own: Profile = robin.fetch(&format!("/users/{robin_id}/profile")).await;
    assert!(own.playtime.is_some() && own.now_playing.is_some(), "people always see their own data");
    let overview: Overview = admin.fetch("/admin/overview").await;
    assert_eq!(overview.playing_now.iter().map(|playing| (playing.username.as_str(), playing.game_title.as_str())).collect::<Vec<_>>(), [("robin", "Factorio")]);
}
