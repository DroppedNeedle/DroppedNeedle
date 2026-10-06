//! The production library port: release lookups through the library's
//! MusicBrainz release source, what the catalog already holds (read over
//! the shared database), and the library's import seam for publishing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use sqlx::SqlitePool;

use super::ports::{ImportFailure, ImportReceipt, ImportRequest, LandingLibrary, OwnedTracks};
use crate::library::identify::sources::{ReleaseHit, ReleaseSource, SourceError};
use crate::library::import::{DownloadImport, ImportError, ImportSource};
use crate::library::matching::Release;
use crate::library::wiring::LibrarySetup;

/// Indexed library tracks with their album's identity and their own,
/// the one notion of "the library holds it" the landing and the wanted
/// watcher share: an album identified as the group, or (not identified
/// yet) a file whose own tags name the group.
pub(crate) const OWNED_TRACKS_FROM: &str = "FROM local_tracks t \
     JOIN local_albums b ON b.id = t.local_album_id AND b.retired_into_album_id IS NULL \
     LEFT JOIN local_album_external_identities ai ON ai.local_album_id = b.id \
     LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
     WHERE t.availability = 'indexed' \
       AND (lower(ai.release_group_mbid) = ?1 OR lower(t.embedded_release_group_mbid) = ?1)";

/// Indexed tracks of one release group: release track and recording
/// (sealed identity first, then the file's own tags) and quality facts.
fn group_tracks_sql() -> String {
    format!(
        "SELECT lower(COALESCE(ti.release_track_mbid, t.embedded_release_track_mbid, '')), \
         lower(COALESCE(ti.recording_mbid, t.embedded_recording_mbid, '')), \
         t.file_format, t.bit_rate, t.bit_depth {OWNED_TRACKS_FROM}"
    )
}

type GroupTrack = (String, String, String, Option<i64>, Option<i64>);

/// The library as the landing sees it in production.
pub struct LibraryLanding {
    library: LibrarySetup,
    releases: Arc<dyn ReleaseSource>,
    pool: SqlitePool,
}

impl LibraryLanding {
    /// Port over the library bundle, its release source, and the reader
    /// pool of the application database.
    pub fn new(library: LibrarySetup, releases: Arc<dyn ReleaseSource>, pool: SqlitePool) -> Self {
        Self {
            library,
            releases,
            pool,
        }
    }

    async fn group_tracks(&self, release_group_mbid: &str) -> Vec<GroupTrack> {
        match sqlx::query_as::<_, GroupTrack>(&group_tracks_sql())
            .bind(release_group_mbid.to_ascii_lowercase())
            .fetch_all(&self.pool)
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "library holdings unreadable; landing treats the group as empty");
                Vec::new()
            }
        }
    }
}

impl LandingLibrary for LibraryLanding {
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        self.releases.search(title, artist)
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        self.releases.release(mbid)
    }

    fn owned<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, OwnedTracks> {
        Box::pin(async move {
            let mut owned = OwnedTracks::default();
            for (release_track, recording, ..) in self.group_tracks(release_group_mbid).await {
                if !release_track.is_empty() {
                    owned.release_tracks.insert(release_track);
                }
                if !recording.is_empty() {
                    owned.recordings.insert(recording);
                }
            }
            owned
        })
    }

    fn held_tier<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let rows = self.group_tracks(release_group_mbid).await;
            let tiers: HashSet<&'static str> = rows
                .iter()
                .map(|(_, _, format, bitrate, depth)| {
                    super::quality::tier_for(
                        format,
                        bitrate.and_then(|rate| u32::try_from(rate).ok()),
                        depth.and_then(|depth| u8::try_from(depth).ok()),
                    )
                })
                .collect();
            super::quality::worst(tiers).map(str::to_owned)
        })
    }

    fn fingerprints(
        &self,
        files: Vec<(String, PathBuf)>,
    ) -> BoxFuture<'_, HashMap<String, Vec<String>>> {
        Box::pin(async move {
            match &self.library.fingerprints {
                Some(source) => source.identify_files(&files).await,
                None => HashMap::new(),
            }
        })
    }

    fn library_dirs(&self) -> Vec<PathBuf> {
        self.library
            .live_registry()
            .roots()
            .iter()
            .map(|root| root.path.clone())
            .collect()
    }

    fn import(
        &self,
        request: ImportRequest,
    ) -> BoxFuture<'_, Result<ImportReceipt, ImportFailure>> {
        Box::pin(async move {
            let library = self.library.clone();
            let import = DownloadImport {
                task_id: request.task_id,
                release: request.release,
                files: request
                    .files
                    .into_iter()
                    .map(|file| ImportSource {
                        path: file.path,
                        track: file.track,
                    })
                    .collect(),
            };
            let done = tokio::task::spawn_blocking(move || library.import_download(&import))
                .await
                .map_err(|error| {
                    ImportFailure::LocalFault(format!("import did not run: {error}"))
                })?;
            match done {
                Ok(album) => Ok(ImportReceipt {
                    bundle_id: album.bundle_id,
                    album_id: album.album_id,
                    paths: album.paths,
                    skipped: album.skipped,
                }),
                Err(ImportError::LocalFault(detail)) => Err(ImportFailure::LocalFault(detail)),
                Err(ImportError::Occupied(detail)) => Err(ImportFailure::Occupied(detail)),
            }
        })
    }
}
