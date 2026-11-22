//! What one download task fetches: the release, its tracklist, and the
//! positions wanted from it.
//!
//! An album task fetches its chosen edition (see [`crate::edition`]);
//! its tracklist comes from MusicBrainz once and is kept in the task's
//! plan, so every source ranks candidates against it and the landing
//! verifies against the same release.
//!
//! A single-track task is fetched the way a person would search by hand:
//! the recording is resolved to an album ([`choose`]: the library's
//! edition of an album that carries it, else the release it was requested
//! from, else the best official release), the album is searched for, and
//! only the wanted track is taken from the best album folder. Lone-track
//! shares are a last resort, with the reason logged. The resolution is
//! written to the task row and its plan once, and every later attempt
//! reuses it.

pub mod choose;
pub mod lookup;
pub mod reasons;
pub mod releases;
pub mod soulseek;

use std::collections::HashMap;
use std::sync::Arc;

use self::lookup::{AlbumLookup, AlbumRelease};
use self::reasons::TrackReason;
use super::downloads::journal::Journal;
use super::downloads::plan::{ExpectedTrack, TaskPlan, TrackAlbum, TrackPosition};
use super::downloads::store::{TaskDetails, TaskRow, TrackAlbumColumns};
use super::edition::{EditionBasis, chosen_edition, library_holds};
use droppedneedle_catalog::metadata::Catalog;
use droppedneedle_providers::RequestPriority;

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
    /// The release's barcode, when it has one and the plan kept it.
    pub barcode: Option<String>,
    /// Positions to fetch (empty for a whole album).
    pub wanted: Vec<TrackPosition>,
    /// Set for a single-track task.
    pub track: Option<TrackWant>,
    /// For an album: no person chose the release, so the landing may take
    /// another edition of the album. False for a pin and for a track.
    pub release_preferred: bool,
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
    /// Try later: MusicBrainz is down.
    Unavailable(TrackReason),
    /// Our side failed: the journal, or a plan that does not read.
    LocalFault(String),
}

/// Works out and remembers each task's target.
pub struct Targets {
    journal: Arc<Journal>,
    /// MusicBrainz reads.
    lookup: Arc<dyn AlbumLookup>,
    /// The album page's edition, preferred for an album no person picked
    /// an edition for.
    catalog: Catalog,
}

impl Targets {
    /// Targets over the shared journal, reading MusicBrainz through
    /// `lookup` and the album page's edition through `catalog`.
    pub fn new(journal: Arc<Journal>, lookup: Arc<dyn AlbumLookup>, catalog: Catalog) -> Self {
        Self {
            journal,
            lookup,
            catalog,
        }
    }

    /// The edition the album page shows for `group`, when only the
    /// server's edition preferences chose it: what a download no person
    /// picked an edition for prefers. `None` while the library holds the
    /// group (its copy decides, even one that names no release), while the
    /// preferences do not read, or when the lookup fails (logged; the next
    /// search asks again). MusicBrainz is asked at `priority`.
    pub async fn preferred_edition(
        &self,
        group: &str,
        priority: RequestPriority,
    ) -> Option<String> {
        let group = group.trim().to_ascii_lowercase();
        if group.is_empty() {
            return None;
        }
        match library_holds(self.journal.db().db(), &group).await {
            Ok(false) => {}
            Ok(true) => return None,
            Err(error) => {
                tracing::warn!(release_group_mbid = %group, %error, "library holdings unreadable; no preferred edition");
                return None;
            }
        }
        match self.catalog.request_edition(&group, priority).await {
            Ok(edition) => edition.map(|mbid| mbid.trim().to_ascii_lowercase()),
            Err(error) => {
                tracing::warn!(release_group_mbid = %group, %error, "album page edition unavailable; no preferred edition");
                None
            }
        }
    }

    /// The task's target, resolved now or read back from its plan. A plan
    /// that does not read is a local fault: the task waits, never runs
    /// without it.
    pub async fn target(&self, task: &TaskRow) -> Result<SearchTarget, TargetError> {
        let (details, plan) = {
            let task_id = task.id.clone();
            self.journal
                .read("downloads.target.details", move |store| {
                    Ok((store.task_details(&task_id)?, store.task_plan(&task_id)?))
                })
                .await
                .map_err(|error| {
                    tracing::warn!(task_id = %task.id, %error, "task details or plan unreadable");
                    TargetError::LocalFault(error.to_string())
                })?
        };
        if task.download_type == "track" {
            self.track_target(task, &details, &plan).await
        } else {
            Ok(self.album_target(task, &details, &plan).await)
        }
    }

    /// When a release group first came out, as MusicBrainz dates it.
    /// `None` when MusicBrainz does not know or date the group, or the
    /// lookup failed (logged); nothing is guessed.
    pub async fn first_release_date(&self, release_group_mbid: &str) -> Option<String> {
        match self.lookup.first_release_date(release_group_mbid).await {
            Ok(date) => date,
            Err(error) => {
                tracing::warn!(release_group_mbid, %error, "release group date unavailable");
                None
            }
        }
    }

    /// One edition's audio tracklist and barcode, for ranking a manual
    /// search. Empty and `None` when MusicBrainz is down or does not know
    /// the release; folders are then judged on their file counts.
    pub async fn edition_tracklist(
        &self,
        release_mbid: &str,
    ) -> (Vec<ExpectedTrack>, Option<String>) {
        match self.lookup.release(release_mbid).await {
            Ok(Some(found)) => (expected_tracks(&found), found.barcode),
            Ok(None) => {
                tracing::warn!(
                    release_mbid,
                    "MusicBrainz does not know the searched edition"
                );
                (Vec::new(), None)
            }
            Err(error) => {
                tracing::warn!(release_mbid, %error, "edition tracklist unavailable for a manual search");
                (Vec::new(), None)
            }
        }
    }

    /// Log that a track came from a lone-track share, and why.
    pub fn record_lone_track(&self, task: &TaskRow, reason: TrackReason) {
        tracing::info!(
            task_id = %task.id,
            code = reason.code(),
            "single track taken from a lone-track share: {}",
            reason.message()
        );
    }

    async fn album_target(
        &self,
        task: &TaskRow,
        details: &TaskDetails,
        plan: &TaskPlan,
    ) -> SearchTarget {
        let mut release = details
            .release_mbid
            .clone()
            .or_else(|| plan.release_mbid.clone())
            .map(|mbid| mbid.trim().to_ascii_lowercase())
            .filter(|mbid| !mbid.is_empty());
        // With no edition asked for, the album page's edition is preferred
        // (never for an upgrade or a copy's missing tracks).
        let mut preferred = false;
        if release.is_none()
            && plan.planned_album.is_none()
            && task.origin != crate::dispatch::DispatchOrigin::Upgrade
            && let Some(edition) = self
                .preferred_edition(&task.release_group_mbid, RequestPriority::BackgroundSync)
                .await
        {
            release = Some(edition);
            preferred = true;
        }
        let same_release = release
            .as_deref()
            .is_some_and(|release| plan.is_for(release));
        // As the landing reads it: a release the plan is not for is a pin.
        let release_preferred = preferred || (plan.release_preferred && same_release);
        let (mut tracklist, mut barcode) = if same_release {
            (plan.expected_tracks.clone(), plan.barcode.clone())
        } else {
            (Vec::new(), None)
        };
        // "Get missing tracks" asks for some positions of the edition only.
        let wanted = if same_release {
            plan.album_wanted.clone()
        } else {
            Vec::new()
        };
        if tracklist.is_empty()
            && let Some(release_mbid) = release.as_deref()
        {
            // An album still downloads when MusicBrainz is down; it is only
            // ranked on file counts until the tracklist can be read.
            match self.lookup.release(release_mbid).await {
                Ok(Some(found)) => {
                    tracklist = expected_tracks(&found);
                    barcode = found.barcode.clone();
                    if preferred {
                        self.keep_preference(task, release_mbid, &found, &tracklist)
                            .await;
                    } else {
                        self.keep_tracklist(task, release_mbid, &found, &tracklist)
                            .await;
                    }
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
            // Without the tracklist the source matches these by file name.
            wanted,
            tracklist,
            barcode,
            track: None,
            release_preferred,
        }
    }

    /// Keep an album's tracklist and barcode in its plan, under the release
    /// MusicBrainz answered for `asked` (the one it redirected to, when it
    /// did). The copy fetched for stays only while it belongs to that
    /// release. A failed write is logged: the next attempt reads the
    /// tracklist again.
    async fn keep_tracklist(
        &self,
        task: &TaskRow,
        asked: &str,
        found: &AlbumRelease,
        tracklist: &[ExpectedTrack],
    ) {
        let task_id = task.id.clone();
        let (asked, barcode) = (asked.to_owned(), found.barcode.clone());
        let found = found.id.clone();
        let tracklist = tracklist.to_vec();
        let clock = self.journal.db().clock().clone();
        let kept = self
            .journal
            .run("downloads.target.tracklist", move |store| {
                let mut plan = store.task_plan(&task_id)?;
                if !(plan.is_for(&asked) || plan.is_for(&found)) {
                    plan.planned_album = None;
                }
                plan.redirected_from = (!asked.eq_ignore_ascii_case(&found)).then_some(asked);
                plan.release_mbid = Some(found);
                plan.expected_tracks = tracklist;
                plan.barcode = barcode;
                let now = crate::db::unix_secs_f64(clock.as_ref());
                store.set_task_plan(&task_id, &plan, now)
            })
            .await;
        if let Err(error) = kept {
            tracing::warn!(task_id = %task.id, %error, "album tracklist not kept in the task plan");
        }
    }

    /// Keep the album page's edition as the task's preferred release, with
    /// its tracklist and barcode under the release MusicBrainz answered for
    /// `asked`. Only while the task still asks for no release: a release
    /// written meanwhile (in its details or its plan) stands. A failed
    /// write is logged: the next search fills it again.
    async fn keep_preference(
        &self,
        task: &TaskRow,
        asked: &str,
        found: &AlbumRelease,
        tracklist: &[ExpectedTrack],
    ) {
        let task_id = task.id.clone();
        let (asked, barcode) = (asked.to_owned(), found.barcode.clone());
        let found = found.id.clone();
        let release = found.clone();
        let tracklist = tracklist.to_vec();
        let clock = self.journal.db().clock().clone();
        let kept = self
            .journal
            .run("downloads.target.preference", move |store| {
                let mut plan = store.task_plan(&task_id)?;
                let asked_meanwhile = store
                    .task_details(&task_id)?
                    .release_mbid
                    .is_some_and(|release| !release.trim().is_empty());
                if asked_meanwhile || plan.release_mbid.is_some() || plan.planned_album.is_some() {
                    return Ok(false);
                }
                plan.redirected_from = (!asked.eq_ignore_ascii_case(&found)).then_some(asked);
                plan.release_mbid = Some(found);
                plan.expected_tracks = tracklist;
                plan.barcode = barcode;
                plan.release_preferred = true;
                let now = crate::db::unix_secs_f64(clock.as_ref());
                store.set_task_plan(&task_id, &plan, now)?;
                Ok(true)
            })
            .await;
        match kept {
            Ok(true) => tracing::info!(
                task_id = %task.id,
                release_mbid = %release,
                "album page edition preferred for the download"
            ),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "preferred edition not kept in the task plan");
            }
        }
    }

    async fn track_target(
        &self,
        task: &TaskRow,
        details: &TaskDetails,
        plan: &TaskPlan,
    ) -> Result<SearchTarget, TargetError> {
        let title = details
            .track_title
            .clone()
            .unwrap_or_else(|| task.album_title.clone());
        // Already resolved for this task: reuse it. The album's title and
        // year are on the row.
        if let Some(album) = plan.track_album.as_ref()
            && let Some(release) = plan.release_mbid.as_deref().filter(|id| !id.is_empty())
            && details
                .release_mbid
                .as_deref()
                .is_some_and(|asked| asked.eq_ignore_ascii_case(release))
        {
            let duration = album.wanted.first().and_then(|position| {
                plan.expected_tracks
                    .iter()
                    .find(|track| same_position(track, *position))
                    .and_then(|track| track.duration_seconds)
            });
            return Ok(SearchTarget {
                artist: album.album_artist.clone(),
                album_title: task.album_title.clone(),
                year: details.year,
                tracklist: plan.expected_tracks.clone(),
                barcode: plan.barcode.clone(),
                wanted: album.wanted.clone(),
                release_preferred: false,
                track: Some(TrackWant {
                    artist: task.artist_name.clone(),
                    title,
                    duration_seconds: duration,
                    no_album: None,
                }),
            });
        }
        let lookup = &self.lookup;
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
                isrcs: Vec::new(),
            }],
            barcode: None,
            wanted: vec![TrackPosition { disc: 1, track: 0 }],
            release_preferred: false,
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
            match chosen_edition(self.journal.db().db(), &group).await {
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

    /// Write a track's album to the task row and its plan in one
    /// transaction, and answer the target it gives.
    async fn remember(
        &self,
        task: &TaskRow,
        release: &AlbumRelease,
        basis: EditionBasis,
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
        let tracklist = expected_tracks(release);
        {
            let task_id = task.id.clone();
            let clock = self.journal.db().clock().clone();
            let release_mbid = release.id.clone();
            let track_album = TrackAlbum {
                album_artist: album_artist.clone(),
                basis,
                wanted: wanted.clone(),
            };
            let tracks = tracklist.clone();
            let barcode = release.barcode.clone();
            self.journal
                .run("downloads.target.track_album", move |store| {
                    let now = crate::db::unix_secs_f64(clock.as_ref());
                    store.set_track_album(&task_id, &columns, now)?;
                    let mut plan = store.task_plan(&task_id)?;
                    plan.release_mbid = Some(release_mbid);
                    plan.redirected_from = None;
                    plan.expected_tracks = tracks;
                    plan.barcode = barcode;
                    plan.track_album = Some(track_album);
                    store.set_task_plan(&task_id, &plan, now)
                })
                .await
                .map_err(|error| TargetError::LocalFault(error.to_string()))?;
        }
        tracing::info!(
            task_id = %task.id,
            release_mbid = %release.id,
            basis = basis.as_str(),
            "single track resolved to its album"
        );
        Ok(SearchTarget {
            artist: album_artist,
            album_title: release.title.clone(),
            year: release.year,
            tracklist,
            barcode: release.barcode.clone(),
            wanted,
            release_preferred: false,
            track: Some(TrackWant {
                artist: task.artist_name.clone(),
                title,
                duration_seconds: wanted_track.and_then(|track| track.duration_seconds),
                no_album: None,
            }),
        })
    }
}

fn same_position(track: &ExpectedTrack, position: TrackPosition) -> bool {
    track.disc_number == i64::from(position.disc) && track.track_number == i64::from(position.track)
}

/// The release's tracks as the plan's tracklist.
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
            isrcs: track.isrcs.clone(),
        })
        .collect()
}
