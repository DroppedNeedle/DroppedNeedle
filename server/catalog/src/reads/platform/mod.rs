//! Platform reads: cover art, version check, and wrapped.
//!
//! Mounting:
//!
//! - `/covers/*`, `/library/albums/{id}/artwork` and `/version*` mount
//!   in the user group, behind the session gate (no role gate).
//! - `/wrapped/*` mounts in the wrapped-key group, outside the session
//!   gate, with only the `X-Wrapped-API-Key` check. Never move these
//!   routes to the public group: public means anyone.
//! - The v2 covers debug route stays out of this router: [`covers_debug`]
//!   mounts on its own, on dev-only tooling routes.

pub mod artwork;
pub mod covers;
pub mod covers_debug;
pub mod listenbrainz_wrapped;
pub mod version;
pub mod wrapped;

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

/// Every platform route in one router with no layers, for tests.
#[cfg(any(test, feature = "test-support"))]
pub fn platform_router(state: PlatformState) -> axum::Router {
    covers::routes()
        .with_state(state.covers)
        .into_router()
        .merge(version::routes().with_state(state.version).into_router())
        .merge(wrapped::routes().with_state(state.wrapped).into_router())
}
