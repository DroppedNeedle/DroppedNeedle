//! Domain logic for the library reads. Every function answers one route:
//! validate, read through the ports, shape the view. Failures are domain
//! typed; handlers map them to HTTP in `error`.

use std::collections::HashSet;

use super::LibraryDeps;
use super::models::{
    AlbumCard, AlbumCardPage, AlbumPage, AlbumQuery, AlbumView, ArtistPage, ArtistQuery,
    ArtistView, BrowseQuery, DecadeShelf, DecadesResponse, GenreList, GenreView,
    LibraryAlbumStatus, LibraryMembershipRequest, LibraryMembershipResponse, LibraryStatusTrack,
    LyricLine, LyricsView, PageQuery, RecentQuery, ResolveTracksRequest, ResolveTracksResponse,
    ResolvedTrack, SearchQuery, SearchResults, StatsView, SuggestionTrack, SuggestionsQuery,
    SuggestionsResponse, TrackPage, TrackQuery, TrackView,
};
use super::stores::{
    AlbumFilter, AlbumRecord, AlbumSort, ArtistRecord, ArtistScope, ArtistSort, StoreError,
    TrackFilter, TrackRecord, TrackSort, UpgradePolicy,
};

/// Domain failures. Handlers convert these; nothing here names HTTP.
#[derive(Debug, PartialEq, Eq)]
pub enum LibraryFailure {
    /// Unknown id, or a track whose file is missing or excluded.
    NotFound,
    /// Bad input. The message is user-facing.
    InvalidInput(String),
    /// Store or port fault. The string goes to the log only.
    Internal(String),
}

impl From<StoreError> for LibraryFailure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Internal(cause) => Self::Internal(cause),
        }
    }
}

/// Parse `limit`/`offset`: defaults, then range.
fn page(
    limit: Option<i64>,
    offset: Option<i64>,
    default: u64,
    max: u64,
) -> Result<(u64, u64), LibraryFailure> {
    let limit = limit.map_or(Ok(default), |value| {
        if (1..=max as i64).contains(&value) {
            Ok(value as u64)
        } else {
            Err(LibraryFailure::InvalidInput(format!(
                "limit must be 1-{max}"
            )))
        }
    })?;
    let offset = offset.map_or(Ok(0), |value| {
        if value >= 0 {
            Ok(value as u64)
        } else {
            Err(LibraryFailure::InvalidInput(
                "offset must be 0 or more".to_owned(),
            ))
        }
    })?;
    Ok((limit, offset))
}

/// Parse `asc`/`desc`.
fn descending(order: Option<&str>) -> Result<bool, LibraryFailure> {
    match order {
        None | Some("asc") => Ok(false),
        Some("desc") => Ok(true),
        Some(other) => Err(LibraryFailure::InvalidInput(format!(
            "unknown order '{other}': want asc or desc"
        ))),
    }
}

fn album_sort(sort: Option<&str>) -> Result<AlbumSort, LibraryFailure> {
    match sort {
        None | Some("name") => Ok(AlbumSort::Name),
        Some("date_added") => Ok(AlbumSort::DateAdded),
        Some("year") => Ok(AlbumSort::Year),
        Some("artist") => Ok(AlbumSort::Artist),
        Some("random") => Ok(AlbumSort::Random),
        Some("rediscover") => Ok(AlbumSort::Rediscover),
        Some(other) => Err(LibraryFailure::InvalidInput(format!(
            "unknown sort '{other}': want name, date_added, year, artist, random, or rediscover"
        ))),
    }
}

fn artist_sort(sort: Option<&str>) -> Result<ArtistSort, LibraryFailure> {
    match sort {
        None | Some("name") => Ok(ArtistSort::Name),
        Some("album_count") => Ok(ArtistSort::AlbumCount),
        Some("appearance_count") => Ok(ArtistSort::AppearanceCount),
        Some("date_added") => Ok(ArtistSort::DateAdded),
        Some(other) => Err(LibraryFailure::InvalidInput(format!(
            "unknown sort '{other}': want name, album_count, appearance_count, or date_added"
        ))),
    }
}

fn track_sort(sort: Option<&str>) -> Result<TrackSort, LibraryFailure> {
    match sort {
        None | Some("title") => Ok(TrackSort::Title),
        Some("date_added") => Ok(TrackSort::DateAdded),
        Some(other) => Err(LibraryFailure::InvalidInput(format!(
            "unknown sort '{other}': want title or date_added"
        ))),
    }
}

fn artist_scope(scope: Option<&str>) -> Result<ArtistScope, LibraryFailure> {
    match scope {
        None | Some("all") => Ok(ArtistScope::All),
        Some("album_artists") => Ok(ArtistScope::AlbumArtists),
        Some("contributors") => Ok(ArtistScope::Contributors),
        Some(other) => Err(LibraryFailure::InvalidInput(format!(
            "unknown scope '{other}': want all, album_artists, or contributors"
        ))),
    }
}

/// Blank filter text reads as absent.
fn clean_q(q: Option<&str>) -> Option<String> {
    q.map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Blank format reads as absent; the rest lowercases so `FLAC` and `flac`
/// match the same rows.
fn clean_format(format: Option<&str>) -> Option<String> {
    format
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_lowercase())
}

/// Decade start years are multiples of ten in a sane range.
fn check_decade(decade: Option<i64>) -> Result<Option<i64>, LibraryFailure> {
    match decade {
        None => Ok(None),
        Some(year) if (1800..=2100).contains(&year) && year % 10 == 0 => Ok(Some(year)),
        Some(year) => Err(LibraryFailure::InvalidInput(format!(
            "unknown decade '{year}': want a start year like 1990"
        ))),
    }
}

fn identity_state(linked: bool) -> String {
    if linked { "linked" } else { "local_only" }.to_owned()
}

fn album_view(record: &AlbumRecord, favorite: bool) -> AlbumView {
    AlbumView {
        id: record.id.clone(),
        title: record.title.clone(),
        artist_name: record.artist_name.clone(),
        artist_id: record.artist_id.clone(),
        release_group_mbid: record.release_group_mbid.clone(),
        release_mbid: record.release_mbid.clone(),
        artist_mbid: record.artist_mbid.clone(),
        identity_state: identity_state(record.linked),
        track_count: record.track_count,
        total_duration_seconds: record.total_duration_seconds,
        total_size_bytes: record.total_size_bytes,
        format: record.format.clone(),
        year: record.year,
        is_compilation: record.is_compilation,
        cover_available: record.cover_available,
        date_added: record.date_added,
        favorite,
        contribution_id: record.contribution_id.clone(),
        contribution_state: record.contribution_state.clone(),
    }
}

fn album_card(record: &AlbumRecord) -> AlbumCard {
    AlbumCard {
        id: record.id.clone(),
        title: record.title.clone(),
        artist_name: record.artist_name.clone(),
        artist_mbid: record.artist_mbid.clone(),
        release_group_mbid: record.release_group_mbid.clone(),
        year: record.year,
        track_count: record.track_count,
        total_size_bytes: record.total_size_bytes,
        primary_format: record.format.clone(),
        cover_available: record.cover_available,
        date_added: record.date_added,
    }
}

fn artist_view(record: &ArtistRecord, favorite: bool) -> ArtistView {
    ArtistView {
        id: record.id.clone(),
        name: record.name.clone(),
        artist_mbid: record.artist_mbid.clone(),
        identity_state: identity_state(record.linked),
        album_count: record.album_count,
        track_count: record.track_count,
        appearance_album_count: record.appearance_album_count,
        date_added: record.date_added,
        favorite,
    }
}

fn track_view(record: &TrackRecord, favorite: bool) -> TrackView {
    TrackView {
        id: record.id.clone(),
        title: record.title.clone(),
        album_id: record.album_id.clone(),
        album_title: record.album_title.clone(),
        artist_name: record.artist_name.clone(),
        artist_id: record.artist_id.clone(),
        album_artist_name: record.album_artist_name.clone(),
        album_artist_id: record.album_artist_id.clone(),
        recording_mbid: record.recording_mbid.clone(),
        release_group_mbid: record.release_group_mbid.clone(),
        artist_mbid: record.artist_mbid.clone(),
        album_artist_mbid: record.album_artist_mbid.clone(),
        disc_number: record.disc_number,
        track_number: record.track_number,
        year: record.year,
        genre: record.genre.clone(),
        duration_seconds: record.duration_seconds,
        format: record.format.clone(),
        bit_rate: record.bit_rate,
        sample_rate: record.sample_rate,
        bit_depth: record.bit_depth,
        channels: record.channels,
        file_size_bytes: record.file_size_bytes,
        date_added: record.date_added,
        cover_available: record.cover_available,
        favorite,
    }
}

fn suggestion_track(record: &TrackRecord, reason: &str) -> SuggestionTrack {
    SuggestionTrack {
        track_id: record.id.clone(),
        title: record.title.clone(),
        album_title: record.album_title.clone(),
        artist_name: record.artist_name.clone(),
        album_id: record.album_id.clone(),
        cover_available: record.cover_available,
        format: record.format.clone(),
        year: record.year,
        duration_seconds: record.duration_seconds,
        reason: reason.to_owned(),
    }
}

/// Favorite flags for one page of ids. Empty pages skip the store call.
async fn favorites_for(
    deps: &LibraryDeps,
    user_id: &str,
    kind: &str,
    ids: &[String],
) -> Result<HashSet<String>, LibraryFailure> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    deps.favorites
        .filter_favorites(user_id, kind, ids)
        .await
        .map_err(LibraryFailure::from)
}

/// One page of catalog albums.
pub async fn list_albums(
    deps: &LibraryDeps,
    user_id: &str,
    query: &AlbumQuery,
) -> Result<AlbumPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 50, 200)?;
    let filter = AlbumFilter {
        q: clean_q(query.q.as_deref()),
        artist_id: query.artist_id.clone(),
        decade: check_decade(query.decade)?,
        format: clean_format(query.format.as_deref()),
    };
    let (records, total) = deps
        .catalog
        .list_albums(
            &filter,
            album_sort(query.sort.as_deref())?,
            descending(query.order.as_deref())?,
            limit,
            offset,
        )
        .await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "album", &ids).await?;
    Ok(AlbumPage {
        items: records
            .iter()
            .map(|record| album_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        offset,
        limit,
    })
}

/// One catalog album. Unknown ids are 404.
pub async fn get_album(
    deps: &LibraryDeps,
    user_id: &str,
    id: &str,
) -> Result<AlbumView, LibraryFailure> {
    let record = deps
        .catalog
        .get_album(id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let favorites = favorites_for(deps, user_id, "album", std::slice::from_ref(&record.id)).await?;
    Ok(album_view(&record, favorites.contains(&record.id)))
}

/// One page of an album's streamable tracks. Unknown albums are 404.
pub async fn album_tracks(
    deps: &LibraryDeps,
    user_id: &str,
    album_id: &str,
    query: &PageQuery,
) -> Result<TrackPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 200, 1000)?;
    deps.catalog
        .get_album(album_id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let (records, total) = deps.catalog.album_tracks(album_id, limit, offset).await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "track", &ids).await?;
    Ok(TrackPage {
        items: records
            .iter()
            .map(|record| track_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        offset,
        limit,
    })
}

/// One page of an album's streamable tracks by release-group mbid or
/// local album id, for browse cards that only carry the mbid. Unknown
/// mbids and ids are 404; sibling groups resolve to the oldest album.
pub async fn album_match(
    deps: &LibraryDeps,
    user_id: &str,
    mbid: &str,
    query: &PageQuery,
) -> Result<TrackPage, LibraryFailure> {
    let id = if deps.catalog.get_album(mbid).await?.is_some() {
        mbid.to_owned()
    } else {
        deps.catalog
            .get_album_by_release_group(mbid)
            .await?
            .map(|record| record.id)
            .ok_or(LibraryFailure::NotFound)?
    };
    album_tracks(deps, user_id, &id, query).await
}

/// Other local albums sharing the album's release group. Unknown albums
/// are 404; unidentified or sibling-free albums read as an empty page.
pub async fn album_copies(
    deps: &LibraryDeps,
    user_id: &str,
    album_id: &str,
) -> Result<AlbumPage, LibraryFailure> {
    deps.catalog
        .get_album(album_id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let records = deps.catalog.album_copies(album_id).await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "album", &ids).await?;
    let total = records.len() as u64;
    Ok(AlbumPage {
        items: records
            .iter()
            .map(|record| album_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        offset: 0,
        limit: total,
    })
}

/// One page of catalog artists.
pub async fn list_artists(
    deps: &LibraryDeps,
    user_id: &str,
    query: &ArtistQuery,
) -> Result<ArtistPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 50, 200)?;
    let q = clean_q(query.q.as_deref());
    let (records, total, album_artist_total, contributor_total) = deps
        .catalog
        .list_artists(
            artist_scope(query.scope.as_deref())?,
            q.as_deref(),
            artist_sort(query.sort.as_deref())?,
            descending(query.order.as_deref())?,
            limit,
            offset,
        )
        .await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "artist", &ids).await?;
    Ok(ArtistPage {
        items: records
            .iter()
            .map(|record| artist_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        album_artist_total,
        contributor_total,
        offset,
        limit,
    })
}

/// One catalog artist. Unknown ids are 404.
pub async fn get_artist(
    deps: &LibraryDeps,
    user_id: &str,
    id: &str,
) -> Result<ArtistView, LibraryFailure> {
    let record = deps
        .catalog
        .get_artist(id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let favorites =
        favorites_for(deps, user_id, "artist", std::slice::from_ref(&record.id)).await?;
    Ok(artist_view(&record, favorites.contains(&record.id)))
}

/// One page of albums led by the artist. Unknown artists are 404.
pub async fn artist_albums(
    deps: &LibraryDeps,
    user_id: &str,
    artist_id: &str,
    query: &PageQuery,
) -> Result<AlbumPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 200, 1000)?;
    deps.catalog
        .get_artist(artist_id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let (records, total) = deps.catalog.artist_albums(artist_id, limit, offset).await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "album", &ids).await?;
    Ok(AlbumPage {
        items: records
            .iter()
            .map(|record| album_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        offset,
        limit,
    })
}

/// One page of albums where the artist appears without leading.
/// Unknown artists are 404.
pub async fn artist_appearances(
    deps: &LibraryDeps,
    user_id: &str,
    artist_id: &str,
    query: &PageQuery,
) -> Result<AlbumPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 200, 1000)?;
    deps.catalog
        .get_artist(artist_id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let (records, total) = deps
        .catalog
        .artist_appearances(artist_id, limit, offset)
        .await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "album", &ids).await?;
    Ok(AlbumPage {
        items: records
            .iter()
            .map(|record| album_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        offset,
        limit,
    })
}

/// One page of streamable tracks.
pub async fn list_tracks(
    deps: &LibraryDeps,
    user_id: &str,
    query: &TrackQuery,
) -> Result<TrackPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 50, 500)?;
    let filter = TrackFilter {
        q: clean_q(query.q.as_deref()),
        album_id: query.album_id.clone(),
        artist_id: query.artist_id.clone(),
        genre: clean_q(query.genre.as_deref()),
    };
    let (records, total) = deps
        .catalog
        .list_tracks(
            &filter,
            track_sort(query.sort.as_deref())?,
            descending(query.order.as_deref())?,
            limit,
            offset,
        )
        .await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "track", &ids).await?;
    Ok(TrackPage {
        items: records
            .iter()
            .map(|record| track_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        offset,
        limit,
    })
}

/// One streamable track. Unknown ids and tracks whose file is missing or
/// excluded are 404.
pub async fn get_track(
    deps: &LibraryDeps,
    user_id: &str,
    id: &str,
) -> Result<TrackView, LibraryFailure> {
    let record = deps
        .catalog
        .get_track(id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let favorites = favorites_for(deps, user_id, "track", std::slice::from_ref(&record.id)).await?;
    Ok(track_view(&record, favorites.contains(&record.id)))
}

/// Library totals plus the caller's favorite counts, the review queue,
/// unidentified albums and the last finished scan.
pub async fn stats(deps: &LibraryDeps, user_id: &str) -> Result<StatsView, LibraryFailure> {
    let record = deps.catalog.stats().await?;
    let extras = deps.lookups.stats_extras().await?;
    let (albums, artists, tracks) = deps.favorites.favorite_counts(user_id).await?;
    Ok(StatsView {
        total_albums: record.total_albums,
        total_artists: record.total_artists,
        total_tracks: record.total_tracks,
        total_size_bytes: record.total_size_bytes,
        format_breakdown: record.format_breakdown,
        favorite_albums: albums,
        favorite_artists: artists,
        favorite_tracks: tracks,
        review_count: extras.review_count,
        local_only_count: extras.local_only_count,
        last_scan_at: extras.last_scan_at,
    })
}

/// Newest albums first, capped.
pub async fn recently_added(
    deps: &LibraryDeps,
    user_id: &str,
    query: &RecentQuery,
) -> Result<AlbumPage, LibraryFailure> {
    let (limit, _) = page(query.limit, None, 20, 50)?;
    let records = deps.catalog.recently_added(limit).await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "album", &ids).await?;
    let total = records.len() as u64;
    Ok(AlbumPage {
        items: records
            .iter()
            .map(|record| album_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        offset: 0,
        limit,
    })
}

/// Full genre listing.
pub async fn genres(deps: &LibraryDeps) -> Result<GenreList, LibraryFailure> {
    let records = deps.catalog.genres().await?;
    Ok(GenreList {
        items: records
            .iter()
            .map(|record| GenreView {
                name: record.name.clone(),
                track_count: record.track_count,
                album_count: record.album_count,
            })
            .collect(),
    })
}

/// One page of a genre's streamable tracks. Unknown genres read as an
/// empty page: genre names are tags, not addressable resources.
pub async fn genre_tracks(
    deps: &LibraryDeps,
    user_id: &str,
    name: &str,
    query: &PageQuery,
) -> Result<TrackPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 200, 1000)?;
    let folded = name.trim().to_lowercase();
    if folded.is_empty() {
        return Err(LibraryFailure::InvalidInput(
            "genre name must not be blank".to_owned(),
        ));
    }
    let (records, total) = deps.catalog.genre_tracks(&folded, limit, offset).await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "track", &ids).await?;
    Ok(TrackPage {
        items: records
            .iter()
            .map(|record| track_view(record, favorites.contains(&record.id)))
            .collect(),
        total,
        offset,
        limit,
    })
}

/// Album-card browse for the local library.
pub async fn browse_albums(
    deps: &LibraryDeps,
    query: &BrowseQuery,
) -> Result<AlbumCardPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 50, 200)?;
    let filter = AlbumFilter {
        q: clean_q(query.q.as_deref()),
        artist_id: None,
        decade: check_decade(query.decade)?,
        format: None,
    };
    let (records, total) = deps
        .catalog
        .list_albums(
            &filter,
            album_sort(query.sort.as_deref())?,
            descending(query.order.as_deref())?,
            limit,
            offset,
        )
        .await?;
    Ok(AlbumCardPage {
        items: records.iter().map(album_card).collect(),
        total,
        offset,
        limit,
    })
}

/// Album plus track search. `q` is required; each group caps separately.
pub async fn search_library(
    deps: &LibraryDeps,
    query: &SearchQuery,
) -> Result<SearchResults, LibraryFailure> {
    let q = clean_q(query.q.as_deref())
        .ok_or_else(|| LibraryFailure::InvalidInput("q is required".to_owned()))?;
    let (limit, _) = page(query.limit, None, 20, 50)?;
    let album_filter = AlbumFilter {
        q: Some(q.clone()),
        artist_id: None,
        decade: None,
        format: None,
    };
    let track_filter = TrackFilter {
        q: Some(q),
        album_id: None,
        artist_id: None,
        genre: None,
    };
    let (albums, _) = deps
        .catalog
        .list_albums(&album_filter, AlbumSort::Name, false, limit, 0)
        .await?;
    let (tracks, _) = deps
        .catalog
        .list_tracks(&track_filter, TrackSort::Title, false, limit, 0)
        .await?;
    Ok(SearchResults {
        albums: albums.iter().map(album_card).collect(),
        tracks: tracks
            .iter()
            .map(|record| suggestion_track(record, "match"))
            .collect(),
    })
}

/// Newest album cards first, capped.
pub async fn recent_albums(
    deps: &LibraryDeps,
    query: &RecentQuery,
) -> Result<Vec<AlbumCard>, LibraryFailure> {
    let (limit, _) = page(query.limit, None, 20, 50)?;
    let records = deps.catalog.recently_added(limit).await?;
    Ok(records.iter().map(album_card).collect())
}

/// Decade shelves, oldest first.
pub async fn decades(deps: &LibraryDeps) -> Result<DecadesResponse, LibraryFailure> {
    let records = deps.catalog.decades().await?;
    Ok(DecadesResponse {
        items: records
            .iter()
            .map(|record| DecadeShelf {
                decade: record.decade,
                label: format!("{}s", record.decade),
                album_count: record.album_count,
            })
            .collect(),
    })
}

/// Reason-tagged track suggestions. Pools mirror v2: newest imports
/// (`recent`), oldest imports (`rediscover`), random picks (`surprise`),
/// plus random same-decade picks (`same_era`) when a decade anchors the
/// request. Pools over-fetch evenly, dedupe by track, then trim.
pub async fn suggestions(
    deps: &LibraryDeps,
    query: &SuggestionsQuery,
) -> Result<SuggestionsResponse, LibraryFailure> {
    let (limit, _) = page(query.limit, None, 12, 40)?;
    let decade = check_decade(query.decade)?;
    let pools = if decade.is_some() { 4 } else { 3 };
    let per = limit.div_ceil(pools).max(1);
    let newest = deps.catalog.newest_tracks(per).await?;
    let oldest = deps.catalog.oldest_tracks(per).await?;
    let surprise = deps.catalog.random_tracks(per, None).await?;
    let era = match decade {
        Some(year) => deps.catalog.random_tracks(per, Some(year)).await?,
        None => Vec::new(),
    };
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for (reason, records) in [
        ("recent", &newest),
        ("rediscover", &oldest),
        ("surprise", &surprise),
        ("same_era", &era),
    ] {
        for record in records {
            if seen.insert(record.id.clone()) {
                items.push(suggestion_track(record, reason));
            }
            if items.len() >= limit as usize {
                break;
            }
        }
        if items.len() >= limit as usize {
            break;
        }
    }
    Ok(SuggestionsResponse { items })
}

/// Stored lyrics for one track. Unknown tracks, unstreamable tracks, and
/// tracks without stored lyrics are all 404.
pub async fn lyrics(deps: &LibraryDeps, track_id: &str) -> Result<LyricsView, LibraryFailure> {
    deps.catalog
        .get_track(track_id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let doc = deps
        .lyrics
        .get(track_id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    Ok(LyricsView {
        text: doc
            .lines
            .iter()
            .map(|(text, _)| text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        is_synced: doc.synced,
        lines: doc
            .lines
            .iter()
            .map(|(text, start_ms)| LyricLine {
                text: text.clone(),
                start_seconds: start_ms.map(|ms| ms as f64 / 1000.0),
            })
            .collect(),
    })
}

/// Most album ids one membership check accepts (v2's cap).
const MEMBERSHIP_MAX_IDS: usize = 500;

/// Most positions one track resolution answers (v2's cap).
const RESOLVE_MAX_ITEMS: usize = 200;

/// Which asked album ids the library holds and which have an open request.
/// Ids are trimmed, lowercased and de-duplicated before the cap applies.
pub async fn membership(
    deps: &LibraryDeps,
    request: &LibraryMembershipRequest,
) -> Result<LibraryMembershipResponse, LibraryFailure> {
    let mut seen = HashSet::new();
    let ids: Vec<String> = request
        .album_ids
        .iter()
        .map(|id| id.trim().to_lowercase())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect();
    if ids.len() > MEMBERSHIP_MAX_IDS {
        return Err(LibraryFailure::InvalidInput(format!(
            "Library membership accepts at most {MEMBERSHIP_MAX_IDS} album ids."
        )));
    }
    if ids.is_empty() {
        return Ok(LibraryMembershipResponse {
            owned_ids: Vec::new(),
            requested_ids: Vec::new(),
        });
    }
    let asked: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let keep = |found: HashSet<String>| {
        let mut kept: Vec<String> = found
            .into_iter()
            .filter(|id| asked.contains(id.as_str()))
            .collect();
        kept.sort();
        kept
    };
    let owned = deps.lookups.owned_albums(&ids).await?;
    let requested = deps.lookups.requested_albums(&ids).await?;
    Ok(LibraryMembershipResponse {
        owned_ids: keep(owned),
        requested_ids: keep(requested),
    })
}

/// Lossless containers by extension (v2 `_LOSSLESS_EXT`).
const LOSSLESS_FORMATS: [&str; 5] = ["flac", "alac", "wav", "ape", "wv"];

/// MP4-family containers: lossless only with bit-depth evidence (ALAC),
/// otherwise lossy AAC on the bitrate bands.
const MP4_FORMATS: [&str; 3] = ["m4a", "mp4", "mov"];

/// Quality tier of one file, the same reading the acquisition side uses
/// (v2 `quality_tiers.tier_for`): lossless containers, then lossy bitrate
/// bands whatever the codec.
pub fn tier_for(format: &str, bit_rate_kbps: Option<i64>, bit_depth: Option<i64>) -> &'static str {
    let format = format.trim().trim_start_matches('.').to_lowercase();
    if LOSSLESS_FORMATS.contains(&format.as_str())
        || (MP4_FORMATS.contains(&format.as_str()) && bit_depth.is_some())
    {
        return "lossless";
    }
    match bit_rate_kbps.unwrap_or(0) {
        rate if rate >= 320 => "mp3_320",
        rate if rate >= 256 => "mp3_256",
        rate if rate >= 192 => "mp3_192",
        _ => "low",
    }
}

/// True when upgrades are on and `tier` ranks below a known cutoff tier.
fn below_cutoff(tier: &str, policy: &UpgradePolicy) -> bool {
    let Some(cutoff) = policy.quality_cutoff.as_deref() else {
        return false;
    };
    let rank = |label: &str| crate::runtime_config::sections::tier_rank(label);
    policy.upgrade_allowed
        && rank(cutoff).is_some_and(|cutoff_rank| rank(tier).unwrap_or(0) < cutoff_rank)
}

/// What the library holds for one album: by local id or alias, else every
/// live album holding the MusicBrainz release group (or release). Each
/// track carries its quality tier and whether it sits below the upgrade
/// cutoff. An album the library lacks answers `in_library: false`.
pub async fn album_status(
    deps: &LibraryDeps,
    user_id: &str,
    identifier: &str,
) -> Result<LibraryAlbumStatus, LibraryFailure> {
    let identifier = identifier.trim();
    if identifier.is_empty() {
        return Err(LibraryFailure::InvalidInput(
            "album id must not be blank".to_owned(),
        ));
    }
    let album_ids = deps.lookups.status_albums(identifier).await?;
    let records = if album_ids.is_empty() {
        Vec::new()
    } else {
        deps.lookups.album_tracks_batch(&album_ids).await?
    };
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "track", &ids).await?;
    let policy = (deps.upgrade_policy)();
    let tracks: Vec<LibraryStatusTrack> = records
        .iter()
        .map(|record| {
            let tier = tier_for(&record.format, record.bit_rate, record.bit_depth);
            LibraryStatusTrack {
                track: track_view(record, favorites.contains(&record.id)),
                current_tier: tier.to_owned(),
                below_cutoff: below_cutoff(tier, &policy),
            }
        })
        .collect();
    let album_id = records
        .first()
        .map(|record| record.album_id.clone())
        .or_else(|| album_ids.first().cloned())
        .unwrap_or_else(|| identifier.to_owned());
    Ok(LibraryAlbumStatus {
        in_library: !tracks.is_empty(),
        album_id,
        track_count: tracks.len() as u64,
        tracks,
    })
}

/// Resolve track positions (album id, disc, track) to playable local
/// files, for lists like an artist's top songs. Positions without an album
/// or track number, albums the library lacks, and MusicBrainz ids several
/// local albums hold all answer unresolved. Two lookups serve the whole
/// batch whatever its size.
pub async fn resolve_tracks(
    deps: &LibraryDeps,
    request: &ResolveTracksRequest,
) -> Result<ResolveTracksResponse, LibraryFailure> {
    let asked = &request.items[..request.items.len().min(RESOLVE_MAX_ITEMS)];
    let mut items: Vec<ResolvedTrack> = asked
        .iter()
        .map(|item| ResolvedTrack {
            release_group_mbid: item.release_group_mbid.clone(),
            disc_number: item.disc_number,
            track_number: item.track_number,
            source: None,
            track_source_id: None,
            stream_url: None,
            format: None,
            duration: None,
        })
        .collect();
    let identifiers: Vec<String> = asked
        .iter()
        .filter(|item| item.track_number.is_some())
        .filter_map(|item| item.release_group_mbid.clone())
        .filter(|id| !id.trim().is_empty())
        .collect();
    if identifiers.is_empty() {
        return Ok(ResolveTracksResponse { items });
    }
    let canonical = deps.lookups.resolve_albums(&identifiers).await?;
    let mut album_ids: Vec<String> = canonical.values().cloned().collect();
    album_ids.sort();
    album_ids.dedup();
    if album_ids.is_empty() {
        return Ok(ResolveTracksResponse { items });
    }
    let tracks = deps.lookups.album_tracks_batch(&album_ids).await?;
    // First file wins per position: albums sort by disc, track, then id.
    let mut by_position: std::collections::HashMap<(&str, i64, i64), &TrackRecord> =
        std::collections::HashMap::new();
    for track in &tracks {
        by_position
            .entry((
                track.album_id.as_str(),
                track.disc_number,
                track.track_number,
            ))
            .or_insert(track);
    }
    for (item, answer) in asked.iter().zip(items.iter_mut()) {
        let (Some(identifier), Some(track_number)) =
            (item.release_group_mbid.as_ref(), item.track_number)
        else {
            continue;
        };
        let Some(album_id) = canonical.get(identifier) else {
            continue;
        };
        let disc = item.disc_number.unwrap_or(1);
        if let Some(track) = by_position.get(&(album_id.as_str(), disc, track_number)) {
            answer.source = Some("local".to_owned());
            answer.track_source_id = Some(track.id.clone());
            answer.stream_url = Some(format!("/api/v3/stream/local/{}", track.id));
            answer.format = Some(track.format.clone());
            answer.duration = track.duration_seconds;
        }
    }
    Ok(ResolveTracksResponse { items })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_follow_the_acquisition_bands() {
        assert_eq!(tier_for("FLAC", None, None), "lossless");
        assert_eq!(tier_for("m4a", Some(256), Some(16)), "lossless");
        assert_eq!(tier_for("m4a", Some(256), None), "mp3_256");
        assert_eq!(tier_for("mp3", Some(320), None), "mp3_320");
        assert_eq!(tier_for("opus", Some(192), None), "mp3_192");
        assert_eq!(tier_for("mp3", None, None), "low");
        let on = UpgradePolicy {
            quality_cutoff: Some("lossless".to_owned()),
            upgrade_allowed: true,
        };
        assert!(below_cutoff("mp3_320", &on));
        assert!(!below_cutoff("lossless", &on));
        let off = UpgradePolicy {
            upgrade_allowed: false,
            ..on.clone()
        };
        assert!(!below_cutoff("low", &off));
        let unknown = UpgradePolicy {
            quality_cutoff: Some("bogus".to_owned()),
            ..on
        };
        assert!(!below_cutoff("low", &unknown));
    }

    #[test]
    fn page_defaults_and_caps() {
        assert_eq!(page(None, None, 50, 200), Ok((50, 0)));
        assert_eq!(
            page(Some(0), None, 50, 200),
            Err(LibraryFailure::InvalidInput(
                "limit must be 1-200".to_owned()
            ))
        );
        assert_eq!(
            page(Some(201), None, 50, 200),
            Err(LibraryFailure::InvalidInput(
                "limit must be 1-200".to_owned()
            ))
        );
        assert_eq!(
            page(None, Some(-1), 50, 200),
            Err(LibraryFailure::InvalidInput(
                "offset must be 0 or more".to_owned()
            ))
        );
    }

    #[test]
    fn unknown_sorts_fail_with_guidance() {
        assert!(album_sort(Some("nope")).is_err());
        assert!(artist_sort(Some("nope")).is_err());
        assert!(track_sort(Some("nope")).is_err());
        assert!(artist_scope(Some("nope")).is_err());
        assert!(descending(Some("sideways")).is_err());
    }

    #[test]
    fn decades_must_be_start_years() {
        assert_eq!(check_decade(None), Ok(None));
        assert_eq!(check_decade(Some(1990)), Ok(Some(1990)));
        assert!(check_decade(Some(1995)).is_err());
        assert!(check_decade(Some(1500)).is_err());
    }

    #[test]
    fn blank_filter_text_reads_as_absent() {
        assert_eq!(clean_q(None), None);
        assert_eq!(clean_q(Some("  ")), None);
        assert_eq!(clean_q(Some("  abba ")), Some("abba".to_owned()));
    }

    #[test]
    fn format_filter_cleans_and_lowercases() {
        assert_eq!(clean_format(None), None);
        assert_eq!(clean_format(Some("  ")), None);
        assert_eq!(clean_format(Some("  FLAC ")), Some("flac".to_owned()));
    }

    #[test]
    fn artist_sort_parses() {
        assert_eq!(album_sort(Some("artist")), Ok(AlbumSort::Artist));
    }

    #[test]
    fn store_faults_stay_internal() {
        let failure = LibraryFailure::from(StoreError::Internal("db is gone".to_owned()));
        assert_eq!(failure, LibraryFailure::Internal("db is gone".to_owned()));
    }
}
