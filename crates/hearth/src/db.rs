//! The SQLite database: one file in the data folder, migrations embedded in the binary.

use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn open(file: &Path) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(file)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(options).await?;
    MIGRATIONS.run(&pool).await?;
    Ok(pool)
}

/// Unix seconds, UTC.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub async fn meta_get(db: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT value FROM meta WHERE key = ?").bind(key).fetch_optional(db).await
}

pub async fn meta_set(db: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(db)
        .await?;
    Ok(())
}

/// Adds a line to "Recent activity" on the Overview.
pub async fn log_activity(db: &SqlitePool, text: impl Into<String>) {
    let text = text.into();
    tracing::info!(activity = %text);
    if let Err(err) = sqlx::query("INSERT INTO activity (at, text) VALUES (?, ?)").bind(now()).bind(&text).execute(db).await {
        tracing::warn!(error = %err, "could not record activity");
    }
}
