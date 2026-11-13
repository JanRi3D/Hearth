//! Hearth, the self-hosted server for Bonfire.
//!
//! The binary is a thin shell around [`run`]. Tests start a Hearth the same way, against a
//! temporary data folder.

use std::net::SocketAddr;

use tokio::{sync::oneshot, task::JoinHandle};

pub mod auth;
pub mod backup;
pub mod dav;
pub mod db;
pub mod error;
pub mod fsutil;
pub mod hub;
pub mod jobs;
pub mod library;
pub mod listener;
pub mod multiplayer;
pub mod paths;
pub mod relay;
pub mod routes;
pub mod settings;
pub mod state;
pub mod storage;
pub mod tls;
pub mod totp;
pub mod update;
pub mod web;

pub use state::{AppState, Options};

pub type StartError = Box<dyn std::error::Error + Send + Sync>;

/// A Hearth that is up and answering.
pub struct Running {
    pub addr: SocketAddr,
    /// Present until first-time setup has been completed.
    pub setup_code: Option<String>,
    pub state: AppState,
    tasks: Vec<JoinHandle<()>>,
}

impl Running {
    pub fn url(&self) -> String {
        let https = self.state.bound().is_some_and(|bound| bound.https);
        format!("{}://{}", if https { "https" } else { "http" }, self.addr)
    }

    /// Stops listening, lets running jobs finish and closes the database.
    pub async fn stop(self) {
        self.state.shutdown.cancel();
        for task in self.tasks {
            let _ = task.await;
        }
        self.state.db.close().await;
    }

    /// Resolves when the Hearth has been told to stop.
    pub async fn stopped(&self) {
        self.state.shutdown.cancelled().await;
    }
}

pub async fn run(opts: Options) -> Result<Running, StartError> {
    // Both TLS and outgoing HTTPS use rustls with ring. A second call is a harmless no-op.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let paths = paths::Paths::create(&opts.data)?;
    let db = db::open(&paths.db()).await?;
    let setup_complete = db::meta_get(&db, "setup_complete").await?.is_some();
    let settings = settings::load(&db).await?.unwrap_or_else(|| settings::defaults(&paths));
    let setup_code = (!setup_complete).then(auth::setup_code);

    let state = AppState::new(db, paths, opts, settings, setup_code.clone());
    let app = routes::router(state.clone());

    let (ready, bound) = oneshot::channel();
    let listener = tokio::spawn(listener::run(state.clone(), app, ready));
    let addr = bound.await??;

    let tasks = vec![
        listener,
        tokio::spawn(library::scan::scanner(state.clone())),
        tokio::spawn(library::manifest::hasher(state.clone())),
        tokio::spawn(library::watch::watcher(state.clone())),
        tokio::spawn(jobs::janitor(state.clone())),
        tokio::spawn(relay::relay(state.clone())),
        tokio::spawn(routes::firelink::checker(state.clone())),
    ];
    // Look at the games folders once at every start. Files may have changed while the Hearth was off.
    state.library.request_scan();

    Ok(Running { addr, setup_code, state, tasks })
}
