//! Looking at the server's own disk: the folder browser and the quick folder scan.
//! Both are reachable only by an admin, or with the setup code before an admin exists.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use hearth_api::settings::{Browse, BrowseDir, FolderScan};

use crate::error::{ApiError, ApiResult};

/// What counts as a game when it is a single file rather than a folder.
pub const GAME_FILE_EXTENSIONS: [&str; 5] = ["zip", "7z", "rar", "exe", "iso"];

pub fn is_game_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| GAME_FILE_EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(known)))
}

/// Lists the folders inside `path`. Never files, and nothing is read from them.
pub fn browse(path: &str) -> ApiResult<Browse> {
    let path = path.trim();
    if path.is_empty() {
        return Ok(Browse { path: String::new(), parent: None, dirs: roots() });
    }
    let dir = Path::new(path);
    if !dir.is_absolute() {
        return Err(ApiError::bad_request("The folder browser needs a full path."));
    }
    let entries = std::fs::read_dir(dir).map_err(|_| ApiError::not_found(format!("{path} cannot be opened.")))?;
    let mut dirs: Vec<BrowseDir> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            // Hidden and system folders only add noise to a picker.
            (!name.starts_with('.') && !name.starts_with('$') && name != "System Volume Information")
                .then(|| BrowseDir { path: entry.path().display().to_string(), name })
        })
        .take(2000)
        .collect();
    dirs.sort_by_key(|dir| dir.name.to_lowercase());
    // Above a drive or "/" sits the list of roots, which the empty path stands for.
    let parent = Some(dir.parent().map(|parent| parent.display().to_string()).unwrap_or_default());
    Ok(Browse { path: dir.display().to_string(), parent, dirs })
}

#[cfg(windows)]
fn roots() -> Vec<BrowseDir> {
    ('A'..='Z')
        .map(|letter| format!("{letter}:\\"))
        .filter(|drive| Path::new(drive).is_dir())
        .map(|drive| BrowseDir { name: drive.clone(), path: drive })
        .collect()
}

#[cfg(not(windows))]
fn roots() -> Vec<BrowseDir> {
    vec![BrowseDir { name: "/".into(), path: "/".into() }]
}

/// Counts what looks like games at the top of a folder and adds up its size, giving up on the
/// size after a few seconds so a huge disk does not stall the wizard.
pub fn scan_summary(path: &str) -> FolderScan {
    let dir = Path::new(path.trim());
    let mut scan =
        FolderScan { path: dir.display().to_string(), exists: dir.is_dir(), readable: false, games: 0, bytes: 0, complete: true };
    if !dir.is_absolute() || !scan.exists {
        scan.exists = false;
        return scan;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return scan };
    scan.readable = true;
    let deadline = Instant::now() + Duration::from_secs(3);
    for entry in entries.flatten() {
        let path = entry.path();
        if is_hidden(&path) {
            continue;
        }
        if path.is_dir() {
            scan.games += 1;
            scan.complete &= add_size(&path, &mut scan.bytes, deadline);
        } else if is_game_file(&path) {
            scan.games += 1;
            scan.bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        }
    }
    scan
}

pub fn is_hidden(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()).is_none_or(|name| name.starts_with('.') || name.starts_with('$'))
}

/// Adds the size of everything under `dir`. Returns false when the deadline cut it short.
fn add_size(dir: &Path, bytes: &mut u64, deadline: Instant) -> bool {
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if Instant::now() > deadline {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            // `file_type` does not follow links, so a link loop cannot trap the walk.
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                Ok(kind) if kind.is_file() => *bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_lists_folders_only_and_scan_counts_games() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("Factorio/bin")).unwrap();
        std::fs::write(root.join("Factorio/bin/factorio.exe"), vec![0u8; 1000]).unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        std::fs::write(root.join("Tyrian 2000.zip"), vec![0u8; 500]).unwrap();
        std::fs::write(root.join("notes.txt"), b"not a game").unwrap();

        let listing = browse(&root.display().to_string()).unwrap();
        let names: Vec<_> = listing.dirs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Factorio"], "files and hidden folders stay out of the browser");
        assert!(listing.parent.is_some());

        let scan = scan_summary(&root.display().to_string());
        assert!(scan.exists && scan.readable && scan.complete);
        assert_eq!((scan.games, scan.bytes), (2, 1500));

        assert!(browse("relative/path").is_err());
        assert!(!scan_summary(&root.join("missing").display().to_string()).exists);
        assert!(!browse("").unwrap().dirs.is_empty(), "the top level lists the drives or /");
    }
}
