//! What one download task fetches: the release, its tracklist, and the
//! positions wanted from it.
//!
//! An album task fetches its chosen edition (see [`crate::acquire::edition`]);
//! its tracklist comes from MusicBrainz once and is kept in the task
//! manifest, so every source ranks candidates against it and the landing
//! verifies against the same release.
//!
//! A single-track task is fetched the way a person would search by hand:
//! the recording is resolved to an album ([`choose`]: the library's
//! edition of an album that carries it, else the release it was requested
//! from, else the best official release), the album is searched for, and
//! only the wanted track is taken from the best album folder. Lone-track
//! shares are a last resort, with the reason recorded in the manifest.
//! The resolution is written to the task row and the manifest once, and
//! every later attempt reuses it.

pub mod choose;
pub mod lookup;
pub mod reasons;
pub mod releases;
pub mod soulseek;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use self::lookup::{AlbumLookup, AlbumRelease};
use self::reasons::TrackReason;
use super::dispatch::Journal;
use super::downloads::manifest::{
    DownloadManifest, ExpectedTrack, ManifestCodec, TrackAlbumContext, TrackPosition,
};
use super::downloads::store::{TaskDetails, TaskRow, TrackAlbumColumns};
use super::edition::chosen_edition;

/// The MusicBrainz port, set once boot has built the client.
pub type LookupSlot = Arc<OnceLock<Arc<dyn AlbumLookup>>>;

/// Release lookups tried for one recording before giving up on an album.
const MAX_RELEASES_TRIED: usize = 4;

/// The single track a track task wants.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackWant {
    /// Track artist.
    pub artist: String,
    /// Track title.
    pub title: String,
    /// Canonical length in seconds, when known.
    pub duration_seconds: Option<f64>,
    /// Why no album could be searched for, when none could.
    pub no_album: Option<TrackReason>,
}

/// What to search for and how to judge the results.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchTarget {
    /// Album artist to search with.
    pub artist: String,
    /// Album title to search for; empty when the track has no album.
    pub album_title: String,
    /// Album year.
    pub year: Option<i32>,
    /// The release's audio tracks (empty when no release is known).
    pub tracklist: Vec<ExpectedTrack>,
    /// Positions to fetch (empty for a whole album).
    pub wanted: Vec<TrackPosition>,
    /// Set for a single-track task.
    pub track: Option<TrackWant>,
}

impl SearchTarget {
    /// Whether an album can be searched for.
    pub fn has_album(&self) -> bool {
        !self.album_title.trim().is_empty()
    }
}

/// Why a target could not be worked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// Try later: MusicBrainz is down or not wired.
    Unavailable(TrackReason),
    /// Our side failed (journal or staging).
    LocalFault(String),
}

/// Works out and remembers each task's target.
pub struct Targets {
    journal: Arc<Journal>,
    staging_root: PathBuf,
    lookup: LookupSlot,
}

impl Targets {
    /// Targets over the shared journal; manifests live under
    /// `staging_root`.
    pub fn new(journal: Arc<Journal>, staging_root: PathBuf) -> Self {
        Self {
            journal,
            staging_root,
            lookup: LookupSlot::default(),
        }
    }

    /// The slot boot puts the MusicBrainz port into.
    pub fn lookup_slot(&self) -> &LookupSlot {
        &self.lookup
    }

    /// The task's target, resolved now or read back from its manifest.
    pub async fn target(&self, task: &TaskRow) -> Result<SearchTarget, TargetError> {
        let details = {
            let task_id = task.id.clone();
            self.journal
                .run("downloads.target.details", move |store| {
                    store.task_details(&task_id)
                })
                .await
                .map_err(TargetError::LocalFault)?
        };
        let manifest = self.read_manifest(&task.id).await;
        if task.download_type == "track" {
            self.track_target(task, &details, manifest).await
        } else {
            Ok(self.album_target(task, &details, manifest).await)
        }
    }

    /// Record that a track came from a lone-track share, and why.
    pub async fn record_lone_track(&self, task: &TaskRow, reason: TrackReason) {
        tracing::info!(
            task_id = %task.id,
            code = reason.code(),
            "single track taken from a lone-track share: {}",
            reason.message()
        );
        let mut manifest = self.manifest_or_new(task).await;
        let context = manifest
            .track_album
            .get_or_insert_with(|| TrackAlbumContext {
                release_group_mbid: String::new(),
                release_mbid: String::new(),
                album_title: String::new(),
                album_artist: String::new(),
                year: None,
                basis: "no_album".to_owned(),
                wanted: Vec::new(),
                lone_track_reason: None,
            });
        context.lone_track_reason = Some(reason.record());
        self.write_manifest(manifest).await;
    }

    async fn album_target(
        &self,
        task: &TaskRow,
        details: &TaskDetails,
        manifest: Option<DownloadManifest>,
    ) -> SearchTarget {
        let release = details
            .release_mbid
            .clone()
            .or_else(|| manifest.as_ref().and_then(|m| m.release_mbid.clone()))
            .map(|mbid| mbid.trim().to_ascii_lowercase())
            .filter(|mbid| !mbid.is_empty());
        let mut tracklist = manifest
            .as_ref()
            .filter(|m| m.release_mbid.as_deref().map(str::to_ascii_lowercase) == release)
            .map(|m| m.expected_tracks.clone())
            .unwrap_or_default();
        if tracklist.is_empty()
            && let Some(release_mbid) = release.as_deref()
            && let Some(lookup) = self.lookup.get()
        {
            // An album still downloads when MusicBrainz is down; it is only
            // ranked on file counts until the tracklist can be read.
            match lookup.release(release_mbid).await {
                Ok(Some(found)) => {
                    tracklist = expected_tracks(&found);
                    let mut manifest = match manifest {
                        Some(manifest) => manifest,
                        None => self.manifest_or_new(task).await,
                    };
                    manifest.release_mbid = Some(found.id.clone());
                    manifest.expected_tracks = tracklist.clone();
                    self.write_manifest(manifest).await;
                }
                Ok(None) => tracing::warn!(
                    task_id = %task.id,
                    release_mbid,
                    "MusicBrainz does not know the chosen edition; ranking on file counts"
                ),
                Err(error) => tracing::warn!(
                    task_id = %task.id,
                    %error,
                    "edition tracklist unavailable; ranking on file counts"
                ),
            }
        }
        SearchTarget {
            artist: task.artist_name.clone(),
            album_title: task.album_title.clone(),
            year: details.year,
            tracklist,
            wanted: Vec::new(),
            track: None,
        }
    }

    async fn track_target(
        &self,
        task: &TaskRow,
        details: &TaskDetails,
        manifest: Option<DownloadManifest>,
    ) -> Result<SearchTarget, TargetError> {
        let title = details
            .track_title
            .clone()
            .unwrap_or_else(|| task.album_title.clone());
        // Already resolved for this task: reuse it.
        if let Some(manifest) = manifest.as_ref()
            && let Some(context) = manifest.track_album.as_ref()
            && !context.release_mbid.is_empty()
            && details
                .release_mbid
                .as_deref()
                .is_some_and(|release| release.eq_ignore_ascii_case(&context.release_mbid))
        {
            let duration = context.wanted.first().and_then(|position| {
                manifest
                    .expected_tracks
                    .iter()
                    .find(|track| same_position(track, *position))
                    .and_then(|track| track.duration_seconds)
            });
            return Ok(SearchTarget {
                artist: context.album_artist.clone(),
                album_title: context.album_title.clone(),
                year: context.year,
                tracklist: manifest.expected_tracks.clone(),
                wanted: context.wanted.clone(),
                track: Some(TrackWant {
                    artist: task.artist_name.clone(),
                    title,
                    duration_seconds: duration,
                    no_album: None,
                }),
            });
        }
        let lookup = self.lookup.get().cloned().ok_or(TargetError::Unavailable(
            TrackReason::AlbumLookupUnavailable,
        ))?;
        let recording = task
            .recording_mbid
            .as_deref()
            .map(|mbid| mbid.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let lone = |reason: TrackReason| SearchTarget {
            artist: task.artist_name.clone(),
            album_title: String::new(),
            year: None,
            tracklist: vec![ExpectedTrack {
                track_number: 0,
                disc_number: 1,
                duration_seconds: None,
                recording_mbid: Some(recording.clone()),
                title: Some(title.clone()),
                release_track_mbid: None,
            }],
            wanted: vec![TrackPosition { disc: 1, track: 0 }],
            track: Some(TrackWant {
                artist: task.artist_name.clone(),
                title: title.clone(),
                duration_seconds: None,
                no_album: Some(reason),
            }),
        };
        let unavailable = |error: lookup::LookupError| {
            tracing::warn!(task_id = %task.id, %error, "track album lookup failed");
            TargetError::Unavailable(TrackReason::AlbumLookupUnavailable)
        };
        let candidates = match lookup
            .recording_releases(&recording)
            .await
            .map_err(unavailable)?
        {
            None => return Ok(lone(TrackReason::RecordingUnknown)),
            Some(candidates) => candidates,
        };
        let mut library = HashMap::new();
        for group in choose::groups_to_check(&candidates) {
            match chosen_edition(self.journal.db().pool(), &group).await {
                Ok(Some(edition)) => {
                    library.insert(group, edition);
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(TargetError::LocalFault(format!(
                        "read the library edition: {error}"
                    )));
                }
            }
        }
        let order = choose::release_order(&candidates, &library, details.release_mbid.as_deref());
        for (release_mbid, basis) in order.into_iter().take(MAX_RELEASES_TRIED) {
            let Some(release) = lookup.release(&release_mbid).await.map_err(unavailable)? else {
                continue;
            };
            let wanted: Vec<TrackPosition> = release
                .tracks
                .iter()
                .filter(|track| track.recording_mbid == recording)
                .map(|track| TrackPosition {
                    disc: track.disc,
                    track: track.position,
                })
                .take(1)
                .collect();
            if wanted.is_empty() {
                continue;
            }
            return self
                .remember(task, &release, basis, wanted, title.clone())
                .await;
        }
        Ok(lone(TrackReason::NoAlbumForRecording))
    }

    /// Write a track's album to the task row and the manifest, and answer
    /// the target it gives.
    async fn remember(
        &self,
        task: &TaskRow,
        release: &AlbumRelease,
        basis: &str,
        wanted: Vec<TrackPosition>,
        title: String,
    ) -> Result<SearchTarget, TargetError> {
        let wanted_track = release.tracks.iter().find(|track| {
            wanted
                .first()
                .is_some_and(|p| p.disc == track.disc && p.track == track.position)
        });
        let album_artist = if release.various_artists || release.artist.is_empty() {
            task.artist_name.clone()
        } else {
            release.artist.clone()
        };
        let columns = TrackAlbumColumns {
            release_group_mbid: release.release_group_mbid.clone(),
            release_mbid: release.id.clone(),
            album_title: release.title.clone(),
            release_track_mbid: wanted_track.map(|track| track.release_track_mbid.clone()),
            track_number: wanted_track.map_or(0, |track| i64::from(track.position)),
            disc_number: wanted_track.map_or(1, |track| i64::from(track.disc)),
            track_count: i64::try_from(release.tracks.len()).unwrap_or(0),
            duration_seconds: wanted_track.and_then(|track| track.duration_seconds),
            year: release.year,
        };
        {
            let task_id = task.id.clone();
            self.journal
                .run("downloads.target.track_album", move |store| {
                    store.set_track_album(&task_id, &columns, now_unix_f64())
                })
                .await
                .map_err(TargetError::LocalFault)?;
        }
        let tracklist = expected_tracks(release);
        let mut manifest = self.manifest_or_new(task).await;
        manifest.release_group_mbid = release.release_group_mbid.clone();
        manifest.release_mbid = Some(release.id.clone());
        manifest.album_title = release.title.clone();
        manifest.year = release.year.map(i64::from);
        manifest.expected_tracks = tracklist.clone();
        manifest.track_album = Some(TrackAlbumContext {
            release_group_mbid: release.release_group_mbid.clone(),
            release_mbid: release.id.clone(),
            album_title: release.title.clone(),
            album_artist: album_artist.clone(),
            year: release.year,
            basis: basis.to_owned(),
            wanted: wanted.clone(),
            lone_track_reason: None,
        });
        self.write_manifest(manifest).await;
        tracing::info!(
            task_id = %task.id,
            release_mbid = %release.id,
            basis,
            "single track resolved to its album"
        );
        Ok(SearchTarget {
            artist: album_artist,
            album_title: release.title.clone(),
            year: release.year,
            tracklist,
            wanted,
            track: Some(TrackWant {
                artist: task.artist_name.clone(),
                title,
                duration_seconds: wanted_track.and_then(|track| track.duration_seconds),
                no_album: None,
            }),
        })
    }

    async fn read_manifest(&self, task_id: &str) -> Option<DownloadManifest> {
        let path = ManifestCodec::checked_path(&self.staging_root, task_id)?;
        let bytes = tokio::fs::read(path).await.ok()?;
        match ManifestCodec.decode(&bytes) {
            Ok(manifest) => Some(manifest),
            Err(error) => {
                tracing::warn!(task_id, %error, "task manifest unreadable; rebuilding it");
                None
            }
        }
    }

    async fn manifest_or_new(&self, task: &TaskRow) -> DownloadManifest {
        match self.read_manifest(&task.id).await {
            Some(manifest) => manifest,
            None => DownloadManifest {
                task_id: task.id.clone(),
                release_group_mbid: task.release_group_mbid.clone(),
                artist_name: task.artist_name.clone(),
                album_title: task.album_title.clone(),
                naming_template: String::new(),
                target_files: Vec::new(),
                source_username: None,
                handle: None,
                expected_tracks: Vec::new(),
                release_mbid: None,
                artist_mbid: None,
                year: None,
                is_track: task.download_type == "track",
                hold_on_wrong_track: false,
                origin: task.origin.clone(),
                requested_by_user_id: None,
                attempt_id: None,
                track_album: None,
            },
        }
    }

    /// Best effort, like the dispatch skeleton: a missing manifest only
    /// means the next attempt resolves again.
    async fn write_manifest(&self, manifest: DownloadManifest) {
        let staging_root = self.staging_root.clone();
        let task_id = manifest.task_id.clone();
        let written =
            tokio::task::spawn_blocking(move || ManifestCodec.write(&staging_root, &manifest))
                .await;
        match written {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(task_id, %error, "target manifest write failed"),
            Err(error) => tracing::warn!(task_id, %error, "target manifest write join failed"),
        }
    }
}

fn same_position(track: &ExpectedTrack, position: TrackPosition) -> bool {
    track.disc_number == i64::from(position.disc) && track.track_number == i64::from(position.track)
}

/// The release's tracks as the manifest's expected map.
pub fn expected_tracks(release: &AlbumRelease) -> Vec<ExpectedTrack> {
    release
        .tracks
        .iter()
        .map(|track| ExpectedTrack {
            track_number: i64::from(track.position),
            disc_number: i64::from(track.disc),
            duration_seconds: track.duration_seconds,
            recording_mbid: Some(track.recording_mbid.clone()),
            title: Some(track.title.clone()),
            release_track_mbid: Some(track.release_track_mbid.clone()),
        })
        .collect()
}

fn now_unix_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
