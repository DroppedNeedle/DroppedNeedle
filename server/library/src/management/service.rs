//! Management entry points: what the HTTP handlers and the worker call.
//!
//! Reads and writes go through the shared database's blocking calls, so
//! everything here runs off the async workers (handlers use
//! `spawn_blocking`; the loop drives its tick on a blocking thread).
//! Requests write on the foreground lane, the worker on the background
//! lane. Identify store methods take their own turn on the database, so
//! they are never called inside a write here.

use droppedneedle_kernel::db::Lane;

use super::models::{
    HistoryFilter, HistoryItem, ItemFilter, ItemPage, JobDetail, JobIntent, JobMode,
    ManagementError, Origin, PinnedProfile, PlannedItem, PreviewHandle, ResultPage, Revisions,
    Selection, SelectionKind, Summary, TagEditMode,
};
use super::planner::{self, PlanContext};
use super::profile::{self, CompiledScripts};
use super::reasons::{self, ItemReason};
use super::selection;
use super::store::{self, NewJob, Snapshot};
use super::takeover;
use crate::clock::LibraryClock as _;
use crate::jobs::{self, ManagementPhase, OperationKind, OperationState};
use crate::manage::SealPins;
use crate::operations::models::OperationJob;
use crate::publish::publisher::read_revision;
use crate::publish::{JournalStore, snapshots::sha256_hex};
use crate::scan::roots::RootRegistry;
use crate::wiring::LibrarySetup;
use droppedneedle_kernel::runtime_config::sections::{
    LibraryManagement, LibraryManagementRootOverrides,
};

/// Largest page of planned files or results.
pub const PAGE_MAX: i64 = 200;
/// Largest page of history.
pub const HISTORY_MAX: i64 = 50;

/// A request for a new preview.
pub struct PreviewRequest {
    pub selection: Selection,
    pub profile_id: String,
    pub expected_settings_revision: String,
    pub expected_policy_revision: String,
    pub idempotency_key: Option<String>,
    pub target_root_id: Option<String>,
    pub overrides: Option<LibraryManagementRootOverrides>,
}

/// Everything a new job pins.
pub(super) struct JobSpec<'a> {
    pub mode: JobMode,
    pub origin: Origin,
    pub actor: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
    pub revisions: Revisions,
    pub selection_json: String,
    pub pinned: &'a PinnedProfile,
    pub target_root_id: Option<&'a str>,
    pub linked_job_id: Option<&'a str>,
    pub intent: JobIntent,
    pub proposed_settings_revision: Option<&'a str>,
    pub retention_hours: i64,
}

/// A request to apply a ready preview.
pub struct ApplyRequest {
    pub preview_token: String,
    pub expected_row_revision: i64,
    pub idempotency_key: String,
    pub confirmation: bool,
}

/// Management jobs over the library setup.
#[derive(Clone)]
pub struct Management {
    pub(super) setup: LibrarySetup,
    /// Cancelled when the server shuts down; the worker then hands its
    /// job back to the queue between albums.
    shutdown: Option<droppedneedle_kernel::jobs::CancellationToken>,
}

/// The reason a file is left out of an undo or a restore whose saved copy
/// is missing or damaged. It is the one block that does not hold the rest
/// of its bundle back.
pub(super) const UNDO_COPY_MISSING: &str = "UNDO_COPY_MISSING";

pub(super) fn stale(message: &str) -> ManagementError {
    ManagementError::Conflict(message.to_owned())
}

impl Management {
    pub fn new(setup: &LibrarySetup) -> Self {
        Self {
            setup: setup.clone(),
            shutdown: None,
        }
    }

    /// Hand running jobs back to the queue once `shutdown` is cancelled.
    pub fn stopping_with(
        mut self,
        shutdown: droppedneedle_kernel::jobs::CancellationToken,
    ) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    /// The stored settings, with every profile's field list complete even
    /// before the settings page has read and saved it back.
    pub(super) fn settings(&self) -> Result<LibraryManagement, ManagementError> {
        let stored = self
            .setup
            .settings
            .management
            .get()
            .map_err(|error| ManagementError::Store(error.to_string()))?;
        let mut settings = LibraryManagement::clone(&stored);
        crate::management::settings::complete_fields(&mut settings);
        Ok(settings)
    }

    /// The stored settings as [`Self::settings`] gives them, or `None`
    /// when they do not read (logged once per revision).
    pub(super) fn readable_settings(&self) -> Option<LibraryManagement> {
        let stored = self.setup.settings.management.read()?;
        let mut settings = LibraryManagement::clone(&stored);
        crate::management::settings::complete_fields(&mut settings);
        Some(settings)
    }

    pub(super) fn snapshot(&self, job_id: &str) -> Result<Snapshot, ManagementError> {
        self.setup.db.read_blocking("management.snapshot", |conn| {
            store::snapshot(conn, job_id)?.ok_or(ManagementError::NotFound(
                "This management job does not exist.",
            ))
        })
    }

    /// A random preview token and its hash.
    pub(super) fn token(&self) -> (String, String) {
        let token =
            format!("{}{}", self.setup.ids.new_id(), self.setup.ids.new_id()).replace('-', "");
        let hash = sha256_hex(token.as_bytes());
        (token, hash)
    }

    // -----------------------------------------------------------------
    // Previews.
    // -----------------------------------------------------------------

    /// Refuse when the settings or the library folders moved since the
    /// page that asks loaded them. Returns the registry the job runs on.
    pub(super) fn check_revisions(
        &self,
        stored: &LibraryManagement,
        expected_settings: &str,
        expected_policy: &str,
    ) -> Result<RootRegistry, ManagementError> {
        if crate::management::settings::settings_revision(stored) != expected_settings {
            return Err(stale(
                "Library Management settings changed since this page loaded. Reload the page and make the preview again.",
            ));
        }
        self.registry_at(
            expected_policy,
            "The library folders or their rules changed since this page loaded. Reload the page and make the preview again.",
        )
    }

    /// The live registry, refused with `message` unless the library
    /// folders and rules still carry `expected`, the policy revision the
    /// library settings page shows.
    pub(super) fn registry_at(
        &self,
        expected: &str,
        message: &str,
    ) -> Result<RootRegistry, ManagementError> {
        self.setup
            .core
            .registry_at(expected)
            .map_err(|error| ManagementError::Store(error.to_string()))?
            .ok_or_else(|| stale(message))
    }

    /// Start a preview: pin the profile and revisions, and queue the
    /// planning. A retried request with the same idempotency key returns
    /// the same job with a fresh token. Blocking.
    pub fn create_preview(
        &self,
        request: PreviewRequest,
        actor: &str,
    ) -> Result<PreviewHandle, ManagementError> {
        let stored = self.settings()?;
        let registry = self.check_revisions(
            &stored,
            &request.expected_settings_revision,
            &request.expected_policy_revision,
        )?;
        let selection = selection::normalize(request.selection)?;
        let pinned = profile::pin(&stored, &request.profile_id, request.overrides)?;
        if let Some(target) = request.target_root_id.as_deref() {
            if registry
                .resolve(target)
                .is_none_or(|root| root.policy == crate::scan::models::EffectivePolicy::Excluded)
            {
                return Err(ManagementError::Invalid(
                    "The destination library folder does not exist or is excluded.".to_owned(),
                ));
            }
            if !pinned.profile.organization.move_enabled {
                return Err(ManagementError::Invalid(
                    "Moving files to another library folder needs file moving turned on in the profile."
                        .to_owned(),
                ));
            }
            if selection.kind == super::models::SelectionKind::Roots
                && selection.ids.iter().any(|id| id == target)
            {
                return Err(ManagementError::Invalid(
                    "The destination folder must differ from every selected folder.".to_owned(),
                ));
            }
        }
        self.start_job(JobSpec {
            mode: JobMode::Preview,
            origin: Origin::Manual,
            actor: Some(actor),
            idempotency_key: request.idempotency_key.as_deref(),
            revisions: profile::revisions(&stored, &pinned, registry.policy_revision()),
            selection_json: serde_json::to_string(&selection)?,
            pinned: &pinned,
            target_root_id: request.target_root_id.as_deref(),
            linked_job_id: None,
            intent: JobIntent::default(),
            proposed_settings_revision: None,
            retention_hours: stored.preview_retention_hours,
        })
    }

    /// Queue a new job, or hand back the one an earlier request with the
    /// same idempotency key made (with a fresh token) when it asked for
    /// the same thing. Blocking.
    pub(super) fn start_job(&self, spec: JobSpec<'_>) -> Result<PreviewHandle, ManagementError> {
        let profile_json = serde_json::to_string(spec.pinned)?;
        let now = self.setup.clock.now_secs();
        let (token, token_hash) = self.token();
        // A job no person asked for is the automatic worker's.
        let lane = if spec.actor.is_some() {
            Lane::Foreground
        } else {
            Lane::Background
        };
        if let Some(key) = spec.idempotency_key
            && let Some(existing) = self
                .setup
                .db
                .read_blocking("management.job_for_key", |conn| {
                    Ok::<_, ManagementError>(store::job_for_key(conn, key)?)
                })?
        {
            let snapshot = self.snapshot(&existing)?;
            let same = snapshot.mode == spec.mode.as_str()
                && snapshot.selection_json == spec.selection_json
                && snapshot.profile_revision == spec.revisions.profile
                && snapshot.settings_revision == spec.revisions.settings
                && snapshot.target_root_id.as_deref() == spec.target_root_id
                && snapshot.linked_job_id.as_deref() == spec.linked_job_id
                && snapshot.intent().tag_edit == spec.intent.tag_edit
                && snapshot.intent().duplicate == spec.intent.duplicate
                && snapshot.intent().edition == spec.intent.edition
                && snapshot.job.requested_by_user_id.as_deref() == spec.actor;
            if !same {
                return Err(stale(
                    "This request key was already used for a different preview. Make the preview again.",
                ));
            }
            self.setup
                .db
                .write_blocking(lane, "management.rotate_token", |conn| {
                    Ok::<_, ManagementError>(store::rotate_token(
                        conn,
                        &existing,
                        &token_hash,
                        now,
                    )?)
                })?;
            return Ok(PreviewHandle {
                job_id: existing,
                preview_token: token,
                created_at: snapshot
                    .preview_created_at
                    .unwrap_or(snapshot.job.created_at),
                expires_at: snapshot
                    .preview_expires_at
                    .unwrap_or(snapshot.job.created_at),
                existing: true,
            });
        }
        let catalog_revision = self
            .setup
            .db
            .read_blocking("management.catalog_revision", |conn| {
                read_revision(conn).map_err(|error| ManagementError::Store(error.to_string()))
            })?;
        let expires_at = now + spec.retention_hours.max(1) as f64 * 3600.0;
        let job_id = self.setup.ids.new_id();
        let summary = Summary::default();
        self.setup
            .db
            .write_blocking(lane, "management.insert_job", |tx| {
                store::insert_job(
                    tx,
                    &NewJob {
                        id: &job_id,
                        requested_by: spec.actor,
                        idempotency_key: spec.idempotency_key,
                        mode: spec.mode.as_str(),
                        selection_json: &spec.selection_json,
                        profile_snapshot_json: &profile_json,
                        revisions: &spec.revisions,
                        catalog_revision: i64::try_from(catalog_revision).unwrap_or(i64::MAX),
                        token_hash: &token_hash,
                        now,
                        expires_at,
                        target_root_id: spec.target_root_id,
                        linked_job_id: spec.linked_job_id,
                        summary: &summary,
                        origin: spec.origin,
                        intent: &spec.intent,
                        proposed_settings_revision: spec.proposed_settings_revision,
                    },
                )
            })?;
        jobs::wake(&self.setup.wakeups, OperationKind::LibraryManagement);
        Ok(PreviewHandle {
            job_id,
            preview_token: token,
            created_at: now,
            expires_at,
            existing: false,
        })
    }

    /// Why a ready job can no longer be applied as planned.
    pub(super) fn stale_reasons(&self, snapshot: &Snapshot) -> Vec<String> {
        if snapshot.job_mode().is_some_and(JobMode::restores) {
            return Vec::new();
        }
        let mut found = Vec::new();
        match self.readable_settings() {
            Some(stored) => {
                if crate::management::settings::settings_revision(&stored)
                    != snapshot.settings_revision
                {
                    found.push(
                        "Library Management settings changed after this preview was made."
                            .to_owned(),
                    );
                }
            }
            None => found.push("Library Management settings could not be read.".to_owned()),
        }
        if self.setup.core.live_registry().policy_revision() != snapshot.policy_revision {
            found.push(
                "The library folders or their rules changed after this preview was made."
                    .to_owned(),
            );
        }
        found
    }

    /// One job as the preview and operation pages show it. Blocking.
    pub fn detail(&self, job_id: &str) -> Result<JobDetail, ManagementError> {
        let snapshot = self.snapshot(job_id)?;
        let now = self.setup.clock.now_secs();
        let expired = snapshot.preview_expires_at.is_none_or(|at| at <= now);
        let ready =
            snapshot.phase == ManagementPhase::Ready && snapshot.job.state == OperationState::Ready;
        let stale_reasons = if ready {
            self.stale_reasons(&snapshot)
        } else {
            Vec::new()
        };
        let undo = self
            .setup
            .db
            .read_blocking("management.undo_counts", |conn| {
                Ok::<_, ManagementError>(store::undo_counts(
                    conn,
                    job_id,
                    self.setup.clock.today_day(),
                )?)
            })?;
        let baseline_available_count = self
            .setup
            .db
            .read_blocking("management.baseline_available", |conn| {
                Ok::<_, ManagementError>(store::baseline_available(conn, job_id)?)
            })?;
        let external_refreshes = self
            .setup
            .db
            .read_blocking("management.external_refreshes", |conn| {
                Ok::<_, ManagementError>(store::external_refreshes(conn, job_id)?)
            })?;
        let (profile_id, profile_name) = snapshot.profile_label();
        let worker_stalled = snapshot.job.state == OperationState::Running
            && snapshot.lease_expires_at.is_none_or(|at| at <= now);
        Ok(JobDetail {
            ready_for_confirmation: ready && !expired && stale_reasons.is_empty(),
            summary: snapshot.summary(),
            selection: serde_json::from_str(&snapshot.selection_json).unwrap_or_default(),
            mode: snapshot.mode,
            origin: snapshot.origin,
            phase: snapshot.phase,
            profile_id,
            profile_name,
            profile_revision: snapshot.profile_revision,
            settings_revision: snapshot.settings_revision,
            policy_revision: snapshot.policy_revision,
            catalog_revision: snapshot.catalog_revision,
            target_root_id: snapshot.target_root_id,
            linked_job_id: snapshot.linked_job_id,
            preview_created_at: snapshot.preview_created_at,
            expires_at: snapshot.preview_expires_at,
            expired,
            stale_reasons,
            lease_expires_at: snapshot.lease_expires_at,
            heartbeat_at: snapshot.heartbeat_at,
            worker_stalled,
            undo_available_count: undo.available,
            undo_expired_count: undo.expired,
            undo_expires_at: undo.first_expiry_day.map(|day| day as f64 * 86_400.0),
            baseline_available_count,
            proposed_settings_revision: snapshot.proposed_settings_revision,
            external_refreshes,
            job: snapshot.job,
        })
    }

    /// A page of planned files. Blocking.
    pub fn items(
        &self,
        job_id: &str,
        after: i64,
        limit: i64,
        filter: &ItemFilter,
    ) -> Result<ItemPage, ManagementError> {
        self.snapshot(job_id)?;
        let limit = limit.clamp(1, PAGE_MAX);
        self.setup.db.read_blocking("management.items", |conn| {
            Ok(store::items_page(conn, job_id, after, limit, filter)?)
        })
    }

    /// The cover art a planned file embeds or writes, by the hash the
    /// preview lists: its bytes and MIME type. Blocking.
    pub fn preview_artwork(
        &self,
        job_id: &str,
        ordinal: i64,
        sha256: &str,
    ) -> Result<(Vec<u8>, String), ManagementError> {
        let page = self.items(job_id, ordinal - 1, 1, &ItemFilter::default())?;
        let item = page
            .items
            .into_iter()
            .find(|item| item.ordinal == ordinal)
            .ok_or(ManagementError::NotFound(
                "That file is not in the preview.",
            ))?;
        let choice = item
            .desired
            .artwork
            .iter()
            .find(|choice| choice.blob_sha256 == sha256)
            .ok_or(ManagementError::NotFound(
                "That artwork is not in the preview.",
            ))?;
        let bytes = self
            .setup
            .db
            .read_blocking("management.preview_artwork", |conn| {
                Ok::<_, ManagementError>(self.setup.core.blobs.get(conn, sha256).ok())
            })?
            .ok_or(ManagementError::NotFound(
                "That artwork is no longer kept; make a new preview.",
            ))?;
        Ok((bytes, choice.mime_type.clone()))
    }

    /// A page of applied results. Blocking.
    pub fn results(
        &self,
        job_id: &str,
        after: i64,
        limit: i64,
    ) -> Result<ResultPage, ManagementError> {
        self.snapshot(job_id)?;
        let limit = limit.clamp(1, PAGE_MAX);
        self.setup.db.read_blocking("management.results", |conn| {
            Ok(store::results(conn, job_id, after, limit)?)
        })
    }

    /// Set a preview aside without applying it. Blocking.
    pub fn discard(
        &self,
        job_id: &str,
        expected_row_revision: i64,
    ) -> Result<JobDetail, ManagementError> {
        self.snapshot(job_id)?;
        let discarded =
            self.setup
                .db
                .write_blocking(Lane::Foreground, "management.discard", |tx| {
                    Ok::<_, ManagementError>(store::discard(
                        tx,
                        job_id,
                        expected_row_revision,
                        self.setup.clock.now_secs(),
                    )?)
                })?;
        if !discarded {
            return Err(stale(
                "This preview changed or is already applying. Reload the page to see where it is.",
            ));
        }
        self.detail(job_id)
    }

    /// A new token for a ready preview, for its owner whose browser lost
    /// the first one. The old token stops working. Blocking.
    pub fn reissue(&self, job_id: &str, actor: &str) -> Result<PreviewHandle, ManagementError> {
        let snapshot = self.snapshot(job_id)?;
        if snapshot.job.requested_by_user_id.as_deref() != Some(actor) {
            tracing::warn!(
                job_id,
                "preview token reissue refused: another administrator's preview"
            );
            return Err(ManagementError::Forbidden(
                "This preview belongs to another administrator.",
            ));
        }
        refuse_activation(&snapshot)?;
        let detail = self.detail(job_id)?;
        if !detail.ready_for_confirmation {
            return Err(stale(
                "This preview is no longer ready to apply. Make a new preview.",
            ));
        }
        let (token, token_hash) = self.token();
        let now = self.setup.clock.now_secs();
        self.setup
            .db
            .write_blocking(Lane::Foreground, "management.reissue", |conn| {
                Ok::<_, ManagementError>(store::rotate_token(conn, job_id, &token_hash, now)?)
            })?;
        tracing::info!(job_id, "preview token reissued");
        Ok(PreviewHandle {
            job_id: job_id.to_owned(),
            preview_token: token,
            created_at: snapshot
                .preview_created_at
                .unwrap_or(snapshot.job.created_at),
            expires_at: snapshot
                .preview_expires_at
                .unwrap_or(snapshot.job.created_at),
            existing: false,
        })
    }

    /// Queue a ready preview's albums for writing. Blocking.
    pub fn apply(
        &self,
        job_id: &str,
        request: ApplyRequest,
    ) -> Result<OperationJob, ManagementError> {
        if !request.confirmation {
            return Err(ManagementError::Invalid(
                "Confirm that you want these files changed before applying.".to_owned(),
            ));
        }
        if request.idempotency_key.trim().is_empty() {
            return Err(ManagementError::Invalid(
                "An apply request key is required.".to_owned(),
            ));
        }
        let snapshot = self.snapshot(job_id)?;
        let presented = sha256_hex(request.preview_token.as_bytes());
        let matches = snapshot
            .token_hash
            .as_deref()
            .is_some_and(|stored| constant_time_eq(stored.as_bytes(), presented.as_bytes()));
        if !matches {
            return Err(ManagementError::Invalid(
                "This preview's confirmation token is not valid. Reopen the preview to get a new one."
                    .to_owned(),
            ));
        }
        refuse_activation(&snapshot)?;
        // The same apply sent again answers with the job as it is now.
        if self
            .setup
            .db
            .read_blocking("management.apply_key", |conn| {
                Ok::<_, ManagementError>(store::apply_key(conn, job_id)?)
            })?
            .as_deref()
            == Some(request.idempotency_key.as_str())
        {
            return self.setup.db.read_blocking("management.apply_job", |conn| {
                jobs::job(conn, job_id)?.ok_or(ManagementError::NotFound(
                    "This management job does not exist.",
                ))
            });
        }
        let detail = self.detail(job_id)?;
        if detail.expired {
            return Err(stale("This preview expired. Make a new preview."));
        }
        if let Some(reason) = detail.stale_reasons.first() {
            return Err(ManagementError::Conflict(format!(
                "{reason} Make a new preview."
            )));
        }
        if !detail.ready_for_confirmation {
            return Err(stale("This preview is not ready to apply."));
        }
        let mode = match snapshot.job_mode() {
            Some(
                mode @ (JobMode::Undo | JobMode::BaselineRestore | JobMode::DuplicateResolution),
            ) => mode,
            _ => JobMode::Apply,
        };
        if mode == JobMode::DuplicateResolution {
            if detail.job.row_revision != request.expected_row_revision {
                return Err(stale(
                    "This preview changed since the page loaded. Reload the page and try again.",
                ));
            }
            if self.recycle_for_duplicate(&snapshot)? {
                // The incoming file is in the recycle bin: nothing else
                // to write.
                return self.setup.db.write_blocking(
                    Lane::Foreground,
                    "management.complete_without_work",
                    |tx| {
                        store::complete_without_work(
                            tx,
                            job_id,
                            "DUPLICATE_RECYCLED",
                            self.setup.clock.now_secs(),
                        )?;
                        store::record_apply_key(tx, job_id, &request.idempotency_key)?;
                        jobs::job(tx, job_id)?.ok_or(ManagementError::NotFound(
                            "This management job does not exist.",
                        ))
                    },
                );
            }
        }
        self.begin(
            job_id,
            mode,
            request.expected_row_revision,
            &request.idempotency_key,
        )
    }

    /// Turn a ready job into queued apply work, one row per album.
    pub(super) fn begin(
        &self,
        job_id: &str,
        mode: JobMode,
        expected_row_revision: i64,
        idempotency_key: &str,
    ) -> Result<OperationJob, ManagementError> {
        let phase = match mode {
            JobMode::Undo => ManagementPhase::Undoing,
            JobMode::BaselineRestore => ManagementPhase::Restoring,
            _ => ManagementPhase::Applying,
        };
        let now = self.setup.clock.now_secs();
        // An automatic apply is the worker's; anything else is a request.
        let lane = if mode == JobMode::AutomaticApply {
            Lane::Background
        } else {
            Lane::Foreground
        };
        let job = self.setup.db.write_blocking(lane, "management.begin", |tx| {
            let current = jobs::job(tx, job_id)?.ok_or(ManagementError::NotFound(
                "This management job does not exist.",
            ))?;
            if current.row_revision != expected_row_revision {
                return Err(stale(
                    "This preview changed since the page loaded. Reload the page and try again.",
                ));
            }
            if let Some(snapshot) = store::snapshot(tx, job_id)? {
                refuse_activation(&snapshot)?;
            }
            let bundles = store::writable_bundles(tx, job_id)?;
            if bundles.is_empty() && mode == JobMode::DuplicateResolution {
                // Keeping both files: the decision is the whole job.
                store::complete_without_work(tx, job_id, "DUPLICATE_KEPT", now)?;
                store::record_apply_key(tx, job_id, idempotency_key)?;
                return jobs::job(tx, job_id)?.ok_or(ManagementError::NotFound(
                    "This management job does not exist.",
                ));
            }
            let edit = store::snapshot(tx, job_id)?.and_then(|snapshot| snapshot.intent().tag_edit);
            if bundles.is_empty()
                && let Some(edit) = edit.filter(|edit| edit.mode != TagEditMode::WriteOnce)
            {
                // The files already hold these values: only the kept
                // override changes.
                store::save_tag_edit(
                    tx,
                    &edit,
                    job_id,
                    current.requested_by_user_id.as_deref(),
                    now,
                )?;
                store::complete_without_work(tx, job_id, "TAG_EDIT_SAVED", now)?;
                store::record_apply_key(tx, job_id, idempotency_key)?;
                return jobs::job(tx, job_id)?.ok_or(ManagementError::NotFound(
                    "This management job does not exist.",
                ));
            }
            if bundles.is_empty() {
                return Err(ManagementError::Conflict(
                    "Nothing in this preview can be written: every file is unchanged or blocked. \
                     See each file's reason."
                        .to_owned(),
                ));
            }
            store::begin_apply(
                tx,
                job_id,
                mode.as_str(),
                phase,
                idempotency_key,
                &bundles,
                now,
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(failure, _)
                    if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    stale("This preview is already applying.")
                }
                other => other.into(),
            })?;
            jobs::job(tx, job_id)?.ok_or(ManagementError::NotFound(
                "This management job does not exist.",
            ))
        })?;
        jobs::wake(&self.setup.wakeups, OperationKind::LibraryManagement);
        Ok(job)
    }

    /// Plan putting an applied job back, as a new preview. Blocking.
    pub fn undo_preview(
        &self,
        job_id: &str,
        expected_row_revision: i64,
        idempotency_key: &str,
        actor: &str,
    ) -> Result<PreviewHandle, ManagementError> {
        let source = self.snapshot(job_id)?;
        if source.job.row_revision != expected_row_revision {
            return Err(stale(
                "This operation changed since the page loaded. Reload the page and try again.",
            ));
        }
        if !matches!(
            source.job_mode(),
            Some(JobMode::Apply | JobMode::AutomaticApply)
        ) || source.phase != ManagementPhase::Complete
        {
            return Err(stale("Only a finished apply can be undone."));
        }
        let bundles = self
            .setup
            .db
            .read_blocking("management.applied_bundles", |conn| {
                Ok::<_, ManagementError>(store::applied_bundles(conn, job_id)?)
            })?;
        if bundles.is_empty() {
            return Err(stale(
                "This operation wrote nothing, so there is nothing to undo.",
            ));
        }
        let now = self.setup.clock.now_secs();
        if let Some(existing) = self
            .setup
            .db
            .read_blocking("management.undo_preview_job_for_key", |conn| {
                Ok::<_, ManagementError>(store::job_for_key(conn, idempotency_key)?)
            })?
        {
            let snapshot = self.snapshot(&existing)?;
            if snapshot.linked_job_id.as_deref() != Some(job_id)
                || snapshot.job.requested_by_user_id.as_deref() != Some(actor)
            {
                return Err(stale(
                    "This request key was already used for something else.",
                ));
            }
            let (token, token_hash) = self.token();
            self.setup.db.write_blocking(
                Lane::Foreground,
                "management.undo_preview_rotate_token",
                |conn| {
                    Ok::<_, ManagementError>(store::rotate_token(
                        conn,
                        &existing,
                        &token_hash,
                        now,
                    )?)
                },
            )?;
            return Ok(PreviewHandle {
                job_id: existing,
                preview_token: token,
                created_at: snapshot
                    .preview_created_at
                    .unwrap_or(snapshot.job.created_at),
                expires_at: snapshot
                    .preview_expires_at
                    .unwrap_or(snapshot.job.created_at),
                existing: true,
            });
        }
        let undos = self
            .setup
            .db
            .read_blocking("management.live_undos", |conn| {
                Ok::<_, ManagementError>(store::live_undos(conn, job_id, now)?)
            })?;
        if undos.iter().any(|(state, phase)| {
            *state == OperationState::Succeeded && *phase == ManagementPhase::Complete
        }) {
            return Err(stale(
                "This operation was already undone, so there is nothing left to undo.",
            ));
        }
        if !undos.is_empty() {
            return Err(stale(
                "An undo for this operation is already waiting or running. Open it from the history.",
            ));
        }
        let stored = self.settings()?;
        let expires_at = now + stored.preview_retention_hours.max(1) as f64 * 3600.0;
        let (token, token_hash) = self.token();
        let new_id = self.setup.ids.new_id();
        let revisions = super::models::Revisions {
            profile: source.profile_revision.clone(),
            settings: source.settings_revision.clone(),
            naming: source.naming_revision.clone(),
            policy: source.policy_revision.clone(),
            overrides: String::new(),
        };
        let summary = Summary::default();
        self.setup.db.write_blocking(
            Lane::Foreground,
            "management.undo_preview_insert_job",
            |tx| {
                store::insert_job(
                    tx,
                    &NewJob {
                        id: &new_id,
                        requested_by: Some(actor),
                        idempotency_key: Some(idempotency_key),
                        mode: JobMode::Undo.as_str(),
                        selection_json: &source.selection_json,
                        profile_snapshot_json: &source.profile_snapshot_json,
                        revisions: &revisions,
                        catalog_revision: source.catalog_revision,
                        token_hash: &token_hash,
                        now,
                        expires_at,
                        target_root_id: source.target_root_id.as_deref(),
                        linked_job_id: Some(job_id),
                        summary: &summary,
                        origin: Origin::Manual,
                        intent: &JobIntent::default(),
                        proposed_settings_revision: None,
                    },
                )
            },
        )?;
        jobs::wake(&self.setup.wakeups, OperationKind::LibraryManagement);
        Ok(PreviewHandle {
            job_id: new_id,
            preview_token: token,
            created_at: now,
            expires_at,
            existing: false,
        })
    }

    /// Management history, newest first. Blocking.
    pub fn history(
        &self,
        mut filter: HistoryFilter,
    ) -> Result<(Vec<HistoryItem>, Option<String>), ManagementError> {
        // No limit means a full page.
        filter.limit = if filter.limit <= 0 {
            HISTORY_MAX
        } else {
            filter.limit.min(HISTORY_MAX)
        };
        self.setup.db.read_blocking("management.history", |conn| {
            Ok(store::history(conn, &filter)?)
        })
    }

    // -----------------------------------------------------------------
    // The worker.
    // -----------------------------------------------------------------

    /// Requeue every management job an earlier process left running, with
    /// the album it was writing. The loop's first turn calls this.
    /// Blocking.
    pub fn recover_at_start(&self) -> Result<u64, ManagementError> {
        self.setup
            .db
            .write_blocking(Lane::Background, "management.recover_at_start", |tx| {
                Ok::<_, ManagementError>(jobs::recover_at_start(
                    tx,
                    OperationKind::LibraryManagement,
                    self.setup.clock.now_secs(),
                )?)
            })
    }

    /// Requeue the management jobs whose worker vanished, then claim and
    /// run the next queued one under `worker`, never while a scan runs.
    /// True when a job was worked on. Blocking.
    pub fn run_next(&self, worker: &str) -> Result<bool, ManagementError> {
        if !self.setup.coordinator.current().is_empty() {
            return Ok(false);
        }
        let now = self.setup.clock.now_secs();
        let Some((job_id, phase)) =
            self.setup
                .db
                .write_blocking(Lane::Background, "management.claim", |tx| {
                    let Some(job) = jobs::claim(tx, OperationKind::LibraryManagement, worker, now)?
                    else {
                        return Ok::<_, ManagementError>(None);
                    };
                    let phase = store::phase(tx, &job.id)?;
                    Ok(Some((job.id, phase)))
                })?
        else {
            return Ok(false);
        };
        let outcome = match phase {
            Some(ManagementPhase::Planning) => self.plan(&job_id, worker),
            _ => self.apply_work(&job_id, worker),
        };
        match outcome {
            Ok(()) => Ok(true),
            // Waiting is not the job's fault (a busy database, or no recycle
            // bin known yet): it goes back to the queue as it stood, as at a
            // shutdown, with why it waits on its work, and the worker waits
            // before it takes it again. A put-back that fails leaves it to
            // its lease.
            Err(ManagementError::Busy(cause)) => {
                if super::logged_elsewhere(&cause) {
                    tracing::debug!(job_id, "management job waits: {cause}");
                } else {
                    tracing::warn!(job_id, "management job waits: {cause}");
                }
                let released =
                    self.setup
                        .db
                        .write_blocking(Lane::Background, "management.release", |tx| {
                            let now = self.setup.clock.now_secs();
                            Ok::<_, ManagementError>(jobs::release_until(
                                tx,
                                &job_id,
                                worker,
                                now,
                                Some(&cause),
                                now,
                            )?)
                        });
                if let Err(error) = released {
                    tracing::warn!(%error, job_id, "management job not put back; its lease brings it back");
                }
                Err(ManagementError::Busy(cause))
            }
            Err(error) => {
                tracing::error!(%error, job_id, "management job failed");
                self.setup
                    .db
                    .write_blocking(Lane::Background, "management.fail_job", |tx| {
                        Ok::<_, ManagementError>(store::fail_job(
                            tx,
                            &job_id,
                            worker,
                            "MANAGEMENT_FAILED",
                            self.setup.clock.now_secs(),
                        )?)
                    })?;
                Ok(true)
            }
        }
    }

    /// Honour a pause, a stop, or a server shutdown. True when the job
    /// left `running` (or went back to the queue) and the step must end.
    pub(super) fn checkpoint(&self, job_id: &str, worker: &str) -> Result<bool, ManagementError> {
        if self
            .shutdown
            .as_ref()
            .is_some_and(|shutdown| shutdown.is_cancelled())
        {
            self.setup
                .db
                .write_blocking(Lane::Background, "management.release", |tx| {
                    Ok::<_, ManagementError>(jobs::release(
                        tx,
                        job_id,
                        worker,
                        self.setup.clock.now_secs(),
                    )?)
                })?;
            return Ok(true);
        }
        self.setup
            .db
            .write_blocking(Lane::Background, "management.checkpoint", |tx| {
                Ok(jobs::checkpoint(tx, job_id, worker, self.setup.clock.now_secs())?.is_some())
            })
    }

    pub(super) fn heartbeat(&self, job_id: &str, worker: &str) -> Result<bool, ManagementError> {
        self.setup
            .db
            .write_blocking(Lane::Background, "management.heartbeat", |conn| {
                Ok(jobs::heartbeat(
                    conn,
                    job_id,
                    worker,
                    self.setup.clock.now_secs(),
                )?)
            })
    }

    pub(super) fn pinned(snapshot: &Snapshot) -> Result<PinnedProfile, ManagementError> {
        serde_json::from_str(&snapshot.profile_snapshot_json).map_err(|error| {
            ManagementError::Store(format!("stored profile snapshot unreadable: {error}"))
        })
    }

    /// Plan every album of a job, then mark it ready. Planning starts over
    /// when a job comes back to it.
    fn plan(&self, job_id: &str, worker: &str) -> Result<(), ManagementError> {
        let snapshot = self.snapshot(job_id)?;
        let automatic = snapshot.origin != Origin::Manual.as_str();
        // Where the app's own bookkeeping stood before the files are read:
        // a file it writes while this plan is made is not someone else's
        // change (the plan is then stale and its apply is refused).
        let before = if automatic {
            let selection: Selection = serde_json::from_str(&snapshot.selection_json)?;
            self.setup
                .db
                .read_blocking("management.state_revisions", |conn| {
                    Ok::<_, ManagementError>(takeover::state_revisions(conn, &selection.ids)?)
                })?
        } else {
            Default::default()
        };
        self.setup
            .db
            .write_blocking(Lane::Background, "management.clear_items", |conn| {
                Ok::<_, ManagementError>(store::clear_items(conn, job_id)?)
            })?;
        let planned = match snapshot.job_mode() {
            Some(JobMode::Undo) => self.plan_undo(&snapshot, worker)?,
            Some(JobMode::BaselineRestore) => self.plan_baseline_restore(&snapshot, worker)?,
            Some(JobMode::DuplicateResolution) => self.plan_duplicate(&snapshot, worker)?,
            _ => self.plan_preview(&snapshot, worker)?,
        };
        let Some(summary) = planned else {
            return Ok(());
        };
        let (albums, changed) = if automatic {
            self.changed_outside(job_id, &before)?
        } else {
            Default::default()
        };
        let takeover::Changed {
            counted: changed,
            accepted,
        } = changed;
        let now = self.setup.clock.now_secs();
        let (finished, stepped_back) =
            self.setup
                .db
                .write_blocking(Lane::Background, "management.finish_planning", |tx| {
                    if !store::finish_planning(tx, job_id, worker, &summary, now)? {
                        return Ok::<_, ManagementError>((false, false));
                    }
                    // An album marked after this run was queued (an undo, or a
                    // second run after a crash) is never written by it, nor one a
                    // scan stamped as followed across a retag that the worker has
                    // not marked yet.
                    for album in &albums {
                        if takeover::is_marked(tx, album)?
                            || takeover::mark_followed(tx, album, now)?
                        {
                            store::set_mode(tx, job_id, JobMode::AutomaticApply.as_str())?;
                            store::complete_without_work(tx, job_id, takeover::TAKEN_OVER, now)?;
                            return Ok((true, true));
                        }
                    }
                    if changed.is_empty() {
                        // Changed only where the profile does not reach: those
                        // bytes are now as the app wants them, so this run's own
                        // write records what it places.
                        takeover::accept(tx, &accepted)?;
                        return Ok((true, false));
                    }
                    // A file the app wrote was changed outside it, and this run
                    // would write over that change. The run ends here with nothing
                    // written, in the transaction that would have made it ready,
                    // so it never waits for a person to apply it either.
                    store::set_mode(tx, job_id, JobMode::AutomaticApply.as_str())?;
                    store::complete_without_work(tx, job_id, takeover::FILES_CHANGED_OUTSIDE, now)?;
                    let mut albums: Vec<&str> =
                        changed.iter().map(|(album, _)| album.as_str()).collect();
                    albums.sort_unstable();
                    albums.dedup();
                    for album in albums {
                        let tracks: Vec<String> = changed
                            .iter()
                            .filter(|(of, _)| of == album)
                            .map(|(_, track)| track.clone())
                            .collect();
                        takeover::mark(
                            tx,
                            album,
                            takeover::Cause::FilesChanged,
                            &tracks,
                            Some(job_id),
                            now,
                        )?;
                    }
                    Ok((true, true))
                })?;
        if stepped_back {
            tracing::info!(
                job_id,
                files = changed.len(),
                "automatic management stepped back from the album"
            );
        } else if finished
            && automatic
            && let Err(error) = self.apply_automatic(job_id, &summary)
        {
            // The automatic worker looks at the albums again and applies
            // the preview then.
            tracing::warn!(%error, job_id, "automatic management apply waits: the database is busy");
            for album in &albums {
                super::automatic::queue_automatic_management(
                    &self.setup.db,
                    &*self.setup.clock,
                    album,
                );
            }
        }
        Ok(())
    }

    /// The albums a planned job covers, and its files that were changed
    /// since the app wrote them. `before` is each track's state revision
    /// before planning.
    fn changed_outside(
        &self,
        job_id: &str,
        before: &std::collections::HashMap<String, i64>,
    ) -> Result<(Vec<String>, takeover::Changed), ManagementError> {
        self.setup
            .db
            .read_blocking("management.changed_outside", |conn| {
                let mut albums = Vec::new();
                let mut changed = takeover::Changed::default();
                for ordinal in store::bundle_ordinals(conn, job_id)? {
                    let items = store::bundle_items(conn, job_id, ordinal)?;
                    albums.extend(items.iter().map(|item| item.album_id.clone()));
                    let found = takeover::changed_outside(conn, &items, before)?;
                    changed.counted.extend(found.counted);
                    changed.accepted.extend(found.accepted);
                }
                albums.sort_unstable();
                albums.dedup();
                Ok((albums, changed))
            })
    }

    /// Apply a preview automatic management made, when nothing in it is
    /// blocked (as v2 did). A preview with a blocked file waits on the
    /// operations page with each file's reason, for an administrator.
    /// True when the run applies or had nothing to apply; `Busy` when the
    /// database was too busy to start it, and the preview waits.
    pub(super) fn apply_automatic(
        &self,
        job_id: &str,
        summary: &Summary,
    ) -> Result<bool, ManagementError> {
        if summary.blocked_count > 0 || summary.stale_count > 0 {
            tracing::info!(
                job_id,
                blocked = summary.blocked_count,
                "automatic management preview has blocked files; it waits for review"
            );
            return Ok(false);
        }
        let outcome = self
            .setup
            .db
            .read_blocking("management.apply_automatic", |conn| {
                Ok(jobs::job(conn, job_id)?)
            })
            .and_then(|job| {
                let job = job.ok_or(ManagementError::NotFound(
                    "This management job does not exist.",
                ))?;
                self.begin(
                    job_id,
                    JobMode::AutomaticApply,
                    job.row_revision,
                    &format!("automatic-apply:{job_id}"),
                )
            });
        match outcome {
            Ok(_) => {
                tracing::info!(job_id, "automatic management applies");
                Ok(true)
            }
            // Nothing to write: the files already look the way the
            // profile wants them.
            Err(ManagementError::Conflict(reason)) => {
                tracing::info!(job_id, reason, "automatic management had nothing to apply");
                match self.finish_unchanged(job_id) {
                    Err(error @ ManagementError::Busy(_)) => return Err(error),
                    Err(error) => {
                        tracing::warn!(%error, job_id, "unchanged automatic run not recorded");
                    }
                    Ok(()) => {}
                }
                Ok(true)
            }
            Err(error @ ManagementError::Busy(_)) => Err(error),
            Err(error) => {
                tracing::warn!(%error, job_id, "automatic management apply refused");
                Ok(false)
            }
        }
    }

    /// An automatic run whose files already look the way the profile
    /// wants: it ends as done, and its files count as managed, so the
    /// album is not looked at again for nothing.
    fn finish_unchanged(&self, job_id: &str) -> Result<(), ManagementError> {
        let ordinals = self.setup.db.write_blocking(
            Lane::Background,
            "management.finish_unchanged",
            |tx| {
                store::set_mode(tx, job_id, JobMode::AutomaticApply.as_str())?;
                store::complete_without_work(
                    tx,
                    job_id,
                    "MANAGEMENT_UNCHANGED",
                    self.setup.clock.now_secs(),
                )?;
                Ok::<_, ManagementError>(store::bundle_ordinals(tx, job_id)?)
            },
        )?;
        let snapshot = self.snapshot(job_id)?;
        for ordinal in ordinals {
            self.after_album(&snapshot, ordinal);
        }
        Ok(())
    }

    /// Store one album's items. False when the job was paused, stopped
    /// or taken away, so planning stops.
    pub(super) fn keep_items(
        &self,
        snapshot: &Snapshot,
        worker: &str,
        items: &[PlannedItem],
    ) -> Result<bool, ManagementError> {
        let job_id = snapshot.job.id.as_str();
        self.setup
            .db
            .write_blocking(Lane::Background, "management.keep_items", |tx| {
                store::insert_items(tx, job_id, snapshot, items, self.setup.clock.now_secs())
            })?;
        if self.checkpoint(job_id, worker)? {
            return Ok(false);
        }
        self.heartbeat(job_id, worker)
    }

    fn plan_preview(
        &self,
        snapshot: &Snapshot,
        worker: &str,
    ) -> Result<Option<Summary>, ManagementError> {
        let pinned = Self::pinned(snapshot)?;
        let intent = planning_intent(snapshot)?;
        let selection: Selection = serde_json::from_str(&snapshot.selection_json)?;
        let scripts = CompiledScripts::new(&pinned);
        let sandbox = self.setup.publishing.publish_sandbox().map_err(|error| {
            ManagementError::Conflict(format!("No library folder can be written: {error}"))
        })?;
        let registry = self.setup.core.live_registry();
        let mut albums = self
            .setup
            .db
            .read_blocking("management.preview_albums", |conn| {
                selection::albums(conn, &selection)
            })?;
        let ctx = PlanContext {
            setup: &self.setup,
            pinned: &pinned,
            scripts: &scripts,
            sandbox: &sandbox,
            registry: &registry,
            target_root: snapshot.target_root_id.as_deref(),
            manage_unconfirmed: planner::manage_unconfirmed(&self.setup),
            tag_edit: intent.tag_edit.as_ref(),
            job_id: &snapshot.job.id,
            analysis_until: std::time::Instant::now() + planner::RUN_ANALYSIS_BUDGET,
            // A folder, an artist or a filter is wide whatever it holds
            // today; albums or tracks are wide once they span two albums.
            wide: albums
                .iter()
                .map(|scope| scope.album_id.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 1
                || matches!(
                    selection.kind,
                    SelectionKind::Roots | SelectionKind::Artists | SelectionKind::Filter
                ),
        };
        // Files that move take their whole album along, so an album is
        // never split between its old and new folder (as in v2). An
        // album-wide tag edit changes every file of the album too.
        let organizes = pinned.profile.organization.rename_enabled
            || pinned.profile.organization.move_enabled
            || snapshot.target_root_id.is_some()
            || intent.tag_edit.as_ref().is_some_and(|edit| {
                edit.fields
                    .iter()
                    .any(|field| field.subject_kind == "album")
            });
        let picked: usize = albums
            .iter()
            .filter_map(|scope| scope.track_ids.as_ref().map(Vec::len))
            .sum();
        if organizes {
            for scope in &mut albums {
                scope.track_ids = None;
            }
        }
        let person = snapshot.origin == Origin::Manual.as_str();
        let mut summary = Summary::default();
        let mut ordinal = 0;
        for (bundle_ordinal, scope) in albums.iter().enumerate() {
            let tracks = self
                .setup
                .db
                .read_blocking("management.preview_tracks", |conn| {
                    selection::tracks(conn, scope)
                })?;
            if tracks.is_empty() {
                continue;
            }
            let held = ctx.wide && person;
            let before = if held {
                self.setup
                    .db
                    .read_blocking("management.plan_preview_state_revisions", |conn| {
                        Ok::<_, ManagementError>(takeover::state_revisions(
                            conn,
                            std::slice::from_ref(&scope.album_id),
                        )?)
                    })?
            } else {
                Default::default()
            };
            let mut items = planner::plan_album(
                &ctx,
                scope,
                &tracks,
                bundle_ordinal as i64,
                ordinal,
                person,
                intent.edition.as_ref(),
            )?;
            if held {
                self.hold_changed(&scope.album_id, &snapshot.job.id, &mut items, &before)?;
            }
            ordinal += items.len() as i64;
            summary.bundle_count += 1;
            for item in &items {
                summary.count(item);
            }
            if !self.keep_items(snapshot, worker, &items)? {
                return Ok(None);
            }
        }
        summary.selected_item_count = Some(if picked > 0 {
            picked as i64
        } else {
            summary.item_count
        });
        summary.expanded_track_count = summary.item_count;
        Ok(Some(summary))
    }

    /// A person's preview of more than one album never writes over a file
    /// changed outside the app that automatic management has not looked at
    /// yet: the check it runs blocks the album here, and where the folder
    /// is managed automatically the album is marked as it would be.
    /// Previewing the album by itself overwrites the change.
    fn hold_changed(
        &self,
        album_id: &str,
        job_id: &str,
        items: &mut [PlannedItem],
        before: &std::collections::HashMap<String, i64>,
    ) -> Result<(), ManagementError> {
        let changed = self
            .setup
            .db
            .read_blocking("management.hold_changed_changed_outside", |conn| {
                Ok::<_, ManagementError>(takeover::changed_outside(conn, items, before)?)
            })?
            .counted;
        if changed.is_empty() {
            return Ok(());
        }
        for item in items.iter_mut().filter(|item| item.eligibility.writes()) {
            let own = changed
                .iter()
                .any(|(_, track)| item.track_id.as_deref() == Some(track.as_str()));
            if own {
                item.block(takeover::changed_outside_reason());
            } else if item.diff.changes_anything() {
                item.block(reasons::bundle_blocked());
            }
        }
        if self.managed_automatically(album_id)? {
            let tracks: Vec<String> = changed.into_iter().map(|(_, track)| track).collect();
            let now = self.setup.clock.now_secs();
            self.setup
                .db
                .write_blocking(Lane::Background, "management.hold_changed", |tx| {
                    Ok::<_, ManagementError>(takeover::mark(
                        tx,
                        album_id,
                        takeover::Cause::FilesChanged,
                        &tracks,
                        Some(job_id),
                        now,
                    )?)
                })?;
        }
        Ok(())
    }

    /// Plan putting each album of the linked apply back.
    fn plan_undo(
        &self,
        snapshot: &Snapshot,
        worker: &str,
    ) -> Result<Option<Summary>, ManagementError> {
        let source_id = snapshot
            .linked_job_id
            .clone()
            .ok_or_else(|| ManagementError::Store("undo job has no source operation".to_owned()))?;
        let bundles = self
            .setup
            .db
            .read_blocking("management.plan_undo_applied_bundles", |conn| {
                Ok::<_, ManagementError>(store::applied_bundles(conn, &source_id)?)
            })?;
        // A partly successful earlier undo: only what it left is retried.
        let undone = self
            .setup
            .db
            .read_blocking("management.undone_ordinals", |conn| {
                Ok::<_, ManagementError>(store::undone_ordinals(conn, &source_id)?)
            })?;
        let bundles: Vec<_> = bundles
            .into_iter()
            .filter(|(ordinal, _)| !undone.contains(ordinal))
            .collect();
        let mut summary = Summary::default();
        let mut ordinal = 0;
        for (bundle_ordinal, bundle_id) in bundles {
            let source_items =
                self.setup
                    .db
                    .read_blocking("management.undo_bundle_items", |conn| {
                        Ok::<_, ManagementError>(store::bundle_items(
                            conn,
                            &source_id,
                            bundle_ordinal,
                        )?)
                    })?;
            let Some(first) = source_items.first().cloned() else {
                continue;
            };
            let items = self.plan_undo_bundle(
                &source_id,
                &bundle_id,
                bundle_ordinal,
                ordinal,
                &first,
                &source_items,
            )?;
            ordinal += items.len() as i64;
            summary.bundle_count += 1;
            for item in &items {
                summary.count(item);
            }
            if !self.keep_items(snapshot, worker, &items)? {
                return Ok(None);
            }
        }
        summary.selected_item_count = Some(summary.item_count);
        summary.expanded_track_count = summary.item_count;
        Ok(Some(summary))
    }

    fn plan_undo_bundle(
        &self,
        source_job: &str,
        bundle_id: &str,
        bundle_ordinal: i64,
        first_ordinal: i64,
        first: &PlannedItem,
        source_items: &[PlannedItem],
    ) -> Result<Vec<PlannedItem>, ManagementError> {
        let album_id = first.album_id.clone();
        let override_of = self.override_lookup(&album_id, Some((source_job, bundle_ordinal)))?;
        let files = match self.setup.publishing.undo_files(bundle_id, &override_of) {
            Ok(files) => files,
            Err(crate::service::ServiceError::NotFound) => Default::default(),
            Err(error) => return Err(ManagementError::Store(format!("{error:?}"))),
        };
        let source_of = |track_id: &str| {
            source_items
                .iter()
                .find(|item| item.track_id.as_deref() == Some(track_id))
        };
        let mut items = Vec::new();
        let template = |track_id: &str, ordinal: i64| -> PlannedItem {
            let mut item = source_of(track_id)
                .cloned()
                .unwrap_or_else(|| first.clone());
            item.ordinal = ordinal;
            item.bundle_ordinal = bundle_ordinal;
            item.track_id = Some(track_id.to_owned());
            item.eligibility = super::models::Eligibility::Eligible;
            item.desired = Default::default();
            item.diff = Default::default();
            item.facts.reason = None;
            item.facts.warnings.clear();
            item.facts.blockers.clear();
            item
        };
        for file in files.eligible {
            let mut item = template(&file.plan.track_id, first_ordinal + items.len() as i64);
            item.source_root = file.plan.source_root.clone();
            item.source_rel = file.plan.source_rel.clone();
            item.dest_root = Some(file.plan.dest_root.clone());
            item.dest_rel = Some(file.plan.dest_rel.clone());
            item.fingerprint = file.plan.fingerprint.sha256.clone();
            item.estimated_bytes = file.plan.staged_bytes_estimate;
            item.diff.path_changed = file.plan.source_root != file.plan.dest_root
                || file.plan.source_rel != file.plan.dest_rel;
            for (name, after) in &file.plan.managed_updates.named() {
                let before = file.current.get(name).cloned().unwrap_or_default();
                if &before == after {
                    continue;
                }
                item.diff
                    .field_mutations
                    .push(super::models::FieldMutation {
                        name: name.clone(),
                        operation: if after.is_empty() { "clear" } else { "set" }.to_owned(),
                        before: profile::json_value(&before),
                        after: profile::json_value(after),
                    });
            }
            item.diff.tags_changed = !item.diff.field_mutations.is_empty();
            item.desired.fields = file
                .plan
                .managed_updates
                .named()
                .into_iter()
                .filter(|(_, values)| !values.is_empty())
                .map(|(name, values)| super::models::DesiredField {
                    name,
                    value: profile::json_value(&values),
                })
                .collect();
            item.diff.sidecars = file
                .plan
                .sidecars
                .iter()
                .map(|sidecar| super::models::SidecarMove {
                    source_relative_path: sidecar.source_rel.clone(),
                    destination_relative_path: sidecar.dest_rel.clone(),
                })
                .collect();
            for left in &file.sidecars_left {
                item.facts.warnings.push(format!(
                    "{left} changed or its old place is taken, so it stays where it is."
                ));
            }
            if !item.facts.warnings.is_empty() {
                item.eligibility = super::models::Eligibility::Warning;
            }
            item.desired.plan = Some(file.plan);
            items.push(item);
        }
        for (track_id, skip) in &files.blocked {
            let mut item = template(track_id, first_ordinal + items.len() as i64);
            item.block(match skip {
                crate::manage::UndoSkip::Plan(block) => reasons::undo_block(block),
                crate::manage::UndoSkip::Gone => ItemReason {
                    code: "FILE_CHANGED".to_owned(),
                    message:
                        "This file is no longer where the change left it, or no longer matched."
                            .to_owned(),
                    action: "Rescan the library, then make a new undo preview.".to_owned(),
                },
                crate::manage::UndoSkip::CopyMissing => ItemReason {
                    code: UNDO_COPY_MISSING.to_owned(),
                    message: "The saved copy of this file's earlier tags is missing.".to_owned(),
                    action: "This file stays as it is.".to_owned(),
                },
            });
            items.push(item);
        }
        if items.is_empty() {
            let mut item = template(first.track_id.as_deref().unwrap_or_default(), first_ordinal);
            item.block(reasons::undo_nothing());
            items.push(item);
        }
        // A file whose saved copy is gone cannot be undone at all, now or
        // later, so it does not hold the rest of the bundle back.
        let holds_back = |item: &PlannedItem| {
            !item.eligibility.writes()
                && item
                    .reason()
                    .is_none_or(|reason| reason.code != UNDO_COPY_MISSING)
        };
        if items.iter().any(holds_back) {
            for item in items.iter_mut().filter(|item| item.eligibility.writes()) {
                item.block(reasons::bundle_blocked());
            }
        }
        Ok(items)
    }

    /// The override revision a write compares for each file of an album.
    /// For an undo of a tag edit, the overrides the edit itself saved do
    /// not count as a change: the file's revision then reads as it was
    /// before the edit, so the edit can be undone.
    pub(super) fn override_lookup<'a>(
        &'a self,
        album_id: &'a str,
        undoes: Option<(&str, i64)>,
    ) -> Result<impl Fn(&str) -> u64 + 'a, ManagementError> {
        let saved = match undoes {
            Some((job, ordinal)) => {
                self.setup
                    .db
                    .read_blocking("management.override_lookup", |conn| {
                        Ok::<_, ManagementError>(store::edit_override_revisions(
                            conn, job, ordinal,
                        )?)
                    })?
            }
            None => Default::default(),
        };
        Ok(move |track_id: &str| {
            let live = self.override_revision(album_id, track_id);
            match saved.get(track_id) {
                Some((after, before)) if *after == live => *before,
                _ => live,
            }
        })
    }

    /// The current revision of a file's saved tag overrides.
    pub(super) fn override_revision(&self, album_id: &str, track_id: &str) -> u64 {
        self.setup
            .db
            .read_blocking("management.override_revision", |conn| {
                Ok::<_, ManagementError>(planner::overrides(conn, album_id, track_id)?)
            })
            .map(|overrides| overrides.revision)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, track_id, "tag overrides unreadable; the file waits");
                u64::MAX
            })
    }

    /// Write the queued albums of an applying job one at a time. A scan
    /// that starts in between hands the job back to the queue until the
    /// scan is done.
    fn apply_work(&self, job_id: &str, worker: &str) -> Result<(), ManagementError> {
        let snapshot = self.snapshot(job_id)?;
        let undo = snapshot.job_mode().is_some_and(JobMode::restores);
        // A profile run writes every managed field of the files it writes.
        let whole_profile = matches!(
            snapshot.job_mode(),
            Some(JobMode::Apply | JobMode::AutomaticApply)
        ) && snapshot.intent().tag_edit.is_none();
        // Whether the job moves files: then an album must still be
        // exactly the files that were planned.
        // A duplicate resolution moves one file of an album on purpose.
        let organizes = snapshot.job_mode() != Some(JobMode::DuplicateResolution)
            && (snapshot.target_root_id.is_some()
                || Self::pinned(&snapshot).is_ok_and(|pinned| {
                    pinned.profile.organization.rename_enabled
                        || pinned.profile.organization.move_enabled
                }));
        loop {
            if self.checkpoint(job_id, worker)? {
                return Ok(());
            }
            if !self.setup.coordinator.current().is_empty() {
                self.setup.db.write_blocking(
                    Lane::Background,
                    "management.apply_work_release",
                    |tx| {
                        Ok::<_, ManagementError>(jobs::release(
                            tx,
                            job_id,
                            worker,
                            self.setup.clock.now_secs(),
                        )?)
                    },
                )?;
                return Ok(());
            }
            let now = self.setup.clock.now_secs();
            let Some(ordinal) =
                self.setup
                    .db
                    .write_blocking(Lane::Background, "management.claim_work", |tx| {
                        Ok::<_, ManagementError>(jobs::claim_work(tx, job_id, now)?)
                    })?
            else {
                let finished = self.setup.db.write_blocking(
                    Lane::Background,
                    "management.finish_apply",
                    |tx| {
                        Ok::<_, ManagementError>(store::finish_apply(
                            tx,
                            job_id,
                            worker,
                            self.setup.clock.now_secs(),
                        )?)
                    },
                )?;
                if finished {
                    self.queue_external_refreshes(&snapshot);
                }
                return Ok(());
            };
            let Some((failure, bundle_id, mut result)) =
                self.apply_album(&snapshot, ordinal, undo, organizes)?
            else {
                // The album's earlier publish is not settled yet: wait for
                // recovery rather than publish it a second time.
                self.setup.db.write_blocking(
                    Lane::Background,
                    "management.apply_work_yield",
                    |tx| {
                        Ok::<_, ManagementError>(jobs::release(
                            tx,
                            job_id,
                            worker,
                            self.setup.clock.now_secs(),
                        )?)
                    },
                )?;
                return Ok(());
            };
            if failure.is_none()
                && let Ok(recycled) = self
                    .setup
                    .db
                    .read_blocking("management.replaced_files", |conn| {
                        Ok::<_, ManagementError>(store::replaced_files(conn, &bundle_id)?)
                    })
                && !recycled.is_empty()
                && let Some(result) = result.as_object_mut()
            {
                result.insert("recycled_to".to_owned(), serde_json::json!(recycled));
            }
            if failure.is_none()
                && let Some(extra) = self.after_album(&snapshot, ordinal)
                && let (Some(result), serde_json::Value::Object(extra)) =
                    (result.as_object_mut(), extra)
            {
                result.extend(extra);
            }
            if failure.is_none() && whole_profile {
                let before = self.record_applied(job_id, ordinal, &bundle_id)?;
                if !before.is_empty()
                    && let Some(result) = result.as_object_mut()
                {
                    result.insert("records_before".to_owned(), before.into());
                }
            }
            // An undo of a profile run gives back the records that run
            // replaced, with the bytes from before it.
            if failure.is_none()
                && snapshot.job_mode() == Some(JobMode::Undo)
                && let Some(source) = snapshot.linked_job_id.as_deref()
            {
                self.setup.db.write_blocking(
                    Lane::Background,
                    "management.restore_records",
                    |tx| Ok::<_, ManagementError>(takeover::restore_records(tx, source, ordinal)?),
                )?;
            }
            self.setup
                .db
                .write_blocking(Lane::Background, "management.finish_work", |tx| {
                    let now = self.setup.clock.now_secs();
                    store::finish_work(tx, job_id, ordinal, failure.as_ref(), &result, now)?;
                    jobs::heartbeat(tx, job_id, worker, now)?;
                    Ok::<_, ManagementError>(())
                })?;
        }
    }

    /// Record the bytes a profile run placed for one album as the app's
    /// (see [`takeover::record_applied`]). Runs again with the album when
    /// a crash came between the commit and here.
    fn record_applied(
        &self,
        job_id: &str,
        ordinal: i64,
        bundle_id: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, ManagementError> {
        let written: std::collections::HashSet<String> = self
            .setup
            .db
            .read_blocking("management.record_applied_bundle_items", |conn| {
                Ok::<_, ManagementError>(store::bundle_items(conn, job_id, ordinal)?)
            })?
            .into_iter()
            .filter(|item| item.desired.plan.is_some() && item.eligibility.writes())
            .filter_map(|item| item.track_id)
            .collect();
        let placed: Vec<(String, String)> = self
            .setup
            .db
            .read_blocking("management.applied_bundle", |conn| {
                JournalStore::new(conn)
                    .bundle(bundle_id)
                    .map_err(|error| ManagementError::Store(error.to_string()))
            })?
            .into_iter()
            .filter_map(|journal| Some((journal.track_id?, journal.staged_sha256)))
            .filter(|(track_id, _)| written.contains(track_id))
            .collect();
        self.setup
            .db
            .write_blocking(Lane::Background, "management.record_applied", |tx| {
                Ok(takeover::record_applied(tx, job_id, ordinal, &placed)?)
            })
    }

    /// Whether the publisher committed `bundle_id`. An unreadable journal
    /// counts as not committed, and says so in the log.
    pub(super) fn committed(&self, bundle_id: &str) -> bool {
        self.setup
            .held_bundles
            .committed(bundle_id)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, bundle_id, "publish journal unreadable");
                false
            })
    }

    /// Apply one album: its outcome, its bundle id and the result to
    /// record, or `None` when an earlier attempt's publish is still
    /// unsettled. The bundle id is recorded on the work row before
    /// anything is written, so an album the publisher committed is never
    /// lost to a crash or a late error: recovery runs first, and a
    /// committed album counts as written and can be undone. A saved bundle
    /// id is never replaced while its journals are unsettled.
    fn apply_album(
        &self,
        snapshot: &Snapshot,
        ordinal: i64,
        undo: bool,
        organizes: bool,
    ) -> Result<Option<(Option<ItemReason>, String, serde_json::Value)>, ManagementError> {
        let job_id = snapshot.job.id.as_str();
        let written = |outcome: &str| serde_json::json!({ "outcome": outcome });
        if let Some(earlier) = self
            .setup
            .db
            .read_blocking("management.work_bundle", |conn| {
                Ok::<_, ManagementError>(jobs::work_bundle(conn, job_id, ordinal)?)
            })?
        {
            if let Err(error) = self.setup.publishing.settle_publishes() {
                tracing::warn!(%error, bundle_id = earlier, "publish recovery did not finish");
            }
            if self.committed(&earlier) {
                return Ok(Some((None, earlier, written("committed"))));
            }
            let unsettled = self
                .setup
                .held_bundles
                .unsettled(&earlier)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, bundle_id = earlier, "publish journal unreadable");
                    true
                });
            if unsettled {
                tracing::info!(
                    bundle_id = earlier,
                    "earlier publish still unsettled; album waits"
                );
                return Ok(None);
            }
        }
        let bundle_id = self.setup.ids.new_id();
        self.setup
            .db
            .write_blocking(Lane::Background, "management.set_work_bundle", |conn| {
                Ok::<_, ManagementError>(jobs::set_work_bundle(conn, job_id, ordinal, &bundle_id)?)
            })?;
        let items = self
            .setup
            .db
            .read_blocking("management.apply_album_bundle_items", |conn| {
                Ok::<_, ManagementError>(store::bundle_items(conn, job_id, ordinal)?)
            })?;
        Ok(Some(
            match self.publish_album(snapshot, &items, undo, organizes, &bundle_id) {
                Ok(applied) => (None, applied.bundle_id, written(applied.outcome.as_str())),
                // The database was busy: the job goes back to the queue. The
                // next pass finds this bundle empty and starts afresh, or
                // finds the publish it stopped and waits for recovery.
                Err(Refused::Busy(error)) => return Err(error),
                Err(_) if self.committed(&bundle_id) => (None, bundle_id, written("committed")),
                // A publish that stopped part way (the journal still holds
                // it) is settled by recovery, which finishes or unwinds it;
                // the album waits for that instead of failing now.
                Err(_)
                    if self
                        .setup
                        .held_bundles
                        .unsettled(&bundle_id)
                        .unwrap_or(true) =>
                {
                    tracing::warn!(
                        bundle_id,
                        "publish stopped part way; album waits for recovery"
                    );
                    return Ok(None);
                }
                Err(Refused::Reason(reason)) => {
                    let result = serde_json::json!({ "reason": reason });
                    (Some(reason), bundle_id, result)
                }
            },
        ))
    }

    /// Why an album may no longer be written as planned: settings, its
    /// match, or its files moved since the preview.
    fn album_gate(
        &self,
        snapshot: &Snapshot,
        items: &[PlannedItem],
        organizes: bool,
    ) -> Result<(), Refused> {
        if !self.stale_reasons(snapshot).is_empty() {
            return Err(Refused::Reason(ItemReason {
                code: "PROFILE_CHANGED".to_owned(),
                message: "Library Management settings or the library folders changed while this run was applying."
                    .to_owned(),
                action: "Make a new preview for the albums that were not written yet.".to_owned(),
            }));
        }
        let Some(first) = items.first() else {
            return Err(reasons::not_scheduled().into());
        };
        let manage_unconfirmed = planner::manage_unconfirmed(&self.setup);
        let state = self
            .setup
            .db
            .read_blocking("management.album_gate", |conn| {
                Ok::<_, ManagementError>((
                    planner::match_gate(conn, &first.album_id, manage_unconfirmed)?,
                    store::album_state(conn, &first.album_id)?,
                ))
            });
        let (gate, (revision, tracks)) =
            state.map_err(|error| Refused::unread(error, &first.album_id, "album state"))?;
        if let Some(reason) = gate {
            return Err(reason.into());
        }
        let planned: std::collections::BTreeSet<&str> = items
            .iter()
            .filter_map(|item| item.track_id.as_deref())
            .collect();
        let current: std::collections::BTreeSet<&str> = tracks.iter().map(String::as_str).collect();
        let membership_holds = if organizes {
            planned == current
        } else {
            planned.is_subset(&current)
        };
        if revision != Some(first.album_revision) || !membership_holds {
            return Err(reasons::album_changed().into());
        }
        Ok(())
    }

    /// Publish one album; the error is the reason every file in it shares.
    fn publish_album(
        &self,
        snapshot: &Snapshot,
        items: &[PlannedItem],
        undo: bool,
        organizes: bool,
        bundle_id: &str,
    ) -> Result<crate::manage::AppliedBundle, Refused> {
        if !undo {
            self.album_gate(snapshot, items, organizes)?;
        }
        let mut plans = planner::bundle_plans(items);
        if plans.is_empty() {
            return Err(reasons::not_scheduled().into());
        }
        if !undo {
            self.plan_replacements(items, &mut plans)?;
        }
        let album_id = items
            .first()
            .map(|item| item.album_id.clone())
            .unwrap_or_default();
        let undoes = match (snapshot.job_mode(), &snapshot.linked_job_id, items.first()) {
            (Some(JobMode::Undo), Some(source), Some(first)) => {
                Some((source.as_str(), first.bundle_ordinal))
            }
            _ => None,
        };
        let override_of = self
            .override_lookup(&album_id, undoes)
            .map_err(|error| Refused::unread(error, &album_id, "tag edit override record"))?;
        let pins = SealPins {
            profile: profile::seal_revision(&snapshot.profile_revision),
            naming: profile::seal_revision(&snapshot.naming_revision),
            settings: profile::seal_revision(&snapshot.settings_revision),
        };
        self.setup
            .publishing
            .publish_plans(bundle_id, plans, pins, &override_of)
            .map_err(|error| {
                // The database refused a call, or no recycle bin is known:
                // the publish stopped where it was, and the job goes back
                // to the queue. The error says which.
                if let crate::publish::PublishError::Interrupted(cause) = &error {
                    tracing::warn!(%error, job_id = snapshot.job.id, "management publish waits");
                    return Refused::Busy(ManagementError::Busy(cause.clone()));
                }
                let reason = reasons::from_publish(&error);
                let detail = error.detail();
                let track_id = detail.and_then(|detail| detail.track_id.as_deref());
                let path = detail
                    .and_then(|detail| detail.file.as_ref())
                    .map(|file| format!("{}:{}", file.root_id, file.rel));
                if reason.code == "WRITE_FAILED" || reason.code == "BASELINE_UNAVAILABLE" {
                    tracing::error!(
                        %error,
                        job_id = snapshot.job.id,
                        track_id,
                        path,
                        "management publish failed"
                    );
                } else {
                    tracing::info!(
                        %error,
                        job_id = snapshot.job.id,
                        track_id,
                        path,
                        "management publish refused"
                    );
                }
                Refused::Reason(reason)
            })
    }
}

/// Why one album was not published.
pub(super) enum Refused {
    /// The reason every file in the album shares.
    Reason(ItemReason),
    /// The database was too busy to read what the album needs; the job
    /// goes back to the queue instead of failing the album.
    Busy(ManagementError),
}

impl From<ItemReason> for Refused {
    fn from(reason: ItemReason) -> Self {
        Self::Reason(reason)
    }
}

impl Refused {
    /// A read the album needs failed: busy waits, anything else leaves
    /// the album alone.
    fn unread(error: ManagementError, album_id: &str, what: &str) -> Self {
        if let ManagementError::Busy(_) = error {
            return Self::Busy(error);
        }
        tracing::error!(%error, album = album_id, "{what} unreadable; album left alone");
        Self::Reason(reasons::write_failed())
    }
}

/// The intent a preview plans with. One that does not read is the
/// default, as v2 rows are, except for an edition's tag write: planning
/// that without its edition could write another one, so the job fails.
fn planning_intent(snapshot: &Snapshot) -> Result<JobIntent, ManagementError> {
    match serde_json::from_str(&snapshot.intent_json) {
        Ok(intent) => Ok(intent),
        Err(error) if snapshot.intent_json.contains("\"edition\"") => Err(ManagementError::Store(
            format!("edition tag write's intent unreadable: {error}"),
        )),
        Err(_) => Ok(JobIntent::default()),
    }
}

/// A dry run for turning on automatic management only proves settings;
/// it is never applied (v2's rule and sentence).
fn refuse_activation(snapshot: &Snapshot) -> Result<(), ManagementError> {
    if snapshot.proposed_settings_revision.is_some() {
        return Err(ManagementError::Invalid(
            "An activation preview cannot be applied.".to_owned(),
        ));
    }
    Ok(())
}

/// Compare two secrets without leaking where they differ.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read an album needs during apply that finds the database busy
    /// sends the job back to the queue; only another failure leaves the
    /// album alone as failed.
    #[test]
    fn a_busy_read_during_apply_never_fails_the_album() {
        let busy = Refused::unread(ManagementError::Busy("busy".to_owned()), "a", "album state");
        assert!(matches!(busy, Refused::Busy(ManagementError::Busy(_))));
        let broken = Refused::unread(
            ManagementError::Store("gone".to_owned()),
            "a",
            "album state",
        );
        assert!(matches!(broken, Refused::Reason(reason) if reason.code == "WRITE_FAILED"));
    }
}
