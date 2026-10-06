//! Unified search: the library's `local_*` tables joined with MusicBrainz.
//!
//! MusicBrainz answers artists and albums the way v2 searched them, under
//! a deadline and with a stale copy standing in when it is down; library
//! copies attach their local id and the in-library flag, and library rows
//! MusicBrainz did not return still show. A dead MusicBrainz never fails a
//! search: the library hits come back with the bucket status saying why
//! the rest is missing.
//!
//! Local matching is accent- and case-insensitive through the baseline folded
//! columns (`folded_name`, `title_folded`, ...): the query is folded with
//! the same [`fold_text`](crate::db::fold::fold_text) the writers
//! use, so a keyboard without accents still finds the artist. Retired
//! (merged-away) artists and albums never match, and tracks outside the
//! `indexed` availability never match. Empty results are absence, never
//! failure.

use std::time::Duration;

use sqlx::{Row, SqlitePool};

use crate::reads::catalog::Catalog;
use crate::reads::catalog::search::{RemoteBucket, RemoteHit, RemoteKind, RemoteState};

use super::models::{
    Degradation, EnrichmentBatchRequest, EnrichmentResponse, EnrichmentSource,
    SearchBucketResponse, SearchKind, SearchRemoteStatus, SearchResponse, SearchResultItem,
    SuggestResponse, SuggestResult,
};
use super::ports::{EnrichmentPort, MAX_ENRICHMENT_PER_BUCKET};

/// Score for an exact folded-title match. Also the top-result bar, kept
/// from v2 (`TOP_RESULT_SCORE_THRESHOLD`).
pub const SCORE_EXACT: i32 = 100;
/// Score for a folded-prefix match. Meets the top-result bar.
pub const SCORE_PREFIX: i32 = 90;
/// Score for a folded-substring match. Ranks, never headlines.
pub const SCORE_SUBSTRING: i32 = 70;

/// A drill-down bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// Local artists.
    Artists,
    /// Local albums.
    Albums,
    /// Local tracks.
    Tracks,
}

impl Bucket {
    /// Parse a bucket segment or filter token. Unknown names fail to `None`;
    /// handlers map that to 404 (path) or 400 (filter).
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "artists" => Some(Self::Artists),
            "albums" => Some(Self::Albums),
            "tracks" => Some(Self::Tracks),
            _ => None,
        }
    }

    /// Canonical bucket name for echoes.
    pub fn name(self) -> &'static str {
        match self {
            Self::Artists => "artists",
            Self::Albums => "albums",
            Self::Tracks => "tracks",
        }
    }
}

/// Per-bucket caps for one unified search.
#[derive(Debug, Clone, Copy)]
pub struct BucketLimits {
    /// Max artists.
    pub artists: u32,
    /// Max albums.
    pub albums: u32,
    /// Max tracks.
    pub tracks: u32,
}

/// Unified search over one SQLite pool plus, when wired, MusicBrainz.
/// Clone shares the pool handle and the catalog.
#[derive(Clone)]
pub struct SearchService {
    pool: SqlitePool,
    remote: Option<Catalog>,
}

impl std::fmt::Debug for SearchService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchService")
            .field("remote", &self.remote.is_some())
            .finish_non_exhaustive()
    }
}

/// How long a full search waits for MusicBrainz (v2
/// `FULL_SEARCH_TIMEOUT_SECONDS`). Local hits never wait on it.
pub const FULL_SEARCH_DEADLINE: Duration = Duration::from_secs(6);
/// How long typeahead waits for MusicBrainz (v2 `SUGGEST_TIMEOUT_SECONDS`).
pub const SUGGEST_DEADLINE: Duration = Duration::from_secs(3);

impl SearchService {
    /// Serve searches from this pool only.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool, remote: None }
    }

    /// Also search MusicBrainz through this catalog.
    #[must_use]
    pub fn with_remote(mut self, catalog: Catalog) -> Self {
        self.remote = Some(catalog);
        self
    }

    /// Ranked hits per selected bucket plus each bucket's standout hit.
    /// Artists and albums join MusicBrainz hits with the library's own
    /// (local copies attach their id and the in-library flag); tracks are
    /// local only. A blank-after-fold query matches nothing.
    pub async fn search(
        &self,
        query: &str,
        limits: BucketLimits,
        buckets: &[Bucket],
    ) -> Result<SearchResponse, sqlx::Error> {
        let folded = crate::db::fold::fold_text(query);
        if folded.is_empty() {
            return Ok(SearchResponse {
                artists: Vec::new(),
                albums: Vec::new(),
                tracks: Vec::new(),
                top_artist: None,
                top_album: None,
                top_track: None,
                artist_status: SearchRemoteStatus::Ok,
                album_status: SearchRemoteStatus::Ok,
                track_status: SearchRemoteStatus::Ok,
            });
        }
        let wants = |bucket: Bucket, limit: u32| buckets.contains(&bucket) && limit > 0;
        let local = async {
            let artists = if wants(Bucket::Artists, limits.artists) {
                self.search_artists(&folded, limits.artists).await?
            } else {
                Vec::new()
            };
            let albums = if wants(Bucket::Albums, limits.albums) {
                self.search_albums(&folded, limits.albums).await?
            } else {
                Vec::new()
            };
            let tracks = if wants(Bucket::Tracks, limits.tracks) {
                self.search_tracks(&folded, limits.tracks, 0).await?
            } else {
                Vec::new()
            };
            Ok::<_, sqlx::Error>((artists, albums, tracks))
        };
        let remote_artists = self.remote_bucket(
            Bucket::Artists,
            query,
            limits.artists,
            0,
            FULL_SEARCH_DEADLINE,
            wants(Bucket::Artists, limits.artists),
        );
        let remote_albums = self.remote_bucket(
            Bucket::Albums,
            query,
            limits.albums,
            0,
            FULL_SEARCH_DEADLINE,
            wants(Bucket::Albums, limits.albums),
        );
        let (local, remote_artists, remote_albums) =
            tokio::join!(local, remote_artists, remote_albums);
        let (local_artists, local_albums, tracks) = local?;
        let artist_status = status_of(remote_artists.as_ref());
        let album_status = status_of(remote_albums.as_ref());
        let mut artists = merge(local_artists, remote_items(remote_artists), limits.artists);
        let mut albums = merge(local_albums, remote_items(remote_albums), limits.albums);
        self.apply_flags(&mut artists, &mut albums).await;
        Ok(SearchResponse {
            top_artist: detect_top_result(&artists, &folded),
            top_album: detect_top_result(&albums, &folded),
            top_track: detect_top_result(&tracks, &folded),
            artists,
            albums,
            tracks,
            artist_status,
            album_status,
            track_status: SearchRemoteStatus::Ok,
        })
    }

    /// One page of one bucket. Artists and albums page through MusicBrainz
    /// (joined with library copies) and fall back to the library's own
    /// page when MusicBrainz fails with nothing stale to show; tracks page
    /// locally. The standout hit rides only the first page.
    pub async fn search_bucket(
        &self,
        bucket: Bucket,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchBucketResponse, sqlx::Error> {
        let folded = crate::db::fold::fold_text(query);
        let (results, status) = if folded.is_empty() {
            (Vec::new(), SearchRemoteStatus::Ok)
        } else {
            self.bucket_page(bucket, query, &folded, limit, offset)
                .await?
        };
        let top_result = if offset == 0 {
            detect_top_result(&results, &folded)
        } else {
            None
        };
        Ok(SearchBucketResponse {
            bucket: bucket.name().to_owned(),
            limit,
            offset,
            results,
            top_result,
            status,
        })
    }

    async fn bucket_page(
        &self,
        bucket: Bucket,
        query: &str,
        folded: &str,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<SearchResultItem>, SearchRemoteStatus), sqlx::Error> {
        let remote = self
            .remote_bucket(
                bucket,
                query,
                limit,
                offset,
                FULL_SEARCH_DEADLINE,
                bucket != Bucket::Tracks,
            )
            .await;
        let status = status_of(remote.as_ref());
        let answered = remote
            .as_ref()
            .is_some_and(|bucket| matches!(bucket.state, RemoteState::Ok | RemoteState::Stale));
        if !answered {
            let local = match bucket {
                Bucket::Artists => self.search_artists_page(folded, limit, offset).await?,
                Bucket::Albums => self.search_albums_page(folded, limit, offset).await?,
                Bucket::Tracks => self.search_tracks(folded, limit, offset).await?,
            };
            return Ok((local, status));
        }
        // Library copies of the hits on this page attach their ids; the
        // local query is bounded by the page, never the whole library.
        let local = match bucket {
            Bucket::Artists => self.search_artists_page(folded, MAX_LOCAL_JOIN, 0).await?,
            Bucket::Albums => self.search_albums_page(folded, MAX_LOCAL_JOIN, 0).await?,
            Bucket::Tracks => Vec::new(),
        };
        let mut items = remote_items(remote);
        attach_local(&mut items, &local);
        let (mut artists, mut albums) = match bucket {
            Bucket::Artists => (items, Vec::new()),
            _ => (Vec::new(), items),
        };
        self.apply_flags(&mut artists, &mut albums).await;
        artists.extend(albums);
        Ok((artists, status))
    }

    /// Merged typeahead across buckets, best first. MusicBrainz adds
    /// artists and albums (60% of the limit each, as in v2) under a short
    /// deadline. Short queries return empty; handlers enforce that first.
    pub async fn suggest(&self, query: &str, limit: u32) -> Result<SuggestResponse, sqlx::Error> {
        let folded = crate::db::fold::fold_text(query);
        if folded.trim().len() < 2 {
            return Ok(SuggestResponse {
                results: Vec::new(),
                status: SearchRemoteStatus::Ok,
            });
        }
        let remote_limit = (limit * 3).div_ceil(5);
        let local = async {
            Ok::<_, sqlx::Error>((
                self.search_artists(&folded, limit).await?,
                self.search_albums(&folded, limit).await?,
                self.search_tracks(&folded, limit, 0).await?,
            ))
        };
        let (local, remote_artists, remote_albums) = tokio::join!(
            local,
            self.remote_bucket(
                Bucket::Artists,
                query,
                remote_limit,
                0,
                SUGGEST_DEADLINE,
                true
            ),
            self.remote_bucket(
                Bucket::Albums,
                query,
                remote_limit,
                0,
                SUGGEST_DEADLINE,
                true
            ),
        );
        let (local_artists, local_albums, tracks) = local?;
        let status = worst_status(
            status_of(remote_artists.as_ref()),
            status_of(remote_albums.as_ref()),
        );
        let mut artists = merge(local_artists, remote_items(remote_artists), limit);
        let mut albums = merge(local_albums, remote_items(remote_albums), limit);
        self.apply_flags(&mut artists, &mut albums).await;
        let mut merged: Vec<SuggestResult> = artists
            .into_iter()
            .chain(albums)
            .chain(tracks)
            .map(SuggestResult::from_item)
            .collect();
        merged.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| kind_order(left.kind).cmp(&kind_order(right.kind)))
                .then_with(|| left.title.cmp(&right.title))
        });
        merged.truncate(limit as usize);
        Ok(SuggestResponse {
            results: merged,
            status,
        })
    }

    /// One MusicBrainz bucket, or `None` when MusicBrainz search is not
    /// wired or the bucket is not wanted.
    async fn remote_bucket(
        &self,
        bucket: Bucket,
        query: &str,
        limit: u32,
        offset: u32,
        deadline: Duration,
        wanted: bool,
    ) -> Option<RemoteBucket> {
        let catalog = self.remote.as_ref().filter(|_| wanted && limit > 0)?;
        match bucket {
            Bucket::Artists => Some(
                catalog
                    .search_artists_remote(query, limit, offset, deadline)
                    .await,
            ),
            Bucket::Albums => Some(
                catalog
                    .search_albums_remote(query, limit, offset, deadline)
                    .await,
            ),
            Bucket::Tracks => None,
        }
    }

    /// Fill in library flags for provider hits: owned artists and albums,
    /// and open requests for albums the library lacks.
    async fn apply_flags(&self, artists: &mut [SearchResultItem], albums: &mut [SearchResultItem]) {
        let Some(catalog) = &self.remote else {
            return;
        };
        let ids = |items: &[SearchResultItem]| -> Vec<String> {
            items
                .iter()
                .filter(|item| item.id.is_none())
                .filter_map(|item| item.musicbrainz_id.clone())
                .collect()
        };
        let (owned_artists, owned_albums, requested) =
            catalog.search_flags(&ids(artists), &ids(albums)).await;
        for item in artists.iter_mut().filter(|item| item.id.is_none()) {
            let mbid = item
                .musicbrainz_id
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            item.in_library = owned_artists.contains(&mbid);
        }
        for item in albums.iter_mut().filter(|item| item.id.is_none()) {
            let mbid = item
                .musicbrainz_id
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase();
            item.in_library = owned_albums.contains(&mbid);
            item.requested = !item.in_library && requested.contains(&mbid);
        }
    }

    /// Best-first artist hits, capped.
    async fn search_artists(
        &self,
        folded: &str,
        limit: u32,
    ) -> Result<Vec<SearchResultItem>, sqlx::Error> {
        self.search_artists_page(folded, limit, 0).await
    }

    /// One page of artist hits: live rows whose folded name contains the
    /// folded query, exact first, then prefix, then alphabetical.
    async fn search_artists_page(
        &self,
        folded: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchResultItem>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT a.id, a.display_name, a.folded_name, e.provider_artist_id AS mbid \
             FROM local_artists a \
             LEFT JOIN local_artist_external_identities e \
             ON e.local_artist_id = a.id AND e.provider = 'musicbrainz' \
             WHERE a.retired_into_artist_id IS NULL \
             AND a.folded_name LIKE ? ESCAPE '\\' \
             ORDER BY CASE WHEN a.folded_name = ? THEN 0 \
             WHEN a.folded_name LIKE ? ESCAPE '\\' THEN 1 ELSE 2 END, \
             a.folded_name ASC LIMIT ? OFFSET ?",
        )
        .bind(like_contains(folded))
        .bind(folded)
        .bind(like_prefix(folded))
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let name_folded: String = row.get("folded_name");
                SearchResultItem {
                    kind: SearchKind::Artist,
                    id: Some(row.get("id")),
                    title: row.get("display_name"),
                    artist: None,
                    year: None,
                    musicbrainz_id: row.get("mbid"),
                    in_library: true,
                    requested: false,
                    score: match_score(&name_folded, folded),
                    disambiguation: None,
                    type_info: None,
                }
            })
            .collect())
    }

    /// Best-first album hits, capped.
    async fn search_albums(
        &self,
        folded: &str,
        limit: u32,
    ) -> Result<Vec<SearchResultItem>, sqlx::Error> {
        self.search_albums_page(folded, limit, 0).await
    }

    /// One page of album hits: live rows whose folded title or folded
    /// album-artist name contains the query, exact title first.
    async fn search_albums_page(
        &self,
        folded: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchResultItem>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT b.id, b.title, b.title_folded, b.album_artist_name, b.year, \
             e.release_group_mbid AS mbid \
             FROM local_albums b \
             LEFT JOIN local_album_external_identities e \
             ON e.local_album_id = b.id AND e.provider = 'musicbrainz' \
             WHERE b.retired_into_album_id IS NULL \
             AND (b.title_folded LIKE ? ESCAPE '\\' \
             OR COALESCE(b.album_artist_name_folded, '') LIKE ? ESCAPE '\\') \
             ORDER BY CASE WHEN b.title_folded = ? THEN 0 \
             WHEN b.title_folded LIKE ? ESCAPE '\\' THEN 1 ELSE 2 END, \
             b.title_folded ASC LIMIT ? OFFSET ?",
        )
        .bind(like_contains(folded))
        .bind(like_contains(folded))
        .bind(folded)
        .bind(like_prefix(folded))
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let title_folded: String = row.get("title_folded");
                SearchResultItem {
                    kind: SearchKind::Album,
                    id: Some(row.get("id")),
                    title: row.get("title"),
                    artist: row.get("album_artist_name"),
                    year: row.get("year"),
                    musicbrainz_id: row.get("mbid"),
                    in_library: true,
                    requested: false,
                    score: match_score(&title_folded, folded),
                    disambiguation: None,
                    type_info: None,
                }
            })
            .collect())
    }

    /// One page of track hits: indexed rows whose folded title, artist, or
    /// album contains the query, exact title first.
    async fn search_tracks(
        &self,
        folded: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchResultItem>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT t.id, t.title, t.title_folded, t.artist_name, t.year, \
             e.recording_mbid AS mbid \
             FROM local_tracks t \
             LEFT JOIN local_track_external_identities e \
             ON e.local_track_id = t.id AND e.provider = 'musicbrainz' \
             WHERE t.availability = 'indexed' \
             AND (t.title_folded LIKE ? ESCAPE '\\' \
             OR COALESCE(t.artist_name_folded, '') LIKE ? ESCAPE '\\' \
             OR t.album_title_folded LIKE ? ESCAPE '\\') \
             ORDER BY CASE WHEN t.title_folded = ? THEN 0 \
             WHEN t.title_folded LIKE ? ESCAPE '\\' THEN 1 ELSE 2 END, \
             t.title_folded ASC LIMIT ? OFFSET ?",
        )
        .bind(like_contains(folded))
        .bind(like_contains(folded))
        .bind(like_contains(folded))
        .bind(folded)
        .bind(like_prefix(folded))
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let title_folded: String = row.get("title_folded");
                SearchResultItem {
                    kind: SearchKind::Track,
                    id: Some(row.get("id")),
                    title: row.get("title"),
                    artist: row.get("artist_name"),
                    year: row.get("year"),
                    musicbrainz_id: row.get("mbid"),
                    in_library: true,
                    requested: false,
                    score: match_score(&title_folded, folded),
                    disambiguation: None,
                    type_info: None,
                }
            })
            .collect())
    }
}

/// Enrich one mixed batch through the port. A provider failure degrades
/// into a typed note inside a successful response: ids echo back with
/// absent counts and source `none`. The cause goes to the log only.
pub async fn enrich_with_degradation(
    port: &dyn EnrichmentPort,
    ids: &dyn crate::ids::IdGenerator,
    request: EnrichmentBatchRequest,
) -> EnrichmentResponse {
    let artists: Vec<super::models::ArtistEnrichmentRequest> = request
        .artists
        .into_iter()
        .filter(|item| !item.musicbrainz_id.trim().is_empty())
        .take(MAX_ENRICHMENT_PER_BUCKET)
        .collect();
    let albums: Vec<super::models::AlbumEnrichmentRequest> = request
        .albums
        .into_iter()
        .filter(|item| !item.musicbrainz_id.trim().is_empty())
        .take(MAX_ENRICHMENT_PER_BUCKET)
        .collect();
    let trimmed = EnrichmentBatchRequest {
        artists: artists.clone(),
        albums: albums.clone(),
    };
    match port.enrich_batch(trimmed).await {
        Ok(response) => response,
        Err(error) => {
            let error_id = ids.new_id();
            tracing::error!(error_id, source = %error.source, cause = %error.message, "search enrichment degraded");
            EnrichmentResponse {
                artists: artists
                    .into_iter()
                    .map(|item| super::models::ArtistEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        release_group_count: None,
                        listen_count: None,
                    })
                    .collect(),
                albums: albums
                    .into_iter()
                    .map(|item| super::models::AlbumEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        track_count: None,
                        listen_count: None,
                    })
                    .collect(),
                source: EnrichmentSource::None,
                degradations: vec![Degradation {
                    source: error.source,
                    code: "ENRICHMENT_UNAVAILABLE".to_owned(),
                    message: "Enrichment temporarily unavailable".to_owned(),
                }],
            }
        }
    }
}

/// LIKE pattern matching rows containing the folded query. Wildcards in
/// the query match literally, never as patterns.
fn like_contains(folded: &str) -> String {
    format!("%{}%", escape_like(folded))
}

/// LIKE pattern matching rows starting with the folded query.
fn like_prefix(folded: &str) -> String {
    format!("{}%", escape_like(folded))
}

/// Escape the LIKE metacharacters so query text matches literally.
fn escape_like(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Score one folded title against the folded query: exact is 100, prefix
/// is 90, anything else that matched is a substring at 70.
fn match_score(title_folded: &str, query_folded: &str) -> i32 {
    if title_folded == query_folded {
        SCORE_EXACT
    } else if title_folded.starts_with(query_folded) {
        SCORE_PREFIX
    } else {
        SCORE_SUBSTRING
    }
}

/// Sort order for merged suggestions: artists, then albums, then tracks.
fn kind_order(kind: SearchKind) -> u8 {
    match kind {
        SearchKind::Artist => 0,
        SearchKind::Album => 1,
        SearchKind::Track => 2,
    }
}

/// The standout hit of a ranked list, ported from v2: the best hit
/// headlines only when it scores 90+ and its tokens overlap the query's,
/// allowing prefix matches for partial typeahead.
fn detect_top_result(items: &[SearchResultItem], query_folded: &str) -> Option<SearchResultItem> {
    let best = items.first()?;
    if best.score < SCORE_PREFIX {
        return None;
    }
    let title_folded = crate::db::fold::fold_text(&best.title);
    let query_tokens = search_tokens(query_folded);
    let title_tokens = search_tokens(&title_folded);
    if query_tokens.is_empty() || title_tokens.is_empty() {
        return None;
    }
    if tokens_match(&query_tokens, &title_tokens) {
        Some(best.clone())
    } else {
        None
    }
}

/// Tokenize for top-result overlap: folded text split on whitespace with
/// non-alphanumeric runs dropped, kept from v2's normalizer.
fn search_tokens(folded: &str) -> Vec<String> {
    folded
        .split_whitespace()
        .map(|token| token.chars().filter(|c| c.is_alphanumeric()).collect())
        .filter(|token: &String| !token.is_empty())
        .collect()
}

/// Token overlap allowing prefix matches in either direction, kept from v2.
fn tokens_match(query_tokens: &[String], title_tokens: &[String]) -> bool {
    const MIN_PREFIX: usize = 2;
    let covers = |needles: &[String], haystack: &[String]| {
        needles.iter().all(|needle| {
            haystack.iter().any(|candidate| {
                needle == candidate || (needle.len() >= MIN_PREFIX && candidate.starts_with(needle))
            })
        })
    };
    covers(query_tokens, title_tokens) || covers(title_tokens, query_tokens)
}

impl SuggestResult {
    /// Shrink a full hit to its typeahead shape.
    fn from_item(item: SearchResultItem) -> Self {
        Self {
            kind: item.kind,
            title: item.title,
            artist: item.artist,
            year: item.year,
            id: item.id,
            musicbrainz_id: item.musicbrainz_id,
            in_library: item.in_library,
            requested: item.requested,
            disambiguation: item.disambiguation,
            score: item.score,
        }
    }
}

/// Library hits fetched to join one MusicBrainz drill-down page.
const MAX_LOCAL_JOIN: u32 = 100;

/// MusicBrainz hits as unflagged search rows.
fn remote_items(bucket: Option<RemoteBucket>) -> Vec<SearchResultItem> {
    bucket
        .map(|bucket| bucket.hits.into_iter().map(remote_item).collect())
        .unwrap_or_default()
}

fn remote_item(hit: RemoteHit) -> SearchResultItem {
    SearchResultItem {
        kind: match hit.kind {
            RemoteKind::Artist => SearchKind::Artist,
            RemoteKind::Album => SearchKind::Album,
        },
        id: None,
        title: hit.title,
        artist: hit.artist,
        year: hit.year,
        musicbrainz_id: Some(hit.mbid),
        in_library: false,
        requested: false,
        score: hit.score,
        disambiguation: hit.disambiguation,
        type_info: hit.type_info,
    }
}

/// Give each MusicBrainz hit the id of its library copy, when one exists.
/// Returns which library rows were claimed.
fn attach_local(remote: &mut [SearchResultItem], local: &[SearchResultItem]) -> Vec<bool> {
    let mut claimed = vec![false; local.len()];
    for hit in remote.iter_mut() {
        let Some(mbid) = hit.musicbrainz_id.as_deref() else {
            continue;
        };
        if let Some(index) = local.iter().position(|row| {
            row.musicbrainz_id
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case(mbid))
        }) {
            hit.id = local[index].id.clone();
            hit.in_library = true;
            claimed[index] = true;
        }
    }
    claimed
}

/// Join MusicBrainz hits with library hits: library copies attach to their
/// MusicBrainz hit, the rest of the library hits join the list, and the
/// whole bucket ranks by score (MusicBrainz first on ties) up to `limit`.
fn merge(
    local: Vec<SearchResultItem>,
    mut remote: Vec<SearchResultItem>,
    limit: u32,
) -> Vec<SearchResultItem> {
    let claimed = attach_local(&mut remote, &local);
    remote.extend(
        local
            .into_iter()
            .zip(claimed)
            .filter(|(_, claimed)| !claimed)
            .map(|(row, _)| row),
    );
    remote.sort_by(|left, right| right.score.cmp(&left.score));
    remote.truncate(limit as usize);
    remote
}

/// The wire status of one MusicBrainz bucket. No bucket (not wired, not
/// wanted) reads as `ok`.
fn status_of(bucket: Option<&RemoteBucket>) -> SearchRemoteStatus {
    match bucket.map(|bucket| bucket.state) {
        None | Some(RemoteState::Ok) => SearchRemoteStatus::Ok,
        Some(RemoteState::Stale) => SearchRemoteStatus::Stale,
        Some(RemoteState::Timeout) => SearchRemoteStatus::Timeout,
        Some(RemoteState::Error) => SearchRemoteStatus::Error,
    }
}

/// The worse of two bucket statuses, for the single typeahead status.
fn worst_status(left: SearchRemoteStatus, right: SearchRemoteStatus) -> SearchRemoteStatus {
    let rank = |status: SearchRemoteStatus| match status {
        SearchRemoteStatus::Ok => 0,
        SearchRemoteStatus::Stale => 1,
        SearchRemoteStatus::Partial => 2,
        SearchRemoteStatus::Timeout => 3,
        SearchRemoteStatus::Error => 4,
    };
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}
