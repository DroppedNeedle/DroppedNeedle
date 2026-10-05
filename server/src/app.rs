//! Router assembly. Registration order is load-bearing and documented:
//! specific routes first, then the conditional test hooks, then the 404 and
//! 405 fallbacks. The request-scope layer wraps everything including the
//! fallbacks.
//!
//! The `/api/v3` router carries two middleware layers, innermost first:
//! the deny-by-default session gate, then the rate limiter. The debug-only
//! CORS layer mounts outermost and only when the constructor enables it.
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

use axum::{Router, extract::Request, middleware, response::IntoResponse, routing::get};

use crate::{
    auth::session::{middleware::require_session, rate_limit::rate_limit},
    compat::http::fallback_redispatch,
    handlers,
    middleware::request_scope,
    state::AppState,
};

/// Build the full application router from explicit state.
pub fn create_app(state: AppState) -> Router {
    let mut v3 = Router::new()
        .nest(
            "/api/v3",
            state
                .auth
                .router()
                .merge(state.reads.gated_router())
                .merge(state.stage6.gated_router())
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
            state.auth.session_auth.clone(),
            require_session,
        ))
        .layer(middleware::from_fn_with_state(
            state.auth.limits.clone(),
            rate_limit,
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
    let app = Router::new()
        .route(
            "/health",
            get(handlers::health).with_state(state.admin.checkpoint.clone()),
        )
        .route("/openapi.json", get(handlers::openapi_json))
        .merge(v3)
        .merge(wrapped)
        .merge(acquire_public)
        .merge(state.compat.router());
    // Dev-only tooling routes (covers-debug, R11). The debug-build gate
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
            let compat_path =
                crate::compat::shared::path_case::is_compat_path(request.uri().path());
            if let Some(redispatch) = fallback_redispatch(&compat_router, request).await {
                return redispatch;
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
