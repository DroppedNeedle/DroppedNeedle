//! Management entry points: what the HTTP handlers and the worker call.
//!
//! Every store call runs on the identify store's connection, so it must
//! run off the async workers (handlers use `spawn_blocking`; the loop
//! drives its tick on a blocking thread). Identify store methods lock the
//! same connection, so they are never called inside a transaction here.

use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::models::{
    HistoryFilter, HistoryItem, ItemFilter, ItemPage, JobDetail, JobMode, ManagementError,
    PinnedProfile, PlannedItem, PreviewHandle, ResultPage, Selection, Summary,
};
use super::planner::{self, PlanContext};
use super::profile::{self, CompiledScripts};
use super::reasons::{self, ItemReason};
use super::selection;
use super::store::{self, NewJob, Snapshot};
use crate::library::clock::{now_unix, today_day};
use crate::library::manage::SealPins;
use crate::library::operations::models::OperationJob;
use crate::library::operations::store as ops;
use crate::library::publish::publisher::Catalog as _;
use crate::library::publish::{SqliteCatalog, snapshots::sha256_hex};
use crate::library::wiring::LibrarySetup;
use crate::runtime_config::sections::{LibraryManagement, LibraryManagementRootOverrides};

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
    setup: LibrarySetup,
    /// Turns true when the server shuts down; the worker then hands its
    /// job back to the queue between albums.
    shutdown: Option<tokio::sync::watch::Receiver<bool>>,
}

fn stale(message: &str) -> ManagementError {
    ManagementError::Conflict(message.to_owned())
}

impl Management {
    pub fn new(setup: &LibrarySetup) -> Self {
        Self {
            setup: setup.clone(),
            shutdown: None,
        }
    }

    /// Hand running jobs back to the queue once `shutdown` turns true.
    pub fn stopping_with(mut self, shutdown: tokio::sync::watch::Receiver<bool>) -> Self {
        self.shutdown = Some(shutdown);
        self
    }

    fn read<T>(
        &self,
        op: impl FnOnce(&Connection) -> Result<T, ManagementError>,
    ) -> Result<T, ManagementError> {
        self.setup.identify_store.with_connection(|conn| op(conn))
    }

    fn write<T>(
        &self,
        op: impl FnOnce(&Transaction<'_>) -> Result<T, ManagementError>,
    ) -> Result<T, ManagementError> {
        self.setup.identify_store.with_connection(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = op(&tx)?;
            tx.commit()?;
            Ok(value)
        })
    }

    fn settings(&self) -> Result<LibraryManagement, ManagementError> {
        self.setup
            .config
            .get::<LibraryManagement>()
            .map_err(|error| ManagementError::Store(error.to_string()))
    }

    fn snapshot(&self, job_id: &str) -> Result<Snapshot, ManagementError> {
        self.read(|conn| {
            store::snapshot(conn, job_id)?.ok_or(ManagementError::NotFound(
                "This management job does not exist.",
            ))
        })
    }

    /// A random preview token and its hash.
    fn token(&self) -> (String, String) {
        let token =
            format!("{}{}", self.setup.ids.new_id(), self.setup.ids.new_id()).replace('-', "");
        let hash = sha256_hex(token.as_bytes());
        (token, hash)
    }

    // -----------------------------------------------------------------
    // Previews.
    // -----------------------------------------------------------------

    /// Start a preview: pin the profile and revisions, and queue the
    /// planning. A retried request with the same idempotency key returns
    /// the same job with a fresh token. Blocking.
    pub fn create_preview(
        &self,
        request: PreviewRequest,
        actor: &str,
    ) -> Result<PreviewHandle, ManagementError> {
        let stored = self.settings()?;
        let settings_revision = crate::settings::management::settings_revision(&stored);
        if settings_revision != request.expected_settings_revision {
            return Err(stale(
                "Library Management settings changed since this page loaded. Reload the page and make the preview again.",
            ));
        }
        let registry = self.setup.live_registry();
        if registry.policy_revision() != request.expected_policy_revision {
            return Err(stale(
                "The library folders or their rules changed since this page loaded. Reload the page and make the preview again.",
            ));
        }
        let selection = selection::normalize(request.selection)?;
        let pinned = profile::pin(&stored, &request.profile_id, request.overrides)?;
        if let Some(target) = request.target_root_id.as_deref() {
            if registry.resolve(target).is_none_or(|root| {
                root.policy == crate::library::scan::models::EffectivePolicy::Excluded
            }) {
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
        let revisions = profile::revisions(&stored, &pinned, registry.policy_revision());
        let selection_json = serde_json::to_string(&selection)?;
        let profile_json = serde_json::to_string(&pinned)?;
        let now = now_unix();
        let (token, token_hash) = self.token();
        if let Some(key) = request.idempotency_key.as_deref()
            && let Some(existing) = self.read(|conn| Ok(store::job_for_key(conn, key)?))?
        {
            let snapshot = self.snapshot(&existing)?;
            let same = snapshot.selection_json == selection_json
                && snapshot.profile_revision == revisions.profile
                && snapshot.settings_revision == revisions.settings
                && snapshot.target_root_id == request.target_root_id
                && snapshot.job.requested_by_user_id.as_deref() == Some(actor);
            if !same {
                return Err(stale(
                    "This request key was already used for a different preview. Make the preview again.",
                ));
            }
            self.read(|conn| Ok(store::rotate_token(conn, &existing, &token_hash, now)?))?;
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
        let catalog_revision = self.read(|conn| {
            SqliteCatalog
                .revision(conn)
                .map_err(|error| ManagementError::Store(error.to_string()))
        })?;
        let expires_at = now + stored.preview_retention_hours.max(1) as f64 * 3600.0;
        let job_id = self.setup.ids.new_id();
        let summary = Summary::default();
        self.write(|tx| {
            store::insert_job(
                tx,
                &NewJob {
                    id: &job_id,
                    requested_by: actor,
                    idempotency_key: request.idempotency_key.as_deref(),
                    mode: JobMode::Preview.as_str(),
                    selection_json: &selection_json,
                    profile_snapshot_json: &profile_json,
                    revisions: &revisions,
                    catalog_revision: i64::try_from(catalog_revision).unwrap_or(i64::MAX),
                    token_hash: &token_hash,
                    now,
                    expires_at,
                    target_root_id: request.target_root_id.as_deref(),
                    linked_job_id: None,
                    summary: &summary,
                },
            )
        })?;
        Ok(PreviewHandle {
            job_id,
            preview_token: token,
            created_at: now,
            expires_at,
            existing: false,
        })
    }

    /// Why a ready job can no longer be applied as planned.
    fn stale_reasons(&self, snapshot: &Snapshot) -> Vec<String> {
        if JobMode::parse(&snapshot.mode) == Some(JobMode::Undo) {
            return Vec::new();
        }
        let mut found = Vec::new();
        match self.settings() {
            Ok(stored) => {
                if crate::settings::management::settings_revision(&stored)
                    != snapshot.settings_revision
                {
                    found.push(
                        "Library Management settings changed after this preview was made."
                            .to_owned(),
                    );
                }
            }
            Err(error) => {
                tracing::warn!(%error, "management settings unreadable; preview treated as stale");
                found.push("Library Management settings could not be read.".to_owned());
            }
        }
        if self.setup.live_registry().policy_revision() != snapshot.policy_revision {
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
        let now = now_unix();
        let expired = snapshot.preview_expires_at.is_none_or(|at| at <= now);
        let ready = snapshot.phase == "ready" && snapshot.job.state.as_str() == "ready";
        let stale_reasons = if ready {
            self.stale_reasons(&snapshot)
        } else {
            Vec::new()
        };
        let undo_available_count =
            self.read(|conn| Ok(store::undo_available(conn, job_id, today_day())?))?;
        let (profile_id, profile_name) = snapshot.profile_label();
        let worker_stalled = snapshot.job.state.as_str() == "running"
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
            undo_available_count,
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
        self.read(|conn| Ok(store::items_page(conn, job_id, after, limit, filter)?))
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
        self.read(|conn| Ok(store::results(conn, job_id, after, limit)?))
    }

    /// Set a preview aside without applying it. Blocking.
    pub fn discard(
        &self,
        job_id: &str,
        expected_row_revision: i64,
    ) -> Result<JobDetail, ManagementError> {
        self.snapshot(job_id)?;
        let discarded = self.write(|tx| {
            Ok(store::discard(
                tx,
                job_id,
                expected_row_revision,
                now_unix(),
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
        let detail = self.detail(job_id)?;
        if !detail.ready_for_confirmation {
            return Err(stale(
                "This preview is no longer ready to apply. Make a new preview.",
            ));
        }
        let (token, token_hash) = self.token();
        let now = now_unix();
        self.read(|conn| Ok(store::rotate_token(conn, job_id, &token_hash, now)?))?;
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
        let (mode, phase) = match JobMode::parse(&snapshot.mode) {
            Some(JobMode::Undo) => (JobMode::Undo, "undoing"),
            _ => (JobMode::Apply, "applying"),
        };
        let now = now_unix();
        let job = self.write(|tx| {
            let current = ops::job(tx, job_id)?.ok_or(ManagementError::NotFound(
                "This management job does not exist.",
            ))?;
            if current.row_revision != request.expected_row_revision {
                return Err(stale(
                    "This preview changed since the page loaded. Reload the page and try again.",
                ));
            }
            let bundles = store::writable_bundles(tx, job_id)?;
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
                &request.idempotency_key,
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
            ops::job(tx, job_id)?.ok_or(ManagementError::NotFound(
                "This management job does not exist.",
            ))
        })?;
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
        if !matches!(source.mode.as_str(), "apply" | "automatic_apply")
            || source.phase != "complete"
        {
            return Err(stale("Only a finished apply can be undone."));
        }
        let bundles = self.read(|conn| Ok(store::applied_bundles(conn, job_id)?))?;
        if bundles.is_empty() {
            return Err(stale(
                "This operation wrote nothing, so there is nothing to undo.",
            ));
        }
        let now = now_unix();
        if let Some(existing) = self.read(|conn| Ok(store::job_for_key(conn, idempotency_key)?))? {
            let snapshot = self.snapshot(&existing)?;
            if snapshot.linked_job_id.as_deref() != Some(job_id)
                || snapshot.job.requested_by_user_id.as_deref() != Some(actor)
            {
                return Err(stale(
                    "This request key was already used for something else.",
                ));
            }
            let (token, token_hash) = self.token();
            self.read(|conn| Ok(store::rotate_token(conn, &existing, &token_hash, now)?))?;
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
        let undos = self.read(|conn| Ok(store::live_undos(conn, job_id, now)?))?;
        if undos
            .iter()
            .any(|(state, phase)| state == "succeeded" && phase == "complete")
        {
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
        self.write(|tx| {
            store::insert_job(
                tx,
                &NewJob {
                    id: &new_id,
                    requested_by: actor,
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
                },
            )
        })?;
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
        self.read(|conn| Ok(store::history(conn, &filter)?))
    }

    // -----------------------------------------------------------------
    // The worker.
    // -----------------------------------------------------------------

    /// Claim and run the next queued management job, never while a scan
    /// runs. True when a job was worked on. Blocking.
    pub fn run_next(&self, worker: &str) -> Result<bool, ManagementError> {
        if !self.setup.coordinator.current().is_empty() {
            return Ok(false);
        }
        let now = now_unix();
        let Some((job_id, phase)) = self.write(|tx| Ok(store::claim(tx, worker, now)?))? else {
            return Ok(false);
        };
        let outcome = match phase.as_str() {
            "planning" => self.plan(&job_id, worker),
            _ => self.apply_work(&job_id, worker),
        };
        if let Err(error) = outcome {
            tracing::error!(%error, job_id, "management job failed");
            self.write(|tx| {
                Ok(store::fail_job(
                    tx,
                    &job_id,
                    worker,
                    "MANAGEMENT_FAILED",
                    now_unix(),
                )?)
            })?;
        }
        Ok(true)
    }

    /// Honour a pause, a stop, or a server shutdown. True when the job
    /// left `running` (or went back to the queue) and the step must end.
    fn checkpoint(&self, job_id: &str, worker: &str) -> Result<bool, ManagementError> {
        if self
            .shutdown
            .as_ref()
            .is_some_and(|shutdown| *shutdown.borrow())
        {
            self.write(|tx| Ok(store::release(tx, job_id, worker, now_unix())?))?;
            return Ok(true);
        }
        self.write(|tx| Ok(ops::checkpoint(tx, job_id, worker, now_unix())?.is_some()))
    }

    fn heartbeat(&self, job_id: &str, worker: &str) -> Result<bool, ManagementError> {
        self.read(|conn| Ok(store::heartbeat(conn, job_id, worker, now_unix())?))
    }

    fn pinned(snapshot: &Snapshot) -> Result<PinnedProfile, ManagementError> {
        serde_json::from_str(&snapshot.profile_snapshot_json).map_err(|error| {
            ManagementError::Store(format!("stored profile snapshot unreadable: {error}"))
        })
    }

    /// Plan every album of a job, then mark it ready. Planning starts over
    /// when a job comes back to it.
    fn plan(&self, job_id: &str, worker: &str) -> Result<(), ManagementError> {
        let snapshot = self.snapshot(job_id)?;
        self.read(|conn| Ok(store::clear_items(conn, job_id)?))?;
        let summary = if JobMode::parse(&snapshot.mode) == Some(JobMode::Undo) {
            match self.plan_undo(&snapshot, worker)? {
                Some(summary) => summary,
                None => return Ok(()),
            }
        } else {
            match self.plan_preview(&snapshot, worker)? {
                Some(summary) => summary,
                None => return Ok(()),
            }
        };
        self.write(|tx| store::finish_planning(tx, job_id, worker, &summary, now_unix()))?;
        Ok(())
    }

    /// Store one album's items. False when the job was paused, stopped
    /// or taken away, so planning stops.
    fn keep_items(
        &self,
        snapshot: &Snapshot,
        worker: &str,
        items: &[PlannedItem],
    ) -> Result<bool, ManagementError> {
        let job_id = snapshot.job.id.as_str();
        self.write(|tx| store::insert_items(tx, job_id, snapshot, items, now_unix()))?;
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
        let selection: Selection = serde_json::from_str(&snapshot.selection_json)?;
        let scripts = CompiledScripts::new(&pinned);
        let sandbox = self.setup.publish_sandbox().map_err(|error| {
            ManagementError::Conflict(format!("No library folder can be written: {error}"))
        })?;
        let registry = self.setup.live_registry();
        let ctx = PlanContext {
            setup: &self.setup,
            pinned: &pinned,
            scripts: &scripts,
            sandbox: &sandbox,
            registry: &registry,
            target_root: snapshot.target_root_id.as_deref(),
            manage_unconfirmed: planner::manage_unconfirmed(&self.setup),
        };
        // Files that move take their whole album along, so an album is
        // never split between its old and new folder (as in v2).
        let organizes = pinned.profile.organization.rename_enabled
            || pinned.profile.organization.move_enabled
            || snapshot.target_root_id.is_some();
        let mut albums = self.read(|conn| selection::albums(conn, &selection))?;
        let picked: usize = albums
            .iter()
            .filter_map(|scope| scope.track_ids.as_ref().map(Vec::len))
            .sum();
        if organizes {
            for scope in &mut albums {
                scope.track_ids = None;
            }
        }
        let mut summary = Summary::default();
        let mut ordinal = 0;
        for (bundle_ordinal, scope) in albums.iter().enumerate() {
            let tracks = self.read(|conn| selection::tracks(conn, scope))?;
            if tracks.is_empty() {
                continue;
            }
            let items = planner::plan_album(&ctx, scope, &tracks, bundle_ordinal as i64, ordinal)?;
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
        let bundles = self.read(|conn| Ok(store::applied_bundles(conn, &source_id)?))?;
        // A partly successful earlier undo: only what it left is retried.
        let undone = self.read(|conn| Ok(store::undone_ordinals(conn, &source_id)?))?;
        let bundles: Vec<_> = bundles
            .into_iter()
            .filter(|(ordinal, _)| !undone.contains(ordinal))
            .collect();
        let mut summary = Summary::default();
        let mut ordinal = 0;
        for (bundle_ordinal, bundle_id) in bundles {
            let source_items =
                self.read(|conn| Ok(store::bundle_items(conn, &source_id, bundle_ordinal)?))?;
            let Some(first) = source_items.first().cloned() else {
                continue;
            };
            let items =
                self.plan_undo_bundle(&bundle_id, bundle_ordinal, ordinal, &first, &source_items)?;
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
        bundle_id: &str,
        bundle_ordinal: i64,
        first_ordinal: i64,
        first: &PlannedItem,
        source_items: &[PlannedItem],
    ) -> Result<Vec<PlannedItem>, ManagementError> {
        let album_id = first.album_id.clone();
        let override_of = |track_id: &str| self.override_revision(&album_id, track_id);
        let files = match self.setup.undo_files(bundle_id, &override_of) {
            Ok(files) => files,
            Err(crate::library::service::ServiceError::NotFound) => Default::default(),
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
            for (name, after) in &file.plan.managed_updates {
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
                .iter()
                .filter(|(_, values)| !values.is_empty())
                .map(|(name, values)| super::models::DesiredField {
                    name: name.clone(),
                    value: profile::json_value(values),
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
                crate::library::manage::UndoSkip::Plan(block) => reasons::undo_block(block),
                crate::library::manage::UndoSkip::Gone => ItemReason {
                    code: "FILE_CHANGED".to_owned(),
                    message:
                        "This file is no longer where the change left it, or no longer matched."
                            .to_owned(),
                    action: "Rescan the library, then make a new undo preview.".to_owned(),
                },
            });
            items.push(item);
        }
        if items.is_empty() {
            let mut item = template(first.track_id.as_deref().unwrap_or_default(), first_ordinal);
            item.block(reasons::undo_nothing());
            items.push(item);
        }
        if items.iter().any(|item| !item.eligibility.writes()) {
            for item in items.iter_mut().filter(|item| item.eligibility.writes()) {
                item.block(reasons::bundle_blocked());
            }
        }
        Ok(items)
    }

    /// The current revision of a file's saved tag overrides.
    fn override_revision(&self, album_id: &str, track_id: &str) -> u64 {
        self.setup
            .identify_store
            .with_connection(|conn| planner::overrides(conn, album_id, track_id))
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
        let undo = JobMode::parse(&snapshot.mode) == Some(JobMode::Undo);
        // Whether the job moves files: then an album must still be
        // exactly the files that were planned.
        let organizes = snapshot.target_root_id.is_some()
            || Self::pinned(&snapshot).is_ok_and(|pinned| {
                pinned.profile.organization.rename_enabled
                    || pinned.profile.organization.move_enabled
            });
        loop {
            if self.checkpoint(job_id, worker)? {
                return Ok(());
            }
            if !self.setup.coordinator.current().is_empty() {
                self.write(|tx| Ok(store::release(tx, job_id, worker, now_unix())?))?;
                return Ok(());
            }
            let now = now_unix();
            let Some(ordinal) = self.write(|tx| Ok(ops::claim_work(tx, job_id, now)?))? else {
                self.write(|tx| Ok(store::finish_apply(tx, job_id, worker, now_unix())?))?;
                return Ok(());
            };
            let Some((failure, result)) = self.apply_album(&snapshot, ordinal, undo, organizes)?
            else {
                // The album's earlier publish is not settled yet: wait for
                // recovery rather than publish it a second time.
                self.write(|tx| Ok(store::release(tx, job_id, worker, now_unix())?))?;
                return Ok(());
            };
            self.write(|tx| {
                Ok(store::finish_work(
                    tx,
                    job_id,
                    ordinal,
                    failure.as_ref(),
                    &result,
                    now_unix(),
                )?)
            })?;
        }
    }

    /// Whether the publisher committed `bundle_id`. An unreadable journal
    /// counts as not committed, and says so in the log.
    fn committed(&self, bundle_id: &str) -> bool {
        self.setup
            .held_bundles
            .committed(bundle_id)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, bundle_id, "publish journal unreadable");
                false
            })
    }

    /// Apply one album: its outcome and the result to record, or `None`
    /// when an earlier attempt's publish is still unsettled. The bundle id
    /// is recorded on the work row before anything is written, so an album
    /// the publisher committed is never lost to a crash or a late error:
    /// recovery runs first, and a committed album counts as written and
    /// can be undone. A saved bundle id is never replaced while its
    /// journals are unsettled.
    fn apply_album(
        &self,
        snapshot: &Snapshot,
        ordinal: i64,
        undo: bool,
        organizes: bool,
    ) -> Result<Option<(Option<ItemReason>, serde_json::Value)>, ManagementError> {
        let job_id = snapshot.job.id.as_str();
        let written = |bundle_id: &str, outcome: &str| serde_json::json!({ "bundle_id": bundle_id, "outcome": outcome });
        if let Some(earlier) = self.read(|conn| Ok(store::work_bundle(conn, job_id, ordinal)?))? {
            if let Err(error) = self.setup.settle_publishes() {
                tracing::warn!(%error, bundle_id = earlier, "publish recovery did not finish");
            }
            if self.committed(&earlier) {
                return Ok(Some((None, written(&earlier, "committed"))));
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
        self.read(|conn| Ok(store::set_work_bundle(conn, job_id, ordinal, &bundle_id)?))?;
        let items = self.read(|conn| Ok(store::bundle_items(conn, job_id, ordinal)?))?;
        Ok(Some(
            match self.publish_album(snapshot, &items, undo, organizes, &bundle_id) {
                Ok(applied) => (None, written(&applied.bundle_id, &applied.outcome)),
                Err(_) if self.committed(&bundle_id) => (None, written(&bundle_id, "committed")),
                Err(reason) => {
                    let result = serde_json::json!({ "bundle_id": bundle_id, "reason": reason });
                    (Some(reason), result)
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
    ) -> Result<(), ItemReason> {
        if !self.stale_reasons(snapshot).is_empty() {
            return Err(ItemReason {
                code: "PROFILE_CHANGED".to_owned(),
                message: "Library Management settings or the library folders changed while this run was applying."
                    .to_owned(),
                action: "Make a new preview for the albums that were not written yet.".to_owned(),
            });
        }
        let Some(first) = items.first() else {
            return Err(reasons::not_scheduled());
        };
        let manage_unconfirmed = planner::manage_unconfirmed(&self.setup);
        let state = self.read(|conn| {
            Ok((
                planner::match_gate(conn, &first.album_id, manage_unconfirmed)?,
                store::album_state(conn, &first.album_id)?,
            ))
        });
        let (gate, (revision, tracks)) = state.map_err(|error| {
            tracing::error!(%error, album = first.album_id, "album state unreadable; album left alone");
            reasons::write_failed()
        })?;
        if let Some(reason) = gate {
            return Err(reason);
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
            return Err(reasons::album_changed());
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
    ) -> Result<crate::library::manage::AppliedBundle, ItemReason> {
        if !undo {
            self.album_gate(snapshot, items, organizes)?;
        }
        let plans = planner::bundle_plans(items);
        if plans.is_empty() {
            return Err(reasons::not_scheduled());
        }
        let album_id = items
            .first()
            .map(|item| item.album_id.clone())
            .unwrap_or_default();
        let override_of = |track_id: &str| self.override_revision(&album_id, track_id);
        let pins = SealPins {
            profile: profile::seal_revision(&snapshot.profile_revision),
            naming: profile::seal_revision(&snapshot.naming_revision),
            settings: profile::seal_revision(&snapshot.settings_revision),
        };
        self.setup
            .publish_plans(bundle_id, plans, pins, &override_of)
            .map_err(|error| {
                let reason = reasons::from_publish(&error);
                if reason.code == "WRITE_FAILED" || reason.code == "BASELINE_UNAVAILABLE" {
                    tracing::error!(%error, job_id = snapshot.job.id, "management publish failed");
                } else {
                    tracing::info!(%error, job_id = snapshot.job.id, "management publish refused");
                }
                reason
            })
    }
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
