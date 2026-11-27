//! Sound analysis's status, for its card on Settings > Library: admins
//! only, like the setting itself.
//!
//! - `GET /settings/library/sound-analysis/status`: what the loop is doing
//!   or why it waits, how much of the library is analysed, how long the
//!   rest should take, and the last file it could not read.

use axum::{Json, extract::State};
use droppedneedle_auth::Admin;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Serialize;
use utoipa::ToSchema;

use super::blocking;
use crate::sound::{
    ANALYSIS_VERSION, MODEL, SoundAnalysis, SoundError, SoundOutcome, SoundState, SoundWaitReason,
};
use crate::wiring::LibraryServices;

/// Sound analysis routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new().admin(routes!(get_sound_analysis_status))
}

/// The last file sound analysis could not read.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SoundNotAnalysedView {
    /// Where it lies below its library folder.
    pub relative_path: String,
    /// Why: `undecodable`, `too_short` or `over_time`.
    pub outcome: SoundOutcome,
}

/// What sound analysis is doing and how far it has come.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SoundAnalysisStatus {
    pub state: SoundState,
    /// With `waiting`: what it waits for. It carries on by itself.
    pub waiting_for: Option<SoundWaitReason>,
    /// Tracks in the library.
    pub tracks: u64,
    /// Tracks analysed for their current file.
    pub analysed: u64,
    /// Tracks still to read.
    pub to_go: u64,
    /// Tracks whose file could not be analysed, until it changes.
    pub not_analysed: u64,
    /// About how long the whole library takes, in seconds.
    pub library_seconds: u64,
    /// About how long the tracks still to read take, in seconds.
    pub seconds_left: u64,
    /// The last file it could not read, if any.
    pub last_not_analysed: Option<SoundNotAnalysedView>,
    /// The model's name.
    pub model: String,
    /// The analysis version rows are written at.
    pub analysis_version: i64,
}

impl From<SoundError> for ApiError {
    fn from(error: SoundError) -> Self {
        match error {
            SoundError::Busy(cause) => ApiError::unavailable(Unavailable::ServerBusy, cause),
            SoundError::Failed(cause) => ApiError::internal(cause),
        }
    }
}

/// What sound analysis is doing now, or why it waits, and how far it has
/// come. `GET /api/v3/settings/library/sound-analysis/status`.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/library/sound-analysis/status",
    responses(
        (status = 200, description = "Sound analysis status", body = SoundAnalysisStatus),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Not an admin"),
    )
)]
pub async fn get_sound_analysis_status(
    State(sound): State<SoundAnalysis>,
    _admin: Admin,
) -> Result<Json<SoundAnalysisStatus>, ApiError> {
    let status = blocking(move || sound.status()).await?;
    let seconds = |tracks: u64| (tracks as f64 * status.seconds_per_track).round() as u64;
    Ok(Json(SoundAnalysisStatus {
        state: status.state,
        waiting_for: status.waiting_for,
        tracks: status.coverage.tracks,
        analysed: status.coverage.analysed,
        to_go: status.coverage.to_go(),
        not_analysed: status.coverage.not_analysed,
        library_seconds: seconds(status.coverage.tracks),
        seconds_left: seconds(status.coverage.to_go()),
        last_not_analysed: status.last_not_analysed.map(|last| SoundNotAnalysedView {
            relative_path: last.relative_path,
            outcome: last.outcome,
        }),
        model: MODEL.to_owned(),
        analysis_version: ANALYSIS_VERSION,
    }))
}
