//! Dev-only covers-debug tooling route.
//!
//! v2 shipped `GET /covers/debug/artist/{id}` on the prod API; v3 keeps the
//! diagnostics but moves them off the API entirely. This module serves the
//! same shape (MBID validity, cache state per size, warming flags, and a
//! plain recommendation) on `/__tooling__/covers/debug/artist/{id}`.
//!
//! Three gates keep it out of production, and any one of them suffices:
//!
//! - `AppConfig.tooling_routes` is constructor-only with no environment
//!   variable, defaulting to off (same pattern as the test hooks).
//! - `create_app` mounts this router only inside
//!   `#[cfg(debug_assertions)]`, so release builds cannot serve it even
//!   with the flag set.
//! - The server binary accepts `--tooling-routes` in debug builds only; a
//!   release binary rejects the flag as unknown.
//!
//! The route carries no utoipa annotation and never appears in the OpenAPI
//! document. Like the `__test__` hooks it mounts outside the session gate:
//! unreachable in production, frictionless on a dev box.

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Serialize;

use crate::reads::platform::covers::{CoverLookup, CoversState};

/// Sizes probed, mirroring the v2 disk-cache slots.
const DEBUG_SIZES: [u32; 2] = [250, 500];

/// Cache state for one size: presence, source label, and byte length.
/// Bytes themselves never leave this route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SizeDebug {
    /// Requested pixel width.
    pub size: u32,
    /// True when the art port holds this image now.
    pub cached: bool,
    /// Source label of the cached bytes, when cached.
    pub source: Option<String>,
    /// Byte length of the cached image, when cached.
    pub bytes: Option<usize>,
    /// True while this image resolves in the background.
    pub warming: bool,
}

/// The debug answer, mirroring the v2 `debug_info` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtistCoverDebug {
    /// The requested id, verbatim.
    pub artist_id: String,
    /// True when the id parses as an MBID.
    pub is_valid_mbid: bool,
    /// The stripped id when valid, else null.
    pub validated_mbid: Option<String>,
    /// Per-size cache and warming state.
    pub sizes: Vec<SizeDebug>,
    /// Plain next step for the operator.
    pub recommendation: String,
}

/// True when the tooling routes may mount: the constructor flag set, in a
/// debug build. Release builds always refuse.
pub fn tooling_routes_enabled(config: &crate::config::AppConfig) -> bool {
    config.tooling_routes && cfg!(debug_assertions)
}

/// The tooling router, mounted by `create_app` only when
/// [`tooling_routes_enabled`] holds.
pub fn router(state: CoversState) -> Router {
    Router::new()
        .route(
            "/__tooling__/covers/debug/artist/{artist_id}",
            get(debug_artist_cover),
        )
        .with_state(state)
}

/// Diagnose one artist image: validity, cache presence, warming, advice.
async fn debug_artist_cover(
    State(state): State<CoversState>,
    Path(artist_id): Path<String>,
) -> Json<ArtistCoverDebug> {
    let validated_mbid = parse_mbid(&artist_id);
    let is_valid_mbid = validated_mbid.is_some();
    let mut sizes = Vec::with_capacity(DEBUG_SIZES.len());
    for size in DEBUG_SIZES {
        let lookup = state.covers.artist_image(&artist_id, Some(size)).await;
        let art = match &lookup {
            CoverLookup::Found(found) => Some(found),
            CoverLookup::Warming | CoverLookup::Missing => None,
        };
        sizes.push(SizeDebug {
            size,
            cached: art.is_some(),
            source: art.map(|found| found.source.clone()),
            bytes: art.map(|found| found.bytes.len()),
            warming: lookup == CoverLookup::Warming,
        });
    }
    let recommendation = recommend(is_valid_mbid, &sizes).to_owned();
    Json(ArtistCoverDebug {
        artist_id,
        is_valid_mbid,
        validated_mbid,
        sizes,
        recommendation,
    })
}

/// MBID shape: 36 hex-or-dash characters after stripping, the same rule the
/// v2 validator and the export validator apply.
fn parse_mbid(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.len() == 36
        && trimmed
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

/// Plain next step, following the v2 recommendation ladder.
fn recommend(is_valid_mbid: bool, sizes: &[SizeDebug]) -> &'static str {
    if !is_valid_mbid {
        return "Invalid MBID format: no image can resolve for this id.";
    }
    if sizes.iter().any(|size| size.cached) {
        return "Image is cached and should load successfully.";
    }
    if sizes.iter().any(|size| size.warming) {
        return "Image is warming in the background; retry shortly.";
    }
    "No image source found. This artist will show a placeholder."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reads::platform::covers::{CoverBytes, FakeCoverArt};
    use std::sync::Arc;

    #[test]
    fn gate_needs_flag_and_debug_build() {
        let off = crate::config::AppConfig::new(crate::config::DEFAULT_PORT);
        assert!(!tooling_routes_enabled(&off));
        let on = crate::config::AppConfig::new(crate::config::DEFAULT_PORT).with_tooling_routes();
        assert_eq!(tooling_routes_enabled(&on), cfg!(debug_assertions));
        #[cfg(not(debug_assertions))]
        assert!(!tooling_routes_enabled(&on));
    }

    #[tokio::test]
    async fn debug_reports_cache_warming_and_invalid() {
        let mbid = uuid::Uuid::new_v4().to_string();
        let state = CoversState::new(Arc::new(
            FakeCoverArt::empty()
                .with_artist(
                    &mbid,
                    Some(250),
                    CoverBytes::new(vec![1, 2, 3], "image/jpeg", "lidarr"),
                )
                .warming_artist(&mbid, Some(500)),
        ));
        let answer = debug_artist_cover(State(state), Path(mbid.to_owned())).await;
        assert!(answer.is_valid_mbid);
        assert_eq!(answer.validated_mbid.as_deref(), Some(mbid.as_str()));
        assert_eq!(answer.sizes.len(), 2);
        assert!(answer.sizes[0].cached);
        assert_eq!(answer.sizes[0].source.as_deref(), Some("lidarr"));
        assert_eq!(answer.sizes[0].bytes, Some(3));
        assert!(!answer.sizes[1].cached);
        assert!(answer.sizes[1].warming);

        let cold = CoversState::new(Arc::new(FakeCoverArt::empty()));
        let invalid = debug_artist_cover(State(cold), Path("nope".to_owned())).await;
        assert!(!invalid.is_valid_mbid);
        assert!(invalid.validated_mbid.is_none());
        assert!(invalid.recommendation.contains("Invalid MBID"));
    }
}
