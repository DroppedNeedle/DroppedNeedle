//! Router assembly. Registration order is load-bearing and documented:
//! specific routes first, then the conditional test hooks, then the 404 and
//! 405 fallbacks. The request-scope layer wraps everything including the
//! fallbacks.

use axum::{Router, middleware, routing::get};

use crate::{handlers, middleware::request_scope, state::AppState};

/// Build the full application router from explicit state.
pub fn create_app(state: AppState) -> Router {
    let mut app = Router::new()
        .route("/health", get(handlers::health))
        .route("/openapi.json", get(handlers::openapi_json));
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
