//! The library as a client sees it: games, manifests, and the bytes themselves.

use std::{io::SeekFrom, path::Path as FsPath};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use hearth_api::{
    ErrorCode,
    library::{CHUNK_SIZE, Diff, Game, GameSummary, Manifest, ManifestFile},
};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{
    auth::Auth,
    error::{ApiError, ApiResult},
    library::{self, manifest},
    state::AppState,
    tls::hex,
};

pub async fn list(State(state): State<AppState>, _auth: Auth) -> ApiResult<Json<Vec<GameSummary>>> {
    Ok(Json(library::load_games(&state.db, false).await?))
}

pub async fn detail(State(state): State<AppState>, auth: Auth, Path(id): Path<i64>) -> ApiResult<Json<Game>> {
    match library::load_game(&state.db, id).await? {
        // A hidden game does not exist as far as players can tell.
        Some(game) if game.summary.visible || auth.is_admin() => Ok(Json(game)),
        _ => Err(ApiError::not_found("This Hearth has no such game.")),
    }
}

/// A version the caller may download, with where its files are.
struct Ready {
    game_id: i64,
    manifest_id: i64,
    root: String,
}

async fn ready_version(state: &AppState, auth: &Auth, version_id: i64) -> ApiResult<Ready> {
    type Row = (i64, bool, Option<i64>, Option<String>, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT g.id, g.visible, m.id, m.root, m.state FROM versions v JOIN games g ON g.id = v.game_id
         LEFT JOIN manifests m ON m.id = v.manifest_id WHERE v.id = ?",
    )
    .bind(version_id)
    .fetch_optional(&state.db)
    .await?;
    match row {
        Some((game_id, visible, Some(manifest_id), Some(root), Some(state))) if visible || auth.is_admin() => match state.as_str() {
            "ready" => Ok(Ready { game_id, manifest_id, root }),
            "pending" => Err(ApiError::conflict("The Hearth is still preparing this version. Try again in a moment.")),
            _ => Err(ApiError::conflict("The files of this version are not on the Hearth right now. Its admin can rescan the library.")),
        },
        _ => Err(ApiError::not_found("This Hearth has no such version.")),
    }
}

pub async fn version_manifest(State(state): State<AppState>, auth: Auth, Path(version_id): Path<i64>) -> ApiResult<Json<Manifest>> {
    let version = ready_version(&state, &auth, version_id).await?;
    let files = manifest::load(&state.db, version.manifest_id).await?;
    Ok(Json(Manifest {
        version_id,
        game_id: version.game_id,
        chunk_size: CHUNK_SIZE,
        bytes: files.iter().map(|file| file.size).sum(),
        files: files
            .into_iter()
            .map(|file| ManifestFile { path: file.path, size: file.size, hash: hex(&file.hash), chunks: hex(&file.chunks) })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct DiffQuery {
    from: i64,
}

pub async fn version_diff(
    State(state): State<AppState>,
    auth: Auth,
    Path(to): Path<i64>,
    Query(query): Query<DiffQuery>,
) -> ApiResult<Json<Diff>> {
    let (new, old) = (ready_version(&state, &auth, to).await?, ready_version(&state, &auth, query.from).await?);
    if new.game_id != old.game_id {
        return Err(ApiError::bad_request("Those two versions belong to different games."));
    }
    let (from_files, to_files) = (manifest::load(&state.db, old.manifest_id).await?, manifest::load(&state.db, new.manifest_id).await?);
    Ok(Json(manifest::diff(query.from, to, &from_files, &to_files)))
}

/// Sends one file of a version, whole or as the byte range the client asks for. Ranges are
/// what make a download resumable and let a client fetch single chunks.
pub async fn version_file(
    State(state): State<AppState>,
    auth: Auth,
    Path((version_id, index)): Path<(i64, i64)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let version = ready_version(&state, &auth, version_id).await?;
    serve_manifest_file(&state, version.manifest_id, &version.root, index, &headers).await
}

pub async fn serve_manifest_file(state: &AppState, manifest_id: i64, root: &str, index: i64, headers: &HeaderMap) -> ApiResult<Response> {
    let row: Option<(String, i64, i64, Vec<u8>)> =
        sqlx::query_as("SELECT path, size, mtime, hash FROM manifest_files WHERE manifest_id = ? AND idx = ?")
            .bind(manifest_id)
            .bind(index)
            .fetch_optional(&state.db)
            .await?;
    let Some((relative, size, mtime, hash)) = row else {
        return Err(ApiError::not_found("This version has no file with that index."));
    };
    let size = size as u64;
    let path = manifest::resolve(FsPath::new(root), &relative).ok_or_else(|| ApiError::not_found("This version has no such file."))?;

    // If the file is not what was hashed, sending it would hand the client bytes that fail
    // its check. Better to say so and hash again.
    let on_disk = tokio::fs::metadata(&path).await.ok();
    let unchanged = on_disk.as_ref().is_some_and(|meta| {
        let changed = meta.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |since| since.as_secs() as i64);
        meta.is_file() && meta.len() == size && changed == mtime
    });
    if !unchanged {
        manifest::invalidate(state, manifest_id).await?;
        return Err(ApiError::conflict("The files of this version changed on the Hearth. Fetch the manifest again in a moment."));
    }

    let range = match headers.get(header::RANGE).and_then(|value| value.to_str().ok()) {
        None => None,
        Some(value) => Some(parse_range(value, size).ok_or_else(|| {
            ApiError::coded(StatusCode::RANGE_NOT_SATISFIABLE, ErrorCode::BadRequest, format!("That range is outside the file, which has {size} bytes."))
        })?),
    };
    let (start, end) = range.unwrap_or((0, size.saturating_sub(1)));
    let length = if size == 0 { 0 } else { end - start + 1 };

    let mut file = tokio::fs::File::open(&path).await?;
    file.seek(SeekFrom::Start(start)).await?;
    let body = Body::from_stream(ReaderStream::with_capacity(file.take(length), 256 * 1024));
    let mut response = Response::builder()
        .status(if range.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, length)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, format!("\"{}\"", hex(&hash)));
    if range.is_some() {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    Ok(response.body(body).expect("the headers above are valid"))
}

/// Reads `bytes=a-b`, `bytes=a-` or `bytes=-n` into an inclusive (first, last) pair.
/// Several ranges in one request are not supported: a client asks once per range.
pub fn parse_range(value: &str, size: u64) -> Option<(u64, u64)> {
    let spec = value.strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        return None;
    }
    let (first, last) = spec.split_once('-')?;
    let (start, end) = match (first.trim(), last.trim()) {
        ("", suffix) => {
            let suffix: u64 = suffix.parse().ok().filter(|n| *n > 0)?;
            (size.saturating_sub(suffix), size - 1)
        }
        (start, "") => (start.parse().ok()?, size - 1),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(size - 1)),
    };
    (start <= end && start < size).then_some((start, end))
}

/// Covers, heroes and logos, by the file name a game points at.
pub async fn art(State(state): State<AppState>, _auth: Auth, Path(name): Path<String>) -> ApiResult<Response> {
    let plain = !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    let content_type = match name.rsplit('.').next() {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "",
    };
    if !plain || content_type.is_empty() {
        return Err(ApiError::not_found("There is no such picture."));
    }
    let bytes = tokio::fs::read(state.paths.dir("art").join(&name)).await.map_err(|_| ApiError::not_found("There is no such picture."))?;
    Ok(([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "private, max-age=86400")], bytes).into_response())
}

#[cfg(test)]
mod tests {
    use super::parse_range;

    #[test]
    fn ranges_are_read_the_way_http_defines_them() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=500-5000", 1000), Some((500, 999)), "an end past the file is clipped");
        for bad in ["bytes=1000-", "bytes=5-2", "bytes=0-1,5-6", "items=0-1", "bytes=-0", "bytes=a-b"] {
            assert_eq!(parse_range(bad, 1000), None, "{bad}");
        }
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }
}
