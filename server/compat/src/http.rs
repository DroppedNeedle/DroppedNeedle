//! Compat HTTP edge: the Subsonic axum adapter plus the shared layers.
//!
//! Before routing, [`preflight`] answers preflights pre-auth and
//! [`canonicalize_case`] fixes path case. Layers on the compat routers,
//! outermost first: CORS (`*`, creds off), and rate limits (media and
//! artwork exempt, a public-IP bucket for unauthenticated routes,
//! principal buckets for signed-in callers, auth-failure backoff). Auth itself lives inside the
//! protocol paths (the Subsonic handler, the Jellyfin sign-in layer), never
//! in the `/api` session middleware: compat mounts outside it.
//!
//! Rejects match v2 exactly: Subsonic answers a failed envelope code 0
//! ("Rate limit exceeded") over HTTP 200 plus `Retry-After`; Jellyfin
//! answers an empty 429 plus `Retry-After`.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{ConnectInfo, MatchedPath, Path, RawQuery, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::adapters::principal::SubsonicVerifier;
use crate::adapters::subsonic_store::SubsonicStore;
use crate::jellyfin::QueryTokens;
use crate::settings::LiveSettings;
use crate::setup::CompatAudio;
use crate::shared::address::{ListenerConfig, base_address};
use crate::shared::{auth, cors, path_case, ratelimit, redact};
use crate::subsonic::error::SubsonicError;
use crate::subsonic::params::{MAX_REQUEST_PARAMETER_BYTES, SubsonicParameters};
use crate::subsonic::picture;
use crate::subsonic::value::{
    Rendered, SubsonicFormat, parse_format, render_error, render_unknown_method, sniff_format,
};
use crate::subsonic::{Ctx, Settings, classify, endpoint, failure, normalize_endpoint};
use droppedneedle_auth::compat_auth::jellyfin::extract_token;
use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_auth::session::client_ip::ClientIp;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::mount::CompatRoutes;

/// Stamp the compat CORS headers on a response. The app also stamps every
/// answer on a compat path, so native 404/405s there are indistinguishable
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

// The app calls the two functions below before routing. A layer cannot do
// their work: axum layers run after route matching, so a rewritten URI
// would never match again, and the app's 405 fallback takes OPTIONS before
// any compat layer sees it.

/// The answer to a compat preflight, 204 before auth, or `None` when
/// `request` is not one.
pub fn preflight(request: &Request) -> Option<Response> {
    if !cors::is_preflight(request.method().as_str(), request.uri().path()) {
        return None;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    *response.status_mut() =
        StatusCode::from_u16(cors::PREFLIGHT_STATUS).unwrap_or(StatusCode::NO_CONTENT);
    stamp_cors(&mut response);
    Some(response)
}

/// Rewrite a case-variant compat path (Feishin posts lowercase Jellyfin
/// paths) to its casing in `templates`, the ones the compat routes
/// recorded, keeping the query.
pub fn canonicalize_case(templates: &[&str], request: &mut Request) {
    let Some(canon) = path_case::canonicalize(templates, request.uri().path()) else {
        return;
    };
    let target = match request.uri().query() {
        Some(query) => format!("{canon}?{query}"),
        None => canon,
    };
    if let Ok(uri) = target.parse() {
        *request.uri_mut() = uri;
    }
}

/// CORS layer: every compat response carries `*` with creds off.
/// Preflights never reach this layer: [`preflight`] answers them.
pub async fn cors_layer(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    stamp_cors(&mut response);
    response
}

/// Rate-limit layer state: the shared buckets, a monotonic clock origin,
/// the app passwords for principal labeling, and the Subsonic settings for
/// shaped rejects.
#[derive(Clone)]
pub struct CompatLimits {
    /// Shared buckets.
    pub limits: Arc<Mutex<ratelimit::CompatRateLimits>>,
    /// Clock origin for bucket timestamps.
    pub started: Arc<Instant>,
    /// App passwords, to find the user behind a Jellyfin token for its
    /// principal bucket.
    pub passwords: ProdCompatPasswords,
    /// Server name/version for Subsonic limit envelopes, read per request.
    pub settings: LiveSettings,
    /// Peers whose `X-Forwarded-For` names the client; loopback by default.
    pub trusted_proxies: TrustedProxies,
}

impl CompatLimits {
    /// Wrap the buckets, the app passwords, and the settings.
    pub fn new(passwords: ProdCompatPasswords, settings: LiveSettings) -> Self {
        Self {
            limits: Arc::new(Mutex::new(ratelimit::CompatRateLimits::new())),
            started: Arc::new(Instant::now()),
            passwords,
            settings,
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

    /// The user behind a Jellyfin token, or `None` when it is unknown or
    /// the store fails (the request then signs in or fails on its own).
    async fn user_for_token(&self, token: &str) -> Option<String> {
        self.passwords
            .user_for_token(token)
            .await
            .ok()
            .flatten()
            .map(|user| user.id)
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
        .map(|info| {
            droppedneedle_auth::session::client_ip::client_ip(info.0, request.headers(), trusted)
        })
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The envelope format and callback from the raw query, for answers made
/// before the parameters decode (limit rejects and malformed requests
/// must not fail on a malformed `f`).
fn sniff_query(raw_query: Option<&str>) -> (SubsonicFormat, Option<String>) {
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
    sniff_format(&values("f"), &values("callback"))
}

/// Protocol-shaped limit reject with `Retry-After`. The open picture
/// address answers an empty 429: a picture loader would keep an envelope
/// as a broken picture.
fn limit_reject(
    path: &str,
    raw_query: Option<&str>,
    retry_after: u64,
    settings: &Settings,
) -> Response {
    let low = path.to_lowercase();
    let mut response = if low.starts_with("/subsonic") && low != picture::PATH {
        let (format, callback) = sniff_query(raw_query);
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

/// The answer to any 5xx on a compat path, written by the app's request
/// boundary. It keeps the status and `Retry-After`, adds compat CORS and
/// drops every other header and the body. Subsonic answers its failed
/// envelope (code 0, the fixed message) in the format the query asks for,
/// with no body for HEAD; the open picture address and Jellyfin answer
/// status only, as every Jellyfin error here does. The query is read for
/// `f` and `callback` only.
pub fn server_error_response(
    path: &str,
    head_only: bool,
    raw_query: Option<&str>,
    status: StatusCode,
    retry_after: Option<axum::http::HeaderValue>,
    settings: &Settings,
) -> Response {
    let low = path.to_lowercase();
    let mut response = if low.starts_with("/subsonic") && low != picture::PATH {
        let (format, callback) = sniff_query(raw_query);
        let rendered = render_error(
            crate::subsonic::error::GENERIC,
            droppedneedle_kernel::error::FIXED_INTERNAL_MESSAGE,
            format,
            callback.as_deref(),
            &settings.server_name,
            &settings.server_version,
        );
        rendered_response(&rendered, head_only)
    } else {
        Response::new(Body::empty())
    };
    *response.status_mut() = status;
    if let Some(value) = retry_after {
        response
            .headers_mut()
            .insert(axum::http::header::RETRY_AFTER, value);
    }
    stamp_cors(&mut response);
    response
}

/// Rate-limit layer. Every request first takes the auth-failure lockout
/// pre-check (else `stream`/`download` would be a brute-force bypass around
/// backoff). Media and artwork then skip the token buckets. The public-IP
/// bucket applies only to unauthenticated routes: the Subsonic public
/// endpoint and the public rows of the Jellyfin route table. Signed-in
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
    let ip = ratelimit::client_key(client);
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
            // The template axum matched, so the route table and the router
            // never disagree on which route this is.
            request
                .extensions()
                .get::<MatchedPath>()
                .is_some_and(|matched| auth::jellyfin_is_anonymous(&method, matched.as_str()))
        } else {
            auth::subsonic_is_public(path.rsplit('/').next().unwrap_or(""))
        };
        if anonymous {
            if let Some(retry_after) = state.lock().public_retry_after(&ip, now) {
                return reject(retry_after);
            }
        } else if is_jellyfin
            && let Some(token) = label_token(&request)
            && let Some(user_id) = state.user_for_token(&token).await
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
    // A malformed query token has no label: sign-in answers it 401.
    let query = QueryTokens::read(request.uri().query()).ok()?;
    extract_token(&query.request(request.headers()))
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

/// Subsonic router state: the verifier, store and audio plus the shared
/// buckets.
#[derive(Clone)]
pub struct SubsonicState {
    /// App-password verifier.
    pub verifier: SubsonicVerifier,
    /// Library store.
    pub store: SubsonicStore,
    /// Audio over the stream engine.
    pub audio: CompatAudio,
    /// Server settings (kill switch, names, transcode policy), read per
    /// request.
    pub settings: LiveSettings,
    /// Shared buckets (same handle as the limits layer).
    pub limits: Arc<Mutex<ratelimit::CompatRateLimits>>,
    /// Clock origin for bucket timestamps.
    pub started: Arc<Instant>,
    /// How apps reach this server, for the address in picture links.
    pub listener: ListenerConfig,
    /// Wall time, stamped on each request.
    pub clock: Arc<dyn Clock>,
}

impl SubsonicState {
    /// Wrap the verifier, store, audio, buckets, clock origin, listener
    /// and wall clock.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        verifier: SubsonicVerifier,
        store: SubsonicStore,
        audio: CompatAudio,
        settings: LiveSettings,
        limits: Arc<Mutex<ratelimit::CompatRateLimits>>,
        started: Arc<Instant>,
        listener: ListenerConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            verifier,
            store,
            audio,
            settings,
            limits,
            started,
            listener,
            clock,
        }
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }
}

/// The `/subsonic` routes. Unknown HTTP methods answer the code-0
/// envelope (never the native 405).
pub fn subsonic_routes(state: SubsonicState) -> CompatRoutes {
    CompatRoutes::new()
        .at(
            "/subsonic/rest/{endpoint}",
            get(subsonic_dispatch)
                .post(subsonic_dispatch)
                .head(subsonic_dispatch),
        )
        .at(picture::PATH, get(subsonic_picture).head(subsonic_picture))
        .method_not_allowed_fallback(subsonic_method_not_allowed)
        .with_state(state)
}

/// Unknown-method fallback: code 0, v2 message style.
async fn subsonic_method_not_allowed(
    State(state): State<SubsonicState>,
    request: Request,
) -> Response {
    let (format, callback) = sniff_query(request.uri().query());
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

/// The open picture address: no sign-in, only a signed album or artist
/// link (see [`picture`]). Every refusal is the same empty 404, and none
/// counts as a failed sign-in.
async fn subsonic_picture(
    State(state): State<SubsonicState>,
    RawQuery(raw_query): RawQuery,
    request: Request,
) -> Response {
    let head = request.method() == axum::http::Method::HEAD;
    match picture::open(
        &state.store,
        &state.settings.subsonic(),
        raw_query.as_deref(),
    )
    .await
    {
        picture::Opened::Picture(rendered) => rendered_response(&rendered, head),
        picture::Opened::Closed => StatusCode::NOT_FOUND.into_response(),
        picture::Opened::Failed => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
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

/// Decode the query, a form body, and a JSON body for reportPlayback into
/// one parameter list. A body over the cap, bad percent-encoding or a
/// parameter over its limits is code 10.
async fn decode_params(
    body: Body,
    raw_query: Option<&str>,
    content_type: &str,
    name: &str,
) -> Result<(SubsonicParameters, Vec<u8>), SubsonicError> {
    let refused = || SubsonicError::code_only(10);
    let body = axum::body::to_bytes(body, MAX_REQUEST_PARAMETER_BYTES + 1)
        .await
        .map_err(|_| refused())?;
    if body.len() > MAX_REQUEST_PARAMETER_BYTES {
        return Err(refused());
    }
    let bad_encoding = |_| SubsonicError::new(10, "Invalid request parameter encoding");
    let mut pairs = crate::subsonic::params::decode_pairs(raw_query.unwrap_or("").as_bytes())
        .map_err(bad_encoding)?;
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if mime == "application/x-www-form-urlencoded" && !body.is_empty() {
        pairs.append(&mut crate::subsonic::params::decode_pairs(&body).map_err(bad_encoding)?);
    } else if mime == "application/json" && name == "reportplayback" && !body.is_empty() {
        pairs.extend(flatten_json_object(&body));
    }
    crate::subsonic::params::check_limits(&pairs).map_err(|_| refused())?;
    Ok((SubsonicParameters::new(pairs), body.to_vec()))
}

/// One Subsonic request: decode params, read the format once, gate
/// enablement, verify (enforcing principal limits and recording
/// failures), then run the method from the endpoint table and render.
async fn subsonic_dispatch(
    State(state): State<SubsonicState>,
    Path(endpoint_name): Path<String>,
    RawQuery(raw_query): RawQuery,
    request: Request,
) -> Response {
    let picture_base = base_address(&request, &state.listener, &state.settings);
    let (parts, body) = request.into_parts();
    let method = parts.method.to_string();
    let head_only = method == "HEAD";
    let settings = &state.settings.subsonic();
    let name = normalize_endpoint(&endpoint_name);
    let content_type = header(&parts.headers, "content-type").unwrap_or("");
    let decoded = decode_params(body, raw_query.as_deref(), content_type, &name).await;
    // The format every answer below is written in: a unique valid `f`
    // among the decoded parameters, or in the raw query when they did not
    // decode.
    let (format, callback) = match &decoded {
        Ok((params, _)) => sniff_format(&params.all("f"), &params.all("callback")),
        Err(_) => sniff_query(raw_query.as_deref()),
    };
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
            head_only,
        )
    };
    let fail = |err: SubsonicError| {
        let (rendered, fault) = failure(err, &name, format, callback.as_deref(), settings);
        let mut response = rendered_response(&rendered, head_only);
        if let Some(fault) = fault {
            response.extensions_mut().insert(fault);
        }
        response
    };
    // Enablement first: a disabled API leaks no method existence.
    if !settings.enabled {
        return envelope(0, "The Subsonic API is disabled on this server.");
    }
    let (params, body) = match decoded {
        Ok(decoded) => decoded,
        Err(err) => return envelope(err.code, &err.message),
    };
    let method_entry = endpoint(&name);
    let ip = ratelimit::client_key(parts.extensions.get::<ClientIp>().map(|client| client.0));
    let now = state.now();
    let record_denial = || {
        let _ = auth::record_auth_denial(
            &mut state
                .limits
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            &ip,
            now,
        );
    };
    // Principal buckets + backoff run here (post-verify); the lockout
    // pre-check and, for the public endpoint, the public bucket ran in the
    // limits layer.
    let principal = if method_entry.is_some_and(|entry| entry.public) {
        None
    } else {
        let credentials = match classify(&params) {
            Ok(Some(credentials)) => credentials,
            Ok(None) => {
                record_denial();
                return fail(SubsonicError::code_only(10));
            }
            Err(denied) => {
                record_denial();
                return fail(denied);
            }
        };
        match state.verifier.verify(&credentials).await {
            Ok(principal) => {
                let label = auth::principal_label(Some(principal.user_id()), &ip);
                let path = format!("/subsonic/rest/{endpoint_name}");
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
                if denied.code != droppedneedle_auth::compat_auth::subsonic::GENERIC {
                    record_denial();
                }
                return fail(denied);
            }
        }
    };
    // Strict decoding of `f` and `callback`: a repeat or a bad value is
    // code 10 (v2 reads a unique valid value only).
    let strict = params
        .string_max("f", Some("xml"), 16)
        .and_then(|requested| parse_format(requested.as_deref()))
        .and_then(|_| params.string_max("callback", None, 128));
    if let Err(err) = strict {
        return fail(err);
    }
    let Some(method_entry) = method_entry else {
        return rendered_response(&render_unknown_method(), head_only);
    };
    let scoped_store = principal
        .as_ref()
        .map(|principal| state.store.for_caller(principal.user_id()));
    let scoped_audio = principal
        .as_ref()
        .map(|principal| state.audio.for_caller(principal.user_id()));
    let ctx = Ctx {
        endpoint_name: name.clone(),
        params,
        principal,
        store: scoped_store.as_ref().unwrap_or(&state.store),
        audio: scoped_audio.as_ref().unwrap_or(&state.audio),
        settings,
        format,
        callback: callback.clone(),
        method: method.clone(),
        range: header(&parts.headers, "range").map(str::to_owned),
        content_type: Some(content_type.to_owned()).filter(|mime| !mime.is_empty()),
        body,
        now_unix: state.clock.now_ms() as f64 / 1000.0,
        picture_base,
    };
    match crate::subsonic::run(method_entry, &ctx).await {
        Ok(rendered) => rendered_response(&rendered, head_only),
        Err(err) => fail(err),
    }
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
