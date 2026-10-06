//! Platform reads: cover art, version check, and wrapped.
//!
//! Mounting:
//!
//! - `/covers/*`, `/library/albums/{id}/artwork` and `/version*` nest
//!   inside the session-gated v3 router (any signed-in user, no role gate).
//! - `/wrapped/*` mounts outside the session middleware with only the
//!   `X-Wrapped-API-Key` gate. Never add these paths to the session
//!   allowlist: allowlisted means public.
//! - The v2 covers debug route stays out of this router.

pub mod artwork;
pub mod covers;
pub mod listenbrainz_wrapped;
pub mod version;
pub mod wrapped;

use axum::Router;

/// Bundle of the three platform states for one-shot assembly.
#[derive(Clone)]
pub struct PlatformState {
    /// Cover art state.
    pub covers: covers::CoversState,
    /// Version state.
    pub version: version::VersionState,
    /// Wrapped state.
    pub wrapped: wrapped::WrappedState,
}

impl PlatformState {
    /// Bundle the three platform states.
    pub fn new(
        covers: covers::CoversState,
        version: version::VersionState,
        wrapped: wrapped::WrappedState,
    ) -> Self {
        Self {
            covers,
            version,
            wrapped,
        }
    }
}

/// All three platform routers merged, for tests. The app nests
/// [`session_router`] inside the session gate and mounts
/// [`wrapped_router`] outside it.
#[cfg(any(test, feature = "test-support"))]
pub fn platform_router(state: PlatformState) -> Router {
    session_router(&state).merge(wrapped_router(&state))
}

/// Covers + version routers for mounting inside the session gate.
pub fn session_router(state: &PlatformState) -> Router {
    covers::routes(state.covers.clone()).merge(version::routes(state.version.clone()))
}

/// Wrapped router for mounting outside the session middleware.
pub fn wrapped_router(state: &PlatformState) -> Router {
    wrapped::routes(state.wrapped.clone())
}
