//! What every request handler and background job shares.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Deref,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use hearth_api::settings::Settings;
use sqlx::SqlitePool;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{ApiError, ApiResult},
    paths::Paths,
};

/// How to start a Hearth. The command line only ever sets `data`; the rest exists for tests,
/// which must not grab fixed ports or open browsers.
#[derive(Debug, Clone)]
pub struct Options {
    pub data: PathBuf,
    pub bind: IpAddr,
    /// Listen here instead of on the port from the settings.
    pub port: Option<u16>,
    /// Same for the relay.
    pub relay_port: Option<u16>,
    pub open_browser: bool,
    /// How long someone may be disconnected before they leave their lobbies and sessions.
    /// Long enough for a client to reconnect after a hiccup.
    pub offline_grace: std::time::Duration,
    /// The host's offset from UTC in seconds, for the nightly backup's time of day and for
    /// file names. Read once at start, before any thread exists.
    /// ponytail: a change of summer time takes effect at the next start of the Hearth.
    pub utc_offset: i32,
    /// Where releases are announced. Tests point this at a local stand-in.
    pub update_api: String,
    /// Space for saves per person in bytes, instead of the setting. Tests use a tiny one.
    pub save_quota: Option<u64>,
    /// Where IGDB and SteamGridDB live. Tests point these at a local stand-in.
    pub metadata: crate::library::metadata::Endpoints,
}

impl Options {
    pub fn new(data: impl Into<PathBuf>) -> Self {
        Self {
            data: data.into(),
            bind: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: None,
            relay_port: None,
            open_browser: false,
            offline_grace: std::time::Duration::from_secs(20),
            utc_offset: 0,
            update_api: crate::update::DEFAULT_API.into(),
            save_quota: None,
            metadata: Default::default(),
        }
    }

    /// For tests: loopback only, ports picked by the system.
    pub fn for_tests(data: impl Into<PathBuf>) -> Self {
        Self {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: Some(0),
            relay_port: Some(0),
            offline_grace: std::time::Duration::from_millis(300),
            ..Self::new(data)
        }
    }
}

/// Where the Hearth is listening right now.
#[derive(Debug, Clone)]
pub struct Bound {
    pub addr: SocketAddr,
    /// True when the Hearth terminates TLS itself.
    pub https: bool,
    pub cert_sha256: Option<String>,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub db: SqlitePool,
    pub paths: Paths,
    pub opts: Options,
    pub started: Instant,
    pub limiter: Limiter,
    pub library: crate::library::Shared,
    pub hub: crate::hub::Hub,
    pub multiplayer: crate::multiplayer::Multiplayer,
    /// What the last look for a newer version found, and the release it found.
    pub update: Mutex<Option<(hearth_api::admin::UpdateCheck, crate::update::Release)>>,
    /// Wakes the listener so it binds again with the current settings.
    pub rebind: Notify,
    pub shutdown: CancellationToken,
    settings: watch::Sender<Arc<Settings>>,
    setup_complete: AtomicBool,
    /// Present until setup has been completed. Kept in memory only.
    setup_code: Mutex<Option<String>>,
    /// Lets the browser that finished setup sign in at the new address without the password.
    handoff: Mutex<Option<(String, i64, Instant)>>,
    bound: RwLock<Option<Bound>>,
}

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    pub fn new(db: SqlitePool, paths: Paths, opts: Options, settings: Settings, setup_code: Option<String>) -> Self {
        let library = crate::library::Shared::new(opts.metadata.clone());
        Self(Arc::new(Inner {
            db,
            paths,
            opts,
            started: Instant::now(),
            limiter: Limiter::default(),
            library,
            hub: crate::hub::Hub::default(),
            multiplayer: crate::multiplayer::Multiplayer::default(),
            update: Mutex::new(None),
            rebind: Notify::new(),
            shutdown: CancellationToken::new(),
            settings: watch::Sender::new(Arc::new(settings)),
            setup_complete: AtomicBool::new(setup_code.is_none()),
            setup_code: Mutex::new(setup_code),
            handoff: Mutex::new(None),
            bound: RwLock::new(None),
        }))
    }
}

impl Inner {
    pub fn settings(&self) -> Arc<Settings> {
        self.settings.borrow().clone()
    }

    /// For jobs that react to a changed setting.
    pub fn watch_settings(&self) -> watch::Receiver<Arc<Settings>> {
        self.settings.subscribe()
    }

    /// Writes the settings to the database, then makes them current.
    pub async fn store_settings(&self, settings: Settings) -> ApiResult<()> {
        crate::settings::save(&self.db, &settings).await?;
        self.settings.send_replace(Arc::new(settings));
        Ok(())
    }

    pub fn setup_complete(&self) -> bool {
        self.setup_complete.load(Ordering::Relaxed)
    }

    pub fn finish_setup(&self) {
        self.setup_complete.store(true, Ordering::Relaxed);
        *self.setup_code.lock().unwrap() = None;
    }

    pub fn setup_code(&self) -> Option<String> {
        self.setup_code.lock().unwrap().clone()
    }

    /// Fails with `setup_required` until an admin exists.
    pub fn require_setup(&self) -> ApiResult<()> {
        if self.setup_complete() {
            return Ok(());
        }
        Err(ApiError::coded(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            hearth_api::ErrorCode::SetupRequired,
            "This Hearth has not been set up yet. Open it in a browser to run first-time setup.",
        ))
    }

    pub fn bound(&self) -> Option<Bound> {
        self.bound.read().unwrap().clone()
    }

    pub fn set_bound(&self, bound: Bound) {
        *self.bound.write().unwrap() = Some(bound);
    }

    /// A one-time token, valid for two minutes, that signs `user_id` in.
    pub fn issue_handoff(&self, user_id: i64) -> String {
        let token = crate::auth::new_token();
        *self.handoff.lock().unwrap() = Some((token.clone(), user_id, Instant::now()));
        token
    }

    pub fn take_handoff(&self, token: &str) -> Option<i64> {
        let mut slot = self.handoff.lock().unwrap();
        match slot.take() {
            Some((expected, user_id, at)) if at.elapsed() < Duration::from_secs(120) && expected == token => Some(user_id),
            _ => None,
        }
    }
}

/// Counts failed attempts per kind and address, so guessing a code or a password gets slow fast.
/// Attempts that succeed are never counted: browsing folders in the wizard must not lock it.
#[derive(Default)]
pub struct Limiter(Mutex<Attempts>);

/// Per kind and address: how many failures, and when the count started.
type Attempts = HashMap<(&'static str, IpAddr), (u32, Instant)>;

impl Limiter {
    /// Refuses when this address has already failed `max` times within `window`.
    pub fn check(&self, kind: &'static str, ip: IpAddr, max: u32, window: Duration) -> ApiResult<()> {
        let map = self.0.lock().unwrap();
        match map.get(&(kind, ip)) {
            Some((count, start)) if start.elapsed() <= window && *count >= max => Err(ApiError::rate_limited()),
            _ => Ok(()),
        }
    }

    /// Records one failure.
    pub fn fail(&self, kind: &'static str, ip: IpAddr, window: Duration) {
        let mut map = self.0.lock().unwrap();
        if map.len() > 10_000 {
            map.retain(|_, (_, start)| start.elapsed() < Duration::from_secs(3600));
        }
        let entry = map.entry((kind, ip)).or_insert((0, Instant::now()));
        if entry.1.elapsed() > window {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_blocks_after_max_and_keeps_kinds_apart() {
        let limiter = Limiter::default();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let window = Duration::from_secs(60);
        for _ in 0..3 {
            limiter.check("login", ip, 3, window).unwrap();
            limiter.fail("login", ip, window);
        }
        assert!(limiter.check("login", ip, 3, window).is_err());
        assert!(limiter.check("invite", ip, 3, window).is_ok());
    }
}
