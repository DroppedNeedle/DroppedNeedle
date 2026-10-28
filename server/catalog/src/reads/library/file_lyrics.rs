//! The lyrics a track's file carries, read for
//! [`LyricsService`](crate::reads::lyrics::LyricsService).
//!
//! A track the lyrics sweep marked is read from disk: its file is found
//! through the same lookup and root confinement as playback, then its
//! `.lrc` file or lyrics tag is read with the function the sweep used, off
//! the async workers. A track that is not marked, whose file is gone, or
//! whose lyrics were removed since the sweep reads as none. A database
//! error is an error, never "no lyrics".

use droppedneedle_library::tags::lyrics::read_file_lyrics;
use droppedneedle_media::stream::local_files::{LibraryFiles, LocateError};

use super::store::LocalCatalog;
use super::stores::{LyricDoc, StoreError};

/// The lyrics `track_id`'s file carries, if any.
pub(crate) async fn read(
    catalog: &LocalCatalog,
    files: &LibraryFiles,
    track_id: &str,
) -> Result<Option<LyricDoc>, StoreError> {
    if !catalog.track_has_lyrics(track_id).await? {
        return Ok(None);
    }
    let path = match files.locate_track(track_id).await {
        Ok((_, path)) => path,
        Err(LocateError::Internal(cause)) => return Err(StoreError::Internal(cause)),
        Err(LocateError::Unknown | LocateError::Refused(_)) => return Ok(None),
    };
    let read = tokio::task::spawn_blocking(move || read_file_lyrics(&path))
        .await
        .map_err(|error| StoreError::Internal(error.to_string()))?;
    Ok(read.map(|(_, lyrics)| LyricDoc {
        lines: lyrics.lines,
        synced: lyrics.synced,
    }))
}
