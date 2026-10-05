//! Free-music and drop-import as registered durable operations.
//!
//! v2 ran both as bare asyncio tasks (`FreeMusicService._tasks`,
//! `DropImportService._tasks`): a restart lost them without a trace. Here
//! each operation registers in the
//! durable job registry ([`FREE_MUSIC_JOB`], [`DROP_IMPORT_JOB`]) as
//! [`JobKind::Durable`](crate::db::JobKind), moves through
//! running/heartbeat/stopped states on the registry, persists its own
//! record in the [`OpStore`] (SQLite), and emits its plugin ticks beside
//! those writes.
//!
//! The drop path runs folder → quarantine → resolve: files land in staging,
//! bad sources quarantine with their reason, and each item resolves to the
//! library, back out of quarantine by hand, or to a faulted record. Only
//! bad sources quarantine; local faults fail open (v2 `file_processor.py`
//! non-quarantine reasons).

use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::acquire::db::AcquireDb;
use crate::acquire::requests::models::RequestKind;
use crate::acquire::requests::sqlite::RequestStore;
use crate::db::{DurableWorkWakeups, JobKind, JobState, WriteLane};

use super::seams::{
    CandidateSearch, Clock, DispatchKind, DispatchRequest, DownloadDispatch, DropVerify,
    LandedHandoff, LibraryOrganise, TickSink, VerifyVerdict,
};
use super::stores::{QuarantineEntry, QuarantineStore};

/// Registry name for the free-music durable operation.
pub const FREE_MUSIC_JOB: &str = "free-music";
/// Registry name for the drop-import durable operation.
pub const DROP_IMPORT_JOB: &str = "drop-import";

/// Candidate title-similarity floor. A candidate that bears no resemblance
/// to the requested album is a different record sharing an artist (v2
/// `_TITLE_MATCH_FLOOR = 0.60`, `free_music_service.py`).
pub const TITLE_MATCH_FLOOR: f64 = 0.60;

/// Progress writes hit the store at most this often; the transfer loop runs
/// far hotter (v2 `_PROGRESS_WRITE_INTERVAL = 1.0`).
pub const PROGRESS_WRITE_INTERVAL_SECS: f64 = 1.0;

/// Staging subfolder where the route streams uploads before a job exists.
/// Same filesystem as the job staging dirs, so adopting them is a rename
/// (v2 `DropImportService.incoming_dir`).
pub const INCOMING_DIR: &str = "_incoming";
/// Staging subfolder holding quarantined files.
pub const QUARANTINE_DIR: &str = "quarantine";

/// Lifecycle of one operation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpState {
    /// Registered, work not started.
    Queued,
    /// Work underway.
    Running,
    /// Finished well.
    Succeeded,
    /// Finished badly.
    Failed,
    /// Cancelled by its owner.
    Cancelled,
}

impl OpState {
    /// Stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse the stored form.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// One operation record: idempotency key, attempts, timestamps, outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRecord {
    /// Operation id.
    pub id: String,
    /// `free-music` or `drop-import`.
    pub kind: String,
    /// Idempotency key (user + request identity); restarts reuse it.
    pub key: String,
    /// Current lifecycle state.
    pub state: OpState,
    /// Attempts so far (dispatch + retries).
    pub attempts: u32,
    /// Unix seconds when the record was created.
    pub created_at: i64,
    /// Unix seconds of the last transition.
    pub updated_at: i64,
    /// Outcome detail: task id, landing job, or failure cause.
    pub detail: String,
}

/// What startup recovery did to unfinished operations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpRecovery {
    /// Free-music operations left running: their download task keeps
    /// going and [`settle_free_music`] lands or fails them.
    pub resumed: usize,
    /// Drop-import operations interrupted mid-run and marked failed. Their
    /// staged files stay where they are; nothing is moved twice.
    pub interrupted: usize,
}

/// Columns every operation read selects.
const OP_SELECT: &str = "SELECT id, kind, op_key, state, attempts, created_at, updated_at, detail \
     FROM acquire_operations";

type OpRow = (String, String, String, String, i64, i64, i64, String);

fn op_from_row(row: OpRow) -> Result<OpRecord, String> {
    let (id, kind, key, state, attempts, created_at, updated_at, detail) = row;
    let state = OpState::parse(&state).ok_or_else(|| format!("unknown operation state {state}"))?;
    Ok(OpRecord {
        id,
        kind,
        key,
        state,
        attempts: u32::try_from(attempts).unwrap_or(0),
        created_at,
        updated_at,
        detail,
    })
}

fn op_from_rusqlite(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpRow> {
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
}

/// Durable operation records over `acquire_operations`.
#[derive(Clone)]
pub struct OpStore {
    db: AcquireDb,
}

impl OpStore {
    /// Records over one database.
    pub fn new(db: AcquireDb) -> Self {
        Self { db }
    }

    /// Register a queued operation under its idempotency key. When the key
    /// already exists, the existing record is returned untouched: a retried
    /// request reuses its operation instead of queuing a duplicate. The
    /// lookup and the insert share one write.
    pub async fn register(&self, kind: &str, key: &str, now: i64) -> Result<OpRecord, String> {
        let (kind, key) = (kind.to_owned(), key.to_owned());
        let id = format!("op-{}", uuid::Uuid::new_v4().simple());
        let row = self
            .db
            .write("operations.register", move |tx| {
                tx.execute(
                    "INSERT INTO acquire_operations (id, kind, op_key, state, attempts, \
                     created_at, updated_at, detail) VALUES (?1, ?2, ?3, 'queued', 0, ?4, ?4, '') \
                     ON CONFLICT (kind, op_key) DO NOTHING",
                    params![id, kind, key, now],
                )?;
                let sql = format!("{OP_SELECT} WHERE kind = ?1 AND op_key = ?2");
                Ok(tx.query_row(&sql, params![kind, key], op_from_rusqlite)?)
            })
            .await
            .map_err(|error| error.to_string())?;
        op_from_row(row)
    }

    /// Transition one operation, stamping the time and detail.
    pub async fn transition(
        &self,
        id: &str,
        state: OpState,
        now: i64,
        detail: &str,
    ) -> Result<(), String> {
        let (id, detail) = (id.to_owned(), detail.to_owned());
        self.db
            .write("operations.transition", move |tx| {
                tx.execute(
                    "UPDATE acquire_operations SET state = ?2, updated_at = ?3, detail = ?4 \
                     WHERE id = ?1",
                    params![id, state.as_str(), now, detail],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// Count one attempt on an operation.
    pub async fn count_attempt(&self, id: &str) -> Result<(), String> {
        let id = id.to_owned();
        self.db
            .write("operations.attempt", move |tx| {
                tx.execute(
                    "UPDATE acquire_operations SET attempts = attempts + 1 WHERE id = ?1",
                    params![id],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| error.to_string())
    }

    /// One record, if present.
    pub async fn get(&self, id: &str) -> Result<Option<OpRecord>, String> {
        let row: Option<OpRow> = sqlx::query_as(&format!("{OP_SELECT} WHERE id = ?1"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
            .map_err(|error| error.to_string())?;
        row.map(op_from_row).transpose()
    }

    /// Records not yet terminal, oldest first.
    pub async fn unfinished(&self) -> Result<Vec<OpRecord>, String> {
        let rows: Vec<OpRow> = sqlx::query_as(&format!(
            "{OP_SELECT} WHERE state IN ('queued', 'running') ORDER BY created_at, id"
        ))
        .fetch_all(self.db.pool())
        .await
        .map_err(|error| error.to_string())?;
        rows.into_iter().map(op_from_row).collect()
    }

    /// Startup recovery: drop-import runs interrupted by the restart fail
    /// (their in-memory item list is gone, and re-running would move files
    /// twice); free-music runs stay open for their settle.
    pub async fn recover(&self, now: i64) -> Result<OpRecovery, String> {
        let mut report = OpRecovery::default();
        for op in self.unfinished().await? {
            if op.kind == DROP_IMPORT_JOB && op.state == OpState::Running {
                self.transition(&op.id, OpState::Failed, now, "interrupted by restart")
                    .await?;
                report.interrupted += 1;
            } else {
                report.resumed += 1;
            }
        }
        Ok(report)
    }
}

/// Register both flow operations as durable jobs. Idempotent: re-registering
/// after a restart refreshes the rows without disturbing liveness.
pub async fn register_durable_ops(
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
) -> Result<(), String> {
    wakeups
        .register_job(lane, FREE_MUSIC_JOB, JobKind::Durable, None)
        .await
        .map_err(|error| error.to_string())?;
    wakeups
        .register_job(lane, DROP_IMPORT_JOB, JobKind::Durable, None)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Move a job to running and beat its heart once.
async fn mark_running(wakeups: &DurableWorkWakeups, lane: &WriteLane, job: &str) {
    if let Err(error) = wakeups.set_job_state(lane, job, JobState::Running).await {
        tracing::warn!(job, %error, "operation running state write failed");
        return;
    }
    heartbeat(wakeups, lane, job).await;
}

/// Beat one job's heart, logging a failed write.
async fn heartbeat(wakeups: &DurableWorkWakeups, lane: &WriteLane, job: &str) {
    if let Err(error) = wakeups.heartbeat(lane, job).await {
        tracing::warn!(job, %error, "operation heartbeat failed");
    }
}

/// Finish a job's liveness: stopped on success, failed otherwise.
async fn mark_finished(wakeups: &DurableWorkWakeups, lane: &WriteLane, job: &str, ok: bool) {
    let state = if ok {
        JobState::Stopped
    } else {
        JobState::Failed
    };
    if let Err(error) = wakeups.set_job_state(lane, job, state).await {
        tracing::warn!(job, %error, "operation finish state write failed");
    }
    heartbeat(wakeups, lane, job).await;
}

/// True when a progress write is due: at most one per
/// [`PROGRESS_WRITE_INTERVAL_SECS`] (v2 `_PROGRESS_WRITE_INTERVAL`).
pub fn progress_write_due(last_write_at: f64, now: f64) -> bool {
    now - last_write_at >= PROGRESS_WRITE_INTERVAL_SECS
}

/// Token-containment similarity between a requested title and a candidate
/// title: the fraction of requested tokens appearing in the candidate. The
/// small port of v2 `title_containment_score` behind the 0.60 floor.
pub fn title_containment(requested: &str, candidate: &str) -> f64 {
    let wanted: Vec<String> = tokens(requested);
    if wanted.is_empty() {
        return 0.0;
    }
    let have = tokens(candidate);
    let hits = wanted.iter().filter(|token| have.contains(token)).count();
    hits as f64 / wanted.len() as f64
}

/// Lowercase alphanumeric tokens of a title.
fn tokens(title: &str) -> Vec<String> {
    title
        .to_lowercase()
        .split(|char: char| !char.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

/// One free-music request: an album or track served lawfully from the
/// Internet Archive (v2 `FreeMusicService`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreeMusicRequest {
    /// Requesting user id.
    pub user_id: String,
    /// Album or track.
    pub kind: DispatchKind,
    /// Release-group MBID (album) or recording MBID (track).
    pub mbid: String,
    /// Artist name.
    pub artist: String,
    /// Album or track title.
    pub title: String,
}

/// Free-music dependencies: Archive search, download dispatch, the
/// drop-import handoff, ticks, and time.
pub struct FreeMusicDeps {
    /// Candidate search (Archive-backed in production).
    pub search: std::sync::Arc<dyn CandidateSearch>,
    /// Download dispatch.
    pub downloads: std::sync::Arc<dyn DownloadDispatch>,
    /// Landing handoff into drop-import.
    pub handoff: std::sync::Arc<dyn LandedHandoff>,
    /// Durable ticks.
    pub ticks: std::sync::Arc<dyn TickSink>,
    /// Clock.
    pub clock: std::sync::Arc<dyn Clock>,
    /// Free-music enabled toggle (v2 `is_ready` gate).
    pub enabled: bool,
}

/// Outcome of one free-music run step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreeMusicOutcome {
    /// The download settled and landed as a drop-import job.
    Landed {
        /// Operation id.
        op_id: String,
        /// Drop-import job id.
        job_id: String,
    },
    /// The download is still in flight; call [`settle_free_music`] later.
    Pending {
        /// Operation id.
        op_id: String,
        /// Download task id.
        task_id: String,
    },
}

/// Request an album or track through free music: register the durable
/// operation, pick the first candidate clearing the title floor, dispatch
/// the download, and land it when already settled. Mirrors v2
/// `request_album` / `request_track` plus the `_run` candidate ladder.
pub async fn run_free_music(
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
    ops: &OpStore,
    deps: &FreeMusicDeps,
    request: &FreeMusicRequest,
) -> Result<FreeMusicOutcome, String> {
    let now = deps.clock.now_unix();
    let key = format!(
        "{}:{}:{}",
        request.user_id,
        request.mbid,
        kind_key(request.kind)
    );
    let op = ops.register("free-music", &key, now).await?;
    if op.state == OpState::Succeeded {
        return Ok(FreeMusicOutcome::Landed {
            op_id: op.id.clone(),
            job_id: op.detail.clone(),
        });
    }
    mark_running(wakeups, lane, FREE_MUSIC_JOB).await;
    ops.transition(&op.id, OpState::Running, now, "").await?;
    ops.count_attempt(&op.id).await?;

    if !deps.enabled {
        return fail_free_music(wakeups, lane, ops, deps, &op, "free music disabled").await;
    }
    let candidates = deps
        .search
        .search_album(&request.artist, &request.title)
        .await
        .map_err(|cause| format!("candidate search failed: {cause}"))?;
    let Some(pick) = candidates
        .iter()
        .find(|candidate| title_containment(&request.title, &candidate.title) >= TITLE_MATCH_FLOOR)
    else {
        return fail_free_music(wakeups, lane, ops, deps, &op, "no matching candidate").await;
    };
    let dispatch = DispatchRequest {
        user_id: request.user_id.clone(),
        kind: request.kind,
        mbid: request.mbid.clone(),
        artist: request.artist.clone(),
        title: pick.title.clone(),
        origin: "free-music".to_owned(),
        idempotency_key: None,
    };
    let task_id = deps.downloads.dispatch(&dispatch).await?;
    heartbeat(wakeups, lane, FREE_MUSIC_JOB).await;

    match deps.downloads.task_status(&task_id).await?.as_deref() {
        Some("completed") => {
            let files = vec![format!("{} - {}.flac", request.artist, pick.title)];
            land_free_music(wakeups, lane, ops, deps, &op, request, &task_id, &files).await
        }
        Some("failed" | "cancelled") => {
            fail_free_music(wakeups, lane, ops, deps, &op, "download failed").await
        }
        _ => {
            ops.transition(&op.id, OpState::Running, now, &task_id)
                .await?;
            Ok(FreeMusicOutcome::Pending {
                op_id: op.id.clone(),
                task_id,
            })
        }
    }
}

/// Settle a pending free-music operation: re-read the task status and land
/// or fail the operation. Safe to call repeatedly; terminal operations
/// answer from their record.
pub async fn settle_free_music(
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
    ops: &OpStore,
    deps: &FreeMusicDeps,
    op_id: &str,
    request: &FreeMusicRequest,
) -> Result<FreeMusicOutcome, String> {
    let Some(op) = ops.get(op_id).await? else {
        return Err("unknown free-music operation".to_owned());
    };
    match op.state {
        OpState::Succeeded => Ok(FreeMusicOutcome::Landed {
            op_id: op.id.clone(),
            job_id: op.detail.clone(),
        }),
        OpState::Failed | OpState::Cancelled => Err(op.detail.clone()),
        OpState::Queued | OpState::Running => {
            let task_id = op.detail.clone();
            match deps.downloads.task_status(&task_id).await?.as_deref() {
                Some("completed") => {
                    let files = vec![format!("{} - {}.flac", request.artist, request.title)];
                    land_free_music(wakeups, lane, ops, deps, &op, request, &task_id, &files).await
                }
                Some("failed" | "cancelled") => {
                    fail_free_music(wakeups, lane, ops, deps, &op, "download failed").await
                }
                _ => Ok(FreeMusicOutcome::Pending {
                    op_id: op.id.clone(),
                    task_id,
                }),
            }
        }
    }
}

/// Land a settled free-music download: hand the files to drop-import,
/// record the win, tick `request_fulfilled` beside it.
#[allow(clippy::too_many_arguments)]
async fn land_free_music(
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
    ops: &OpStore,
    deps: &FreeMusicDeps,
    op: &OpRecord,
    request: &FreeMusicRequest,
    task_id: &str,
    files: &[String],
) -> Result<FreeMusicOutcome, String> {
    let now = deps.clock.now_unix();
    let job_id = deps.handoff.land(&op.id, &request.user_id, files).await?;
    ops.transition(&op.id, OpState::Succeeded, now, &job_id)
        .await?;
    deps.ticks.emit_about(
        "request_fulfilled",
        &request.mbid,
        &format!(
            "free-music {} landed as {job_id} (task {task_id})",
            request.mbid
        ),
        now,
    );
    mark_finished(wakeups, lane, FREE_MUSIC_JOB, true).await;
    Ok(FreeMusicOutcome::Landed {
        op_id: op.id.clone(),
        job_id,
    })
}

/// Fail a free-music operation: record the cause, tick it, fail the job row.
async fn fail_free_music(
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
    ops: &OpStore,
    deps: &FreeMusicDeps,
    op: &OpRecord,
    cause: &str,
) -> Result<FreeMusicOutcome, String> {
    let now = deps.clock.now_unix();
    ops.transition(&op.id, OpState::Failed, now, cause).await?;
    deps.ticks.emit(
        "free_music.failed",
        &format!("free-music {} failed: {cause}", op.key),
        now,
    );
    mark_finished(wakeups, lane, FREE_MUSIC_JOB, false).await;
    Err(cause.to_owned())
}

/// Cancel a free-music operation. Terminal records stay as they are.
pub async fn cancel_free_music(
    ops: &OpStore,
    clock: &dyn Clock,
    op_id: &str,
) -> Result<bool, String> {
    let Some(op) = ops.get(op_id).await? else {
        return Ok(false);
    };
    if matches!(
        op.state,
        OpState::Succeeded | OpState::Failed | OpState::Cancelled
    ) {
        return Ok(false);
    }
    ops.transition(&op.id, OpState::Cancelled, clock.now_unix(), "cancelled")
        .await?;
    Ok(true)
}

/// Idempotency-key fragment for the request kind.
fn kind_key(kind: DispatchKind) -> &'static str {
    match kind {
        DispatchKind::Album => "album",
        DispatchKind::Track => "track",
    }
}

/// One staged drop file and its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropItem {
    /// Original file name.
    pub name: String,
    /// Staged path.
    pub staged_path: String,
    /// Outcome once processed.
    pub outcome: Option<DropItemOutcome>,
}

/// Outcome of one processed drop item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropItemOutcome {
    /// Organised into the library at this path.
    Resolved(String),
    /// Quarantined with this reason.
    Quarantined(String),
    /// Failed without quarantine (local fault).
    Faulted(String),
    /// Hand-discarded from quarantine.
    Discarded,
}

/// One drop-import job: staged uploads moving toward resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropJob {
    /// Job id.
    pub id: String,
    /// Uploading user id.
    pub user_id: String,
    /// Display name (`first +N more`, v2 `create_job`).
    pub upload_name: String,
    /// Staging directory.
    pub staging_dir: String,
    /// Staged items.
    pub items: Vec<DropItem>,
    /// Unix seconds when the job was created.
    pub created_at: i64,
}

/// Drop-import dependencies: verify, organise, quarantine, requests, ticks.
pub struct DropImportDeps {
    /// File verify seam.
    pub verify: std::sync::Arc<dyn DropVerify>,
    /// Library organise seam.
    pub organise: std::sync::Arc<dyn LibraryOrganise>,
    /// Quarantine registry.
    pub quarantine: QuarantineStore,
    /// Request ledger for resolve marks.
    pub ledger: RequestStore,
    /// Durable ticks.
    pub ticks: std::sync::Arc<dyn TickSink>,
    /// Clock.
    pub clock: std::sync::Arc<dyn Clock>,
}

/// Scan a drop folder for importable files, sorted by name. Folders pass
/// through untouched; only files import.
pub fn scan_drop_folder(drop_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries =
        std::fs::read_dir(drop_dir).map_err(|error| format!("cannot scan drop folder: {error}"))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read drop entry: {error}"))?;
        let path = entry.path();
        let is_file = entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false);
        if is_file {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Where the route streams uploads before a job exists.
pub fn incoming_dir(staging_root: &Path) -> PathBuf {
    staging_root.join(INCOMING_DIR)
}

/// Where quarantined files wait for a resolve decision.
pub fn quarantine_dir(staging_root: &Path) -> PathBuf {
    staging_root.join(QUARANTINE_DIR)
}

/// Create a drop-import job: adopt uploaded files into a staging dir as
/// `{index:03d}_{safe name}` (v2 `create_job`), register the durable
/// operation, and return the job. Refuses empty uploads (v2
/// `ValidationError("No files were uploaded")`).
#[allow(clippy::too_many_arguments)]
pub async fn create_drop_job(
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
    ops: &OpStore,
    staging_root: &Path,
    job_seq: u64,
    user_id: &str,
    uploads: &[(String, PathBuf)],
    now: i64,
) -> Result<DropJob, String> {
    if uploads.is_empty() {
        return Err("No files were uploaded".to_owned());
    }
    let job_id = format!("drop-{job_seq}");
    let staging_dir = staging_root.join(&job_id);
    tokio::fs::create_dir_all(&staging_dir)
        .await
        .map_err(|error| format!("cannot stage drop job: {error}"))?;
    let mut items = Vec::with_capacity(uploads.len());
    for (index, (name, tmp_path)) in uploads.iter().enumerate() {
        let staged = staging_dir.join(format!("{index:03}_{}", safe_component(name)));
        tokio::fs::rename(tmp_path, &staged)
            .await
            .map_err(|error| format!("cannot stage {name}: {error}"))?;
        items.push(DropItem {
            name: name.clone(),
            staged_path: staged.to_string_lossy().into_owned(),
            outcome: None,
        });
    }
    let upload_name = if uploads.len() == 1 {
        uploads[0].0.clone()
    } else {
        format!("{} +{} more", uploads[0].0, uploads.len() - 1)
    };
    ops.register("drop-import", &job_id, now).await?;
    mark_running(wakeups, lane, DROP_IMPORT_JOB).await;
    Ok(DropJob {
        id: job_id,
        user_id: user_id.to_owned(),
        upload_name,
        staging_dir: staging_dir.to_string_lossy().into_owned(),
        items,
        created_at: now,
    })
}

/// Process every item in a drop job: good files resolve to the library,
/// bad sources quarantine with their reason, local faults fail open.
/// Heartbeats once per item; one bad item never stops its siblings.
pub async fn process_drop_job(
    wakeups: &DurableWorkWakeups,
    lane: &WriteLane,
    ops: &OpStore,
    deps: &DropImportDeps,
    staging_root: &Path,
    job: &mut DropJob,
    rg_mbid: Option<&str>,
) -> Result<(), String> {
    let now = deps.clock.now_unix();
    let op_id = ops.register("drop-import", &job.id, now).await?.id;
    ops.transition(&op_id, OpState::Running, now, "").await?;
    tokio::fs::create_dir_all(quarantine_dir(staging_root))
        .await
        .map_err(|error| format!("cannot open quarantine: {error}"))?;

    for item in &mut job.items {
        if item.outcome.is_some() {
            continue;
        }
        let at = deps.clock.now_unix();
        match deps.verify.verify(&item.name) {
            VerifyVerdict::Ok => {
                let final_path = deps.organise.organise(&job.id, &item.staged_path)?;
                item.outcome = Some(DropItemOutcome::Resolved(final_path.clone()));
                if let Some(rg) = rg_mbid {
                    mark_imported(&deps.ledger, rg, at).await;
                }
                deps.ticks.emit_about(
                    "drop_import.resolved",
                    rg_mbid.unwrap_or_default(),
                    &format!("{} resolved to {final_path}", item.name),
                    at,
                );
            }
            VerifyVerdict::BadSource(reason) => {
                // Job-prefixed so a second job's same-name file never lands
                // on top of this one.
                let held = quarantine_dir(staging_root).join(format!(
                    "{}_{}",
                    job.id,
                    safe_component(&item.name)
                ));
                if tokio::fs::try_exists(&held).await.unwrap_or(true) {
                    return Err(format!(
                        "quarantine destination for {} is occupied",
                        item.name
                    ));
                }
                tokio::fs::rename(&item.staged_path, &held)
                    .await
                    .map_err(|error| format!("cannot quarantine {}: {error}", item.name))?;
                item.staged_path = held.to_string_lossy().into_owned();
                item.outcome = Some(DropItemOutcome::Quarantined(reason.clone()));
                deps.quarantine
                    .quarantine(QuarantineEntry {
                        key: format!("{}:{}", job.id, item.name),
                        album_key: rg_mbid.map(str::to_owned),
                        reason: reason.clone(),
                        at,
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                deps.ticks.emit(
                    "drop_import.quarantined",
                    &format!("{} quarantined: {reason}", item.name),
                    at,
                );
            }
            VerifyVerdict::LocalFault(reason) => {
                item.outcome = Some(DropItemOutcome::Faulted(reason.clone()));
                deps.ticks.emit(
                    "drop_import.faulted",
                    &format!("{} faulted (not quarantined): {reason}", item.name),
                    at,
                );
            }
        }
        heartbeat(wakeups, lane, DROP_IMPORT_JOB).await;
    }
    let done = deps.clock.now_unix();
    ops.transition(
        &op_id,
        OpState::Succeeded,
        done,
        &format!("{} items", job.items.len()),
    )
    .await?;
    mark_finished(wakeups, lane, DROP_IMPORT_JOB, true).await;
    Ok(())
}

/// How a hand resolve treats a quarantined item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveDecision {
    /// The source was fine after all: organise it into the library.
    Match,
    /// Drop it: delete the file and its quarantine entry.
    Discard,
}

/// Resolve one quarantined item by hand (v2 `match_item` / `discard_item`).
/// Matching clears the quarantine entry, organises the file, and ticks the
/// resolve; discarding deletes both. Answers false when the item is not
/// quarantined.
pub async fn resolve_quarantined_item(
    deps: &DropImportDeps,
    job: &mut DropJob,
    file_name: &str,
    decision: ResolveDecision,
    rg_mbid: Option<&str>,
) -> Result<bool, String> {
    let Some(item) = job.items.iter_mut().find(|item| item.name == file_name) else {
        return Ok(false);
    };
    let DropItemOutcome::Quarantined(_) =
        item.outcome.clone().unwrap_or(DropItemOutcome::Discarded)
    else {
        return Ok(false);
    };
    let key = format!("{}:{}", job.id, item.name);
    let at = deps.clock.now_unix();
    match decision {
        ResolveDecision::Match => {
            let final_path = deps.organise.organise(&job.id, &item.staged_path)?;
            deps.quarantine
                .clear(&key)
                .await
                .map_err(|error| error.to_string())?;
            item.outcome = Some(DropItemOutcome::Resolved(final_path.clone()));
            if let Some(rg) = rg_mbid {
                mark_imported(&deps.ledger, rg, at).await;
            }
            deps.ticks.emit(
                "drop_import.resolved",
                &format!("{} matched by hand to {final_path}", item.name),
                at,
            );
        }
        ResolveDecision::Discard => {
            tokio::fs::remove_file(&item.staged_path)
                .await
                .map_err(|error| format!("cannot discard {}: {error}", item.name))?;
            deps.quarantine
                .clear(&key)
                .await
                .map_err(|error| error.to_string())?;
            item.outcome = Some(DropItemOutcome::Discarded);
            deps.ticks.emit(
                "drop_import.discarded",
                &format!("{} discarded by hand", item.name),
                at,
            );
        }
    }
    Ok(true)
}

/// Mark one album request imported after a drop resolved it. A failed
/// write is logged; the status sync reconciles the row later.
async fn mark_imported(ledger: &RequestStore, rg_mbid: &str, at: i64) {
    let at = u64::try_from(at).unwrap_or(0);
    if let Err(error) = ledger
        .update_status(RequestKind::Album, rg_mbid, "imported", Some(at), None)
        .await
    {
        tracing::warn!(
            rg_mbid,
            ?error,
            "drop import could not mark the request imported"
        );
    }
}

/// Strip path separators and control characters from a staged name.
fn safe_component(name: &str) -> String {
    name.chars()
        .map(|char| {
            if char.is_control() || char == '/' || char == '\\' {
                '_'
            } else {
                char
            }
        })
        .collect()
}
