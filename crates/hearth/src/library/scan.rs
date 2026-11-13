//! The scanner: looks into the games folders, records what it finds, and places each new
//! find, either into the library on its own or into Metadata review.

use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    time::UNIX_EPOCH,
};

use hearth_api::library::{Players, SourceKind};

use super::{
    kind_from_db, kind_to_db,
    metadata::{Meta, Providers},
    names,
    players_to_db,
};
use crate::{
    db::{self, now},
    error::ApiResult,
    fsutil,
    state::AppState,
};

/// An entry at the top of a games folder.
struct Found {
    path: String,
    folder: String,
    name: String,
    kind: SourceKind,
    bytes: u64,
    files: u64,
    signature: String,
}

/// A source as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Source {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub kind: String,
    pub signature: String,
    pub state: String,
}

fn kind_of(path: &Path) -> Option<SourceKind> {
    if path.is_dir() {
        return Some(SourceKind::Folder);
    }
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "zip" | "7z" | "rar" => Some(SourceKind::Archive),
        "exe" => Some(SourceKind::Installer),
        "iso" => Some(SourceKind::Image),
        _ => None,
    }
}

/// Lists one games folder. Sizes come from walking each game folder, which also yields the
/// signature that tells a later scan whether anything inside changed.
fn look(folder: &str, seen: &AtomicU64) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        tracing::warn!(folder, "a games folder could not be opened");
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // Hidden entries include uploads that are still arriving (".name.part").
        if fsutil::is_hidden(&path) {
            continue;
        }
        let (Some(kind), Some(name), Some(full)) = (kind_of(&path), path.file_name().and_then(|name| name.to_str()), path.to_str()) else {
            continue;
        };
        let (mut files, mut bytes, mut newest) = (0u64, 0u64, 0u64);
        let mut note = |meta: &std::fs::Metadata| {
            files += 1;
            bytes += meta.len();
            let changed = meta.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |since| since.as_secs());
            newest = newest.max(changed);
            seen.fetch_add(1, Ordering::Relaxed);
        };
        if kind == SourceKind::Folder {
            let mut stack = vec![path.clone()];
            while let Some(dir) = stack.pop() {
                for inner in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    match inner.file_type() {
                        Ok(kind) if kind.is_dir() => stack.push(inner.path()),
                        Ok(kind) if kind.is_file() => {
                            if let Ok(meta) = inner.metadata() {
                                note(&meta);
                            }
                        }
                        _ => {}
                    }
                }
            }
        } else if let Ok(meta) = entry.metadata() {
            note(&meta);
        }
        found.push(Found {
            path: full.to_string(),
            folder: folder.to_string(),
            name: name.to_string(),
            kind,
            bytes,
            files,
            signature: format!("{files}:{bytes}:{newest}"),
        });
    }
    found
}

/// Runs until shutdown: one scan per request, never two at once.
pub async fn scanner(state: AppState) {
    loop {
        tokio::select! {
            _ = state.library.scan_wake.notified() => {}
            _ = state.shutdown.cancelled() => return,
        }
        if !state.setup_complete() {
            continue;
        }
        state.library.set_running(true, None);
        if let Err(err) = scan(&state).await {
            tracing::error!(error = %err.message, "the scan stopped early");
        }
        let finished = now();
        let _ = db::meta_set(&state.db, "last_scan_at", &finished.to_string()).await;
        state.library.set_running(false, Some(finished));
    }
}

async fn scan(state: &AppState) -> ApiResult<()> {
    let folders = state.settings().games_folders.clone();
    let library = state.clone();
    let found: Vec<Found> =
        tokio::task::spawn_blocking(move || folders.iter().flat_map(|folder| look(folder, &library.library.files_seen)).collect()).await?;

    let known: Vec<Source> = sqlx::query_as("SELECT id, path, name, kind, signature, state FROM sources").fetch_all(&state.db).await?;
    let now = now();
    let mut added = 0;
    for entry in &found {
        match known.iter().find(|source| source.path == entry.path) {
            None => {
                sqlx::query("INSERT INTO sources (path, folder, name, kind, bytes, files, signature, found_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
                    .bind(&entry.path)
                    .bind(&entry.folder)
                    .bind(&entry.name)
                    .bind(kind_to_db(entry.kind))
                    .bind(entry.bytes as i64)
                    .bind(entry.files as i64)
                    .bind(&entry.signature)
                    .bind(now)
                    .execute(&state.db)
                    .await?;
                added += 1;
            }
            Some(source) if source.signature != entry.signature || source.state == "missing" => {
                // The files changed, or came back. A version built from them has to be hashed again.
                let has_version: Option<i64> =
                    sqlx::query_scalar("SELECT manifest_id FROM versions WHERE source_id = ? AND manifest_id IS NOT NULL").bind(source.id).fetch_optional(&state.db).await?;
                let next = match (source.state.as_str(), has_version) {
                    ("missing", Some(_)) => "matched",
                    ("missing", None) => "new",
                    (state, _) => state,
                };
                sqlx::query("UPDATE sources SET bytes = ?, files = ?, signature = ?, state = ? WHERE id = ?")
                    .bind(entry.bytes as i64)
                    .bind(entry.files as i64)
                    .bind(&entry.signature)
                    .bind(next)
                    .bind(source.id)
                    .execute(&state.db)
                    .await?;
                if let Some(manifest_id) = has_version {
                    super::manifest::invalidate(state, manifest_id).await?;
                }
            }
            Some(_) => {}
        }
    }
    for source in known.iter().filter(|source| source.state != "missing" && !found.iter().any(|entry| entry.path == source.path)) {
        sqlx::query("UPDATE sources SET state = 'missing' WHERE id = ?").bind(source.id).execute(&state.db).await?;
        // What is gone from disk cannot be downloaded.
        sqlx::query("UPDATE manifests SET state = 'failed' WHERE id IN (SELECT manifest_id FROM versions WHERE source_id = ?)")
            .bind(source.id)
            .execute(&state.db)
            .await?;
        if source.state == "matched" {
            db::log_activity(&state.db, format!("{} is gone from its games folder", source.name)).await;
        }
    }

    let placed = place_new(state).await?;
    crate::routes::emulators::refresh(state).await?;
    state.library.hash_wake.notify_one();
    let files: u64 = found.iter().map(|entry| entry.files).sum();
    if added > 0 {
        db::log_activity(&state.db, format!("Rescan finished: {files} files, {placed} added, {} waiting in Metadata review", added - placed)).await;
    } else {
        tracing::info!(files, "rescan finished, nothing new");
    }
    Ok(())
}

/// Decides what each newly found source is. Returns how many went straight into the library.
async fn place_new(state: &AppState) -> ApiResult<u32> {
    let fresh: Vec<Source> = sqlx::query_as("SELECT id, path, name, kind, signature, state FROM sources WHERE state = 'new' ORDER BY name").fetch_all(&state.db).await?;
    let settings = state.settings();
    let providers = Providers::igdb_ready(&settings) || Providers::sgdb_ready(&settings);
    let mut placed = 0;
    for source in fresh {
        let parsed = names::parse(&source.name);
        let games: Vec<(i64, String)> = sqlx::query_as("SELECT id, title FROM games").fetch_all(&state.db).await?;

        // A newer build of a game that is already here is never added as a second game.
        if let Some((game_id, _)) = games.iter().find(|(_, title)| names::is_update_of(&parsed, title)) {
            send_to_review(state, source.id, &[], Some(*game_id)).await?;
            continue;
        }
        if !providers {
            // No keys: every game is listed under the name of its folder.
            create_game(state, &source, None).await?;
            placed += 1;
            continue;
        }
        match state.library.providers.search(&settings, &parsed).await {
            Ok(found) => match found.first().filter(|best| best.candidate.confidence >= settings.match_threshold) {
                Some(best) => {
                    create_game(state, &source, Some(best)).await?;
                    db::log_activity(&state.db, format!("Rescan found {}, matched at {:.2}", best.candidate.title, best.candidate.confidence)).await;
                    placed += 1;
                }
                None => send_to_review(state, source.id, &found, None).await?,
            },
            Err(err) => {
                tracing::warn!(source = %source.name, error = %err, "metadata lookup failed");
                send_to_review(state, source.id, &[], None).await?;
            }
        }
    }
    Ok(placed)
}

async fn send_to_review(state: &AppState, source_id: i64, candidates: &[Meta], update_of: Option<i64>) -> ApiResult<()> {
    sqlx::query("UPDATE sources SET state = 'review', candidates = ?, update_of = ? WHERE id = ?")
        .bind(serde_json::to_string(candidates)?)
        .bind(update_of)
        .bind(source_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Adds a source to the library as a new game, with the details of `meta` when there are any.
pub async fn create_game(state: &AppState, source: &Source, meta: Option<&Meta>) -> ApiResult<i64> {
    let parsed = names::parse(&source.name);
    let settings = state.settings();
    let now = now();
    let title = meta.map_or(parsed.title.clone(), |meta| meta.candidate.title.clone());
    let players = meta.map_or(Players::Single, |meta| meta.players);

    let mut tx = state.db.begin().await?;
    let game_id: i64 = sqlx::query_scalar(
        "INSERT INTO games (title, developer, year, genre, players, description, igdb_id, sgdb_id, networking, added_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&title)
    .bind(meta.map_or("", |meta| meta.developer.as_str()))
    .bind(meta.and_then(|meta| meta.candidate.year).or(parsed.year))
    .bind(meta.map_or("", |meta| meta.genre.as_str()))
    .bind(players_to_db(players))
    .bind(meta.map_or("", |meta| meta.description.as_str()))
    .bind(meta.and_then(|meta| meta.candidate.igdb_id).map(|id| id as i64))
    .bind(meta.and_then(|meta| meta.candidate.sgdb_id).map(|id| id as i64))
    // Bonfire networking only means something for a game people play together.
    .bind(settings.networking_default && players != Players::Single)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    add_version_in(&mut tx, source, game_id, parsed.version.as_deref().unwrap_or("1.0"), "").await?;
    tx.commit().await?;

    if kind_from_db(&source.kind) == SourceKind::Folder {
        let (root, wanted) = (source.path.clone(), title.clone());
        if let Some(exe) = tokio::task::spawn_blocking(move || find_executable(Path::new(&root), &wanted)).await? {
            sqlx::query("INSERT INTO launch_options (game_id, position, name, path, is_default) VALUES (?, 0, ?, ?, 1)")
                .bind(game_id)
                .bind(&title)
                .bind(exe)
                .execute(&state.db)
                .await?;
        }
    }
    state.library.hash_wake.notify_one();

    if Providers::sgdb_ready(&settings) {
        let (state, sgdb_id) = (state.clone(), meta.and_then(|meta| meta.candidate.sgdb_id));
        tokio::spawn(async move {
            let art = state.library.providers.art(&state.settings(), game_id, sgdb_id, &title, &state.paths.dir("art")).await;
            let saved = sqlx::query("UPDATE games SET cover = COALESCE(?, cover), hero = COALESCE(?, hero), logo = COALESCE(?, logo), updated_at = ? WHERE id = ?")
                .bind(art.cover)
                .bind(art.hero)
                .bind(art.logo)
                .bind(db::now())
                .bind(game_id)
                .execute(&state.db)
                .await;
            if let Err(err) = saved {
                tracing::warn!(game_id, error = %err, "could not store the art");
            }
        });
    }
    Ok(game_id)
}

/// Adds a source as a version of an existing game.
pub async fn add_version(state: &AppState, source: &Source, game_id: i64, name: &str, note: &str) -> ApiResult<i64> {
    let mut tx = state.db.begin().await?;
    let version_id = add_version_in(&mut tx, source, game_id, name, note).await?;
    sqlx::query("UPDATE games SET updated_at = ? WHERE id = ?").bind(now()).bind(game_id).execute(&mut *tx).await?;
    tx.commit().await?;
    state.library.hash_wake.notify_one();
    Ok(version_id)
}

async fn add_version_in(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, source: &Source, game_id: i64, name: &str, note: &str) -> ApiResult<i64> {
    let now = now();
    let manifest_id: i64 =
        sqlx::query_scalar("INSERT INTO manifests (root, created_at) VALUES (?, ?) RETURNING id").bind(&source.path).bind(now).fetch_one(&mut **tx).await?;
    let version_id: i64 =
        sqlx::query_scalar("INSERT INTO versions (game_id, source_id, manifest_id, name, note, added_at) VALUES (?, ?, ?, ?, ?, ?) RETURNING id")
            .bind(game_id)
            .bind(source.id)
            .bind(manifest_id)
            .bind(name.trim())
            .bind(note.trim())
            .bind(now)
            .fetch_one(&mut **tx)
            .await?;
    sqlx::query("UPDATE sources SET state = 'matched', candidates = '[]', update_of = NULL WHERE id = ?").bind(source.id).execute(&mut **tx).await?;
    Ok(version_id)
}

/// Picks the program that most likely starts the game: an .exe near the top of the folder
/// whose name is closest to the title. Installers and helpers are passed over.
/// ponytail: Windows games only. A Linux build has no .exe, and its admin names the program
/// in the game editor.
fn find_executable(root: &Path, title: &str) -> Option<String> {
    const HELPERS: [&str; 9] = ["unins", "crash", "redist", "setup", "install", "dxsetup", "vc_", "dotnet", "report"];
    let wanted = names::normalize(title);
    let mut best: Option<(f64, usize, String)> = None;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() && depth < 3 => stack.push((path, depth + 1)),
                Ok(kind) if kind.is_file() && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe")) => {
                    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default().to_lowercase();
                    if HELPERS.iter().any(|helper| stem.contains(helper)) {
                        continue;
                    }
                    let Some(relative) = path.strip_prefix(root).ok().and_then(|path| path.to_str()) else { continue };
                    let score = strsim::normalized_levenshtein(&names::normalize(&stem), &wanted);
                    // Closest name wins. Between equals, the one nearer the top.
                    if best.as_ref().is_none_or(|(best_score, best_depth, _)| score > *best_score || (score == *best_score && depth < *best_depth)) {
                        best = Some((score, depth, relative.replace('\\', "/")));
                    }
                }
                _ => {}
            }
        }
    }
    best.map(|(_, _, path)| path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_into_a_games_folder_and_finds_the_program_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("Factorio/bin/x64")).unwrap();
        std::fs::write(root.join("Factorio/bin/x64/factorio.exe"), vec![0u8; 100]).unwrap();
        std::fs::write(root.join("Factorio/unins000.exe"), vec![0u8; 10]).unwrap();
        std::fs::write(root.join("Tyrian 2000 (1999).zip"), vec![0u8; 50]).unwrap();
        std::fs::write(root.join("notes.txt"), b"x").unwrap();
        std::fs::write(root.join(".upload.zip.part"), b"x").unwrap();

        let seen = AtomicU64::new(0);
        let mut found = look(root.to_str().unwrap(), &seen);
        found.sort_by(|a, b| a.name.cmp(&b.name));
        let summary: Vec<_> = found.iter().map(|entry| (entry.name.as_str(), entry.kind, entry.files, entry.bytes)).collect();
        assert_eq!(summary, [("Factorio", SourceKind::Folder, 2, 110), ("Tyrian 2000 (1999).zip", SourceKind::Archive, 1, 50)]);
        assert_eq!(seen.load(Ordering::Relaxed), 3);

        // Writing into a game folder changes its signature, which is how a rescan notices.
        let before = found[0].signature.clone();
        std::fs::write(root.join("Factorio/new.dat"), b"more").unwrap();
        assert_ne!(look(root.to_str().unwrap(), &seen).iter().find(|entry| entry.name == "Factorio").unwrap().signature, before);

        assert_eq!(find_executable(&root.join("Factorio"), "Factorio").as_deref(), Some("bin/x64/factorio.exe"));
        std::fs::create_dir(root.join("Empty")).unwrap();
        assert_eq!(find_executable(&root.join("Empty"), "Empty"), None);
    }
}
