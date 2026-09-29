//! Local catalog search over the 0001 `local_*` tables.
//!
//! Matching is accent- and case-insensitive through the baseline folded
//! columns (`folded_name`, `title_folded`, ...): the query is folded with
//! the same [`fold_text`](droppedneedle::db::fold::fold_text) the writers
//! use, so a keyboard without accents still finds the artist. Retired
//! (merged-away) artists and albums never match, and tracks outside the
//! `indexed` availability never match. Empty results are absence, never
//! failure. Provider-backed results join these same handlers in stage 5.

use sqlx::{Row, SqlitePool};

use super::models::{
    Degradation, EnrichmentBatchRequest, EnrichmentResponse, EnrichmentSource,
    SearchBucketResponse, SearchKind, SearchResponse, SearchResultItem, SuggestResponse,
    SuggestResult,
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

/// Local search over one SQLite pool. Clone shares the pool handle.
#[derive(Debug, Clone)]
pub struct SearchService {
    pool: SqlitePool,
}

impl SearchService {
    /// Serve searches from this pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Ranked hits per selected bucket plus each bucket's standout hit.
    /// A blank-after-fold query matches nothing and returns empty lists.
    pub async fn search(
        &self,
        query: &str,
        limits: BucketLimits,
        buckets: &[Bucket],
    ) -> Result<SearchResponse, sqlx::Error> {
        let folded = droppedneedle::db::fold::fold_text(query);
        if folded.is_empty() {
            return Ok(SearchResponse {
                artists: Vec::new(),
                albums: Vec::new(),
                tracks: Vec::new(),
                top_artist: None,
                top_album: None,
                top_track: None,
            });
        }
        let mut artists = Vec::new();
        let mut albums = Vec::new();
        let mut tracks = Vec::new();
        if buckets.contains(&Bucket::Artists) {
            artists = self.search_artists(&folded, limits.artists).await?;
        }
        if buckets.contains(&Bucket::Albums) {
            albums = self.search_albums(&folded, limits.albums).await?;
        }
        if buckets.contains(&Bucket::Tracks) {
            tracks = self.search_tracks(&folded, limits.tracks, 0).await?;
        }
        Ok(SearchResponse {
            top_artist: detect_top_result(&artists, &folded),
            top_album: detect_top_result(&albums, &folded),
            top_track: detect_top_result(&tracks, &folded),
            artists,
            albums,
            tracks,
        })
    }

    /// One page of one bucket. The standout hit rides only the first page.
    pub async fn search_bucket(
        &self,
        bucket: Bucket,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchBucketResponse, sqlx::Error> {
        let folded = droppedneedle::db::fold::fold_text(query);
        let results = if folded.is_empty() {
            Vec::new()
        } else {
            match bucket {
                Bucket::Artists => self.search_artists_page(&folded, limit, offset).await?,
                Bucket::Albums => self.search_albums_page(&folded, limit, offset).await?,
                Bucket::Tracks => self.search_tracks(&folded, limit, offset).await?,
            }
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
        })
    }

    /// Merged typeahead across buckets, best first. Short queries return
    /// empty here too; handlers enforce that before calling.
    pub async fn suggest(&self, query: &str, limit: u32) -> Result<SuggestResponse, sqlx::Error> {
        let folded = droppedneedle::db::fold::fold_text(query);
        if folded.trim().len() < 2 {
            return Ok(SuggestResponse {
                results: Vec::new(),
            });
        }
        let mut merged: Vec<SuggestResult> = Vec::new();
        for item in self.search_artists(&folded, limit).await? {
            merged.push(SuggestResult::from_item(item));
        }
        for item in self.search_albums(&folded, limit).await? {
            merged.push(SuggestResult::from_item(item));
        }
        for item in self.search_tracks(&folded, limit, 0).await? {
            merged.push(SuggestResult::from_item(item));
        }
        merged.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| kind_order(left.kind).cmp(&kind_order(right.kind)))
                .then_with(|| left.title.cmp(&right.title))
        });
        merged.truncate(limit as usize);
        Ok(SuggestResponse { results: merged })
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
                    id: row.get("id"),
                    title: row.get("display_name"),
                    artist: None,
                    year: None,
                    musicbrainz_id: row.get("mbid"),
                    in_library: true,
                    requested: false,
                    score: match_score(&name_folded, folded),
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
                    id: row.get("id"),
                    title: row.get("title"),
                    artist: row.get("album_artist_name"),
                    year: row.get("year"),
                    musicbrainz_id: row.get("mbid"),
                    in_library: true,
                    requested: false,
                    score: match_score(&title_folded, folded),
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
                    id: row.get("id"),
                    title: row.get("title"),
                    artist: row.get("artist_name"),
                    year: row.get("year"),
                    musicbrainz_id: row.get("mbid"),
                    in_library: true,
                    requested: false,
                    score: match_score(&title_folded, folded),
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
    ids: &dyn droppedneedle::ids::IdGenerator,
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
    let title_folded = droppedneedle::db::fold::fold_text(&best.title);
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
            id: item.id,
            musicbrainz_id: item.musicbrainz_id,
            score: item.score,
        }
    }
}
