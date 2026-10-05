//! Library browsing: views, items, latest, artists, genres, filters and
//! the owned-only discovery routes.

use crate::auth::compat_auth::jellyfin::{JellyfinPasswordStore, server_id};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;

use super::builders::{self, Builder, LIBRARY_INTERNAL_ID};
use super::models::{BaseItemDto, BaseItemDtoQueryResult};
use super::params::{self, CiParams, SortKey};
use super::query::{page, primary_type};
use super::router::*;
use super::seams::{
    ALL, AlbumFilter, AlbumView, ArtistScope, ArtistView, IdMap, ItemSort, LibraryRead,
    PlaybackSessions, StreamEngine, TrackFilter, TrackView,
};

// ===== Library browsing =====

/// The single "Music" view, both dialects (v2 `_views`).
pub(super) async fn views<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    if authed(&state.passwords, &headers, query.as_deref())
        .await
        .is_err()
    {
        return error(StatusCode::UNAUTHORIZED);
    }
    let library_id = state.ids.to_jf("library", LIBRARY_INTERNAL_ID).await;
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items: vec![builders::music_view(&library_id, &server_id())],
            total_record_count: 1,
            start_index: 0,
        },
    )
}
/// Decode a Jellyfin id to an artist mbid, `None` when undecodable or not an
/// artist (v2 `_decode_artist`).
pub(super) async fn decode_artist<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    jf_id: &str,
) -> Option<String>
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (kind, internal) = state.ids.from_jf(jf_id).await?;
    (kind == "artist").then_some(internal)
}

/// One item by decoded kind. Genre and library kinds have no branch in v2
/// `_single_item` either, so they resolve to `None` (skipped in `Ids`
/// lookups, 404 for direct fetch), as v2 does.
pub(super) async fn fetch_item<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    kind: &str,
    internal: &str,
) -> Option<super::models::BaseItemDto>
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let b = builder(state);
    match kind {
        "track" => {
            let t = state.library.track(user_id, internal).await?;
            Some(b.audio(&t).await)
        }
        "album" => {
            let a = state.library.album(user_id, internal).await?;
            Some(b.album(&a).await)
        }
        "artist" => {
            let a = state.library.artist(user_id, internal).await?;
            Some(b.artist(&a).await)
        }
        "playlist" => {
            let found = state
                .library
                .playlists(user_id)
                .await
                .into_iter()
                .find(|p| p.id == internal)?;
            Some(b.playlist(&found).await)
        }
        _ => None,
    }
}

/// One query result over already-built DTOs.
fn result(items: Vec<BaseItemDto>, total: usize, start: usize) -> Response {
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items,
            total_record_count: total,
            start_index: start,
        },
    )
}

/// The empty result (unknown parent, filter that resolved to nothing).
fn nothing(start: usize) -> Response {
    result(Vec::new(), 0, start)
}

/// `Limit=0` means everything from `start` (v2 `_build_page`).
fn or_all(limit: usize) -> usize {
    if limit == 0 { ALL } else { limit }
}

/// The page order for a parsed `SortBy`, else `fallback`.
fn sort_or(sort_key: Option<SortKey>, desc: bool, fallback: ItemSort) -> ItemSort {
    sort_key.map_or(fallback, |key| ItemSort::By(key, desc))
}

/// Decode artist ids; undecodable ids drop out.
async fn decode_artists<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    jf_ids: &[String],
) -> Vec<String>
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let mut mbids = Vec::new();
    for jf_id in jf_ids {
        if let Some(mbid) = decode_artist(state, jf_id).await
            && !mbids.contains(&mbid)
        {
            mbids.push(mbid);
        }
    }
    mbids
}

async fn build_tracks<I: IdMap>(b: &Builder<'_, I>, tracks: &[TrackView]) -> Vec<BaseItemDto> {
    let mut built = Vec::with_capacity(tracks.len());
    for track in tracks {
        built.push(b.audio(track).await);
    }
    built
}

async fn build_albums<I: IdMap>(b: &Builder<'_, I>, albums: &[AlbumView]) -> Vec<BaseItemDto> {
    let mut built = Vec::with_capacity(albums.len());
    for album in albums {
        built.push(b.album(album).await);
    }
    built
}

async fn build_artists<I: IdMap>(b: &Builder<'_, I>, artists: &[ArtistView]) -> Vec<BaseItemDto> {
    let mut built = Vec::with_capacity(artists.len());
    for artist in artists {
        built.push(b.artist(artist).await);
    }
    built
}

/// Main browse, both dialects (v2 `_browse`, same branch order).
pub(super) async fn browse<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let raw_query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    let authed = match authed(&state.passwords, &headers, raw_query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let user_id = authed.principal.id.clone();
    let q = CiParams::parse(raw_query.as_deref());
    let b = builder(&state);
    let start = params::qint(&q, "StartIndex", 0).max(0) as usize;
    let limit = params::qint(&q, "Limit", 100).max(0) as usize;
    let search = q.get("SearchTerm").map(str::to_owned);
    let types = params::csv_param(&q, "IncludeItemTypes");
    let ids = params::csv_param(&q, "Ids");
    let album_artist_ids = params::csv_param(&q, "AlbumArtistIds");
    let artist_ids = params::csv_param(&q, "ArtistIds");
    let contributing_ids = params::csv_param(&q, "ContributingArtistIds");
    let (sort_key, sort_desc) = params::browse_sort(&q);

    if !ids.is_empty() {
        let mut built = Vec::new();
        for jf_id in &ids {
            let Some((kind, internal)) = state.ids.from_jf(jf_id).await else {
                continue;
            };
            if let Some(item) = fetch_item(&state, &user_id, &kind, &internal).await {
                built.push(item);
            }
        }
        let (items, total) = page(&built, start, limit);
        return result(items, total, start);
    }

    let mut parent_kind: Option<String> = None;
    let mut parent_internal = String::new();
    if let Some(parent) = q.get("ParentId") {
        match state.ids.from_jf(parent).await {
            Some((kind, internal)) => {
                parent_kind = Some(kind);
                parent_internal = internal;
            }
            None => return nothing(start),
        }
    }

    if params::wants_favorites(&q) {
        return favorites_browse(&state, &user_id, &types, start, limit).await;
    }

    if parent_kind.as_deref() == Some("album") {
        let filter = TrackFilter {
            album: Some(parent_internal),
            ..TrackFilter::default()
        };
        let (tracks, total) = state
            .library
            .track_page(&user_id, &filter, ItemSort::Disc, start, or_all(limit))
            .await;
        return result(build_tracks(&b, &tracks).await, total, start);
    }

    match primary_type(&types) {
        "MusicArtist" => {
            let size = if limit == 0 { 100_000 } else { limit };
            let (artists, total) = state
                .library
                .artist_page(&user_id, ArtistScope::All, search.as_deref(), start, size)
                .await;
            result(build_artists(&b, &artists).await, total, start)
        }
        "MusicGenre" => {
            let genres = state.library.genres().await;
            let (genres, total) = page(&genres, start, limit);
            let mut built = Vec::with_capacity(genres.len());
            for g in &genres {
                built.push(b.genre(g).await);
            }
            result(built, total, start)
        }
        "Playlist" => {
            let views = state.library.playlists(&user_id).await;
            let (views, total) = page(&views, start, limit);
            let mut built = Vec::with_capacity(views.len());
            for v in &views {
                built.push(b.playlist(v).await);
            }
            result(built, total, start)
        }
        "Audio" => {
            audio_browse(
                &state,
                &user_id,
                search.as_deref(),
                &album_artist_ids,
                &artist_ids,
                sort_key,
                sort_desc,
                start,
                limit,
            )
            .await
        }
        _ => {
            album_browse(
                &state,
                &user_id,
                search.as_deref(),
                &album_artist_ids,
                &artist_ids,
                &contributing_ids,
                sort_key,
                sort_desc,
                start,
                limit,
            )
            .await
        }
    }
}

/// Favorite listing for browse (v2 `_favorite_items`): one batch read per
/// kind, DTOs for the page only.
pub(super) async fn favorites_browse<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    types: &[String],
    start: usize,
    limit: usize,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let b = builder(state);
    let kind = match primary_type(types) {
        "MusicArtist" => "artist",
        "MusicAlbum" => "album",
        _ => "track",
    };
    let ids = state.library.favorites(user_id, kind).await;
    match kind {
        "track" => {
            let tracks = state.library.tracks_by_ids(user_id, &ids).await;
            let (tracks, total) = page(&tracks, start, limit);
            result(build_tracks(&b, &tracks).await, total, start)
        }
        "album" => {
            let albums = state.library.albums_by_ids(user_id, &ids).await;
            let (albums, total) = page(&albums, start, limit);
            result(build_albums(&b, &albums).await, total, start)
        }
        _ => {
            let mut artists = Vec::with_capacity(ids.len());
            for id in &ids {
                if let Some(artist) = state.library.artist(user_id, id).await {
                    artists.push(artist);
                }
            }
            let (artists, total) = page(&artists, start, limit);
            result(build_artists(&b, &artists).await, total, start)
        }
    }
}

/// Track browse arm (v2 `_browse` `Audio` branch).
#[allow(clippy::too_many_arguments)]
pub(super) async fn audio_browse<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    search: Option<&str>,
    album_artist_ids: &[String],
    artist_ids: &[String],
    sort_key: Option<SortKey>,
    sort_desc: bool,
    start: usize,
    limit: usize,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let b = builder(state);
    let sort = sort_or(sort_key, sort_desc, ItemSort::Catalog);
    let (filter, size) = if !album_artist_ids.is_empty() {
        let album_artists = decode_artists(state, album_artist_ids).await;
        if album_artists.is_empty() {
            return nothing(start);
        }
        let filter = TrackFilter {
            album_artists,
            ..TrackFilter::default()
        };
        (filter, or_all(limit))
    } else if !artist_ids.is_empty() {
        let artists = decode_artists(state, artist_ids).await;
        if artists.is_empty() {
            return nothing(start);
        }
        let filter = TrackFilter {
            artists,
            ..TrackFilter::default()
        };
        (filter, or_all(limit))
    } else {
        let filter = TrackFilter {
            search: search.map(str::to_owned),
            ..TrackFilter::default()
        };
        (filter, if limit == 0 { 100 } else { limit })
    };
    let (tracks, total) = state
        .library
        .track_page(user_id, &filter, sort, start, size)
        .await;
    result(build_tracks(&b, &tracks).await, total, start)
}

/// Album browse arm (v2 `_browse` fallthrough branch).
#[allow(clippy::too_many_arguments)]
pub(super) async fn album_browse<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    search: Option<&str>,
    album_artist_ids: &[String],
    artist_ids: &[String],
    contributing_ids: &[String],
    sort_key: Option<SortKey>,
    sort_desc: bool,
    start: usize,
    limit: usize,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let b = builder(state);
    let size = if limit == 0 { 100 } else { limit };
    let (filter, sort, offset, size) = if !contributing_ids.is_empty() {
        let appears_on = decode_artists(state, contributing_ids).await;
        if appears_on.is_empty() {
            // A contributor filter must never fall through to the full catalog.
            return nothing(start);
        }
        let filter = AlbumFilter {
            appears_on,
            ..AlbumFilter::default()
        };
        let newest = ItemSort::By(SortKey::Recent, true);
        (
            filter,
            sort_or(sort_key, sort_desc, newest),
            start,
            or_all(limit),
        )
    } else if !album_artist_ids.is_empty() || !artist_ids.is_empty() {
        let ids = album_artist_ids
            .iter()
            .chain(artist_ids)
            .cloned()
            .collect::<Vec<_>>();
        let artists = decode_artists(state, &ids).await;
        if artists.is_empty() {
            return nothing(start);
        }
        let filter = AlbumFilter {
            artists,
            ..AlbumFilter::default()
        };
        (
            filter,
            sort_or(sort_key, sort_desc, ItemSort::Catalog),
            start,
            or_all(limit),
        )
    } else {
        let filter = AlbumFilter {
            search: search.map(str::to_owned),
            ..AlbumFilter::default()
        };
        // Without SortBy, v2 pages by `page = start // limit + 1`.
        let offset = match (sort_key, limit) {
            (Some(_), _) => start,
            (None, 0) => 0,
            (None, _) => (start / limit) * size,
        };
        (
            filter,
            sort_or(sort_key, sort_desc, ItemSort::Catalog),
            offset,
            size,
        )
    };
    let (albums, total) = state
        .library
        .album_page(user_id, &filter, sort, offset, size)
        .await;
    result(build_albums(&b, &albums).await, total, start)
}

/// Jellify Recently Added: a bare JSON array of the newest albums (v2
/// `_latest`). `ParentId`, when given, must be the music library.
pub(super) async fn latest<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let raw_query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    let authed = match authed(&state.passwords, &headers, raw_query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(raw_query.as_deref());
    if let Some(parent) = q.get("ParentId") {
        match state.ids.from_jf(parent).await {
            Some((kind, _)) if kind == "library" => {}
            _ => return json(StatusCode::OK, &Vec::<BaseItemDto>::new()),
        }
    }
    let mut limit = params::qint(&q, "Limit", 10);
    if limit <= 0 {
        limit = 10;
    }
    let b = builder(&state);
    let (albums, _) = state
        .library
        .album_page(
            &authed.principal.id,
            &AlbumFilter::default(),
            ItemSort::By(SortKey::Recent, true),
            0,
            limit as usize,
        )
        .await;
    json(StatusCode::OK, &build_albums(&b, &albums).await)
}

pub(super) async fn artists<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    artists_scoped(State(state), request, ArtistScope::All).await
}

pub(super) async fn album_artists<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    artists_scoped(State(state), request, ArtistScope::Album).await
}

pub(super) async fn artists_scoped<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
    scope: ArtistScope,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let raw_query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    let authed = match authed(&state.passwords, &headers, raw_query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(raw_query.as_deref());
    let start = params::qint(&q, "StartIndex", 0).max(0) as usize;
    let limit = params::qint(&q, "Limit", 100).max(0) as usize;
    let size = if limit == 0 { 100_000 } else { limit };
    let (artists, total) = state
        .library
        .artist_page(
            &authed.principal.id,
            scope,
            q.get("SearchTerm"),
            start,
            size,
        )
        .await;
    let b = builder(&state);
    result(build_artists(&b, &artists).await, total, start)
}

/// Both genre dialects share one handler (v2 `_genres`).
pub(super) async fn genres<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let raw_query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    let authed = match authed(&state.passwords, &headers, raw_query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let _ = &authed;
    let q = CiParams::parse(raw_query.as_deref());
    let start = params::qint(&q, "StartIndex", 0).max(0) as usize;
    let limit = params::qint(&q, "Limit", 100).max(0) as usize;
    let genres = state.library.genres().await;
    let (genres, total) = page(&genres, start, limit);
    let b = builder(&state);
    let mut built = Vec::with_capacity(genres.len());
    for g in &genres {
        built.push(b.genre(g).await);
    }
    result(built, total, start)
}

/// Manet's boot call: a 404 here can leave the library empty (v2
/// `_items_filters`). Genres are the only facet modelled; the key name
/// `OfficialRatings` is v2-verbatim.
pub(super) async fn filters<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let raw_query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    if authed(&state.passwords, &headers, raw_query.as_deref())
        .await
        .is_err()
    {
        return error(StatusCode::UNAUTHORIZED);
    }
    let genres = state.library.genres().await;
    json(
        StatusCode::OK,
        &serde_json::json!({
            "Genres": genres.iter().map(|g| &g.name).collect::<Vec<_>>(),
            "Tags": Vec::<String>::new(),
            "OfficialRatings": Vec::<String>::new(),
            "Years": Vec::<u32>::new(),
        }),
    )
}

pub(super) async fn single_item<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let raw_query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    let authed = match authed(&state.passwords, &headers, raw_query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let Some((kind, internal)) = state.ids.from_jf(&item_id).await else {
        return error(StatusCode::NOT_FOUND);
    };
    match fetch_item(&state, &authed.principal.id, &kind, &internal).await {
        Some(item) => json(StatusCode::OK, &item),
        None => error(StatusCode::NOT_FOUND),
    }
}

// ===== Discovery (owned-only) =====

/// Similar + both InstantMix routes share one handler (v2 `_similar`):
/// resolves the artist from an artist/track/album id and serves same-artist
/// tracks. Unknown ids and unresolvable kinds yield an empty result, not a
/// 404. (No real similarity ranking is bound yet.)
pub(super) async fn similar<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(item_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |ctx| {
        let raw_query = request.uri().query().map(str::to_owned);
        let q = CiParams::parse(raw_query.as_deref());
        let mut limit = params::qint(&q, "Limit", 50);
        if limit == 0 {
            limit = 50;
        }
        let limit = limit.max(1) as usize;
        let empty = BaseItemDtoQueryResult {
            items: Vec::new(),
            total_record_count: 0,
            start_index: 0,
        };
        let Some((kind, internal)) = state.ids.from_jf(&item_id).await else {
            return json(StatusCode::OK, &empty);
        };
        let artist_mbid = match kind.as_str() {
            "artist" => Some(internal),
            "track" => state
                .library
                .track(&ctx.principal.id, &internal)
                .await
                .and_then(|t| t.artist_mbid),
            "album" => state
                .library
                .album(&ctx.principal.id, &internal)
                .await
                .and_then(|a| a.artist_mbid),
            _ => None,
        };
        let Some(artist_mbid) = artist_mbid else {
            return json(StatusCode::OK, &empty);
        };
        let filter = TrackFilter {
            artists: vec![artist_mbid],
            ..TrackFilter::default()
        };
        let (tracks, _) = state
            .library
            .track_page(&ctx.principal.id, &filter, ItemSort::Catalog, 0, limit)
            .await;
        let b = builder(&state);
        let built = build_tracks(&b, &tracks).await;
        let total = built.len();
        result(built, total, 0)
    })
}
