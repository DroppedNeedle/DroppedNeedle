//! Compat HTTP edge: the Subsonic axum adapter plus the shared layers.
//!
//! Layers on the compat routers, outermost first: case-insensitive path
//! canonicalization (before routing), CORS (`*`, creds off, preflight
//! short-circuit pre-auth), and rate limits (media and artwork exempt, a
//! public-IP bucket for unauthenticated routes, principal buckets for
//! signed-in callers, auth-failure backoff). Auth itself lives inside the
//! protocol paths (Subsonic `dispatch`, Jellyfin handlers), never in the
//! `/api` session middleware: compat mounts outside it.
//!
//! Rejects match v2 exactly: Subsonic answers a failed envelope code 0
//! ("Rate limit exceeded") over HTTP 200 plus `Retry-After`; Jellyfin
//! answers an empty 429 plus `Retry-After`.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, RawQuery, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::auth::compat_auth::jellyfin::{JellyfinPasswordStore, JellyfinRequest, extract_token};
use crate::auth::session::middleware::TrustedProxies;
use crate::client_ip::ClientIp;
use crate::compat::settings::LiveSettings;
use crate::compat::shared::{auth, cors, path_case, ratelimit, redact};
use crate::compat::subsonic::auth::Principal;
use crate::compat::subsonic::error::SubsonicError;
use crate::compat::subsonic::params::{MAX_REQUEST_PARAMETER_BYTES, SubsonicParameters};
use crate::compat::subsonic::value::{Rendered, SubsonicFormat, parse_format, render_error};
use crate::compat::subsonic::{
    AudioBackend, Request as SubsonicRequest, Settings, Store, Verifier, classify, dispatch_with,
    normalize_endpoint,
};

/// Jellyfin route templates (registered casing) for path canonicalization.
/// Mirrors `jellyfin::router` registration; the wiring test pins at
/// least the lowercase-login route so drift shows up as a failure, not a
/// silent 404.
pub const JELLYFIN_TEMPLATES: &[&str] = &[
    "/jellyfin/System/Info/Public",
    "/jellyfin/System/Info",
    "/jellyfin/QuickConnect/Enabled",
    "/jellyfin/Sessions/Logout",
    "/jellyfin/Users/AuthenticateByName",
    "/jellyfin/Users/Me",
    "/jellyfin/Users/{user_id}",
    "/jellyfin/Users/{user_id}/Views",
    "/jellyfin/UserViews",
    "/jellyfin/Users/{user_id}/Items",
    "/jellyfin/Items",
    "/jellyfin/UserItems/Latest",
    "/jellyfin/Users/{user_id}/Items/Latest",
    "/jellyfin/Artists",
    "/jellyfin/Artists/AlbumArtists",
    "/jellyfin/Genres",
    "/jellyfin/MusicGenres",
    "/jellyfin/Items/Filters",
    "/jellyfin/Users/{user_id}/Items/Filters",
    "/jellyfin/Users/{user_id}/Items/{item_id}",
    "/jellyfin/Items/{item_id}",
    "/jellyfin/Items/{item_id}/Images/{image_type}",
    "/jellyfin/Items/{item_id}/Images/{image_type}/{index}",
    "/jellyfin/Audio/{item_id}/{tail}",
    "/jellyfin/Items/{item_id}/File",
    "/jellyfin/Items/{item_id}/PlaybackInfo",
    "/jellyfin/Users/{user_id}/FavoriteItems/{item_id}",
    "/jellyfin/UserFavoriteItems/{item_id}",
    "/jellyfin/Users/{user_id}/PlayedItems/{item_id}",
    "/jellyfin/UserPlayedItems/{item_id}",
    "/jellyfin/Sessions/Playing",
    "/jellyfin/Sessions/Playing/Progress",
    "/jellyfin/Sessions/Playing/Stopped",
    "/jellyfin/Sessions/Playing/Ping",
    "/jellyfin/Sessions/Capabilities/Full",
    "/jellyfin/Playlists",
    "/jellyfin/Playlists/{playlist_id}",
    "/jellyfin/Playlists/{playlist_id}/Items",
    "/jellyfin/Playlists/{playlist_id}/Items/{entry_id}/Move/{new_index}",
    "/jellyfin/Items/{item_id}/Similar",
    "/jellyfin/Items/{item_id}/InstantMix",
    "/jellyfin/Artists/{item_id}/InstantMix",
];

/// Subsonic route templates for path canonicalization.
pub const SUBSONIC_TEMPLATES: &[&str] = &["/subsonic/rest/{endpoint}"];

/// All compat templates in one list for the case layer.
pub fn compat_templates() -> Vec<&'static str> {
    let mut out = Vec::with_capacity(SUBSONIC_TEMPLATES.len() + JELLYFIN_TEMPLATES.len());
    out.extend_from_slice(SUBSONIC_TEMPLATES);
    out.extend_from_slice(JELLYFIN_TEMPLATES);
    out
}

/// Stamp the compat CORS headers on a response. Also used by the app
/// fallbacks so native 404/405s on compat paths are indistinguishable
/// from disabled-protocol 404s (no enablement enumeration via CORS).
pub fn stamp_cors(response: &mut Response) {
    for (name, value) in cors::HEADERS {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(name.as_bytes()),
            axum::http::HeaderValue::from_str(value),
        ) {
            response.headers_mut().insert(name, value);
        }
    }
}

/// Compat fallback redispatch, called from the app's 404/405 fallbacks:
/// preflights short-circuit 204 (OPTIONS matches no route, so the CORS
/// layer never sees them), and case-variant compat paths (Feishin posts
/// lowercase Jellyfin paths) rewrite to registered casing and redispatch
/// into the compat router. Returns `None` for non-compat paths so the
/// native fallback answers.
///
/// A middleware layer cannot do this: axum layers run after route
/// matching, so a rewritten URI never re-matches. The fallback only fires
/// when nothing matched, which is exactly the rewrite case.
pub async fn fallback_redispatch(router: &axum::Router, request: Request) -> Option<Response> {
    use tower::ServiceExt as _;

    let path = request.uri().path().to_owned();
    let method = request.method().to_string();
    if cors::is_preflight(&method, &path) {
        let mut response = StatusCode::NO_CONTENT.into_response();
        *response.status_mut() =
            StatusCode::from_u16(cors::PREFLIGHT_STATUS).unwrap_or(StatusCode::NO_CONTENT);
        stamp_cors(&mut response);
        return Some(response);
    }
    // Case-insensitive gate: `/SUBSONIC/...` must redispatch exactly
    // like `/subsonic/...` (the case-sensitive CORS spelling would strand
    // it on the native 404).
    if !path_case::is_compat_path(&path) {
        return None;
    }
    let canon = path_case::canonicalize(&compat_templates(), &path)?;
    let target = match request.uri().query() {
        Some(query) => format!("{canon}?{query}"),
        None => canon,
    };
    let uri = target.parse().ok()?;
    let (mut parts, body) = request.into_parts();
    parts.uri = uri;
    let rewritten = Request::from_parts(parts, body);
    router.clone().oneshot(rewritten).await.ok()
}

/// CORS layer: every compat response carries `*` with creds off.
/// Preflights never reach this layer (OPTIONS matches no route); the
/// fallback redispatch short-circuits them 204 pre-auth.
pub async fn cors_layer(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    stamp_cors(&mut response);
    response
}

/// Object-safe token → user lookup for limit labeling (the
/// [`JellyfinPasswordStore`] trait is not object-safe, and axum middleware
/// fns must be non-generic, so this erases the store type).
pub trait LabelLookup: Send + Sync {
    /// User id for a token, or `None` when unknown or the store fails.
    fn user_for_token<'a>(
        &'a self,
        token: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Option<String>>;
}

/// [`LabelLookup`] over any Jellyfin password store.
#[derive(Clone)]
pub struct StoreLabels<S> {
    passwords: S,
}

impl<S> StoreLabels<S> {
    /// Wrap the store.
    pub fn new(passwords: S) -> Self {
        Self { passwords }
    }
}

impl<S: JellyfinPasswordStore> LabelLookup for StoreLabels<S> {
    fn user_for_token<'a>(
        &'a self,
        token: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            self.passwords
                .user_for_token(token)
                .await
                .ok()
                .flatten()
                .map(|user| user.id)
        })
    }
}

/// Rate-limit layer state: the shared buckets, a monotonic clock origin,
/// the token lookup for principal labeling, and the Subsonic settings for
/// shaped rejects.
#[derive(Clone)]
pub struct CompatLimits {
    /// Shared buckets.
    pub limits: Arc<Mutex<ratelimit::CompatRateLimits>>,
    /// Clock origin for bucket timestamps.
    pub started: Arc<Instant>,
    /// Token → user lookup for Jellyfin principal buckets.
    pub labels: Arc<dyn LabelLookup>,
    /// Server name/version for Subsonic limit envelopes, read per request.
    pub settings: LiveSettings,
    /// Peers whose `X-Forwarded-For` names the client; loopback by default.
    pub trusted_proxies: TrustedProxies,
}

impl CompatLimits {
    /// Wrap the buckets, the label lookup, and the settings.
    pub fn new(labels: Arc<dyn LabelLookup>, settings: impl Into<LiveSettings>) -> Self {
        Self {
            limits: Arc::new(Mutex::new(ratelimit::CompatRateLimits::new())),
            started: Arc::new(Instant::now()),
            labels,
            settings: settings.into(),
            trusted_proxies: TrustedProxies::default(),
        }
    }

    /// Key buckets and lockouts by the client behind these proxies
    /// (`TRUSTED_PROXY_IPS`).
    #[must_use]
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.trusted_proxies = trusted;
        self
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ratelimit::CompatRateLimits> {
        self.limits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Client ip for limit labeling: the trusted-proxy verdict over the TCP
/// peer, else `unknown` when served without connect info.
fn limit_ip(request: &Request, trusted: &TrustedProxies) -> Option<IpAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| crate::client_ip::client_ip(info.0, request.headers(), trusted))
}

/// Exact-spelling query lookup for the two Jellyfin auth keys (v2 reads
/// case-sensitive params: only `ApiKey` and `api_key`).
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

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Sniff the envelope format from raw params the way `dispatch` does: a
/// unique valid `f` value only (limit rejects render before strict
/// decoding, so they must not fail on a malformed `f`).
fn sniff_format(raw_query: Option<&str>) -> (SubsonicFormat, Option<String>) {
    let mut format = SubsonicFormat::Xml;
    let mut callback = None;
    let pairs = raw_query.unwrap_or("").split('&').collect::<Vec<_>>();
    let values = |key: &str| {
        pairs
            .iter()
            .filter_map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (name == key).then_some(value)
            })
            .collect::<Vec<_>>()
    };
    if let [sniffed] = values("f").as_slice()
        && let Ok(parsed) = parse_format(Some(sniffed))
    {
        format = parsed;
    }
    if let [sniffed] = values("callback").as_slice()
        && sniffed.len() <= 128
    {
        callback = Some(sniffed.to_string());
    }
    (format, callback)
}

/// Protocol-shaped limit reject with `Retry-After`.
fn limit_reject(
    path: &str,
    raw_query: Option<&str>,
    retry_after: u64,
    settings: &Settings,
) -> Response {
    let mut response = if path.to_lowercase().starts_with("/subsonic") {
        let (format, callback) = sniff_format(raw_query);
        let rendered = render_error(
            0,
            "Rate limit exceeded",
            format,
            callback.as_deref(),
            &settings.server_name,
            &settings.server_version,
        );
        rendered_response(&rendered, false)
    } else {
        StatusCode::TOO_MANY_REQUESTS.into_response()
    };
    if let Ok(value) = retry_after.to_string().parse() {
        response
            .headers_mut()
            .insert(axum::http::header::RETRY_AFTER, value);
    }
    response
}

/// Rate-limit layer. Every request first takes the auth-failure lockout
/// pre-check (else `stream`/`download` would be a brute-force bypass around
/// backoff). Media and artwork then skip the token buckets. The public-IP
/// bucket applies only to unauthenticated routes: the Subsonic public
/// endpoint and the anonymous Jellyfin routes (login, public info). Signed-in
/// Jellyfin callers take their principal bucket here (labeled by a token
/// lookup with no use stamp); Subsonic principal buckets run post-verify in
/// the adapter. Jellyfin 401s feed backoff on the way out (every Jellyfin
/// 401 is an auth denial); Subsonic denials record in the adapter.
pub async fn limits_layer(
    State(state): State<CompatLimits>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().to_string();
    let exempt = ratelimit::is_media_request(&path) || ratelimit::is_artwork_request(&path);
    let client = limit_ip(&request, &state.trusted_proxies);
    // The Subsonic adapter keys its post-verify buckets on the same value.
    if let Some(client) = client {
        request.extensions_mut().insert(ClientIp(client));
    }
    let ip = client.map_or_else(|| "unknown".to_owned(), |client| client.to_string());
    let now = state.now();
    let raw_query = request.uri().query().map(str::to_owned);
    let reject = |retry_after: u64| {
        limit_reject(
            &path,
            raw_query.as_deref(),
            retry_after,
            &state.settings.subsonic(),
        )
    };
    if let Some(retry_after) = auth::auth_locked_out(&mut state.lock(), &ip, now) {
        return reject(retry_after);
    }
    let is_jellyfin = path.to_lowercase().starts_with("/jellyfin");
    if !exempt {
        let anonymous = if is_jellyfin {
            auth::jellyfin_is_anonymous(&method, &path)
        } else {
            auth::subsonic_is_public(path.rsplit('/').next().unwrap_or(""))
        };
        if anonymous {
            if let Some(retry_after) = state.lock().public_retry_after(&ip, now) {
                return reject(retry_after);
            }
        } else if is_jellyfin
            && let Some(token) = label_token(&request)
            && let Some(user_id) = state.labels.user_for_token(&token).await
        {
            let principal = auth::principal_label(Some(&user_id), &ip);
            let mutation = ratelimit::is_mutation_request(&method, &path);
            if let Some(retry_after) = state
                .lock()
                .principal_retry_after(&principal, mutation, now)
            {
                return reject(retry_after);
            }
        }
    }
    let response = next.run(request).await;
    if is_jellyfin && response.status() == StatusCode::UNAUTHORIZED {
        let _ = auth::record_auth_denial(&mut state.lock(), &ip, now);
    }
    response
}

/// Extract a Jellyfin request token for limit labeling. Synchronous: the
/// request borrow never crosses an await (`Request<Body>` is not `Sync`).
fn label_token(request: &Request) -> Option<String> {
    let headers = request.headers();
    let raw_query = request.uri().query();
    let probe = JellyfinRequest {
        authorization: header(headers, "authorization"),
        emby_authorization: header(headers, "x-emby-authorization"),
        emby_token: header(headers, "x-emby-token"),
        mediabrowser_token: header(headers, "x-mediabrowser-token"),
        query_apikey: query_exact(raw_query, "ApiKey"),
        query_api_key: query_exact(raw_query, "api_key"),
    };
    extract_token(&probe)
}

/// Map a rendered Subsonic outcome onto a response. Non-empty bodies
/// without a `Content-Length` stream unsized (transcodes: estimate off
/// means no length on the wire); empty bodies stay sized.
pub fn rendered_response(rendered: &Rendered, head_only: bool) -> Response {
    let status = StatusCode::from_u16(rendered.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    let live = rendered.stream.as_ref().and_then(|stream| stream.take());
    let mut sized = head_only || (rendered.body.is_empty() && live.is_none());
    for (name, value) in &rendered.headers {
        if name.eq_ignore_ascii_case("content-length") {
            sized = true;
        }
        builder = builder.header(name.as_str(), value.as_str());
    }
    if let Ok(content_type) = rendered.content_type.parse::<axum::http::HeaderValue>() {
        builder = builder.header(axum::http::header::CONTENT_TYPE, content_type);
    }
    let body = if head_only {
        if !sized {
            builder = builder.header(
                axum::http::header::CONTENT_LENGTH,
                rendered.body.len().to_string(),
            );
        }
        Body::empty()
    } else if let Some(chunks) = live {
        // Streamed audio; a Content-Length header, when set, is exact.
        Body::from_stream(chunks)
    } else if sized {
        Body::from(rendered.body.clone())
    } else {
        let bytes = rendered.body.clone();
        Body::from_stream(futures_util::stream::once(async move {
            Ok::<_, std::convert::Infallible>(bytes)
        }))
    };
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Subsonic router state: the dispatch seams plus the shared buckets.
#[derive(Clone)]
pub struct SubsonicState<V, S, B> {
    /// App-password verifier.
    pub verifier: V,
    /// Library store.
    pub store: S,
    /// Audio over the stream engine.
    pub audio: B,
    /// Server settings (kill switch, names, transcode policy), read per
    /// request.
    pub settings: LiveSettings,
    /// Shared buckets (same handle as the limits layer).
    pub limits: Arc<Mutex<ratelimit::CompatRateLimits>>,
    /// Clock origin for bucket timestamps.
    pub started: Arc<Instant>,
}

impl<V, S, B> SubsonicState<V, S, B> {
    /// Wrap the seams, buckets, and clock origin.
    pub fn new(
        verifier: V,
        store: S,
        audio: B,
        settings: impl Into<LiveSettings>,
        limits: Arc<Mutex<ratelimit::CompatRateLimits>>,
        started: Arc<Instant>,
    ) -> Self {
        Self {
            verifier,
            store,
            audio,
            settings: settings.into(),
            limits,
            started,
        }
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}

/// Build the `/subsonic`-mounted router. Unknown HTTP methods answer the
/// code-0 envelope (never the native 405).
pub fn subsonic_router<V, S, B>(state: SubsonicState<V, S, B>) -> axum::Router
where
    V: Verifier + 'static,
    S: Store + 'static,
    B: AudioBackend + 'static,
{
    axum::Router::new()
        .route(
            "/subsonic/rest/{endpoint}",
            get(subsonic_dispatch::<V, S, B>)
                .post(subsonic_dispatch::<V, S, B>)
                .head(subsonic_dispatch::<V, S, B>),
        )
        .method_not_allowed_fallback(subsonic_method_not_allowed::<V, S, B>)
        .with_state(state)
}

/// Unknown-method fallback: code 0, v2 message style.
async fn subsonic_method_not_allowed<V, S, B>(
    State(state): State<SubsonicState<V, S, B>>,
    request: Request,
) -> Response
where
    V: Verifier,
    S: Store,
    B: AudioBackend,
{
    let (format, callback) = sniff_format(request.uri().query());
    let settings = state.settings.subsonic();
    rendered_response(
        &render_error(
            0,
            "Unknown method",
            format,
            callback.as_deref(),
            &settings.server_name,
            &settings.server_version,
        ),
        false,
    )
}

/// Flatten a JSON object body into scalar params (reportPlayback only).
fn flatten_json_object(body: &[u8]) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in object {
        let text = match value {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Number(number) => Some(number.to_string()),
            serde_json::Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        };
        if let Some(text) = text {
            out.push((key.clone(), text));
        }
    }
    out
}

/// One Subsonic request: decode params, gate enablement, enforce limits,
/// verify (recording failures), dispatch, and render.
async fn subsonic_dispatch<V, S, B>(
    State(state): State<SubsonicState<V, S, B>>,
    Path(endpoint): Path<String>,
    RawQuery(raw_query): RawQuery,
    request: Request,
) -> Response
where
    V: Verifier,
    S: Store,
    B: AudioBackend,
{
    let (parts, body) = request.into_parts();
    let method = parts.method.to_string();
    let (format, callback) = sniff_format(raw_query.as_deref());
    let settings = &state.settings.subsonic();
    let envelope = |code: u8, message: &str| {
        rendered_response(
            &render_error(
                code,
                message,
                format,
                callback.as_deref(),
                &settings.server_name,
                &settings.server_version,
            ),
            method == "HEAD",
        )
    };
    // Enablement first: a disabled API leaks no method existence.
    if !settings.enabled {
        return envelope(0, "The Subsonic API is disabled on this server.");
    }
    let name = normalize_endpoint(&endpoint);
    let body_bytes = match axum::body::to_bytes(body, MAX_REQUEST_PARAMETER_BYTES + 1).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return envelope(10, "Required parameter is missing.");
        }
    };
    if body_bytes.len() > MAX_REQUEST_PARAMETER_BYTES {
        return envelope(10, "Required parameter is missing.");
    }
    let mut pairs = match crate::compat::subsonic::params::decode_pairs(
        raw_query.as_deref().unwrap_or("").as_bytes(),
    ) {
        Ok(pairs) => pairs,
        Err(_) => return envelope(10, "Invalid request parameter encoding"),
    };
    let content_type = header(&parts.headers, "content-type").unwrap_or("");
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if mime == "application/x-www-form-urlencoded" && !body_bytes.is_empty() {
        match crate::compat::subsonic::params::decode_pairs(&body_bytes) {
            Ok(mut form) => pairs.append(&mut form),
            Err(_) => return envelope(10, "Invalid request parameter encoding"),
        }
    } else if mime == "application/json" && name == "reportplayback" && !body_bytes.is_empty() {
        pairs.extend(flatten_json_object(&body_bytes));
    }
    if crate::compat::subsonic::params::check_limits(&pairs).is_err() {
        return envelope(10, "Required parameter is missing.");
    }
    let params = SubsonicParameters::new(pairs);
    let ip = parts
        .extensions
        .get::<ClientIp>()
        .map(|client| client.0.to_string())
        .unwrap_or_else(|| "unknown".to_owned());
    let now = state.now();
    // Principal buckets + backoff run here (post-verify); the lockout
    // pre-check and, for the public endpoint, the public bucket ran in the
    // limits layer.
    let principal = if name == crate::compat::subsonic::PUBLIC_ENDPOINT {
        None
    } else {
        let credentials = match classify(&params) {
            Ok(Some(credentials)) => credentials,
            Ok(None) => {
                let _ = auth::record_auth_denial(
                    &mut state
                        .limits
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                    &ip,
                    now,
                );
                return render_denied(
                    &SubsonicError::code_only(10),
                    &name,
                    format,
                    callback.as_deref(),
                    settings,
                    method == "HEAD",
                );
            }
            Err(denied) => {
                let _ = auth::record_auth_denial(
                    &mut state
                        .limits
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                    &ip,
                    now,
                );
                return render_denied(
                    &denied,
                    &name,
                    format,
                    callback.as_deref(),
                    settings,
                    method == "HEAD",
                );
            }
        };
        match state.verifier.verify(&credentials).await {
            Ok(principal) => {
                let label = auth::principal_label(Some(principal.user_id()), &ip);
                let path = format!("/subsonic/rest/{endpoint}");
                let exempt =
                    ratelimit::is_media_request(&path) || ratelimit::is_artwork_request(&path);
                let mutation = ratelimit::is_mutation_request(&method, &path);
                let retry_after = if exempt {
                    None
                } else {
                    state
                        .limits
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .principal_retry_after(&label, mutation, now)
                };
                if let Some(retry_after) = retry_after {
                    let mut response = envelope(0, "Rate limit exceeded");
                    if let Ok(value) = retry_after.to_string().parse() {
                        response
                            .headers_mut()
                            .insert(axum::http::header::RETRY_AFTER, value);
                    }
                    return response;
                }
                Some(principal)
            }
            Err(denied) => {
                // Code 0 is a server-side failure (the credential store
                // broke), not a wrong guess: it must not count toward the
                // caller's lockout.
                if denied.code != crate::auth::compat_auth::subsonic::GENERIC {
                    let _ = auth::record_auth_denial(
                        &mut state
                            .limits
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner()),
                        &ip,
                        now,
                    );
                }
                return render_denied(
                    &denied,
                    &name,
                    format,
                    callback.as_deref(),
                    settings,
                    method == "HEAD",
                );
            }
        }
    };
    let mut headers = HashMap::new();
    if let Some(range) = header(&parts.headers, "range") {
        headers.insert("Range".to_owned(), range.to_owned());
    }
    if let Some(content) = header(&parts.headers, "content-type") {
        headers.insert("Content-Type".to_owned(), content.to_owned());
    }
    let subsonic_request = SubsonicRequest {
        method: method.clone(),
        endpoint,
        params,
        headers,
        body: body_bytes.to_vec(),
        content_type: Some(content_type.to_owned()).filter(|mime| !mime.is_empty()),
        now_unix: None,
    };
    let rendered = dispatch_with(
        &state.verifier,
        &state.store,
        &state.audio,
        settings,
        &subsonic_request,
        principal,
    )
    .await;
    rendered_response(&rendered, method == "HEAD")
}

/// Render a pre-dispatch auth denial through the binary-vs-envelope split.
fn render_denied(
    denied: &SubsonicError,
    normalized_endpoint: &str,
    format: SubsonicFormat,
    callback: Option<&str>,
    settings: &Settings,
    head_only: bool,
) -> Response {
    let rendered =
        if crate::compat::subsonic::dispatch_uses_envelope(denied.code, normalized_endpoint) {
            render_error(
                denied.code,
                &denied.message,
                format,
                callback,
                &settings.server_name,
                &settings.server_version,
            )
        } else {
            crate::compat::subsonic::value::render_binary_error(denied.code, &denied.message)
        };
    rendered_response(&rendered, head_only)
}

/// Redact a compat request target for access logs. Every persisted
/// target routes through here (the journey-trace recorder is the wiring
/// proof: `trace_redacts_credentials_from_recorded_uris`), and any future
/// target logging must do the same. Live request spans stay path-only
/// (never the query) per the middleware posture, so secrets never reach
/// the logs on either path.
pub fn redact_target(target: &str) -> String {
    redact::redact_request_target(target)
}
