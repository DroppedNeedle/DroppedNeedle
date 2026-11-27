//! What a track sounds like, for the sound bench. Open to every signed-in
//! user, as the track is catalog data; both answer only while an admin has
//! sound analysis on, and refuse with their own code while its settings
//! cannot be read.
//!
//! - `GET /library/tracks/{id}/similar`: the library tracks that sound
//!   most like it, nearest first, each with its distance, a closeness word
//!   and the tags it shares with the track.
//! - `GET /library/tracks/{id}/sound`: the track's 200 stored measures, as
//!   stored.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use droppedneedle_auth::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidQuery;
use droppedneedle_kernel::mount::{Routes, routes};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::blocking;
use crate::sound::{Closeness, SoundAnalysis, SoundError};
use crate::wiring::LibraryServices;

/// The code both routes answer while sound analysis is off.
pub const SOUND_ANALYSIS_OFF: &str = "SOUND_ANALYSIS_OFF";
/// Tracks a similar list holds unless asked for fewer.
const SIMILAR_DEFAULT: usize = 10;
/// Most tracks a similar list holds.
const SIMILAR_MAX: usize = 50;

/// Track sound routes.
pub fn routes() -> Routes<LibraryServices> {
    Routes::new()
        .user(routes!(get_similar_tracks))
        .user(routes!(get_track_sound))
}

/// The code both routes answer while the sound analysis settings cannot
/// be read.
pub const SOUND_ANALYSIS_UNREADABLE: &str = "SOUND_ANALYSIS_UNREADABLE";

/// Whether the routes may answer: off and unreadable settings are each
/// their own 409.
fn switched_on(sound: &SoundAnalysis) -> Result<(), ApiError> {
    match sound.search().switch() {
        Some(true) => Ok(()),
        Some(false) => Err(off()),
        None => Err(ApiError::new(
            StatusCode::CONFLICT,
            SOUND_ANALYSIS_UNREADABLE,
            "The sound analysis settings could not be read. An admin needs to save them again.",
        )),
    }
}

fn off() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        SOUND_ANALYSIS_OFF,
        "Sound analysis is off. An admin can turn it on in Settings > Library.",
    )
}

/// How many similar tracks to list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SimilarTracksQuery {
    /// Tracks to list, 1 to 50 (10 by default); more is read as 50.
    pub limit: Option<u32>,
}

/// One library track that sounds like the asked one.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SimilarTrackView {
    /// The local track id.
    pub track_id: String,
    pub title: String,
    pub artist_name: String,
    /// The local album id.
    pub album_id: String,
    pub album_title: Option<String>,
    pub duration_seconds: Option<f64>,
    /// Cosine distance from the asked track: 0 is the same sound. Never
    /// past 0.15.
    pub distance: f32,
    /// That distance in words.
    pub closeness: Closeness,
    /// Tags among both tracks' five strongest, in the asked track's order.
    pub shared_tags: Vec<String>,
}

/// The library tracks that sound like one track.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SimilarTracksView {
    /// The analysis version the distances were read at.
    pub analysis_version: i64,
    /// Nearest first. Short, or empty, when few tracks sound close.
    pub tracks: Vec<SimilarTrackView>,
}

/// One track's stored sound.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TrackSoundView {
    /// The 200 values the model gave the track, as stored.
    pub measures: Vec<f32>,
}

/// The library tracks that sound most like this one, nearest first.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/tracks/{id}/similar",
    params(("id" = String, Path, description = "Local track id"), SimilarTracksQuery),
    responses(
        (status = 200, description = "Tracks that sound like it", body = SimilarTracksView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown track, or one not analysed for its current file"),
        (status = 409, description = "SOUND_ANALYSIS_OFF: sound analysis is off; SOUND_ANALYSIS_UNREADABLE: its settings cannot be read"),
    )
)]
pub async fn get_similar_tracks(
    State(sound): State<SoundAnalysis>,
    _caller: User,
    Path(track_id): Path<String>,
    ValidQuery(query): ValidQuery<SimilarTracksQuery>,
) -> Result<Json<SimilarTracksView>, ApiError> {
    let limit = query
        .limit
        .map_or(SIMILAR_DEFAULT, |limit| limit as usize)
        .clamp(1, SIMILAR_MAX);
    switched_on(&sound)?;
    let search = sound.search().clone();
    let found = blocking(move || {
        let Some(index) = search.index()? else {
            return Ok::<_, SoundError>(None);
        };
        let tracks = index.similar(&track_id, limit, &|_| false).map(|found| {
            found
                .into_iter()
                .filter_map(|neighbour| {
                    let track = index.track(&neighbour.track_id)?;
                    Some(SimilarTrackView {
                        track_id: neighbour.track_id,
                        title: track.title.clone(),
                        artist_name: track.artist_name.clone(),
                        album_id: track.album_id.clone(),
                        album_title: track.album_title.clone(),
                        duration_seconds: track.duration_seconds,
                        distance: neighbour.distance,
                        closeness: neighbour.closeness,
                        shared_tags: neighbour
                            .shared_tags
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                    })
                })
                .collect()
        });
        Ok(Some((index.analysis_version(), tracks)))
    })
    .await?;
    let (analysis_version, tracks) = found.ok_or_else(off)?;
    Ok(Json(SimilarTracksView {
        analysis_version,
        tracks: tracks.ok_or_else(ApiError::not_found)?,
    }))
}

/// A track's 200 stored sound measures, as stored.
#[utoipa::path(
    get,
    tag = "library",
    path = "/api/v3/library/tracks/{id}/sound",
    params(("id" = String, Path, description = "Local track id")),
    responses(
        (status = 200, description = "The track's stored sound", body = TrackSoundView),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Unknown track, or one not analysed for its current file"),
        (status = 409, description = "SOUND_ANALYSIS_OFF: sound analysis is off; SOUND_ANALYSIS_UNREADABLE: its settings cannot be read"),
    )
)]
pub async fn get_track_sound(
    State(sound): State<SoundAnalysis>,
    _caller: User,
    Path(track_id): Path<String>,
) -> Result<Json<TrackSoundView>, ApiError> {
    switched_on(&sound)?;
    let stored = blocking(move || sound.track(&track_id))
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(TrackSoundView {
        measures: stored.vector,
    }))
}
