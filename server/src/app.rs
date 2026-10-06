//! Router assembly. Registration order matters and is documented here:
//! specific routes first, then the conditional test hooks, then the 404 and
//! 405 fallbacks. The request-scope layer wraps everything including the
//! fallbacks.
//!
//! The `/api/v3` router carries two middleware layers, innermost first:
//! the rate limiter, then the deny-by-default session gate, so the limiter
//! keys signed-in callers by user and everyone else by client address. The
//! debug-only CORS layer mounts outermost and only when the constructor
//! enables it.
//!
//! The wrapped trio nests under `/api/v3` on its own router outside the
//! session gate: its `X-Wrapped-API-Key` extractor is the only credential,
//! and allowlisting those paths would wrongly make them public. It keeps
//! the shared rate limiter.
//!
//! The compat shims (`/subsonic`, `/jellyfin`) merge outside the session
//! gate too: they carry their own app-password auth plus the compat layers
//! (CORS, limits), and never inherit `/api` session auth. Case-variant
//! compat paths and preflights match no route, so the 404/405 fallbacks
//! redispatch them into the compat router (preflight 204, canonical
//! rewrite) before answering natively. Native OPTIONS preflights on
//! existing paths land in the 405 fallback too, so under debug CORS it
//! answers them directly with the pinned localhost headers.
//!
//! The web UI is the last stop of the 404 fallback: after the compat
//! redispatch, a GET that is not an API path gets a static file or the
//! SPA `index.html`. Everything above sits under `BASE_PATH` when one is
//! set: the prefix is stripped once, up front, so routes, the session gate
//! and the UI all see the same base-relative path, and requests outside
//! the base get the 404 envelope.

use axum::{Router, extract::Request, middleware, response::IntoResponse, routing::get};
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};

use crate::{
    auth::session::{middleware::require_session, rate_limit::rate_limit},
    compat::http::fallback_redispatch,
    handlers,
    middleware::request_scope,
    state::AppState,
    web::WebUi,
};

/// Build the application router without a web UI.
pub fn create_app(state: AppState) -> Router {
    create_app_with_web(state, None)
}

/// Build the full application router, serving `web` as the fallback when
/// given.
pub fn create_app_with_web(state: AppState, web: Option<WebUi>) -> Router {
    let base_path = state.config.base_path.clone();
    let outside_base = Router::new()
        .fallback(handlers::fallback_404)
        .layer(middleware::from_fn_with_state(state.clone(), request_scope));
    let app = base_relative_app(state, web);
    if base_path.is_empty() {
        app
    } else {
        // Strip the base by hand rather than with `Router::nest`, which
        // does not route `{base}/` (the SPA's own root URL) into the app.
        Router::new().fallback(move |request: Request| async move {
            use tower::ServiceExt as _;

            let target = match base_relative_uri(&base_path, request.uri()) {
                Some(uri) => {
                    let (mut parts, body) = request.into_parts();
                    parts.uri = uri;
                    app.oneshot(Request::from_parts(parts, body)).await
                }
                None => outside_base.oneshot(request).await,
            };
            match target {
                Ok(response) => response,
                Err(never) => match never {},
            }
        })
    }
}

/// `uri` with the base path removed, on a segment boundary: `{base}` and
/// `{base}/` become `/`, `{base}/x` becomes `/x`. `None` when the request
/// is outside the base.
fn base_relative_uri(base_path: &str, uri: &axum::http::Uri) -> Option<axum::http::Uri> {
    let rest = uri.path().strip_prefix(base_path)?;
    let path = match rest {
        "" => "/",
        rest if rest.starts_with('/') => rest,
        _ => return None,
    };
    let target = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    target.parse().ok()
}

/// Every route, fallback and layer, at base-relative paths.
fn base_relative_app(state: AppState, web: Option<WebUi>) -> Router {
    let mut v3 = Router::new()
        .nest(
            "/api/v3",
            state
                .auth
                .router()
                .merge(state.reads.gated_router())
                .merge(state.media.gated_router())
                .merge(state.acquire.gated_router())
                .merge(state.library.gated_router())
                .merge(state.admin.gated_router())
                .merge(state.plugins.gated_router())
                .merge(state.settings.gated_router())
                .merge(state.settings.me_router())
                .merge(state.jobs.settings_router()),
        )
        .merge(state.reads.search_router())
        .layer(middleware::from_fn_with_state(
            state.auth.limits.clone(),
            rate_limit,
        ))
        .layer(middleware::from_fn_with_state(
            state.auth.session_auth.clone(),
            require_session,
        ));
    if state.config.debug_cors {
        v3 = v3.layer(debug_cors_layer());
    }
    let wrapped = Router::new()
        .nest("/api/v3", state.reads.wrapped_router())
        .layer(middleware::from_fn_with_state(
            state.auth.limits.clone(),
            rate_limit,
        ));
    // Spotify OAuth callback: outside the session gate (state-token
    // identified), still rate-limited like the wrapped trio.
    let acquire_public = Router::new()
        .nest("/api/v3", state.acquire.callback_router())
        .layer(middleware::from_fn_with_state(
            state.auth.limits.clone(),
            rate_limit,
        ));
    // The OIDC callback at its v2 path, so a migrated identity provider
    // keeps redirecting somewhere real. Public like the v3 one.
    let oidc_legacy = state
        .auth
        .legacy_oidc_router()
        .layer(middleware::from_fn_with_state(
            state.auth.limits.clone(),
            rate_limit,
        ));
    let app = Router::new()
        .route(
            "/health",
            get(handlers::health).with_state(state.admin.checkpoint.clone()),
        )
        .route("/openapi.json", get(handlers::openapi_json))
        .merge(v3)
        .merge(wrapped)
        .merge(acquire_public)
        .merge(oidc_legacy)
        .merge(state.compat.router());
    // Dev-only tooling routes (covers-debug). The debug-build gate
    // compiles this mount out of release binaries entirely, so no
    // production process can serve tooling however it was configured.
    #[cfg(debug_assertions)]
    let app = if crate::tooling::covers_debug::tooling_routes_enabled(&state.config) {
        app.merge(crate::tooling::covers_debug::router(
            state.reads.platform.covers.clone(),
        ))
    } else {
        app
    };
    #[cfg(any(test, feature = "test-support"))]
    let app = if state.config.test_hooks {
        app.route(
            "/__test__/typed-error",
            get(handlers::test_hooks::typed_error),
        )
        .route(
            "/__test__/panic",
            get(handlers::test_hooks::panicking_handler),
        )
        .route("/__test__/raw-500", get(handlers::test_hooks::raw_500))
        .route("/__test__/raw-503", get(handlers::test_hooks::raw_503))
    } else {
        app
    };
    // Compat-aware fallbacks: preflights and case-variant compat paths
    // redispatch into the compat router; everything else answers natively.
    // Closures carry the compat router (fallbacks take handlers, and the
    // app router itself stays stateless).
    let compat_router = state.compat.router();
    // Native fallbacks on compat paths still stamp compat CORS: a bare
    // 404 without `*` next to a disabled-protocol 404 with `*` would let
    // callers enumerate which protocol is enabled.
    let fallback_404 = {
        let compat_router = compat_router.clone();
        move |request: Request| async move {
            let path = request.uri().path().to_owned();
            let compat_path = crate::compat::shared::path_case::is_compat_path(&path);
            let method = request.method().clone();
            let headers = web.as_ref().map(|_| request.headers().clone());
            if let Some(redispatch) = fallback_redispatch(&compat_router, request).await {
                return redispatch;
            }
            if !compat_path
                && let (Some(web), Some(headers)) = (&web, &headers)
                && let Some(page) = web.respond(&method, &path, headers).await
            {
                return page;
            }
            let mut response = handlers::fallback_404().await.into_response();
            if compat_path {
                crate::compat::http::stamp_cors(&mut response);
            }
            response
        }
    };
    // Captured before `state` moves into the request-scope layer below.
    let debug_cors = state.config.debug_cors;
    let fallback_405 = {
        let compat_router = compat_router.clone();
        move |request: Request| async move {
            // Read before `request` moves into the redispatch: OPTIONS
            // matches no route, so native preflights land here without ever
            // seeing `debug_cors_layer` (same reason compat preflights
            // short-circuit inside `fallback_redispatch`).
            let is_options = request.method() == axum::http::Method::OPTIONS;
            let origin = request.headers().get(axum::http::header::ORIGIN).cloned();
            let compat_path =
                crate::compat::shared::path_case::is_compat_path(request.uri().path());
            if let Some(redispatch) = fallback_redispatch(&compat_router, request).await {
                return redispatch;
            }
            if debug_cors
                && is_options
                && let Some(preflight) = debug_preflight_response(origin.as_ref())
            {
                return preflight;
            }
            let mut response = handlers::fallback_405().await.into_response();
            if compat_path {
                crate::compat::http::stamp_cors(&mut response);
            }
            response
        }
    };
    app.fallback(fallback_404)
        .method_not_allowed_fallback(fallback_405)
        .layer(middleware::from_fn_with_state(state, request_scope))
        .layer(compression_layer())
}

/// Response compression (brotli or gzip, as the client accepts) for API,
/// Subsonic and Jellyfin bodies: a 500-song sync page is several hundred KB
/// of JSON or XML and shrinks about tenfold. Media is never compressed:
/// audio and images are already compressed, ranged and sized, and
/// transcodes stream live. Responses that already carry a
/// `Content-Encoding` (the web UI's precompressed assets, audio's
/// `identity`) pass through untouched, as do bodies under 32 bytes and
/// event streams.
fn compression_layer() -> tower_http::compression::CompressionLayer<impl Predicate> {
    let predicate = DefaultPredicate::new()
        .and(NotForContentType::const_new("audio/"))
        .and(NotForContentType::const_new("video/"))
        .and(NotForContentType::const_new("application/ogg"))
        .and(NotForContentType::const_new("application/octet-stream"))
        .and(NotForContentType::const_new("application/zip"))
        .and(NotForContentType::const_new("application/gzip"));
    tower_http::compression::CompressionLayer::new().compress_when(predicate)
}

/// Debug-only CORS layer over the pinned localhost list. Production never
/// mounts this: `AppConfig::debug_cors` is constructor-only and `main`
/// enables it solely in debug builds.
fn debug_cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::AllowOrigin;

    use crate::auth::session::cors::DEBUG_CORS_ORIGINS;

    let origins: Vec<axum::http::HeaderValue> = DEBUG_CORS_ORIGINS
        .iter()
        .filter_map(|origin| origin.parse().ok())
        .collect();
    let methods = [
        axum::http::Method::GET,
        axum::http::Method::POST,
        axum::http::Method::PUT,
        axum::http::Method::PATCH,
        axum::http::Method::DELETE,
        axum::http::Method::HEAD,
        axum::http::Method::OPTIONS,
    ];
    tower_http::cors::CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_credentials(true)
        .allow_methods(methods)
        // Explicit, never wildcard: tower-http rejects `*` headers combined
        // with credentials, and the SPA only needs these three.
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::header::RANGE,
        ])
        .expose_headers([
            axum::http::header::CONTENT_RANGE,
            axum::http::header::ACCEPT_RANGES,
            axum::http::header::CONTENT_LENGTH,
        ])
}

/// Native preflight answer for debug CORS. Mirrors `debug_cors_layer`
/// (same origin allowlist, methods, and headers) because the layer never
/// sees OPTIONS requests: they land in the 405 fallback instead. Returns
/// `None` for a missing or disallowed `Origin` so the bare 405 still
/// answers non-browser callers.
fn debug_preflight_response(
    origin: Option<&axum::http::HeaderValue>,
) -> Option<axum::response::Response> {
    use crate::auth::session::cors::DEBUG_CORS_ORIGINS;

    let origin = origin?;
    let allowed = DEBUG_CORS_ORIGINS
        .iter()
        .any(|pinned| pinned.as_bytes() == origin.as_bytes());
    if !allowed {
        return None;
    }
    let mut response = axum::http::StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        origin.clone(),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        axum::http::HeaderValue::from_static("true"),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
        axum::http::HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS"),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
        axum::http::HeaderValue::from_static("authorization, content-type, range"),
    );
    headers.insert(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static(
            "origin, access-control-request-method, access-control-request-headers",
        ),
    );
    Some(response)
}
