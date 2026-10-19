//! Jellyfin router: state, route registration, response helpers and the
//! auth gates every handler shares.
//!
//! Wire contract: PascalCase JSON, real HTTP statuses, empty error bodies
//! (not the native envelope), except the answers Jellyfin itself sends with
//! a body (see `not_found_item`). Audio needs the caller's token like every
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

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use droppedneedle_auth::compat_auth::jellyfin::{
    JellyfinRequest, extract_client, extract_token, resolve_token, server_id,
};
use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_kernel::ids::RequestId;
use droppedneedle_kernel::mount::CompatRoutes;
use uuid::Uuid;

use crate::body::AudioBody;
use crate::settings::LiveSettings;
use crate::shared::address::{ListenerConfig, base_address};

use super::audio::*;
use super::browse::*;
use super::builders::Builder;
use super::images::*;
use super::playlists::*;
use super::playstate::*;
use super::seams::{
    ByteOutcome, IdMap, LibraryRead, PlaybackSessions, Principal, StreamEngine, WriteRefusal,
};
use super::system::*;

// ===== State + registration =====

/// Router dependencies. Every seam but the app-password store is a generic
/// so production binds the real adapters without touching the routes: `L`
/// library reads, `E` stream engine, `P` scrobble adapter, `I` persisted id
/// map.
#[derive(Clone)]
pub struct JellyfinState<L, E, P, I> {
    /// App-password store.
    pub passwords: ProdCompatPasswords,
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
    /// Restart-stable server id, computed once (`compat_auth` owns the value).
    pub server_id: String,
    /// Port, trusted proxies and base path, from the deployment config.
    pub listener: ListenerConfig,
}

impl<L, E, P, I> JellyfinState<L, E, P, I> {
    /// Assemble state; `server_id()` is restart-stable.
    pub fn new(
        passwords: ProdCompatPasswords,
        library: L,
        engine: E,
        sessions: P,
        ids: I,
        settings: impl Into<LiveSettings>,
        listener: ListenerConfig,
    ) -> Self {
        Self {
            passwords,
            library,
            engine,
            sessions,
            ids,
            settings: settings.into(),
            server_id: server_id(),
            listener,
        }
    }
}

/// The `/jellyfin` routes.
pub fn routes<L, E, P, I>(state: JellyfinState<L, E, P, I>) -> CompatRoutes
where
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    CompatRoutes::new()
        .at(
            "/jellyfin/System/Info/Public",
            get(system_info_public::<L, E, P, I>),
        )
        .at("/jellyfin/System/Info", get(system_info::<L, E, P, I>))
        .at(
            "/jellyfin/QuickConnect/Enabled",
            get(quick_connect::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Sessions/Logout",
            post(sessions_logout::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Users/AuthenticateByName",
            post(authenticate::<L, E, P, I>),
        )
        .at("/jellyfin/Users/Me", get(users_me::<L, E, P, I>))
        .at("/jellyfin/Users/{user_id}", get(user_by_id::<L, E, P, I>))
        .at("/jellyfin/Users/{user_id}/Views", get(views::<L, E, P, I>))
        .at("/jellyfin/UserViews", get(views::<L, E, P, I>))
        .at("/jellyfin/Users/{user_id}/Items", get(browse::<L, E, P, I>))
        .at("/jellyfin/Items", get(browse::<L, E, P, I>))
        // Not recorded: Jellyfin 10.10.6 serves this from
        // `UserLibraryController.GetLatestMedia`, the user from the token.
        // Jellify asks here (`@jellyfin/sdk` 0.13.0). Before `/Items/{item_id}`.
        .at("/jellyfin/Items/Latest", get(latest::<L, E, P, I>))
        .at(
            "/jellyfin/Users/{user_id}/Items/Latest",
            get(latest::<L, E, P, I>),
        )
        .at("/jellyfin/Artists", get(artists::<L, E, P, I>))
        .at(
            "/jellyfin/Artists/AlbumArtists",
            get(album_artists::<L, E, P, I>),
        )
        .at("/jellyfin/Genres", get(genres::<L, E, P, I>))
        .at("/jellyfin/MusicGenres", get(genres::<L, E, P, I>))
        // Filters before `/{item_id}`: "Filters" must never be an id.
        .at("/jellyfin/Items/Filters", get(filters::<L, E, P, I>))
        .at(
            "/jellyfin/Users/{user_id}/Items/Filters",
            get(filters::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Users/{user_id}/Items/{item_id}",
            get(single_item_legacy::<L, E, P, I>),
        )
        .at("/jellyfin/Items/{item_id}", get(single_item::<L, E, P, I>))
        .at(
            "/jellyfin/Items/{item_id}/Images/{image_type}",
            get(image::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Items/{item_id}/Images/{image_type}/{index}",
            get(image_indexed::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Audio/{item_id}/{tail}",
            get(audio::<L, E, P, I>).head(audio_head::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Items/{item_id}/File",
            get(item_file::<L, E, P, I>).head(item_file_head::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Items/{item_id}/Download",
            get(item_download::<L, E, P, I>).head(item_download_head::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Items/{item_id}/PlaybackInfo",
            get(playback_info::<L, E, P, I>).post(playback_info::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Users/{user_id}/FavoriteItems/{item_id}",
            post(favorite_add_legacy::<L, E, P, I>).delete(favorite_remove_legacy::<L, E, P, I>),
        )
        .at(
            "/jellyfin/UserFavoriteItems/{item_id}",
            post(favorite_add_modern::<L, E, P, I>).delete(favorite_remove_modern::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Users/{user_id}/PlayedItems/{item_id}",
            post(played_add_legacy::<L, E, P, I>).delete(played_remove_legacy::<L, E, P, I>),
        )
        .at(
            "/jellyfin/UserPlayedItems/{item_id}",
            post(played_add_modern::<L, E, P, I>).delete(played_remove_modern::<L, E, P, I>),
        )
        .at("/jellyfin/Sessions/Playing", post(playing::<L, E, P, I>))
        .at(
            "/jellyfin/Sessions/Playing/Progress",
            post(playing_progress::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Sessions/Playing/Stopped",
            post(playing_stopped::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Sessions/Playing/Ping",
            post(sessions_ping::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Sessions/Capabilities/Full",
            post(sessions_capabilities::<L, E, P, I>),
        )
        .at("/jellyfin/Playlists", post(create_playlist::<L, E, P, I>))
        .at(
            "/jellyfin/Playlists/{playlist_id}",
            get(get_playlist::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Playlists/{playlist_id}/Items",
            get(playlist_items::<L, E, P, I>)
                .post(playlist_add::<L, E, P, I>)
                .delete(playlist_remove::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Playlists/{playlist_id}/Items/{entry_id}/Move/{new_index}",
            post(playlist_move::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Items/{item_id}/Similar",
            get(similar::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Items/{item_id}/InstantMix",
            get(similar::<L, E, P, I>),
        )
        .at(
            "/jellyfin/Artists/{item_id}/InstantMix",
            get(similar::<L, E, P, I>),
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

/// The 404 Jellyfin sends for an item that does not exist: an ASP.NET
/// problem object. The recording `error.unknown-item.json` (under
/// `server/tests/reference/compat/jellyfin-10.10.6/`) keeps the path of the
/// RFC 9110 link, but the recorder replaced its host, so the host is
/// ASP.NET's default. `traceId` is the request's own id when the app set one.
pub(super) fn not_found_item(request: &Request<Body>) -> Response {
    let trace_id = request
        .extensions()
        .get::<RequestId>()
        .map_or_else(|| Uuid::new_v4().to_string(), |id| id.0.clone());
    json(
        StatusCode::NOT_FOUND,
        &serde_json::json!({
            "type": "https://tools.ietf.org/html/rfc9110#section-15.5.5",
            "title": "Not Found",
            "status": 404,
            "traceId": trace_id,
        }),
    )
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

/// Map a refused library write to its status (v2 maps not-found to 404
/// and permission errors to 403).
pub(super) fn refused(refusal: WriteRefusal) -> Response {
    error(match refusal {
        WriteRefusal::NotFound => StatusCode::NOT_FOUND,
        WriteRefusal::Forbidden => StatusCode::FORBIDDEN,
        WriteRefusal::Invalid => StatusCode::BAD_REQUEST,
        WriteRefusal::Conflict => StatusCode::CONFLICT,
        WriteRefusal::Failed => StatusCode::INTERNAL_SERVER_ERROR,
    })
}

/// Map an engine byte outcome to a response (200/206 bytes, 416 with its
/// headers, 404/429 status-only). Outcomes without a `Content-Length` carry a
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

/// A query token that is not valid percent-encoding.
#[derive(Debug)]
pub(crate) struct MalformedToken;

/// Undo the percent-encoding of a query token, once: `%3D` is `=`, and
/// `%253D` is the text `%3D`. An app password ends in `=`, which a player
/// may send either way. Nothing else changes: a `+` stays a `+` and is
/// never a space (app passwords are URL-safe base64 and hold neither),
/// nothing is trimmed and case is kept. A `%` without two hex digits, or
/// bytes that are not UTF-8, refuse the token.
///
/// Written here because no decoder among the dependencies is strict
/// enough for a credential: `percent-encoding` keeps a bad escape as
/// text, and the form decoders turn `+` into a space and replace bytes
/// that are not UTF-8.
fn decode_token(raw: &str) -> Result<String, MalformedToken> {
    fn hex(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte != b'%' {
            out.push(byte);
            rest = tail;
            continue;
        }
        let [hi, lo, tail @ ..] = tail else {
            return Err(MalformedToken);
        };
        let (Some(hi), Some(lo)) = (hex(*hi), hex(*lo)) else {
            return Err(MalformedToken);
        };
        out.push(hi << 4 | lo);
        rest = tail;
    }
    String::from_utf8(out).map_err(|_| MalformedToken)
}

/// The tokens a request carries in its query, decoded. The handlers and
/// the rate limiter both read them here, so both see the same token.
pub(crate) struct QueryTokens {
    apikey: Option<String>,
    api_key: Option<String>,
}

impl QueryTokens {
    /// Fails when either value is malformed, even if a header token or the
    /// other value would have been used: such a request is refused.
    pub(crate) fn read(raw_query: Option<&str>) -> Result<Self, MalformedToken> {
        let token = |key| query_exact(raw_query, key).map(decode_token).transpose();
        Ok(Self {
            apikey: token("ApiKey")?,
            api_key: token("api_key")?,
        })
    }

    /// The auth inputs of a request: its headers as sent, plus these.
    pub(crate) fn request<'a>(&'a self, headers: &'a HeaderMap) -> JellyfinRequest<'a> {
        JellyfinRequest {
            authorization: header(headers, "authorization"),
            emby_authorization: header(headers, "x-emby-authorization"),
            emby_token: header(headers, "x-emby-token"),
            mediabrowser_token: header(headers, "x-mediabrowser-token"),
            query_apikey: self.apikey.as_deref(),
            query_api_key: self.api_key.as_deref(),
        }
    }
}

/// Resolve the caller or return the 401 (empty body, never raises).
pub(super) async fn authed(
    passwords: &ProdCompatPasswords,
    headers: &HeaderMap,
    raw_query: Option<&str>,
) -> Result<Authed, Response> {
    let Ok(query) = QueryTokens::read(raw_query) else {
        return Err(error(StatusCode::UNAUTHORIZED));
    };
    let request = query.request(headers);
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
pub(super) fn gate<L, E, P, I>(state: &JellyfinState<L, E, P, I>) -> Option<Response> {
    if state.settings.jellyfin().enabled {
        None
    } else {
        Some(error(StatusCode::NOT_FOUND))
    }
}

/// The address of this API as `request` reached it ([`base_address`]) plus
/// `/jellyfin`. `LocalAddress` is required, so a request that names no host
/// gets `http://localhost` and the base path.
pub(super) fn local_address<L, E, P, I>(
    state: &JellyfinState<L, E, P, I>,
    request: &Request<Body>,
) -> String {
    let base = base_address(request, &state.listener, &state.settings)
        .unwrap_or_else(|| format!("http://localhost{}", state.listener.base_path));
    format!("{base}/jellyfin")
}

pub(super) fn builder<'a, L, E, P, I>(state: &'a JellyfinState<L, E, P, I>) -> Builder<'a, I>
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
