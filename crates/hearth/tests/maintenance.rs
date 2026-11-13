//! Milestone 8: database backups and the update check.

mod common;

use common::TestHearth;
use hearth::update::{self, Release};
use hearth_api::admin::{Maintenance, Overview};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[tokio::test(flavor = "multi_thread")]
async fn the_database_is_backed_up_on_request_and_once_a_night() {
    let (hearth, admin) = TestHearth::ready().await;
    let state = &hearth.running().state;
    let folder = state.paths.dir("backups");

    let before: Maintenance = admin.fetch("/admin/maintenance").await;
    assert!(before.backup.is_none() && before.backup_folder.ends_with("backups"));

    // "Back up now" writes a complete copy that opens as a database of its own.
    let after: Maintenance = admin.ok(Method::POST, "/admin/backup", &Value::Null).await;
    let backup = after.backup.expect("a backup was just made");
    assert!(backup.bytes > 0 && backup.file.starts_with("hearth-") && backup.file.ends_with(".db"));
    let copy = hearth::db::open(&folder.join(&backup.file)).await.unwrap();
    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'").fetch_one(&copy).await.unwrap();
    assert_eq!(admins, 1, "the copy holds the accounts");
    copy.close().await;

    // The nightly backup runs once its time has come, and only once a day.
    let count = || std::fs::read_dir(&folder).unwrap().flatten().filter(|entry| entry.file_name().to_string_lossy().ends_with(".db")).count();
    let _: Value = admin.ok(Method::PATCH, "/admin/settings", &json!({ "backup_time": "00:00" })).await;
    hearth::backup::nightly(state).await.unwrap();
    assert_eq!(count(), 1, "today's backup exists already");
    hearth::db::meta_set(&state.db, "last_backup_at", &(hearth::db::now() - 86400).to_string()).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    hearth::backup::nightly(state).await.unwrap();
    assert_eq!(count(), 2, "yesterday's backup does not count for today");
    hearth::backup::nightly(state).await.unwrap();
    assert_eq!(count(), 2);

    // Switched off, nothing runs on its own.
    let _: Value = admin.ok(Method::PATCH, "/admin/settings", &json!({ "backup_nightly": false })).await;
    hearth::db::meta_set(&state.db, "last_backup_at", "0").await.unwrap();
    hearth::backup::nightly(state).await.unwrap();
    assert_eq!(count(), 2);

    let overview: Overview = admin.fetch("/admin/overview").await;
    assert!(overview.services.iter().any(|service| service.name == "Database backup"));
    assert_eq!(hearth.client().fails(Method::POST, "/admin/backup", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");
}

/// Stands in for the release server: one release with a program for this platform and its checksums.
async fn release_server(version: &'static str, honest: bool) -> String {
    use axum::{Json, Router, routing::get};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let program = b"the new hearth program".to_vec();
    let sum = hearth::tls::hex(&Sha256::digest(if honest { &program[..] } else { b"something else" }));
    let sums = format!("0000  hearth-some-other-platform\n{sum}  {}\n", update::asset_name());
    let announce = {
        let base = base.clone();
        move || async move {
            Json(json!({
                "tag_name": format!("v{version}"),
                "assets": [
                    { "name": update::asset_name(), "browser_download_url": format!("{base}/download/program") },
                    { "name": "SHA256SUMS", "browser_download_url": format!("{base}/download/sums") },
                ],
            }))
        }
    };
    let app = Router::new()
        .route(&format!("/repos/{}/releases/latest", update::REPOSITORY), get(announce))
        .route("/download/program", get(move || async move { program }))
        .route("/download/sums", get(move || async move { sums }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_is_found_downloaded_and_checked_before_anything_changes() {
    let api = release_server("9.9.9", true).await;
    let hearth = TestHearth::start_with(|opts| opts.update_api = api.clone()).await;
    let admin = hearth.browser();
    hearth.complete_setup(&admin, |_| {}).await;

    // The Hearth only looks when an admin asks.
    let before: Maintenance = admin.fetch("/admin/maintenance").await;
    assert!(before.update.is_none());
    let checked: Maintenance = admin.ok(Method::POST, "/admin/update/check", &Value::Null).await;
    let found = checked.update.expect("the check found a release");
    assert_eq!((found.latest.as_str(), found.available), ("9.9.9", true));

    // The download is verified against the release's checksums before it is used.
    let release = update::latest(&api).await.unwrap();
    let file = update::download(&api, &release, hearth.dir.path()).await.unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"the new hearth program");

    let tampered = release_server("9.9.9", false).await;
    let release = update::latest(&tampered).await.unwrap();
    let refused = update::download(&tampered, &release, hearth.dir.path()).await.unwrap_err();
    assert!(refused.contains("checksum"), "{refused}");

    // A release that points somewhere else is not followed.
    let elsewhere = Release { version: "9.9.9".into(), assets: vec![(update::asset_name(), "http://203.0.113.5/hearth".into()), ("SHA256SUMS".into(), "http://203.0.113.5/sums".into())] };
    assert!(update::download(&api, &elsewhere, hearth.dir.path()).await.unwrap_err().contains("not where Hearth is published"));
    let incomplete = Release { version: "9.9.9".into(), assets: vec![] };
    assert!(update::download(&api, &incomplete, hearth.dir.path()).await.unwrap_err().contains("no build for this platform"));

    // Nothing newer: nothing to update to.
    let old = release_server("0.0.1", true).await;
    let hearth = TestHearth::start_with(|opts| opts.update_api = old).await;
    let admin = hearth.browser();
    hearth.complete_setup(&admin, |_| {}).await;
    let checked: Maintenance = admin.ok(Method::POST, "/admin/update/check", &Value::Null).await;
    assert!(!checked.update.unwrap().available);
    assert_eq!(admin.fails(Method::POST, "/admin/update", &Value::Null, StatusCode::CONFLICT).await, "conflict");
}
