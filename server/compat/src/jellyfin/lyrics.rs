//! `GET /Audio/{id}/Lyrics`: a track's lyrics, as Jellyfin 10.10.6 answers
//! them (`server/tests/reference/compat/jellyfin-10.10.6-lyrics/`).
//!
//! Finamp asks only for a track whose PlaybackInfo lists a `Lyric` stream;
//! Jellify asks for every track it plays and shows a lyrics button for any
//! answer with lines. So a track without lyrics, an unknown id and a failed
//! online lookup all get Jellyfin's own 404, never an empty list.

use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use droppedneedle_kernel::error::ApiError;

use super::models::{LyricDto, LyricLine, LyricMetadata};
use super::router::*;

/// Ticks per millisecond: a line's `Start` is in ticks.
const TICKS_PER_MILLISECOND: i64 = 10_000;

/// Answer the lyrics of `item_id`, or `missing` (Jellyfin's 404) when it
/// has none.
pub(super) async fn lyrics(state: &JellyfinState, item_id: &str, missing: Response) -> Response {
    let internal = match state.ids.from_jf(item_id).await {
        Ok(Some((kind, internal))) if kind == "track" => internal,
        Ok(_) => return missing,
        Err(failed) => return ApiError::from(failed).into_response(),
    };
    match state.library.lyrics(&internal).await {
        Ok(Some(view)) if !view.lines.is_empty() => json(
            StatusCode::OK,
            &LyricDto {
                metadata: LyricMetadata {
                    is_synced: view.synced,
                },
                lyrics: view
                    .lines
                    .into_iter()
                    .map(|(text, start_ms)| LyricLine {
                        text,
                        start: start_ms.map(|ms| ms.saturating_mul(TICKS_PER_MILLISECOND)),
                    })
                    .collect(),
            },
        ),
        Ok(_) => missing,
        Err(failed) => ApiError::from(failed).into_response(),
    }
}
