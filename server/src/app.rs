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

use axum::{Router, middleware, routing::get};

use crate::{
    auth::session::{middleware::require_session, rate_limit::rate_limit},
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
                .merge(state.acquire.gated_router()),
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
    let mut app = Router::new()
        .route("/health", get(handlers::health))
        .route("/openapi.json", get(handlers::openapi_json))
        .merge(v3)
        .merge(wrapped)
        .merge(acquire_public);
    if state.config.test_hooks {
        app = app
            .route(
                "/__test__/typed-error",
                get(handlers::test_hooks::typed_error),
            )
            .route(
                "/__test__/panic",
                get(handlers::test_hooks::panicking_handler),
            )
            .route("/__test__/raw-500", get(handlers::test_hooks::raw_500))
            .route("/__test__/raw-503", get(handlers::test_hooks::raw_503));
    }
    app.fallback(handlers::fallback_404)
        .method_not_allowed_fallback(handlers::fallback_405)
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
