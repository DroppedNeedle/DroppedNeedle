//! Unified search: the library's `local_*` tables joined with MusicBrainz.
//!
//! MusicBrainz answers artists and albums the way v2 searched them, under
//! a deadline and with a stale copy standing in when it is down; library
//! copies attach their local id and the in-library flag, and library rows
//! MusicBrainz did not return still show. Every list ranks through one
//! [`rank`]: a library row that matches exactly or as a prefix comes
//! first, everything else goes by score. A dead MusicBrainz never fails a
//! search: the library hits come back with the bucket status saying why
//! the rest is missing.
//!
//! Local matching is accent- and case-insensitive through the baseline folded
//! columns (`folded_name`, `title_folded`, ...): the query is folded with
//! the same [`fold_text`](droppedneedle_kernel::db::fold::fold_text) the writers
//! use, so a keyboard without accents still finds the artist. Retired
//! (merged-away) artists and albums never match, and tracks outside the
//! `indexed` availability never match. Empty results are absence, never
//! failure.

use std::time::Duration;

use sqlx::{Row, SqlitePool};

use crate::reads::catalog::Catalog;
use crate::reads::catalog::search::{RemoteBucket, RemoteHit, RemoteKind, RemoteState};
use crate::reads::library::definitions::{musicbrainz, owned_album, owned_artist, recording_mbid};

use super::models::{
    SearchBucketResponse, SearchKind, SearchRemoteStatus, SearchResponse, SearchResultItem,
    SearchScope, SearchTrackDetail, SuggestResponse, SuggestResult,
};

/// Score for an exact folded-title match. Also the top-result bar, kept
/// from v2 (`TOP_RESULT_SCORE_THRESHOLD`).
pub const SCORE_EXACT: i32 = 100;
/// Score for a folded-prefix match. Meets the top-result bar.
pub const SCORE_PREFIX: i32 = 90;
/// Score for a folded-substring match. Ranks, never headlines.
pub const SCORE_SUBSTRING: i32 = 70;
/// Score for a song matched only through its artist or album words.
pub const SCORE_OTHER_FIELD: i32 = 60;

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
    /// local only. The library scope leaves MusicBrainz out. A
    /// blank-after-fold query matches nothing.
    pub async fn search(
        &self,
        query: &str,
        limits: BucketLimits,
        buckets: &[Bucket],
        scope: SearchScope,
    ) -> Result<SearchResponse, sqlx::Error> {
        let folded = droppedneedle_kernel::db::fold::fold_text(query);
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
        let remote_wants =
            |bucket: Bucket, limit: u32| scope == SearchScope::All && wants(bucket, limit);
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
            remote_wants(Bucket::Artists, limits.artists),
        );
        let remote_albums = self.remote_bucket(
            Bucket::Albums,
            query,
            limits.albums,
            0,
            FULL_SEARCH_DEADLINE,
            remote_wants(Bucket::Albums, limits.albums),
        );
        let (local, remote_artists, remote_albums) =
            tokio::join!(local, remote_artists, remote_albums);
        let (local_artists, local_albums, tracks) = local?;
        let artist_status = status_of(remote_artists.as_ref());
        let album_status = status_of(remote_albums.as_ref());
        let mut artists = join(local_artists, remote_items(remote_artists));
        let mut albums = join(local_albums, remote_items(remote_albums));
        self.apply_flags(&mut artists, &mut albums).await;
        let artists = best(artists, limits.artists);
        let albums = best(albums, limits.albums);
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
    /// joined with the library page at the same offset, and fall back to
    /// the library's own page when MusicBrainz fails with nothing stale to
    /// show; tracks page locally. The standout hit rides only the first
    /// page.
    pub async fn search_bucket(
        &self,
        bucket: Bucket,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchBucketResponse, sqlx::Error> {
        let folded = droppedneedle_kernel::db::fold::fold_text(query);
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
                Bucket::Artists => {
                    one_per_mbid(self.search_artists_page(folded, limit, offset).await?)
                }
                Bucket::Albums => {
                    one_per_mbid(self.search_albums_page(folded, limit, offset).await?)
                }
                Bucket::Tracks => self.search_tracks(folded, limit, offset).await?,
            };
            return Ok((local, status));
        }
        // Library copies of the hits on this page attach their ids; the
        // local query is bounded, never the whole library.
        let (joined, page) = match bucket {
            Bucket::Artists => {
                let joined = self.search_artists_page(folded, MAX_LOCAL_JOIN, 0).await?;
                let page = match slice_of(&joined, limit, offset) {
                    Some(page) => page,
                    None => self.search_artists_page(folded, limit, offset).await?,
                };
                (joined, page)
            }
            Bucket::Albums => {
                let joined = self.search_albums_page(folded, MAX_LOCAL_JOIN, 0).await?;
                let page = match slice_of(&joined, limit, offset) {
                    Some(page) => page,
                    None => self.search_albums_page(folded, limit, offset).await?,
                };
                (joined, page)
            }
            Bucket::Tracks => (Vec::new(), Vec::new()),
        };
        let mut candidates = join_page(&joined, page, remote_items(remote));
        let mut none = Vec::new();
        let (artists, albums) = match bucket {
            Bucket::Artists => (&mut candidates, &mut none),
            _ => (&mut none, &mut candidates),
        };
        self.apply_flags(artists, albums).await;
        let items = ranked(candidates)
            .into_iter()
            .map(|(_, item)| item)
            .collect();
        Ok((items, status))
    }

    /// Merged typeahead across buckets, best first. MusicBrainz adds
    /// artists and albums (60% of the limit each, as in v2) under a short
    /// deadline. Short queries return empty; handlers enforce that first.
    pub async fn suggest(&self, query: &str, limit: u32) -> Result<SuggestResponse, sqlx::Error> {
        let folded = droppedneedle_kernel::db::fold::fold_text(query);
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
        let mut artists = join(local_artists, remote_items(remote_artists));
        let mut albums = join(local_albums, remote_items(remote_albums));
        self.apply_flags(&mut artists, &mut albums).await;
        let tracks = tracks.into_iter().map(Candidate::local).collect();
        let mut merged: Vec<(Rank, SuggestResult)> = [artists, albums, tracks]
            .into_iter()
            .flat_map(|bucket| {
                let mut bucket = ranked(bucket);
                bucket.truncate(limit as usize);
                bucket
            })
            .map(|(rank, item)| (rank, SuggestResult::from_item(item)))
            .collect();
        merged.sort_by_cached_key(|(rank, item)| {
            (
                std::cmp::Reverse(*rank),
                kind_order(item.kind),
                droppedneedle_kernel::db::fold::fold_text(&item.title),
            )
        });
        merged.truncate(limit as usize);
        Ok(SuggestResponse {
            results: merged.into_iter().map(|(_, item)| item).collect(),
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
    async fn apply_flags(&self, artists: &mut [Candidate], albums: &mut [Candidate]) {
        let Some(catalog) = &self.remote else {
            return;
        };
        let ids = |rows: &[Candidate]| -> Vec<String> {
            rows.iter()
                .filter(|row| row.item.id.is_none())
                .filter_map(|row| row.item.musicbrainz_id.clone())
                .collect()
        };
        let (owned_artists, owned_albums, requested) =
            catalog.search_flags(&ids(artists), &ids(albums)).await;
        let mbid = |item: &SearchResultItem| {
            item.musicbrainz_id
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
        };
        for row in artists.iter_mut().filter(|row| row.item.id.is_none()) {
            row.item.in_library = owned_artists.contains(&mbid(&row.item));
        }
        for row in albums.iter_mut().filter(|row| row.item.id.is_none()) {
            let mbid = mbid(&row.item);
            row.item.in_library = owned_albums.contains(&mbid);
            row.item.requested = !row.item.in_library && requested.contains(&mbid);
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
        let rows = sqlx::query(concat!(
            "SELECT a.id, a.display_name, a.folded_name, e.provider_artist_id AS mbid, (",
            owned_artist!("a"),
            ") AS owned FROM local_artists a \
             LEFT JOIN local_artist_external_identities e ON e.local_artist_id = a.id AND ",
            musicbrainz!("e"),
            " WHERE a.retired_into_artist_id IS NULL \
             AND a.folded_name LIKE ? ESCAPE '\\' \
             ORDER BY CASE WHEN a.folded_name = ? THEN 0 \
             WHEN a.folded_name LIKE ? ESCAPE '\\' THEN 1 ELSE 2 END, \
             a.folded_name ASC LIMIT ? OFFSET ?"
        ))
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
                    in_library: row.get("owned"),
                    requested: false,
                    score: match_score(&name_folded, folded),
                    disambiguation: None,
                    type_info: None,
                    track: None,
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

    /// One page of album hits: owned albums whose folded title or folded
    /// album-artist name contains the query, exact title first. An album
    /// without files is found in the MusicBrainz half, as not in library.
    async fn search_albums_page(
        &self,
        folded: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchResultItem>, sqlx::Error> {
        let rows = sqlx::query(concat!(
            "SELECT b.id, b.title, b.title_folded, b.album_artist_name, b.year, \
             e.release_group_mbid AS mbid \
             FROM local_albums b \
             LEFT JOIN local_album_external_identities e ON e.local_album_id = b.id AND ",
            musicbrainz!("e"),
            " WHERE ",
            owned_album!("b"),
            " AND (b.title_folded LIKE ? ESCAPE '\\' \
             OR COALESCE(b.album_artist_name_folded, '') LIKE ? ESCAPE '\\') \
             ORDER BY CASE WHEN b.title_folded = ? THEN 0 \
             WHEN b.title_folded LIKE ? ESCAPE '\\' THEN 1 ELSE 2 END, \
             b.title_folded ASC LIMIT ? OFFSET ?"
        ))
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
                    track: None,
                }
            })
            .collect())
    }

    /// One page of song hits, ranked by [`song_query`]. The page is cut on
    /// `local_tracks` alone; identities, artwork and the track artist are
    /// read for the page's rows only.
    async fn search_tracks(
        &self,
        folded: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SearchResultItem>, sqlx::Error> {
        let (sql, binds) = song_query(folded, limit, offset);
        let mut query = sqlx::query(&sql);
        for bind in binds {
            query = match bind {
                SongBind::Text(text) => query.bind(text),
                SongBind::Int(number) => query.bind(number),
            };
        }
        let rows = query.fetch_all(&self.pool).await?;
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
                    score: song_score(&title_folded, folded),
                    disambiguation: None,
                    type_info: None,
                    track: Some(SearchTrackDetail {
                        album_id: row.get("local_album_id"),
                        album_title: row.get("album_title"),
                        artist_id: row.get("artist_id"),
                        release_group_mbid: row.get("release_group_mbid"),
                        disc_number: row.get("disc_number"),
                        track_number: row.get("track_number"),
                        duration_seconds: row.get("duration_seconds"),
                        format: row.get("file_format"),
                        cover_available: row.get("cover_available"),
                    }),
                }
            })
            .collect())
    }
}

/// One bind of [`song_query`]'s statement, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SongBind {
    /// A text value.
    Text(String),
    /// An integer value.
    Int(i64),
}

/// The song search statement for one page and its binds, in order.
///
/// A query with a word of 3+ characters goes through the FTS index: every
/// word must appear in the title, artist or album. One with only shorter
/// words ("U2", "xx") matches a title prefix or an exact artist name, each
/// through its own index: the prefix walk stops after the page's last row,
/// since every prefix match outranks every other match and the walk runs in
/// rank order. Either way the candidates drive the read: the availability
/// test is kept off its index (`+`), which would otherwise walk every
/// streamable track. Songs rank exact title, title prefix, title
/// containing the query, then songs matched only through their artist or
/// album; ties go by title, then id, so offset pages never repeat or skip
/// a row.
pub(crate) fn song_query(folded: &str, limit: u32, offset: u32) -> (String, Vec<SongBind>) {
    let search = crate::reads::library::sqlite::TextSearch::parse(folded);
    let mut binds: Vec<SongBind> = [
        folded.to_owned(),
        like_prefix(folded),
        like_contains(folded),
    ]
    .into_iter()
    .map(SongBind::Text)
    .collect();
    let text = match &search {
        Some(search) => {
            binds.extend(search.binds().into_iter().map(SongBind::Text));
            search.sql()
        }
        None => {
            binds.extend([
                SongBind::Text(folded.to_owned()),
                SongBind::Text(format!("{folded}\u{10FFFF}")),
                SongBind::Int(i64::from(limit) + i64::from(offset)),
                SongBind::Text(folded.to_owned()),
            ]);
            "t.rowid IN (SELECT rowid FROM (SELECT rowid FROM local_tracks \
             WHERE availability = 'indexed' AND title_folded >= ? AND title_folded < ? \
             ORDER BY title_folded, id LIMIT ?) \
             UNION SELECT rowid FROM local_tracks \
             WHERE availability = 'indexed' AND COALESCE(artist_name_folded, '') = ?)"
                .to_owned()
        }
    };
    binds.extend([
        SongBind::Int(i64::from(limit)),
        SongBind::Int(i64::from(offset)),
    ]);
    let order = "tier, t.title_folded, t.id";
    let sql = format!(
        "SELECT t.id, t.title, t.title_folded, t.artist_name, t.year, t.local_album_id, \
         t.album_title, t.disc_number, t.track_number, t.duration_seconds, t.file_format, \
         {recording} AS mbid, ae.release_group_mbid AS release_group_mbid, \
         (w.local_album_id IS NOT NULL) AS cover_available, \
         (SELECT ta.local_artist_id FROM local_track_artists ta \
          WHERE ta.local_track_id = t.id ORDER BY ta.position LIMIT 1) AS artist_id \
         FROM (SELECT t.rowid AS hit, \
               CASE WHEN t.title_folded = ? THEN 0 \
               WHEN t.title_folded LIKE ? ESCAPE '\\' THEN 1 \
               WHEN t.title_folded LIKE ? ESCAPE '\\' THEN 2 ELSE 3 END AS tier \
               FROM local_tracks t \
               WHERE +t.availability = 'indexed' AND {text} \
               ORDER BY {order} LIMIT ? OFFSET ?) p \
         JOIN local_tracks t ON t.rowid = p.hit \
         LEFT JOIN local_track_external_identities e \
         ON e.local_track_id = t.id AND {provider} \
         LEFT JOIN local_album_external_identities ae \
         ON ae.local_album_id = t.local_album_id AND {album_provider} \
         LEFT JOIN local_album_artwork w ON w.local_album_id = t.local_album_id \
         ORDER BY p.{order}",
        recording = recording_mbid!("e", "t"),
        provider = musicbrainz!("e"),
        album_provider = musicbrainz!("ae"),
    );
    (sql, binds)
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

/// Score one song's folded title against the folded query, matching the
/// tiers [`song_query`] ranks by.
fn song_score(title_folded: &str, query_folded: &str) -> i32 {
    if title_folded.contains(query_folded) {
        match_score(title_folded, query_folded)
    } else {
        SCORE_OTHER_FIELD
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
    let title_folded = droppedneedle_kernel::db::fold::fold_text(&best.title);
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
        track: None,
    }
}

/// Where a hit ranks in its bucket; a higher rank sorts first. Search,
/// "see all" and suggest all rank through [`rank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Rank {
    /// A library copy that matched exactly or as a prefix: above every
    /// row that lacks one.
    boosted: bool,
    /// The better of the library tier and the MusicBrainz score.
    score: i32,
    /// The library holds it: wins a tie.
    library: bool,
}

/// Rank one hit from its library match tier (one of the `SCORE_*` values,
/// when a library row matched), its MusicBrainz score (when MusicBrainz
/// returned it) and whether the library holds it. A library row that
/// matches exactly or as a prefix ranks above every other row; the rest,
/// substring matches included, go by the better of the two scores, and a
/// tie goes to the library. Equal ranks are then ordered by title.
pub(crate) fn rank(local_tier: Option<i32>, remote_score: Option<i32>, in_library: bool) -> Rank {
    Rank {
        boosted: local_tier.is_some_and(|tier| tier >= SCORE_PREFIX),
        score: local_tier.max(remote_score).unwrap_or(0),
        library: in_library || local_tier.is_some(),
    }
}

/// A hit before ranking, with what matched it on each side.
#[derive(Debug, Clone)]
struct Candidate {
    item: SearchResultItem,
    /// Library match tier, when a library row matched.
    local_tier: Option<i32>,
    /// MusicBrainz relevance, when MusicBrainz returned it.
    remote_score: Option<i32>,
}

impl Candidate {
    /// A library row; its score is its match tier.
    fn local(item: SearchResultItem) -> Self {
        Self {
            local_tier: Some(item.score),
            remote_score: None,
            item,
        }
    }

    fn rank(&self) -> Rank {
        rank(self.local_tier, self.remote_score, self.item.in_library)
    }
}

/// Rank a bucket, best first: each row's score becomes its ranked score,
/// and equal ranks go by folded title.
fn ranked(candidates: Vec<Candidate>) -> Vec<(Rank, SearchResultItem)> {
    let mut rows: Vec<(Rank, String, SearchResultItem)> = candidates
        .into_iter()
        .map(|candidate| {
            let rank = candidate.rank();
            let mut item = candidate.item;
            item.score = rank.score;
            let title = droppedneedle_kernel::db::fold::fold_text(&item.title);
            (rank, title, item)
        })
        .collect();
    rows.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    rows.into_iter()
        .map(|(rank, _, item)| (rank, item))
        .collect()
}

/// The best `limit` rows of a bucket.
fn best(candidates: Vec<Candidate>, limit: u32) -> Vec<SearchResultItem> {
    ranked(candidates)
        .into_iter()
        .take(limit as usize)
        .map(|(_, item)| item)
        .collect()
}

/// Give each MusicBrainz hit the id and match tier of its library copy,
/// when one exists. Returns the hits and which library rows were claimed:
/// every copy of a release group that has a MusicBrainz hit is claimed,
/// not just the first, so a library holding two editions of one album
/// never adds a second row for it.
fn attach_local(
    remote: Vec<SearchResultItem>,
    local: &[SearchResultItem],
) -> (Vec<Candidate>, Vec<bool>) {
    let mut claimed = vec![false; local.len()];
    let hits = remote
        .into_iter()
        .map(|mut hit| {
            let remote_score = Some(hit.score);
            let mut local_tier = None;
            let mbid = hit
                .musicbrainz_id
                .as_deref()
                .map(str::trim)
                .filter(|mbid| !mbid.is_empty())
                .map(str::to_owned);
            if let Some(mbid) = mbid {
                for (index, row) in local.iter().enumerate() {
                    let same = row
                        .musicbrainz_id
                        .as_deref()
                        .is_some_and(|id| id.trim().eq_ignore_ascii_case(&mbid));
                    if !same {
                        continue;
                    }
                    if !hit.in_library {
                        hit.id = row.id.clone();
                        hit.in_library = true;
                    }
                    local_tier = local_tier.max(Some(row.score));
                    claimed[index] = true;
                }
            }
            Candidate {
                item: hit,
                local_tier,
                remote_score,
            }
        })
        .collect();
    (hits, claimed)
}

/// Keep the first library row per MusicBrainz id. Two copies of one
/// release group (two editions on disk) read as one search hit; rows
/// without a MusicBrainz id (or a blank one) are all kept.
fn one_per_mbid(rows: impl IntoIterator<Item = SearchResultItem>) -> Vec<SearchResultItem> {
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|row| match row.musicbrainz_id.as_deref().map(str::trim) {
            Some(mbid) if !mbid.is_empty() => seen.insert(mbid.to_ascii_lowercase()),
            _ => true,
        })
        .collect()
}

/// Join MusicBrainz hits with library hits, unranked: library copies
/// attach to their MusicBrainz hit and the rest of the library hits join
/// the bucket, one per release group.
fn join(local: Vec<SearchResultItem>, remote: Vec<SearchResultItem>) -> Vec<Candidate> {
    let (mut rows, claimed) = attach_local(remote, &local);
    rows.extend(
        one_per_mbid(
            local
                .into_iter()
                .zip(claimed)
                .filter(|(_, claimed)| !claimed)
                .map(|(row, _)| row),
        )
        .into_iter()
        .map(Candidate::local),
    );
    rows
}

/// One "see all" page, unranked: this page's MusicBrainz hits, with ids
/// from the library rows in `joined`, plus the library's own rows at the
/// same offset (`page`) that no hit here claimed. Library rows page in
/// step with MusicBrainz, so each shows on exactly one page whatever
/// MusicBrainz returns, and a page stays full while library rows remain.
fn join_page(
    joined: &[SearchResultItem],
    page: Vec<SearchResultItem>,
    remote: Vec<SearchResultItem>,
) -> Vec<Candidate> {
    let local: Vec<SearchResultItem> = joined.iter().cloned().chain(page).collect();
    let (mut rows, claimed) = attach_local(remote, &local);
    rows.extend(
        one_per_mbid(
            local
                .into_iter()
                .zip(claimed)
                .skip(joined.len())
                .filter(|(_, claimed)| !claimed)
                .map(|(row, _)| row),
        )
        .into_iter()
        .map(Candidate::local),
    );
    rows
}

/// The rows at `offset` of a best-first library list fetched from offset
/// 0, or `None` when the list was cut before the page ends.
fn slice_of(rows: &[SearchResultItem], limit: u32, offset: u32) -> Option<Vec<SearchResultItem>> {
    let end = offset.checked_add(limit)?;
    if end > MAX_LOCAL_JOIN && rows.len() >= MAX_LOCAL_JOIN as usize {
        return None;
    }
    let start = (offset as usize).min(rows.len());
    let end = (end as usize).min(rows.len());
    Some(rows[start..end].to_vec())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn album(id: Option<&str>, mbid: Option<&str>, score: i32) -> SearchResultItem {
        SearchResultItem {
            kind: SearchKind::Album,
            id: id.map(str::to_owned),
            title: "Visions".to_owned(),
            artist: None,
            year: None,
            musicbrainz_id: mbid.map(str::to_owned),
            in_library: id.is_some(),
            requested: false,
            score,
            disambiguation: None,
            type_info: None,
            track: None,
        }
    }

    #[test]
    fn join_emits_one_row_per_release_group() {
        let local = vec![
            album(Some("copy-1"), Some("rg-1"), 100),
            album(Some("copy-2"), Some("RG-1"), 100),
            album(Some("other-1"), Some("rg-2"), 90),
            album(Some("other-2"), Some("rg-2"), 90),
            album(Some("loose"), None, 70),
        ];
        let remote = vec![album(None, Some("RG-1"), 95)];
        let merged = best(join(local, remote), 10);
        let ids: Vec<_> = merged
            .iter()
            .map(|row| (row.id.as_deref(), row.musicbrainz_id.as_deref()))
            .collect();
        assert_eq!(
            ids,
            vec![
                (Some("copy-1"), Some("RG-1")),
                (Some("other-1"), Some("rg-2")),
                (Some("loose"), None),
            ]
        );
    }

    /// Each row: a hit that must rank strictly above another, as (library
    /// tier, MusicBrainz score, held by the library).
    #[test]
    fn rank_puts_good_library_matches_first() {
        type Hit = (Option<i32>, Option<i32>, bool);
        let library = |tier: i32| -> Hit { (Some(tier), None, true) };
        let remote = |score: i32| -> Hit { (None, Some(score), false) };
        let claimed = |tier: i32, score: i32| -> Hit { (Some(tier), Some(score), true) };
        let cases: [(&str, Hit, Hit); 12] = [
            ("exact beats a perfect remote", library(100), remote(100)),
            ("prefix beats a perfect remote", library(90), remote(100)),
            ("exact beats a weak remote", library(100), remote(20)),
            ("exact beats prefix", library(100), library(90)),
            ("a strong remote beats substring", remote(95), library(70)),
            ("substring beats a weak remote", library(70), remote(60)),
            (
                "substring wins a tie with a remote",
                library(70),
                remote(70),
            ),
            (
                "a claimed prefix copy is boosted",
                claimed(90, 40),
                remote(100),
            ),
            (
                "a claimed substring takes the better score",
                claimed(70, 95),
                remote(90),
            ),
            (
                "a claimed substring is not boosted",
                library(90),
                claimed(70, 100),
            ),
            (
                "an owned remote wins a tie",
                (None, Some(80), true),
                remote(80),
            ),
            (
                "a weak claimed substring keeps its tier",
                claimed(70, 40),
                remote(69),
            ),
        ];
        for (name, above, below) in cases {
            let above = rank(above.0, above.1, above.2);
            let below = rank(below.0, below.1, below.2);
            assert!(above > below, "{name}: {above:?} vs {below:?}");
        }
        assert_eq!(
            rank(Some(70), Some(50), true),
            rank(Some(70), None, true),
            "a weak claimed substring ties its unclaimed copy"
        );
    }

    #[test]
    fn equal_ranks_go_by_title_and_show_the_ranked_score() {
        let titled = |title: &str, item: SearchResultItem| SearchResultItem {
            title: title.to_owned(),
            ..item
        };
        let rows = vec![
            Candidate::local(titled("Zebra", album(Some("z"), None, 70))),
            Candidate {
                item: titled("Moon", album(None, Some("rg-m"), 95)),
                local_tier: Some(SCORE_SUBSTRING),
                remote_score: Some(95),
            },
            Candidate::local(titled("Ábaco", album(Some("a"), None, 70))),
        ];
        let order: Vec<(String, i32)> = ranked(rows)
            .into_iter()
            .map(|(_, item)| (item.title, item.score))
            .collect();
        assert_eq!(
            order,
            vec![
                ("Moon".to_owned(), 95),
                ("Ábaco".to_owned(), 70),
                ("Zebra".to_owned(), 70),
            ]
        );
    }
}
