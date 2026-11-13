//! Milestone 2: scanning, matching, and downloading the way Bonfire will.

mod common;

use std::{collections::BTreeMap, path::Path};

use common::{Api, TestHearth};
use hearth::library::metadata::Endpoints;
use hearth_api::library::{
    CHUNK_SIZE, Diff, Game, GamePatch, GameSummary, LaunchOption, Library, Manifest, ManifestState, Players, ReviewAction, ReviewItem,
    SourceKind,
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn write(path: impl AsRef<Path>, bytes: &[u8]) {
    std::fs::create_dir_all(path.as_ref().parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Bytes that differ from place to place, so a wrong offset shows up as a wrong hash.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ seed).collect()
}

fn hex(bytes: &[u8]) -> String {
    hearth::tls::hex(bytes)
}

/// Downloads a whole version chunk by chunk and checks every chunk and every file against
/// the manifest. Returns the files and how many bytes travelled.
async fn kindle(api: &Api, version_id: i64) -> (BTreeMap<String, Vec<u8>>, u64) {
    let manifest: Manifest = api.fetch(&format!("/versions/{version_id}/manifest")).await;
    assert_eq!(manifest.chunk_size, CHUNK_SIZE);
    let mut files = BTreeMap::new();
    let mut travelled = 0;
    for (index, file) in manifest.files.iter().enumerate() {
        let mut content = Vec::new();
        for (chunk, expected) in file.chunks.as_bytes().chunks(64).enumerate() {
            let first = chunk as u64 * CHUNK_SIZE;
            let last = (first + CHUNK_SIZE).min(file.size) - 1;
            let (status, bytes) = api.bytes(&format!("/versions/{version_id}/files/{index}"), Some((first, last))).await;
            assert_eq!(status, StatusCode::PARTIAL_CONTENT);
            assert_eq!(hex(blake3::hash(&bytes).as_bytes()).as_bytes(), expected, "chunk {chunk} of {}", file.path);
            travelled += bytes.len() as u64;
            content.extend(bytes);
        }
        assert_eq!(content.len() as u64, file.size);
        assert_eq!(hex(blake3::hash(&content).as_bytes()), file.hash, "{}", file.path);
        files.insert(file.path.clone(), content);
    }
    (files, travelled)
}

/// Turns local files of version `from` into version `to` by fetching only what the diff names.
async fn update(api: &Api, local: &mut BTreeMap<String, Vec<u8>>, from: i64, to: i64) -> u64 {
    let diff: Diff = api.fetch(&format!("/versions/{to}/diff?from={from}")).await;
    let mut travelled = 0;
    for path in &diff.delete {
        local.remove(path);
    }
    for file in &diff.fetch {
        let url = format!("/versions/{to}/files/{}", file.index);
        let content = local.entry(file.path.clone()).or_default();
        content.resize(file.size as usize, 0);
        match &file.chunks {
            None => {
                let (status, bytes) = api.bytes(&url, None).await;
                assert_eq!(status, StatusCode::OK);
                travelled += bytes.len() as u64;
                *content = bytes;
            }
            Some(chunks) => {
                for &chunk in chunks {
                    let first = chunk as u64 * CHUNK_SIZE;
                    let last = (first + CHUNK_SIZE).min(file.size) - 1;
                    let (_, bytes) = api.bytes(&url, Some((first, last))).await;
                    travelled += bytes.len() as u64;
                    content[first as usize..=last as usize].copy_from_slice(&bytes);
                }
            }
        }
    }
    assert_eq!(travelled, diff.bytes, "the diff says up front how much will travel");
    // Whatever the diff did not name must already be right: check everything against the manifest.
    let manifest: Manifest = api.fetch(&format!("/versions/{to}/manifest")).await;
    assert_eq!(manifest.files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), local.keys().map(String::as_str).collect::<Vec<_>>());
    for file in &manifest.files {
        assert_eq!(hex(blake3::hash(&local[&file.path]).as_bytes()), file.hash, "{} after the update", file.path);
    }
    travelled
}

#[tokio::test(flavor = "multi_thread")]
async fn a_games_folder_is_scanned_and_downloadable_and_an_update_sends_only_what_changed() {
    let hearth = TestHearth::start().await;
    let big = pattern(2 * CHUNK_SIZE as usize + 1234, 1);
    write(hearth.games.join("Factorio/bin/x64/factorio.exe"), &pattern(1000, 2));
    write(hearth.games.join("Factorio/data/base.pak"), &big);
    write(hearth.games.join("Factorio/old.txt"), b"goes away in the next version");
    write(hearth.games.join("Tyrian 2000 (1999).zip"), &pattern(5000, 3));
    write(hearth.games.join("readme.txt"), b"not a game");

    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    let library = hearth.settled(&admin, 0).await;
    assert_eq!(library.games.iter().map(|game| game.title.as_str()).collect::<Vec<_>>(), ["Factorio", "Tyrian 2000"]);
    assert_eq!(library.review, 0, "without metadata keys every game is listed under its folder name");

    // A player sees the same library through the client API.
    let robin = hearth.join(&done.invite_code, "robin").await;
    let games: Vec<GameSummary> = robin.fetch("/games").await;
    let (factorio, tyrian) = (&games[0], &games[1]);
    assert_eq!((factorio.kind, tyrian.kind, tyrian.year), (SourceKind::Folder, SourceKind::Archive, Some(1999)));
    assert_eq!(factorio.bytes as usize, big.len() + 1000 + 29);
    let detail: Game = robin.fetch(&format!("/games/{}", factorio.id)).await;
    assert_eq!(detail.launch, [LaunchOption { name: "Factorio".into(), path: "bin/x64/factorio.exe".into(), args: String::new(), default: true }]);
    assert_eq!((detail.versions.len(), detail.versions[0].state, detail.versions[0].update_bytes), (1, ManifestState::Ready, None));

    // Kindle it: every chunk is fetched by range and verified.
    let v1 = factorio.version.clone().unwrap().id;
    let (mut local, travelled) = kindle(&robin, v1).await;
    assert_eq!(travelled, factorio.bytes);
    assert_eq!(local["data/base.pak"], big);
    let (zip, _) = kindle(&robin, tyrian.version.clone().unwrap().id).await;
    assert_eq!(zip["Tyrian 2000 (1999).zip"].len(), 5000, "an archive is a version of one file");

    // A download that was cut off picks up in the middle of a file.
    let (status, tail) = robin.bytes(&format!("/versions/{v1}/files/1"), Some((CHUNK_SIZE + 7, u64::MAX / 2))).await;
    assert_eq!((status, tail.as_slice()), (StatusCode::PARTIAL_CONTENT, &big[CHUNK_SIZE as usize + 7..]));
    let (status, _) = robin.bytes(&format!("/versions/{v1}/files/1"), Some((big.len() as u64, big.len() as u64 + 5))).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(robin.fails(Method::GET, &format!("/versions/{v1}/files/99"), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
    assert_eq!(hearth.client().fails(Method::GET, "/games", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");

    // The admin puts a new build next to the old one. One chunk of the big file differs.
    let mut big2 = big.clone();
    big2[CHUNK_SIZE as usize + 5] ^= 0xff;
    write(hearth.games.join("Factorio 2.0.16/bin/x64/factorio.exe"), &pattern(1000, 2));
    write(hearth.games.join("Factorio 2.0.16/data/base.pak"), &big2);
    write(hearth.games.join("Factorio 2.0.16/new.txt"), b"arrives with 2.0.16");
    let library = hearth.rescan(&admin).await;
    assert_eq!((library.games.len(), library.review), (2, 1), "a newer build is never added as a second game");
    let review: Vec<ReviewItem> = admin.fetch("/admin/review").await;
    assert_eq!(review[0].update_of.as_ref().map(|game| game.name.as_str()), Some("Factorio"));
    assert_eq!(review[0].version_guess.as_deref(), Some("2.0.16"));
    assert_eq!(robin.fails(Method::GET, "/admin/review", &Value::Null, StatusCode::FORBIDDEN).await, "forbidden");

    let action = ReviewAction::Update { game_id: factorio.id, version: "2.0.16".into(), note: "Factory Friday stays on 2.0.15 until 6 Nov".into() };
    let (status, _) = admin.post(&format!("/admin/review/{}", review[0].id), &action).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let before: Library = admin.fetch("/admin/library").await;
    hearth.settled(&admin, before.scan.scans_finished - 1).await;

    let detail: Game = robin.fetch(&format!("/games/{}", factorio.id)).await;
    let v2 = detail.summary.version.clone().unwrap();
    assert_eq!((v2.name.as_str(), detail.versions.len()), ("2.0.16", 2));
    assert_eq!(detail.versions[0].note, "Factory Friday stays on 2.0.15 until 6 Nov");
    let expected_update = CHUNK_SIZE + 19;
    assert_eq!(detail.versions[0].update_bytes, Some(expected_update), "one chunk and one small new file");

    // The update: only the changed chunk and the new file travel, and the result is exact.
    let travelled = update(&robin, &mut local, v1, v2.id).await;
    assert_eq!(travelled, expected_update);
    assert!(travelled < detail.summary.bytes / 2);
    assert_eq!(local["data/base.pak"], big2);
    assert!(!local.contains_key("old.txt"));

    // The old version stays downloadable, for people who match a server on it.
    let (old, _) = kindle(&robin, v1).await;
    assert_eq!(old["data/base.pak"], big);
}

#[tokio::test(flavor = "multi_thread")]
async fn files_that_change_on_disk_are_never_served_stale_and_hidden_games_stay_hidden() {
    let hearth = TestHearth::start().await;
    write(hearth.games.join("Celeste/Celeste.exe"), &pattern(3000, 4));
    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    let library = hearth.settled(&admin, 0).await;
    let game = library.games[0].clone();
    let version = game.version.clone().unwrap().id;
    let robin = hearth.join(&done.invite_code, "robin").await;

    // The file is replaced behind the Hearth's back. The old hash no longer describes it.
    write(hearth.games.join("Celeste/Celeste.exe"), &pattern(3001, 5));
    assert_eq!(robin.fails(Method::GET, &format!("/versions/{version}/files/0"), &Value::Null, StatusCode::CONFLICT).await, "conflict");
    for _ in 0..200 {
        let library: Library = admin.fetch("/admin/library").await;
        if library.scan.hashing == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let (files, _) = kindle(&robin, version).await;
    assert_eq!(files["Celeste.exe"], pattern(3001, 5), "after hashing again the new bytes are served, verified");

    // The game editor: details, launch setup, and whether players see the game.
    let patch = GamePatch {
        developer: Some("Maddy Makes Games".into()),
        year: Some(Some(2018)),
        players: Some(Players::Single),
        launch: Some(vec![
            LaunchOption { name: "Celeste".into(), path: "Celeste.exe".into(), args: String::new(), default: false },
            LaunchOption { name: "Windowed".into(), path: "Celeste.exe".into(), args: "--windowed".into(), default: true },
        ]),
        ..Default::default()
    };
    let edited: Game = admin.ok(Method::PATCH, &format!("/admin/games/{}", game.id), &patch).await;
    assert_eq!((edited.summary.developer.as_str(), edited.summary.year), ("Maddy Makes Games", Some(2018)));
    assert_eq!(edited.launch.iter().map(|option| option.default).collect::<Vec<_>>(), [false, true]);
    let escape = json!({ "launch": [{ "name": "x", "path": "../../Windows/System32/cmd.exe" }] });
    assert_eq!(admin.fails(Method::PATCH, &format!("/admin/games/{}", game.id), &escape, StatusCode::BAD_REQUEST).await, "bad_request");
    assert_eq!(robin.fails(Method::PATCH, &format!("/admin/games/{}", game.id), &patch, StatusCode::FORBIDDEN).await, "forbidden");

    let _: Game = admin.ok(Method::PATCH, &format!("/admin/games/{}", game.id), &json!({ "visible": false })).await;
    let games: Vec<GameSummary> = robin.fetch("/games").await;
    assert!(games.is_empty());
    assert_eq!(robin.fails(Method::GET, &format!("/games/{}", game.id), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");
    assert_eq!(robin.fails(Method::GET, &format!("/versions/{version}/manifest"), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");

    // A game that leaves the disk leaves the downloads, and comes back with its folder.
    std::fs::rename(hearth.games.join("Celeste"), hearth.dir.path().join("moved-away")).unwrap();
    hearth.rescan(&admin).await;
    assert_eq!(admin.fails(Method::GET, &format!("/versions/{version}/manifest"), &Value::Null, StatusCode::CONFLICT).await, "conflict");
    std::fs::rename(hearth.dir.path().join("moved-away"), hearth.games.join("Celeste")).unwrap();
    hearth.rescan(&admin).await;
    let manifest: Manifest = admin.fetch(&format!("/versions/{version}/manifest")).await;
    assert_eq!(manifest.files.len(), 1);
}

/// Stands in for Twitch, IGDB and SteamGridDB, with the shapes their real answers have.
async fn mock_providers() -> Endpoints {
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode, header},
        routing::{get, post},
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let authorized = |headers: &HeaderMap, token: &str| headers.get(header::AUTHORIZATION).is_some_and(|value| value == &format!("Bearer {token}"));
    let art = {
        let base = base.clone();
        move |kind: &'static str| {
            let base = base.clone();
            get(move |headers: HeaderMap| async move {
                if !authorized(&headers, "sgdb-key") {
                    return (StatusCode::UNAUTHORIZED, Json(json!({ "success": false })));
                }
                (StatusCode::OK, Json(json!({ "success": true, "data": [{ "url": format!("{base}/img/{kind}.png") }] })))
            })
        }
    };
    let app = Router::new()
        .route(
            "/token",
            post(|body: String| async move {
                if body.contains("client_id=igdb-id") && body.contains("client_secret=igdb-secret") {
                    (StatusCode::OK, Json(json!({ "access_token": "igdb-token", "expires_in": 5000000, "token_type": "bearer" })))
                } else {
                    (StatusCode::BAD_REQUEST, Json(json!({ "status": 400, "message": "invalid client" })))
                }
            }),
        )
        .route(
            "/igdb/games",
            post(move |headers: HeaderMap, query: String| async move {
                if !authorized(&headers, "igdb-token") || headers.get("client-id").is_none_or(|id| id != "igdb-id") {
                    return (StatusCode::UNAUTHORIZED, Json(json!([])));
                }
                assert!(query.starts_with("search \""), "IGDB takes a search in its own query language: {query}");
                let company = |name: &str| json!([{ "developer": false, "company": { "name": "Some Publisher" } }, { "developer": true, "company": { "name": name } }]);
                (
                    StatusCode::OK,
                    Json(json!([
                        { "id": 101, "name": "TUNIC", "first_release_date": 1647388800, "summary": "A small fox on a big adventure.",
                          "genres": [{ "name": "Adventure" }, { "name": "Indie" }], "game_modes": [{ "name": "Single player" }],
                          "involved_companies": company("Andrew Shouldice") },
                        { "id": 102, "name": "Celeste 64: Fragments of the Mountain", "first_release_date": 1706486400,
                          "genres": [{ "name": "Platform" }], "game_modes": [{ "name": "Single player" }],
                          "involved_companies": company("Maddy Makes Games") },
                        { "id": 103, "name": "Celeste", "first_release_date": 1516838400, "game_modes": [{ "name": "Single player" }] },
                        { "id": 104, "name": "Stardew Valley", "first_release_date": 1456444800, "summary": "You inherit a farm.",
                          "genres": [{ "name": "Simulator" }], "game_modes": [{ "name": "Single player" }, { "name": "Co-operative" }],
                          "involved_companies": company("ConcernedApe") },
                    ])),
                )
            }),
        )
        .route(
            "/sgdb/search/autocomplete/{term}",
            get(move |headers: HeaderMap| async move {
                if !authorized(&headers, "sgdb-key") {
                    return (StatusCode::UNAUTHORIZED, Json(json!({ "success": false })));
                }
                (StatusCode::OK, Json(json!({ "success": true, "data": [{ "id": 201, "name": "Tunic", "release_date": 1647388800 }, { "id": 202, "name": "Something Else" }] })))
            }),
        )
        .route("/sgdb/grids/game/{id}", art("cover"))
        .route("/sgdb/heroes/game/{id}", art("hero"))
        .route("/sgdb/logos/game/{id}", art("logo"))
        .route("/img/{name}", get(|| async { ([(header::CONTENT_TYPE, "image/png")], b"\x89PNG stand-in".to_vec()) }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Endpoints { twitch_token: format!("{base}/token"), igdb: format!("{base}/igdb"), sgdb: format!("{base}/sgdb") }
}

#[tokio::test(flavor = "multi_thread")]
async fn games_are_matched_with_metadata_and_the_rest_waits_in_review() {
    let endpoints = mock_providers().await;
    let hearth = TestHearth::start_with(|opts| opts.metadata = endpoints).await;
    write(hearth.games.join("tunic_win_1.0.2.zip"), &pattern(900, 6));
    write(hearth.games.join("Stardew Valley/Stardew Valley.exe"), &pattern(900, 7));
    write(hearth.games.join("Celeste 64 Fragmnts.zip"), &pattern(900, 8));
    write(hearth.games.join("Totally Unknown Thing.iso"), &pattern(900, 9));

    let admin = hearth.browser();
    hearth
        .complete_setup(&admin, |settings| {
            settings.igdb_client_id = "igdb-id".into();
            settings.igdb_client_secret = "igdb-secret".into();
            settings.sgdb_api_key = "sgdb-key".into();
        })
        .await;
    let library = hearth.settled(&admin, 0).await;

    // Sure matches are applied on their own, with details from IGDB.
    assert_eq!(library.games.iter().map(|game| game.title.as_str()).collect::<Vec<_>>(), ["Stardew Valley", "TUNIC"]);
    let (stardew, tunic) = (&library.games[0], &library.games[1]);
    assert_eq!((tunic.developer.as_str(), tunic.year, tunic.genre.as_str()), ("Andrew Shouldice", Some(2022), "Adventure, Indie"));
    assert_eq!(tunic.version.as_ref().unwrap().name, "1.0.2", "the version comes from the file name");
    assert_eq!((stardew.players, stardew.networking), (Players::Coop, true), "games played together get Bonfire networking by default");
    assert_eq!((tunic.players, tunic.networking), (Players::Single, false));
    let detail: Game = admin.fetch(&format!("/games/{}", tunic.id)).await;
    assert_eq!(detail.description, "A small fox on a big adventure.");

    // Art arrives from SteamGridDB shortly after, and is served by the Hearth itself.
    let mut cover = None;
    for _ in 0..200 {
        let game: Game = admin.fetch(&format!("/games/{}", tunic.id)).await;
        if game.summary.cover.is_some() && game.summary.hero.is_some() && game.summary.logo.is_some() {
            cover = game.summary.cover;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let cover = cover.expect("cover, hero and logo should have been downloaded");
    let (status, image) = admin.bytes(cover.strip_prefix("/api/v1").unwrap(), None).await;
    assert_eq!((status, image.as_slice()), (StatusCode::OK, &b"\x89PNG stand-in"[..]));
    assert_eq!(hearth.client().fails(Method::GET, cover.strip_prefix("/api/v1").unwrap(), &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");
    assert_eq!(admin.fails(Method::GET, "/art/..%2Fhearth.db", &Value::Null, StatusCode::NOT_FOUND).await, "not_found");

    // Everything below the threshold waits for an admin, best guess first.
    assert_eq!(library.review, 2);
    let review: Vec<ReviewItem> = admin.fetch("/admin/review").await;
    let (celeste, unknown) = (&review[0], &review[1]);
    assert_eq!(celeste.candidates[0].title, "Celeste 64: Fragments of the Mountain");
    assert!(celeste.candidates[0].confidence < 0.85 && celeste.candidates[0].confidence > celeste.candidates[1].confidence);
    assert_eq!(admin.fails(Method::POST, &format!("/admin/review/{}", celeste.id), &ReviewAction::Accept { candidate: 9 }, StatusCode::BAD_REQUEST).await, "bad_request");
    let (status, _) = admin.post(&format!("/admin/review/{}", celeste.id), &ReviewAction::Accept { candidate: 0 }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = admin.post(&format!("/admin/review/{}", unknown.id), &ReviewAction::Ignore).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let library = hearth.rescan(&admin).await;
    assert_eq!(library.review, 0, "an ignored file does not come back with the next scan");
    let accepted = library.games.iter().find(|game| game.title.starts_with("Celeste 64")).expect("the accepted match is in the library");
    assert_eq!((accepted.developer.as_str(), accepted.year), ("Maddy Makes Games", Some(2024)));

    // The Test buttons in Settings say in words whether a key works.
    let (status, _) = admin.post("/admin/metadata/test", &json!({ "provider": "igdb" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = admin.post("/admin/metadata/test", &json!({ "provider": "sgdb" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let _: Value = admin.ok(Method::PATCH, "/admin/settings", &json!({ "igdb_client_secret": "wrong", "sgdb_api_key": "wrong" })).await;
    assert_eq!(admin.fails(Method::POST, "/admin/metadata/test", &json!({ "provider": "igdb" }), StatusCode::BAD_REQUEST).await, "bad_request");
    assert_eq!(admin.fails(Method::POST, "/admin/metadata/test", &json!({ "provider": "sgdb" }), StatusCode::BAD_REQUEST).await, "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn uploads_land_in_a_games_folder_and_new_files_are_noticed_without_a_rescan() {
    let hearth = TestHearth::start().await;
    let admin = hearth.browser();
    let done = hearth.complete_setup(&admin, |_| {}).await;
    hearth.settled(&admin, 0).await;
    let robin = hearth.join(&done.invite_code, "robin").await;

    let put = |api: &Api, name: &str, body: Vec<u8>| {
        let request = api.http.put(format!("{}/api/v1/admin/library/upload", api.base)).query(&[("name", name)]).header(hearth_api::WEB_UI_HEADER, "1").body(body);
        let request = match &api.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        async move { request.send().await.unwrap().status() }
    };
    let game = pattern(70_000, 10);
    assert_eq!(put(&admin, "Hollow Knight.zip", game.clone()).await, StatusCode::CREATED);
    assert_eq!(std::fs::read(hearth.games.join("Hollow Knight.zip")).unwrap(), game);
    assert_eq!(put(&admin, "Hollow Knight.zip", game.clone()).await, StatusCode::CONFLICT, "an upload never overwrites");
    for bad in ["../outside.zip", "sub/inner.zip", "notes.txt", ".hidden.zip", "C:evil.zip"] {
        assert_eq!(put(&admin, bad, game.clone()).await, StatusCode::BAD_REQUEST, "{bad}");
    }
    assert_eq!(put(&admin, "empty.zip", Vec::new()).await, StatusCode::BAD_REQUEST);
    assert_eq!(put(&robin, "Player Upload.zip", game.clone()).await, StatusCode::FORBIDDEN);
    assert!(!hearth.games.join(".empty.zip.part").exists(), "a failed upload leaves nothing behind");

    // The upload asked for a scan itself. Then a file is copied in by hand, and the watcher sees it.
    let library = hearth.settled(&admin, 1).await;
    assert_eq!(library.games.iter().map(|game| game.title.as_str()).collect::<Vec<_>>(), ["Hollow Knight"]);
    write(hearth.games.join("Hades.zip"), &pattern(800, 11));
    let library = hearth.settled(&admin, library.scan.scans_finished).await;
    assert_eq!(library.games.iter().map(|game| game.title.as_str()).collect::<Vec<_>>(), ["Hades", "Hollow Knight"]);
}
