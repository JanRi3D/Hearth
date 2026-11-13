//! Database backups: one consistent copy of `hearth.db` per night in `<data>/backups`, and
//! one whenever an admin presses "Back up now".

use std::path::PathBuf;

use hearth_api::admin::Backup;

use crate::{
    db::{self, now},
    error::ApiResult,
    state::AppState,
};

/// Backups kept. Older ones are deleted.
const KEPT: usize = 14;

fn stamp(unix: i64, offset_seconds: i32) -> String {
    let offset = time::UtcOffset::from_whole_seconds(offset_seconds).unwrap_or(time::UtcOffset::UTC);
    let at = time::OffsetDateTime::from_unix_timestamp(unix).unwrap_or(time::OffsetDateTime::UNIX_EPOCH).to_offset(offset);
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", at.year(), at.month() as u8, at.day(), at.hour(), at.minute(), at.second())
}

/// Writes a backup and returns it. SQLite copies the database in one consistent piece, so
/// the Hearth keeps running while it does.
pub async fn run(state: &AppState) -> ApiResult<Backup> {
    let at = now();
    let folder = state.paths.dir("backups");
    let name = format!("hearth-{}.db", stamp(at, state.opts.utc_offset));
    let file = folder.join(&name);
    let _ = tokio::fs::remove_file(&file).await;
    // The path is the Hearth's own. Quotes are doubled all the same, as SQL wants them.
    let target = file.display().to_string().replace('\'', "''");
    sqlx::query(sqlx::AssertSqlSafe(format!("VACUUM INTO '{target}'"))).execute(&state.db).await?;
    let bytes = tokio::fs::metadata(&file).await?.len();
    db::meta_set(&state.db, "last_backup_at", &at.to_string()).await?;

    let mut old = list(&folder);
    old.sort();
    for stale in old.iter().rev().skip(KEPT) {
        let _ = tokio::fs::remove_file(stale).await;
    }
    db::log_activity(&state.db, format!("Database backup finished, {:.1} MB", bytes as f64 / (1024.0 * 1024.0))).await;
    Ok(Backup { at, bytes, file: name })
}

fn list(folder: &std::path::Path) -> Vec<PathBuf> {
    std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("hearth-") && name.ends_with(".db")))
        .collect()
}

/// The newest backup on disk.
pub async fn latest(state: &AppState) -> ApiResult<Option<Backup>> {
    let Some(at) = db::meta_get(&state.db, "last_backup_at").await?.and_then(|at| at.parse::<i64>().ok()) else { return Ok(None) };
    let mut files = list(&state.paths.dir("backups"));
    files.sort();
    Ok(files.last().and_then(|file| {
        let bytes = std::fs::metadata(file).ok()?.len();
        Some(Backup { at, bytes, file: file.file_name()?.to_str()?.to_string() })
    }))
}

/// Called once a minute. Runs the nightly backup when its time has come and it has not run
/// yet today, which also catches up if the Hearth was switched off at that hour.
pub async fn nightly(state: &AppState) -> ApiResult<()> {
    let settings = state.settings();
    let Some((hour, minute)) = crate::settings::parse_time(&settings.backup_time).filter(|_| settings.backup_nightly) else { return Ok(()) };
    let now = now();
    let offset = state.opts.utc_offset as i64;
    let local = now + offset;
    let due = local - local.rem_euclid(86400) + hour as i64 * 3600 + minute as i64 * 60 - offset;
    let last = db::meta_get(&state.db, "last_backup_at").await?.and_then(|at| at.parse::<i64>().ok()).unwrap_or(0);
    if now >= due && last < due {
        run(state).await?;
        // The same quiet hour is a good moment for a full look at the games folders.
        state.library.request_scan();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_sort_by_time_and_follow_the_local_clock() {
        assert_eq!(stamp(1_794_500_000, 0), "20261112-161320");
        assert_eq!(stamp(1_794_500_000, 3600), "20261112-171320");
        assert!(stamp(1_794_500_000, 0) < stamp(1_794_586_400, 0));
    }
}
