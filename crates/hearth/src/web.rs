//! The web UI, embedded in the binary. `build.rs` compiles crates/hearth-web to WebAssembly;
//! this module serves it, plus a page shell for every path the UI routes itself.

use std::sync::LazyLock;

use axum::{
    Router,
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use sha2::{Digest, Sha256};
use tower_http::compression::CompressionLayer;

use crate::{error::ApiError, state::AppState};

const INDEX: &str = include_str!("../ui/index.html");
const BOOT_JS: &str = include_str!("../ui/boot.js");
const CSS: &str = include_str!("../ui/app.css");
const APP_JS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ui/hearth-web.js"));
const APP_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ui/hearth-web_bg.wasm"));

struct Asset {
    body: &'static [u8],
    content_type: &'static str,
    etag: LazyLock<String>,
}

macro_rules! asset {
    ($body:expr, $content_type:literal) => {
        Asset { body: $body, content_type: $content_type, etag: LazyLock::new(|| etag($body)) }
    };
}

static ASSETS: [(&str, Asset); 4] = [
    ("/assets/app.css", asset!(CSS.as_bytes(), "text/css; charset=utf-8")),
    ("/assets/boot.js", asset!(BOOT_JS.as_bytes(), "text/javascript; charset=utf-8")),
    ("/assets/hearth-web.js", asset!(APP_JS, "text/javascript; charset=utf-8")),
    ("/assets/hearth-web_bg.wasm", asset!(APP_WASM, "application/wasm")),
];

fn etag(body: &[u8]) -> String {
    format!("\"{}\"", &crate::tls::hex(&Sha256::digest(body))[..24])
}

pub fn router() -> Router<AppState> {
    Router::new().route("/assets/{file}", get(asset)).fallback(page).layer(CompressionLayer::new())
}

async fn asset(uri: Uri, headers: HeaderMap) -> Response {
    let Some((_, asset)) = ASSETS.iter().find(|(path, _)| *path == uri.path()) else {
        return ApiError::not_found("There is no such file.").into_response();
    };
    // The browser may keep a copy but asks each time, so a new Hearth version shows up at once.
    let caching = [(header::ETAG, asset.etag.as_str()), (header::CACHE_CONTROL, "no-cache")];
    if headers.get(header::IF_NONE_MATCH).is_some_and(|seen| seen.as_bytes() == asset.etag.as_bytes()) {
        return (StatusCode::NOT_MODIFIED, caching).into_response();
    }
    (caching, [(header::CONTENT_TYPE, asset.content_type)], asset.body).into_response()
}

/// Every other GET is a page of the UI, which reads the path and shows the right view.
async fn page(method: axum::http::Method, uri: Uri) -> Response {
    let path = uri.path();
    if method != axum::http::Method::GET || path.starts_with("/api/") || path.starts_with("/dav/") {
        return ApiError::not_found("There is nothing at this address.").into_response();
    }
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], INDEX).into_response()
}

/// Headers every response carries. The page may load code and data from the Hearth only.
pub fn security_headers() -> [(header::HeaderName, HeaderValue); 4] {
    [
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; \
                 img-src 'self' data: blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; \
                 form-action 'self'; frame-ancestors 'none'",
            ),
        ),
        (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        (header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")),
        (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
    ]
}
