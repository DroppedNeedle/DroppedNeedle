//! Cover art routes: release-group, release and artist images by MBID, and
//! an album's own art by local id.
//!
//! Carried over from v2's covers routes: size validation with the same
//! allowed sizes and `original` aliases, ETag + `If-None-Match` handling
//! (strong, weak, and `*`), the 202 warming answer while art resolves in the
//! background, and distinct album/artist placeholders. The v2 debug route
//! is absent here on purpose (it lives in `tooling::covers_debug`).
//!
//! Caching follows Navidrome: the ETag is the content hash the art already
//! carries (no hashing per request), `immutable` is sent only when the
//! request names the art's current version (`?v=`), and placeholders are
//! never cached, so art that turns up later replaces them at once.
//! Unversioned art keeps v2's five-minute window, then revalidates.
//!
//! Bytes come from the [`CoverArt`] port; production runs
//! [`ArtworkService`](super::artwork::ArtworkService).

use axum::{
    Json, Router,
    extract::{FromRequest, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use utoipa::ToSchema;

/// Sizes the release cover routes accept, v2 set kept.
const ALLOWED_SIZES: &[&str] = &["250", "500", "1200"];
/// Aliases meaning "full size", v2 set kept.
const SIZE_ALIAS_ORIGINAL: &[&str] = &["", "original", "full", "max", "largest"];
/// Default release cover size, v2 kept.
const DEFAULT_SIZE: &str = "500";

/// Placeholders and misses are never stored: a cover that turns up later
/// must replace them on the next view.
const NO_STORE: &str = "no-store";
/// Unversioned art: v2's five-minute window, then revalidation by ETag.
const SHORT_CACHE: &str = "public, max-age=300";
/// The request named an older version: revalidate every time.
const REVALIDATE_CACHE: &str = "private, no-cache";
/// An album's own art at its current version, per user like v2.
const PRIVATE_IMMUTABLE_CACHE: &str = "private, max-age=31536000, immutable";
/// An album's own art requested without a version.
const PRIVATE_SHORT_CACHE: &str = "private, max-age=300";

/// Machine code for a bad `size` query value, matching the auth routes.
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

/// Cover bytes with their identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverBytes {
    /// Image bytes served to the client.
    pub bytes: Vec<u8>,
    /// MIME type of the bytes.
    pub content_type: String,
    /// Where the art came from, echoed in `X-Cover-Source`.
    pub source: String,
    /// Hex SHA-256 of the bytes: the ETag and cache identity.
    pub hash: String,
    /// Local art version, when the art is the album's own.
    pub version: Option<i64>,
}

#[cfg(any(test, feature = "test-support"))]
impl CoverBytes {
    /// Build one result, hashing the bytes.
    pub fn new(bytes: Vec<u8>, content_type: &str, source: &str) -> Self {
        Self {
            hash: super::artwork::cache::sha256_hex(&bytes),
            bytes,
            content_type: content_type.to_owned(),
            source: source.to_owned(),
            version: None,
        }
    }
}

/// Outcome of a cover lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverLookup {
    /// Art found.
    Found(CoverBytes),
    /// A fetch is still running; ask again shortly.
    Warming,
    /// No art anywhere.
    Missing,
}

/// Cover art port.
pub trait CoverArt: Send + Sync + 'static {
    /// Release-group cover by MBID.
    fn release_group_cover<'a>(
        &'a self,
        release_group_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup>;
    /// Release cover by MBID.
    fn release_cover<'a>(
        &'a self,
        release_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup>;
    /// Artist image by MBID.
    fn artist_image<'a>(
        &'a self,
        artist_id: &'a str,
        size_px: Option<u32>,
    ) -> BoxFuture<'a, CoverLookup>;
    /// Art for a local album by any source, waiting for a network fetch to
    /// finish (compat clients cannot poll a 202).
    fn album_cover<'a>(
        &'a self,
        album_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>>;
    /// The album's own art (folder or embedded) at `size` (`None` is full
    /// size), with its version.
    fn local_album_art<'a>(
        &'a self,
        album_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>>;
}

/// Scripted art source for tests. Entries are keyed exactly as the
/// handlers query them; anything missing reads as absent.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Default)]
pub struct FakeCoverArt {
    release_groups: std::collections::HashMap<(String, Option<String>), CoverBytes>,
    artists: std::collections::HashMap<(String, Option<u32>), CoverBytes>,
    albums: std::collections::HashMap<String, CoverBytes>,
    warming_release_groups: std::collections::HashSet<(String, Option<String>)>,
    warming_artists: std::collections::HashSet<(String, Option<u32>)>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeCoverArt {
    /// Empty fake: every id misses and nothing warms.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Preload one release-group cover.
    pub fn with_release_group(mut self, id: &str, size: Option<&str>, cover: CoverBytes) -> Self {
        self.release_groups
            .insert((id.to_owned(), size.map(str::to_owned)), cover);
        self
    }

    /// Preload one artist image.
    pub fn with_artist(mut self, id: &str, size_px: Option<u32>, cover: CoverBytes) -> Self {
        self.artists.insert((id.to_owned(), size_px), cover);
        self
    }

    /// Preload one local album's art.
    pub fn with_album(mut self, album_id: &str, cover: CoverBytes) -> Self {
        self.albums.insert(album_id.to_owned(), cover);
        self
    }

    /// Mark one release-group cover as warming.
    pub fn warming_release_group(mut self, id: &str, size: Option<&str>) -> Self {
        self.warming_release_groups
            .insert((id.to_owned(), size.map(str::to_owned)));
        self
    }

    /// Mark one artist image as warming.
    pub fn warming_artist(mut self, id: &str, size_px: Option<u32>) -> Self {
        self.warming_artists.insert((id.to_owned(), size_px));
        self
    }
}

#[cfg(any(test, feature = "test-support"))]
impl CoverArt for FakeCoverArt {
    fn release_group_cover<'a>(
        &'a self,
        release_group_id: &'a str,
        size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup> {
        let key = (release_group_id.to_owned(), size.map(str::to_owned));
        let found = match self.release_groups.get(&key) {
            Some(cover) => CoverLookup::Found(cover.clone()),
            None if self.warming_release_groups.contains(&key) => CoverLookup::Warming,
            None => CoverLookup::Missing,
        };
        Box::pin(async move { found })
    }

    fn release_cover<'a>(
        &'a self,
        _release_id: &'a str,
        _size: Option<&'a str>,
    ) -> BoxFuture<'a, CoverLookup> {
        Box::pin(async { CoverLookup::Missing })
    }

    fn artist_image<'a>(
        &'a self,
        artist_id: &'a str,
        size_px: Option<u32>,
    ) -> BoxFuture<'a, CoverLookup> {
        let key = (artist_id.to_owned(), size_px);
        let found = match self.artists.get(&key) {
            Some(cover) => CoverLookup::Found(cover.clone()),
            None if self.warming_artists.contains(&key) => CoverLookup::Warming,
            None => CoverLookup::Missing,
        };
        Box::pin(async move { found })
    }

    fn album_cover<'a>(
        &'a self,
        album_id: &'a str,
        _size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>> {
        let found = self.albums.get(album_id).cloned();
        Box::pin(async move { found })
    }

    fn local_album_art<'a>(
        &'a self,
        album_id: &'a str,
        _size: Option<&'a str>,
    ) -> BoxFuture<'a, Option<CoverBytes>> {
        let found = self.albums.get(album_id).cloned();
        Box::pin(async move { found })
    }
}

/// Handler state: the art port behind an `Arc` so handlers stay non-generic
/// for utoipa.
#[derive(Clone)]
pub struct CoversState {
    /// Art source.
    pub covers: std::sync::Arc<dyn CoverArt>,
}

impl CoversState {
    /// Wire the state from any port implementation.
    pub fn new(covers: std::sync::Arc<dyn CoverArt>) -> Self {
        Self { covers }
    }
}

/// Cover routes, relative paths for nesting under `/api/v3`.
pub fn routes(state: CoversState) -> Router {
    Router::new()
        .route(
            "/covers/release-group/{release_group_id}",
            get(cover_from_release_group),
        )
        .route("/covers/release/{release_id}", get(cover_from_release))
        .route("/covers/artist/{artist_id}", get(artist_cover))
        .route("/library/albums/{id}/artwork", get(album_artwork))
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

/// Query for the album artwork route.
#[derive(Debug, Deserialize)]
pub struct AlbumArtQuery {
    /// Art version the caller holds (`cover_version`).
    pub v: Option<i64>,
    /// 250, 500, 1200, or absent / `original` for full size.
    #[serde(default)]
    pub size: String,
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
            (header::CACHE_CONTROL, NO_STORE),
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
            (header::CACHE_CONTROL, NO_STORE),
            (
                header::HeaderName::from_static("x-cover-source"),
                "placeholder",
            ),
        ],
        svg.as_bytes().to_vec(),
    )
        .into_response()
}

/// Serve art with its ETag, or 304 when the client already has it.
fn art_response(art: CoverBytes, headers: &HeaderMap, cache: &'static str) -> Response {
    let etag = format!("\"{}\"", art.hash);
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    if etag_matches(if_none_match, &etag) {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::CACHE_CONTROL, cache),
                (header::ETAG, etag.as_str()),
            ],
        )
            .into_response();
    }
    let mut response = (StatusCode::OK, art.bytes).into_response();
    let response_headers = response.headers_mut();
    if let Ok(content_type) = art.content_type.parse() {
        response_headers.insert(header::CONTENT_TYPE, content_type);
    }
    response_headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(cache),
    );
    if let Ok(source) = art.source.parse() {
        response_headers.insert(header::HeaderName::from_static("x-cover-source"), source);
    }
    if let Ok(tag) = etag.parse() {
        response_headers.insert(header::ETAG, tag);
    }
    response
}

fn lookup_response(
    lookup: CoverLookup,
    headers: &HeaderMap,
    placeholder: &'static str,
) -> Response {
    match lookup {
        CoverLookup::Found(art) => art_response(art, headers, SHORT_CACHE),
        CoverLookup::Warming => warming_response(),
        CoverLookup::Missing => placeholder_response(placeholder),
    }
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
    let lookup = state
        .covers
        .release_group_cover(&release_group_id, desired_size.as_deref())
        .await;
    lookup_response(lookup, &headers, ALBUM_PLACEHOLDER_SVG)
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
    let lookup = state
        .covers
        .release_cover(&release_id, desired_size.as_deref())
        .await;
    lookup_response(lookup, &headers, ALBUM_PLACEHOLDER_SVG)
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
    let lookup = state.covers.artist_image(&artist_id, query.size).await;
    lookup_response(lookup, &headers, ARTIST_PLACEHOLDER_SVG)
}

/// An album's own art: the folder image or embedded picture the library
/// scan found. Never reaches out to the network. Pass the album's
/// `cover_version` as `v`: a matching version is cached for good, an older
/// one revalidates.
#[utoipa::path(
    get,
    path = "/api/v3/library/albums/{id}/artwork",
    params(
        ("id" = String, Path, description = "Local album id"),
        ("v" = Option<i64>, Query, description = "Art version the caller holds"),
        ("size" = Option<String>, Query, description = "250, 500, 1200, or original (the default) for full size"),
    ),
    responses(
        (status = 200, description = "Image bytes"),
        (status = 304, description = "Image unchanged"),
        (status = 400, description = "Bad query string or unsupported size"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "The album has no local art"),
    )
)]
pub async fn album_artwork(
    State(state): State<CoversState>,
    Path(album_id): Path<String>,
    headers: HeaderMap,
    ValidQuery(query): ValidQuery<AlbumArtQuery>,
) -> Response {
    let size = match normalize_size(&query.size) {
        Ok(size) => size,
        Err(message) => return invalid_input(message),
    };
    let Some(art) = state
        .covers
        .local_album_art(&album_id, size.as_deref())
        .await
    else {
        return (
            StatusCode::NOT_FOUND,
            [
                (header::CACHE_CONTROL, NO_STORE),
                (header::HeaderName::from_static("x-cover-state"), "missing"),
            ],
        )
            .into_response();
    };
    let cache = match (query.v, art.version) {
        (Some(asked), Some(current)) if asked == current => PRIVATE_IMMUTABLE_CACHE,
        (Some(_), _) => REVALIDATE_CACHE,
        (None, _) => PRIVATE_SHORT_CACHE,
    };
    art_response(art, &headers, cache)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_defaults_and_aliases() {
        assert_eq!(normalize_size("500").unwrap(), Some("500".to_owned()));
        for alias in ["", "original", "FULL", " max ", "Largest"] {
            assert_eq!(normalize_size(alias).unwrap(), None, "alias {alias}");
        }
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
}
