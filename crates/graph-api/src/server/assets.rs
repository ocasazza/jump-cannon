//! Content-correct cache validators for the frontend-dist static service.
//!
//! The dist comes from the Nix store, where every file's mtime is epoch, so
//! `ServeDir`'s `Last-Modified` is `Thu, 01 Jan 1970 00:00:01 GMT` for every
//! asset. Browsers revalidate with `If-Modified-Since: <that date>` and get a
//! 304 forever — after an upgrade they keep running the OLD wasm.
//!
//! So revalidation is driven by a **strong ETag derived from the file's
//! content** (SHA-256, computed once at startup for every file in the dist,
//! including the precompressed `.br` / `.gz` siblings — each encoding is a
//! distinct representation with its own ETag, and `Vary: Accept-Encoding`
//! already tells shared caches to key on it). The layer honours
//! `If-None-Match` (matching → 304) and *ignores* `If-Modified-Since` /
//! `If-Unmodified-Since`, stripping them before the request reaches
//! `ServeDir` and dropping `Last-Modified` from responses, so an epoch date
//! can never produce a false 304.
//!
//! Why ETags and not Trunk's `filehash`: the app spawns workers at fixed URLs
//! (`/tvix-worker.js`, `/pest-worker.js`, `*_bg.wasm` — see `app/Trunk.toml`,
//! `filehash = false`), and the boot shell is served under the fixed name
//! `index.html`; content-hashed filenames would break those loads. ETags
//! change no URLs.

use std::{
    collections::HashMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::Response,
    Router,
};
use sha2::{Digest, Sha256};
use tower_http::services::ServeDir;

/// The frontend-dist static service: `ServeDir` (with the precompressed
/// siblings enabled) plus the content-hash validator layer. This is the exact
/// service wired as the router's fallback by [`super::router_with_host`].
pub fn assets_service(dist: PathBuf) -> Router {
    let validator = Arc::new(AssetValidator::new(dist.clone()));
    let serve = ServeDir::new(dist)
        .precompressed_br()
        .precompressed_gzip();
    Router::new()
        .fallback_service(serve)
        .layer(axum::middleware::from_fn_with_state(
            validator,
            etag_middleware,
        ))
}

/// The `If-None-Match` / ETag layer. Runs in front of `ServeDir`: evaluates
/// the conditional itself (so a matching tag short-circuits to 304 with no
/// file read), then stamps the content ETag onto the 200 that `ServeDir`
/// produces and drops its epoch `Last-Modified`.
async fn etag_middleware(
    State(validator): State<Arc<AssetValidator>>,
    mut req: Request,
    next: Next,
) -> Response {
    // The dist files carry epoch mtimes, so any date-conditional header
    // (`If-Modified-Since`, `If-Unmodified-Since`) is meaningless: ServeDir
    // must never see it, or it answers 304/412 from the epoch date.
    strip_date_conditionals(req.headers_mut());

    let uri_path = req.uri().path().to_owned();
    let accept_encoding = req
        .headers()
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let rel = resolve_served_file(&validator.dist, &uri_path, accept_encoding.as_deref());

    // Range requests must keep their 206 (a 304 would break byte-range
    // revalidation), so only plain full-body requests short-circuit.
    let range = req.headers().contains_key(header::RANGE);
    if !range {
        if let (Some(rel), Some(inm)) = (&rel, req.headers().get(header::IF_NONE_MATCH)) {
            if let Some(tag) = validator.etag.get(rel) {
                if if_none_match_matches(inm, tag) {
                    return not_modified(tag);
                }
            }
        }
    }

    let resp = next.run(req).await;
    if resp.status() != StatusCode::OK {
        return resp;
    }
    let Some(rel) = rel else { return resp };

    let Some(tag) = validator.etag.get(&rel).cloned() else {
        // A file that appeared after startup (e.g. a `trunk watch` dev dist):
        // hash the file being served, once, and remember it. The body itself
        // is left untouched.
        let Ok(bytes) = std::fs::read(validator.dist.join(&rel)) else {
            return resp;
        };
        let tag = etag_for(&bytes);
        validator.remember(&rel, &tag);
        let mut resp = resp;
        finish_headers(&mut resp, &tag);
        return resp;
    };
    let mut resp = resp;
    finish_headers(&mut resp, &tag);
    resp
}

/// Precomputed content hashes for one dist, shared by every request.
///
/// `etag` maps each regular file under the dist root (identity files AND
/// their precompressed siblings — the exact path `ServeDir` will open) to its
/// strong ETag, computed once at startup. Nix store files are immutable, so
/// the map is valid for the server's lifetime; a file added later is hashed
/// once from its served body into `live`.
struct AssetValidator {
    dist: PathBuf,
    etag: HashMap<PathBuf, String>,
    live: std::sync::Mutex<HashMap<PathBuf, String>>,
}

impl AssetValidator {
    fn new(dist: PathBuf) -> Self {
        let mut etag = HashMap::new();
        Self::walk(&dist, &dist, &mut etag);
        Self {
            dist,
            etag,
            live: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn walk(dist: &Path, dir: &Path, etag: &mut HashMap<PathBuf, String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::walk(dist, &path, etag);
            } else if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(rel) = path.strip_prefix(dist) {
                    etag.insert(rel.to_path_buf(), etag_for(&bytes));
                }
            }
        }
    }

    /// Remember the hash of a file that was not part of the startup snapshot.
    fn remember(&self, rel: &Path, tag: &str) {
        if let Ok(mut live) = self.live.lock() {
            live.insert(rel.to_path_buf(), tag.to_owned());
        }
    }
}

/// Strong ETag for a byte string: hex SHA-256 in RFC 9110 quoted form.
fn etag_for(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(66);
    out.push('"');
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out.push('"');
    out
}

/// `true` when the request's `If-None-Match` matches the given strong ETag.
/// RFC 9110 §13.1.2 uses the *weak* comparison function here, so a `W/`
/// prefixed copy of the tag matches too; `*` matches any representation.
fn if_none_match_matches(header_value: &HeaderValue, etag: &str) -> bool {
    let list = header_value.to_str().unwrap_or_default();
    list.split(',')
        .map(str::trim)
        .map(|tag| tag.strip_prefix("W/").unwrap_or(tag))
        .any(|tag| tag == "*" || tag == etag)
}

/// Drop the date-conditional request headers before `ServeDir` sees them.
fn strip_date_conditionals(headers: &mut HeaderMap) {
    headers.remove(header::IF_MODIFIED_SINCE);
    headers.remove(header::IF_UNMODIFIED_SINCE);
}

/// Attach the ETag and drop `Last-Modified` from a 200 asset response.
fn finish_headers(resp: &mut Response<Body>, etag: &str) {
    if let Ok(value) = HeaderValue::from_str(etag) {
        resp.headers_mut().insert(header::ETAG, value);
    }
    resp.headers_mut().remove(header::LAST_MODIFIED);
}

/// 304 for a matched `If-None-Match`: ETag only, no body, no date headers.
fn not_modified(etag: &str) -> Response<Body> {
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::NOT_MODIFIED;
    if let Ok(value) = HeaderValue::from_str(etag) {
        resp.headers_mut().insert(header::ETAG, value);
    }
    resp
}

/// The dist file that `ServeDir` would serve for this URI, as a path relative
/// to the dist root, mirroring `ServeDir`'s own resolution: percent-decode,
/// reject `..`/absolute components, `/` → `index.html`, trailing-slash
/// directories → their `index.html`, then the precompressed sibling for the
/// best accepted encoding. `None` when there is no such file (404, or the
/// directory redirect tower-http issues) — those responses need no validator.
fn resolve_served_file(
    dist: &Path,
    uri_path: &str,
    accept_encoding: Option<&str>,
) -> Option<PathBuf> {
    let decoded = percent_decode(uri_path);
    let mut rel = PathBuf::new();
    for component in decoded.split('/') {
        match component {
            "" | "." => {}
            ".." => return None,
            c => rel.push(c),
        }
    }
    if rel.as_os_str().is_empty() {
        rel.push("index.html");
    } else if dist.join(&rel).is_dir() {
        if !uri_path.ends_with('/') {
            return None; // tower-http answers the directory with a redirect
        }
        rel.push("index.html");
    }
    // Precompressed sibling, in the same preference order tower-http tries
    // encodings (its Encoding enum orders Deflate < Gzip < Brotli < Zstd).
    let name = rel.file_name()?.to_string_lossy().into_owned();
    for (suffix, encoding) in [
        (".zz", "deflate"),
        (".gz", "gzip"),
        (".br", "br"),
        (".zst", "zstd"),
    ] {
        if accept_encoding_is(accept_encoding, encoding) {
            let mut variant = rel.clone();
            variant.set_file_name(format!("{name}{suffix}"));
            if dist.join(&variant).is_file() {
                return Some(variant);
            }
        }
    }
    dist.join(&rel).is_file().then_some(rel)
}

/// True when an `Accept-Encoding` header value admits the named encoding at a
/// positive q-value (the variant lookup only; tower-http re-negotiates the
/// full header itself and falls back to the identity file if the sibling is
/// absent).
fn accept_encoding_is(value: Option<&str>, encoding: &str) -> bool {
    let Some(value) = value else { return false };
    value
        .split(',')
        .filter_map(|item| {
            let mut parts = item.splitn(2, ';');
            let name = parts.next()?.trim().to_ascii_lowercase();
            let q = parts.next().map(parse_q).unwrap_or(1.0);
            (name == encoding || name == "*").then_some(q)
        })
        .any(|q| q > 0.0)
}

fn parse_q(param: &str) -> f32 {
    let param = param.trim();
    let Some(q) = param
        .strip_prefix("q=")
        .or_else(|| param.strip_prefix("Q="))
    else {
        return 1.0;
    };
    q.trim().parse().unwrap_or(1.0)
}

/// Percent-decode a URI path (UTF-8 lossy: dist filenames are ASCII).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
