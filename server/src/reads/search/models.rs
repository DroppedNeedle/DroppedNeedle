//! Unified search DTOs: the native `/api/v3/search` shapes.
//!
//! Clean-slate v3 shapes (v2 wire is not preserved). Everything is
//! snake_case. Search reads the local `local_*` catalog tables only;
//! provider-backed results land in stage 5 behind the same handlers, which
//! is why items already carry `in_library`/`requested` (local rows are
//! always in-library and never requested).

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// Which catalog bucket a result came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchKind {
    /// A local artist row.
    Artist,
    /// A local album row.
    Album,
    /// A local track row.
    Track,
}

/// Per-bucket provider health for one search call. The values mirror v1's
/// `SearchRemoteStatus` exactly so the same notice and stale-time logic
/// applies. The local-only stage always emits `Ok`; stage-5 provider
/// fan-out fills in the real values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchRemoteStatus {
    /// Full results.
    Ok,
    /// Some provider results missing; what arrived is shown.
    Partial,
    /// The provider timed out; local matches are still shown.
    Timeout,
    /// The provider failed; local matches are still shown.
    Error,
    /// The provider is unavailable; cached results are shown.
    Stale,
}

/// One search hit. Cover art is deliberately absent: the covers endpoints
/// serve art by id, so search never duplicates those URLs.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchResultItem {
    /// Result bucket.
    pub kind: SearchKind,
    /// Local catalog id.
    pub id: String,
    /// Display title (artist name, album title, or track title).
    pub title: String,
    /// Owning artist name, when the row has one.
    pub artist: Option<String>,
    /// Release year, when known.
    pub year: Option<i32>,
    /// Provider id from the external-identity tables, when the row has an
    /// accepted identity. Absent means unidentified, never failure.
    pub musicbrainz_id: Option<String>,
    /// True for every local hit; provider hits in stage 5 vary.
    pub in_library: bool,
    /// Always false in stage 4; the requests slice owns this flag.
    pub requested: bool,
    /// Match score, 0-100. Exact folded match is 100, folded prefix is 90,
    /// folded substring is 70. Ranks hits, never filters them.
    pub score: i32,
}

/// Unified search response: one ranked list per bucket plus the standout
/// hit per bucket, when one earns it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchResponse {
    /// Matching artists, best first.
    pub artists: Vec<SearchResultItem>,
    /// Matching albums, best first.
    pub albums: Vec<SearchResultItem>,
    /// Matching tracks, best first.
    pub tracks: Vec<SearchResultItem>,
    /// Standout artist hit, if one scored high enough to headline.
    pub top_artist: Option<SearchResultItem>,
    /// Standout album hit, if one scored high enough to headline.
    pub top_album: Option<SearchResultItem>,
    /// Standout track hit, if one scored high enough to headline.
    pub top_track: Option<SearchResultItem>,
    /// Provider health for the artist bucket.
    pub artist_status: SearchRemoteStatus,
    /// Provider health for the album bucket.
    pub album_status: SearchRemoteStatus,
    /// Provider health for the track bucket.
    pub track_status: SearchRemoteStatus,
}

/// One bucket drill-down page.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SearchBucketResponse {
    /// Echoed bucket name.
    pub bucket: String,
    /// Echoed page size.
    pub limit: u32,
    /// Echoed offset.
    pub offset: u32,
    /// This page of hits, best first.
    pub results: Vec<SearchResultItem>,
    /// Standout hit, present only on the first page and only when one
    /// scored high enough to headline.
    pub top_result: Option<SearchResultItem>,
    /// Provider health for this bucket.
    pub status: SearchRemoteStatus,
}

/// One typeahead suggestion.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SuggestResult {
    /// Suggestion bucket.
    pub kind: SearchKind,
    /// Display title.
    pub title: String,
    /// Owning artist name, when the row has one.
    pub artist: Option<String>,
    /// Local catalog id.
    pub id: String,
    /// Provider id, when the row has an accepted identity.
    pub musicbrainz_id: Option<String>,
    /// Match score, 0-100, same scale as full search.
    pub score: i32,
}

/// Typeahead response: one merged list across buckets, best first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SuggestResponse {
    /// Suggestions, best first.
    pub results: Vec<SuggestResult>,
    /// Provider health for the suggestion call.
    pub status: SearchRemoteStatus,
}

/// Query params for `GET /api/v3/search`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct SearchQuery {
    /// Search text. Required; blank is a 400.
    pub q: String,
    /// Max artists (0-100, default 10).
    pub limit_artists: Option<u32>,
    /// Max albums (0-100, default 10).
    pub limit_albums: Option<u32>,
    /// Max tracks (0-100, default 10).
    pub limit_tracks: Option<u32>,
    /// Comma-separated subset (`artists,albums,tracks`); absent means all.
    pub buckets: Option<String>,
}

/// Query params for `GET /api/v3/search/{bucket}`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct BucketQuery {
    /// Search text. Required; blank is a 400.
    pub q: String,
    /// Page size (1-100, default 50).
    pub limit: Option<u32>,
    /// Pagination offset (default 0).
    pub offset: Option<u32>,
}

/// Query params for `GET /api/v3/search/suggest`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct SuggestQuery {
    /// Search text. Queries shorter than two characters return an empty
    /// 200, kept from v2 so the typeahead never errors mid-typing.
    pub q: String,
    /// Max suggestions (1-10, default 5).
    pub limit: Option<u32>,
}

/// One artist row to enrich.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ArtistEnrichmentRequest {
    /// Provider artist id.
    pub musicbrainz_id: String,
    /// Artist name, a fallback key for providers without the id.
    #[serde(default)]
    pub name: String,
}

/// One album row to enrich.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlbumEnrichmentRequest {
    /// Provider release-group id.
    pub musicbrainz_id: String,
    /// Album artist name, a fallback key for providers without the id.
    #[serde(default)]
    pub artist_name: String,
    /// Album name, a fallback key for providers without the id.
    #[serde(default)]
    pub album_name: String,
}

/// The single enrich-batch body: both enrichable buckets in one call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct EnrichmentBatchRequest {
    /// Artists to enrich (capped per bucket, extras ignored).
    #[serde(default)]
    pub artists: Vec<ArtistEnrichmentRequest>,
    /// Albums to enrich (capped per bucket, extras ignored).
    #[serde(default)]
    pub albums: Vec<AlbumEnrichmentRequest>,
}

/// Which provider answered the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EnrichmentSource {
    /// ListenBrainz listen counts.
    Listenbrainz,
    /// Last.fm listener counts.
    Lastfm,
    /// No provider answered; counts are absent, never zero-filled.
    None,
}

/// Enriched artist counts. Absent counts mean unknown, never zero.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ArtistEnrichment {
    /// Echoed provider artist id.
    pub musicbrainz_id: String,
    /// Known release-group count, when the provider reported one.
    pub release_group_count: Option<i64>,
    /// Listen count, when the provider reported one.
    pub listen_count: Option<i64>,
}

/// Enriched album counts. Absent counts mean unknown, never zero.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AlbumEnrichment {
    /// Echoed provider release-group id.
    pub musicbrainz_id: String,
    /// Known track count, when the provider reported one.
    pub track_count: Option<i64>,
    /// Listen count, when the provider reported one.
    pub listen_count: Option<i64>,
}

/// One degraded source inside an otherwise successful batch. The recording
/// is the error signal: optional enrichment degrades, never fails.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Degradation {
    /// Source that degraded (`listenbrainz`, `lastfm`).
    pub source: String,
    /// Fixed machine code; provider detail never reaches the wire.
    pub code: String,
    /// Fixed human message.
    pub message: String,
}

/// Enrich-batch response for both buckets plus degradation notes.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EnrichmentResponse {
    /// Artist counts, same order as the request.
    pub artists: Vec<ArtistEnrichment>,
    /// Album counts, same order as the request.
    pub albums: Vec<AlbumEnrichment>,
    /// Provider that answered.
    pub source: EnrichmentSource,
    /// Sources that degraded while answering; usually empty.
    pub degradations: Vec<Degradation>,
}
