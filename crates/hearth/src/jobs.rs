//! Housekeeping that runs once a minute for as long as the Hearth is up.

use std::time::Duration;

use hearth_api::play::SESSION_TIMEOUT;

use crate::{db::now, routes::people::ONLINE_SECONDS, state::AppState};

const SAMPLE_SECONDS: i64 = 600;
/// Removed accounts, and with them their saves, are kept this long.
pub const REMOVED_KEPT_SECONDS: i64 = 30 * 86400;

pub async fn janitor(state: AppState) {
    loop {
        if state.setup_complete()
            && let Err(err) = tidy(&state).await
        {
            tracing::warn!(error = %err, "housekeeping failed, trying again in a minute");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
            _ = state.shutdown.cancelled() => return,
        }
    }
}

pub async fn tidy(state: &AppState) -> Result<(), sqlx::Error> {
    let now = now();
    let db = &state.db;

    // One sample per ten minutes feeds the "People online" chart on the Overview.
    let recent: Vec<i64> = sqlx::query_scalar("SELECT id FROM users WHERE removed_at IS NULL AND last_seen_at > ?").bind(now - ONLINE_SECONDS).fetch_all(db).await?;
    let connected = state.hub.online();
    let people = (recent.len() + connected.iter().filter(|id| !recent.contains(id)).count()) as i64;
    sqlx::query("INSERT INTO online_samples (at, people) VALUES (?1, ?2) ON CONFLICT (at) DO UPDATE SET people = MAX(people, ?2)")
        .bind(now - now % SAMPLE_SECONDS)
        .bind(people)
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM online_samples WHERE at < ?").bind(now - 2 * 86400).execute(db).await?;

    // A game whose heartbeat stopped ended at its last beat. Friends see that person leave the game.
    let stale: Vec<i64> = sqlx::query_scalar("SELECT DISTINCT user_id FROM play_sessions WHERE ended_at IS NULL AND last_beat_at < ?").bind(now - SESSION_TIMEOUT).fetch_all(db).await?;
    sqlx::query("UPDATE play_sessions SET ended_at = last_beat_at WHERE ended_at IS NULL AND last_beat_at < ?").bind(now - SESSION_TIMEOUT).execute(db).await?;
    for user_id in stale {
        crate::routes::social::announce(state, user_id).await;
    }

    crate::routes::multiplayer::announce_closed(state, state.multiplayer.close_idle_p2p());
    state.multiplayer.sample_relay_rate();
    crate::routes::community::remind(state).await?;
    if let Err(err) = crate::backup::nightly(state).await {
        tracing::warn!(error = %err.message, "the nightly backup failed");
    }

    let expired: Vec<i64> = sqlx::query_scalar("DELETE FROM sessions WHERE expires_at IS NOT NULL AND expires_at <= ? RETURNING id").bind(now).fetch_all(db).await?;
    expired.into_iter().for_each(|id| state.hub.kick_session(id));
    sqlx::query("DELETE FROM password_resets WHERE expires_at <= ? OR used_at IS NOT NULL").bind(now).execute(db).await?;
    sqlx::query("DELETE FROM download_queue WHERE state IN ('done', 'failed') AND queued_at < ?").bind(now - 7 * 86400).execute(db).await?;

    let gone: Vec<(i64, String)> = sqlx::query_as("SELECT id, username FROM users WHERE removed_at IS NOT NULL AND removed_at < ?").bind(now - REMOVED_KEPT_SECONDS).fetch_all(db).await?;
    for (id, username) in gone {
        sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(db).await?;
        let saves = state.paths.dir("saves").join(id.to_string());
        if saves.exists()
            && let Err(err) = tokio::fs::remove_dir_all(&saves).await
        {
            tracing::warn!(user = id, error = %err, "could not delete the saves of a removed account");
        }
        crate::db::log_activity(db, format!("The account {username} and its saves were deleted, 30 days after it was removed")).await;
    }
    Ok(())
}
