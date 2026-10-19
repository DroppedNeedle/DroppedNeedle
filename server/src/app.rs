//! Router assembly, and the order a request passes through, outermost
//! first:
//!
//! 1. HSTS, so every response can carry the header, the 404 outside the
//!    base included. It adds one only when the Security settings ask.
//! 2. The base-path split. `BASE_PATH` is stripped once, on a segment
//!    boundary, so everything below (layers, session gate, routes, web UI)
//!    sees base-relative paths. A request outside the base gets the 404
//!    envelope, with its own request scope and content-security header.
//! 3. The content-security layer (every native response; the compat shims
//!    are left alone), compression, and `request_scope` (request id, span,
//!    the 5xx boundary). They wrap steps 4 and 5, fallbacks included.
//! 4. Pre-routing. On a compat path (`/subsonic`, `/jellyfin`, in any
//!    case) a preflight answers 204 with compat CORS before auth, a
//!    case-variant path is rewritten to its registered casing (Feishin posts
//!    lower-case Jellyfin paths), and every answer carries compat CORS, so a
//!    native 404 or 405 there looks like a disabled protocol's 404 and the
//!    enabled protocols cannot be enumerated. Any other path goes through
//!    the debug-only CORS layer when the constructor enables it.
//! 5. The router: the routes, a plain 404 fallback that serves the web UI
//!    for a GET outside the API and compat paths, and a plain 405.
//!
//! Step 4 has to come before routing. The router's
//! `method_not_allowed_fallback` replaces every route's default method
//! fallback, layered ones included, so no layer on an inner router sees an
//! OPTIONS or wrong-method request on an existing path: it goes straight to
//! the 405. A layer also runs after its route matched, so a path it
//! rewrote would never match again. Subsonic keeps its own 405 envelope
//! because it sets its fallback before its layers, and a fallback that is
//! not the default is never replaced.
//!
//! Inside the router, matching does not follow registration order (an
//! overlap panics at build). What is load-bearing is which layers wrap
//! which routes:
//! - Areas declare their routes as [`Routes`], in posture groups (public,
//!   user, curator, admin), and [`mount`] lists them once. Each group is
//!   layered on its own and merged at the root: the user, curator and
//!   admin groups get the rate limiter, then the session gate outside it,
//!   so the limiter keys signed-in callers by user; the public group gets
//!   the limiter alone. The API document comes from the same list.
//! - The areas not yet declared as [`Routes`] nest under one `/api/v3`
//!   router with the same two layers, the deny-by-default session gate
//!   outermost.
//! - The wrapped trio nests under `/api/v3` on its own router outside the
//!   session gate: its `X-Wrapped-API-Key` extractor is the only credential,
//!   and allowlisting those paths would wrongly make them public. It keeps
//!   the shared rate limiter, as do the public callbacks.
//! - The compat shims merge outside the session gate too: they carry their
//!   own app-password auth plus the compat layers (CORS, limits), and never
//!   inherit `/api` session auth.

use axum::{Router, extract::Request, middleware, response::IntoResponse};
use tower::{Layer as _, ServiceExt as _};
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};

use crate::{
    handlers,
    middleware::request_scope,
    security_headers::{
        ContentSecurity, TransportSecurity, content_security, strict_transport_security,
    },
    state::AppState,
    web::WebUi,
};
use droppedneedle_auth::session::{
    event_stream, middleware::require_session, rate_limit::rate_limit,
};
use droppedneedle_compat::{
    http::{canonicalize_case, preflight, stamp_cors},
    shared::path_case::is_compat_path,
};
use droppedneedle_kernel::mount::{Postures, Routes, routes};
use utoipa::openapi::OpenApi;

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
        .layer(middleware::from_fn_with_state(state.clone(), request_scope))
        .layer(middleware::from_fn_with_state(
            ContentSecurity::from_state(&state),
            content_security,
        ));
    let hsts = middleware::from_fn_with_state(
        TransportSecurity::from_state(&state),
        strict_transport_security,
    );
    let app = pre_routing(&state, routes(&state, web))
        .layer(middleware::from_fn_with_state(state.clone(), request_scope))
        .layer(compression_layer())
        .layer(middleware::from_fn_with_state(
            ContentSecurity::from_state(&state),
            content_security,
        ));
    let app = if base_path.is_empty() {
        app
    } else {
        // Strip the base by hand rather than with `Router::nest`, which
        // does not route `{base}/` (the SPA's own root URL) into the app.
        Router::new().fallback(move |request: Request| async move {
            match base_relative_uri(&base_path, request.uri()) {
                Some(uri) => {
                    let (mut parts, body) = request.into_parts();
                    parts.uri = uri;
                    answer(app.oneshot(Request::from_parts(parts, body)).await)
                }
                None => answer(outside_base.oneshot(request).await),
            }
        })
    };
    app.layer(hsts)
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

/// The pre-routing step in front of `routes` (step 4 in the module docs).
fn pre_routing(state: &AppState, routes: Router) -> Router {
    let native = if state.config.debug_cors {
        Router::new().fallback_service(debug_cors_layer().layer(routes.clone()))
    } else {
        routes.clone()
    };
    Router::new().fallback(move |mut request: Request| async move {
        if !is_compat_path(request.uri().path()) {
            return answer(native.oneshot(request).await);
        }
        if let Some(preflight) = preflight(&request) {
            return preflight;
        }
        canonicalize_case(&mut request);
        let mut response = answer(routes.oneshot(request).await);
        stamp_cors(&mut response);
        response
    })
}

/// The response of a service that cannot fail.
fn answer(
    result: Result<axum::response::Response, std::convert::Infallible>,
) -> axum::response::Response {
    match result {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// Every area mounted through [`Routes`], listed once. Serving and the API
/// document both read this list.
fn mount(m: Mount<'_>) -> Mount<'_> {
    m.area(server_routes(), |_| ())
        .area(crate::admin::routes(), |state| state.admin.clone())
        .area(crate::settings::wiring::routes(), |state| {
            state.settings.clone()
        })
        .area(droppedneedle_plugins::wiring::routes(), |state| {
            state.plugins.clone()
        })
        .area(droppedneedle_auth::wiring::routes(), |state| {
            state.auth.clone()
        })
        .area(event_stream::routes(), |state| {
            event_stream::StreamRoute::new(
                state.events.clone(),
                event_stream::session_check(state.auth.session_auth.store.clone()),
            )
        })
        .area(droppedneedle_media::routes(), |state| state.media.clone())
        .area(
            droppedneedle_media::remotes::playlist_sync::routes(),
            |state| state.jobs.playlist_sync(),
        )
}

/// The server crate's own routes.
fn server_routes() -> Routes {
    Routes::new()
        .public(routes!(handlers::health))
        .public(routes!(handlers::openapi_json))
}

/// The document entries of every mounted area, built without state.
pub(crate) fn document() -> OpenApi {
    mount(Mount::document()).document
}

/// Collects the areas: their routes when serving, their document always.
struct Mount<'a> {
    state: Option<&'a AppState>,
    routes: Routes,
    document: OpenApi,
}

impl<'a> Mount<'a> {
    fn serve(state: &'a AppState) -> Self {
        Self {
            state: Some(state),
            routes: Routes::new(),
            document: OpenApi::default(),
        }
    }

    fn document() -> Self {
        Self {
            state: None,
            routes: Routes::new(),
            document: OpenApi::default(),
        }
    }

    fn area<S: Clone + Send + Sync + 'static>(
        mut self,
        routes: Routes<S>,
        state: impl FnOnce(&AppState) -> S,
    ) -> Self {
        self.document.merge(routes.openapi());
        if let Some(app) = self.state {
            self.routes = self.routes.merge(routes.with_state(state(app)));
        }
        self
    }
}

/// Each posture group with its own layers, merged at the root. User,
/// curator and admin routes get the rate limiter inside the session gate,
/// so the limiter keys signed-in callers by user; public routes get the
/// limiter alone. A group with no routes is left out: `route_layer`
/// refuses an empty router.
fn layered(state: &AppState, routes: Routes) -> Router {
    let Postures {
        public,
        user,
        curator,
        admin,
    } = routes.into_postures();
    let limited = |group: Router| {
        group.route_layer(middleware::from_fn_with_state(
            state.auth.limits.clone(),
            rate_limit,
        ))
    };
    let mut app = Router::new();
    let public = Router::from(public);
    if public.has_routes() {
        app = app.merge(limited(public));
    }
    for group in [user, curator, admin] {
        let group = Router::from(group);
        if group.has_routes() {
            app = app.merge(limited(group).route_layer(middleware::from_fn_with_state(
                state.auth.session_auth.clone(),
                require_session,
            )));
        }
    }
    app
}

/// Every route and the 404 and 405 fallbacks, at base-relative paths.
fn routes(state: &AppState, web: Option<WebUi>) -> Router {
    let mounted = layered(state, mount(Mount::serve(state)).routes);
    // The areas below are not mounted through `Routes` yet.
    let v3 = Router::new()
        .nest(
            "/api/v3",
            state
                .reads
                .gated_router()
                .merge(state.acquire.gated_router())
                .merge(state.library.gated_router())
                .merge(state.concerts.gated_router()),
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
    let wrapped = Router::new()
        .nest("/api/v3", state.reads.wrapped_router())
        .layer(middleware::from_fn_with_state(
            state.auth.limits.clone(),
            rate_limit,
        ));
    // Spotify OAuth and MusicBrainz contribution callbacks: outside the
    // session gate (token identified), still rate-limited like the
    // wrapped trio.
    let acquire_public = Router::new()
        .nest("/api/v3", state.acquire.callback_router())
        .merge(state.acquire.legacy_callback_router())
        .merge(state.library.contrib_callback_router())
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
    let app = mounted
        .merge(v3)
        .merge(wrapped)
        .merge(acquire_public)
        .merge(oidc_legacy)
        .merge(state.compat.router());
    // Dev-only tooling routes (covers-debug). The debug-build gate
    // compiles this mount out of release binaries entirely, so no
    // production process can serve tooling however it was configured.
    #[cfg(debug_assertions)]
    let app = if droppedneedle_catalog::reads::platform::covers_debug::tooling_routes_enabled(
        &state.config,
    ) {
        app.merge(
            droppedneedle_catalog::reads::platform::covers_debug::router(
                state.reads.platform.covers.clone(),
            ),
        )
    } else {
        app
    };
    #[cfg(any(test, feature = "test-support"))]
    let app = if state.config.test_hooks {
        app.route(
            "/__test__/typed-error",
            axum::routing::get(handlers::test_hooks::typed_error),
        )
        .route(
            "/__test__/panic",
            axum::routing::get(handlers::test_hooks::panicking_handler),
        )
        .route(
            "/__test__/raw-500",
            axum::routing::get(handlers::test_hooks::raw_500),
        )
        .route(
            "/__test__/raw-503",
            axum::routing::get(handlers::test_hooks::raw_503),
        )
        .route(
            "/__test__/unavailable/{code}",
            axum::routing::get(handlers::test_hooks::unavailable),
        )
        .route(
            "/subsonic/rest/__test__/raw-503",
            axum::routing::get(handlers::test_hooks::raw_503),
        )
        .route(
            "/jellyfin/__test__/raw-503",
            axum::routing::get(handlers::test_hooks::raw_503),
        )
    } else {
        app
    };
    // The web UI answers a GET that no route took, outside the API and the
    // compat shims.
    let fallback_404 = move |request: Request| async move {
        let path = request.uri().path();
        if !is_compat_path(path)
            && let Some(web) = &web
            && let Some(page) = web.respond(request.method(), path, request.headers()).await
        {
            return page;
        }
        handlers::fallback_404().await.into_response()
    };
    app.fallback(fallback_404)
        .method_not_allowed_fallback(handlers::fallback_405)
}

/// Response compression (brotli or gzip, as the client accepts) for API,
/// Subsonic and Jellyfin bodies: a 500-song sync page is several hundred KB
/// of JSON or XML and shrinks about tenfold. Media is never compressed:
/// audio and images are already compressed, ranged and sized, and
/// transcodes stream live. Responses that already carry a
/// `Content-Encoding` (the web UI's precompressed assets, audio's
/// `identity`) pass through untouched, as do bodies under 32 bytes and
/// event streams: an encoder buffers its output, which would hold
/// `/api/v3/events/stream` frames back. `DefaultPredicate` skips
/// `text/event-stream` too; the explicit rule keeps that true if the base
/// predicate ever changes.
fn compression_layer() -> tower_http::compression::CompressionLayer<impl Predicate> {
    let predicate = DefaultPredicate::new()
        .and(NotForContentType::SSE)
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

    use droppedneedle_auth::session::cors::DEBUG_CORS_ORIGINS;

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
