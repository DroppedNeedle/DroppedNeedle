//! The production library port: release lookups through the library's
//! MusicBrainz release source, what the catalog already holds (read over
//! the shared database), and the library's import seam for publishing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use sqlx::SqlitePool;

use super::ports::{
    ImportFailure, ImportReceipt, ImportRequest, LandingLibrary, OwnedCopy, OwnedTracks, Recycled,
};
use crate::library::identify::sources::{ReleaseHit, ReleaseSource, SourceError};
use crate::library::import::{DownloadImport, ImportError, ImportSource};
use crate::library::matching::Release;
use crate::library::mutations::{MutationError, Mutations};
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

/// Every indexed copy in a group, with the album it sits in, that album's
/// release (sealed identity first, then the file's own tags), the copy's
/// release track and recording, and its quality facts.
fn group_copies_sql() -> String {
    format!(
        "SELECT t.id AS track_id, b.id AS album_id, \
         lower(COALESCE(ai.release_mbid, t.embedded_release_mbid, '')) AS release_mbid, \
         lower(COALESCE(ti.release_track_mbid, t.embedded_release_track_mbid, '')) \
           AS release_track_mbid, \
         lower(COALESCE(ti.recording_mbid, t.embedded_recording_mbid, '')) AS recording_mbid, \
         t.file_format AS format, t.bit_rate AS bitrate, t.bit_depth AS depth \
         {OWNED_TRACKS_FROM}"
    )
}

/// One row of [`group_copies_sql`].
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct GroupCopy {
    pub track_id: String,
    pub album_id: String,
    pub release_mbid: String,
    pub release_track_mbid: String,
    pub recording_mbid: String,
    pub format: String,
    pub bitrate: Option<i64>,
    pub depth: Option<i64>,
}

/// The copies an upgrade of one release track replaces. Only the local
/// album holding the release being imported counts (the album whose
/// release is that edition, else the group's only album), so another
/// edition the person keeps is never touched. Inside that album, copies
/// of the release track win; the recording is matched only when no copy
/// of the release track exists. Two albums of the same edition are
/// ambiguous and answer nothing, so nothing is replaced.
pub fn copies_to_replace<'a>(
    rows: &'a [GroupCopy],
    release_mbid: &str,
    release_track_mbid: &str,
    recording_mbid: &str,
) -> Vec<&'a GroupCopy> {
    let release_mbid = release_mbid.to_ascii_lowercase();
    let albums: HashSet<&str> = rows.iter().map(|row| row.album_id.as_str()).collect();
    let of_release: HashSet<&str> = rows
        .iter()
        .filter(|row| !release_mbid.is_empty() && row.release_mbid == release_mbid)
        .map(|row| row.album_id.as_str())
        .collect();
    let album = match (of_release.len(), albums.len()) {
        (1, _) => of_release.into_iter().next(),
        (0, 1) => albums.into_iter().next(),
        _ => None,
    };
    let Some(album) = album else {
        return Vec::new();
    };
    let wanted = |value: &str, field: &str| !value.is_empty() && field.eq_ignore_ascii_case(value);
    let in_album = || rows.iter().filter(move |row| row.album_id == album);
    let by_track: Vec<&GroupCopy> = in_album()
        .filter(|row| wanted(release_track_mbid, &row.release_track_mbid))
        .collect();
    if !by_track.is_empty() {
        return by_track;
    }
    in_album()
        .filter(|row| wanted(recording_mbid, &row.recording_mbid))
        .collect()
}

fn tier_of(format: &str, bitrate: Option<i64>, depth: Option<i64>) -> &'static str {
    super::quality::tier_for(
        format,
        bitrate.and_then(|rate| u32::try_from(rate).ok()),
        depth.and_then(|depth| u8::try_from(depth).ok()),
    )
}

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

    fn chosen_edition<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            match crate::acquire::edition::chosen_edition(&self.pool, release_group_mbid).await {
                Ok(chosen) => chosen.map(|chosen| chosen.release_mbid),
                Err(error) => {
                    tracing::warn!(%error, "chosen edition unreadable; the request's edition is used");
                    None
                }
            }
        })
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
                .map(|(_, _, format, bitrate, depth)| tier_of(format, *bitrate, *depth))
                .collect();
            super::quality::worst(tiers).map(str::to_owned)
        })
    }

    fn owned_copies<'a>(
        &'a self,
        release_group_mbid: &'a str,
        release_mbid: &'a str,
        release_track_mbid: &'a str,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, Vec<OwnedCopy>> {
        Box::pin(async move {
            let rows: Vec<GroupCopy> = match sqlx::query_as(&group_copies_sql())
                .bind(release_group_mbid.to_ascii_lowercase())
                .fetch_all(&self.pool)
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(%error, "library copies unreadable; nothing is replaced");
                    return Vec::new();
                }
            };
            let mut seen = HashSet::new();
            copies_to_replace(&rows, release_mbid, release_track_mbid, recording_mbid)
                .into_iter()
                .filter(|row| seen.insert(row.track_id.clone()))
                .map(|row| OwnedCopy {
                    track_id: row.track_id.clone(),
                    tier: tier_of(&row.format, row.bitrate, row.depth),
                })
                .collect()
        })
    }

    fn recycle(
        &self,
        track_ids: Vec<String>,
        actor: String,
    ) -> BoxFuture<'_, Result<Recycled, String>> {
        Box::pin(async move {
            let library = self.library.clone();
            tokio::task::spawn_blocking(move || {
                let mutations = Mutations::new(&library);
                let mut moved: Recycled = Vec::new();
                for track_id in &track_ids {
                    match mutations.remove_track(track_id, true, &actor) {
                        Ok(removed) => moved.extend(removed.recycled),
                        Err(error) => {
                            let message = match error {
                                MutationError::NotFound(reason)
                                | MutationError::Conflict(reason)
                                | MutationError::Files(reason) => reason.message.to_owned(),
                                MutationError::Store(cause) => {
                                    tracing::error!(%cause, "recycle for an upgrade failed");
                                    "The library database could not be updated.".to_owned()
                                }
                            };
                            if !moved.is_empty() && !mutations.put_back(&moved, &actor) {
                                tracing::error!(
                                    "an upgrade's recycled files could not all be put back"
                                );
                            }
                            return Err(message);
                        }
                    }
                }
                Ok(moved)
            })
            .await
            .map_err(|error| {
                tracing::error!(%error, "recycle task did not run");
                "The old files could not be moved to the recycle bin.".to_owned()
            })?
        })
    }

    fn put_back(&self, moved: Recycled, actor: String) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let library = self.library.clone();
            tokio::task::spawn_blocking(move || Mutations::new(&library).put_back(&moved, &actor))
                .await
                .unwrap_or(false)
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
                staging: request.staging,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(track: &str, album: &str, release: &str, rt: &str, rec: &str) -> GroupCopy {
        GroupCopy {
            track_id: track.to_owned(),
            album_id: album.to_owned(),
            release_mbid: release.to_owned(),
            release_track_mbid: rt.to_owned(),
            recording_mbid: rec.to_owned(),
            format: "mp3".to_owned(),
            ..GroupCopy::default()
        }
    }

    fn ids(rows: Vec<&GroupCopy>) -> Vec<&str> {
        rows.into_iter().map(|row| row.track_id.as_str()).collect()
    }

    // An upgrade replaces only the copy it stands in for: never the same
    // recording in another edition the person keeps, and never a second
    // appearance of the recording when the release track itself is there.
    #[test]
    fn upgrade_replaces_only_its_own_album_and_track() {
        let rows = vec![
            copy("std-1", "standard", "rel-std", "rt-1", "rec-1"),
            copy("std-9", "standard", "rel-std", "rt-9", "rec-1"),
            copy("dlx-1", "deluxe", "rel-dlx", "rt-d1", "rec-1"),
        ];
        assert_eq!(
            ids(copies_to_replace(&rows, "REL-STD", "rt-1", "rec-1")),
            ["std-1"]
        );
        // No release-track copy: the recording, inside the edition only.
        assert_eq!(
            ids(copies_to_replace(&rows, "rel-dlx", "rt-gone", "rec-1")),
            ["dlx-1"]
        );
        // An edition the library does not hold, with two albums of the
        // group: nothing is replaced.
        assert!(copies_to_replace(&rows, "rel-other", "rt-1", "rec-1").is_empty());
        // The group's only album counts even without a sealed release.
        let only = vec![copy("a-1", "only", "", "rt-1", "rec-1")];
        assert_eq!(
            ids(copies_to_replace(&only, "rel-std", "rt-1", "rec-1")),
            ["a-1"]
        );
    }
}
