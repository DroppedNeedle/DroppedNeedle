//! "Get missing tracks": fetch what the album's chosen edition lacks and
//! better copies of its tracks below the quality cutoff.
//!
//! The library's edition plan names the positions,
//! and one album download on the chosen release asks for exactly those:
//! the missing tracks, plus the owned tracks below the cutoff, which the
//! landing replaces only with a strictly better file. A track a file of
//! the copy is another version of (the same title and artist, not this
//! edition's recording or length) is asked for only with
//! `replace_versions`, and never on a copy the app leaves alone: the
//! plan names that file, and the landing replaces it when the new one
//! is as good or better. Without it the file stays and its track is not
//! asked for. The best whole-album source is searched for, the way a
//! person would by hand. Only one such download runs per album, and none
//! while another download of the album is still working. Existing files
//! are never retagged.

use std::collections::HashSet;

use super::RequestsService;
use crate::dispatch::{
    DispatchError, DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget,
};
use crate::downloads::plan::{PlannedAlbum, ReplaceVersion};
use crate::downloads::{DownloadStore, StoreError, TaskStatus};
use crate::edition::{copy_is_held, local_names};
use crate::flows::stores::UpgradeWorklist;
use crate::held::store::HeldRows;
use crate::requests::error::RequestsError;
use crate::requests::ledger::{EditionMark, validate_mbid};
use crate::requests::models::{
    EditionAcquireResponse, EditionFetchResponse, EditionFetchState, EditionFetchTrack,
    EditionFetchTrackState, EditionFetchView,
};
use crate::requests::sql;
use droppedneedle_auth::Curator;
use droppedneedle_kernel::db::DbError;
use droppedneedle_library::operations::models::OperationError;
use droppedneedle_library::operations::plan::{EditionPlan, FileFate, PlanTrack};
use rusqlite::OptionalExtension;

/// Bytes a track takes in staging, at least.
const TRACK_FLOOR_BYTES: u64 = 8 * 1024 * 1024;
/// Bytes per second of audio assumed for the estimate (lossless-ish).
const BYTES_PER_SECOND: u64 = 32_000;
/// Room kept free on the staging disk on top of the estimate.
const DISK_SAFETY_BYTES: u64 = 256 * 1024 * 1024;

/// What the edition still needs, from the library's plan.
#[derive(Debug, Default)]
struct Gap {
    album_id: String,
    release_mbid: String,
    artist_name: String,
    title: String,
    total_tracks: u32,
    missing: Vec<(u32, u32)>,
    upgrades: Vec<(u32, u32)>,
    /// Tracks asked for in this edition's version, with the file each
    /// replaces.
    versions: Vec<((u32, u32), ReplaceVersion)>,
    /// Tracks a file of the copy is another version of, asked for or not.
    other_versions: usize,
    /// The copy's files are left as they are (see [`copy_is_held`]).
    held: bool,
    estimate_bytes: u64,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn track_bytes(track: &PlanTrack) -> u64 {
    let seconds = track.length_ms.unwrap_or(240_000) / 1000;
    (seconds * BYTES_PER_SECOND).max(TRACK_FLOOR_BYTES)
}

/// The positions to ask for: the missing tracks, the owned tracks whose
/// files sit below the cutoff (`below` names those files) and, with
/// `replace` on a copy that is not `held`, the tracks a file is another
/// version of. A track a file is another version of is never missing.
fn gap_of(plan: &EditionPlan, below: &[String], replace: bool, held: bool) -> Gap {
    let spot = |track: &PlanTrack| Some((track.disc, track.position));
    let others: Vec<(&PlanTrack, &str)> = plan
        .files
        .iter()
        .filter(|file| file.fate == FileFate::OtherVersion)
        .filter_map(|file| {
            let track = plan
                .missing()
                .find(|track| spot(track) == file.edition_track)?;
            Some((track, file.local_track_id.as_str()))
        })
        .collect();
    let missing: Vec<&PlanTrack> = plan
        .missing()
        .filter(|track| !others.iter().any(|(other, _)| spot(other) == spot(track)))
        .collect();
    let versions: &[(&PlanTrack, &str)] = if replace && !held { &others } else { &[] };
    let upgrades: Vec<&PlanTrack> = plan
        .tracks
        .iter()
        .filter(|track| {
            track
                .local_track_id
                .as_ref()
                .is_some_and(|id| below.contains(id))
        })
        .collect();
    Gap {
        album_id: plan.local_album_id.clone(),
        release_mbid: plan.release_mbid.clone(),
        artist_name: plan.artist_name.clone(),
        title: plan.title.clone(),
        total_tracks: count(plan.tracks.len()),
        estimate_bytes: missing
            .iter()
            .chain(&upgrades)
            .chain(versions.iter().map(|(track, _)| track))
            .map(|track| track_bytes(track))
            .sum(),
        missing: missing.iter().map(|t| (t.disc, t.position)).collect(),
        upgrades: upgrades.iter().map(|t| (t.disc, t.position)).collect(),
        versions: versions
            .iter()
            .map(|(track, file)| {
                (
                    (track.disc, track.position),
                    ReplaceVersion {
                        release_track_mbid: track.release_track_mbid.clone(),
                        local_track_id: (*file).to_owned(),
                    },
                )
            })
            .collect(),
        other_versions: others.len(),
        held,
    }
}

/// "the 1 missing track", "the 3 missing tracks".
fn missing_words(n: u32) -> String {
    if n == 1 {
        "the 1 missing track".to_owned()
    } else {
        format!("the {n} missing tracks")
    }
}

/// What a started download says it fetches.
fn started_message(requested: Option<u32>, versions: u32) -> String {
    let tracks = if versions == 1 { "track" } else { "tracks" };
    match (requested, versions) {
        (Some(0), 0) => "Looking for better copies of this edition's tracks.".to_owned(),
        (Some(0), v) => format!("Downloading this edition's version of {v} of your {tracks}."),
        (Some(n), 0) => format!("Downloading {} of this edition.", missing_words(n)),
        (Some(n), v) => format!(
            "Downloading {} of this edition and {v} in this edition's version.",
            missing_words(n)
        ),
        (None, _) => "Downloading this edition.".to_owned(),
    }
}

/// What the download journal says about one edition fetch.
struct FetchFacts {
    task_id: String,
    status: TaskStatus,
    release_mbid: Option<String>,
    /// The positions asked for, disc first.
    wanted: Vec<(u32, u32)>,
    replace_versions: bool,
    /// Positions a landing imported or found in the library.
    landed: HashSet<(u32, u32)>,
    /// A landing imported every track the release lacked.
    all_landed: bool,
    /// Positions whose file waits in the held list.
    held: HashSet<(u32, u32)>,
}

impl FetchFacts {
    fn view(self) -> EditionFetchView {
        let state = match self.status {
            TaskStatus::Queued => EditionFetchState::Waiting,
            TaskStatus::Downloading | TaskStatus::Processing => EditionFetchState::Fetching,
            TaskStatus::Completed => EditionFetchState::Done,
            TaskStatus::Partial | TaskStatus::Failed => EditionFetchState::Failed,
            TaskStatus::Cancelled => EditionFetchState::Stopped,
        };
        let tracks: Vec<EditionFetchTrack> = self
            .wanted
            .iter()
            .map(|&(disc, position)| EditionFetchTrack {
                disc,
                position,
                state: self.track_state((disc, position)),
            })
            .collect();
        // A held track waits for a person, not for another download.
        let can_retry = matches!(
            state,
            EditionFetchState::Failed | EditionFetchState::Stopped
        ) && (tracks.is_empty()
            || tracks
                .iter()
                .any(|track| track.state == EditionFetchTrackState::Failed));
        EditionFetchView {
            task_id: self.task_id,
            release_mbid: self.release_mbid,
            state,
            tracks,
            replace_versions: self.replace_versions,
            can_retry,
            can_stop: matches!(
                state,
                EditionFetchState::Waiting | EditionFetchState::Fetching
            ),
        }
    }

    fn track_state(&self, at: (u32, u32)) -> EditionFetchTrackState {
        if self.held.contains(&at) {
            return EditionFetchTrackState::Held;
        }
        if self.all_landed || self.landed.contains(&at) {
            return EditionFetchTrackState::Landed;
        }
        match self.status {
            TaskStatus::Queued => EditionFetchTrackState::Waiting,
            TaskStatus::Downloading | TaskStatus::Processing => EditionFetchTrackState::Fetching,
            TaskStatus::Completed => EditionFetchTrackState::Landed,
            TaskStatus::Partial | TaskStatus::Failed | TaskStatus::Cancelled => {
                EditionFetchTrackState::Failed
            }
        }
    }
}

/// The fetch a mark names, in one read: its download, or the retry the
/// queue started after it for the album (same owner and origin, carrying
/// the same plan). `None` once the download left the queue.
///
/// A track counts as landed once a landing of either download imported
/// the whole release, or left it off the positions still missing. A track
/// the copy already held (an upgrade) counts as landed then too, whether
/// or not a better file came.
fn fetch_facts(
    store: &DownloadStore<'_>,
    mark: &EditionMark,
) -> Result<Option<FetchFacts>, StoreError> {
    let Some(first) = store.get_task(&mark.task_id)? else {
        return Ok(None);
    };
    let mut current = first.clone();
    if first.status.is_terminal()
        && let Some(id) = store.task_since(
            &first.user_id,
            "album",
            &first.release_group_mbid,
            mark.started_at as f64,
        )?
        && id != first.id
        && let Some(retry) = store.get_task(&id)?
        && retry.origin == first.origin
    {
        current = retry;
    }
    let plan = store.task_plan(&current.id)?;
    let wanted: Vec<(u32, u32)> = plan
        .album_wanted
        .iter()
        .map(|at| (at.disc.max(1), at.track))
        .collect();
    let mut landed = HashSet::new();
    let mut all_landed = false;
    let mut held = HashSet::new();
    let rows = HeldRows::new(store.conn());
    let tasks = if current.id == first.id {
        vec![first.id.clone()]
    } else {
        vec![first.id.clone(), current.id.clone()]
    };
    for task_id in &tasks {
        if let Some(import) = store.latest_import(task_id)? {
            if import.outcome == "imported" {
                all_landed = true;
            } else {
                let missing: Vec<(u32, u32)> = serde_json::from_str(&import.missing_positions)
                    .unwrap_or_else(|error| {
                        tracing::warn!(task_id, %error, "missing positions do not decode");
                        wanted.clone()
                    });
                landed.extend(wanted.iter().filter(|at| !missing.contains(at)));
            }
        }
        for entry in rows.list(None, None, Some(task_id))? {
            for file in entry.files {
                if file.source_task_id.as_deref() != Some(task_id.as_str()) {
                    continue;
                }
                if let (Some(disc), Some(track)) =
                    (file.pairing.disc_number, file.pairing.track_number)
                    && let (Ok(disc), Ok(track)) = (u32::try_from(disc), u32::try_from(track))
                {
                    held.insert((disc.max(1), track));
                }
            }
        }
    }
    Ok(Some(FetchFacts {
        task_id: current.id,
        status: current.status,
        release_mbid: plan.release_mbid,
        wanted,
        replace_versions: plan
            .planned_album
            .is_some_and(|planned| !planned.replace.is_empty()),
        landed,
        all_landed,
        held,
    }))
}

/// The answer while another download of the album is still working.
fn already_in_progress(task_id: String) -> EditionAcquireResponse {
    EditionAcquireResponse {
        status: "already_in_progress".to_owned(),
        message: "This album is already downloading. Wait for that download to finish, \
                  then try again."
            .to_owned(),
        task_id: Some(task_id),
        ..EditionAcquireResponse::default()
    }
}

impl RequestsService {
    /// Fill the chosen edition's missing tracks and upgrade its owned
    /// tracks below the cutoff (curator only). `album_id` names
    /// the library copy the edition picker planned, when it did. With
    /// `replace_versions`, also fetch this edition's version of each track
    /// a file of the copy is another version of, to replace that file.
    pub async fn acquire_edition(
        &self,
        curator: &Curator,
        release_group: &str,
        album_id: Option<&str>,
        replace_versions: bool,
    ) -> Result<EditionAcquireResponse, RequestsError> {
        let mbid = validate_mbid(release_group)?;
        let now = self.now_epoch();
        if let Some(busy) = self.album_busy(&mbid).await? {
            return Ok(already_in_progress(busy));
        }
        self.quota
            .check_storage_admission(curator.user_id(), curator.role(), DispatchOrigin::Edition)
            .await?;
        let gap = self.gap(album_id, &mbid, replace_versions).await?;
        let (artist_name, title) = match &gap {
            Some(gap) if !gap.artist_name.trim().is_empty() && !gap.title.trim().is_empty() => {
                (gap.artist_name.clone(), gap.title.clone())
            }
            _ => local_names(self.library.db(), &mbid)
                .await
                .map_err(|error| RequestsError::internal(&error))?
                .ok_or_else(|| RequestsError::InvalidInput {
                    message: "This album is not in your library yet. Request the album instead."
                        .to_owned(),
                })?,
        };
        let mut response = EditionAcquireResponse {
            release_mbid: gap.as_ref().map(|gap| gap.release_mbid.clone()),
            total_tracks: gap.as_ref().map(|gap| gap.total_tracks),
            requested: gap.as_ref().map(|gap| count(gap.missing.len())),
            upgrades: gap.as_ref().map(|gap| count(gap.upgrades.len())),
            versions: gap.as_ref().map(|gap| count(gap.versions.len())),
            ..EditionAcquireResponse::default()
        };
        // A held copy keeps its own versions, and says so.
        let kept_note = if gap
            .as_ref()
            .is_some_and(|gap| replace_versions && gap.held && gap.other_versions > 0)
        {
            " Your own versions stay, because this album's files are left as they are."
        } else {
            ""
        };
        if let Some(gap) = &gap {
            if gap.missing.is_empty() && gap.upgrades.is_empty() && gap.versions.is_empty() {
                response.status = "already_complete".to_owned();
                response.message = if gap.other_versions > 0 {
                    format!(
                        "Every track of this edition is in your library, {} of them in another \
                         version.{kept_note}",
                        gap.other_versions
                    )
                } else {
                    "Every track of this edition is already in your library.".to_owned()
                };
                return Ok(response);
            }
            self.check_disk(gap.estimate_bytes).await?;
        }
        // The copy the tracks are fetched for, which they then join: the
        // plan's, else the copy asked for or the group's, with the release
        // its identity names.
        let planned_album = match &gap {
            Some(gap) => Some(PlannedAlbum {
                album_id: gap.album_id.clone(),
                release_mbid: gap.release_mbid.clone(),
                replace: gap
                    .versions
                    .iter()
                    .map(|(_, version)| version.clone())
                    .collect(),
            }),
            None => self.planned_copy(album_id, &mbid).await?,
        };
        let release_mbid = match &planned_album {
            Some(planned) => Some(planned.release_mbid.clone()),
            None => self.chosen_release(&mbid).await,
        };
        let wanted = gap
            .as_ref()
            .map(|gap| {
                gap.missing
                    .iter()
                    .chain(&gap.upgrades)
                    .chain(gap.versions.iter().map(|(at, _)| at))
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        // With no source set up the queued download waits for the worker,
        // which starts it once a source is saved.
        let waiting = self
            .readiness
            .as_ref()
            .is_some_and(|readiness| !readiness.sources.can_acquire());
        let request = DispatchRequest {
            user_id: curator.user_id().to_owned(),
            target: DispatchTarget::Album {
                release_group_mbid: mbid.clone(),
            },
            artist_name,
            title,
            origin: DispatchOrigin::Edition,
            release_mbid,
            wanted,
            planned_album,
            // No stable per-decision key exists (a mark outlives its task,
            // so a key would pin re-acquires to the old task); the busy
            // check above owns double-submit dedup.
            idempotency_key: None,
        };
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Busy { operation }) => Err(RequestsError::Busy { operation }),
            Err(DispatchError::Failed(cause)) => Err(RequestsError::internal(&format_args!(
                "edition dispatch failed: {cause}"
            ))),
            Ok(DispatchOutcome::Active { task_id }) => Ok(already_in_progress(task_id)),
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                self.editions
                    .set(EditionMark {
                        key: mbid,
                        task_id: task_id.clone(),
                        started_at: now,
                    })
                    .await?;
                response.status = "started".to_owned();
                response.message = if waiting {
                    "No download source is set up, so this edition waits. It starts by \
                     itself once Soulseek, Usenet or a plugin source is saved."
                        .to_owned()
                } else {
                    let versions = response.versions.unwrap_or_default();
                    format!(
                        "{}{kept_note}",
                        started_message(response.requested, versions)
                    )
                };
                response.task_id = Some(task_id);
                Ok(response)
            }
        }
    }

    /// The album's current or last edition fetch, with where each track it
    /// asked for is (curator only). The mark of the last fetch stays after
    /// it ends, until the next fetch replaces it.
    pub async fn edition_fetch(
        &self,
        release_group: &str,
    ) -> Result<EditionFetchResponse, RequestsError> {
        let mbid = validate_mbid(release_group)?;
        let Some(mark) = self.editions.get(&mbid).await? else {
            return Ok(EditionFetchResponse::default());
        };
        let fetch = self
            .dispatch
            .journal()
            .read("editions.fetch", move |store| fetch_facts(store, &mark))
            .await
            .map_err(|error| super::dispatch_fault(error.into()))?;
        Ok(EditionFetchResponse {
            latest: fetch.map(|facts| facts.view()),
        })
    }

    /// A download already working on this album: this action's own, or
    /// any other (a request, an upgrade, a retry). Never two at once.
    async fn album_busy(&self, release_group_mbid: &str) -> Result<Option<String>, RequestsError> {
        if let Some(mark) = self.editions.get(release_group_mbid).await? {
            let task = self
                .dispatch
                .task(&mark.task_id)
                .await
                .map_err(super::dispatch_fault)?;
            if task.is_some_and(|task| !task.status.is_terminal()) {
                return Ok(Some(mark.task_id));
            }
        }
        let group = release_group_mbid.to_ascii_lowercase();
        self.library
            .db()
            .read("editions.album_busy", move |conn| {
                conn.query_row(sql::GROUP_DOWNLOADING.text(), [group], |row| row.get(0))
                    .optional()
                    .map_err(DbError::from)
            })
            .await
            .map_err(|error| RequestsError::internal(&error))
    }

    /// The library copy a fetch with no plan is for: the copy asked for
    /// (followed to the album it was merged into), else the group's copy
    /// with an edition, a person's first, then the oldest. `None` when that
    /// copy's identity names no release.
    async fn planned_copy(
        &self,
        album_id: Option<&str>,
        release_group_mbid: &str,
    ) -> Result<Option<PlannedAlbum>, RequestsError> {
        let group = release_group_mbid.to_ascii_lowercase();
        let album_id = album_id.map(str::to_owned);
        let found: Option<(String, String)> = self
            .library
            .db()
            .read("editions.planned_copy", move |conn| {
                conn.query_row(
                    sql::PLANNED_COPY.text(),
                    rusqlite::params![group, album_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(DbError::from)
            })
            .await
            .map_err(|error| RequestsError::internal(&error))?;
        Ok(found.map(|(album_id, release_mbid)| PlannedAlbum {
            album_id,
            release_mbid,
            replace: Vec::new(),
        }))
    }

    /// The library's plan for the album's chosen edition. `None` when it
    /// cannot be read right now (MusicBrainz down, no copy with an edition,
    /// library not wired): the whole album is fetched then, and the landing
    /// skips what is owned.
    async fn gap(
        &self,
        album_id: Option<&str>,
        release_group_mbid: &str,
        replace_versions: bool,
    ) -> Result<Option<Gap>, RequestsError> {
        let Some(plans) = &self.edition_plans else {
            return Ok(None);
        };
        let plan = match plans.group_plan(album_id, release_group_mbid).await {
            Ok(Some(plan)) => plan,
            Ok(None) => return Ok(None),
            Err(OperationError::Unavailable(cause)) => {
                tracing::warn!(release_group_mbid, %cause, "edition plan unavailable; fetching the whole album");
                return Ok(None);
            }
            Err(OperationError::NotFound(reason)) => {
                tracing::info!(
                    release_group_mbid,
                    code = reason.code,
                    "no edition plan; fetching the whole album"
                );
                return Ok(None);
            }
            Err(OperationError::Store(cause)) => return Err(RequestsError::internal(&cause)),
            Err(OperationError::Busy(operation)) => {
                return Err(RequestsError::Busy { operation });
            }
            Err(OperationError::Invalid(reason) | OperationError::Conflict(reason)) => {
                return Err(RequestsError::InvalidInput {
                    message: format!("{} {}", reason.message, reason.action),
                });
            }
        };
        let held = copy_is_held(self.library.db(), &plan.local_album_id)
            .await
            .map_err(|error| RequestsError::internal(&error))?;
        Ok(Some(gap_of(
            &plan,
            &self.below_cutoff(&plan).await,
            replace_versions,
            held,
        )))
    }

    /// The plan's kept files below the upgrade cutoff; none while upgrades
    /// are off or the qualities cannot be read.
    async fn below_cutoff(&self, plan: &EditionPlan) -> Vec<String> {
        let Some(policy) = self.upgrade_policy.as_ref().map(|live| {
            crate::flows::stores::UpgradePolicy::from_policy(&live.read().unwrap_or_default())
        }) else {
            return Vec::new();
        };
        if !policy.upgrade_allowed {
            return Vec::new();
        }
        let kept: Vec<String> = plan
            .kept()
            .map(|file| file.local_track_id.clone())
            .collect();
        match UpgradeWorklist::new(self.library.clone())
            .tracks_below_cutoff(&kept, &policy.cutoff)
            .await
        {
            Ok(below) => below,
            Err(error) => {
                tracing::warn!(?error, "track qualities unreadable; no upgrades asked for");
                Vec::new()
            }
        }
    }

    /// Refuse when the disk downloads land on lacks room for them.
    async fn check_disk(&self, estimate: u64) -> Result<(), RequestsError> {
        let Some(readiness) = &self.readiness else {
            return Ok(());
        };
        let root = readiness.staging_root.clone();
        let free = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&root).ok();
            droppedneedle_library::adapters::free_bytes_for(&root)
        })
        .await
        .map_err(|error| RequestsError::internal(&error))?;
        let free = match free {
            Ok(free) => free,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "free space unreadable where downloads land; not checked"
                );
                return Ok(());
            }
        };
        let needed = estimate.saturating_add(DISK_SAFETY_BYTES);
        if free < needed {
            const MB: u64 = 1024 * 1024;
            return Err(RequestsError::InvalidInput {
                message: format!(
                    "There is not enough free disk space for these downloads: about {} MB is \
                     needed where downloads land, and {} MB is free. Free some space, then try \
                     again.",
                    needed / MB,
                    free / MB
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_library::operations::plan::{EditionFacts, FileFate, PlanFile};

    fn track(position: u32, file: Option<&str>) -> PlanTrack {
        PlanTrack {
            disc: 1,
            position,
            title: format!("T{position}"),
            recording_mbid: format!("rec-{position}"),
            release_track_mbid: format!("rt-{position}"),
            length_ms: Some(200_000),
            local_track_id: file.map(str::to_owned),
        }
    }

    // The positions asked for are the missing tracks plus the kept tracks
    // whose files sit below the cutoff; nothing else.
    #[test]
    fn asks_for_missing_and_below_cutoff_positions() {
        let plan = EditionPlan {
            local_album_id: "al".into(),
            release_group_mbid: "rg".into(),
            release_mbid: "rel".into(),
            title: "Album".into(),
            artist_name: "Artist".into(),
            is_current: true,
            same_album: true,
            edition: EditionFacts::default(),
            tracks: vec![track(1, Some("f1")), track(2, None), track(3, Some("f3"))],
            files: ["f1", "f3"]
                .iter()
                .map(|id| PlanFile {
                    local_track_id: (*id).into(),
                    title: String::new(),
                    disc: 1,
                    track: 0,
                    length_ms: None,
                    fate: FileFate::Keep,
                    edition_track: None,
                    note: None,
                })
                .collect(),
        };
        let gap = gap_of(&plan, &["f3".to_owned()], false, false);
        assert_eq!(gap.missing, vec![(1, 2)]);
        assert_eq!(gap.upgrades, vec![(1, 3)]);
        assert_eq!(gap.total_tracks, 3);
        assert!(gap.estimate_bytes >= 2 * TRACK_FLOOR_BYTES);
    }
}
