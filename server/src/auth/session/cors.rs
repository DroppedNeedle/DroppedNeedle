//! Debug-only CORS data.
//!
//! Production serves same-origin with no CORS middleware (v2 parity). Debug
//! builds layer an explicit localhost-origins allowlist with credentials. The
//! origins below are the v2 dev list verbatim. Enforcement is one wiring line
//! (see below); this module pins the list and its predicate so a test can
//! hold it stable.
//!
//! Wiring: add `tower-http` with the `cors` feature, then (debug-gated on the
//! deployment tier, same constructor-only style as `test_hooks`):
//!
//! ```ignore
//! use tower_http::cors::{AllowOrigin, CorsLayer};
//! let origins: Vec<HeaderValue> = cors::DEBUG_CORS_ORIGINS
//!     .iter()
//!     .filter_map(|origin| origin.parse().ok())
//!     .collect();
//! let cors = CorsLayer::new()
//!     .allow_origin(AllowOrigin::list(origins))
//!     .allow_credentials(true)
//!     .allow_methods([Method::GET, Method::POST, Method::PUT, Method::PATCH])
//!     .allow_headers([AUTHORIZATION, CONTENT_TYPE, RANGE]);
//! // .layer(cors) last on the v3 router so it runs first (outermost).
//! // Never `Any` methods/headers with credentials: tower-http rejects the
//! // combination, and the SPA needs only the lists above (see
//! // `debug_cors_layer` in `app.rs` for the mounted copy).
//! ```

/// Localhost dev origins allowed in debug builds only (v2 list verbatim).
pub const DEBUG_CORS_ORIGINS: &[&str] = &[
    "http://localhost:5173",
    "http://127.0.0.1:5173",
    "http://[::1]:5173",
    "http://localhost:4173",
    "http://127.0.0.1:4173",
    "http://[::1]:4173",
    "http://localhost:3000",
    "http://127.0.0.1:3000",
    "http://[::1]:3000",
];

/// True when `origin` is on the debug allowlist. Production never calls this:
/// no CORS layer is mounted there at all.
pub fn is_debug_origin(origin: &str) -> bool {
    DEBUG_CORS_ORIGINS.contains(&origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_list_is_localhost_only() {
        assert_eq!(DEBUG_CORS_ORIGINS.len(), 9);
        assert!(is_debug_origin("http://localhost:5173"));
        assert!(!is_debug_origin("https://music.example.com"));
        assert!(!is_debug_origin("http://localhost:5173.evil.test"));
        for origin in DEBUG_CORS_ORIGINS {
            assert!(origin.starts_with("http://"), "{origin} must be plain http");
        }
    }
}
