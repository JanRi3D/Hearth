//! Milestone 1: first-time setup, sign-in, sessions and settings, over the real HTTP API.

mod common;

use common::{ADMIN, PASSWORD, TestHearth, device, login_request};
use hearth_api::{
    HearthInfo,
    auth::{LoginResponse, Me, RegisterRequest, Role, Session, TotpCode, TotpSetup},
    settings::{Browse, FolderScan, FolderScanRequest, SettingsPatch, SettingsView, TlsMode},
    setup::{Handoff, SetupDefaults, SetupRequest},
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread")]
async fn setup_needs_the_code_and_closes_for_good() {
    let hearth = TestHearth::start().await;
    let code = hearth.running().setup_code.clone().expect("a fresh Hearth prints a setup code");
    let anyone = hearth.client();

    let info: HearthInfo = anyone.fetch("/hearth").await;
    assert!(!info.setup_complete);
    assert_eq!(anyone.fails(Method::POST, "/auth/login", &login_request(ADMIN, PASSWORD), StatusCode::SERVICE_UNAVAILABLE).await, "setup_required");

    // Without the code, or with a wrong one, nothing about the machine is revealed.
    assert_eq!(anyone.fails(Method::POST, "/setup/begin", &Value::Null, StatusCode::FORBIDDEN).await, "setup_code_invalid");
    let mut guesser = hearth.client();
    guesser.setup_code = Some("AAAA-AAAA".into());
    assert_eq!(guesser.fails(Method::GET, "/setup/browse", &Value::Null, StatusCode::FORBIDDEN).await, "setup_code_invalid");

    // The code works however it is typed.
    let mut owner = hearth.browser();
    owner.setup_code = Some(code.to_lowercase().replace('-', " "));
    let defaults: SetupDefaults = owner.ok(Method::POST, "/setup/begin", &Value::Null).await;
    assert_eq!(defaults.settings.port, 7300);
    assert!(defaults.data_folder.ends_with("data"));

    // The folder browser shows folders, never files.
    std::fs::create_dir_all(hearth.games.join("Factorio")).unwrap();
    std::fs::write(hearth.games.join("Tyrian 2000.zip"), b"zip").unwrap();
    std::fs::write(hearth.games.join("readme.txt"), b"text").unwrap();
    let listing: Browse = owner.fetch(&format!("/setup/browse?path={}", hearth.games.display())).await;
    assert_eq!(listing.dirs.iter().map(|dir| dir.name.as_str()).collect::<Vec<_>>(), ["Factorio"]);
    let scan: FolderScan = owner.ok(Method::POST, "/setup/scan", &FolderScanRequest { path: hearth.games.display().to_string() }).await;
    assert!(scan.readable);
    assert_eq!(scan.games, 2);

    // Bad input is refused with a reason before anything is written.
    let mut settings = defaults.settings.clone();
    settings.address = "127.0.0.1".into();
    settings.tls = TlsMode::Proxy;
    settings.games_folders = vec![hearth.games.display().to_string()];
    let request = |password: &str, settings: &hearth_api::settings::Settings| SetupRequest {
        username: ADMIN.into(),
        password: password.into(),
        settings: settings.clone(),
    };
    assert_eq!(owner.fails(Method::POST, "/setup/complete", &request("short", &settings), StatusCode::BAD_REQUEST).await, "weak_password");
    let mut no_folders = settings.clone();
    no_folders.games_folders.clear();
    assert_eq!(owner.fails(Method::POST, "/setup/complete", &request(PASSWORD, &no_folders), StatusCode::BAD_REQUEST).await, "bad_request");

    let done = hearth.complete_setup(&hearth.browser(), |_| {}).await;
    assert!(done.invite_code.starts_with("BNFR-") && done.invite_code.len() == 14);
    assert_eq!(done.invite_max_uses, 5);
    assert!(!done.moved, "same port and no TLS: the admin stays signed in on this page");

    // Setup is closed now, even with the right code.
    assert_eq!(owner.fails(Method::POST, "/setup/begin", &Value::Null, StatusCode::GONE).await, "setup_complete");
    assert_eq!(owner.fails(Method::POST, "/setup/complete", &request(PASSWORD, &settings), StatusCode::GONE).await, "setup_complete");
    let info: HearthInfo = anyone.fetch("/hearth").await;
    assert!(info.setup_complete);
    assert_eq!(info.name, "home");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_change_in_the_ui_and_survive_a_restart() {
    let (mut hearth, admin) = TestHearth::ready().await;

    let view: SettingsView = admin.fetch("/admin/settings").await;
    assert_eq!(view.settings.save_quota_gb, 5);

    let patch = SettingsPatch {
        name: Some("lodge".into()),
        save_quota_gb: Some(10),
        session_days: Some(0),
        sgdb_api_key: Some("sgdb-secret".into()),
        ..Default::default()
    };
    let view: SettingsView = admin.ok(Method::PATCH, "/admin/settings", &patch).await;
    assert_eq!((view.settings.name.as_str(), view.settings.save_quota_gb, view.settings.session_days), ("lodge", 10, 0));
    assert!(view.settings.sgdb_api_key.is_empty() && view.settings.sgdb_api_key_set, "a secret is stored but never sent back");

    assert_eq!(admin.fails(Method::PATCH, "/admin/settings", &json!({ "save_quota_gb": 3 }), StatusCode::BAD_REQUEST).await, "bad_request");
    assert_eq!(admin.fails(Method::PATCH, "/admin/settings", &json!({ "games_folders": [] }), StatusCode::BAD_REQUEST).await, "bad_request");
    assert_eq!(admin.fails(Method::PATCH, "/admin/settings", &json!({ "admin_totp_required": true }), StatusCode::BAD_REQUEST).await, "bad_request");

    // Another site can make a browser send the cookie, but not the UI's header.
    let mut forged = admin.clone();
    forged.ui = false;
    assert_eq!(forged.fails(Method::PATCH, "/admin/settings", &json!({ "name": "pwned" }), StatusCode::FORBIDDEN).await, "forbidden");
    assert_eq!(hearth.client().fails(Method::GET, "/admin/settings", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");

    hearth.restart().await;
    assert!(hearth.running().setup_code.is_none(), "a Hearth that has an owner prints no setup code");
    let mut admin = hearth.client();
    let login: LoginResponse = admin.ok(Method::POST, "/auth/login", &login_request(ADMIN, PASSWORD)).await;
    assert_eq!(login.expires_at, None, "sessions never end when the setting says so");
    admin.token = login.token;
    let view: SettingsView = admin.fetch("/admin/settings").await;
    assert_eq!((view.settings.name.as_str(), view.settings.save_quota_gb), ("lodge", 10));
    assert!(view.settings.sgdb_api_key_set);
}

#[tokio::test(flavor = "multi_thread")]
async fn invites_create_accounts_until_they_run_out() {
    let hearth = TestHearth::start().await;
    let done = hearth.complete_setup(&hearth.browser(), |_| {}).await;

    let robin = hearth.join(&done.invite_code.to_lowercase(), "robin").await;
    let me: Me = robin.fetch("/me").await;
    assert_eq!((me.user.username.as_str(), me.user.role), ("robin", Role::Player));
    assert_eq!(robin.fails(Method::GET, "/admin/settings", &Value::Null, StatusCode::FORBIDDEN).await, "forbidden");

    let anyone = hearth.client();
    let register = |invite: Option<&str>, username: &str| RegisterRequest {
        invite_code: invite.map(Into::into),
        username: username.into(),
        password: PASSWORD.into(),
        device: device("Laptop"),
    };
    assert_eq!(anyone.fails(Method::POST, "/auth/register", &register(Some(&done.invite_code), "Robin"), StatusCode::CONFLICT).await, "username_taken");
    assert_eq!(anyone.fails(Method::POST, "/auth/register", &register(Some("BNFR-AAAA-AAAA"), "noor"), StatusCode::FORBIDDEN).await, "invite_invalid");
    assert_eq!(anyone.fails(Method::POST, "/auth/register", &register(None, "noor"), StatusCode::FORBIDDEN).await, "invite_invalid");

    // The taken name above still spent a use? No: a failed registration rolls the use back.
    for name in ["noor", "lea", "sam", "kai"] {
        hearth.join(&done.invite_code, name).await;
    }
    assert_eq!(
        anyone.fails(Method::POST, "/auth/register", &register(Some(&done.invite_code), "late"), StatusCode::FORBIDDEN).await,
        "invite_invalid",
        "five uses, five accounts"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn anyone_on_the_network_can_join_when_the_owner_allows_it() {
    let hearth = TestHearth::start().await;
    hearth.complete_setup(&hearth.browser(), |settings| settings.join = hearth_api::settings::JoinMode::Network).await;
    let request = RegisterRequest { invite_code: None, username: "noor".into(), password: PASSWORD.into(), device: device("Desk PC") };
    let login: LoginResponse = hearth.client().ok(Method::POST, "/auth/register", &request).await;
    assert_eq!(login.user.role, Role::Player);
}

#[tokio::test(flavor = "multi_thread")]
async fn five_wrong_passwords_lock_the_account() {
    let (hearth, _admin) = TestHearth::ready().await;
    let anyone = hearth.client();
    for _ in 0..5 {
        assert_eq!(anyone.fails(Method::POST, "/auth/login", &login_request(ADMIN, "wrong password"), StatusCode::UNAUTHORIZED).await, "invalid_credentials");
    }
    assert_eq!(anyone.fails(Method::POST, "/auth/login", &login_request(ADMIN, PASSWORD), StatusCode::FORBIDDEN).await, "account_locked");
    assert_eq!(anyone.fails(Method::POST, "/auth/login", &login_request("nobody", PASSWORD), StatusCode::UNAUTHORIZED).await, "invalid_credentials");
}

#[tokio::test(flavor = "multi_thread")]
async fn sessions_are_listed_per_device_and_can_be_revoked() {
    let hearth = TestHearth::start().await;
    let done = hearth.complete_setup(&hearth.browser(), |_| {}).await;
    let desktop = hearth.join(&done.invite_code, "robin").await;

    let mut laptop = hearth.client();
    let mut request = login_request("robin", PASSWORD);
    request.device = device("Laptop");
    let login: LoginResponse = laptop.ok(Method::POST, "/auth/login", &request).await;
    laptop.token = login.token;

    let sessions: Vec<Session> = desktop.fetch("/auth/sessions").await;
    let mut names: Vec<_> = sessions.iter().map(|session| session.device_name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Desktop", "Laptop"]);
    assert_eq!(sessions.iter().filter(|session| session.current).count(), 1);

    let (status, _) = desktop.call(Method::DELETE, &format!("/auth/sessions/{}", login.session_id), &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(laptop.fails(Method::GET, "/me", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");

    // Someone else's session is not yours to end, and looks as if it did not exist.
    let noor = hearth.join(&done.invite_code, "noor").await;
    let robins = sessions.iter().find(|session| session.current).unwrap().id;
    assert_eq!(noor.fails(Method::DELETE, &format!("/auth/sessions/{robins}"), &Value::Null, StatusCode::NOT_FOUND).await, "not_found");

    let (status, _) = desktop.post("/auth/logout", &Value::Null).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(desktop.fails(Method::GET, "/me", &Value::Null, StatusCode::UNAUTHORIZED).await, "unauthorized");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_step_sign_in_asks_for_a_code() {
    let (hearth, admin) = TestHearth::ready().await;
    let setup: TotpSetup = admin.ok(Method::POST, "/auth/totp/setup", &Value::Null).await;
    assert!(setup.uri.starts_with("otpauth://totp/") && setup.uri.contains(&setup.secret));
    let code = || hearth::totp::code_at(&setup.secret, hearth::db::now()).unwrap();

    assert_eq!(admin.fails(Method::POST, "/auth/totp/enable", &TotpCode { code: "000000".into() }, StatusCode::FORBIDDEN).await, "totp_invalid");
    let (status, _) = admin.post("/auth/totp/enable", &TotpCode { code: code() }).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let anyone = hearth.client();
    assert_eq!(anyone.fails(Method::POST, "/auth/login", &login_request(ADMIN, PASSWORD), StatusCode::UNAUTHORIZED).await, "totp_required");
    let mut request = login_request(ADMIN, PASSWORD);
    request.totp = Some("123456".into());
    assert_eq!(anyone.fails(Method::POST, "/auth/login", &request, StatusCode::UNAUTHORIZED).await, "totp_invalid");
    request.totp = Some(code());
    let login: LoginResponse = anyone.ok(Method::POST, "/auth/login", &request).await;
    assert!(login.token.is_some());

    // Now that this admin has it, it can be required for all admins, and then not switched off.
    let _: SettingsView = admin.ok(Method::PATCH, "/admin/settings", &json!({ "admin_totp_required": true })).await;
    assert_eq!(admin.fails(Method::POST, "/auth/totp/disable", &TotpCode { code: code() }, StatusCode::BAD_REQUEST).await, "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_signed_hearth_moves_to_https_and_hands_the_admin_over() {
    let hearth = TestHearth::start().await;
    let done = hearth.complete_setup(&hearth.browser(), |settings| settings.tls = TlsMode::SelfSigned).await;
    assert!(done.moved && done.url.starts_with("https://"));
    let handoff = done.handoff.expect("a move comes with a one-time sign-in token");

    // The listener binds again a moment after answering.
    for _ in 0..100 {
        if hearth.running().state.bound().is_some_and(|bound| bound.https) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(hearth.base().starts_with("https://"), "the Hearth should be serving HTTPS by now");

    // A client pins the certificate by the fingerprint the Hearth reports.
    let info: HearthInfo = hearth.client().fetch("/hearth").await;
    let http = reqwest::Client::builder().danger_accept_invalid_certs(true).tls_info(true).build().unwrap();
    let response = http.get(format!("{}/api/v1/health", hearth.base())).send().await.unwrap();
    let certificate = response.extensions().get::<reqwest::tls::TlsInfo>().and_then(|tls| tls.peer_certificate()).unwrap().to_vec();
    assert_eq!(info.cert_sha256.unwrap(), hearth::tls::hex(&<sha2::Sha256 as sha2::Digest>::digest(&certificate)));

    let browser = hearth.browser();
    let login: LoginResponse = browser.ok(Method::POST, "/auth/handoff", &Handoff { token: handoff.clone() }).await;
    assert_eq!(login.token, None, "the web UI gets a cookie, not a token it could leak");
    let me: Me = browser.fetch("/me").await;
    assert_eq!(me.user.role, Role::Admin);
    assert_eq!(hearth.browser().fails(Method::POST, "/auth/handoff", &Handoff { token: handoff }, StatusCode::UNAUTHORIZED).await, "unauthorized");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_web_ui_is_embedded_and_locked_down() {
    let hearth = TestHearth::start().await;
    let http = reqwest::Client::new();
    for path in ["/", "/admin/overview", "/setup"] {
        let page = http.get(format!("{}{path}", hearth.base())).send().await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let policy = page.headers()["content-security-policy"].to_str().unwrap().to_string();
        assert!(policy.contains("default-src 'self'") && policy.contains("frame-ancestors 'none'"));
        assert!(page.text().await.unwrap().contains("/assets/boot.js"), "{path} is a page of the UI");
    }
    let wasm = http.get(format!("{}/assets/hearth-web_bg.wasm", hearth.base())).send().await.unwrap();
    assert_eq!(wasm.headers()["content-type"], "application/wasm");
    let etag = wasm.headers()["etag"].clone();
    assert_eq!(&wasm.bytes().await.unwrap()[..4], b"\0asm");
    let again = http.get(format!("{}/assets/hearth-web_bg.wasm", hearth.base())).header("if-none-match", etag).send().await.unwrap();
    assert_eq!(again.status(), StatusCode::NOT_MODIFIED);

    let missing = http.get(format!("{}/api/v1/nope", hearth.base())).send().await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(missing.json::<Value>().await.unwrap()["error"]["code"], "not_found");
}
