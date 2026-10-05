//! Jellyfin router: state, route registration, response helpers and the
//! auth gates every handler shares.
//!
//! Wire contract: PascalCase JSON, real HTTP statuses, empty error bodies
//! (not the native envelope). Audio needs the caller's token like every
//! other library route. Headerless players (Jellify, Finamp, Manet) still
//! work: the URLs PlaybackInfo hands out carry `api_key=<token>`, and the
//! token is read from the query as well as the headers. Anonymous audio is
//! a 401, so nobody can stream or start transcodes without an account.
//!
//! Route notes: `/Items/Filters` must stay registered
//! alongside (before, for clarity) `/Items/{item_id}` or "Filters" would be
//! captured as an id (v2 comment; axum prefers the static route). There is
//! no `/jellyfin/socket` endpoint, on purpose (v2 gap: uvicorn 403s the
//! websocket scope; Finamp PlayOn only, non-blocking).

use crate::auth::compat_auth::jellyfin::{
    JellyfinPasswordStore, JellyfinRequest, extract_client, extract_token, resolve_token, server_id,
};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};

use crate::compat::body::AudioBody;
use crate::compat::settings::LiveSettings;

use super::audio::*;
use super::browse::*;
use super::builders::Builder;
use super::images::*;
use super::playlists::*;
use super::playstate::*;
use super::seams::{ByteOutcome, IdMap, LibraryRead, PlaybackSessions, Principal, StreamEngine};
use super::system::*;

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
    /// Connect-apps settings subset, read per request.
    pub settings: LiveSettings,
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
        settings: impl Into<LiveSettings>,
    ) -> Self {
        Self {
            passwords,
            library,
            engine,
            sessions,
            ids,
            settings: settings.into(),
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
pub(super) fn error(status: StatusCode) -> Response {
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

pub(super) fn json<T: serde::Serialize>(status: StatusCode, payload: &T) -> Response {
    match serde_json::to_vec(payload) {
        Ok(body) => Response::builder()
            .status(status)
            .header("Content-Type", "application/json")
            .body(Body::from(body))
            .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR)),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(super) fn raw_json(status: StatusCode, body: &str) -> Response {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

pub(super) fn no_content() -> Response {
    error(StatusCode::NO_CONTENT)
}

/// Map an engine byte outcome to a response (200/206 bytes, 416 range-only,
/// 404/429 status-only). Outcomes without a `Content-Length` carry a
/// non-empty body only for transcodes (estimate off on Jellyfin), and those
/// must stream unsized: axum stamps a `Content-Length` on sized bodies, so a
/// sized transcode response would violate the v2 wire contract. Empty bodies
/// stay sized (`Content-Length: 0`, the v2 416 shape).
pub(super) fn outcome_response(outcome: &ByteOutcome) -> Response {
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
    let body = match &outcome.body {
        // Streamed audio: file spans or the ffmpeg pipe, lease inside.
        AudioBody::Live(live) => live.take().map_or_else(Body::empty, Body::from_stream),
        AudioBody::Bytes(bytes) if sized => Body::from(bytes.clone()),
        AudioBody::Bytes(bytes) => {
            let bytes = bytes.clone();
            Body::from_stream(futures_util::stream::once(async move {
                Ok::<_, std::convert::Infallible>(bytes)
            }))
        }
    };
    builder
        .body(body)
        .unwrap_or_else(|_| error(StatusCode::INTERNAL_SERVER_ERROR))
}

// ===== Auth + gates =====

/// Authenticated request context: principal plus the media-browser facts.
pub(super) struct Authed {
    pub(super) principal: Principal,
    pub(super) client: Option<String>,
}

pub(super) fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// Exact-spelling raw-query lookup for the two auth keys (v2 `auth.py`
/// reads Starlette's case-sensitive params: only `ApiKey` and `api_key`).
pub(super) fn query_exact<'a>(raw: Option<&'a str>, key: &str) -> Option<&'a str> {
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
pub(super) async fn authed<S: JellyfinPasswordStore>(
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
        Err(denied) => Err(error(
            StatusCode::from_u16(denied.status()).unwrap_or(StatusCode::UNAUTHORIZED),
        )),
    }
}

/// The kill-switch gate: disabled → 404 before any handler lookup.
pub(super) fn gate<S, L, E, P, I>(state: &JellyfinState<S, L, E, P, I>) -> Option<Response> {
    if state.settings.jellyfin().enabled {
        None
    } else {
        Some(error(StatusCode::NOT_FOUND))
    }
}

/// Advertised origin plus shim prefix (v2 `_local_address`).
pub(super) fn local_address<S, L, E, P, I>(
    state: &JellyfinState<S, L, E, P, I>,
    headers: &HeaderMap,
) -> String {
    let host = header(headers, "host").unwrap_or("localhost");
    format!("http://{host}{}/jellyfin", state.base_path)
}

pub(super) fn builder<'a, S, L, E, P, I>(state: &'a JellyfinState<S, L, E, P, I>) -> Builder<'a, I>
where
    I: IdMap,
{
    Builder::new(&state.ids, &state.server_id)
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
pub(super) use authed_handler;
