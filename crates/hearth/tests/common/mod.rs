//! A small test client. It talks to a real Hearth over HTTP, with the same wire types
//! Bonfire will use, against a temporary data folder.
#![allow(dead_code)]

use std::path::PathBuf;

use hearth::{Options, Running};
use hearth_api::{
    auth::{DeviceInfo, LoginRequest, LoginResponse, RegisterRequest},
    library::Library,
    settings::{Settings, TlsMode},
    setup::{SetupDefaults, SetupDone, SetupRequest},
};
use futures_util::StreamExt;
use hearth_api::social::{Event, Presence, Status};
use reqwest::{Method, StatusCode};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use tempfile::TempDir;

pub const ADMIN: &str = "mika";
pub const PASSWORD: &str = "correct horse battery";

pub struct TestHearth {
    pub running: Option<Running>,
    pub dir: TempDir,
    pub games: PathBuf,
    opts: Options,
}

impl TestHearth {
    /// A Hearth on a fresh data folder, waiting for first-time setup.
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// Like `start`, with a say in how the Hearth is started.
    pub async fn start_with(change: impl FnOnce(&mut Options)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let games = dir.path().join("games");
        std::fs::create_dir_all(&games).unwrap();
        let mut opts = Options::for_tests(dir.path().join("data"));
        change(&mut opts);
        let running = hearth::run(opts.clone()).await.unwrap();
        Self { running: Some(running), dir, games, opts }
    }

    /// Waits until more than `scans` scans have finished and every version is hashed.
    pub async fn settled(&self, admin: &Api, scans: u64) -> Library {
        for _ in 0..600 {
            let library: Library = admin.fetch("/admin/library").await;
            if library.scan.scans_finished > scans && !library.scan.running && library.scan.hashing == 0 {
                return library;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("the library did not settle within 30 seconds");
    }

    /// Asks for a rescan, as the Rescan button does, and waits for it and the hashing to finish.
    pub async fn rescan(&self, admin: &Api) -> Library {
        let before: Library = admin.fetch("/admin/library").await;
        let (status, _) = admin.post("/admin/library/scan", &Value::Null).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        self.settled(admin, before.scan.scans_finished).await
    }

    /// A Hearth that has been through setup, plus its admin signed in the way the web UI is.
    pub async fn ready() -> (Self, Api) {
        let hearth = Self::start().await;
        let admin = hearth.browser();
        let done = hearth.complete_setup(&admin, |_| {}).await;
        assert!(!done.moved);
        (hearth, admin)
    }

    pub async fn complete_setup(&self, browser: &Api, change: impl FnOnce(&mut Settings)) -> SetupDone {
        let mut setup = browser.clone();
        setup.setup_code = self.running().setup_code.clone();
        let defaults: SetupDefaults = setup.ok(Method::POST, "/setup/begin", &Value::Null).await;
        let mut settings = defaults.settings;
        settings.name = "home".into();
        settings.address = "127.0.0.1".into();
        settings.tls = TlsMode::Proxy;
        settings.games_folders = vec![self.games.display().to_string()];
        change(&mut settings);
        setup.ok(Method::POST, "/setup/complete", &SetupRequest { username: ADMIN.into(), password: PASSWORD.into(), settings }).await
    }

    pub fn running(&self) -> &Running {
        self.running.as_ref().unwrap()
    }

    pub fn base(&self) -> String {
        let bound = self.running().state.bound().unwrap();
        format!("{}://127.0.0.1:{}", if bound.https { "https" } else { "http" }, bound.addr.port())
    }

    /// A client that keeps cookies and sends the web UI's header, like the admin pages do.
    pub fn browser(&self) -> Api {
        let http = reqwest::Client::builder().cookie_store(true).danger_accept_invalid_certs(true).build().unwrap();
        Api { http, base: self.base(), token: None, ui: true, setup_code: None }
    }

    /// A client without any session, like Bonfire before sign-in.
    pub fn client(&self) -> Api {
        let http = reqwest::Client::builder().danger_accept_invalid_certs(true).build().unwrap();
        Api { http, base: self.base(), token: None, ui: false, setup_code: None }
    }

    /// Creates an account with an invite and returns a client signed in as it.
    pub async fn join(&self, invite: &str, username: &str) -> Api {
        let mut api = self.client();
        let request = RegisterRequest {
            invite_code: Some(invite.into()),
            username: username.into(),
            password: PASSWORD.into(),
            device: device("Desktop"),
        };
        let login: LoginResponse = api.ok(Method::POST, "/auth/register", &request).await;
        api.token = login.token;
        api
    }

    /// Stops the Hearth and starts it again on the same data folder.
    pub async fn restart(&mut self) {
        self.running.take().unwrap().stop().await;
        self.running = Some(hearth::run(self.opts.clone()).await.unwrap());
    }
}

pub fn device(name: &str) -> DeviceInfo {
    DeviceInfo { client_id: format!("test-{name}"), name: name.into(), platform: "Windows 11".into(), client: "Bonfire test".into() }
}

pub fn login_request(username: &str, password: &str) -> LoginRequest {
    LoginRequest { username: username.into(), password: password.into(), totp: None, device: device("Desktop"), cookie: false }
}

#[derive(Clone)]
pub struct Api {
    pub http: reqwest::Client,
    pub base: String,
    pub token: Option<String>,
    pub ui: bool,
    pub setup_code: Option<String>,
}

impl Api {
    /// Sends a request and returns the status with the body as JSON (null when there is none).
    pub async fn call(&self, method: Method, path: &str, body: &impl Serialize) -> (StatusCode, Value) {
        let mut request = self.http.request(method, format!("{}/api/v1{path}", self.base));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if self.ui {
            request = request.header(hearth_api::WEB_UI_HEADER, "1");
        }
        if let Some(code) = &self.setup_code {
            request = request.header(hearth_api::SETUP_CODE_HEADER, code);
        }
        let body = serde_json::to_value(body).unwrap();
        if !body.is_null() {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.call(Method::GET, path, &Value::Null).await
    }

    pub async fn post(&self, path: &str, body: &impl Serialize) -> (StatusCode, Value) {
        self.call(Method::POST, path, body).await
    }

    /// Expects success and reads the answer into the shared wire type.
    pub async fn ok<T: DeserializeOwned>(&self, method: Method, path: &str, body: &impl Serialize) -> T {
        let (status, value) = self.call(method.clone(), path, body).await;
        assert!(status.is_success(), "{method} {path} answered {status}: {value}");
        serde_json::from_value(value.clone()).unwrap_or_else(|err| panic!("{method} {path} answered {value}, which is not the expected type: {err}"))
    }

    pub async fn fetch<T: DeserializeOwned>(&self, path: &str) -> T {
        self.ok(Method::GET, path, &Value::Null).await
    }

    /// Fetches raw bytes, optionally one inclusive byte range, the way a download does.
    pub async fn bytes(&self, path: &str, range: Option<(u64, u64)>) -> (StatusCode, Vec<u8>) {
        let mut request = self.http.get(format!("{}/api/v1{path}", self.base));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some((first, last)) = range {
            request = request.header("range", format!("bytes={first}-{last}"));
        }
        let response = request.send().await.unwrap();
        (response.status(), response.bytes().await.unwrap().to_vec())
    }

    /// Expects a failure and returns its error code.
    pub async fn fails(&self, method: Method, path: &str, body: &impl Serialize, status: StatusCode) -> String {
        let (got, value) = self.call(method.clone(), path, body).await;
        assert_eq!(got, status, "{method} {path} answered {value}");
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(!message.is_empty(), "an error must say what happened: {value}");
        value["error"]["code"].as_str().unwrap_or_default().to_string()
    }
}

// ---------- the live connection ----------

pub type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A client's live connection.
pub struct Live(pub Socket);

impl Live {
    pub async fn connect(api: &Api) -> Self {
        let mut request = format!("{}/api/v1/ws", api.base.replace("http", "ws")).into_client_request().unwrap();
        request.headers_mut().insert("authorization", format!("Bearer {}", api.token.as_deref().unwrap()).parse().unwrap());
        let (socket, _) = tokio_tungstenite::connect_async(request).await.expect("the WebSocket should open");
        let mut live = Self(socket);
        assert!(matches!(live.next().await, Some(Event::Hello(_))), "the first event greets the client");
        live
    }

    /// The next event, or `None` when the Hearth closed the connection.
    pub async fn next(&mut self) -> Option<Event> {
        loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), self.0.next()).await.expect("an event should arrive within 5 seconds");
            match frame {
                Some(Ok(tungstenite::Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap_or_else(|err| panic!("{text} is not an event: {err}"))),
                Some(Ok(tungstenite::Message::Close(_))) | Some(Err(_)) | None => return None,
                Some(Ok(_)) => {}
            }
        }
    }

    /// Skips ahead to the first event the test is interested in.
    pub async fn find<T>(&mut self, pick: impl Fn(Event) -> Option<T>) -> T {
        loop {
            let event = self.next().await.expect("the connection closed before the expected event arrived");
            if let Some(found) = pick(event) {
                return found;
            }
        }
    }

    pub async fn presence_of(&mut self, user_id: i64, status: Status) -> Presence {
        self.find(|event| match event {
            Event::Presence(presence) if presence.user_id == user_id && presence.status == status => Some(presence),
            _ => None,
        })
        .await
    }
}

/// Signs an existing account in on a device with this name.
pub async fn sign_in(hearth: &TestHearth, username: &str, device_name: &str) -> Api {
    let mut api = hearth.client();
    let mut request = login_request(username, PASSWORD);
    request.device = device(device_name);
    let login: LoginResponse = api.ok(Method::POST, "/auth/login", &request).await;
    api.token = login.token;
    api
}
