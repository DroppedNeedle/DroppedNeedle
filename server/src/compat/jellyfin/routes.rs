//! Axum handlers and route registration, ported from v2
//! `api/compat/jellyfin/router.py`.
//!
//! Wire contract: PascalCase JSON, real HTTP statuses, empty error bodies
//! (NOT the native envelope). Streaming is anonymous: real Jellyfin audio
//! routes have no `[Authorize]`, and native players (Jellify, Finamp, Manet)
//! fetch with no auth header. Still gated by protocol-enabled + a valid
//! opaque item id (v2 `_handle(auth=False)`: players fetch audio URLs
//! without headers).
//!
//! Route notes: `/Items/Filters` must stay registered
//! alongside (before, for clarity) `/Items/{item_id}` or "Filters" would be
//! captured as an id (v2 comment; axum prefers the static route). There is
//! no `/jellyfin/socket` endpoint, on purpose (v2 gap: uvicorn 403s the
//! websocket scope; Finamp PlayOn only, non-blocking).

use crate::auth::compat_auth::jellyfin::{
    JellyfinPasswordStore, JellyfinRequest, SessionFacts, authenticate_by_name, extract_client,
    extract_device, extract_token, login_echo_json, resolve_token, server_id,
};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use uuid::Uuid;

use super::builders::{self, Builder, LIBRARY_INTERNAL_ID};
use super::models::{
    AuthenticateRequest, BaseItemDtoQueryResult, CreatePlaylistDto, MediaSourceInfo,
    PlaybackInfoBody, PlaybackInfoResponse, PlaybackProgressInfo, PlaybackStartInfo,
    PlaybackStopInfo, PublicSystemInfo, SystemInfo, UserItemDataDto,
};
use super::params::{self, CiParams, SortKey};
use super::seams::{
    ArtistScope, ByteOutcome, DecideInput, IdMap, JellyfinSettings, LibraryRead, PlaybackSessions,
    Principal, StreamEngine, StreamPlan, TICKS_PER_SECOND, decide, should_scrobble,
};

// ===== State + registration =====

/// Router dependencies. Every seam is a generic so production binds the
/// real adapters without touching the routes: `S` app-password store
/// (from `compat_auth`), `L` library reads, `E` stream engine, `P` scrobble
/// adapter, `I` persisted id map.
#[derive(Clone)]
pub struct JellyfinState<S, L, E, P, I> {
    /// App-password store (`compat_auth`'s `JellyfinPasswordStore`).
    pub passwords: S,
    /// Library reads.
    pub library: L,
    /// Streaming engine.
    pub engine: E,
    /// Playback sessions (scrobble adapter binding).
    pub sessions: P,
    /// Opaque id map.
    pub ids: I,
    /// Connect-apps settings subset.
    pub settings: JellyfinSettings,
    /// Deployment prefix for `TranscodingUrl` (`""` standalone; real base
    /// path when wired, so players stay inside the prefix).
    pub base_path: String,
    /// Restart-stable server id, computed once (`compat_auth` owns the value).
    pub server_id: String,
}

impl<S, L, E, P, I> JellyfinState<S, L, E, P, I> {
    /// Assemble state; `server_id()` is restart-stable.
    pub fn new(
        passwords: S,
        library: L,
        engine: E,
        sessions: P,
        ids: I,
        settings: JellyfinSettings,
    ) -> Self {
        Self {
            passwords,
            library,
            engine,
            sessions,
            ids,
            settings,
            base_path: String::new(),
            server_id: server_id(),
        }
    }
}

/// Build the `/jellyfin`-prefixed router.
pub fn router<S, L, E, P, I>(state: JellyfinState<S, L, E, P, I>) -> Router
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    Router::new()
        .route(
            "/System/Info/Public",
            get(system_info_public::<S, L, E, P, I>),
        )
        .route("/System/Info", get(system_info::<S, L, E, P, I>))
        .route("/QuickConnect/Enabled", get(quick_connect::<S, L, E, P, I>))
        .route("/Sessions/Logout", post(sessions_logout::<S, L, E, P, I>))
        .route(
            "/Users/AuthenticateByName",
            post(authenticate::<S, L, E, P, I>),
        )
        .route("/Users/Me", get(users_me::<S, L, E, P, I>))
        .route("/Users/{user_id}", get(user_by_id::<S, L, E, P, I>))
        .route("/Users/{user_id}/Views", get(views::<S, L, E, P, I>))
        .route("/UserViews", get(views::<S, L, E, P, I>))
        .route("/Users/{user_id}/Items", get(browse::<S, L, E, P, I>))
        .route("/Items", get(browse::<S, L, E, P, I>))
        .route("/UserItems/Latest", get(latest::<S, L, E, P, I>))
        .route(
            "/Users/{user_id}/Items/Latest",
            get(latest::<S, L, E, P, I>),
        )
        .route("/Artists", get(artists::<S, L, E, P, I>))
        .route("/Artists/AlbumArtists", get(album_artists::<S, L, E, P, I>))
        .route("/Genres", get(genres::<S, L, E, P, I>))
        .route("/MusicGenres", get(genres::<S, L, E, P, I>))
        // Filters before `/{item_id}`: "Filters" must never be an id.
        .route("/Items/Filters", get(filters::<S, L, E, P, I>))
        .route(
            "/Users/{user_id}/Items/Filters",
            get(filters::<S, L, E, P, I>),
        )
        .route(
            "/Users/{user_id}/Items/{item_id}",
            get(single_item::<S, L, E, P, I>),
        )
        .route("/Items/{item_id}", get(single_item::<S, L, E, P, I>))
        .route(
            "/Items/{item_id}/Images/{image_type}",
            get(image::<S, L, E, P, I>),
        )
        .route(
            "/Items/{item_id}/Images/{image_type}/{index}",
            get(image_indexed::<S, L, E, P, I>),
        )
        .route(
            "/Audio/{item_id}/{tail}",
            get(audio::<S, L, E, P, I>).head(audio_head::<S, L, E, P, I>),
        )
        .route(
            "/Items/{item_id}/PlaybackInfo",
            get(playback_info::<S, L, E, P, I>).post(playback_info::<S, L, E, P, I>),
        )
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}",
            post(favorite_add_legacy::<S, L, E, P, I>)
                .delete(favorite_remove_legacy::<S, L, E, P, I>),
        )
        .route(
            "/UserFavoriteItems/{item_id}",
            post(favorite_add_modern::<S, L, E, P, I>)
                .delete(favorite_remove_modern::<S, L, E, P, I>),
        )
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(played_add_legacy::<S, L, E, P, I>).delete(played_remove_legacy::<S, L, E, P, I>),
        )
        .route(
            "/UserPlayedItems/{item_id}",
            post(played_add_modern::<S, L, E, P, I>).delete(played_remove_modern::<S, L, E, P, I>),
        )
        .route("/Sessions/Playing", post(playing::<S, L, E, P, I>))
        .route(
            "/Sessions/Playing/Progress",
            post(playing_progress::<S, L, E, P, I>),
        )
        .route(
            "/Sessions/Playing/Stopped",
            post(playing_stopped::<S, L, E, P, I>),
        )
        .route(
            "/Sessions/Playing/Ping",
            post(sessions_ping::<S, L, E, P, I>),
        )
        .route(
            "/Sessions/Capabilities/Full",
            post(sessions_capabilities::<S, L, E, P, I>),
        )
        .route("/Playlists", post(create_playlist::<S, L, E, P, I>))
        .route(
            "/Playlists/{playlist_id}",
            get(get_playlist::<S, L, E, P, I>),
        )
        .route(
            "/Playlists/{playlist_id}/Items",
            get(playlist_items::<S, L, E, P, I>)
                .post(playlist_add::<S, L, E, P, I>)
                .delete(playlist_remove::<S, L, E, P, I>),
        )
        .route(
            "/Playlists/{playlist_id}/Items/{entry_id}/Move/{new_index}",
            post(playlist_move::<S, L, E, P, I>),
        )
        .route("/Items/{item_id}/Similar", get(similar::<S, L, E, P, I>))
        .route("/Items/{item_id}/InstantMix", get(similar::<S, L, E, P, I>))
        .route(
            "/Artists/{item_id}/InstantMix",
            get(similar::<S, L, E, P, I>),
        )
        .with_state(state)
}

// ===== Response helpers =====

/// Empty-body error: v2 `JellyfinError(status, message)` carries no body by
/// default, and every mapped exception renders status-only.
fn error(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .body(Body::empty())
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap_or_default()
        })
}

fn json<T: serde::Serialize>(status: StatusCode, payload: &T) -> Response {
    match serde_json::to_vec(payload) {
        Ok(body) => Response::builder()
            .status(status)
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR)),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn raw_json(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

fn no_content() -> Response {
    error(StatusCode::NO_CONTENT)
}

/// Map an engine byte outcome to a response (200/206 bytes, 416 range-only,
/// 404/429 status-only). Outcomes without a `Content-Length` carry a
/// non-empty body only for transcodes (estimate off on Jellyfin), and those
/// must stream unsized: axum stamps a `Content-Length` on sized bodies, so a
/// sized transcode response would violate the v2 wire contract. Empty bodies
/// stay sized (`Content-Length: 0`, the v2 416 shape).
fn outcome_response(outcome: &ByteOutcome) -> Response {
    let status = StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if outcome.status == StatusCode::NOT_FOUND.as_u16() {
        return error(StatusCode::NOT_FOUND);
    }
    let mut builder = Response::builder().status(status);
    let mut sized = outcome.body.is_empty();
    for (name, value) in &outcome.headers {
        if name.eq_ignore_ascii_case("content-length") {
            sized = true;
        }
        builder = builder.header(name.as_str(), value.as_str());
    }
    let body = if sized {
        Body::from(outcome.body.clone())
    } else {
        let bytes = outcome.body.clone();
        Body::from_stream(futures_util::stream::once(async move {
            Ok::<_, std::convert::Infallible>(bytes)
        }))
    };
    builder
        .body(body)
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

// ===== Auth + gates =====

/// Authenticated request context: principal plus the media-browser facts.
struct Authed {
    principal: Principal,
    client: Option<String>,
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Exact-spelling raw-query lookup for the two auth keys (v2 `auth.py`
/// reads Starlette's case-sensitive params: only `ApiKey` and `api_key`).
fn query_exact<'a>(raw: Option<&'a str>, key: &str) -> Option<&'a str> {
    for pair in raw.unwrap_or("").split('&') {
        let (name, value) = match pair.find('=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, ""),
        };
        if name == key {
            return Some(value);
        }
    }
    None
}

/// Resolve the caller or return the 401 (empty body, never raises).
async fn authed<S: JellyfinPasswordStore>(
    passwords: &S,
    headers: &HeaderMap,
    raw_query: Option<&str>,
) -> Result<Authed, Response> {
    let request = JellyfinRequest {
        authorization: header(headers, "authorization"),
        emby_authorization: header(headers, "x-emby-authorization"),
        emby_token: header(headers, "x-emby-token"),
        mediabrowser_token: header(headers, "x-mediabrowser-token"),
        query_apikey: query_exact(raw_query, "ApiKey"),
        query_api_key: query_exact(raw_query, "api_key"),
    };
    let token = extract_token(&request);
    let client = extract_client(&request);
    match resolve_token(passwords, token.as_deref()).await {
        Ok(user) => Ok(Authed {
            principal: Principal::from_user(&user, token.as_deref().unwrap_or("")),
            client,
        }),
        Err(_) => Err(error(StatusCode::UNAUTHORIZED)),
    }
}

/// The kill-switch gate: disabled → 404 before any handler lookup.
fn gate<S, L, E, P, I>(state: &JellyfinState<S, L, E, P, I>) -> Option<Response> {
    if state.settings.enabled {
        None
    } else {
        Some(error(StatusCode::NOT_FOUND))
    }
}

/// Advertised origin plus shim prefix (v2 `_local_address`).
fn local_address<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    headers: &HeaderMap,
) -> String {
    let host = header(headers, "host").unwrap_or("localhost");
    format!("http://{host}{}/jellyfin", state.base_path)
}

fn builder<'a, S, L, E, P, I>(state: &'a JellyfinState<S, L, E, P, I>) -> Builder<'a, I, L>
where
    I: IdMap,
    L: LibraryRead,
{
    Builder::new(&state.ids, &state.library, &state.server_id)
}

// ===== System / identity =====

async fn system_info_public<S, L, E, P, I>(
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
    json(
        StatusCode::OK,
        &PublicSystemInfo {
            local_address: local_address(&state, request.headers()),
            server_name: state.settings.server_name.clone(),
            version: state.settings.server_version.clone(),
            product_name: "Jellyfin Server".to_owned(),
            operating_system: String::new(),
            id: server_id(),
            startup_wizard_completed: true,
        },
    )
}

async fn system_info<S, L, E, P, I>(
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
    if authed(&state.passwords, request.headers(), query.as_deref())
        .await
        .is_err()
    {
        return error(StatusCode::UNAUTHORIZED);
    }
    json(
        StatusCode::OK,
        &SystemInfo {
            local_address: local_address(&state, request.headers()),
            server_name: state.settings.server_name.clone(),
            version: state.settings.server_version.clone(),
            product_name: "Jellyfin Server".to_owned(),
            operating_system: String::new(),
            id: server_id(),
            startup_wizard_completed: true,
            has_pending_restart: false,
            is_shutting_down: false,
            supports_library_monitor: true,
        },
    )
}

/// QuickConnect is unsupported: the literal `false` (v2 `_handle` lambda).
async fn quick_connect<S, L, E, P, I>(State(state): State<JellyfinState<S, L, E, P, I>>) -> Response
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
    raw_json(StatusCode::OK, "false")
}

async fn sessions_logout<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
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
    no_content()
}

// ===== Auth / user =====

/// `AuthenticateByName`: the app password echoes verbatim as `AccessToken`
/// with a fresh `SessionInfo` and the full non-null user object
/// (`compat_auth` renders it; bad credentials → 401, never 403).
async fn authenticate<S, L, E, P, I>(
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
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return error(StatusCode::BAD_REQUEST),
    };
    if bytes.is_empty() {
        return error(StatusCode::BAD_REQUEST);
    }
    let login: AuthenticateRequest = match serde_json::from_slice(&bytes) {
        Ok(login) => login,
        Err(_) => return error(StatusCode::BAD_REQUEST),
    };
    let probe = JellyfinRequest {
        authorization: header(&parts.headers, "authorization"),
        emby_authorization: header(&parts.headers, "x-emby-authorization"),
        emby_token: None,
        mediabrowser_token: None,
        query_apikey: None,
        query_api_key: None,
    };
    let client = extract_client(&probe);
    let (device_name, device_id) = extract_device(&probe);
    let user = match authenticate_by_name(
        &state.passwords,
        &login.username,
        &login.pw,
        client.as_deref(),
    )
    .await
    {
        Ok(user) => user,
        Err(_) => return error(StatusCode::UNAUTHORIZED),
    };
    let facts = SessionFacts {
        id: Uuid::new_v4().simple().to_string(),
        client,
        device_name,
        device_id,
        last_activity: builders::utc_now_iso(),
    };
    raw_json(StatusCode::OK, &login_echo_json(&user, &login.pw, &facts))
}

async fn users_me<S, L, E, P, I>(
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
    match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => json(StatusCode::OK, &authed.principal.user_dto(&server_id())),
        Err(denied) => denied,
    }
}

/// The `{user_id}` path id is ignored; the caller is always returned (v2
/// `_user_dto(u)` parity).
async fn user_by_id<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(_user_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    users_me(State(state), request).await
}

// ===== Library browsing =====

/// The single "Music" view, both dialects (v2 `_views`).
async fn views<S, L, E, P, I>(
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

/// `IncludeItemTypes` priority: MusicArtist > MusicAlbum > Audio > Playlist >
/// MusicGenre, default album (v2 `_primary_type`). Type names are
/// case-sensitive, like v2's set membership.
fn primary_type(types: &[String]) -> &'static str {
    for candidate in [
        "MusicArtist",
        "MusicAlbum",
        "Audio",
        "Playlist",
        "MusicGenre",
    ] {
        if types.iter().any(|t| t == candidate) {
            return candidate;
        }
    }
    "MusicAlbum"
}

/// Page a built list: `limit == 0` means ALL from start (v2 `_build_page`).
fn page<T: Clone>(items: &[T], start: usize, limit: usize) -> (Vec<T>, usize) {
    let total = items.len();
    let page = if start >= total {
        Vec::new()
    } else if limit == 0 {
        items[start..].to_vec()
    } else {
        items[start..(start.saturating_add(limit)).min(total)].to_vec()
    };
    (page, total)
}

fn matches(haystacks: &[Option<String>], needle: &str) -> bool {
    let needle = needle.to_lowercase();
    haystacks
        .iter()
        .flatten()
        .any(|h| h.to_lowercase().contains(&needle))
}

fn flip(desc: bool, ord: std::cmp::Ordering) -> std::cmp::Ordering {
    if desc { ord.reverse() } else { ord }
}

fn sort_tracks(tracks: &mut [super::seams::TrackView], key: SortKey, desc: bool) {
    tracks.sort_by(|a, b| {
        let ord = match key {
            SortKey::Recent => a
                .created_at
                .partial_cmp(&b.created_at)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            SortKey::Year | SortKey::PremiereDate => a.year.cmp(&b.year),
            SortKey::Random => fnv(&a.file_id).cmp(&fnv(&b.file_id)),
            SortKey::DatePlayed => a
                .last_played
                .partial_cmp(&b.last_played)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::PlayCount => a.play_count.cmp(&b.play_count),
        };
        flip(desc, ord.then_with(|| a.file_id.cmp(&b.file_id)))
    });
}

fn sort_albums(albums: &mut [super::seams::AlbumView], key: SortKey, desc: bool) {
    albums.sort_by(|a, b| {
        let ord = match key {
            SortKey::Recent => a
                .date_added
                .partial_cmp(&b.date_added)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            SortKey::Year | SortKey::PremiereDate => a.year.cmp(&b.year),
            SortKey::Random => fnv(&a.rg_mbid).cmp(&fnv(&b.rg_mbid)),
            SortKey::DatePlayed => a
                .last_played
                .partial_cmp(&b.last_played)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::PlayCount => a.play_count.cmp(&b.play_count),
        };
        flip(desc, ord.then_with(|| a.rg_mbid.cmp(&b.rg_mbid)))
    });
}

/// Stable stand-in shuffle for `SortBy=Random` (the real discover ordering
/// is not bound yet; tests only pin stability + completeness).
fn fnv(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Decode a Jellyfin id to an artist mbid, `None` when undecodable or not an
/// artist (v2 `_decode_artist`).
async fn decode_artist<S, L, E, P, I>(
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
async fn fetch_item<S, L, E, P, I>(
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
async fn browse<S, L, E, P, I>(
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
async fn favorites_browse<S, L, E, P, I>(
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
async fn audio_browse<S, L, E, P, I>(
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
async fn album_browse<S, L, E, P, I>(
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
        // Appears-on: albums whose artist is NOT the contributor but whose
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
async fn latest<S, L, E, P, I>(
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

async fn artists<S, L, E, P, I>(
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

async fn album_artists<S, L, E, P, I>(
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

async fn artists_scoped<S, L, E, P, I>(
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
async fn genres<S, L, E, P, I>(
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
async fn filters<S, L, E, P, I>(
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

async fn single_item<S, L, E, P, I>(
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

// ===== Images (anonymous) =====

/// 1x1 opaque PNG for the library view's advertised `ImageTags.Primary`, so
/// the request resolves instead of 404ing (v2 `_LIBRARY_COVER_PNG`).
const LIBRARY_COVER_PNG_B64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAAC0lEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC";

fn library_png() -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(LIBRARY_COVER_PNG_B64)
        .unwrap_or_default()
}

/// Size bucket from the first present width/height-ish param (v2
/// `_image_size`).
fn image_size(q: &CiParams) -> &'static str {
    for key in [
        "fillWidth",
        "maxWidth",
        "width",
        "fillHeight",
        "maxHeight",
        "height",
    ] {
        if let Some(raw) = q.get(key)
            && !raw.is_empty()
            && raw.bytes().all(|b| b.is_ascii_digit())
            && let Ok(px) = raw.parse::<u32>()
        {
            return if px <= 300 {
                "250"
            } else if px <= 750 {
                "500"
            } else {
                "1200"
            };
        }
    }
    "500"
}

async fn image<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((item_id, image_type)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let query = request.uri().query().map(str::to_owned);
    serve_image(&state, query.as_deref(), &item_id, &image_type).await
}

/// Indexed variant: the index is accepted and ignored (v2 takes an int and
/// never reads it either).
async fn image_indexed<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((item_id, image_type, _index)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let query = request.uri().query().map(str::to_owned);
    serve_image(&state, query.as_deref(), &item_id, &image_type).await
}

async fn serve_image<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    query: Option<&str>,
    item_id: &str,
    image_type: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(state) {
        return denied;
    }
    if !image_type.eq_ignore_ascii_case("primary") {
        return error(StatusCode::NOT_FOUND);
    }
    let Some((kind, internal)) = state.ids.from_jf(item_id).await else {
        return error(StatusCode::NOT_FOUND);
    };
    let q = CiParams::parse(query);
    let size = image_size(&q);
    let found = match kind.as_str() {
        "library" => Some(super::seams::CoverBytes {
            bytes: library_png(),
            content_type: "image/png".to_owned(),
        }),
        "album" => state.library.cover(&internal, size).await,
        "track" => match state.library.track("", &internal).await {
            Some(track) => match track.rg_mbid.as_deref() {
                Some(rg) => state.library.cover(rg, size).await,
                None => None,
            },
            None => None,
        },
        "artist" => state.library.artist_image(&internal).await,
        _ => None,
    };
    match found {
        Some(cover) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", cover.content_type)
            .header("Cache-Control", "public, max-age=31536000, immutable")
            .body(Body::from(cover.bytes))
            .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR)),
        None => error(StatusCode::NOT_FOUND),
    }
}

// ===== Streaming + PlaybackInfo =====

/// Accepted `Container` values: comma-separated, pipe-variants split with
/// the first token winning (v2 `_accepted_containers`).
fn accepted_containers(param: Option<&str>) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for entry in param.unwrap_or("").split(',') {
        let entry = entry.trim();
        if !entry.is_empty() {
            out.insert(entry.split('|').next().unwrap_or("").trim().to_lowercase());
        }
    }
    out
}

/// Client codec → what we can produce: `mp3`/`opus` pass through, anything
/// else coerces to `opus`, "nearest we can produce" (v2 `_map_jf_codec`).
fn map_codec(codec: Option<&str>) -> Option<String> {
    let codec = codec.unwrap_or("");
    if codec.is_empty() {
        return None;
    }
    let lower = codec.to_lowercase();
    Some(if lower == "mp3" || lower == "opus" {
        lower
    } else {
        "opus".to_owned()
    })
}

/// Decode an audio item id to its file id (v2 `_decode_track`).
async fn decode_track<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    item_id: &str,
) -> Result<String, Response>
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    match state.ids.from_jf(item_id).await {
        Some((kind, internal)) if kind == "track" => Ok(internal),
        _ => Err(error(StatusCode::NOT_FOUND)),
    }
}

/// Run the policy and serve direct or transcoded bytes (v2
/// `_stream_decided`, minus the plugin fallback: no plugin seam exists
/// here yet, so a local miss is a plain 404.
async fn stream_decided<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    range: Option<&str>,
    internal: &str,
    req_format: Option<String>,
    max_kbps: Option<u32>,
    start_seconds: f64,
    force: bool,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let Some(track) = state.library.track("", internal).await else {
        return error(StatusCode::NOT_FOUND);
    };
    let plan = decide(&DecideInput {
        src_format: track.file_format.as_deref(),
        src_bitrate_kbps: track.bitrate.unwrap_or(0),
        requested: req_format.as_deref(),
        ceiling_kbps: max_kbps,
        force_original: force,
        start_seconds,
        transcoding_enabled: state.settings.transcoding_enabled,
        server_max_kbps: state.settings.transcode_max_bitrate_kbps,
        default_format: &state.settings.transcode_default_format,
        ffmpeg: state.settings.ffmpeg_available,
    });
    match plan {
        StreamPlan::Direct => outcome_response(&state.engine.direct(internal, range).await),
        StreamPlan::Transcode {
            format,
            bitrate_kbps,
            start_seconds,
        } => outcome_response(
            &state
                .engine
                .transcode(internal, &format, bitrate_kbps, start_seconds)
                .await,
        ),
    }
}

async fn audio<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((item_id, tail)): Path<(String, String)>,
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
    // Owned inputs only past this point: `&Request<Body>` is not `Send`, so
    // holding it across an await would break the `Handler` impl.
    let headers = request.headers().clone();
    let query = request.uri().query().map(str::to_owned);
    if tail == "universal" {
        return universal(&state, &headers, query.as_deref(), &item_id).await;
    }
    if tail == "stream" || tail.starts_with("stream.") {
        return audio_stream(&state, &headers, query.as_deref(), &item_id).await;
    }
    error(StatusCode::NOT_FOUND)
}

/// `HEAD /Audio/...`: the same status and headers GET would answer
/// (200/206/416; HEAD honors Range), always with an empty body, no
/// lease (v2 `_audio_stream_head`).
async fn audio_head<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((item_id, tail)): Path<(String, String)>,
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
    // Unknown tails 404 exactly like GET (no `universal`/`stream` probe
    // that answers 200 for a route GET would refuse).
    if tail != "universal" && tail != "stream" && !tail.starts_with("stream.") {
        return error(StatusCode::NOT_FOUND);
    }
    let headers = request.headers().clone();
    let range = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let internal = match decode_track(&state, &item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let outcome = state.engine.head(&internal, range.as_deref()).await;
    if outcome.status == StatusCode::NOT_FOUND.as_u16() {
        return error(StatusCode::NOT_FOUND);
    }
    let status = StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    for (name, value) in &outcome.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder
        .body(Body::empty())
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

/// `/universal` container negotiation (v2 `_universal`).
async fn universal<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    headers: &HeaderMap,
    query: Option<&str>,
    item_id: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query);
    let max_bps = params::qint(&q, "MaxStreamingBitrate", 0);
    let max_kbps = (max_bps > 0).then(|| (max_bps as f64 / 1000.0).round() as u32);
    let start_seconds = params::qint(&q, "StartTimeTicks", 0) as f64 / TICKS_PER_SECOND as f64;
    let accepted = accepted_containers(q.get("Container"));
    let track_format = state
        .library
        .track("", &internal)
        .await
        .and_then(|t| t.file_format);
    let req_format = if track_format
        .as_deref()
        .is_some_and(|f| accepted.contains(&f.to_lowercase()))
    {
        None
    } else {
        map_codec(q.get("AudioCodec"))
    };
    let range = header(headers, "range").map(str::to_owned);
    stream_decided(
        state,
        range.as_deref(),
        &internal,
        req_format,
        max_kbps,
        start_seconds,
        false,
    )
    .await
}

/// `/stream[.ext]`: `static=true` (case-insensitive) forces direct (v2
/// `_audio_stream`).
async fn audio_stream<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    headers: &HeaderMap,
    query: Option<&str>,
    item_id: &str,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let internal = match decode_track(state, item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query);
    if q.get("static").unwrap_or("").eq_ignore_ascii_case("true") {
        let range = header(headers, "range");
        return outcome_response(&state.engine.direct(&internal, range).await);
    }
    let audio_bps = params::qint(&q, "audioBitRate", 0);
    let max_kbps = (audio_bps > 0).then(|| (audio_bps as f64 / 1000.0).round() as u32);
    let start_seconds = params::qint(&q, "startTimeTicks", 0) as f64 / TICKS_PER_SECOND as f64;
    let range = header(headers, "range").map(str::to_owned);
    stream_decided(
        state,
        range.as_deref(),
        &internal,
        map_codec(q.get("audioCodec")),
        max_kbps,
        start_seconds,
        false,
    )
    .await
}

/// GET+POST PlaybackInfo (v2 `_playback_info`). `DirectStreamUrl` embeds
/// `?api_key=<token>` for headerless players; transcoding fields appear only
/// when the policy says transcode; Finamp's 15 non-null fields are always
/// present (MediaStream 5 + MediaSourceInfo 10, v2 issue #438).
async fn playback_info<S, L, E, P, I>(
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
    let is_post = request.method() == axum::http::Method::POST;
    let (parts, body) = request.into_parts();
    let authed = match authed(&state.passwords, &parts.headers, raw_query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let internal = match decode_track(&state, &item_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let Some(track) = state.library.track(&authed.principal.id, &internal).await else {
        return error(StatusCode::NOT_FOUND);
    };
    let mut max_bps: Option<i64> = None;
    if is_post
        && let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024).await
        && !bytes.is_empty()
        && let Ok(parsed) = serde_json::from_slice::<PlaybackInfoBody>(&bytes)
        && parsed.max_streaming_bitrate.unwrap_or(0) != 0
    {
        max_bps = parsed.max_streaming_bitrate;
    }
    if max_bps.is_none() {
        let q = CiParams::parse(raw_query.as_deref());
        if let Some(raw) = q.get("maxStreamingBitrate")
            && !raw.is_empty()
            && raw.bytes().all(|b| b.is_ascii_digit())
            && let Ok(parsed) = raw.parse::<i64>()
        {
            max_bps = Some(parsed);
        }
    }
    let max_kbps = max_bps
        .filter(|b| *b > 0)
        .map(|b| (b as f64 / 1000.0).round() as u32);
    let will_transcode = matches!(
        decide(&DecideInput {
            src_format: track.file_format.as_deref(),
            src_bitrate_kbps: track.bitrate.unwrap_or(0),
            requested: None,
            ceiling_kbps: max_kbps,
            force_original: false,
            start_seconds: 0.0,
            transcoding_enabled: state.settings.transcoding_enabled,
            server_max_kbps: state.settings.transcode_max_bitrate_kbps,
            default_format: &state.settings.transcode_default_format,
            ffmpeg: state.settings.ffmpeg_available,
        }),
        StreamPlan::Transcode { .. }
    );
    let psid = Uuid::new_v4().simple().to_string();
    let ext = track
        .file_format
        .clone()
        .unwrap_or_else(|| "dat".to_owned());
    let direct_url = format!(
        "{}/Audio/{item_id}/stream.{ext}?static=true&mediaSourceId={item_id}&api_key={}",
        local_address(&state, &parts.headers),
        authed.principal.token,
    );
    let mut src = MediaSourceInfo {
        id: item_id.clone(),
        protocol: "File".to_owned(),
        container: track.file_format.clone(),
        size: track.file_size_bytes,
        bitrate: track
            .bitrate
            .map(|b| u64::from(b) * 1000)
            .filter(|b| *b != 0),
        run_time_ticks: builders::ticks(track.duration_seconds),
        supports_direct_play: true,
        supports_direct_stream: true,
        supports_transcoding: state.settings.transcoding_enabled && state.settings.ffmpeg_available,
        default_audio_stream_index: 0,
        media_streams: vec![builders::media_stream(&track)],
        name: None,
        is_remote: false,
        direct_stream_url: Some(direct_url),
        transcoding_url: None,
        transcoding_sub_protocol: None,
        transcoding_container: None,
        source_type: "Default".to_owned(),
        is_infinite_stream: false,
        requires_opening: false,
        requires_closing: false,
        requires_looping: false,
        supports_probing: false,
        read_at_native_framerate: false,
        ignore_dts: false,
        ignore_index: false,
        gen_pts_input: false,
    };
    if will_transcode {
        let out = state.settings.transcode_default_format.clone();
        // Root-relative yet inside the deployment prefix: players resolve
        // against the advertised origin, so a bare path would escape the
        // base path under non-empty BASE_PATH deployments (v2 `_playback_info`).
        src.transcoding_url = Some(format!(
            "{}/jellyfin/Audio/{item_id}/universal?AudioCodec={out}&Container={out}&PlaySessionId={psid}",
            state.base_path,
        ));
        src.transcoding_sub_protocol = Some("http".to_owned());
        src.transcoding_container = Some(if out == "mp3" {
            "mp3".to_owned()
        } else {
            "ogg".to_owned()
        });
    }
    json(
        StatusCode::OK,
        &PlaybackInfoResponse {
            media_sources: vec![src],
            play_session_id: psid,
            error_code: None,
        },
    )
}

// ===== Favorites + played (both dialects, 200 UserItemDataDto) =====

fn marker(item_id: &str, is_favorite: bool, played: bool) -> UserItemDataDto {
    UserItemDataDto {
        item_id: item_id.to_owned(),
        key: item_id.to_owned(),
        playback_position_ticks: 0,
        play_count: 0,
        is_favorite,
        played,
        last_played_date: None,
        rating: None,
        played_percentage: None,
    }
}

async fn set_favorite<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    item_id: &str,
    add: bool,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let Some((kind, internal)) = state.ids.from_jf(item_id).await else {
        return error(StatusCode::NOT_FOUND);
    };
    if kind != "artist" && kind != "album" && kind != "track" {
        return error(StatusCode::BAD_REQUEST);
    }
    state
        .library
        .set_favorite(user_id, &kind, &internal, add)
        .await;
    json(StatusCode::OK, &marker(item_id, add, false))
}

async fn set_played<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    item_id: &str,
    played: bool,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    // Marker only: play counting goes via Sessions/Playing/Stopped, so this
    // writes nothing (avoids unwanted scrobble forwards, v2 `_set_played`).
    if state.ids.from_jf(item_id).await.is_none() {
        return error(StatusCode::NOT_FOUND);
    }
    json(StatusCode::OK, &marker(item_id, false, played))
}

macro_rules! authed_handler {
    ($state:expr, $request:expr, |$authed:ident| $body:expr) => {{
        if let Some(denied) = gate(&$state) {
            return denied;
        }
        let raw_query = $request.uri().query().map(str::to_owned);
        let headers = $request.headers().clone();
        let $authed = match authed(&$state.passwords, &headers, raw_query.as_deref()).await {
            Ok(authed) => authed,
            Err(denied) => return denied,
        };
        $body
    }};
}

async fn favorite_add_legacy<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
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
        set_favorite(&state, &ctx.principal.id, &item_id, true).await
    })
}

async fn favorite_add_modern<S, L, E, P, I>(
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
        set_favorite(&state, &ctx.principal.id, &item_id, true).await
    })
}

async fn favorite_remove_legacy<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
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
        set_favorite(&state, &ctx.principal.id, &item_id, false).await
    })
}

async fn favorite_remove_modern<S, L, E, P, I>(
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
        set_favorite(&state, &ctx.principal.id, &item_id, false).await
    })
}

async fn played_add_legacy<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |_ctx| set_played(&state, &item_id, true)
        .await)
}

async fn played_add_modern<S, L, E, P, I>(
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
    authed_handler!(state, request, |_ctx| set_played(&state, &item_id, true)
        .await)
}

async fn played_remove_legacy<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((_user_id, item_id)): Path<(String, String)>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    authed_handler!(state, request, |_ctx| set_played(&state, &item_id, false)
        .await)
}

async fn played_remove_modern<S, L, E, P, I>(
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
    authed_handler!(state, request, |_ctx| set_played(&state, &item_id, false)
        .await)
}

// ===== Playback reporting / scrobbling (all 204, lenient bodies) =====

/// Item id → file id for session reports (`None` for unknown/non-track,
/// v2 `_track_from_item_id`).
async fn report_file_id<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    item_id: &str,
) -> Option<String>
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let (kind, internal) = state.ids.from_jf(item_id).await?;
    (kind == "track").then_some(internal)
}

async fn read_body_lenient<T: serde::de::DeserializeOwned>(
    request: Request<Body>,
) -> (HeaderMap, Option<String>, Option<T>) {
    let (parts, body) = request.into_parts();
    let query = parts.uri.query().map(str::to_owned);
    let parsed = axum::body::to_bytes(body, 64 * 1024)
        .await
        .ok()
        .filter(|bytes| !bytes.is_empty())
        .and_then(|bytes| serde_json::from_slice::<T>(&bytes).ok());
    (parts.headers, query, parsed)
}

/// Start → presence + now-playing (v2 `_playing_start`).
async fn playing<S, L, E, P, I>(
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
    let (headers, query, body) = read_body_lenient::<PlaybackStartInfo>(request).await;
    let authed = match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let Some(body) = body else {
        return no_content();
    };
    let Some(item_id) = body.item_id.as_deref() else {
        return no_content();
    };
    let key = body.play_session_id.as_deref().or(Some(item_id));
    if let Some(key) = key {
        state.sessions.mark_started(&authed.principal.id, key).await;
    }
    if let Some(file_id) = report_file_id(&state, item_id).await {
        state
            .sessions
            .now_playing(&authed.principal.id, &file_id, authed.client.as_deref())
            .await;
    }
    no_content()
}

/// Progress → live scrubber + heartbeat, never a scrobble (v2
/// `_playing_progress`).
async fn playing_progress<S, L, E, P, I>(
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
    let (headers, query, body) = read_body_lenient::<PlaybackProgressInfo>(request).await;
    let authed = match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let Some(body) = body else {
        return no_content();
    };
    let Some(item_id) = body.item_id.as_deref() else {
        return no_content();
    };
    if let Some(file_id) = report_file_id(&state, item_id).await {
        let position_ms = body.position_ticks.map(|t| t / (TICKS_PER_SECOND / 1000));
        state
            .sessions
            .progress(&authed.principal.id, &file_id, position_ms, body.is_paused)
            .await;
    }
    no_content()
}

/// Stop → always drop presence, then scrobble only if past the threshold (v2
/// `_playing_stopped`). `Failed` stops skip the scrobble.
async fn playing_stopped<S, L, E, P, I>(
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
    let (headers, query, body) = read_body_lenient::<PlaybackStopInfo>(request).await;
    let authed = match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    // Drop presence even when the play never reaches the threshold below,
    // or it lingers until TTL.
    state
        .sessions
        .clear_presence(&authed.principal.id, authed.client.as_deref())
        .await;
    let Some(body) = body else {
        return no_content();
    };
    if body.failed {
        return no_content();
    }
    let Some(item_id) = body.item_id.as_deref() else {
        return no_content();
    };
    let Some(file_id) = report_file_id(&state, item_id).await else {
        return no_content();
    };
    let track = state.library.track(&authed.principal.id, &file_id).await;
    let runtime = body.run_time_ticks.or_else(|| {
        track
            .as_ref()
            .and_then(|t| builders::ticks(t.duration_seconds))
    });
    if !should_scrobble(body.position_ticks, runtime) {
        return no_content();
    }
    let key = body
        .play_session_id
        .as_deref()
        .or(Some(item_id))
        .unwrap_or("");
    state.sessions.pop_started(&authed.principal.id, key).await;
    state
        .sessions
        .scrobble(&authed.principal.id, &file_id, authed.client.as_deref())
        .await;
    no_content()
}

async fn sessions_ping<S, L, E, P, I>(
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
    authed_handler!(state, request, |_ctx| no_content())
}

/// No session registry stores reported capabilities; accept and 204 to
/// avoid a 404 (v2 `_handle` lambda).
async fn sessions_capabilities<S, L, E, P, I>(
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
    authed_handler!(state, request, |_ctx| no_content())
}

// ===== Playlists =====

async fn decode_playlist<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    jf_id: &str,
) -> Result<String, Response>
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    match state.ids.from_jf(jf_id).await {
        Some((kind, internal)) if kind == "playlist" => Ok(internal),
        _ => Err(error(StatusCode::NOT_FOUND)),
    }
}

/// Create from a JSON body or `?name=`/`?ids=` (v2 `_create_playlist`).
/// Unknown track ids are skipped silently.
async fn create_playlist<S, L, E, P, I>(
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
    let (headers, query, body) = read_body_lenient::<CreatePlaylistDto>(request).await;
    let authed = match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => authed,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(query.as_deref());
    let body = body.unwrap_or_default();
    let name = if !body.name.is_empty() {
        body.name
    } else {
        q.get("name").unwrap_or("Playlist").to_owned()
    };
    let track_ids = if !body.ids.is_empty() {
        body.ids
    } else {
        params::ids_param(&q, &["ids"])
    };
    let internal = state
        .library
        .create_playlist(&authed.principal.id, &name)
        .await;
    for jf_id in &track_ids {
        if let Some(file_id) = report_file_id(&state, jf_id).await {
            state
                .library
                .add_playlist_entry(&authed.principal.id, &internal, &file_id)
                .await;
        }
    }
    json(
        StatusCode::OK,
        &serde_json::json!({ "Id": state.ids.to_jf("playlist", &internal).await }),
    )
}

async fn get_playlist<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(playlist_id): Path<String>,
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
        let internal = match decode_playlist(&state, &playlist_id).await {
            Ok(internal) => internal,
            Err(denied) => return denied,
        };
        let detail = match state.library.playlist(&ctx.principal.id, &internal).await {
            Some(detail) => detail,
            None => return error(StatusCode::NOT_FOUND),
        };
        let mut item_ids = Vec::new();
        for entry in &detail.entries {
            if let Some(file_id) = entry.file_id.as_deref() {
                item_ids.push(state.ids.to_jf("track", file_id).await);
            }
        }
        json(
            StatusCode::OK,
            &serde_json::json!({
                "Id": playlist_id,
                "Name": detail.name,
                "Type": "Playlist",
                "ServerId": state.server_id,
                "ItemIds": item_ids,
            }),
        )
    })
}

async fn playlist_items<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(playlist_id): Path<String>,
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
    let ctx = match authed(&state.passwords, &headers, raw_query.as_deref()).await {
        Ok(ctx) => ctx,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(raw_query.as_deref());
    playlist_items_inner(&state, &ctx.principal.id, &playlist_id, &q).await
}

/// Real playlist-items body, split out so the auth gate stays readable.
async fn playlist_items_inner<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    user_id: &str,
    playlist_id: &str,
    q: &CiParams,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    let internal = match decode_playlist(state, playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let detail = match state.library.playlist(user_id, &internal).await {
        Some(detail) => detail,
        None => return error(StatusCode::NOT_FOUND),
    };
    // v2 reads both `startIndex` and `StartIndex`, but params are
    // case-insensitive so both spellings are one key.
    let start = params::qint(q, "startIndex", 0).max(0) as usize;
    let limit = params::qint(q, "limit", 0).max(0) as usize;
    let b = builder(state);
    let mut items = Vec::new();
    for entry in detail.entries.iter().filter(|e| e.file_id.is_some()) {
        let file_id = entry.file_id.as_deref().unwrap_or("");
        let Some(track) = state.library.track(user_id, file_id).await else {
            continue;
        };
        let mut dto = b.audio(&track).await;
        dto.playlist_item_id = Some(entry.id.clone());
        items.push(dto);
    }
    let total = items.len();
    let items = if start >= total {
        Vec::new()
    } else if limit == 0 {
        items[start..].to_vec()
    } else {
        items[start..(start.saturating_add(limit)).min(total)].to_vec()
    };
    json(
        StatusCode::OK,
        &BaseItemDtoQueryResult {
            items,
            total_record_count: total,
            start_index: start,
        },
    )
}

async fn playlist_add<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(playlist_id): Path<String>,
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
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(raw_query.as_deref());
    for jf_id in params::ids_param(&q, &["ids", "Ids"]) {
        if let Some(file_id) = report_file_id(&state, &jf_id).await {
            state
                .library
                .add_playlist_entry(&authed.principal.id, &internal, &file_id)
                .await;
        }
    }
    no_content()
}

async fn playlist_remove<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(playlist_id): Path<String>,
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
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    let q = CiParams::parse(raw_query.as_deref());
    let entry_ids = params::ids_param(&q, &["entryIds", "EntryIds"]);
    if !entry_ids.is_empty() {
        state
            .library
            .remove_playlist_entries(&authed.principal.id, &internal, &entry_ids)
            .await;
    }
    no_content()
}

async fn playlist_move<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path((playlist_id, entry_id, new_index)): Path<(String, String, String)>,
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
    let Ok(index) = new_index.parse::<usize>() else {
        return error(StatusCode::BAD_REQUEST);
    };
    let internal = match decode_playlist(&state, &playlist_id).await {
        Ok(internal) => internal,
        Err(denied) => return denied,
    };
    state
        .library
        .move_playlist_entry(&authed.principal.id, &internal, &entry_id, index)
        .await;
    no_content()
}

// ===== Discovery (owned-only) =====

/// Similar + both InstantMix routes share one handler (v2 `_similar`):
/// resolves the artist from an artist/track/album id and serves same-artist
/// tracks. Unknown ids and unresolvable kinds yield an empty result, not a
/// 404. (No real similarity ranking is bound yet.)
async fn similar<S, L, E, P, I>(
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
