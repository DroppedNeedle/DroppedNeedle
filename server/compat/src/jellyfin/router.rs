//! Jellyfin router: state, the route table, response helpers and the two
//! layers every route sits behind.
//!
//! Wire contract: PascalCase JSON, real HTTP statuses, empty error bodies
//! (not the native envelope), except the answers Jellyfin itself sends with
//! a body (see `not_found_item`). The table ([`ROUTES`]) says which routes
//! are public; every other one, audio included, needs the caller's token.
//! Headerless players (Jellify, Finamp, Manet) still work: the URLs
//! PlaybackInfo hands out carry `api_key=<token>`, and the token is read
//! from the query as well as the headers.

use axum::body::Body;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{MethodFilter, MethodRouter, on};
use droppedneedle_auth::compat_auth::jellyfin::{
    JellyfinRequest, extract_client, extract_token, resolve_token, server_id,
};
use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::ids::RequestId;
use droppedneedle_kernel::mount::CompatRoutes;
use uuid::Uuid;

use crate::adapters::jellyfin_library::{CatalogIds, JellyfinLibrary};
use crate::adapters::playback::CompatPlayback;
use crate::body::AudioBody;
use crate::settings::LiveSettings;
use crate::setup::CompatAudio;
use crate::shared::address::{ListenerConfig, base_address};
use crate::shared::percent::{self, Malformed, Plus};

use super::audio::*;
use super::browse::*;
use super::builders::Builder;
use super::images::*;
use super::playlists::*;
use super::playstate::*;
use super::seams::{ByteOutcome, Principal, WriteRefusal};
use super::system::*;

// ===== State + registration =====

/// Router dependencies.
#[derive(Clone)]
pub struct JellyfinState {
    /// App-password store.
    pub passwords: ProdCompatPasswords,
    /// Library reads.
    pub library: JellyfinLibrary,
    /// Audio over the stream gateway.
    pub engine: CompatAudio,
    /// Presence and scrobbles.
    pub sessions: CompatPlayback,
    /// The stored Jellyfin ids.
    pub ids: CatalogIds,
    /// Connect-apps settings subset, read per request.
    pub settings: LiveSettings,
    /// Restart-stable server id, computed once (`compat_auth` owns the value).
    pub server_id: String,
    /// Port, trusted proxies and base path, from the deployment config.
    pub listener: ListenerConfig,
}

impl JellyfinState {
    /// Assemble state; `server_id()` is restart-stable.
    pub fn new(
        passwords: ProdCompatPasswords,
        library: JellyfinLibrary,
        engine: CompatAudio,
        sessions: CompatPlayback,
        ids: CatalogIds,
        settings: LiveSettings,
        listener: ListenerConfig,
    ) -> Self {
        Self {
            passwords,
            library,
            engine,
            sessions,
            ids,
            settings,
            server_id: server_id(),
            listener,
        }
    }
}

// ===== The route table =====

/// Who may call a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Anyone who can reach the server, while the API is on.
    Public,
    /// A caller with a valid app password.
    SignedIn,
}

/// The methods the table uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// `GET`, which also answers `HEAD` on a template with no `HEAD` row.
    Get,
    /// `HEAD`.
    Head,
    /// `POST`.
    Post,
    /// `DELETE`.
    Delete,
}

impl Verb {
    /// The method's name on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }

    fn filter(self) -> MethodFilter {
        match self {
            Self::Get => MethodFilter::GET,
            Self::Head => MethodFilter::HEAD,
            Self::Post => MethodFilter::POST,
            Self::Delete => MethodFilter::DELETE,
        }
    }
}

/// One row of the route table.
#[derive(Debug, Clone, Copy)]
pub struct Route {
    /// Who may call it.
    pub access: Access,
    /// Its method.
    pub verb: Verb,
    /// Its path template, as axum matches it.
    pub template: &'static str,
}

/// Whether `method` on the route axum matched as `template` is public.
/// `HEAD` is answered by the `GET` row where the template has no `HEAD`
/// row, as axum does.
pub fn is_public(method: &str, template: &str) -> bool {
    let row = |verb: Verb| {
        ROUTES
            .iter()
            .find(|row| row.template == template && row.verb == verb)
    };
    let found = match method {
        "GET" => row(Verb::Get),
        "HEAD" => row(Verb::Head).or_else(|| row(Verb::Get)),
        "POST" => row(Verb::Post),
        "DELETE" => row(Verb::Delete),
        _ => None,
    };
    found.is_some_and(|row| row.access == Access::Public)
}

/// The handlers of one or more rows.
type Methods = MethodRouter<JellyfinState>;

/// One template with the handlers of all its rows.
type Template = (&'static str, Methods);

/// Lists every route once, with its access and handler, and makes both
/// [`ROUTES`] and the handlers, in the same order, from that one list.
macro_rules! route_table {
    ($(
        $access:ident $verb:ident $template:literal => $handler:ident;
    )*) => {
        /// Every Jellyfin route: who may call it, its method and template.
        pub const ROUTES: &[Route] = &[
            $(Route { access: Access::$access, verb: Verb::$verb, template: $template },)*
        ];

        /// The handler of each row of [`ROUTES`], in the same order.
        fn handlers() -> Vec<Methods> {
            vec![$(on(Verb::$verb.filter(), $handler),)*]
        }
    };
}

// Public: what a real Jellyfin 10.10.6 serves without sign-in, so an app
// can find the server, sign in and load pictures. Everything else, audio
// included, needs a token. `/Users/Public` lists nobody: every user here
// is hidden.
//
// `POST /Sessions/Logout` needs a token, as on a real server
// (`SessionController.cs:419-420`). Neither listed app is held signed in by
// a refusal there. Finamp (`673b568a`, `logoutCurrentUser`) sends it with
// its token in `Authorization` and removes the user on the device whatever
// the answer. Jellify (`7dae4ea3`, `sign-out-modal.tsx`) never calls it.
//
// `/Items/Latest` and `/Items/Filters` sit beside `/Items/{item_id}`, and
// `/Users/Public` and `/Users/Me` beside `/Users/{user_id}`: axum and the
// case fixing both prefer a literal segment to a parameter, so none of them
// is read as an id. There is no `/jellyfin/socket`, on purpose (Finamp's
// PlayOn only, which does not block playback).
route_table! {
    Public Get "/jellyfin/System/Info/Public" => system_info_public;
    SignedIn Get "/jellyfin/System/Info" => system_info;
    Public Get "/jellyfin/QuickConnect/Enabled" => quick_connect;
    SignedIn Post "/jellyfin/Sessions/Logout" => sessions_logout;
    Public Post "/jellyfin/Users/AuthenticateByName" => authenticate;
    Public Get "/jellyfin/Users/Public" => users_public;
    SignedIn Get "/jellyfin/Users/Me" => users_me;
    SignedIn Get "/jellyfin/Users/{user_id}" => user_by_id;
    SignedIn Get "/jellyfin/Users/{user_id}/Views" => views;
    SignedIn Get "/jellyfin/UserViews" => views;
    SignedIn Get "/jellyfin/Users/{user_id}/Items" => browse;
    SignedIn Get "/jellyfin/Items" => browse;
    // Not recorded: Jellyfin 10.10.6 serves this from
    // `UserLibraryController.GetLatestMedia`, the user from the token.
    // Jellify asks here (`@jellyfin/sdk` 0.13.0).
    SignedIn Get "/jellyfin/Items/Latest" => latest;
    SignedIn Get "/jellyfin/Users/{user_id}/Items/Latest" => latest;
    SignedIn Get "/jellyfin/Artists" => artists;
    SignedIn Get "/jellyfin/Artists/AlbumArtists" => album_artists;
    SignedIn Get "/jellyfin/Genres" => genres;
    SignedIn Get "/jellyfin/MusicGenres" => genres;
    SignedIn Get "/jellyfin/Items/Filters" => filters;
    SignedIn Get "/jellyfin/Users/{user_id}/Items/Filters" => filters;
    SignedIn Get "/jellyfin/Users/{user_id}/Items/{item_id}" => single_item_legacy;
    SignedIn Get "/jellyfin/Items/{item_id}" => single_item;
    Public Get "/jellyfin/Items/{item_id}/Images/{image_type}" => image;
    Public Get "/jellyfin/Items/{item_id}/Images/{image_type}/{index}" => image_indexed;
    SignedIn Get "/jellyfin/Audio/{item_id}/{tail}" => audio;
    SignedIn Head "/jellyfin/Audio/{item_id}/{tail}" => audio_head;
    SignedIn Get "/jellyfin/Items/{item_id}/File" => item_file;
    SignedIn Head "/jellyfin/Items/{item_id}/File" => item_file_head;
    SignedIn Get "/jellyfin/Items/{item_id}/Download" => item_download;
    SignedIn Head "/jellyfin/Items/{item_id}/Download" => item_download_head;
    SignedIn Get "/jellyfin/Items/{item_id}/PlaybackInfo" => playback_info;
    SignedIn Post "/jellyfin/Items/{item_id}/PlaybackInfo" => playback_info;
    SignedIn Post "/jellyfin/Users/{user_id}/FavoriteItems/{item_id}" => favorite_add_legacy;
    SignedIn Delete "/jellyfin/Users/{user_id}/FavoriteItems/{item_id}" => favorite_remove_legacy;
    SignedIn Post "/jellyfin/UserFavoriteItems/{item_id}" => favorite_add_modern;
    SignedIn Delete "/jellyfin/UserFavoriteItems/{item_id}" => favorite_remove_modern;
    SignedIn Post "/jellyfin/Users/{user_id}/PlayedItems/{item_id}" => played_add_legacy;
    SignedIn Delete "/jellyfin/Users/{user_id}/PlayedItems/{item_id}" => played_remove_legacy;
    SignedIn Post "/jellyfin/UserPlayedItems/{item_id}" => played_add_modern;
    SignedIn Delete "/jellyfin/UserPlayedItems/{item_id}" => played_remove_modern;
    SignedIn Post "/jellyfin/Sessions/Playing" => playing;
    SignedIn Post "/jellyfin/Sessions/Playing/Progress" => playing_progress;
    SignedIn Post "/jellyfin/Sessions/Playing/Stopped" => playing_stopped;
    SignedIn Post "/jellyfin/Sessions/Playing/Ping" => sessions_ping;
    SignedIn Post "/jellyfin/Sessions/Capabilities/Full" => sessions_capabilities;
    SignedIn Post "/jellyfin/Playlists" => create_playlist;
    SignedIn Get "/jellyfin/Playlists/{playlist_id}" => get_playlist;
    SignedIn Get "/jellyfin/Playlists/{playlist_id}/Items" => playlist_items;
    SignedIn Post "/jellyfin/Playlists/{playlist_id}/Items" => playlist_add;
    SignedIn Delete "/jellyfin/Playlists/{playlist_id}/Items" => playlist_remove;
    SignedIn Post "/jellyfin/Playlists/{playlist_id}/Items/{entry_id}/Move/{new_index}" => playlist_move;
    SignedIn Get "/jellyfin/Items/{item_id}/Similar" => similar;
    SignedIn Get "/jellyfin/Items/{item_id}/InstantMix" => similar;
    SignedIn Get "/jellyfin/Artists/{item_id}/InstantMix" => similar;
}

/// The `/jellyfin` routes, built from [`ROUTES`]. A signed-in row sits
/// behind the sign-in layer, and every route, public or not, behind the
/// gate outside it: with the API off nothing reads a token, so nothing
/// answers 401 or counts toward a lockout.
pub fn routes(state: JellyfinState) -> CompatRoutes {
    let signed_in = from_fn_with_state(state.passwords.clone(), sign_in);
    let gate = from_fn_with_state(state.clone(), gate);
    let mut grouped: Vec<Template> = Vec::new();
    for (row, handler) in ROUTES.iter().zip(handlers()) {
        let handler = match row.access {
            Access::Public => handler,
            Access::SignedIn => handler.route_layer(signed_in.clone()),
        };
        match grouped
            .iter_mut()
            .find(|(template, _)| *template == row.template)
        {
            Some((_, methods)) => *methods = std::mem::take(methods).merge(handler),
            None => grouped.push((row.template, handler)),
        }
    }
    grouped
        .into_iter()
        .fold(CompatRoutes::new(), |routes, (template, methods)| {
            routes.at(template, methods.layer(gate.clone()))
        })
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
/// and permission errors to 403). A busy write wrote nothing, so it is
/// 503 with `Retry-After`; the request boundary logs a server fault once,
/// with its cause.
pub(super) fn refused(refusal: WriteRefusal) -> Response {
    use droppedneedle_kernel::error::{ApiError, Unavailable};
    error(match refusal {
        WriteRefusal::NotFound => StatusCode::NOT_FOUND,
        WriteRefusal::Forbidden => StatusCode::FORBIDDEN,
        WriteRefusal::Invalid => StatusCode::BAD_REQUEST,
        WriteRefusal::Conflict => StatusCode::CONFLICT,
        WriteRefusal::Busy(cause) => {
            return ApiError::unavailable(Unavailable::ServerBusy, cause).into_response();
        }
        WriteRefusal::Failed(cause) => return ApiError::internal(cause).into_response(),
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

/// The signed-in caller: principal plus the media-browser facts. Only the
/// sign-in layer makes one; a handler that takes it on a route without that
/// layer answers an empty 401.
#[derive(Clone)]
pub(super) struct Authed {
    pub(super) principal: Principal,
    pub(super) client: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for Authed {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Response> {
        parts
            .extensions
            .remove::<Authed>()
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED))
    }
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

/// Undo the percent-encoding of a query token, once: `%3D` is `=`, and
/// `%253D` is the text `%3D`. An app password ends in `=`, which a player
/// may send either way. Nothing else changes: a `+` stays a `+` and is
/// never a space (app passwords are URL-safe base64 and hold neither),
/// nothing is trimmed and case is kept. A `%` without two hex digits, or
/// bytes that are not UTF-8, refuse the token.
fn decode_token(raw: &str) -> Result<String, Malformed> {
    percent::strict(raw, Plus::Literal)
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
    pub(crate) fn read(raw_query: Option<&str>) -> Result<Self, Malformed> {
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

/// The sign-in layer of every signed-in route: reads the token once (the
/// headers, then `ApiKey` and `api_key` in the query) and hands the caller
/// to the handler as [`Authed`]. No token, a malformed one or an unknown
/// one is an empty 401, which the limits layer counts toward the lockout.
/// A credential-store failure keeps its own status, never a 401.
pub(super) async fn sign_in(
    State(passwords): State<ProdCompatPasswords>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Ok(query) = QueryTokens::read(request.uri().query()) else {
        return error(StatusCode::UNAUTHORIZED);
    };
    let (token, client) = {
        let inputs = query.request(request.headers());
        (extract_token(&inputs), extract_client(&inputs))
    };
    match resolve_token(&passwords, token.as_deref()).await {
        Ok(user) => {
            request.extensions_mut().insert(Authed {
                principal: Principal::from_user(&user, token.as_deref().unwrap_or("")),
                client,
            });
            next.run(request).await
        }
        Err(denied) => {
            error(StatusCode::from_u16(denied.status()).unwrap_or(StatusCode::UNAUTHORIZED))
        }
    }
}

/// The gate in front of every route, outside sign-in. With the API off it
/// answers the empty 404 before anything reads a token. Then, until this
/// process has loaded the stored ids, 503 server busy with `Retry-After`,
/// so no answer is a 404 or an empty list for an id the store has not
/// caught up with.
pub(super) async fn gate(
    State(state): State<JellyfinState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !state.settings.jellyfin().enabled {
        return error(StatusCode::NOT_FOUND);
    }
    if !state.ids.ready() {
        return ApiError::unavailable(Unavailable::ServerBusy, "jellyfin ids are not loaded yet")
            .into_response();
    }
    next.run(request).await
}

/// The address of this API as `request` reached it ([`base_address`]) plus
/// `/jellyfin`. `LocalAddress` is required, so a request that names no host
/// gets `http://localhost` and the base path.
pub(super) fn local_address(state: &JellyfinState, request: &Request<Body>) -> String {
    let base = base_address(request, &state.listener, &state.settings)
        .unwrap_or_else(|| format!("http://localhost{}", state.listener.base_path));
    format!("{base}/jellyfin")
}

pub(super) fn builder<'a>(state: &'a JellyfinState) -> Builder<'a> {
    Builder::new(&state.ids, &state.server_id)
}
