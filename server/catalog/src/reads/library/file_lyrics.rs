//! [`LyricsPort`] that serves the lyrics a track's file carries before
//! asking the port it wraps (the provider lookup, or [`NoOnlineLyrics`]).
//!
//! A track the lyrics sweep marked is read from disk: its file is found
//! through the same lookup and root confinement as playback, then its
//! `.lrc` file or lyrics tag is read with the function the sweep used, off
//! the async workers. Only when that gives nothing (the track is not
//! marked, its file is gone, or the lyrics were removed since the sweep)
//! does the wrapped port answer. A database error is an error, never "no
//! lyrics".

use std::sync::Arc;

use droppedneedle_library::tags::lyrics::read_file_lyrics;
use droppedneedle_media::stream::local_files::{LibraryFiles, LocateError};

use super::sqlite::LibraryCatalog;
use super::stores::{BoxFuture, LyricDoc, LyricsPort, StoreError};

/// File lyrics first, then the wrapped port.
pub struct FileLyrics {
    catalog: LibraryCatalog,
    files: LibraryFiles,
    fallback: Arc<dyn LyricsPort>,
}

impl FileLyrics {
    /// Serve marked tracks from their files, everything else from `fallback`.
    pub fn new(
        catalog: LibraryCatalog,
        files: LibraryFiles,
        fallback: Arc<dyn LyricsPort>,
    ) -> Self {
        Self {
            catalog,
            files,
            fallback,
        }
    }

    async fn read_file(&self, track_id: &str) -> Result<Option<LyricDoc>, StoreError> {
        if !self.catalog.track_has_lyrics(track_id).await? {
            return Ok(None);
        }
        let path = match self.files.locate_track(track_id).await {
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
}

impl LyricsPort for FileLyrics {
    fn get<'a>(&'a self, track_id: &'a str) -> BoxFuture<'a, Result<Option<LyricDoc>, StoreError>> {
        Box::pin(async move {
            match self.read_file(track_id).await? {
                Some(doc) => Ok(Some(doc)),
                None => self.fallback.get(track_id).await,
            }
        })
    }
}

/// The port behind file lyrics when no online lyrics source is set up:
/// every track reads as having none.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOnlineLyrics;

impl LyricsPort for NoOnlineLyrics {
    fn get<'a>(
        &'a self,
        _track_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LyricDoc>, StoreError>> {
        Box::pin(async { Ok(None) })
    }
}
