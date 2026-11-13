//! Manifests: the hashed file list of one version. Building one reads every byte of a game,
//! so it happens in the background, one game at a time, and reuses hashes of files that did
//! not change.

use std::{
    collections::HashMap,
    io::Read,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use hearth_api::library::{CHUNK_SIZE, Diff, DiffFile};
use sqlx::SqlitePool;

use crate::{db::now, state::AppState, tls::hex};

/// One file of a manifest as it is stored.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub hash: Vec<u8>,
    /// 32 bytes per chunk.
    pub chunks: Vec<u8>,
}

/// Where a manifest's file lives on disk. `None` for a path that would leave the root, here
/// or on a player's PC: manifest paths use forward slashes, and a backslash or a colon in a
/// name means another place on Windows. The answer is the same on every system.
pub fn resolve(root: &Path, relative: &str) -> Option<PathBuf> {
    if root.is_file() {
        // A single archive or installer: the manifest has one entry, the file itself.
        return Some(root.to_path_buf());
    }
    if relative.contains(['\\', ':']) {
        return None;
    }
    let relative = Path::new(relative);
    relative.components().all(|part| matches!(part, Component::Normal(_))).then(|| root.join(relative))
}

fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map_or(0, |since| since.as_secs() as i64)
}

/// Every regular file under `root`, as (relative path with forward slashes, size, mtime),
/// sorted by path. Links are not followed, so a manifest never reaches outside its folder.
pub fn list_files(root: &Path) -> std::io::Result<Vec<(String, u64, i64)>> {
    let meta = std::fs::symlink_metadata(root)?;
    if meta.is_file() {
        let name = root.file_name().and_then(|name| name.to_str()).unwrap_or("file").to_string();
        return Ok(vec![(name, meta.len(), mtime_of(&meta))]);
    }
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                let meta = entry.metadata()?;
                let relative = entry.path().strip_prefix(root).unwrap_or(&entry.path()).to_path_buf();
                // A name that is not valid Unicode cannot travel in JSON. Such files are left out.
                // The names are joined with forward slashes whatever this system puts between
                // them. Where a backslash is part of a name, it stays one.
                let names: Option<Vec<&str>> = relative.components().map(|part| part.as_os_str().to_str()).collect();
                if let Some(names) = names {
                    files.push((names.join("/"), meta.len(), mtime_of(&meta)));
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

/// BLAKE3 of a whole file and of each of its chunks.
pub fn hash_file(file: &Path) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let mut input = std::fs::File::open(file)?;
    let mut whole = blake3::Hasher::new();
    let mut chunks = Vec::new();
    let mut buffer = vec![0u8; CHUNK_SIZE as usize];
    loop {
        // Fill the buffer completely, so chunk borders do not depend on how the disk hands out bytes.
        let mut filled = 0;
        while filled < buffer.len() {
            match input.read(&mut buffer[filled..])? {
                0 => break,
                read => filled += read,
            }
        }
        if filled == 0 {
            break;
        }
        whole.update(&buffer[..filled]);
        chunks.extend_from_slice(blake3::hash(&buffer[..filled]).as_bytes());
        if filled < buffer.len() {
            break;
        }
    }
    Ok((whole.finalize().as_bytes().to_vec(), chunks))
}

/// Hashes everything under `root`. A file whose size and change time match `known` keeps its hashes.
pub fn build(root: &Path, known: &HashMap<String, FileEntry>) -> std::io::Result<Vec<FileEntry>> {
    list_files(root)?
        .into_iter()
        .filter_map(|(path, size, mtime)| {
            // ponytail: a file whose name has a backslash or a colon is not offered at all,
            // because a Windows PC cannot store it. Offer it to other systems once Bonfire
            // runs on them.
            let Some(file) = resolve(root, &path) else {
                tracing::warn!(%path, "left out of the download, the name cannot be used on Windows");
                return None;
            };
            Some(match known.get(&path) {
                Some(same) if same.size == size && same.mtime == mtime => Ok(same.clone()),
                _ => hash_file(&file).map(|(hash, chunks)| FileEntry { path, size, mtime, hash, chunks }),
            })
        })
        .collect()
}

pub async fn load(db: &SqlitePool, manifest_id: i64) -> Result<Vec<FileEntry>, sqlx::Error> {
    type Row = (String, i64, i64, Vec<u8>, Vec<u8>);
    let rows: Vec<Row> =
        sqlx::query_as("SELECT path, size, mtime, hash, chunks FROM manifest_files WHERE manifest_id = ? ORDER BY idx")
            .bind(manifest_id)
            .fetch_all(db)
            .await?;
    Ok(rows.into_iter().map(|(path, size, mtime, hash, chunks)| FileEntry { path, size: size as u64, mtime, hash, chunks }).collect())
}

/// Marks a manifest as needing a rebuild and wakes the hasher.
pub async fn invalidate(state: &AppState, manifest_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE manifests SET state = 'pending' WHERE id = ?").bind(manifest_id).execute(&state.db).await?;
    state.library.hash_wake.notify_one();
    Ok(())
}

/// Runs until shutdown: builds every pending manifest, oldest first.
pub async fn hasher(state: AppState) {
    loop {
        loop {
            let next: Result<Option<(i64, String)>, _> =
                sqlx::query_as("SELECT id, root FROM manifests WHERE state = 'pending' ORDER BY id LIMIT 1").fetch_optional(&state.db).await;
            match next {
                Ok(Some((id, root))) => build_one(&state, id, root).await,
                Ok(None) => break,
                Err(err) => {
                    tracing::error!(error = %err, "could not look for manifests to build");
                    break;
                }
            }
            if state.shutdown.is_cancelled() {
                return;
            }
        }
        tokio::select! {
            _ = state.library.hash_wake.notified() => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
            _ = state.shutdown.cancelled() => return,
        }
    }
}

async fn build_one(state: &AppState, id: i64, root: String) {
    let known: HashMap<String, FileEntry> = match load(&state.db, id).await {
        Ok(files) => files.into_iter().map(|file| (file.path.clone(), file)).collect(),
        Err(_) => HashMap::new(),
    };
    let root_path = PathBuf::from(&root);
    let built = tokio::task::spawn_blocking(move || build(&root_path, &known)).await;
    let files = match built {
        Ok(Ok(files)) => files,
        Ok(Err(err)) => return fail(state, id, &root, &err.to_string()).await,
        Err(err) => return fail(state, id, &root, &err.to_string()).await,
    };
    let stored: Result<(), sqlx::Error> = async {
        let mut tx = state.db.begin().await?;
        sqlx::query("DELETE FROM manifest_files WHERE manifest_id = ?").bind(id).execute(&mut *tx).await?;
        for (index, file) in files.iter().enumerate() {
            sqlx::query("INSERT INTO manifest_files (manifest_id, idx, path, size, mtime, hash, chunks) VALUES (?, ?, ?, ?, ?, ?, ?)")
                .bind(id)
                .bind(index as i64)
                .bind(&file.path)
                .bind(file.size as i64)
                .bind(file.mtime)
                .bind(&file.hash)
                .bind(&file.chunks)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE manifests SET state = 'ready', bytes = ?, files = ?, ready_at = ? WHERE id = ?")
            .bind(files.iter().map(|file| file.size as i64).sum::<i64>())
            .bind(files.len() as i64)
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE games SET updated_at = ? WHERE id IN (SELECT game_id FROM versions WHERE manifest_id = ?)")
            .bind(now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    match stored {
        Ok(()) => {
            tracing::info!(manifest = id, files = files.len(), %root, "manifest ready");
            if let Err(err) = note_update_size(state, id, &files).await {
                tracing::warn!(manifest = id, error = %err, "could not work out the update size");
            }
        }
        Err(err) => fail(state, id, &root, &err.to_string()).await,
    }
}

/// Stores how much a client coming from the version before has to download, and tells the
/// activity log that the version is out.
async fn note_update_size(state: &AppState, manifest_id: i64, files: &[FileEntry]) -> Result<(), sqlx::Error> {
    let versions: Vec<(i64, i64, String, String)> =
        sqlx::query_as("SELECT v.id, v.game_id, v.name, g.title FROM versions v JOIN games g ON g.id = v.game_id WHERE v.manifest_id = ?")
            .bind(manifest_id)
            .fetch_all(&state.db)
            .await?;
    for (version_id, game_id, name, title) in versions {
        let before: Option<(i64, i64)> = sqlx::query_as(
            "SELECT v.id, v.manifest_id FROM versions v JOIN manifests m ON m.id = v.manifest_id
             WHERE v.game_id = ? AND v.id < ? AND m.state = 'ready' ORDER BY v.id DESC LIMIT 1",
        )
        .bind(game_id)
        .bind(version_id)
        .fetch_optional(&state.db)
        .await?;
        let Some((before_id, before_manifest)) = before else { continue };
        let update = diff(before_id, version_id, &load(&state.db, before_manifest).await?, files);
        let known: Option<i64> = sqlx::query_scalar("SELECT update_bytes FROM versions WHERE id = ?").bind(version_id).fetch_one(&state.db).await?;
        sqlx::query("UPDATE versions SET update_bytes = ? WHERE id = ?").bind(update.bytes as i64).bind(version_id).execute(&state.db).await?;
        if known.is_none() {
            let megabytes = update.bytes as f64 / (1024.0 * 1024.0);
            crate::db::log_activity(&state.db, format!("{title} {name} published, {megabytes:.0} MB update")).await;
            let hearth = state.settings().name.clone();
            crate::routes::social::notify(
                state,
                crate::routes::social::To::Everyone(None),
                hearth_api::social::NotificationKind::Update,
                format!("{title} {name} is on {hearth}"),
                format!("A {megabytes:.0} MB update."),
                &[("game_id", game_id), ("version_id", version_id)],
            )
            .await;
        }
    }
    Ok(())
}

async fn fail(state: &AppState, id: i64, root: &str, why: &str) {
    tracing::warn!(manifest = id, %root, error = why, "could not build the manifest");
    let _ = sqlx::query("UPDATE manifests SET state = 'failed' WHERE id = ?").bind(id).execute(&state.db).await;
}

/// What a client holding `from` has to change to hold `to`.
pub fn diff(from_id: i64, to_id: i64, from: &[FileEntry], to: &[FileEntry]) -> Diff {
    let old: HashMap<&str, &FileEntry> = from.iter().map(|file| (file.path.as_str(), file)).collect();
    let kept: std::collections::HashSet<&str> = to.iter().map(|file| file.path.as_str()).collect();
    let mut fetch = Vec::new();
    for (index, file) in to.iter().enumerate() {
        let (chunks, bytes) = match old.get(file.path.as_str()) {
            Some(same) if same.hash == file.hash => continue,
            Some(before) => {
                // Chunks are compared by position: a patch that rewrites part of a big file
                // leaves the other chunks alone.
                let changed: Vec<u32> = file
                    .chunks
                    .chunks(32)
                    .enumerate()
                    .filter(|(i, hash)| before.chunks.get(i * 32..i * 32 + 32) != Some(*hash))
                    .map(|(i, _)| i as u32)
                    .collect();
                let bytes = changed.iter().map(|&i| chunk_len(file.size, i)).sum();
                (Some(changed), bytes)
            }
            None => (None, file.size),
        };
        fetch.push(DiffFile { index: index as u32, path: file.path.clone(), size: file.size, chunks, bytes });
    }
    Diff {
        from: from_id,
        to: to_id,
        delete: from.iter().filter(|file| !kept.contains(file.path.as_str())).map(|file| file.path.clone()).collect(),
        bytes: fetch.iter().map(|file| file.bytes).sum(),
        fetch,
    }
}

/// Length of chunk `index` of a file of `size` bytes.
pub fn chunk_len(size: u64, index: u32) -> u64 {
    size.saturating_sub(index as u64 * CHUNK_SIZE).min(CHUNK_SIZE)
}

pub fn chunks_hex(chunks: &[u8]) -> String {
    hex(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64, chunk_marks: &[u8]) -> FileEntry {
        let chunks: Vec<u8> = chunk_marks.iter().flat_map(|mark| [*mark; 32]).collect();
        FileEntry { path: path.into(), size, mtime: 0, hash: blake3::hash(&chunks).as_bytes().to_vec(), chunks }
    }

    #[test]
    fn hashes_files_in_chunks_and_reuses_what_did_not_change() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Game");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let big = vec![7u8; CHUNK_SIZE as usize + 10];
        std::fs::write(root.join("bin/game.exe"), &big).unwrap();
        std::fs::write(root.join("readme.txt"), b"hello").unwrap();

        let files = build(&root, &HashMap::new()).unwrap();
        assert_eq!(files.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["bin/game.exe", "readme.txt"]);
        assert_eq!(files[0].chunks.len(), 64, "two chunks: one full, one of 10 bytes");
        assert_eq!(files[0].hash, blake3::hash(&big).as_bytes());
        assert_eq!(&files[0].chunks[32..], blake3::hash(&[7u8; 10]).as_bytes());
        assert_eq!(files[1].hash, blake3::hash(b"hello").as_bytes());

        // A known file with the same size and time is not read again: its made-up hash survives.
        let mut known: HashMap<String, FileEntry> = files.iter().map(|file| (file.path.clone(), file.clone())).collect();
        known.get_mut("readme.txt").unwrap().hash = vec![1; 32];
        assert_eq!(build(&root, &known).unwrap()[1].hash, vec![1; 32]);

        // A single file is a manifest of one.
        let single = build(&root.join("readme.txt"), &HashMap::new()).unwrap();
        assert_eq!((single.len(), single[0].path.as_str()), (1, "readme.txt"));
    }

    #[test]
    fn paths_cannot_leave_the_game_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve(dir.path(), "bin/game.exe").is_some());
        for bad in ["../secret", "bin/../../secret", "/etc/passwd", "C:\\Windows\\win.ini", "..\\..\\secret", "bin\\game.exe", "notes.txt:hidden"] {
            assert!(resolve(dir.path(), bad).is_none(), "{bad}");
        }
    }

    /// Only systems where a backslash can be part of a file's name can have such a file.
    #[cfg(unix)]
    #[test]
    fn a_name_windows_cannot_store_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fine.txt"), b"a").unwrap();
        std::fs::write(dir.path().join("odd\\name.txt"), b"b").unwrap();
        let listed: Vec<String> = list_files(dir.path()).unwrap().into_iter().map(|(path, _, _)| path).collect();
        assert_eq!(listed, ["fine.txt", "odd\\name.txt"], "the backslash is part of the name here, not a separator");
        let offered: Vec<String> = build(dir.path(), &HashMap::new()).unwrap().into_iter().map(|file| file.path).collect();
        assert_eq!(offered, ["fine.txt"]);
    }

    #[test]
    fn a_diff_lists_only_what_changed() {
        let size = 2 * CHUNK_SIZE + 100;
        let from = [entry("data.pak", size, &[1, 2, 3]), entry("old.dll", 50, &[4]), entry("same.txt", 5, &[5])];
        let to = [entry("data.pak", size, &[1, 9, 3]), entry("new.dll", 70, &[6]), entry("same.txt", 5, &[5])];
        let diff = diff(1, 2, &from, &to);
        assert_eq!(diff.delete, ["old.dll"]);
        assert_eq!(diff.fetch.len(), 2, "the unchanged file is not fetched");
        assert_eq!((diff.fetch[0].path.as_str(), diff.fetch[0].chunks.clone(), diff.fetch[0].bytes), ("data.pak", Some(vec![1]), CHUNK_SIZE));
        assert_eq!((diff.fetch[1].path.as_str(), diff.fetch[1].chunks.clone(), diff.fetch[1].bytes), ("new.dll", None, 70));
        assert_eq!(diff.bytes, CHUNK_SIZE + 70);
        assert_eq!(chunk_len(size, 2), 100);
    }
}
