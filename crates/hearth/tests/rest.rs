//! Milestone 6: cloud saves over WebDAV, the way rclone talks to them, and shared worlds.

mod common;

use common::{Api, TestHearth, sign_in};
use hearth_api::{
    admin::Networking,
    auth::Me,
    people::AdminUser,
    saves::{AddWorldMember, CommitSave, CreateWorld, GameSaves, JournalEntry, JournalNote, ReleaseWorld, RestoreSave, Revision, Saves, Storage, World},
};
use reqwest::{Method, StatusCode, header::HeaderMap};
use serde_json::{Value, json};

struct Dav<'a>(&'a Api);

impl Dav<'_> {
    /// One WebDAV request, with the session token as a bearer token like rclone sends it.
    async fn send(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut request = self.0.http.request(Method::from_bytes(method.as_bytes()).unwrap(), format!("{}/dav{path}", self.0.base)).body(body.to_vec());
        if let Some(token) = &self.0.token {
            request = request.bearer_auth(token);
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        (response.status(), response.headers().clone(), response.bytes().await.unwrap().to_vec())
    }

    async fn put(&self, path: &str, body: &[u8]) -> StatusCode {
        self.send("PUT", path, &[("x-oc-mtime", "1700000000")], body).await.0
    }

    async fn get(&self, path: &str) -> (StatusCode, Vec<u8>) {
        let (status, _, body) = self.send("GET", path, &[], b"").await;
        (status, body)
    }

    /// The names a folder lists, as rclone reads them from a PROPFIND answer.
    async fn list(&self, path: &str) -> (StatusCode, Vec<String>) {
        let (status, _, body) = self.send("PROPFIND", path, &[("depth", "1")], b"").await;
        let xml = String::from_utf8(body).unwrap();
        let mut names: Vec<String> = xml.split("<D:displayname>").skip(2).map(|rest| rest.split("</D:displayname>").next().unwrap().to_string()).collect();
        names.sort();
        (status, names)
    }
}

async fn world() -> (TestHearth, Api, i64, String) {
    // A thousand bytes of saves per person, so the quota can be reached with small files.
    let hearth = TestHearth::start_with(|opts| opts.save_quota = Some(1000)).await;
    std::fs::create_dir_all(hearth.games.join("Stardew Valley")).unwrap();
    std::fs::write(hearth.games.join("Stardew Valley/Stardew Valley.exe"), b"game").unwrap();
    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    let game = hearth.settled(&admin, 0).await.games[0].id;
    (hearth, admin, game, done.invite_code)
}

async fn commit(api: &Api, game: i64, base: Option<u32>, force: bool) -> Revision {
    api.ok(Method::POST, &format!("/saves/{game}/commit"), &CommitSave { base, force }).await
}

#[tokio::test(flavor = "multi_thread")]
async fn saves_rest_on_the_hearth_and_two_pcs_notice_a_conflict() {
    let (hearth, admin, game, invite) = world().await;
    let desktop = hearth.join(&invite, "robin").await;
    let laptop = sign_in(&hearth, "robin", "Laptop").await;
    let noor = hearth.join(&invite, "noor").await;
    let (dav, dav_laptop, dav_noor) = (Dav(&desktop), Dav(&laptop), Dav(&noor));
    let up = format!("/saves/{game}/upload");

    // What rclone does first: look around.
    assert_eq!(dav.list("/").await, (StatusCode::MULTI_STATUS, vec!["saves".to_string(), "worlds".to_string()]));
    assert_eq!(dav.list(&format!("/saves/{game}")).await.1, ["rev", "upload"]);
    assert_eq!(dav.list(&up).await, (StatusCode::MULTI_STATUS, vec![]), "an upload folder nothing was put into is empty, not missing");
    let (status, headers, _) = Dav(&hearth.client()).send("PROPFIND", "/", &[], b"").await;
    assert_eq!((status, headers["www-authenticate"].to_str().unwrap()), (StatusCode::UNAUTHORIZED, "Basic realm=\"Hearth\""));

    // Sync the saves up: folders, then files with their own modification times.
    assert_eq!(dav.send("MKCOL", &format!("{up}/slot 1"), &[], b"").await.0, StatusCode::CREATED);
    assert_eq!(dav.send("MKCOL", &format!("{up}/slot 1"), &[], b"").await.0, StatusCode::METHOD_NOT_ALLOWED, "which rclone reads as: it is there");
    assert_eq!(dav.put(&format!("{up}/slot%201/world.sav"), b"day 12").await, StatusCode::CREATED);
    assert_eq!(dav.put(&format!("{up}/options.ini"), b"volume=7").await, StatusCode::CREATED);
    let (_, _, listing) = dav.send("PROPFIND", &format!("{up}/slot%201/"), &[("depth", "1")], b"").await;
    let listing = String::from_utf8(listing).unwrap();
    assert!(listing.contains("<D:href>/dav/saves/1/upload/slot%201/world.sav</D:href>"), "{listing}");
    assert!(listing.contains("<D:getcontentlength>6</D:getcontentlength>") && listing.contains("Tue, 14 Nov 2023 22:13:20 GMT"), "size and the file's own time: {listing}");

    // Nothing leaves the caller's own area.
    for escape in [format!("{up}/..%2F..%2F..%2Fhearth.db"), format!("{up}/%2e%2e/x"), format!("{up}/a%5Cb"), "/saves/999/upload/x".to_string()] {
        let status = dav.put(&escape, b"x").await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND, "{escape} gave {status}");
    }

    // Rest: the upload becomes revision 1.
    let first = commit(&desktop, game, None, false).await;
    assert_eq!((first.number, first.files, first.bytes, first.device_name.as_str()), (1, 2, 14, "Desktop"));
    assert_eq!(dav.list(&format!("/saves/{game}")).await.1, ["current", "rev", "upload"]);
    assert_eq!(dav.get(&format!("/saves/{game}/current/slot%201/world.sav")).await, (StatusCode::OK, b"day 12".to_vec()));
    let (status, _, part) = dav.send("GET", &format!("/saves/{game}/rev/1/options.ini"), &[("range", "bytes=7-")], b"").await;
    assert_eq!((status, part.as_slice()), (StatusCode::PARTIAL_CONTENT, &b"7"[..]));
    assert_eq!(dav.put(&format!("/saves/{game}/current/slot%201/world.sav"), b"cheat").await, StatusCode::FORBIDDEN, "kept revisions are read-only");
    assert_eq!(dav.send("DELETE", &format!("/saves/{game}/rev/1/options.ini"), &[], b"").await.0, StatusCode::FORBIDDEN);
    assert_eq!(dav_noor.get(&format!("/saves/{game}/current/options.ini")).await.0, StatusCode::NOT_FOUND, "saves belong to one account");
    assert_eq!(dav.get(&format!("/saves/{game}/rev/2/options.ini")).await.0, StatusCode::NOT_FOUND);

    // The laptop sees there are saves it does not have, plays, and rests on top of them.
    let state: GameSaves = laptop.fetch(&format!("/saves/{game}")).await;
    assert_eq!(state.head.as_ref().map(|head| (head.number, head.hash.clone())), Some((1, first.hash.clone())));
    assert_eq!(dav_laptop.list(&format!("{up}/slot%201")).await.1, ["world.sav"], "the upload area starts from the last revision, so only changes travel");
    assert_eq!(dav_laptop.put(&format!("{up}/slot%201/world.sav"), b"day 13, laptop").await, StatusCode::NO_CONTENT);
    let second = commit(&laptop, game, Some(1), false).await;
    assert_eq!((second.number, second.device_name.as_str()), (2, "Laptop"));
    assert_eq!(dav.get(&format!("/saves/{game}/rev/1/slot%201/world.sav")).await.1, b"day 12", "the older revision is untouched");

    // The desktop also played, without having seen revision 2. That is a conflict, for the person to decide.
    assert_eq!(dav.put(&format!("{up}/slot%201/world.sav"), b"day 13, desktop").await, StatusCode::NO_CONTENT);
    assert_eq!(desktop.fails(Method::POST, &format!("/saves/{game}/commit"), &CommitSave { base: Some(1), force: false }, StatusCode::CONFLICT).await, "save_conflict");
    let third = commit(&desktop, game, Some(1), true).await;
    assert_eq!(third.number, 3, "keeping this PC's saves is a new revision. The laptop's stay in the history");
    assert_eq!(commit(&desktop, game, Some(3), false).await.number, 3, "resting the same saves again changes nothing");

    // An older revision can be made the newest again.
    let restored: Revision = desktop.ok(Method::POST, &format!("/saves/{game}/restore"), &RestoreSave { revision: 1 }).await;
    assert_eq!((restored.number, restored.hash == first.hash), (4, true));
    assert_eq!(dav.get(&format!("/saves/{game}/current/slot%201/world.sav")).await.1, b"day 12");
    assert_eq!(desktop.fails(Method::POST, &format!("/saves/{game}/restore"), &RestoreSave { revision: 99 }, StatusCode::NOT_FOUND).await, "not_found");

    // Moving and deleting in the upload area, as a sync does.
    let (status, _, _) = dav.send("MOVE", &format!("{up}/options.ini"), &[("destination", &format!("{}/dav{up}/settings.ini", desktop.base)), ("overwrite", "T")], b"").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(dav.send("DELETE", &format!("{up}/slot%201"), &[], b"").await.0, StatusCode::NO_CONTENT);
    assert_eq!(dav.list(&up).await.1, ["settings.ini"]);
    let outside = format!("{}/dav/saves/{game}/rev/1/stolen.ini", desktop.base);
    assert_eq!(dav.send("MOVE", &format!("{up}/settings.ini"), &[("destination", &outside)], b"").await.0, StatusCode::FORBIDDEN);

    // Space is limited per person. Too much is refused while it arrives and when it is rested.
    assert_eq!(dav.put(&format!("{up}/huge.sav"), &vec![0u8; 2000]).await, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(dav.list(&up).await.1, ["settings.ini"], "a refused upload leaves nothing behind");
    for round in 0..6u8 {
        assert_eq!(dav.put(&format!("{up}/big.sav"), &vec![round; 300]).await, if round == 0 { StatusCode::CREATED } else { StatusCode::NO_CONTENT });
        let head: GameSaves = desktop.fetch(&format!("/saves/{game}")).await;
        commit(&desktop, game, head.head.map(|head| head.number), false).await;
    }
    let saves: Saves = desktop.fetch("/saves").await;
    assert_eq!(saves.quota_bytes, 1000);
    assert!(saves.used_bytes <= 1000, "old revisions make room: {} bytes used", saves.used_bytes);
    assert!(saves.games[0].revisions.len() <= 5 && saves.games[0].head.as_ref().unwrap().number == 10);

    // The admin sees who uses how much.
    let storage: Storage = admin.fetch("/admin/storage").await;
    let robin = storage.accounts.iter().find(|account| account.username == "robin").unwrap();
    assert_eq!((robin.used_bytes, robin.games), (saves.used_bytes, 1));
    let users: Vec<AdminUser> = admin.fetch("/admin/users").await;
    assert_eq!(users.iter().find(|user| user.username == "robin").unwrap().save_bytes, saves.used_bytes);

    // Other WebDAV clients only know passwords: the token works as one.
    let basic = hearth.client().http.request(Method::from_bytes(b"PROPFIND").unwrap(), format!("{}/dav/saves/", desktop.base)).basic_auth("robin", desktop.token.as_deref()).send().await.unwrap();
    assert_eq!(basic.status(), StatusCode::MULTI_STATUS);

    let (status, _) = desktop.call(Method::DELETE, &format!("/saves/{game}"), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let saves: Saves = desktop.fetch("/saves").await;
    assert_eq!((saves.used_bytes, saves.games.len()), (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shared_world_lives_on_the_hearth_and_one_person_hosts_it_at_a_time() {
    let (hearth, admin, game, invite) = world().await;
    let (robin, noor, lea) = (hearth.join(&invite, "robin").await, hearth.join(&invite, "noor").await, hearth.join(&invite, "lea").await);
    let noor_id = noor.fetch::<Me>("/me").await.user.id;

    let world: World = robin.ok(Method::POST, "/worlds", &CreateWorld { game_id: game, name: "Noor's farm".into(), max_players: 4 }).await;
    let path = format!("/worlds/{}", world.id);
    let world: World = robin.ok(Method::POST, &format!("{path}/members"), &AddWorldMember { user_id: noor_id }).await;
    assert_eq!(world.members.iter().map(|member| member.username.as_str()).collect::<Vec<_>>(), ["noor", "robin"]);
    assert_eq!(lea.fails(Method::GET, &path, &Value::Null, StatusCode::NOT_FOUND).await, "not_found", "a world is for its members");
    assert_eq!(Dav(&lea).list(&path).await.0, StatusCode::NOT_FOUND);
    let mine: Vec<World> = noor.fetch(&format!("/worlds?game_id={game}")).await;
    assert_eq!(mine.len(), 1);

    // Robin starts it and becomes the host. While he hosts, nobody else can.
    let hosted: World = robin.ok(Method::POST, &format!("{path}/host"), &Value::Null).await;
    assert_eq!(hosted.host.as_ref().map(|host| host.username.as_str()), Some("robin"));
    assert_eq!(noor.fails(Method::POST, &format!("{path}/host"), &Value::Null, StatusCode::CONFLICT).await, "world_locked");
    assert_eq!(noor.fails(Method::POST, &format!("{path}/heartbeat"), &Value::Null, StatusCode::CONFLICT).await, "conflict");
    assert_eq!(Dav(&robin).put(&format!("{path}/upload/farm.sav"), b"spring, day 1").await, StatusCode::CREATED);
    assert_eq!(Dav(&noor).put(&format!("{path}/upload/farm.sav"), b"mine").await, StatusCode::FORBIDDEN);
    assert_eq!(Dav(&noor).list(&format!("{path}/current")).await, (StatusCode::MULTI_STATUS, vec![]), "nothing stored yet");
    let (status, _) = robin.post(&format!("{path}/heartbeat"), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // He stops: the world is stored back, with a note for the next host.
    let note = "Planted the whole south field with pumpkins. The left side still has no sprinklers.";
    let stored: World = robin.ok(Method::POST, &format!("{path}/release"), &ReleaseWorld { store: true, note: note.into() }).await;
    assert_eq!((stored.revision, stored.bytes, stored.host.is_none()), (1, 13, true));
    assert_eq!(robin.fails(Method::POST, &format!("{path}/release"), &ReleaseWorld::default(), StatusCode::CONFLICT).await, "conflict");

    // Noor hosts next, starting from what Robin stored.
    let _: World = noor.ok(Method::POST, &format!("{path}/host"), &Value::Null).await;
    assert_eq!(Dav(&noor).get(&format!("{path}/upload/farm.sav")).await, (StatusCode::OK, b"spring, day 1".to_vec()));
    assert_eq!(Dav(&noor).put(&format!("{path}/upload/farm.sav"), b"spring, day 2, with sprinklers").await, StatusCode::NO_CONTENT);
    let stored: World = noor.ok(Method::POST, &format!("{path}/release"), &ReleaseWorld { store: true, note: String::new() }).await;
    assert_eq!(stored.revision, 2);
    assert_eq!(Dav(&robin).get(&format!("{path}/current/farm.sav")).await.1, b"spring, day 2, with sprinklers");

    // The journal: who hosted, and what they left for the next one.
    let (status, _) = robin.post(&format!("{path}/journal"), &JournalNote { note: "Fixed the bridge to the quarry.".into() }).await;
    assert_eq!(status, StatusCode::CREATED);
    let journal: Vec<JournalEntry> = noor.fetch(&format!("{path}/journal")).await;
    assert_eq!(journal.len(), 3);
    assert!(journal.iter().any(|entry| entry.username == "robin" && entry.note == note));
    assert!(journal.iter().any(|entry| entry.username == "noor" && entry.note.is_empty()));

    // A host whose PC went dark does not hold the world forever.
    let _: World = robin.ok(Method::POST, &format!("{path}/host"), &Value::Null).await;
    sqlx::query("UPDATE worlds SET host_beat_at = host_beat_at - 301 WHERE id = ?").bind(world.id).execute(&hearth.running().state.db).await.unwrap();
    let taken: World = noor.ok(Method::POST, &format!("{path}/host"), &Value::Null).await;
    assert_eq!(taken.host.as_ref().map(|host| host.username.as_str()), Some("noor"));
    assert_eq!(Dav(&robin).put(&format!("{path}/upload/farm.sav"), b"too late").await, StatusCode::FORBIDDEN);
    // Giving it back unchanged keeps what was stored.
    let back: World = noor.ok(Method::POST, &format!("{path}/release"), &ReleaseWorld { store: false, note: "Only looked around.".into() }).await;
    assert_eq!(back.revision, 2);

    // The admin's networking page lists the worlds.
    let networking: Networking = admin.fetch("/admin/networking").await;
    assert_eq!(networking.worlds.iter().map(|world| (world.name.as_str(), world.members.len())).collect::<Vec<_>>(), [("Noor's farm", 2)]);

    // Leaving, and deleting.
    assert_eq!(noor.fails(Method::DELETE, &path, &Value::Null, StatusCode::FORBIDDEN).await, "forbidden");
    let (status, _) = noor.call(Method::DELETE, &format!("{path}/members/{noor_id}"), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(noor.fails(Method::GET, &path, &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
    let (status, _) = robin.call(Method::DELETE, &path, &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!hearth.running().state.paths.dir("worlds").join(world.id.to_string()).exists());
    let _ = json!({});
}
