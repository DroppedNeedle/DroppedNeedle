//! Domain logic for the library reads. Every function answers one route:
//! validate, read through the ports, shape the view. Failures are domain
//! typed; handlers map them to HTTP in `error`.

use std::collections::HashSet;

use super::LibraryDeps;
use super::cursor::{Cursor, fresh_seed};
use super::models::{
    AlbumDetailView, AlbumPage, AlbumQuery, AlbumView, ArtistPage, ArtistQuery, ArtistView,
    CursorQuery, DecadeShelf, DecadesResponse, GenreList, GenreView, LibraryAlbumStatus,
    LibraryMbids, LibraryMembershipRequest, LibraryMembershipResponse, LibraryStatusTrack,
    LyricLine, LyricsView, ManagementHoldView, PageQuery, RecentQuery, ResolveTracksRequest,
    ResolveTracksResponse, ResolvedTrack, StatsView, SuggestionTrack, SuggestionsQuery,
    SuggestionsResponse, TrackListPage, TrackPage, TrackQuery, TrackTags, TrackView,
};
use super::sql;
use super::stores::{
    After, AlbumFilter, AlbumRecord, AlbumSort, ArtistRecord, ArtistScope, ArtistSort, KeyedPage,
    StoreError, TagRead, TrackFilter, TrackRecord, TrackSort, UpgradePolicy,
};
use droppedneedle_auth::Admin;
use rusqlite::types::Value;

/// Domain failures. Handlers convert these; nothing here names HTTP.
#[derive(Debug, PartialEq, Eq)]
pub enum LibraryFailure {
    /// Unknown id, or a track whose file is missing or excluded.
    NotFound,
    /// Bad input. The message is user-facing.
    InvalidInput(String),
    /// Not allowed. The message is user-facing.
    Forbidden(String),
    /// The database was too busy to answer in time. Names the operation.
    Busy(String),
    /// Store or port fault. The string goes to the log only.
    Internal(String),
}

impl From<StoreError> for LibraryFailure {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(operation) => Self::Busy(operation),
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
        Some("artist") => Ok(TrackSort::Artist),
        Some("album") => Ok(TrackSort::Album),
        Some("random") => Ok(TrackSort::Random),
        Some(other) => Err(LibraryFailure::InvalidInput(format!(
            "unknown sort '{other}': want title, date_added, artist, album, or random"
        ))),
    }
}

/// Where a page of the list taken in `order` starts: the first page, or
/// after the cursor's row. A first page in random order draws its seed.
/// A cursor from another list or order is bad input.
fn start(
    after: Option<&str>,
    order: &str,
    width: usize,
    random: bool,
) -> Result<After, LibraryFailure> {
    let Some(text) = after else {
        return Ok(After {
            seed: if random { fresh_seed() } else { 0 },
            key: None,
        });
    };
    let cursor = Cursor::decode(text, order, width).map_err(|_| {
        LibraryFailure::InvalidInput(
            "after is not a cursor from this list and order; start from the first page".to_owned(),
        )
    })?;
    Ok(After {
        seed: cursor.seed,
        key: Some(cursor.key),
    })
}

/// The cursor of the page after one that started at `after`.
fn next_cursor(order: String, after: &After, key: Option<Vec<Value>>) -> Option<String> {
    key.map(|key| {
        Cursor {
            order,
            seed: after.seed,
            key,
        }
        .encode()
    })
}

/// A page of albums with the caller's favorite flags.
async fn album_page(
    deps: &LibraryDeps,
    user_id: &str,
    order: String,
    after: &After,
    page: KeyedPage<AlbumRecord>,
) -> Result<AlbumPage, LibraryFailure> {
    let ids: Vec<String> = page.items.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "album", &ids).await?;
    Ok(AlbumPage {
        items: page
            .items
            .into_iter()
            .map(|record| {
                let favorite = favorites.contains(&record.id);
                AlbumView::from((record, favorite))
            })
            .collect(),
        total: page.total,
        next: next_cursor(order, after, page.next),
    })
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
    deps.catalog
        .filter_favorites(user_id, kind, ids)
        .await
        .map_err(LibraryFailure::from)
}

/// The catalog album an id names now. A merged or retired album's old id
/// answers for the album it was merged into. Unknown ids are 404.
async fn album_record(deps: &LibraryDeps, id: &str) -> Result<AlbumRecord, LibraryFailure> {
    let current = deps
        .catalog
        .current_album(id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    deps.catalog
        .get_album(&current)
        .await?
        .ok_or(LibraryFailure::NotFound)
}

/// The catalog artist an id names now, as [`album_record`] does for albums,
/// or the artist linked to it when the id is a MusicBrainz artist id, so a
/// provider artist page can ask by the id it holds. Unknown ids are 404.
async fn artist_record(deps: &LibraryDeps, id: &str) -> Result<ArtistRecord, LibraryFailure> {
    if let Some(current) = deps.catalog.current_artist(id).await?
        && let Some(record) = deps.catalog.get_artist(&current).await?
    {
        return Ok(record);
    }
    let linked = deps
        .catalog
        .artist_for_mbid(id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    deps.catalog
        .get_artist(&linked)
        .await?
        .ok_or(LibraryFailure::NotFound)
}

/// One page of catalog albums.
pub async fn list_albums(
    deps: &LibraryDeps,
    user_id: &str,
    query: &AlbumQuery,
) -> Result<AlbumPage, LibraryFailure> {
    let (limit, _) = page(query.limit, None, 50, 200)?;
    let filter = AlbumFilter {
        q: clean_q(query.q.as_deref()),
        artist_id: query.artist_id.clone(),
        decade: check_decade(query.decade)?,
        format: clean_format(query.format.as_deref()),
    };
    let sort = album_sort(query.sort.as_deref())?;
    let descending = descending(query.order.as_deref())?;
    let order = format!("albums/{sort:?}/{descending}");
    let after = start(
        query.after.as_deref(),
        &order,
        sql::album_keys(sort, descending).len(),
        sort == AlbumSort::Random,
    )?;
    let page = deps
        .catalog
        .list_albums(&filter, sort, descending, limit, &after)
        .await?;
    album_page(deps, user_id, order, &after, page).await
}

/// One catalog album, answered for the album the id names now. Unknown ids
/// are 404.
pub async fn get_album(
    deps: &LibraryDeps,
    user_id: &str,
    id: &str,
) -> Result<AlbumDetailView, LibraryFailure> {
    let record = album_record(deps, id).await?;
    let favorites = favorites_for(deps, user_id, "album", std::slice::from_ref(&record.id)).await?;
    let hold = deps.catalog.management_hold(&record.id).await?;
    let favorite = favorites.contains(&record.id);
    Ok(AlbumDetailView {
        album: AlbumView::from((record, favorite)),
        management_hold: hold.map(|hold| ManagementHoldView {
            kind: hold.kind,
            held_at: hold.held_at,
            exclusion_revision: hold.exclusion_revision,
            handing_back: hold.handing_back,
        }),
    })
}

/// One page of an album's streamable tracks. Unknown albums are 404.
pub async fn album_tracks(
    deps: &LibraryDeps,
    user_id: &str,
    album_id: &str,
    query: &PageQuery,
) -> Result<TrackPage, LibraryFailure> {
    let (limit, offset) = page(query.limit, query.offset, 200, 1000)?;
    let album = album_record(deps, album_id).await?;
    let (records, total) = deps.catalog.album_tracks(&album.id, limit, offset).await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "track", &ids).await?;
    Ok(TrackPage {
        items: records
            .into_iter()
            .map(|record| {
                let favorite = favorites.contains(&record.id);
                TrackView::from((record, favorite))
            })
            .collect(),
        total,
        offset,
        limit,
    })
}

/// One page of an album's streamable tracks by release-group mbid or
/// local album id, for browse cards that only carry the mbid. Unknown
/// mbids and ids are 404. A local id or alias answers for the album it
/// names now; a group answers with its oldest copy with files, else its
/// oldest copy.
pub async fn album_match(
    deps: &LibraryDeps,
    user_id: &str,
    mbid: &str,
    query: &PageQuery,
) -> Result<TrackPage, LibraryFailure> {
    let id = if let Some(current) = deps.catalog.current_album(mbid).await? {
        current
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
    let album = album_record(deps, album_id).await?;
    let records = deps.catalog.album_copies(&album.id).await?;
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "album", &ids).await?;
    let total = records.len() as u64;
    Ok(AlbumPage {
        items: records
            .into_iter()
            .map(|record| {
                let favorite = favorites.contains(&record.id);
                AlbumView::from((record, favorite))
            })
            .collect(),
        total,
        next: None,
    })
}

/// One page of catalog artists.
pub async fn list_artists(
    deps: &LibraryDeps,
    user_id: &str,
    query: &ArtistQuery,
) -> Result<ArtistPage, LibraryFailure> {
    let (limit, _) = page(query.limit, None, 50, 200)?;
    let q = clean_q(query.q.as_deref());
    let scope = artist_scope(query.scope.as_deref())?;
    let sort = artist_sort(query.sort.as_deref())?;
    let descending = descending(query.order.as_deref())?;
    // The count sorts' cursors hold the kept counts, the negated count when
    // most come first; the tag turns a cursor from before them into a 400.
    let kept = match sort {
        ArtistSort::AlbumCount | ArtistSort::AppearanceCount => "/kept",
        ArtistSort::Name | ArtistSort::DateAdded => "",
    };
    let order = format!("artists/{scope:?}/{sort:?}/{descending}{kept}");
    let after = start(
        query.after.as_deref(),
        &order,
        sql::artist_keys(scope, sort, descending).len(),
        false,
    )?;
    let (page, album_artist_total, contributor_total) = deps
        .catalog
        .artist_list(scope, q.as_deref(), sort, descending, limit, &after)
        .await?;
    let ids: Vec<String> = page.items.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "artist", &ids).await?;
    Ok(ArtistPage {
        items: page
            .items
            .into_iter()
            .map(|record| {
                let favorite = favorites.contains(&record.id);
                ArtistView::from((record, favorite))
            })
            .collect(),
        total: page.total,
        album_artist_total,
        contributor_total,
        next: next_cursor(order, &after, page.next),
    })
}

/// One catalog artist, answered for the artist the id names now. Unknown
/// ids are 404.
pub async fn get_artist(
    deps: &LibraryDeps,
    user_id: &str,
    id: &str,
) -> Result<ArtistView, LibraryFailure> {
    let record = artist_record(deps, id).await?;
    let favorites =
        favorites_for(deps, user_id, "artist", std::slice::from_ref(&record.id)).await?;
    let favorite = favorites.contains(&record.id);
    Ok(ArtistView::from((record, favorite)))
}

/// One page of albums led by the artist. Unknown artists are 404.
pub async fn artist_albums(
    deps: &LibraryDeps,
    user_id: &str,
    artist_id: &str,
    query: &CursorQuery,
) -> Result<AlbumPage, LibraryFailure> {
    let (limit, _) = page(query.limit, None, 200, 1000)?;
    let order = "artist-albums".to_owned();
    let after = start(
        query.after.as_deref(),
        &order,
        sql::title_keys().len(),
        false,
    )?;
    let artist = artist_record(deps, artist_id).await?;
    let page = deps.catalog.led_albums(&artist.id, limit, &after).await?;
    album_page(deps, user_id, order, &after, page).await
}

/// One page of albums where the artist appears without leading.
/// Unknown artists are 404.
pub async fn artist_appearances(
    deps: &LibraryDeps,
    user_id: &str,
    artist_id: &str,
    query: &CursorQuery,
) -> Result<AlbumPage, LibraryFailure> {
    let (limit, _) = page(query.limit, None, 200, 1000)?;
    let order = "appearances".to_owned();
    let after = start(
        query.after.as_deref(),
        &order,
        sql::title_keys().len(),
        false,
    )?;
    let artist = artist_record(deps, artist_id).await?;
    let page = deps
        .catalog
        .artist_appearances(&artist.id, limit, &after)
        .await?;
    album_page(deps, user_id, order, &after, page).await
}

/// One page of streamable tracks.
pub async fn list_tracks(
    deps: &LibraryDeps,
    user_id: &str,
    query: &TrackQuery,
) -> Result<TrackListPage, LibraryFailure> {
    let (limit, _) = page(query.limit, None, 50, 500)?;
    let filter = TrackFilter {
        q: clean_q(query.q.as_deref()),
        album_id: query.album_id.clone(),
        artist_id: query.artist_id.clone(),
        genre: clean_q(query.genre.as_deref()),
    };
    let sort = track_sort(query.sort.as_deref())?;
    let descending = descending(query.order.as_deref())?;
    let order = format!("tracks/{sort:?}/{descending}");
    let after = start(
        query.after.as_deref(),
        &order,
        sql::track_keys(sort, descending).len(),
        sort == TrackSort::Random,
    )?;
    let page = deps
        .catalog
        .list_tracks(&filter, sort, descending, limit, &after)
        .await?;
    let ids: Vec<String> = page.items.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "track", &ids).await?;
    Ok(TrackListPage {
        items: page
            .items
            .into_iter()
            .map(|record| {
                let favorite = favorites.contains(&record.id);
                TrackView::from((record, favorite))
            })
            .collect(),
        total: page.total,
        next: next_cursor(order, &after, page.next),
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
    let favorite = favorites.contains(&record.id);
    Ok(TrackView::from((record, favorite)))
}

/// Library totals plus the caller's favorite counts, the review queue,
/// unidentified albums and the last finished scan.
pub async fn stats(deps: &LibraryDeps, user_id: &str) -> Result<StatsView, LibraryFailure> {
    let record = deps.catalog.stats().await?;
    let extras = deps.catalog.stats_extras().await?;
    let (albums, artists, tracks) = deps.catalog.favorite_counts(user_id).await?;
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
            .into_iter()
            .map(|record| {
                let favorite = favorites.contains(&record.id);
                AlbumView::from((record, favorite))
            })
            .collect(),
        total,
        next: None,
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
            .into_iter()
            .map(|record| {
                let favorite = favorites.contains(&record.id);
                TrackView::from((record, favorite))
            })
            .collect(),
        total,
        offset,
        limit,
    })
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
        ("recent", newest),
        ("rediscover", oldest),
        ("surprise", surprise),
        ("same_era", era),
    ] {
        for record in records {
            if seen.insert(record.id.clone()) {
                items.push(SuggestionTrack::from((record, reason)));
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
    let track = deps
        .catalog
        .get_track(track_id)
        .await?
        .ok_or(LibraryFailure::NotFound)?;
    let doc = deps
        .lyrics
        .get(&track.id)
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
    let owned = deps.catalog.owned_albums(&ids).await?;
    let requested = deps.catalog.requested_albums(&ids).await?;
    Ok(LibraryMembershipResponse {
        owned_ids: keep(owned),
        requested_ids: keep(requested),
    })
}

/// The tags in one catalog track's file, as v2 read them: admins only.
/// A file gone from disk or unreadable is a 400 with v2's message, one
/// outside the library roots a 403.
pub async fn track_tags(
    deps: &LibraryDeps,
    _admin: &Admin,
    track_id: &str,
) -> Result<TrackTags, LibraryFailure> {
    match deps.tags.read(track_id).await? {
        TagRead::Found(tag) => Ok(TrackTags::from(*tag)),
        TagRead::Unknown => Err(LibraryFailure::NotFound),
        TagRead::Gone => Err(LibraryFailure::InvalidInput(
            "The audio file is no longer present on disk.".to_owned(),
        )),
        TagRead::Unreadable => Err(LibraryFailure::InvalidInput(
            "Could not read the audio file.".to_owned(),
        )),
        TagRead::Outside => Err(LibraryFailure::Forbidden(
            droppedneedle_media::stream::gateway::OUTSIDE_MESSAGE.to_owned(),
        )),
    }
}

/// Every held release group and every open album request.
pub async fn album_mbids(deps: &LibraryDeps) -> Result<LibraryMbids, LibraryFailure> {
    let found = deps.catalog.all_album_mbids().await?;
    Ok(LibraryMbids {
        mbids: found.owned,
        requested_mbids: found.requested,
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
    let rank = |label: &str| droppedneedle_kernel::runtime_config::sections::tier_rank(label);
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
    let album_ids = deps.catalog.status_albums(identifier).await?;
    let records = if album_ids.is_empty() {
        Vec::new()
    } else {
        deps.catalog.album_tracks_batch(&album_ids).await?
    };
    let ids: Vec<String> = records.iter().map(|record| record.id.clone()).collect();
    let favorites = favorites_for(deps, user_id, "track", &ids).await?;
    let policy = UpgradePolicy::read(deps.upgrade_policy.as_ref());
    let album_id = records
        .first()
        .map(|record| record.album_id.clone())
        .or_else(|| album_ids.first().cloned())
        .unwrap_or_else(|| identifier.to_owned());
    let tracks: Vec<LibraryStatusTrack> = records
        .into_iter()
        .map(|record| {
            let tier = tier_for(&record.format, record.bit_rate, record.bit_depth);
            let favorite = favorites.contains(&record.id);
            LibraryStatusTrack {
                track: TrackView::from((record, favorite)),
                current_tier: tier.to_owned(),
                below_cutoff: below_cutoff(tier, &policy),
            }
        })
        .collect();
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
    let canonical = deps.catalog.resolve_albums(&identifiers).await?;
    let mut album_ids: Vec<String> = canonical.values().cloned().collect();
    album_ids.sort();
    album_ids.dedup();
    if album_ids.is_empty() {
        return Ok(ResolveTracksResponse { items });
    }
    let tracks = deps.catalog.album_tracks_batch(&album_ids).await?;
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
