//! Library browsing: views, items, latest, artists, genres, filters and
//! the owned-only discovery routes.

use crate::auth::compat_auth::jellyfin::{JellyfinPasswordStore, server_id};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;

use super::builders::{self, LIBRARY_INTERNAL_ID};
use super::models::BaseItemDtoQueryResult;
use super::params::{self, CiParams, SortKey};
use super::query::*;
use super::router::*;
use super::seams::{ArtistScope, IdMap, LibraryRead, PlaybackSessions, StreamEngine};

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
        return json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items,
                total_record_count: total,
                start_index: start,
            },
        );
    }

    let mut parent_kind: Option<String> = None;
    let mut parent_internal = String::new();
    if let Some(parent) = q.get("ParentId") {
        match state.ids.from_jf(parent).await {
            Some((kind, internal)) => {
                parent_kind = Some(kind);
                parent_internal = internal;
            }
            None => {
                return json(
                    StatusCode::OK,
                    &BaseItemDtoQueryResult {
                        items: Vec::new(),
                        total_record_count: 0,
                        start_index: start,
                    },
                );
            }
        }
    }

    if params::wants_favorites(&q) {
        return favorites_browse(&state, &user_id, &types, start, limit).await;
    }

    if parent_kind.as_deref() == Some("album") {
        let mut tracks: Vec<_> = state
            .library
            .tracks(&user_id)
            .await
            .into_iter()
            .filter(|t| t.rg_mbid.as_deref() == Some(parent_internal.as_str()))
            .collect();
        tracks.sort_by(|a, b| {
            (
                a.disc_number.unwrap_or(0),
                a.track_number.unwrap_or(0),
                &a.file_id,
            )
                .cmp(&(
                    b.disc_number.unwrap_or(0),
                    b.track_number.unwrap_or(0),
                    &b.file_id,
                ))
        });
        let mut built = Vec::with_capacity(tracks.len());
        for t in &tracks {
            built.push(b.audio(t).await);
        }
        let (items, total) = page(&built, start, limit);
        return json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items,
                total_record_count: total,
                start_index: start,
            },
        );
    }

    match primary_type(&types) {
        "MusicArtist" => {
            let mut artists = state.library.artists(&user_id, ArtistScope::All).await;
            if let Some(needle) = search.as_deref() {
                artists.retain(|a| matches(&[Some(a.name.clone())], needle));
            }
            let total = artists.len();
            let size = if limit == 0 { 100_000 } else { limit };
            let items = artists.get(start..(start + size).min(total)).unwrap_or(&[]);
            let mut built = Vec::with_capacity(items.len());
            for a in items {
                built.push(b.artist(a).await);
            }
            json(
                StatusCode::OK,
                &BaseItemDtoQueryResult {
                    items: built,
                    total_record_count: total,
                    start_index: start,
                },
            )
        }
        "MusicGenre" => {
            let genres = state.library.genres().await;
            let mut built = Vec::with_capacity(genres.len());
            for g in &genres {
                built.push(b.genre(g).await);
            }
            let (items, total) = page(&built, start, limit);
            json(
                StatusCode::OK,
                &BaseItemDtoQueryResult {
                    items,
                    total_record_count: total,
                    start_index: start,
                },
            )
        }
        "Playlist" => {
            let views = state.library.playlists(&user_id).await;
            let mut built = Vec::with_capacity(views.len());
            for v in &views {
                built.push(b.playlist(v).await);
            }
            let (items, total) = page(&built, start, limit);
            json(
                StatusCode::OK,
                &BaseItemDtoQueryResult {
                    items,
                    total_record_count: total,
                    start_index: start,
                },
            )
        }
        "Audio" => {
            audio_browse(
                &state,
                &user_id,
                &q,
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
                &q,
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

/// Favorite listing for browse (v2 `_favorite_items`).
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
    let kind = match primary_type(types) {
        "MusicArtist" => "artist",
        "MusicAlbum" => "album",
        "Audio" => "track",
        _ => "track",
    };
    let mut built = Vec::new();
    for internal in state.library.favorites(user_id, kind).await {
        if let Some(item) = fetch_item(state, user_id, kind, &internal).await {
            built.push(item);
        }
    }
    let (items, total) = page(&built, start, limit);
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items,
            total_record_count: total,
            start_index: start,
        },
    )
}

/// Track browse arm (v2 `_browse` `Audio` branch).
#[allow(clippy::too_many_arguments)]
pub(super) async fn audio_browse<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    _q: &CiParams,
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
    let size = if limit == 0 { 100 } else { limit };
    if !album_artist_ids.is_empty() {
        let mut mbids = Vec::new();
        for jf_id in album_artist_ids {
            if let Some(m) = decode_artist(state, jf_id).await {
                mbids.push(m);
            }
        }
        let mut tracks: Vec<_> = state
            .library
            .tracks(user_id)
            .await
            .into_iter()
            .filter(|t| {
                t.album_artist_mbid
                    .as_ref()
                    .is_some_and(|m| mbids.contains(m))
            })
            .collect();
        if let Some(key) = sort_key {
            sort_tracks(&mut tracks, key, sort_desc);
        }
        let mut built = Vec::with_capacity(tracks.len());
        for t in &tracks {
            built.push(b.audio(t).await);
        }
        let (items, total) = page(&built, start, limit);
        return json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items,
                total_record_count: total,
                start_index: start,
            },
        );
    }
    if !artist_ids.is_empty() {
        let mut mbids = Vec::new();
        for jf_id in artist_ids {
            if let Some(m) = decode_artist(state, jf_id).await {
                mbids.push(m);
            }
        }
        let mut tracks: Vec<_> = state
            .library
            .tracks(user_id)
            .await
            .into_iter()
            .filter(|t| t.artist_mbid.as_ref().is_some_and(|m| mbids.contains(m)))
            .collect();
        if let Some(key) = sort_key {
            sort_tracks(&mut tracks, key, sort_desc);
        }
        let mut built = Vec::with_capacity(tracks.len());
        for t in &tracks {
            built.push(b.audio(t).await);
        }
        let (items, total) = page(&built, start, limit);
        return json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items,
                total_record_count: total,
                start_index: start,
            },
        );
    }
    let mut tracks = state.library.tracks(user_id).await;
    if let Some(needle) = search {
        tracks.retain(|t| {
            matches(
                &[
                    Some(t.title.clone()),
                    t.artist_name.clone(),
                    t.album_title.clone(),
                ],
                needle,
            )
        });
    }
    if let Some(key) = sort_key.filter(|k| k.is_history()) {
        // History sorts page from play history: unplayed tracks are excluded.
        if key == SortKey::DatePlayed {
            tracks.retain(|t| t.last_played.is_some());
        } else {
            tracks.retain(|t| t.play_count > 0);
        }
        sort_tracks(&mut tracks, key, sort_desc);
    } else if let Some(key) = sort_key {
        sort_tracks(&mut tracks, key, sort_desc);
    }
    let total = tracks.len();
    let items = tracks.get(start..(start + size).min(total)).unwrap_or(&[]);
    let mut built = Vec::with_capacity(items.len());
    for t in items {
        built.push(b.audio(t).await);
    }
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items: built,
            total_record_count: total,
            start_index: start,
        },
    )
}

/// Album browse arm (v2 `_browse` fallthrough branch).
#[allow(clippy::too_many_arguments)]
pub(super) async fn album_browse<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    _q: &CiParams,
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
    if !contributing_ids.is_empty() {
        let mut mbids = Vec::new();
        for jf_id in contributing_ids {
            if let Some(m) = decode_artist(state, jf_id).await {
                mbids.push(m);
            }
        }
        if mbids.is_empty() {
            // A contributor filter must never fall through to the full catalog.
            return json(
                StatusCode::OK,
                &BaseItemDtoQueryResult {
                    items: Vec::new(),
                    total_record_count: 0,
                    start_index: start,
                },
            );
        }
        // Appears-on: albums whose artist is not the contributor but whose
        // tracks credit them (in-memory approximation of the discover call).
        let tracks = state.library.tracks(user_id).await;
        let mut appears_on: Vec<String> = tracks
            .iter()
            .filter(|t| {
                t.artist_mbid.as_ref().is_some_and(|m| mbids.contains(m))
                    && t.album_artist_mbid
                        .as_ref()
                        .is_none_or(|m| !mbids.contains(m))
            })
            .filter_map(|t| t.rg_mbid.clone())
            .collect();
        appears_on.sort();
        appears_on.dedup();
        let mut albums = Vec::new();
        for rg in &appears_on {
            if let Some(a) = state.library.album(user_id, rg).await {
                albums.push(a);
            }
        }
        match sort_key {
            Some(key) => sort_albums(&mut albums, key, sort_desc),
            None => sort_albums(&mut albums, SortKey::Recent, true),
        }
        let mut built = Vec::with_capacity(albums.len());
        for a in &albums {
            built.push(b.album(a).await);
        }
        let (items, total) = page(&built, start, limit);
        return json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items,
                total_record_count: total,
                start_index: start,
            },
        );
    }
    if !album_artist_ids.is_empty() || !artist_ids.is_empty() {
        let mut albums = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let all = state.library.albums(user_id).await;
        for jf_id in album_artist_ids.iter().chain(artist_ids.iter()) {
            let Some(mb) = decode_artist(state, jf_id).await else {
                continue;
            };
            for a in &all {
                if a.artist_mbid.as_deref() == Some(mb.as_str()) && seen.insert(a.rg_mbid.clone()) {
                    albums.push(a.clone());
                }
            }
        }
        if let Some(key) = sort_key {
            sort_albums(&mut albums, key, sort_desc);
        }
        let mut built = Vec::with_capacity(albums.len());
        for a in &albums {
            built.push(b.album(a).await);
        }
        let (items, total) = page(&built, start, limit);
        return json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items,
                total_record_count: total,
                start_index: start,
            },
        );
    }
    let mut albums = state.library.albums(user_id).await;
    if let Some(needle) = search {
        albums.retain(|a| matches(&[Some(a.title.clone()), a.artist_name.clone()], needle));
    }
    if let Some(key) = sort_key.filter(|k| k.is_history()) {
        if key == SortKey::DatePlayed {
            albums.retain(|a| a.last_played.is_some());
        } else {
            albums.retain(|a| a.play_count > 0);
        }
        sort_albums(&mut albums, key, sort_desc);
        let total = albums.len();
        let items = albums.get(start..(start + size).min(total)).unwrap_or(&[]);
        let mut built = Vec::with_capacity(items.len());
        for a in items {
            built.push(b.album(a).await);
        }
        return json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items: built,
                total_record_count: total,
                start_index: start,
            },
        );
    }
    if let Some(key) = sort_key {
        sort_albums(&mut albums, key, sort_desc);
        let total = albums.len();
        let items = albums.get(start..(start + size).min(total)).unwrap_or(&[]);
        let mut built = Vec::with_capacity(items.len());
        for a in items {
            built.push(b.album(a).await);
        }
        return json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items: built,
                total_record_count: total,
                start_index: start,
            },
        );
    }
    // Legacy order: v2's `page = start // limit + 1` service paging.
    let page_no = if limit == 0 { 1 } else { start / limit + 1 };
    let offset = (page_no - 1) * size;
    let total = albums.len();
    let items = albums
        .get(offset..(offset + size).min(total))
        .unwrap_or(&[]);
    let mut built = Vec::with_capacity(items.len());
    for a in items {
        built.push(b.album(a).await);
    }
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items: built,
            total_record_count: total,
            start_index: start,
        },
    )
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
            _ => return json(StatusCode::OK, &Vec::<super::models::BaseItemDto>::new()),
        }
    }
    let mut limit = params::qint(&q, "Limit", 10);
    if limit <= 0 {
        limit = 10;
    }
    let b = builder(&state);
    let mut albums = state.library.albums(&authed.principal.id).await;
    sort_albums(&mut albums, SortKey::Recent, true);
    let mut built = Vec::new();
    for a in albums.iter().take(limit as usize) {
        built.push(b.album(a).await);
    }
    json(StatusCode::OK, &built)
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
    let mut artists = state.library.artists(&authed.principal.id, scope).await;
    if let Some(needle) = q.get("SearchTerm") {
        artists.retain(|a| matches(&[Some(a.name.clone())], needle));
    }
    let total = artists.len();
    let size = if limit == 0 { 100_000 } else { limit };
    let items = artists.get(start..(start + size).min(total)).unwrap_or(&[]);
    let b = builder(&state);
    let mut built = Vec::with_capacity(items.len());
    for a in items {
        built.push(b.artist(a).await);
    }
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items: built,
            total_record_count: total,
            start_index: start,
        },
    )
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
    let b = builder(&state);
    let mut built = Vec::with_capacity(genres.len());
    for g in &genres {
        built.push(b.genre(g).await);
    }
    let (items, total) = page(&built, start, limit);
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items,
            total_record_count: total,
            start_index: start,
        },
    )
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
        let tracks: Vec<_> = state
            .library
            .tracks(&ctx.principal.id)
            .await
            .into_iter()
            .filter(|t| t.artist_mbid.as_deref() == Some(artist_mbid.as_str()))
            .take(limit)
            .collect();
        let b = builder(&state);
        let mut built = Vec::with_capacity(tracks.len());
        for t in &tracks {
            built.push(b.audio(t).await);
        }
        let total = built.len();
        json(
            StatusCode::OK,
            &BaseItemDtoQueryResult {
                items: built,
                total_record_count: total,
                start_index: 0,
            },
        )
    })
}
