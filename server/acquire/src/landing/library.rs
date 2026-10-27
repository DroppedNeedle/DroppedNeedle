//! The production library port: release lookups through the library's
//! MusicBrainz release source, what the catalog already holds (read over
//! the shared database), and the library's import seam for publishing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension, Params, params};

use super::ports::{
    ImportFailure, ImportReceipt, ImportRequest, LandingLibrary, OwnedCopy, OwnedTracks, VersionRow,
};
use super::sql;
use crate::downloads::plan::PlannedAlbum;
use droppedneedle_kernel::db::DbError;
use droppedneedle_library::identify::sources::{
    Prioritised, ReleaseHit, ReleaseSource, SourceError, group_editions,
};
use droppedneedle_library::import::{DownloadImport, ImportError, ImportSource, Imports};
use droppedneedle_library::management::service::Management;
use droppedneedle_library::matching::Release;
use droppedneedle_library::publish::paths::{ResolvedRoot, left_out};
use droppedneedle_library::scan::EffectivePolicy;
use droppedneedle_library::wiring::LibrarySetup;
use droppedneedle_providers::RequestPriority;

type GroupTrack = (String, String, String, Option<i64>, Option<i64>);

/// One row of `sql::GROUP_COPIES`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupCopy {
    pub track_id: String,
    pub album_id: String,
    pub release_mbid: String,
    /// The release the album's identity names; empty when it has none.
    pub album_release_mbid: String,
    pub release_track_mbid: String,
    pub recording_mbid: String,
    pub format: String,
    pub bitrate: Option<i64>,
    pub depth: Option<i64>,
}

impl GroupCopy {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            track_id: row.get(0)?,
            album_id: row.get(1)?,
            release_mbid: row.get(2)?,
            album_release_mbid: row.get(3)?,
            release_track_mbid: row.get(4)?,
            recording_mbid: row.get(5)?,
            format: row.get(6)?,
            bitrate: row.get(7)?,
            depth: row.get(8)?,
        })
    }
}

/// Every row one statement selects, mapped by `map`.
fn read_rows<T>(
    conn: &Connection,
    text: &str,
    values: impl Params,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut statement = conn.prepare_cached(text)?;
    let rows = statement.query_map(values, map)?;
    rows.collect()
}

/// One row of `sql::VERSION_ROW`: availability, format, bitrate, depth,
/// on the planned album, an alias, on the release, and the album.
type TrackVersion = (
    String,
    String,
    Option<i64>,
    Option<i64>,
    bool,
    bool,
    bool,
    String,
);

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
    library: Imports,
    management: Management,
    releases: Prioritised<Arc<dyn ReleaseSource>>,
}

impl LibraryLanding {
    /// Port over the imports and management services `library` built and
    /// its release sources. The library's reads run on the imports'
    /// database.
    pub fn new(library: &LibrarySetup, releases: Prioritised<Arc<dyn ReleaseSource>>) -> Self {
        Self {
            library: library.imports.clone(),
            management: Management::new(library),
            releases,
        }
    }

    async fn group_tracks(&self, release_group_mbid: &str, album: Option<&str>) -> Vec<GroupTrack> {
        let values = (
            release_group_mbid.to_ascii_lowercase(),
            album.map(str::to_owned),
        );
        let read = self
            .library
            .db
            .read("landing.group_tracks", move |conn| {
                read_rows(conn, sql::GROUP_TRACKS.text(), values, |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .map_err(DbError::from)
            })
            .await;
        match read {
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
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        self.releases.at(priority).search(title, artist)
    }

    fn release<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        self.releases.at(priority).release(mbid)
    }

    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        self.releases.at(priority).canonical_recording(mbid)
    }

    fn chosen_edition<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            match crate::edition::chosen_edition(&self.library.db, release_group_mbid).await {
                Ok(chosen) => chosen.map(|chosen| chosen.release_mbid),
                Err(error) => {
                    tracing::warn!(%error, "chosen edition unreadable; the request's edition is used");
                    None
                }
            }
        })
    }

    fn owned<'a>(
        &'a self,
        release_group_mbid: &'a str,
        album: Option<&'a str>,
    ) -> BoxFuture<'a, OwnedTracks> {
        Box::pin(async move {
            let mut owned = OwnedTracks::default();
            for (release_track, recording, ..) in self.group_tracks(release_group_mbid, album).await
            {
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

    fn held_tier<'a>(
        &'a self,
        release_group_mbid: &'a str,
        album: Option<&'a str>,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let rows = self.group_tracks(release_group_mbid, album).await;
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
        album: Option<&'a str>,
        release_mbid: &'a str,
        release_track_mbid: &'a str,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, Vec<OwnedCopy>> {
        Box::pin(async move {
            let values = (
                release_group_mbid.to_ascii_lowercase(),
                album.map(str::to_owned),
            );
            let read = self
                .library
                .db
                .read("landing.owned_copies", move |conn| {
                    read_rows(conn, sql::GROUP_COPIES.text(), values, GroupCopy::from_row)
                        .map_err(DbError::from)
                })
                .await;
            let rows: Vec<GroupCopy> = match read {
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
                    album_id: row.album_id.clone(),
                    release_mbid: row.album_release_mbid.clone(),
                    tier: tier_of(&row.format, row.bitrate, row.depth),
                })
                .collect()
        })
    }

    fn version_row<'a>(
        &'a self,
        track_id: &'a str,
        planned: &'a PlannedAlbum,
        release: &'a Release,
    ) -> BoxFuture<'a, Option<VersionRow>> {
        Box::pin(async move {
            let release_tracks = serde_json::to_string(
                &release
                    .tracks
                    .iter()
                    .map(|track| track.id.to_ascii_lowercase())
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default();
            let values = (
                track_id.to_owned(),
                planned.album_id.clone(),
                planned.release_mbid.clone(),
                release_tracks,
            );
            let read = self
                .library
                .db
                .read("landing.version_row", move |conn| {
                    conn.query_row(
                        sql::VERSION_ROW.text(),
                        params![values.0, values.1, values.2, values.3],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                                row.get(7)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(DbError::from)
                })
                .await;
            let row: Option<TrackVersion> = match read {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(%error, track_id, "track to replace unreadable; nothing is replaced");
                    return None;
                }
            };
            let (availability, format, bitrate, depth, on_planned, alias, on_release, album) = row?;
            let held = match crate::edition::copy_is_held(&self.library.db, &album).await {
                Ok(held) => held,
                Err(error) => {
                    tracing::warn!(%error, album, "album hold unreadable; nothing is replaced");
                    return None;
                }
            };
            Some(VersionRow {
                on_planned,
                availability,
                alias,
                on_release,
                held,
                tier: tier_of(&format, bitrate, depth),
            })
        })
    }

    fn fingerprints(
        &self,
        files: Vec<(String, PathBuf)>,
    ) -> BoxFuture<'_, HashMap<String, Vec<String>>> {
        Box::pin(async move { self.library.fingerprints.identify_files(&files).await })
    }

    fn group_releases<'a>(
        &'a self,
        release_group_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        Box::pin(async move {
            Ok(
                group_editions(self.releases.at(priority).as_ref(), release_group_mbid)
                    .await?
                    .into_iter()
                    .map(|edition| ReleaseHit {
                        id: edition.release_mbid,
                        release_group_id: edition.release_group_mbid,
                        score: edition.score,
                        track_count: (edition.track_count > 0).then_some(edition.track_count),
                        title: edition.title,
                    })
                    .collect(),
            )
        })
    }

    fn fingerprint_releases(
        &self,
        files: Vec<(String, PathBuf)>,
    ) -> BoxFuture<'_, HashMap<String, Vec<String>>> {
        Box::pin(async move {
            self.library
                .fingerprints
                .identify_file_releases(&files)
                .await
        })
    }

    fn library_dirs(&self) -> Vec<PathBuf> {
        self.library
            .core
            .live_registry()
            .roots()
            .iter()
            .map(|root| root.path.clone())
            .collect()
    }

    fn import_folder(&self) -> Option<PathBuf> {
        let registry = self.library.core.live_registry();
        let usable: Vec<_> = registry
            .roots()
            .iter()
            .filter(|root| root.policy != EffectivePolicy::Excluded)
            .collect();
        let resolved: Vec<ResolvedRoot> = registry
            .roots()
            .iter()
            .map(|root| ResolvedRoot::of(&root.id, &root.path))
            .collect();
        let left_out = left_out(&resolved);
        usable
            .iter()
            .find(|root| root.policy == EffectivePolicy::Automatic && !left_out.contains(&root.id))
            .or_else(|| usable.first())
            .map(|root| root.path.clone())
    }

    fn import(
        &self,
        request: ImportRequest,
    ) -> BoxFuture<'_, Result<ImportReceipt, ImportFailure>> {
        Box::pin(async move {
            let library = self.library.clone();
            let import = DownloadImport {
                task_id: request.task_id,
                origin: request.origin,
                extras: request.extras,
                staging: request.staging,
                release: request.release,
                files: request
                    .files
                    .into_iter()
                    .map(|file| ImportSource {
                        path: file.path,
                        track: file.track,
                        replaces: file.replaces,
                    })
                    .collect(),
                actor: Some(request.actor),
            };
            let join = request.join;
            let beneath = request.read_beneath;
            let done = tokio::task::spawn_blocking(move || match beneath {
                Some(folders) => library.import_download_from(&import, join, &|path| {
                    super::confine::open_beneath(&folders, path)
                }),
                None => library.import_download_joined(&import, join),
            })
            .await
            .map_err(|error| ImportFailure::LocalFault(format!("import did not run: {error}")))?;
            match done {
                Ok(album) => Ok(ImportReceipt {
                    bundle_id: album.bundle_id,
                    album_id: album.album_id,
                    paths: album.paths,
                    skipped: album.skipped,
                    blocked: album.blocked,
                    tags_kept: album.tags_kept,
                }),
                Err(ImportError::LocalFault(detail)) => Err(ImportFailure::LocalFault(detail)),
                Err(ImportError::Occupied(detail)) => Err(ImportFailure::Occupied(detail)),
                Err(ImportError::NamingScript(detail)) => Err(ImportFailure::NamingScript(detail)),
                Err(ImportError::FolderMissing(folder)) => {
                    Err(ImportFailure::FolderMissing(folder))
                }
            }
        })
    }

    fn publish_settled<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            let management = self.management.clone();
            let task_id = task_id.to_owned();
            tokio::task::spawn_blocking(move || management.download_publish_settled(&task_id))
                .await
                .map_err(|error| format!("publish journal read did not run: {error}"))?
                .map_err(|error| error.to_string())
        })
    }

    fn placed_sources<'a>(
        &'a self,
        staging: &'a str,
    ) -> BoxFuture<'a, Result<Option<HashSet<String>>, String>> {
        Box::pin(async move {
            if !self.publish_settled(staging).await? {
                return Ok(None);
            }
            // An import stages under `<hidden prefix>import/<name>/` below
            // the library root (the held list's names need no escaping) and
            // records the hash of each file it copied in as the row's
            // source hash.
            let folder = format!(
                "{}import/{staging}/",
                droppedneedle_library::publish::HIDDEN_PREFIX
            );
            let hashes: Vec<String> = self
                .library
                .db
                .read("landing.placed_sources", move |conn| {
                    read_rows(conn, sql::PLACED_SOURCES.text(), [folder], |row| row.get(0))
                        .map_err(DbError::from)
                })
                .await
                .map_err(|error| format!("publish journal unreadable: {error}"))?;
            Ok(Some(hashes.into_iter().collect()))
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
