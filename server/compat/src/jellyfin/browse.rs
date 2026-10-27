//! Library browsing: views, items, latest, artists, genres, filters and
//! the owned-only discovery routes.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use droppedneedle_auth::compat_auth::jellyfin::server_id;
use droppedneedle_kernel::error::ApiError;

use super::builders::{self, Builder, LIBRARY_INTERNAL_ID};
use super::models::{BaseItemDto, BaseItemDtoQueryResult};
use super::params::{self, CiParams, SortKey};
use super::query::{page, primary_type};
use super::router::*;
use super::seams::{
    ALL, AlbumFilter, AlbumView, ArtistScope, ArtistView, ItemSort, ReadFailed, TrackFilter,
    TrackView,
};

// ===== Library browsing =====

/// The single "Music" view, both dialects (v2 `_views`).
pub(super) async fn views(
    State(state): State<JellyfinState>,
    authed: Authed,
) -> Result<Response, ApiError> {
    let library_id = state.ids.to_jf("library", LIBRARY_INTERNAL_ID).await;
    let (_, album_artists) = state
        .library
        .artist_page(&authed.principal.id, ArtistScope::Album, None, 0, 0)
        .await?;
    Ok(json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items: vec![builders::music_view(
                &library_id,
                &server_id(),
                album_artists,
            )],
            total_record_count: 1,
            start_index: 0,
        },
    ))
}
/// Decode a Jellyfin id to an artist mbid, `Ok(None)` when undecodable or
/// not an artist (v2 `_decode_artist`).
pub(super) async fn decode_artist(
    state: &JellyfinState,
    jf_id: &str,
) -> Result<Option<String>, ReadFailed> {
    Ok(match state.ids.from_jf(jf_id).await? {
        Some((kind, internal)) if kind == "artist" => Some(internal),
        _ => None,
    })
}

/// One item by decoded kind. Genre and library kinds have no branch in v2
/// `_single_item` either, so they resolve to `None` (skipped in `Ids`
/// lookups, 404 for direct fetch), as v2 does.
pub(super) async fn fetch_item(
    state: &JellyfinState,
    user_id: &str,
    kind: &str,
    internal: &str,
) -> Result<Option<super::models::BaseItemDto>, ReadFailed> {
    let b = builder(state);
    Ok(match kind {
        "track" => match state.library.track(user_id, internal).await? {
            Some(t) => Some(b.audio(&t).await),
            None => None,
        },
        "album" => match state.library.album(user_id, internal).await? {
            Some(a) => Some(b.album(&a).await),
            None => None,
        },
        "artist" => match state.library.artist(user_id, internal).await? {
            Some(a) => Some(b.artist(&a).await),
            None => None,
        },
        "playlist" => {
            let found = state
                .library
                .playlists(user_id)
                .await?
                .into_iter()
                .find(|p| p.id == internal);
            match found {
                Some(found) => Some(b.playlist(&found).await),
                None => None,
            }
        }
        _ => None,
    })
}

/// One query result over already-built DTOs. Without a counted `total`
/// the page's length stands in, as Jellyfin sends it when asked for no
/// total.
fn result(items: Vec<BaseItemDto>, total: Option<usize>, start: usize) -> Response {
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            total_record_count: total.unwrap_or(items.len()),
            items,
            start_index: start,
        },
    )
}

/// One item in the long form Jellyfin sends for a fetch by id.
async fn fetch_full_item(
    state: &JellyfinState,
    user: &super::seams::Principal,
    kind: &str,
    internal: &str,
) -> Result<Option<BaseItemDto>, ReadFailed> {
    let b = builder(state);
    Ok(match kind {
        "track" => match state.library.track(&user.id, internal).await? {
            Some(t) => {
                let settings = state.settings.jellyfin();
                let can_download = settings.policy_facts(&user.role).content_downloading;
                let can_transcode = settings.transcoding_enabled && settings.ffmpeg_available;
                Some(b.audio_full(&t, can_download, can_transcode).await)
            }
            None => None,
        },
        "album" => match state.library.album(&user.id, internal).await? {
            Some(a) => Some(b.album_full(&a).await),
            None => None,
        },
        "artist" => match state.library.artist(&user.id, internal).await? {
            Some(a) => {
                let library_id = state.ids.to_jf("library", LIBRARY_INTERNAL_ID).await;
                Some(b.artist_full(&a, &library_id).await)
            }
            None => None,
        },
        "playlist" => {
            let found = state
                .library
                .playlists(&user.id)
                .await?
                .into_iter()
                .find(|p| p.id == internal);
            match found {
                Some(found) => Some(b.playlist_full(&found).await),
                None => None,
            }
        }
        _ => None,
    })
}

/// The empty result (unknown parent, filter that resolved to nothing).
fn nothing(start: usize) -> Response {
    result(Vec::new(), Some(0), start)
}

/// `Limit=0` means everything from `start` (v2 `_build_page`).
fn or_all(limit: usize) -> usize {
    if limit == 0 { ALL } else { limit }
}

/// One page as a request asks for it: `start` is `StartIndex`, the offset
/// in every order, `limit` is `Limit` as asked, and `counted` is
/// `EnableTotalRecordCount`.
#[derive(Clone, Copy)]
pub(super) struct Paging {
    start: usize,
    limit: usize,
    counted: bool,
}

/// The page order for a parsed `SortBy`, else `fallback`.
fn sort_or(sort_key: Option<SortKey>, desc: bool, fallback: ItemSort) -> ItemSort {
    sort_key.map_or(fallback, |key| ItemSort::By(key, desc))
}

/// Decode artist ids; undecodable ids drop out.
async fn decode_artists(
    state: &JellyfinState,
    jf_ids: &[String],
) -> Result<Vec<String>, ReadFailed> {
    let mut mbids = Vec::new();
    for jf_id in jf_ids {
        if let Some(mbid) = decode_artist(state, jf_id).await?
            && !mbids.contains(&mbid)
        {
            mbids.push(mbid);
        }
    }
    Ok(mbids)
}

async fn build_tracks(b: &Builder<'_>, tracks: &[TrackView]) -> Vec<BaseItemDto> {
    let mut built = Vec::with_capacity(tracks.len());
    for track in tracks {
        built.push(b.audio(track).await);
    }
    built
}

async fn build_albums(b: &Builder<'_>, albums: &[AlbumView]) -> Vec<BaseItemDto> {
    let mut built = Vec::with_capacity(albums.len());
    for album in albums {
        built.push(b.album(album).await);
    }
    built
}

async fn build_artists(b: &Builder<'_>, artists: &[ArtistView]) -> Vec<BaseItemDto> {
    let mut built = Vec::with_capacity(artists.len());
    for artist in artists {
        built.push(b.artist(artist).await);
    }
    built
}

/// Main browse, both dialects (v2 `_browse`, same branch order).
pub(super) async fn browse(
    State(state): State<JellyfinState>,
    authed: Authed,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let raw_query = request.uri().query().map(str::to_owned);
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
    let played = params::played_filter(&q);
    let counted = params::wants_total(&q);
    let paging = Paging {
        start,
        limit,
        counted,
    };

    if !ids.is_empty() {
        let mut built = Vec::new();
        for (kind, internal) in state.ids.from_jf_many(&ids).await?.into_iter().flatten() {
            if let Some(item) = fetch_item(&state, &user_id, &kind, &internal).await? {
                built.push(item);
            }
        }
        let (items, total) = page(&built, start, limit);
        return Ok(result(items, counted.then_some(total), start));
    }

    let mut parent_kind: Option<String> = None;
    let mut parent_internal = String::new();
    if let Some(parent) = q.get("ParentId") {
        match state.ids.from_jf(parent).await? {
            Some((kind, internal)) => {
                parent_kind = Some(kind);
                parent_internal = internal;
            }
            None => return Ok(nothing(start)),
        }
    }

    if params::wants_favorites(&q) {
        return favorites_browse(&state, &user_id, &types, played, paging).await;
    }

    if parent_kind.as_deref() == Some("album") {
        let filter = TrackFilter {
            album: Some(parent_internal),
            played,
            ..TrackFilter::default()
        };
        let (tracks, total) = state
            .library
            .track_page(
                &user_id,
                &filter,
                ItemSort::Disc,
                start,
                or_all(limit),
                counted,
            )
            .await?;
        return Ok(result(build_tracks(&b, &tracks).await, total, start));
    }

    match primary_type(&types) {
        "MusicArtist" => {
            let size = if limit == 0 { 100_000 } else { limit };
            let (artists, total) = state
                .library
                .artist_page(&user_id, ArtistScope::All, search.as_deref(), start, size)
                .await?;
            let built = build_artists(&b, &artists).await;
            Ok(result(built, counted.then_some(total), start))
        }
        "MusicGenre" => {
            let genres = state.library.genres().await?;
            let (genres, total) = page(&genres, start, limit);
            let mut built = Vec::with_capacity(genres.len());
            for g in &genres {
                built.push(b.genre(g).await);
            }
            Ok(result(built, counted.then_some(total), start))
        }
        "Playlist" => {
            let views = state.library.playlists(&user_id).await?;
            let (views, total) = page(&views, start, limit);
            let mut built = Vec::with_capacity(views.len());
            for v in &views {
                built.push(b.playlist(v).await);
            }
            Ok(result(built, counted.then_some(total), start))
        }
        "Audio" => {
            audio_browse(
                &state,
                &user_id,
                search.as_deref(),
                &album_artist_ids,
                &artist_ids,
                played,
                sort_key,
                sort_desc,
                paging,
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
                played,
                sort_key,
                sort_desc,
                paging,
            )
            .await
        }
    }
}

/// Favorite listing for browse (v2 `_favorite_items`): one batch read per
/// kind, DTOs for the page only. `played` narrows favourite tracks and
/// albums to the ones that are or are not played for the caller.
pub(super) async fn favorites_browse(
    state: &JellyfinState,
    user_id: &str,
    types: &[String],
    played: Option<bool>,
    paging: Paging,
) -> Result<Response, ApiError> {
    let Paging {
        start,
        limit,
        counted,
    } = paging;
    let b = builder(state);
    let kind = match primary_type(types) {
        "MusicArtist" => "artist",
        "MusicAlbum" => "album",
        _ => "track",
    };
    let ids = state.library.favorites(user_id, kind).await?;
    Ok(match kind {
        "track" => {
            let mut tracks = state.library.tracks_by_ids(user_id, &ids).await?;
            if let Some(played) = played {
                tracks.retain(|track| track.played == played);
            }
            let (tracks, total) = page(&tracks, start, limit);
            result(
                build_tracks(&b, &tracks).await,
                counted.then_some(total),
                start,
            )
        }
        "album" => {
            let mut albums = state.library.albums_by_ids(user_id, &ids).await?;
            if let Some(played) = played {
                albums.retain(|album| album.played == played);
            }
            let (albums, total) = page(&albums, start, limit);
            result(
                build_albums(&b, &albums).await,
                counted.then_some(total),
                start,
            )
        }
        _ => {
            let mut artists = Vec::with_capacity(ids.len());
            for id in &ids {
                if let Some(artist) = state.library.artist(user_id, id).await? {
                    artists.push(artist);
                }
            }
            let (artists, total) = page(&artists, start, limit);
            result(
                build_artists(&b, &artists).await,
                counted.then_some(total),
                start,
            )
        }
    })
}

/// Track browse arm (v2 `_browse` `Audio` branch).
#[allow(clippy::too_many_arguments)]
pub(super) async fn audio_browse(
    state: &JellyfinState,
    user_id: &str,
    search: Option<&str>,
    album_artist_ids: &[String],
    artist_ids: &[String],
    played: Option<bool>,
    sort_key: Option<SortKey>,
    sort_desc: bool,
    paging: Paging,
) -> Result<Response, ApiError> {
    let Paging {
        start,
        limit,
        counted,
    } = paging;
    let b = builder(state);
    let sort = sort_or(sort_key, sort_desc, ItemSort::Catalog);
    let (mut filter, size) = if !album_artist_ids.is_empty() {
        let album_artists = decode_artists(state, album_artist_ids).await?;
        if album_artists.is_empty() {
            return Ok(nothing(start));
        }
        let filter = TrackFilter {
            album_artists,
            ..TrackFilter::default()
        };
        (filter, or_all(limit))
    } else if !artist_ids.is_empty() {
        let artists = decode_artists(state, artist_ids).await?;
        if artists.is_empty() {
            return Ok(nothing(start));
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
    filter.played = played;
    let (tracks, total) = state
        .library
        .track_page(user_id, &filter, sort, start, size, counted)
        .await?;
    Ok(result(build_tracks(&b, &tracks).await, total, start))
}

/// Album browse arm (v2 `_browse` fallthrough branch).
#[allow(clippy::too_many_arguments)]
pub(super) async fn album_browse(
    state: &JellyfinState,
    user_id: &str,
    search: Option<&str>,
    album_artist_ids: &[String],
    artist_ids: &[String],
    contributing_ids: &[String],
    played: Option<bool>,
    sort_key: Option<SortKey>,
    sort_desc: bool,
    paging: Paging,
) -> Result<Response, ApiError> {
    let Paging {
        start,
        limit,
        counted,
    } = paging;
    let b = builder(state);
    let (filter, sort, size) = if !contributing_ids.is_empty() {
        let appears_on = decode_artists(state, contributing_ids).await?;
        if appears_on.is_empty() {
            // A contributor filter must never fall through to the full catalog.
            return Ok(nothing(start));
        }
        let filter = AlbumFilter {
            appears_on,
            ..AlbumFilter::default()
        };
        let newest = ItemSort::By(SortKey::Recent, true);
        (filter, sort_or(sort_key, sort_desc, newest), or_all(limit))
    } else if !album_artist_ids.is_empty() || !artist_ids.is_empty() {
        let ids = album_artist_ids
            .iter()
            .chain(artist_ids)
            .cloned()
            .collect::<Vec<_>>();
        let artists = decode_artists(state, &ids).await?;
        if artists.is_empty() {
            return Ok(nothing(start));
        }
        let filter = AlbumFilter {
            artists,
            ..AlbumFilter::default()
        };
        (
            filter,
            sort_or(sort_key, sort_desc, ItemSort::Catalog),
            or_all(limit),
        )
    } else {
        let filter = AlbumFilter {
            search: search.map(str::to_owned),
            ..AlbumFilter::default()
        };
        (
            filter,
            sort_or(sort_key, sort_desc, ItemSort::Catalog),
            if limit == 0 { 100 } else { limit },
        )
    };
    let filter = AlbumFilter { played, ..filter };
    let (albums, total) = state
        .library
        .album_page(user_id, &filter, sort, start, size, counted)
        .await?;
    Ok(result(build_albums(&b, &albums).await, total, start))
}

/// Jellify Recently Added: a bare JSON array of the newest albums (v2
/// `_latest`). `ParentId`, when given, must be the music library.
pub(super) async fn latest(
    State(state): State<JellyfinState>,
    authed: Authed,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let raw_query = request.uri().query().map(str::to_owned);
    let q = CiParams::parse(raw_query.as_deref());
    if let Some(parent) = q.get("ParentId") {
        match state.ids.from_jf(parent).await? {
            Some((kind, _)) if kind == "library" => {}
            _ => return Ok(json(StatusCode::OK, &Vec::<BaseItemDto>::new())),
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
            false,
        )
        .await?;
    Ok(json(StatusCode::OK, &build_albums(&b, &albums).await))
}

pub(super) async fn artists(
    State(state): State<JellyfinState>,
    authed: Authed,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    artists_scoped(State(state), authed, request, ArtistScope::All).await
}

pub(super) async fn album_artists(
    State(state): State<JellyfinState>,
    authed: Authed,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    artists_scoped(State(state), authed, request, ArtistScope::Album).await
}

pub(super) async fn artists_scoped(
    State(state): State<JellyfinState>,
    authed: Authed,
    request: Request<Body>,
    scope: ArtistScope,
) -> Result<Response, ApiError> {
    let raw_query = request.uri().query().map(str::to_owned);
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
        .await?;
    let b = builder(&state);
    let built = build_artists(&b, &artists).await;
    Ok(result(
        built,
        params::wants_total(&q).then_some(total),
        start,
    ))
}

/// Both genre dialects share one handler (v2 `_genres`).
pub(super) async fn genres(
    State(state): State<JellyfinState>,
    _caller: Authed,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let raw_query = request.uri().query().map(str::to_owned);
    let q = CiParams::parse(raw_query.as_deref());
    let start = params::qint(&q, "StartIndex", 0).max(0) as usize;
    let limit = params::qint(&q, "Limit", 100).max(0) as usize;
    let genres = state.library.genres().await?;
    let (genres, total) = page(&genres, start, limit);
    let b = builder(&state);
    let mut built = Vec::with_capacity(genres.len());
    for g in &genres {
        built.push(b.genre(g).await);
    }
    Ok(result(
        built,
        params::wants_total(&q).then_some(total),
        start,
    ))
}

/// Manet's boot call: a 404 here can leave the library empty (v2
/// `_items_filters`). Genres and album years are the facets modelled; the
/// key name `OfficialRatings` is v2-verbatim.
pub(super) async fn filters(
    State(state): State<JellyfinState>,
    _caller: Authed,
) -> Result<Response, ApiError> {
    let genres = state.library.genres().await?;
    let years = state.library.years().await?;
    Ok(json(
        StatusCode::OK,
        &serde_json::json!({
            "Genres": genres.iter().map(|g| &g.name).collect::<Vec<_>>(),
            "Tags": Vec::<String>::new(),
            "OfficialRatings": Vec::<String>::new(),
            "Years": years,
        }),
    ))
}

pub(super) async fn single_item(
    State(state): State<JellyfinState>,
    Path(item_id): Path<String>,
    authed: Authed,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let Some((kind, internal)) = state.ids.from_jf(&item_id).await? else {
        return Ok(not_found_item(&request));
    };
    Ok(
        match fetch_full_item(&state, &authed.principal, &kind, &internal).await? {
            Some(item) => json(StatusCode::OK, &item),
            None => not_found_item(&request),
        },
    )
}

/// The older address, with a user id in the path. The id is not read: the
/// token decides whose library answers, as on the other `/Users/{id}/...`
/// routes.
pub(super) async fn single_item_legacy(
    state: State<JellyfinState>,
    Path((_user_id, item_id)): Path<(String, String)>,
    authed: Authed,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    single_item(state, Path(item_id), authed, request).await
}

// ===== Discovery (owned-only) =====

/// Similar + both InstantMix routes share one handler (v2 `_similar`):
/// resolves the artist from an artist/track/album id and serves same-artist
/// tracks. Unknown ids and unresolvable kinds yield an empty result, not a
/// 404. (No real similarity ranking is bound yet.)
pub(super) async fn similar(
    State(state): State<JellyfinState>,
    Path(item_id): Path<String>,
    ctx: Authed,
    request: Request<Body>,
) -> Result<Response, ApiError> {
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
    let Some((kind, internal)) = state.ids.from_jf(&item_id).await? else {
        return Ok(json(StatusCode::OK, &empty));
    };
    let artist_id = match kind.as_str() {
        "artist" => Some(internal),
        "track" => state
            .library
            .track(&ctx.principal.id, &internal)
            .await?
            .and_then(|t| t.artist_id),
        "album" => state
            .library
            .album(&ctx.principal.id, &internal)
            .await?
            .and_then(|a| a.artist_id),
        _ => None,
    };
    let Some(artist_id) = artist_id else {
        return Ok(json(StatusCode::OK, &empty));
    };
    let filter = TrackFilter {
        artists: vec![artist_id],
        ..TrackFilter::default()
    };
    let (tracks, _) = state
        .library
        .track_page(
            &ctx.principal.id,
            &filter,
            ItemSort::Catalog,
            0,
            limit,
            false,
        )
        .await?;
    let b = builder(&state);
    Ok(result(build_tracks(&b, &tracks).await, None, 0))
}
