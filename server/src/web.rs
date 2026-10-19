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
//! Files are served by `tower_http`'s `ServeDir`, which sends a `.br` or
//! `.gz` sibling when the client accepts it and answers ranges. Cache
//! policy follows v2's `static_server.py`: `_app/immutable/*` is cached
//! for a year, `index.html` and the rest of `_app/` are `no-cache`, other
//! assets for a week. Any other GET that is not an API path gets
//! `index.html` so client-side routes load.
//!
//! `index.html` holds inline script, which the page's content-security
//! policy admits by hash. The hashes are taken from the file as served,
//! after stamping, and every answer that is that page carries them to the
//! layer in `security_headers.rs`.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header},
    response::Response,
};
use base64::Engine as _;
use sha2::Digest as _;
use thiserror::Error;
use tower_http::services::ServeDir;

use crate::security_headers::AppShell;

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
    assets: ServeDir,
    shell: AppShell,
}

impl WebUi {
    /// Serve an already prepared tree as is. Blocking file work: it reads
    /// `index.html` for its inline scripts.
    pub fn serve_dir(root: &Path) -> Self {
        let index = root.join("index.html");
        let script_hashes = match std::fs::read_to_string(&index) {
            Ok(page) => {
                let hashes = inline_script_hashes(&page);
                // The build has always had one: the page's boot script.
                // None means the build or the scanner changed, and an
                // enforced policy would leave the page blank.
                if hashes.is_empty() {
                    tracing::warn!(
                        path = %index.display(),
                        "the web UI page has no inline script; an enforced \
                         content-security policy may leave it blank"
                    );
                }
                hashes
            }
            Err(error) => {
                tracing::warn!(path = %index.display(), %error, "cannot read the web UI page");
                Vec::new()
            }
        };
        Self {
            root: Arc::new(root.to_owned()),
            assets: ServeDir::new(root)
                .append_index_html_on_directories(false)
                .precompressed_br()
                .precompressed_gzip(),
            shell: AppShell {
                script_hashes: script_hashes.into(),
            },
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
        // Every check runs on the path `ServeDir` will open, so no spelling
        // of `index.html` reaches it as a plain file.
        let decoded = percent_encoding::percent_decode_str(path)
            .decode_utf8()
            .ok()?;
        if is_api_path(&decoded) {
            return None;
        }
        let relative = decoded.trim_start_matches('/');
        let segments: Vec<&str> = relative.split('/').collect();
        let unsafe_segment = segments
            .iter()
            .any(|segment| matches!(*segment, "." | "..") || segment.contains(['\\', '\0']));
        if unsafe_segment {
            return None;
        }
        if !relative.is_empty() && !relative.ends_with('/') && relative != "index.html" {
            if let Some(asset) = self.asset(method, path, relative, headers).await {
                return Some(asset);
            }
            let asset_dir = matches!(segments.first(), Some(&("_app" | "img" | "fonts")));
            if asset_dir {
                return None;
            }
        }
        self.page(headers).await
    }

    /// A file under the root, through `ServeDir`: a precompressed sibling
    /// for a client that accepts it, ranges, HEAD and `Last-Modified`.
    /// `None` when there is no such file.
    async fn asset(
        &self,
        method: &Method,
        path: &str,
        relative: &str,
        headers: &HeaderMap,
    ) -> Option<Response> {
        let mut request = Request::new(Body::empty());
        *request.method_mut() = method.clone();
        *request.uri_mut() = Uri::try_from(path).ok()?;
        *request.headers_mut() = headers.clone();
        // A range is taken of the file itself, never of a compressed copy.
        if headers.contains_key(header::RANGE) {
            request.headers_mut().remove(header::ACCEPT_ENCODING);
        }
        let mut response = match self.assets.clone().try_call(request).await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(path = %relative, %error, "cannot read web UI file");
                return None;
            }
        };
        if response.status() == StatusCode::NOT_FOUND {
            return None;
        }
        let headers = response.headers_mut();
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control(relative)),
        );
        // Any file may have a precompressed sibling, and the compression
        // layer varies the rest.
        headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
        if headers.contains_key(header::CONTENT_TYPE) {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(relative)),
            );
        }
        Some(response.map(Body::new))
    }

    /// The app's page, marked as such for the content-security layer. It
    /// has its own `ETag`, which that layer extends with the policy mark so
    /// a page cached under other settings is sent again in full; the
    /// `Last-Modified` that `ServeDir` sends cannot carry the mark. The
    /// page is sent as is and the compression layer compresses it.
    async fn page(&self, headers: &HeaderMap) -> Option<Response> {
        let path = self.root.join("index.html");
        let metadata = tokio::fs::metadata(&path).await.ok()?;
        let modified_nanos = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        let etag = format!("\"{:x}-{:x}\"", metadata.len(), modified_nanos);
        let not_modified = headers
            .get(header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|tag| tag.trim() == "*" || tag.trim().trim_start_matches("W/") == etag)
            });
        let builder = Response::builder()
            .header(header::CACHE_CONTROL, NO_CACHE)
            .header(header::ETAG, &etag)
            .extension(self.shell.clone());
        let built = if not_modified {
            builder.status(StatusCode::NOT_MODIFIED).body(Body::empty())
        } else {
            let bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "cannot read the web UI page");
                    return None;
                }
            };
            builder
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Body::from(bytes))
        };
        built.ok()
    }
}

/// `sha256-<base64>` of each inline script in `page`, the form a
/// content-security policy names one by. The text hashed is exactly what
/// sits between the tags. A script with a `src` has no text to hash.
///
/// Checked against the page SvelteKit 2.70 builds: one inline classic
/// script, inside the body's `<div>`, and no `<script` in a comment or an
/// attribute. A page with none gives an empty list. A hash that does not
/// match the served text cannot go unnoticed: the app does not start.
fn inline_script_hashes(page: &str) -> Vec<String> {
    // ASCII lowercasing keeps every byte offset, so positions found in
    // `lower` index `page`.
    let lower = page.to_ascii_lowercase();
    let mut hashes = Vec::new();
    let mut at = 0;
    while let Some(open) = lower[at..].find("<script").map(|found| at + found) {
        let attributes = open + "<script".len();
        let Some(text) = lower[attributes..]
            .find('>')
            .map(|found| attributes + found + 1)
        else {
            break;
        };
        let Some(close) = lower[text..].find("</script").map(|found| text + found) else {
            break;
        };
        let has_src = lower[attributes..text]
            .split_ascii_whitespace()
            .any(|attribute| attribute.starts_with("src=") || attribute == "src");
        if !has_src {
            let digest = sha2::Sha256::digest(&page.as_bytes()[text..close]);
            hashes.push(format!(
                "sha256-{}",
                base64::engine::general_purpose::STANDARD.encode(digest)
            ));
        }
        at = close;
    }
    hashes
}

/// `/api` or anything under it, segment-aware (v2 refused the whole
/// `api*` prefix; `/apiary` is a fine client route).
fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// Cache policy for a file other than the page, by location, matching v2.
fn cache_control(relative: &str) -> &'static str {
    if relative.starts_with("_app/immutable/") {
        IMMUTABLE
    } else if relative.starts_with("_app/") {
        NO_CACHE
    } else if relative == "robots.txt" {
        ONE_DAY
    } else {
        ONE_WEEK
    }
}

/// The content type a file is sent as, in place of the one `ServeDir`
/// takes from `mime_guess`. Where the two differ this keeps what these
/// files have always been sent as: text keeps its charset, source maps are
/// JSON, and an unknown extension stays opaque rather than becoming
/// something a browser may run.
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
    use droppedneedle_kernel::db::scratch::ScratchDir;

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

    fn scratch(name: &str) -> ScratchDir {
        ScratchDir::new(&format!("web-{name}")).unwrap()
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
}
