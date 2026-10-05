//! The web UI: stamped at boot, served as the router fallback.
//!
//! The image ships the SvelteKit build with every root-relative URL behind
//! the literal [`BASE_PATH_PLACEHOLDER`] (see `frontend/svelte.config.js`).
//! [`WebUi::prepare`] copies that pristine tree into `<cache>/static`,
//! replaces the placeholder with the configured `BASE_PATH` (an empty base
//! replaces it with nothing), drops the precompressed `.br`/`.gz` copies of
//! the files it rewrote, and refuses to boot if a placeholder survives. The
//! new tree is staged next to the old one and swapped in only once it is
//! complete.
//!
//! Serving follows v2's `static_server.py`: `_app/immutable/*` is cached
//! for a year, `index.html` and the rest of `_app/` are `no-cache`, other
//! assets for a week. A smaller `.br` or `.gz` sibling is sent when the
//! client accepts it, with `Vary: Accept-Encoding`. Any other GET that is
//! not an API path gets `index.html` so client-side routes load.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use thiserror::Error;

/// Token the frontend build bakes in front of every root-relative URL.
/// Must match `frontend/svelte.config.js` byte for byte.
pub const BASE_PATH_PLACEHOLDER: &str = "/__DROPPEDNEEDLE_BASE__";

/// File suffixes that may carry the placeholder. Finding it anywhere else
/// means a broken build, which fails the boot rather than patching bytes.
const TEXT_SUFFIXES: &[&str] = &[
    "html",
    "js",
    "mjs",
    "css",
    "json",
    "map",
    "txt",
    "xml",
    "svg",
    "webmanifest",
];

/// Precompressed siblings, as (content coding, file suffix).
const ENCODINGS: &[(&str, &str)] = &[("br", "br"), ("gzip", "gz")];

const STAGE_PREFIX: &str = ".static-stage-";
const PREVIOUS_PREFIX: &str = ".static-previous-";

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const NO_CACHE: &str = "no-cache";
const ONE_WEEK: &str = "public, max-age=604800";
const ONE_DAY: &str = "public, max-age=86400";

/// Why the web UI could not be prepared. Each one stops the boot.
#[derive(Debug, Error)]
pub enum WebError {
    /// Copying or rewriting the tree failed.
    #[error("cannot prepare the web UI at {path}: {source}")]
    Io {
        /// File or directory involved.
        path: PathBuf,
        /// Underlying failure.
        source: std::io::Error,
    },
    /// A base path is set but the build has no placeholder to replace.
    #[error(
        "the web UI build in {0} has no base path placeholder, so BASE_PATH cannot apply; \
         build it with DROPPEDNEEDLE_BASE_PATH_PLACEHOLDER=1"
    )]
    NoPlaceholder(PathBuf),
    /// The placeholder sits in a file that is not text.
    #[error("the base path placeholder is inside non-text asset {0}")]
    PlaceholderInBinary(PathBuf),
    /// The placeholder is still present after stamping.
    #[error("the base path placeholder survived stamping in {0}")]
    PlaceholderSurvived(PathBuf),
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> WebError + '_ {
    move |source| WebError::Io {
        path: path.to_owned(),
        source,
    }
}

/// A prepared web UI tree, ready to serve.
#[derive(Debug, Clone)]
pub struct WebUi {
    root: Arc<PathBuf>,
}

impl WebUi {
    /// Serve an already prepared tree as is.
    pub fn serve_dir(root: &Path) -> Self {
        Self {
            root: Arc::new(root.to_owned()),
        }
    }

    /// Directory being served.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Rebuild `served` from the pristine `template` with `base_path`
    /// stamped in. Returns `None`, touching nothing, when `template` holds
    /// no build (no `index.html`), so a server without a UI still serves
    /// the API. Blocking file work: run it off the async workers.
    pub fn prepare(
        template: &Path,
        served: &Path,
        base_path: &str,
    ) -> Result<Option<Self>, WebError> {
        if !template.join("index.html").is_file() {
            return Ok(None);
        }
        let parent = served.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(io_error(parent))?;
        clear_stale_stages(parent)?;
        let stage = parent.join(format!("{STAGE_PREFIX}{}", std::process::id()));
        let built = copy_tree(template, &stage).and_then(|()| stamp(&stage, base_path));
        let swapped = built.and_then(|tokens| {
            if tokens == 0 && !base_path.is_empty() {
                return Err(WebError::NoPlaceholder(template.to_owned()));
            }
            swap_into_place(&stage, served)
        });
        if let Err(error) = swapped {
            if let Err(cleanup) = remove_tree(&stage) {
                tracing::warn!(stage = %stage.display(), %cleanup, "cannot remove web UI staging tree");
            }
            return Err(error);
        }
        Ok(Some(Self::serve_dir(served)))
    }

    /// Answer a GET or HEAD for `path` (already stripped of the base path).
    /// `None` means "not mine": a non-GET method, an API path, or a missing
    /// file under an asset directory. The caller then answers with its 404.
    pub async fn respond(
        &self,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
    ) -> Option<Response> {
        if method != Method::GET && method != Method::HEAD {
            return None;
        }
        if is_api_path(path) {
            return None;
        }
        let relative = path.trim_start_matches('/');
        let segments: Vec<&str> = relative.split('/').collect();
        let unsafe_segment = segments
            .iter()
            .any(|segment| matches!(*segment, "." | "..") || segment.contains(['\\', '\0']));
        if unsafe_segment {
            return None;
        }
        if !relative.is_empty() && !relative.ends_with('/') {
            let file = self.root.join(relative);
            if is_file(&file).await {
                return Some(serve_file(&file, relative, headers).await);
            }
            let asset_dir = matches!(segments.first(), Some(&("_app" | "img" | "fonts")));
            if asset_dir {
                return None;
            }
        }
        let index = self.root.join("index.html");
        Some(serve_file(&index, "index.html", headers).await)
    }
}

/// `/api` or anything under it, segment-aware (v2 refused the whole
/// `api*` prefix; `/apiary` is a fine client route).
fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

async fn is_file(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_file())
}

/// Cache policy by location, matching v2.
fn cache_control(relative: &str) -> &'static str {
    if relative.starts_with("_app/immutable/") {
        IMMUTABLE
    } else if relative == "index.html" || relative.starts_with("_app/") {
        NO_CACHE
    } else if relative == "robots.txt" {
        ONE_DAY
    } else {
        ONE_WEEK
    }
}

fn content_type(relative: &str) -> &'static str {
    let extension = relative.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Quality the client gives `coding` in `Accept-Encoding` (0 when refused).
fn encoding_quality(accept: &str, coding: &str) -> f32 {
    let mut wildcard = 0.0;
    for item in accept.split(',') {
        let mut parts = item.split(';');
        let token = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }
        let mut quality = 1.0_f32;
        for parameter in parts {
            if let Some(("q", value)) = parameter.trim().split_once('=') {
                quality = value.trim().parse::<f32>().unwrap_or(0.0).clamp(0.0, 1.0);
            }
        }
        if token == coding {
            return quality;
        }
        if token == "*" {
            wildcard = quality;
        }
    }
    wildcard
}

/// One file on disk chosen to answer the request.
struct Representation {
    path: PathBuf,
    length: u64,
    modified_nanos: u128,
    coding: Option<&'static str>,
}

async fn representation(path: &Path, coding: Option<&'static str>) -> Option<Representation> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    Some(Representation {
        path: path.to_owned(),
        length: metadata.len(),
        modified_nanos,
        coding,
    })
}

/// Pick the identity file or its best accepted precompressed sibling.
/// Returns the choice and whether any sibling exists (for `Vary`).
async fn choose(path: &Path, headers: &HeaderMap) -> Option<(Representation, bool)> {
    let identity = representation(path, None).await?;
    let mut variants = Vec::new();
    for (coding, suffix) in ENCODINGS {
        let mut sibling = path.as_os_str().to_owned();
        sibling.push(format!(".{suffix}"));
        if let Some(found) = representation(Path::new(&sibling), Some(coding)).await
            && found.length < identity.length
        {
            variants.push(found);
        }
    }
    let has_variants = !variants.is_empty();
    if !has_variants || headers.contains_key(header::RANGE) {
        return Some((identity, has_variants));
    }
    let accept = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let mut best: Option<(f32, Representation)> = None;
    for variant in variants {
        let quality = encoding_quality(accept, variant.coding.unwrap_or(""));
        // Ties go to the first listed coding (brotli).
        if quality > 0.0 && best.as_ref().is_none_or(|(top, _)| quality > *top) {
            best = Some((quality, variant));
        }
    }
    Some((best.map_or(identity, |(_, chosen)| chosen), true))
}

async fn serve_file(path: &Path, relative: &str, headers: &HeaderMap) -> Response {
    let Some((chosen, has_variants)) = choose(path, headers).await else {
        return status_only(StatusCode::NOT_FOUND);
    };
    let etag = format!(
        "\"{:x}-{:x}{}\"",
        chosen.length,
        chosen.modified_nanos,
        chosen
            .coding
            .map_or(String::new(), |coding| format!("-{coding}"))
    );
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|tag| tag.trim() == "*" || tag.trim().trim_start_matches("W/") == etag)
        });
    let mut builder = Response::builder()
        .header(header::CACHE_CONTROL, cache_control(relative))
        .header(header::ETAG, &etag);
    if has_variants {
        builder = builder.header(header::VARY, "Accept-Encoding");
    }
    if not_modified {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .unwrap_or_else(|_| status_only(StatusCode::INTERNAL_SERVER_ERROR));
    }
    let bytes = match tokio::fs::read(&chosen.path).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(path = %chosen.path.display(), %error, "cannot read web UI file");
            return status_only(StatusCode::NOT_FOUND);
        }
    };
    builder = builder.header(header::CONTENT_TYPE, content_type(relative));
    if let Some(coding) = chosen.coding {
        builder = builder.header(header::CONTENT_ENCODING, coding);
    }
    builder
        .status(StatusCode::OK)
        .body(Body::from(bytes))
        .unwrap_or_else(|_| status_only(StatusCode::INTERNAL_SERVER_ERROR))
}

fn status_only(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_CACHE));
    response
}

/// Remove staging and previous trees left behind by a crashed boot.
fn clear_stale_stages(parent: &Path) -> Result<(), WebError> {
    for entry in std::fs::read_dir(parent).map_err(io_error(parent))? {
        let entry = entry.map_err(io_error(parent))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(STAGE_PREFIX) || name.starts_with(PREVIOUS_PREFIX) {
            remove_tree(&entry.path())?;
        }
    }
    Ok(())
}

fn remove_tree(path: &Path) -> Result<(), WebError> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(WebError::Io {
            path: path.to_owned(),
            source: error,
        }),
        _ => Ok(()),
    }
}

/// Copy directories and regular files; anything else (links, devices)
/// is skipped with a warning, since a build never contains them.
fn copy_tree(from: &Path, to: &Path) -> Result<(), WebError> {
    std::fs::create_dir_all(to).map_err(io_error(to))?;
    for entry in std::fs::read_dir(from).map_err(io_error(from))? {
        let entry = entry.map_err(io_error(from))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().map_err(io_error(&source))?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_file() {
            std::fs::copy(&source, &target).map_err(io_error(&source))?;
        } else {
            tracing::warn!(path = %source.display(), "skipping non-regular file in the web UI build");
        }
    }
    Ok(())
}

fn files_under(root: &Path) -> Result<Vec<PathBuf>, WebError> {
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(io_error(&dir))? {
            let path = entry.map_err(io_error(&dir))?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn extension_of(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Replace every placeholder in the staged tree and return how many were
/// replaced. Precompressed copies of a rewritten file are now stale and
/// are deleted; the identity file serves until the next build.
fn stamp(stage: &Path, base_path: &str) -> Result<usize, WebError> {
    let token = BASE_PATH_PLACEHOLDER.as_bytes();
    let mut replaced = 0;
    for path in files_under(stage)? {
        let extension = extension_of(&path);
        if ENCODINGS.iter().any(|(_, suffix)| *suffix == extension) {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(io_error(&path))?;
        if !contains(&bytes, token) {
            continue;
        }
        if !TEXT_SUFFIXES.contains(&extension.as_str()) {
            return Err(WebError::PlaceholderInBinary(path));
        }
        let text = String::from_utf8_lossy(&bytes);
        replaced += text.matches(BASE_PATH_PLACEHOLDER).count();
        let stamped = text.replace(BASE_PATH_PLACEHOLDER, base_path);
        if contains(stamped.as_bytes(), token) {
            return Err(WebError::PlaceholderSurvived(path));
        }
        std::fs::write(&path, stamped).map_err(io_error(&path))?;
        for (_, suffix) in ENCODINGS {
            let mut sibling = path.as_os_str().to_owned();
            sibling.push(format!(".{suffix}"));
            match std::fs::remove_file(&sibling) {
                Err(error) if error.kind() != ErrorKind::NotFound => {
                    return Err(WebError::Io {
                        path: PathBuf::from(sibling),
                        source: error,
                    });
                }
                _ => {}
            }
        }
    }
    Ok(replaced)
}

/// Move the finished stage into place, keeping the old tree until the
/// rename lands so a failure leaves the previous UI serving.
fn swap_into_place(stage: &Path, served: &Path) -> Result<(), WebError> {
    let parent = served.parent().unwrap_or(Path::new("."));
    let previous = parent.join(format!("{PREVIOUS_PREFIX}{}", std::process::id()));
    let had_previous = served.exists();
    if had_previous {
        std::fs::rename(served, &previous).map_err(io_error(served))?;
    }
    if let Err(source) = std::fs::rename(stage, served) {
        if had_previous && let Err(error) = std::fs::rename(&previous, served) {
            tracing::error!(%error, "cannot restore the previous web UI tree");
        }
        return Err(WebError::Io {
            path: served.to_owned(),
            source,
        });
    }
    if had_previous && let Err(error) = remove_tree(&previous) {
        tracing::warn!(%error, "cannot remove the previous web UI tree");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(dir: &Path) -> PathBuf {
        let template = dir.join("template");
        std::fs::create_dir_all(template.join("_app/immutable")).unwrap();
        std::fs::write(
            template.join("index.html"),
            format!("<script src=\"{BASE_PATH_PLACEHOLDER}/_app/immutable/a.js\"></script>"),
        )
        .unwrap();
        std::fs::write(template.join("index.html.gz"), b"stale").unwrap();
        std::fs::write(template.join("_app/immutable/a.js"), b"base").unwrap();
        std::fs::write(template.join("logo.png"), b"\x89PNG").unwrap();
        template
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dn-web-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn prepare_stamps_the_base_and_drops_stale_variants() {
        let dir = scratch("stamp");
        let template = build(&dir);
        let served = dir.join("cache/static");
        WebUi::prepare(&template, &served, "/music")
            .unwrap()
            .unwrap();
        let index = std::fs::read_to_string(served.join("index.html")).unwrap();
        assert_eq!(
            index,
            "<script src=\"/music/_app/immutable/a.js\"></script>"
        );
        assert!(!served.join("index.html.gz").exists());
        assert!(
            template.join("index.html.gz").exists(),
            "the template is never touched"
        );

        // A restart with a new base rebuilds from the pristine template.
        WebUi::prepare(&template, &served, "").unwrap().unwrap();
        let index = std::fs::read_to_string(served.join("index.html")).unwrap();
        assert_eq!(index, "<script src=\"/_app/immutable/a.js\"></script>");
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("cache"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("static")]);
    }

    #[test]
    fn prepare_refuses_builds_it_cannot_stamp() {
        let dir = scratch("refuse");
        let template = build(&dir);
        std::fs::write(template.join("logo.png"), BASE_PATH_PLACEHOLDER).unwrap();
        let served = dir.join("static");
        assert!(matches!(
            WebUi::prepare(&template, &served, ""),
            Err(WebError::PlaceholderInBinary(_))
        ));
        assert!(!served.exists());

        std::fs::write(template.join("logo.png"), b"\x89PNG").unwrap();
        std::fs::write(template.join("index.html"), b"<p>no token</p>").unwrap();
        assert!(matches!(
            WebUi::prepare(&template, &served, "/music"),
            Err(WebError::NoPlaceholder(_))
        ));
        assert!(
            WebUi::prepare(&dir.join("missing"), &served, "")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn accept_encoding_quality_follows_q_values() {
        assert_eq!(encoding_quality("gzip, br", "br"), 1.0);
        assert_eq!(encoding_quality("gzip;q=0.5, *;q=0.1", "br"), 0.1);
        assert_eq!(encoding_quality("br;q=0", "br"), 0.0);
        assert_eq!(encoding_quality("", "gzip"), 0.0);
    }
}
