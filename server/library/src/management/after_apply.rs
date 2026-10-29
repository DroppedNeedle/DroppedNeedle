//! What happens after an album is written: the files' management state
//! (so automatic management knows they are done), the overrides a tag
//! edit asked to keep, and the media servers told to rescan when the job
//! ends. A rescan is a job on the jobs table, one per server, which the
//! media crate's handler sends.

use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::jobs::queue::{JobKind, JobStatus, State};
use droppedneedle_kernel::runtime_config::sections::ExternalRefreshSettings;
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::models::{JobMode, ManagementError};
use super::service::Management;
use super::store::{self, ManagedMark, Snapshot};
use super::takeover;
use crate::clock::LibraryClock as _;
use crate::jobs;

impl Management {
    /// Bookkeeping for one album the job just wrote. The files are
    /// already written; a failure here is logged and leaves them as they
    /// are (the next run sees them as not managed yet and plans them
    /// again, which writes nothing new).
    /// Returns what to add to the album's result: for a tag edit, each
    /// file's override revision before and after, so the edit's own
    /// override never blocks its undo.
    pub(super) fn after_album(
        &self,
        snapshot: &Snapshot,
        ordinal: i64,
    ) -> Option<serde_json::Value> {
        match self.record_album(snapshot, ordinal) {
            Ok(extra) => extra,
            Err(error) => {
                tracing::error!(
                    %error,
                    job_id = snapshot.job.id,
                    ordinal,
                    "management state not recorded after a written album"
                );
                None
            }
        }
    }

    fn record_album(
        &self,
        snapshot: &Snapshot,
        ordinal: i64,
    ) -> Result<Option<serde_json::Value>, ManagementError> {
        let job_id = snapshot.job.id.as_str();
        let items = self
            .setup
            .db
            .read_blocking("management.record_bundle_items", |conn| {
                Ok::<_, ManagementError>(store::bundle_items(conn, job_id, ordinal)?)
            })?;
        let intent = snapshot.intent();
        let mode = snapshot.job_mode();
        let now = self.setup.clock.now_secs();
        let mut extra = None;
        if let Some(edit) = &intent.tag_edit {
            self.setup
                .db
                .write_blocking(Lane::Background, "management.save_tag_edit", |tx| {
                    Ok::<_, ManagementError>(store::save_tag_edit(
                        tx,
                        edit,
                        job_id,
                        snapshot.job.requested_by_user_id.as_deref(),
                        now,
                    )?)
                })?;
            let revisions: serde_json::Map<String, serde_json::Value> = items
                .iter()
                .filter_map(|item| {
                    let plan = item.desired.plan.as_ref()?;
                    let after = self.override_revision(&item.album_id, &plan.track_id);
                    Some((
                        plan.track_id.clone(),
                        serde_json::json!([after, plan.override_revision]),
                    ))
                })
                .collect();
            extra = Some(serde_json::json!({ "override_revisions": revisions }));
        }
        if mode == Some(JobMode::Undo)
            && let Some(source) = snapshot.linked_job_id.as_deref()
            && let Some(source) = self.setup.db.read_blocking("management.snapshot", |conn| {
                Ok::<_, ManagementError>(store::snapshot(conn, source)?)
            })?
        {
            if let Some(edit) = source.intent().tag_edit {
                // Undoing a tag edit also puts its kept values back.
                self.setup.db.write_blocking(
                    Lane::Background,
                    "management.revert_tag_edit",
                    |tx| Ok::<_, ManagementError>(store::revert_tag_edit(tx, &edit, job_id, now)?),
                )?;
            } else if let Some(album_id) = items.first().map(|item| item.album_id.as_str()) {
                self.run_undone(&source, album_id, Some(job_id), now)?;
            }
        }
        let written = items.iter().filter(|item| item.eligibility.writes());
        match mode {
            Some(JobMode::BaselineRestore) => {
                self.setup
                    .db
                    .write_blocking(Lane::Background, "management.mark_restored", |tx| {
                        for item in written {
                            if let Some(track_id) = &item.track_id {
                                store::mark_restored(tx, track_id, job_id, now)?;
                            }
                        }
                        Ok::<_, ManagementError>(())
                    })
            }
            // A profile run: remember what it applied. Tag edits and
            // duplicate resolutions apply no whole profile, and an undo
            // puts the old state back.
            Some(JobMode::Apply | JobMode::AutomaticApply) if intent.tag_edit.is_none() => {
                let (profile_id, _) = snapshot.profile_label();
                let sandbox = self.setup.publishing.publish_sandbox().ok();
                struct Mark {
                    track_id: String,
                    album_id: String,
                    root: String,
                    override_revision: u64,
                    path: Option<String>,
                    found: Option<String>,
                }
                let marks: Vec<Mark> = written
                    .filter_map(|item| {
                        let track_id = item.track_id.clone()?;
                        let root = item.dest_root.clone().unwrap_or(item.source_root.clone());
                        let rel = item.dest_rel.clone().unwrap_or(item.source_rel.clone());
                        Some(Mark {
                            override_revision: self.override_revision(&item.album_id, &track_id),
                            album_id: item.album_id.clone(),
                            path: sandbox.as_ref().and_then(|sandbox| {
                                super::automatic::path_revision(sandbox, &root, &rel)
                            }),
                            // A file with no plan of its own was left as
                            // the plan read it.
                            found: (item.desired.plan.is_none() && !item.fingerprint.is_empty())
                                .then(|| item.fingerprint.clone()),
                            track_id,
                            root,
                        })
                    })
                    .collect();
                self.setup
                    .db
                    .write_blocking(Lane::Background, "management.mark_managed", |tx| {
                        use crate::identify::sqlite::{read_album_identity, read_track_identity};
                        for mark in &marks {
                            // The match each file was written for, read on
                            // this transaction.
                            let album_identity = read_album_identity(tx, &mark.album_id)?
                                .map(|identity| identity.row_revision);
                            let track_identity = read_track_identity(tx, &mark.track_id)?
                                .map(|identity| identity.row_revision);
                            store::mark_managed(
                                tx,
                                &ManagedMark {
                                    track_id: &mark.track_id,
                                    root_id: &mark.root,
                                    profile_id: &profile_id,
                                    profile_revision: &snapshot.profile_revision,
                                    naming_revision: &snapshot.naming_revision,
                                    override_revision: mark.override_revision,
                                    job_id,
                                    album_identity_revision: album_identity,
                                    track_identity_revision: track_identity,
                                    path_revision: mark.path.clone(),
                                    found_sha256: mark.found.as_deref(),
                                },
                                now,
                            )?;
                        }
                        Ok(())
                    })
            }
            _ => Ok(()),
        }?;
        Ok(extra)
    }

    /// A person undid `source`'s run on the album. When that was a run of
    /// the profile in a folder managed without asking, the files are the
    /// app's own bytes again and automatic management would do the same
    /// run over, so the album is left to the person.
    fn run_undone(
        &self,
        source: &Snapshot,
        album_id: &str,
        job_id: Option<&str>,
        now: f64,
    ) -> Result<(), ManagementError> {
        let profile_run = matches!(
            source.job_mode(),
            Some(JobMode::Apply | JobMode::AutomaticApply)
        ) && source.intent().tag_edit.is_none();
        if profile_run && self.managed_automatically(album_id)? {
            self.setup
                .db
                .write_blocking(Lane::Background, "management.run_undone", |tx| {
                    Ok::<_, ManagementError>(takeover::mark(
                        tx,
                        album_id,
                        takeover::Cause::RunUndone,
                        &[],
                        job_id,
                        now,
                    )?)
                })?;
        }
        Ok(())
    }

    /// Undo one published bundle (see
    /// [`Publishing::undo_bundle`](crate::manage::Publishing::undo_bundle)).
    /// An undone run of the profile is not run again by itself; the undo
    /// stands either way. Blocking.
    pub fn undo_bundle(
        &self,
        bundle_id: &str,
        only_by: Option<&str>,
    ) -> Result<crate::manage::AppliedBundle, crate::service::ServiceError> {
        let applied = self.setup.publishing.undo_bundle(bundle_id, only_by)?;
        if let Err(error) = self.bundle_undone(bundle_id) {
            tracing::warn!(%error, bundle_id, "undone bundle not recorded for automatic management");
        }
        Ok(applied)
    }

    /// The same for one bundle undone on its own, outside a job (the
    /// route that undoes a single bundle). Blocking.
    fn bundle_undone(&self, bundle_id: &str) -> Result<(), ManagementError> {
        let found: Option<(String, i64)> = self
            .setup
            .db
            .read_blocking("management.undone_bundle", |conn| {
                Ok::<_, ManagementError>(jobs::work_of_bundle(conn, bundle_id)?)
            })?;
        let Some((source_id, ordinal)) = found else {
            return Ok(());
        };
        let Some(source) = self
            .setup
            .db
            .read_blocking("management.bundle_undone_snapshot", |conn| {
                Ok::<_, ManagementError>(store::snapshot(conn, &source_id)?)
            })?
        else {
            return Ok(());
        };
        let items = self
            .setup
            .db
            .read_blocking("management.bundle_undone_bundle_items", |conn| {
                Ok::<_, ManagementError>(store::bundle_items(conn, &source_id, ordinal)?)
            })?;
        match items.first() {
            Some(item) => {
                self.run_undone(&source, &item.album_id, None, self.setup.clock.now_secs())
            }
            None => Ok(()),
        }
    }

    /// The media servers a finished run asks to rescan: every server whose
    /// own switch is on, when the run's profile and the library-wide
    /// switch both ask for it. Settings that cannot be read ask for none.
    pub(super) fn refresh_targets(&self, snapshot: &Snapshot) -> Vec<RefreshTarget> {
        let wants = Self::pinned(snapshot)
            .is_ok_and(|pinned| pinned.profile.notification.refresh_external_servers);
        if !wants {
            return Vec::new();
        }
        let Some(settings) = self.readable_settings() else {
            tracing::warn!("media server rescans skipped: settings unreadable");
            return Vec::new();
        };
        RefreshTarget::switched_on(&settings.external_refresh)
    }

    /// Queue each target's rescan on the finishing run's own transaction,
    /// due [`REFRESH_DELAY_MS`] later, and note the job on the run. A
    /// rescan already queued for the server is joined, so a burst of runs
    /// asks each server once.
    pub(super) fn queue_refreshes(
        &self,
        tx: &Transaction<'_>,
        job_id: &str,
        targets: &[RefreshTarget],
    ) -> Result<(), ManagementError> {
        let due = self.setup.clock.now_ms().saturating_add(REFRESH_DELAY_MS);
        for &target in targets {
            let queued = self
                .setup
                .jobs
                .enqueue_at(
                    tx,
                    MEDIA_SERVER_REFRESH,
                    target.as_str(),
                    &RefreshJob { target },
                    due,
                )
                .map_err(|error| {
                    if error.is_busy() {
                        ManagementError::Busy(error.to_string())
                    } else {
                        ManagementError::Store(error.to_string())
                    }
                })?;
            store::record_refresh_job(tx, job_id, target, queued.id)?;
        }
        Ok(())
    }

    /// How each rescan a run asked for went, read from the jobs table. A
    /// job that was pruned or folded into a newer request reads as no
    /// record.
    pub async fn external_refreshes(
        &self,
        jobs: &[(RefreshTarget, i64)],
    ) -> Result<Vec<ExternalRefresh>, ManagementError> {
        let mut refreshes = Vec::with_capacity(jobs.len());
        for &(target, id) in jobs {
            let status = self.setup.jobs.get(id).await?;
            refreshes.push(ExternalRefresh::read(target, status));
        }
        Ok(refreshes)
    }
}

/// The jobs-table kind a media server's rescan runs as, keyed by the
/// server ([`RefreshTarget::as_str`]).
pub const MEDIA_SERVER_REFRESH: JobKind = JobKind::new("media_server.refresh");

/// How long after a run its rescans wait, so the runs of a burst join one.
pub const REFRESH_DELAY_MS: i64 = 15_000;

/// A media server Library Management can ask to rescan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefreshTarget {
    Jellyfin,
    Plex,
    Navidrome,
}

impl RefreshTarget {
    /// The job key and the stored name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jellyfin => "jellyfin",
            Self::Plex => "plex",
            Self::Navidrome => "navidrome",
        }
    }

    /// The servers the settings switch on, none while the library-wide
    /// switch is off.
    pub fn switched_on(settings: &ExternalRefreshSettings) -> Vec<Self> {
        if !settings.enabled {
            return Vec::new();
        }
        [
            (Self::Jellyfin, settings.jellyfin_enabled),
            (Self::Plex, settings.plex_enabled),
            (Self::Navidrome, settings.navidrome_enabled),
        ]
        .into_iter()
        .filter_map(|(target, on)| on.then_some(target))
        .collect()
    }
}

/// A rescan job's payload: a function of its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshJob {
    pub target: RefreshTarget,
}

/// Why a rescan did not go through. The handler stores these as the
/// job's code; the run's page shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum RefreshCode {
    /// The server, or its rescan, is not set up. Not retried.
    #[serde(rename = "EXTERNAL_REFRESH_NOT_CONFIGURED")]
    NotConfigured,
    /// The server refused the saved credential. Not retried.
    #[serde(rename = "EXTERNAL_REFRESH_AUTH_FAILED")]
    AuthFailed,
    /// The server has no rescan call at that address. Not retried.
    #[serde(rename = "EXTERNAL_REFRESH_NOT_FOUND")]
    NotFound,
    /// The server could not be reached. Retried.
    #[serde(rename = "EXTERNAL_REFRESH_UNREACHABLE")]
    Unreachable,
    /// The server answered with an error. Retried.
    #[serde(rename = "EXTERNAL_REFRESH_FAILED")]
    Failed,
    /// The server stopped or the call ran too long. Retried.
    #[serde(rename = "EXTERNAL_REFRESH_INTERRUPTED")]
    Interrupted,
}

impl RefreshCode {
    /// The stored and wire code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "EXTERNAL_REFRESH_NOT_CONFIGURED",
            Self::AuthFailed => "EXTERNAL_REFRESH_AUTH_FAILED",
            Self::NotFound => "EXTERNAL_REFRESH_NOT_FOUND",
            Self::Unreachable => "EXTERNAL_REFRESH_UNREACHABLE",
            Self::Failed => "EXTERNAL_REFRESH_FAILED",
            Self::Interrupted => "EXTERNAL_REFRESH_INTERRUPTED",
        }
    }

    /// The code a job row holds: one of ours, or one the jobs table wrote
    /// when a run did not finish.
    fn read(stored: &str) -> Self {
        use droppedneedle_kernel::jobs::queue::{
            BUSY, HANDLER_PANICKED, INTERRUPTED, LEASE_EXPIRED, TIMED_OUT,
        };
        let ours = [
            Self::NotConfigured,
            Self::AuthFailed,
            Self::NotFound,
            Self::Unreachable,
            Self::Failed,
            Self::Interrupted,
        ]
        .into_iter()
        .find(|code| code.as_str() == stored);
        match ours {
            Some(code) => code,
            None if [
                INTERRUPTED,
                LEASE_EXPIRED,
                TIMED_OUT,
                HANDLER_PANICKED,
                BUSY,
            ]
            .contains(&stored) =>
            {
                Self::Interrupted
            }
            None => Self::Failed,
        }
    }
}

/// Where a rescan stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefreshState {
    /// Waiting to be sent, or to be tried again after its code.
    Queued,
    /// Being sent.
    Running,
    /// The server took it.
    Done,
    /// Given up for good.
    Failed,
    /// No longer kept (after a week), or joined into a later run's rescan.
    NoRecord,
}

/// One media server's rescan after a run, as the run's page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExternalRefresh {
    pub target: RefreshTarget,
    pub state: RefreshState,
    /// Tries so far.
    pub attempts: u32,
    /// Why the last try did not go through.
    pub code: Option<RefreshCode>,
    /// When it ended, unix ms.
    pub finished_at: Option<i64>,
}

impl ExternalRefresh {
    fn read(target: RefreshTarget, status: Option<JobStatus>) -> Self {
        let Some(status) = status else {
            return Self {
                target,
                state: RefreshState::NoRecord,
                attempts: 0,
                code: None,
                finished_at: None,
            };
        };
        Self {
            target,
            state: match status.state {
                State::Queued => RefreshState::Queued,
                State::Running => RefreshState::Running,
                State::Done => RefreshState::Done,
                State::Failed => RefreshState::Failed,
            },
            attempts: status.attempts,
            code: status.last_error.as_deref().map(RefreshCode::read),
            finished_at: status.finished_at,
        }
    }
}
