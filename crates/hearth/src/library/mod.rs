//! The game library: what is on disk, what it is, and how clients download it.

use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use hearth_api::library::{Game, GameSummary, LaunchOption, ManifestState, Players, Prerequisite, ScanStatus, SourceKind, Version, VersionRef};
use sqlx::SqlitePool;
use tokio::sync::Notify;

pub mod manifest;
pub mod metadata;
pub mod names;
pub mod scan;
pub mod watch;

/// What the scanner, the hasher and the request handlers share.
pub struct Shared {
    pub providers: metadata::Providers,
    pub scan_wake: Notify,
    pub hash_wake: Notify,
    status: Mutex<ScanStatus>,
    /// Files seen by the scan that is running. Updated from the blocking walk.
    files_seen: AtomicU64,
}

impl Shared {
    pub fn new(endpoints: metadata::Endpoints) -> Self {
        Self {
            providers: metadata::Providers::new(endpoints),
            scan_wake: Notify::new(),
            hash_wake: Notify::new(),
            status: Mutex::new(ScanStatus::default()),
            files_seen: AtomicU64::new(0),
        }
    }

    /// Asks for a scan. Several requests while one runs fold into a single further scan.
    pub fn request_scan(&self) {
        self.scan_wake.notify_one();
    }

    pub async fn status(&self, db: &SqlitePool) -> ScanStatus {
        let hashing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM manifests WHERE state = 'pending'").fetch_one(db).await.unwrap_or(0);
        let mut status = self.status.lock().unwrap().clone();
        status.files = self.files_seen.load(Ordering::Relaxed);
        status.hashing = hashing as u32;
        status
    }

    fn set_running(&self, running: bool, finished_at: Option<i64>) {
        let mut status = self.status.lock().unwrap();
        status.running = running;
        if running {
            self.files_seen.store(0, Ordering::Relaxed);
        }
        if finished_at.is_some() {
            status.last_finished_at = finished_at;
            status.scans_finished += 1;
        }
    }
}

pub fn kind_to_db(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Folder => "folder",
        SourceKind::Archive => "archive",
        SourceKind::Installer => "installer",
        SourceKind::Image => "image",
    }
}

pub fn kind_from_db(kind: &str) -> SourceKind {
    match kind {
        "archive" => SourceKind::Archive,
        "installer" => SourceKind::Installer,
        "image" => SourceKind::Image,
        _ => SourceKind::Folder,
    }
}

pub fn players_to_db(players: Players) -> &'static str {
    match players {
        Players::Single => "single",
        Players::Coop => "coop",
        Players::Online => "online",
    }
}

fn players_from_db(players: &str) -> Players {
    match players {
        "coop" => Players::Coop,
        "online" => Players::Online,
        _ => Players::Single,
    }
}

fn state_from_db(state: Option<&str>) -> ManifestState {
    match state {
        Some("ready") => ManifestState::Ready,
        Some("failed") | None => ManifestState::Failed,
        Some(_) => ManifestState::Pending,
    }
}

fn art_url(file: Option<String>) -> Option<String> {
    file.map(|file| format!("{}/art/{file}", hearth_api::API_PREFIX))
}

#[derive(sqlx::FromRow)]
struct GameRow {
    id: i64,
    title: String,
    developer: String,
    year: Option<i64>,
    genre: String,
    players: String,
    description: String,
    steam_app_id: Option<i64>,
    cover: Option<String>,
    hero: Option<String>,
    logo: Option<String>,
    visible: bool,
    networking: bool,
    emulator_id: Option<i64>,
    added_at: i64,
    updated_at: i64,
}

#[derive(sqlx::FromRow)]
struct VersionRow {
    id: i64,
    game_id: i64,
    name: String,
    note: String,
    added_at: i64,
    update_bytes: Option<i64>,
    state: Option<String>,
    bytes: Option<i64>,
    files: Option<i64>,
    kind: Option<String>,
}

const VERSIONS: &str = "SELECT v.id, v.game_id, v.name, v.note, v.added_at, v.update_bytes, m.state, m.bytes, m.files, s.kind
    FROM versions v LEFT JOIN manifests m ON m.id = v.manifest_id LEFT JOIN sources s ON s.id = v.source_id";

fn summarize(game: &GameRow, versions: &[&VersionRow]) -> GameSummary {
    // Versions come newest first. The one clients get is the newest that is ready.
    let current = versions.iter().find(|version| version.state.as_deref() == Some("ready"));
    let newest = versions.first();
    GameSummary {
        id: game.id,
        title: game.title.clone(),
        developer: game.developer.clone(),
        year: game.year.map(|year| year as i32),
        genre: game.genre.clone(),
        players: players_from_db(&game.players),
        cover: art_url(game.cover.clone()),
        hero: art_url(game.hero.clone()),
        logo: art_url(game.logo.clone()),
        kind: kind_from_db(current.or(newest).and_then(|version| version.kind.as_deref()).unwrap_or("folder")),
        bytes: current.or(newest).and_then(|version| version.bytes).unwrap_or(0) as u64,
        version: current.map(|version| VersionRef { id: version.id, name: version.name.clone() }),
        networking: game.networking,
        emulator_id: game.emulator_id,
        visible: game.visible,
        added_at: game.added_at,
        updated_at: game.updated_at,
    }
}

/// Every game, with hidden ones only when `all` is set.
pub async fn load_games(db: &SqlitePool, all: bool) -> Result<Vec<GameSummary>, sqlx::Error> {
    let games: Vec<GameRow> = sqlx::query_as("SELECT * FROM games WHERE visible = 1 OR ?1 ORDER BY title COLLATE NOCASE").bind(all).fetch_all(db).await?;
    let versions: Vec<VersionRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{VERSIONS} ORDER BY v.id DESC"))).fetch_all(db).await?;
    Ok(games
        .iter()
        .map(|game| summarize(game, &versions.iter().filter(|version| version.game_id == game.id).collect::<Vec<_>>()))
        .collect())
}

pub async fn load_game(db: &SqlitePool, id: i64) -> Result<Option<Game>, sqlx::Error> {
    let Some(game) = sqlx::query_as::<_, GameRow>("SELECT * FROM games WHERE id = ?").bind(id).fetch_optional(db).await? else {
        return Ok(None);
    };
    let versions: Vec<VersionRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{VERSIONS} WHERE v.game_id = ? ORDER BY v.id DESC"))).bind(id).fetch_all(db).await?;
    let launch: Vec<(String, String, String, bool)> =
        sqlx::query_as("SELECT name, path, args, is_default FROM launch_options WHERE game_id = ? ORDER BY position").bind(id).fetch_all(db).await?;
    let prerequisites: Vec<(String, String, String)> =
        sqlx::query_as("SELECT name, path, args FROM prerequisites WHERE game_id = ? ORDER BY position").bind(id).fetch_all(db).await?;
    Ok(Some(Game {
        summary: summarize(&game, &versions.iter().collect::<Vec<_>>()),
        description: game.description,
        steam_app_id: game.steam_app_id.map(|id| id as u32),
        launch: launch.into_iter().map(|(name, path, args, default)| LaunchOption { name, path, args, default }).collect(),
        prerequisites: prerequisites.into_iter().map(|(name, path, args)| Prerequisite { name, path, args }).collect(),
        versions: versions
            .into_iter()
            .map(|version| Version {
                id: version.id,
                name: version.name,
                note: version.note,
                state: state_from_db(version.state.as_deref()),
                bytes: version.bytes.unwrap_or(0) as u64,
                files: version.files.unwrap_or(0) as u32,
                update_bytes: version.update_bytes.map(|bytes| bytes as u64),
                added_at: version.added_at,
            })
            .collect(),
    }))
}
