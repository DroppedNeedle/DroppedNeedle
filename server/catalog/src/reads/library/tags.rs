//! Tag reads from the files on disk: the catalog track's file,
//! found through the same lookup and root confinement as playback and
//! downloads, read with the library's tag reader off the async workers.

use super::stores::{StoreError, TagRead};
use droppedneedle_media::stream::local_files::{LibraryFiles, LocateError, PathRefusal};

/// Reads tags from catalog files under the live library roots.
#[derive(Clone)]
pub struct FileTagReader {
    files: LibraryFiles,
}

impl FileTagReader {
    /// Reader over the library catalog and roots.
    pub fn new(files: LibraryFiles) -> Self {
        Self { files }
    }

    /// Read the tags of one catalog track's file.
    pub async fn read(&self, track_id: &str) -> Result<TagRead, StoreError> {
        let path = match self.files.locate_track(track_id).await {
            Ok((_, path)) => path,
            Err(LocateError::Unknown) => return Ok(TagRead::Unknown),
            Err(LocateError::Refused(PathRefusal::Missing)) => return Ok(TagRead::Gone),
            Err(LocateError::Refused(PathRefusal::Outside)) => {
                tracing::warn!(
                    track_id,
                    "tag read refused: file resolves outside the library roots"
                );
                return Ok(TagRead::Outside);
            }
            Err(LocateError::Internal(cause)) => return Err(StoreError::Internal(cause)),
        };
        let read =
            tokio::task::spawn_blocking(move || droppedneedle_library::tags::read_tags(&path))
                .await
                .map_err(|error| StoreError::Internal(error.to_string()))?;
        Ok(match read {
            Ok((tag, _)) => TagRead::Found(Box::new(tag)),
            Err(error) => {
                tracing::warn!(track_id, %error, "could not read the tags of a library file");
                TagRead::Unreadable
            }
        })
    }
}
