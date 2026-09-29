//! Cover art reads: release-group, release, and artist images.
//!
//! Carried over from v2 `covers.py` (trace A:69-71): size validation with the
//! same allowed sizes and `original` aliases, ETag + `If-None-Match` handling
//! (strong, weak, and `*`), the 202 warming answer while art resolves in the
//! background, and distinct album/artist placeholders. The v2 debug route
//! (A:72) is deliberately absent here.
//!
//! Cover bytes come from the [`CoverArt`] port. Stage 4 ships the
//! [`FakeCoverArt`] only; real art providers land in stage 5 behind the same
//! trait. Thumbnailing is a no-op here: the fake returns final bytes and stage
//! 5 applies the size after fetching.
//!
//! Self-contained on purpose: no `crate::` imports, so this module compiles
//! both inside the wired tree and standalone in the slice tests. The error
//! envelope mirrors `crate::error` exactly.

use std::collections::{HashMap, HashSet};

use axum::{
    Json, Router,
    extract::{FromRequest, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

/// Sizes the release cover routes accept, v2 set kept.
const ALLOWED_SIZES: &[&str] = &["250", "500", "1200"];
/// Aliases meaning "full size", v2 set kept.
const SIZE_ALIAS_ORIGINAL: &[&str] = &["", "original", "full", "max", "largest"];
/// Default release cover size, v2 kept.
const DEFAULT_SIZE: &str = "500";

/// Placeholder cache window. Short on purpose (v2 comment kept): a cold cover
/// warms in the background, and a long-lived placeholder in the browser cache
/// would mask the real art.
const PLACEHOLDER_CACHE_CONTROL: &str = "public, max-age=300";
/// Preferred-source cache window: immutable for a year.
const PREFERRED_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
/// Cover Art Archive cache window. CAA art can change under us, so it stays
/// short like the placeholder.
const FALLBACK_CACHE_CONTROL: &str = "public, max-age=300";
/// Source label marking CAA bytes for the short cache window.
const FALLBACK_SOURCE: &str = "cover-art-archive";

/// Machine code for a bad `size` query value, matching the auth slices.
const INVALID_INPUT: &str = "INVALID_INPUT";

/// Album placeholder, v2 SVG kept so cold covers look the same.
const ALBUM_PLACEHOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
        <rect fill="#374151" width="200" height="200"/>
        <circle cx="100" cy="100" r="70" fill="#1f2937" stroke="#4B5563" stroke-width="2"/>
        <circle cx="100" cy="100" r="50" fill="none" stroke="#4B5563" stroke-width="1"/>
        <circle cx="100" cy="100" r="30" fill="none" stroke="#4B5563" stroke-width="1"/>
        <circle cx="100" cy="100" r="12" fill="#4B5563"/>
        <circle cx="100" cy="100" r="4" fill="#374151"/>
    </svg>"##;

/// Artist placeholder, v2 SVG kept.
const ARTIST_PLACEHOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200">
            <rect fill="#374151" width="200" height="200"/>
            <circle cx="100" cy="80" r="30" fill="#6B7280"/>
            <path d="M60 120 Q100 140 140 120 L140 160 Q100 180 60 160 Z" fill="#6B7280"/>
        </svg>"##;

/// Fetched cover bytes with their content type and source label.
#[derive(Debug, Clone)]
pub struct CoverBytes {
    /// Final image bytes served to the client.
    pub bytes: Vec<u8>,
    /// MIME type of the bytes.
    pub content_type: String,
    /// Where the art came from, echoed in `X-Cover-Source`.
    pub source: String,
}

impl CoverBytes {
    /// Build one fetched result.
    pub fn new(bytes: Vec<u8>, content_type: &str, source: &str) -> Self {
        Self {
            bytes,
            content_type: content_type.to_owned(),
            source: source.to_owned(),
        }
    }
}

/// Cover art port. Stage 5 implements the real providers; stage 4 tests and
/// handlers run against [`FakeCoverArt`].
pub trait CoverArt: Send + Sync + 'static {
    /// Release-group cover, or `None` when no art is cached yet.
    fn release_group_cover(
        &self,
        release_group_id: &str,
        size: Option<&str>,
    ) -> BoxFuture<'_, Option<CoverBytes>>;
    /// Release cover, or `None` when no art is cached yet.
    fn release_cover(
        &self,
        release_id: &str,
        size: Option<&str>,
    ) -> BoxFuture<'_, Option<CoverBytes>>;
    /// Artist image, or `None` when no art is cached yet.
    fn artist_image(
        &self,
        artist_id: &str,
        size_px: Option<u32>,
    ) -> BoxFuture<'_, Option<CoverBytes>>;
    /// True while a release-group cover resolves in the background.
    fn is_release_group_warming(&self, release_group_id: &str, size: Option<&str>) -> bool;
    /// True while a release cover resolves in the background.
    fn is_release_warming(&self, release_id: &str) -> bool;
    /// True while an artist image resolves in the background.
    fn is_artist_warming(&self, artist_id: &str, size_px: Option<u32>) -> bool;
}

/// Fake art source for stage 4. Entries are keyed exactly as the handlers
/// query them; anything missing reads as absent (placeholder or warming).
#[derive(Debug, Clone, Default)]
pub struct FakeCoverArt {
    release_groups: HashMap<(String, Option<String>), CoverBytes>,
    releases: HashMap<(String, Option<String>), CoverBytes>,
    artists: HashMap<(String, Option<u32>), CoverBytes>,
    warming_release_groups: HashSet<(String, Option<String>)>,
    warming_releases: HashSet<String>,
    warming_artists: HashSet<(String, Option<u32>)>,
}

impl FakeCoverArt {
    /// Empty fake: every id misses and nothing warms.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Preload one release-group cover.
    pub fn with_release_group(
        mut self,
        id: &str,
        size: Option<&str>,
        bytes: Vec<u8>,
        content_type: &str,
        source: &str,
    ) -> Self {
        self.release_groups.insert(
            (id.to_owned(), size.map(str::to_owned)),
            CoverBytes::new(bytes, content_type, source),
        );
        self
    }

    /// Preload one release cover.
    pub fn with_release(
        mut self,
        id: &str,
        size: Option<&str>,
        bytes: Vec<u8>,
        content_type: &str,
        source: &str,
    ) -> Self {
        self.releases.insert(
            (id.to_owned(), size.map(str::to_owned)),
            CoverBytes::new(bytes, content_type, source),
        );
        self
    }

    /// Preload one artist image.
    pub fn with_artist(
        mut self,
        id: &str,
        size_px: Option<u32>,
        bytes: Vec<u8>,
        content_type: &str,
        source: &str,
    ) -> Self {
        self.artists.insert(
            (id.to_owned(), size_px),
            CoverBytes::new(bytes, content_type, source),
        );
        self
    }

    /// Mark one release-group cover as warming.
    pub fn warming_release_group(mut self, id: &str, size: Option<&str>) -> Self {
        self.warming_release_groups
            .insert((id.to_owned(), size.map(str::to_owned)));
        self
    }

    /// Mark one release cover as warming.
    pub fn warming_release(mut self, id: &str) -> Self {
        self.warming_releases.insert(id.to_owned());
        self
    }

    /// Mark one artist image as warming.
    pub fn warming_artist(mut self, id: &str, size_px: Option<u32>) -> Self {
        self.warming_artists.insert((id.to_owned(), size_px));
        self
    }
}

impl CoverArt for FakeCoverArt {
    fn release_group_cover(
        &self,
        release_group_id: &str,
        size: Option<&str>,
    ) -> BoxFuture<'_, Option<CoverBytes>> {
        let found = self
            .release_groups
            .get(&(release_group_id.to_owned(), size.map(str::to_owned)))
            .cloned();
        Box::pin(async move { found })
    }

    fn release_cover(
        &self,
        release_id: &str,
        size: Option<&str>,
    ) -> BoxFuture<'_, Option<CoverBytes>> {
        let found = self
            .releases
            .get(&(release_id.to_owned(), size.map(str::to_owned)))
            .cloned();
        Box::pin(async move { found })
    }

    fn artist_image(
        &self,
        artist_id: &str,
        size_px: Option<u32>,
    ) -> BoxFuture<'_, Option<CoverBytes>> {
        let found = self.artists.get(&(artist_id.to_owned(), size_px)).cloned();
        Box::pin(async move { found })
    }

    fn is_release_group_warming(&self, release_group_id: &str, size: Option<&str>) -> bool {
        self.warming_release_groups
            .contains(&(release_group_id.to_owned(), size.map(str::to_owned)))
    }

    fn is_release_warming(&self, release_id: &str) -> bool {
        self.warming_releases.contains(release_id)
    }

    fn is_artist_warming(&self, artist_id: &str, size_px: Option<u32>) -> bool {
        self.warming_artists
            .contains(&(artist_id.to_owned(), size_px))
    }
}

/// Handler state: the art port behind an `Arc` so handlers stay non-generic
/// for utoipa.
#[derive(Clone)]
pub struct CoversState {
    /// Art source (fake in stage 4, providers in stage 5).
    pub covers: std::sync::Arc<dyn CoverArt>,
}

impl CoversState {
    /// Wire the state from any port implementation.
    pub fn new(covers: std::sync::Arc<dyn CoverArt>) -> Self {
        Self { covers }
    }
}

/// Routes for this slice, relative paths for nesting under `/api/v3`.
pub fn routes(state: CoversState) -> Router {
    Router::new()
        .route(
            "/covers/release-group/{release_group_id}",
            get(cover_from_release_group),
        )
        .route("/covers/release/{release_id}", get(cover_from_release))
        .route("/covers/artist/{artist_id}", get(artist_cover))
        .with_state(state)
}

/// `size` query for the release cover routes.
#[derive(Debug, Deserialize)]
pub struct SizeQuery {
    /// Preferred size: 250, 500, 1200, or `original` for full size.
    #[serde(default = "default_size_param")]
    pub size: String,
}

fn default_size_param() -> String {
    DEFAULT_SIZE.to_owned()
}

/// `size` query for the artist route: a pixel width, or absent.
#[derive(Debug, Deserialize)]
pub struct ArtistSizeQuery {
    /// Preferred width in pixels.
    pub size: Option<u32>,
}

/// Validate a release-cover `size` value. `Ok(None)` means full size;
/// `Err` carries the user-facing message, v2 wording kept.
fn normalize_size(size: &str) -> Result<Option<String>, String> {
    let normalized = size.trim().to_ascii_lowercase();
    if SIZE_ALIAS_ORIGINAL.contains(&normalized.as_str()) {
        return Ok(None);
    }
    if ALLOWED_SIZES.contains(&normalized.as_str()) {
        return Ok(Some(normalized));
    }
    Err(format!(
        "Unsupported size '{size}'. Choose one of 250, 500, 1200 or original."
    ))
}

/// Cache window for fetched art by source label.
fn cache_control_for(source: &str) -> &'static str {
    if source == FALLBACK_SOURCE {
        FALLBACK_CACHE_CONTROL
    } else {
        PREFERRED_CACHE_CONTROL
    }
}

/// Strong ETag for served bytes. The hash is opaque to clients; only the
/// quoting and matching rules are contract.
fn etag_for(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("\"{:x}\"", hasher.finalize())
}

/// `If-None-Match` matching, v2 rules kept: `*` matches anything, and both
/// the strong tag and its `W/` weak form match.
fn etag_matches(if_none_match: Option<&str>, etag: &str) -> bool {
    let Some(header_value) = if_none_match else {
        return false;
    };
    let candidates: Vec<&str> = header_value.split(',').map(str::trim).collect();
    if candidates.contains(&"*") || candidates.contains(&etag) {
        return true;
    }
    let weak = format!("W/{etag}");
    candidates.contains(&weak.as_str())
}

/// Shared error envelope, byte-identical in shape to `crate::error`.
#[derive(Debug, Clone, Serialize, ToSchema)]
struct CoversErrorBody {
    code: String,
    message: String,
    details: Option<serde_json::Value>,
}

/// Shared error envelope, byte-identical in shape to `crate::error`.
#[derive(Debug, Clone, Serialize, ToSchema)]
struct CoversErrorEnvelope {
    error: CoversErrorBody,
}

fn invalid_input(message: String) -> Response {
    let body = CoversErrorEnvelope {
        error: CoversErrorBody {
            code: INVALID_INPUT.to_owned(),
            message,
            details: None,
        },
    };
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// Query extractor that renders failures in the shared envelope instead
/// of Axum's default plain-text 400.
pub struct ValidQuery<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidQuery<T> {
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request(req, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|cause| invalid_input(format!("Invalid query string: {cause}")))
    }
}

/// Warming answer: 202 with an empty body so `<img>` fires `onerror`, which
/// the frontend treats as "still warming" (skeleton + poll) rather than a
/// settled placeholder. `no-store` so each poll re-requests.
fn warming_response() -> Response {
    (
        StatusCode::ACCEPTED,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::HeaderName::from_static("x-cover-source"), "warming"),
        ],
        Vec::<u8>::new(),
    )
        .into_response()
}

fn placeholder_response(svg: &'static str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, PLACEHOLDER_CACHE_CONTROL),
            (
                header::HeaderName::from_static("x-cover-source"),
                "placeholder",
            ),
        ],
        svg.as_bytes().to_vec(),
    )
        .into_response()
}

fn art_response(art: &CoverBytes, headers: &HeaderMap, preferred_cache: &'static str) -> Response {
    let etag = etag_for(&art.bytes);
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    if etag_matches(if_none_match, &etag) {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::CACHE_CONTROL, preferred_cache),
                (header::ETAG, etag.as_str()),
            ],
        )
            .into_response();
    }
    let mut response = (StatusCode::OK, art.bytes.clone()).into_response();
    let response_headers = response.headers_mut();
    if let Ok(content_type) = art.content_type.parse() {
        response_headers.insert(header::CONTENT_TYPE, content_type);
    }
    if let Ok(cache) = preferred_cache.parse() {
        response_headers.insert(header::CACHE_CONTROL, cache);
    }
    if let Ok(source) = art.source.parse() {
        response_headers.insert(header::HeaderName::from_static("x-cover-source"), source);
    }
    if let Ok(tag) = etag.parse() {
        response_headers.insert(header::ETAG, tag);
    }
    response
}

/// Release-group cover art.
#[utoipa::path(
    get,
    path = "/api/v3/covers/release-group/{release_group_id}",
    params(
        ("release_group_id" = String, Path, description = "MusicBrainz release group id"),
        ("size" = Option<String>, Query, description = "Preferred size: 250, 500, 1200, or original for full size"),
    ),
    responses(
        (status = 200, description = "Cover bytes or placeholder SVG"),
        (status = 202, description = "Cover is warming; poll again"),
        (status = 304, description = "Cover unchanged"),
        (status = 400, description = "Unsupported size"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn cover_from_release_group(
    State(state): State<CoversState>,
    Path(release_group_id): Path<String>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<SizeQuery>,
) -> Response {
    let desired_size = match normalize_size(&query.size) {
        Ok(size) => size,
        Err(message) => return invalid_input(message),
    };
    let art = state
        .covers
        .release_group_cover(&release_group_id, desired_size.as_deref())
        .await;
    if let Some(art) = art {
        let cache = cache_control_for(&art.source);
        return art_response(&art, &headers, cache);
    }
    if state
        .covers
        .is_release_group_warming(&release_group_id, desired_size.as_deref())
    {
        return warming_response();
    }
    placeholder_response(ALBUM_PLACEHOLDER_SVG)
}

/// Release cover art.
#[utoipa::path(
    get,
    path = "/api/v3/covers/release/{release_id}",
    params(
        ("release_id" = String, Path, description = "MusicBrainz release id"),
        ("size" = Option<String>, Query, description = "Preferred size: 250, 500, 1200, or original for full size"),
    ),
    responses(
        (status = 200, description = "Cover bytes or placeholder SVG"),
        (status = 202, description = "Cover is warming; poll again"),
        (status = 304, description = "Cover unchanged"),
        (status = 400, description = "Unsupported size"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn cover_from_release(
    State(state): State<CoversState>,
    Path(release_id): Path<String>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<SizeQuery>,
) -> Response {
    let desired_size = match normalize_size(&query.size) {
        Ok(size) => size,
        Err(message) => return invalid_input(message),
    };
    let art = state
        .covers
        .release_cover(&release_id, desired_size.as_deref())
        .await;
    if let Some(art) = art {
        let cache = cache_control_for(&art.source);
        return art_response(&art, &headers, cache);
    }
    if state.covers.is_release_warming(&release_id) {
        return warming_response();
    }
    placeholder_response(ALBUM_PLACEHOLDER_SVG)
}

/// Artist image.
#[utoipa::path(
    get,
    path = "/api/v3/covers/artist/{artist_id}",
    params(
        ("artist_id" = String, Path, description = "MusicBrainz artist id"),
        ("size" = Option<u32>, Query, description = "Preferred width in pixels"),
    ),
    responses(
        (status = 200, description = "Image bytes or placeholder SVG"),
        (status = 202, description = "Image is warming; poll again"),
        (status = 304, description = "Image unchanged"),
        (status = 400, description = "Bad query string"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn artist_cover(
    State(state): State<CoversState>,
    Path(artist_id): Path<String>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<ArtistSizeQuery>,
) -> Response {
    let art = state.covers.artist_image(&artist_id, query.size).await;
    if let Some(art) = art {
        return art_response(&art, &headers, PREFERRED_CACHE_CONTROL);
    }
    if state.covers.is_artist_warming(&artist_id, query.size) {
        return warming_response();
    }
    placeholder_response(ARTIST_PLACEHOLDER_SVG)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_defaults_and_aliases() {
        assert_eq!(normalize_size("500").unwrap(), Some("500".to_owned()));
        assert_eq!(normalize_size("250").unwrap(), Some("250".to_owned()));
        assert_eq!(normalize_size("1200").unwrap(), Some("1200".to_owned()));
        for alias in ["", "original", "FULL", " max ", "Largest"] {
            assert_eq!(normalize_size(alias).unwrap(), None, "alias {alias}");
        }
    }

    #[test]
    fn size_rejects_unknown_values() {
        let err = normalize_size("999").unwrap_err();
        assert!(err.contains("Unsupported size '999'"), "{err}");
    }

    #[test]
    fn etag_matching_covers_strong_weak_and_star() {
        assert!(etag_matches(Some("\"abc\""), "\"abc\""));
        assert!(etag_matches(Some("W/\"abc\""), "\"abc\""));
        assert!(etag_matches(Some("*"), "\"abc\""));
        assert!(etag_matches(Some("\"other\", \"abc\""), "\"abc\""));
        assert!(!etag_matches(Some("\"other\""), "\"abc\""));
        assert!(!etag_matches(None, "\"abc\""));
    }

    #[test]
    fn fallback_source_gets_short_cache_window() {
        assert_eq!(
            cache_control_for("cover-art-archive"),
            FALLBACK_CACHE_CONTROL
        );
        assert_eq!(cache_control_for("audiodb"), PREFERRED_CACHE_CONTROL);
    }

    #[test]
    fn etag_is_stable_and_quoted() {
        let tag = etag_for(b"bytes");
        assert_eq!(tag, etag_for(b"bytes"));
        assert!(tag.starts_with('"') && tag.ends_with('"'));
        assert_ne!(tag, etag_for(b"other"));
    }
}
