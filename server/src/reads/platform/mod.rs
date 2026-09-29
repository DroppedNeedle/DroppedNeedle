//! Stage-4 platform read slice: covers, version, and wrapped.
//!
//! Clean-slate `/api/v3` handlers over trait ports with stage-4 fakes; real
//! art providers, the GitHub client, and ListenBrainz aggregation arrive in
//! stage 5 behind the same traits. Posture at wiring:
//!
//! - `/covers/*` and `/version*` nest inside the session-gated v3 router
//!   (trace `U*`: any signed-in user, no role gate).
//! - `/wrapped/*` mounts outside the session middleware with only the
//!   `X-Wrapped-API-Key` gate (trace `W`). Never add these paths to the
//!   session allowlist: allowlisted means public.
//! - The v2 covers debug route stays out (trace A:72, dropped).

pub mod covers;
pub mod version;
pub mod wrapped;

use axum::Router;

/// Bundle of the three slice states for one-shot assembly.
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
    /// Bundle the three slice states.
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

/// Merge the three slice routers. The result carries relative paths; wiring
/// nests the covers/version routers inside the session gate and mounts the
/// wrapped router outside it (see [`wrapped_router`] and
/// [`session_router`]).
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
