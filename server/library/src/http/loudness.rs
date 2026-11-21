//! A track's levelling numbers for the web player.
//!
//! - `GET /library/tracks/{id}/loudness`: the gains and peaks the player
//!   evens out volume with, measured by the server first and from the
//!   file's ReplayGain tags second. Open to every signed-in user, as the
//!   track is catalog data.

use axum::{
    Json,
    extract::{Path, State},
};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::mount::{Routes, routes};
use serde::Serialize;
use utoipa::ToSchema;

use super::blocking;
use crate::loudness::{Loudness, LoudnessError, TrackLevels};
use crate::wiring::LibraryServices;
use droppedneedle_auth::User;

/// Loudness routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new().user(routes!(get_track_loudness))
}

/// A track's levelling numbers, as ReplayGain 2.0 gives them. Each is null
/// when neither a measurement nor the file's tags give it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TrackLoudnessView {
    /// Gain in dB that brings the track to the -18 LUFS reference.
    pub track_gain_db: Option<f64>,
    /// The track's largest sample, 1.0 being full scale.
    pub track_peak: Option<f64>,
    /// Gain in dB that brings the track's album to the reference.
    pub album_gain_db: Option<f64>,
    /// The album's largest sample.
    pub album_peak: Option<f64>,
}

impl From<TrackLevels> for TrackLoudnessView {
    fn from(levels: TrackLevels) -> Self {
        Self {
            track_gain_db: levels.track_gain_db,
            track_peak: levels.track_peak,
            album_gain_db: levels.album_gain_db,
            album_peak: levels.album_peak,
        }
    }
}

impl From<LoudnessError> for ApiError {
    fn from(error: LoudnessError) -> Self {
        match error {
            LoudnessError::Busy(cause) => ApiError::unavailable(Unavailable::ServerBusy, cause),
            LoudnessError::Failed(cause) => ApiError::internal(cause),
        }
    }
}

/// A track's gains and peaks for evening out volume: measured first, the
/// file's tags second.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/tracks/{id}/loudness",
    params(("id" = String, Path, description = "Local track id")),
    responses(
        (status = 200, description = "The track's levelling numbers", body = TrackLoudnessView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown track, or one not in the library now"),
    )
)]
pub async fn get_track_loudness(
    State(loudness): State<Loudness>,
    _caller: User,
    Path(track_id): Path<String>,
) -> Result<Json<TrackLoudnessView>, ApiError> {
    let levels = blocking(move || loudness.levels(&track_id))
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(levels.into()))
}
