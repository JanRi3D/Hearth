//! A small WebDAV server at `/dav`, the storage endpoint rclone talks to. It serves two
//! trees per person: the saves of each game, and the shared worlds they are a member of.
//! The layout is described in `hearth_api::saves`.
//!
//! Only what rclone needs is here: PROPFIND, GET, PUT, DELETE, MKCOL and MOVE. Every path
//! is rebuilt from checked segments under a folder the Hearth chose, so nothing a client
//! sends can point outside its own area.

use std::{
    io::SeekFrom,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::Body,
    extract::{FromRequestParts, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use hearth_api::ErrorCode;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use crate::{
    auth::Auth,
    error::{ApiError, ApiResult},
    routes::{saves, worlds},
    state::AppState,
    storage,
};

const ALLOW: &str = "OPTIONS, PROPFIND, GET, HEAD, PUT, DELETE, MKCOL, MOVE";
/// A shared world may grow to this size. ponytail: one fixed ceiling for every world; make it
/// a setting if a game's worlds outgrow it.
pub const WORLD_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new().route("/dav", any(handle)).route("/dav/", any(handle)).route("/dav/{*path}", any(handle))
}

/// Where a path leads.
pub enum Node {
    /// A folder that only exists as a name, with the names inside it.
    Listing(Vec<String>),
    /// A file or folder on disk. `area` is set where the caller may write.
    Disk { path: PathBuf, area: Option<Area> },
}

/// A writable area and how many bytes it may hold.
#[derive(Clone)]
pub struct Area {
    pub root: PathBuf,
    pub limit: u64,
}

fn not_found() -> ApiError {
    ApiError::not_found("There is nothing at this path.")
}

fn read_only() -> ApiError {
    ApiError::forbidden("This folder is read-only. Write into upload/ and commit.")
}

/// Splits a request path into decoded segments, refusing anything that is not a plain name.
fn segments(path: &str) -> ApiResult<Vec<String>> {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let name = percent_decode(segment).ok_or_else(|| ApiError::bad_request("The path is not valid UTF-8."))?;
            let plain = !name.is_empty()
                && name != "."
                && name != ".."
                && name.len() <= 255
                && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'));
            plain.then_some(name).ok_or_else(|| ApiError::bad_request("A file or folder name in the path is not allowed."))
        })
        .collect()
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn join(base: PathBuf, rest: &[String]) -> PathBuf {
    rest.iter().fold(base, |path, name| path.join(name))
}

async fn resolve(state: &AppState, auth: &Auth, path: &[String]) -> ApiResult<Node> {
    let names = |list: &[&str]| Node::Listing(list.iter().map(|name| name.to_string()).collect());
    match path {
        [] => Ok(names(&["saves", "worlds"])),
        [root] if root == "saves" => Ok(Node::Listing(saves::games_with_saves(state, auth.user_id).await?.iter().map(i64::to_string).collect())),
        [root, game, rest @ ..] if root == "saves" => {
            let game_id: i64 = game.parse().map_err(|_| not_found())?;
            saves::check_game(state, auth, game_id).await?;
            let store = saves::store(state, auth.user_id, game_id);
            let head = saves::head_number(state, auth.user_id, game_id).await?;
            match rest {
                [] => Ok(names(if head.is_some() { &["upload", "current", "rev"] } else { &["upload", "rev"] })),
                [area, rest @ ..] if area == "upload" => {
                    Ok(Node::Disk { path: join(store.upload(), rest), area: Some(Area { root: store.upload(), limit: saves::quota_bytes(state) }) })
                }
                [area, rest @ ..] if area == "current" => Ok(Node::Disk { path: join(store.revision(head.ok_or_else(not_found)?), rest), area: None }),
                [area] if area == "rev" => Ok(Node::Listing(saves::revision_numbers(state, auth.user_id, game_id).await?.iter().map(u32::to_string).collect())),
                [area, number, rest @ ..] if area == "rev" => {
                    let number: u32 = number.parse().map_err(|_| not_found())?;
                    if !saves::revision_numbers(state, auth.user_id, game_id).await?.contains(&number) {
                        return Err(not_found());
                    }
                    Ok(Node::Disk { path: join(store.revision(number), rest), area: None })
                }
                _ => Err(not_found()),
            }
        }
        [root] if root == "worlds" => Ok(Node::Listing(worlds::member_of(state, auth.user_id).await?.iter().map(i64::to_string).collect())),
        [root, world, rest @ ..] if root == "worlds" => {
            let world_id: i64 = world.parse().map_err(|_| not_found())?;
            let access = worlds::access(state, world_id, auth.user_id).await?.ok_or_else(not_found)?;
            let store = worlds::store(state, world_id);
            match rest {
                [] => Ok(names(if access.hosting { &["current", "upload"] } else { &["current"] })),
                [area] if area == "current" && access.revision == 0 => Ok(Node::Listing(Vec::new())),
                [area, rest @ ..] if area == "current" && access.revision > 0 => Ok(Node::Disk { path: join(store.revision(access.revision), rest), area: None }),
                [area, rest @ ..] if area == "upload" => {
                    if !access.hosting {
                        return Err(ApiError::forbidden("Only the person hosting this world right now can write to it."));
                    }
                    Ok(Node::Disk { path: join(store.upload(), rest), area: Some(Area { root: store.upload(), limit: WORLD_LIMIT }) })
                }
                _ => Err(not_found()),
            }
        }
        _ => Err(not_found()),
    }
}

/// rclone can send the token as a bearer token. Other WebDAV clients only know usernames
/// and passwords, so the token is also accepted as the password of basic auth.
fn accept_basic_auth(parts: &mut Parts) {
    let token = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(|encoded| STANDARD.decode(encoded.trim()).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|pair| pair.split_once(':').map(|(_, password)| password.to_string()));
    if let Some(token) = token
        && let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}"))
    {
        parts.headers.insert(header::AUTHORIZATION, value);
    }
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let (mut parts, body) = request.into_parts();
    if parts.method == Method::OPTIONS {
        return (StatusCode::OK, [("dav", "1"), ("allow", ALLOW)]).into_response();
    }
    accept_basic_auth(&mut parts);
    let auth = match Auth::from_request_parts(&mut parts, &state).await {
        Ok(auth) => auth,
        Err(err) if err.status == StatusCode::UNAUTHORIZED => {
            return ([(header::WWW_AUTHENTICATE, "Basic realm=\"Hearth\"")], err).into_response();
        }
        Err(err) => return err.into_response(),
    };
    match serve(&state, &auth, &parts, body).await {
        Ok(response) => response,
        Err(err) => err.into_response(),
    }
}

async fn serve(state: &AppState, auth: &Auth, parts: &Parts, body: Body) -> ApiResult<Response> {
    let path = segments(parts.uri.path().strip_prefix("/dav").unwrap_or_default())?;
    let node = resolve(state, auth, &path).await?;
    match parts.method.as_str() {
        "PROPFIND" => propfind(&path, &node, parts.headers.get("depth").and_then(|depth| depth.to_str().ok()).unwrap_or("1")).await,
        "GET" | "HEAD" => get(&node, &parts.headers, parts.method == Method::HEAD).await,
        "PUT" => put(&node, &parts.headers, body).await,
        "MKCOL" => {
            let (path, _) = writable(&node)?;
            if tokio::fs::metadata(path).await.is_ok() {
                // rclone takes 405 to mean "it is there already", which is what it wanted.
                return Ok(StatusCode::METHOD_NOT_ALLOWED.into_response());
            }
            tokio::fs::create_dir_all(path).await?;
            Ok(StatusCode::CREATED.into_response())
        }
        "DELETE" => {
            let (path, _) = writable(&node)?;
            match tokio::fs::metadata(path).await {
                Ok(meta) if meta.is_dir() => tokio::fs::remove_dir_all(path).await?,
                Ok(_) => tokio::fs::remove_file(path).await?,
                Err(_) => return Err(not_found()),
            }
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        "MOVE" => {
            let (from, area) = writable(&node)?;
            let destination = parts.headers.get("destination").and_then(|value| value.to_str().ok()).ok_or_else(|| ApiError::bad_request("MOVE needs a Destination header."))?;
            // The header holds a whole address or just a path. Only the path matters.
            let target_path = destination.split_once("://").map_or(destination, |(_, rest)| rest.find('/').map_or("", |at| &rest[at..]));
            let target = resolve(state, auth, &segments(target_path.strip_prefix("/dav").ok_or_else(|| ApiError::bad_request("The destination is not on this Hearth."))?)?).await?;
            let (to, to_area) = writable(&target)?;
            if to_area.root != area.root {
                return Err(ApiError::forbidden("Files can only be moved within one upload folder."));
            }
            let existed = tokio::fs::metadata(to).await.is_ok();
            if existed && parts.headers.get("overwrite").is_some_and(|value| value == "F") {
                return Err(ApiError::coded(StatusCode::PRECONDITION_FAILED, ErrorCode::Conflict, "The destination exists already."));
            }
            if let Some(parent) = to.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if existed && tokio::fs::metadata(to).await.is_ok_and(|meta| meta.is_dir()) {
                tokio::fs::remove_dir_all(to).await?;
            }
            tokio::fs::rename(from, to).await.map_err(|_| not_found())?;
            Ok(if existed { StatusCode::NO_CONTENT } else { StatusCode::CREATED }.into_response())
        }
        _ => Ok((StatusCode::METHOD_NOT_ALLOWED, [("allow", ALLOW)]).into_response()),
    }
}

fn writable(node: &Node) -> ApiResult<(&Path, &Area)> {
    match node {
        Node::Disk { path, area: Some(area) } => Ok((path, area)),
        _ => Err(read_only()),
    }
}

// ---------- reading ----------

fn http_date(time: SystemTime) -> String {
    httpdate::fmt_http_date(time)
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// One `<D:response>` of a PROPFIND answer.
fn entry(href: &str, name: &str, meta: Option<&std::fs::Metadata>, is_dir: bool) -> String {
    let modified = meta.and_then(|meta| meta.modified().ok()).unwrap_or(UNIX_EPOCH);
    let mut props = format!("<D:displayname>{}</D:displayname><D:getlastmodified>{}</D:getlastmodified>", xml_escape(name), http_date(modified));
    if is_dir {
        props.push_str("<D:resourcetype><D:collection/></D:resourcetype>");
    } else {
        let size = meta.map_or(0, |meta| meta.len());
        let stamp = modified.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        props.push_str(&format!("<D:resourcetype/><D:getcontentlength>{size}</D:getcontentlength><D:getetag>\"{size:x}-{stamp:x}\"</D:getetag>"));
    }
    let href = if is_dir && !href.ends_with('/') { format!("{href}/") } else { href.to_string() };
    format!("<D:response><D:href>{href}</D:href><D:propstat><D:prop>{props}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>")
}

async fn propfind(path: &[String], node: &Node, depth: &str) -> ApiResult<Response> {
    let href = path.iter().fold(String::from("/dav"), |href, name| format!("{href}/{}", percent_encode(name)));
    let name = path.last().map_or("dav", String::as_str);
    let children = depth != "0";
    let mut body = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"DAV:\">");
    match node {
        Node::Listing(names) => {
            body.push_str(&entry(&href, name, None, true));
            for child in names.iter().filter(|_| children) {
                body.push_str(&entry(&format!("{href}/{}", percent_encode(child)), child, None, true));
            }
        }
        Node::Disk { path: on_disk, area } => {
            let meta = match tokio::fs::metadata(on_disk).await {
                Ok(meta) => meta,
                // An upload folder that nothing was put into yet is simply empty.
                Err(_) if area.as_ref().is_some_and(|area| area.root == *on_disk) => {
                    body.push_str(&entry(&href, name, None, true));
                    body.push_str("</D:multistatus>");
                    return Ok(multistatus(body));
                }
                Err(_) => return Err(not_found()),
            };
            body.push_str(&entry(&href, name, Some(&meta), meta.is_dir()));
            if meta.is_dir() && children {
                let mut entries = tokio::fs::read_dir(on_disk).await?;
                while let Some(child) = entries.next_entry().await? {
                    let Some(child_name) = child.file_name().to_str().map(str::to_string) else { continue };
                    // Uploads in flight are not files yet.
                    if child_name.ends_with(".hearth-part") {
                        continue;
                    }
                    if let Ok(meta) = child.metadata().await {
                        body.push_str(&entry(&format!("{href}/{}", percent_encode(&child_name)), &child_name, Some(&meta), meta.is_dir()));
                    }
                }
            }
        }
    }
    body.push_str("</D:multistatus>");
    Ok(multistatus(body))
}

fn multistatus(body: String) -> Response {
    (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
}

async fn get(node: &Node, headers: &HeaderMap, head_only: bool) -> ApiResult<Response> {
    let Node::Disk { path, .. } = node else {
        return Ok((StatusCode::METHOD_NOT_ALLOWED, [("allow", ALLOW)]).into_response());
    };
    let meta = tokio::fs::metadata(path).await.map_err(|_| not_found())?;
    if !meta.is_file() {
        return Ok((StatusCode::METHOD_NOT_ALLOWED, [("allow", ALLOW)]).into_response());
    }
    let size = meta.len();
    let range = match headers.get(header::RANGE).and_then(|value| value.to_str().ok()) {
        None => None,
        Some(value) => Some(crate::routes::parse_range(value, size).ok_or_else(|| {
            ApiError::coded(StatusCode::RANGE_NOT_SATISFIABLE, ErrorCode::BadRequest, format!("That range is outside the file, which has {size} bytes."))
        })?),
    };
    let (start, end) = range.unwrap_or((0, size.saturating_sub(1)));
    let length = if size == 0 { 0 } else { end - start + 1 };
    let mut response = Response::builder()
        .status(if range.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, length)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::LAST_MODIFIED, http_date(meta.modified().unwrap_or(UNIX_EPOCH)));
    if range.is_some() {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    if head_only {
        return Ok(response.body(Body::empty()).expect("the headers above are valid"));
    }
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(SeekFrom::Start(start)).await?;
    Ok(response.body(Body::from_stream(ReaderStream::with_capacity(file.take(length), 256 * 1024))).expect("the headers above are valid"))
}

// ---------- writing ----------

/// Stores an upload. The bytes go to a hidden name first and take the real name once they
/// are complete, so a broken upload never replaces a good file, and a file that is a hard
/// link into a kept revision is replaced rather than written through.
async fn put(node: &Node, headers: &HeaderMap, body: Body) -> ApiResult<Response> {
    let (path, area) = writable(node)?;
    if *path == area.root {
        return Err(ApiError::bad_request("Upload files into the folder, not onto it."));
    }
    let existing = tokio::fs::metadata(path).await.ok();
    if existing.as_ref().is_some_and(|meta| meta.is_dir()) {
        return Err(ApiError::conflict("There is a folder with that name."));
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|name| name.to_str())) else {
        return Err(ApiError::bad_request("That is not a file name."));
    };
    tokio::fs::create_dir_all(parent).await?;

    // What the area would hold with this file in place must fit.
    let root = area.root.clone();
    let held = tokio::task::spawn_blocking(move || storage::size_of(&root)).await?;
    let room = area.limit.saturating_sub(held.saturating_sub(existing.as_ref().map_or(0, |meta| meta.len())));
    let too_big = || {
        ApiError::coded(
            StatusCode::INSUFFICIENT_STORAGE,
            ErrorCode::QuotaExceeded,
            "This does not fit into the space set aside for it. Free some space, or ask an admin for more under Settings.",
        )
    };
    if headers.get(header::CONTENT_LENGTH).and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok()).is_some_and(|length| length > room) {
        return Err(too_big());
    }

    let partial = parent.join(storage::partial_name(name));
    let written: ApiResult<()> = async {
        let mut out = tokio::fs::File::create(&partial).await?;
        let mut stream = body.into_data_stream();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ApiError::bad_request("The upload was cut off. Send the file again."))?;
            total += chunk.len() as u64;
            if total > room {
                return Err(too_big());
            }
            out.write_all(&chunk).await?;
        }
        out.flush().await?;
        // rclone sends the file's own modification time, so that a later sync can tell
        // changed files from unchanged ones.
        if let Some(seconds) = headers.get("x-oc-mtime").and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<u64>().ok()) {
            let _ = out.into_std().await.set_modified(UNIX_EPOCH + Duration::from_secs(seconds));
        }
        Ok(())
    }
    .await;
    if let Err(err) = written {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(err);
    }
    tokio::fs::rename(&partial, path).await?;
    Ok(([("x-oc-mtime", "accepted")], if existing.is_some() { StatusCode::NO_CONTENT } else { StatusCode::CREATED }).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_taken_apart_into_plain_names_only() {
        assert_eq!(segments("/saves/7/upload/slot%201/world.sav").unwrap(), ["saves", "7", "upload", "slot 1", "world.sav"]);
        assert_eq!(segments("//saves//7/").unwrap(), ["saves", "7"]);
        for bad in ["/saves/../secret", "/saves/%2e%2e/secret", "/saves/7/upload/a%5Cb", "/saves/7/upload/C%3Ax", "/saves/%ff", "/saves/a%00b", "/saves/%2Fetc"] {
            assert!(segments(bad).is_err(), "{bad}");
        }
        assert_eq!(percent_decode(&percent_encode("slot 1/ü.sav")).unwrap(), "slot 1/ü.sav");
    }

    #[test]
    fn an_entry_says_what_rclone_reads() {
        let file = entry("/dav/saves/7/upload/a&b.sav", "a&b.sav", None, false);
        assert!(file.contains("<D:displayname>a&amp;b.sav</D:displayname>") && file.contains("<D:getcontentlength>0</D:getcontentlength>") && file.contains("<D:resourcetype/>"));
        let folder = entry("/dav/saves/7/upload", "upload", None, true);
        assert!(folder.contains("<D:href>/dav/saves/7/upload/</D:href>") && folder.contains("<D:collection/>") && folder.contains("GMT</D:getlastmodified>"));
    }
}
