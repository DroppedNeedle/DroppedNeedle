//! The repairs service: what the HTTP handlers and the worker loop call.
//!
//! Reads and writes go through the shared database's blocking calls, so
//! they run off the async workers (handlers use `spawn_blocking`; the loop
//! drives its tick on a blocking thread). Requests write on the foreground
//! lane, the worker on the background lane.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Transaction;

use super::apply::apply_album;
use super::classify;
use super::hygiene;
use super::models::{
    EditionSummary, FindingCode, FindingPage, HYGIENE_VERSION, MANAGEMENT_MAPPING_VERSION,
    NewRepair, Phase, PreparationEstimate, Purpose, RepairEstimate, Summary, Verdict,
};
use super::plays;
use super::reasons;
use super::store::{self, RepairSnapshot, Unreadable, Watermark};
use crate::clock::LibraryClock as _;
use crate::identify::models::{IdentifyKind, LocalAlbumFacts, RecallResult};
use crate::identify::service::IdentifyService;
use crate::identify::sqlite::{SqliteIdentifyStore, album_input_revision};
use crate::jobs::{self, OperationState, WorkState};
use crate::operations::models::{
    Control, OperationDetail, OperationError, OperationJob, OperationKind,
    ReidentificationCandidate,
};
use crate::operations::reidentify::evaluation;
use crate::operations::service::{Operations, job_detail};
use crate::scan::watcher::WorkWakeups;
use crate::wiring::ScanCoordinator;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::CancellationToken;
use droppedneedle_kernel::jobs::schedule::SplitMix64;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;
use droppedneedle_providers::RequestPriority;

/// Wait before a job the database was too busy for goes on, plus up to
/// as much again.
const BUSY_RETRY: Duration = Duration::from_secs(5);
/// Wait before an album MusicBrainz could not answer for is tried again.
const PROVIDER_RETRY_SECS: f64 = 120.0;
/// Largest history page.
const HISTORY_MAX: usize = 50;
/// Largest findings page.
const FINDINGS_MAX: usize = 200;

/// A repair job with its report summary.
#[derive(Debug, Clone)]
pub struct RepairDetail {
    pub detail: OperationDetail,
    pub summary: Option<Summary>,
}

/// What a check made of one album.
enum Checked {
    Found(Box<Verdict>, Option<i64>),
    /// MusicBrainz did not answer; try the album again later.
    Deferred,
}

/// Identity repairs, identity preparations and catalog cleanup: the
/// requests the HTTP handlers make.
#[derive(Clone)]
pub struct Repairs {
    db: Db,
    ops: Operations,
    ids: Arc<dyn IdGenerator>,
    wakeups: WorkWakeups,
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
}

impl Repairs {
    pub fn new(
        db: Db,
        ops: Operations,
        ids: Arc<dyn IdGenerator>,
        wakeups: WorkWakeups,
        clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        Self {
            db,
            ops,
            ids,
            wakeups,
            clock,
        }
    }

    /// A repair job's report summary for the generic operation view, or
    /// `None` when `job` is not a repair. Blocking.
    pub fn summary_of(&self, job: &OperationJob) -> Result<Option<Summary>, OperationError> {
        if OperationKind::parse(&job.kind) != Some(OperationKind::Repair) {
            return Ok(None);
        }
        self.db.read_blocking("repairs.summary", |conn| {
            let snap = store::snapshot(conn, &job.id)?.transpose()?;
            Ok::<_, OperationError>(snap.and_then(|snap| snap.summary))
        })
    }

    /// A scan run's redacted diagnostic report: its file name and bytes.
    /// The outer error is the database's, the inner one the report's.
    /// Blocking.
    pub fn scan_diagnostics(
        &self,
        run_id: &str,
    ) -> Result<Result<(String, Vec<u8>), OperationError>, DbError> {
        self.db.read_blocking("scan_diagnostics", |conn| {
            Ok::<_, DbError>(super::diagnostics::export(conn, run_id))
        })
    }

    // -----------------------------------------------------------------------
    // Requests. All blocking.
    // -----------------------------------------------------------------------

    /// Start an identity repair or preparation, or find the one this
    /// idempotency key already started.
    pub fn create(
        &self,
        purpose: Purpose,
        user_id: &str,
        idempotency_key: &str,
        root_ids: Vec<String>,
        source_matcher_version: Option<String>,
        target_matcher_version: String,
    ) -> Result<OperationJob, OperationError> {
        let new = NewRepair {
            job_id: self.ids.new_id(),
            purpose,
            requested_by_user_id: Some(user_id.to_owned()),
            idempotency_key: idempotency_key.to_owned(),
            root_ids: dedupe(root_ids),
            album_ids: Vec::new(),
            scan_run_id: None,
            source_matcher_version,
            target_matcher_version,
        };
        let job = self
            .db
            .write_blocking(Lane::Foreground, "repairs.create", |tx| {
                store::create(tx, &new, self.clock.now_secs())
            })?;
        jobs::wake(&self.wakeups, OperationKind::Repair);
        Ok(job)
    }

    pub fn estimate_repair(&self, root_ids: Vec<String>) -> Result<RepairEstimate, OperationError> {
        let roots = dedupe(root_ids);
        self.db.read_blocking("repairs.estimate_repair", |conn| {
            Ok(store::estimate_repair(conn, &roots)?)
        })
    }

    pub fn estimate_preparation(
        &self,
        root_ids: Vec<String>,
    ) -> Result<PreparationEstimate, OperationError> {
        let roots = dedupe(root_ids);
        self.db
            .read_blocking("repairs.estimate_preparation", |conn| {
                Ok(store::estimate_preparation(conn, &roots)?)
            })
    }

    /// One repair job with its summary; not found when it is not a job of
    /// `purpose`.
    pub fn get(&self, job_id: &str, purpose: Purpose) -> Result<RepairDetail, OperationError> {
        // The job and its summary come from one read: read apart, a commit
        // between them shows a ready job with no summary.
        self.db.read_blocking("repairs.get", |conn| {
            let (_, snap) = store::repair_job(conn, job_id, purpose)?;
            Ok(RepairDetail {
                detail: job_detail(conn, job_id)?,
                summary: snap.summary,
            })
        })
    }

    /// A page of a purpose's jobs, newest first, and the next cursor.
    pub fn history(
        &self,
        purpose: Purpose,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<RepairDetail>, Option<String>), OperationError> {
        if !(1..=HISTORY_MAX).contains(&limit) {
            return Err(OperationError::Invalid(reasons::PAGE_SIZE_INVALID));
        }
        let before = cursor.map(parse_cursor).transpose()?;
        // One read: the page, its snapshots, then each job's detail.
        let (items, more) = self.db.read_blocking("repairs.history", |conn| {
            let page = store::history(conn, purpose, limit + 1, before)?;
            let more = page.len() > limit;
            let items = page
                .into_iter()
                .take(limit)
                .map(|(id, snap)| {
                    Ok(RepairDetail {
                        detail: job_detail(conn, &id)?,
                        summary: snap?.summary,
                    })
                })
                .collect::<Result<Vec<_>, OperationError>>()?;
            Ok::<_, OperationError>((items, more))
        })?;
        let next = more
            .then(|| items.last())
            .flatten()
            .map(|last| format!("{}:{}", last.detail.job.created_at, last.detail.job.id));
        Ok((items, next))
    }

    /// One page of a job's findings.
    pub fn findings(
        &self,
        job_id: &str,
        purpose: Purpose,
        limit: usize,
        cursor: Option<&str>,
        category: Option<&str>,
    ) -> Result<FindingPage, OperationError> {
        if !(1..=FINDINGS_MAX).contains(&limit) {
            return Err(OperationError::Invalid(reasons::PAGE_SIZE_INVALID));
        }
        let codes = category
            .map(|category| {
                store::category_codes(purpose, category)
                    .ok_or(OperationError::Invalid(reasons::CATEGORY_INVALID))
            })
            .transpose()?;
        let cursor = cursor.map(parse_cursor).transpose()?;
        self.db.read_blocking("repairs.findings", |conn| {
            let (_, snap) = store::repair_job(conn, job_id, purpose)?;
            let mut page = store::findings_page(conn, job_id, purpose, codes, cursor, limit)?;
            page.refresh_required = purpose == Purpose::ManagementReadiness
                && snap.target_matcher_version != MANAGEMENT_MAPPING_VERSION;
            Ok(page)
        })
    }

    /// Apply a ready report's safe changes in the background.
    pub fn begin_apply(
        &self,
        job_id: &str,
        purpose: Purpose,
        expected_row_revision: i64,
        confirmation: bool,
    ) -> Result<OperationJob, OperationError> {
        if !confirmation {
            return Err(OperationError::Invalid(reasons::CONFIRMATION_REQUIRED));
        }
        let job = self
            .db
            .write_blocking(Lane::Foreground, "repairs.begin_apply", |tx| {
                store::begin_apply(
                    tx,
                    job_id,
                    purpose,
                    expected_row_revision,
                    self.clock.now_secs(),
                )
            })?;
        jobs::wake(&self.wakeups, OperationKind::Repair);
        Ok(job)
    }

    /// Dismiss a ready preparation report.
    pub fn discard(
        &self,
        job_id: &str,
        expected_row_revision: i64,
    ) -> Result<OperationJob, OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "repairs.discard", |tx| {
                store::discard(tx, job_id, expected_row_revision, self.clock.now_secs())
            })
    }

    /// Pause, resume, or stop a repair job.
    pub fn control(
        &self,
        job_id: &str,
        purpose: Purpose,
        control: Control,
        expected_row_revision: i64,
        idempotency_key: Option<&str>,
    ) -> Result<OperationJob, OperationError> {
        self.db.read_blocking("repairs.control", |conn| {
            store::repair_job(conn, job_id, purpose).map(|_| ())
        })?;
        self.ops
            .control(job_id, control, expected_row_revision, idempotency_key)
    }
}

/// The repairs worker the loop runs: it claims and works repair jobs and
/// queues the cleanups, re-checks and play filings that follow scans and
/// identifications. It remembers what it already followed up, so the loop
/// keeps one for its whole life.
#[derive(Clone)]
pub struct RepairWorker {
    db: Db,
    store: Arc<SqliteIdentifyStore>,
    identify: Arc<IdentifyService>,
    ids: Arc<dyn IdGenerator>,
    coordinator: Arc<ScanCoordinator>,
    hygiene_checked: Arc<AtomicBool>,
    /// The scan run whose cleanup this worker already queued or skipped.
    hygiene_scan: Arc<Mutex<Option<String>>>,
    /// The latest ended scan when play filing was last queued or skipped.
    /// `None` until the first look after a server start.
    plays_scan: Arc<Mutex<Option<Option<String>>>>,
    /// The latest ended scan when the one-time re-identifications were last
    /// checked; `None` until the first look after a server start.
    backfill_scan: Arc<Mutex<Option<Option<String>>>>,
    management: Live<LibraryManagement>,
    wakeups: WorkWakeups,
    /// The worker hands its job back between albums once this is
    /// cancelled, so a stopping server never waits out a whole job.
    shutdown: Option<CancellationToken>,
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
}

impl RepairWorker {
    /// A worker for the jobs `repairs` queues, over the same database,
    /// ids, wakeups and clock.
    pub fn new(
        repairs: &Repairs,
        store: Arc<SqliteIdentifyStore>,
        identify: Arc<IdentifyService>,
        coordinator: Arc<ScanCoordinator>,
        management: Live<LibraryManagement>,
    ) -> Self {
        Self {
            db: repairs.db.clone(),
            store,
            identify,
            ids: repairs.ids.clone(),
            coordinator,
            hygiene_checked: Arc::new(AtomicBool::new(false)),
            hygiene_scan: Arc::new(Mutex::new(None)),
            plays_scan: Arc::new(Mutex::new(None)),
            backfill_scan: Arc::new(Mutex::new(None)),
            management,
            wakeups: repairs.wakeups.clone(),
            shutdown: None,
            clock: repairs.clock.clone(),
        }
    }

    /// Wakes the worker when a repair job is queued or resumed.
    pub(super) fn wakeups(&self) -> &WorkWakeups {
        &self.wakeups
    }

    /// Whether a scan is running; the worker claims nothing meanwhile.
    pub(super) fn scanning(&self) -> bool {
        !self.coordinator.current().is_empty()
    }

    /// The same service, handing its job back between albums once
    /// `shutdown` is cancelled.
    pub fn stopping_with(&self, shutdown: CancellationToken) -> Self {
        Self {
            shutdown: Some(shutdown),
            ..self.clone()
        }
    }

    /// Whether the worker must hand its job back: a scan started, or the
    /// server is stopping.
    fn must_yield(&self) -> bool {
        !self.coordinator.current().is_empty()
            || self
                .shutdown
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
    }

    /// Queue a cleanup of albums identification finished with (v2's
    /// per-album hygiene after identification), one job per page of
    /// finished identifications after the stored watermark. Each job is
    /// queued in the write that moves the watermark past its page, so a
    /// restart neither repeats nor skips one, and a busy database leaves
    /// both for the next turn. The watermark was seeded when it was
    /// added: identifications finished before then were left to the
    /// one-time cleanup.
    fn queue_hygiene_after_identification(&self) -> Result<(), OperationError> {
        loop {
            let (after, page) = self.db.read_blocking("repairs.identified_after", |conn| {
                let after = match store::identified_watermark(conn)? {
                    Ok(after) => after,
                    Err(seed) => return Ok::<_, OperationError>((seed, None)),
                };
                let page = store::identified_after(conn, &after)?;
                Ok((after, Some(page)))
            })?;
            let Some(page) = page else {
                return self.db.write_blocking(
                    Lane::Background,
                    "repairs.seed_identified_watermark",
                    |tx| Ok(store::set_identified_watermark(tx, &after)?),
                );
            };
            let Some(last) = page.last.clone() else {
                return Ok(());
            };
            self.db
                .write_blocking(Lane::Background, "repairs.queue_hygiene_identified", |tx| {
                    if !page.albums.is_empty() {
                        let new = NewRepair {
                            job_id: self.ids.new_id(),
                            purpose: Purpose::CatalogIdentityHygiene,
                            requested_by_user_id: None,
                            idempotency_key: identified_key(&after, &last),
                            root_ids: Vec::new(),
                            album_ids: page.albums.clone(),
                            scan_run_id: None,
                            source_matcher_version: None,
                            target_matcher_version: HYGIENE_VERSION.to_owned(),
                        };
                        store::create(tx, &new, self.clock.now_secs())?;
                    }
                    Ok::<_, OperationError>(store::set_identified_watermark(tx, &last)?)
                })?;
            if !page.full || self.must_yield() {
                return Ok(());
            }
        }
    }

    /// Queue the one-time catalog cleanup when this database has albums
    /// it could apply to (imported groupings, empty shells, albums on the
    /// placeholder artist).
    fn queue_hygiene_once(&self) -> Result<(), OperationError> {
        if self.hygiene_checked.load(Ordering::Relaxed) {
            return Ok(());
        }
        let queued = self
            .db
            .write_blocking(Lane::Background, "repairs.queue_hygiene_once", |tx| {
                if store::queued(tx, store::HYGIENE_BACKFILL_KEY)?
                    || !store::hygiene_candidates_exist(tx)?
                {
                    return Ok(());
                }
                let new = NewRepair {
                    job_id: self.ids.new_id(),
                    purpose: Purpose::CatalogIdentityHygiene,
                    requested_by_user_id: None,
                    idempotency_key: store::HYGIENE_BACKFILL_KEY.to_owned(),
                    root_ids: Vec::new(),
                    album_ids: Vec::new(),
                    scan_run_id: None,
                    source_matcher_version: None,
                    target_matcher_version: HYGIENE_VERSION.to_owned(),
                };
                store::create(tx, &new, self.clock.now_secs()).map(|_| ())
            });
        if settled(&queued) {
            self.hygiene_checked.store(true, Ordering::Relaxed);
        }
        queued
    }

    /// Identify once more, at background priority, every album an older
    /// matcher may have left on another edition or unmatched because one
    /// file's track id named another edition (see
    /// [`store::track_id_backfill_albums`]), and every album whose
    /// automatic match names a release group but no release (see
    /// [`store::group_only_backfill_albums`]), and every confirmed
    /// automatic match outside the release group its files name (see
    /// [`store::older_confirmation_albums`]), and every confirmed
    /// automatic match on an edition other than the one its files'
    /// barcode or catalog number names (see
    /// [`store::edition_id_albums`]). Only a database a v2 import was
    /// committed into has such albums; any other is never looked at. Each
    /// set is looked at once per server start and after each scan until it
    /// first holds an album, so a library that arrives later (a first scan
    /// after the import) still gets it; then never again.
    fn queue_backfills_once(&self) -> Result<(), OperationError> {
        let (run, carried) = self.db.read_blocking("repairs.latest_ended_scan", |conn| {
            Ok::<_, OperationError>((
                store::latest_ended_scan(conn)?,
                store::carried_from_v2(conn)?,
            ))
        })?;
        // A busy database leaves the run unseen, so the next turn queues
        // its albums again.
        let mut seen = self
            .backfill_scan
            .lock()
            .map_err(|_| OperationError::Store("backfill marker poisoned".to_owned()))?;
        if seen.as_ref() == Some(&run) {
            return Ok(());
        }
        if !carried {
            *seen = Some(run);
            return Ok(());
        }
        // Each pass runs on its own: one that fails leaves the other be.
        let passes = [
            (
                store::TRACK_ID_BACKFILL_KEY,
                self.queue_backfill(
                    store::TRACK_ID_BACKFILL_KEY,
                    store::track_id_backfill_albums,
                    "queued albums for re-identification after the track id rule changed",
                ),
            ),
            (
                store::GROUP_ONLY_BACKFILL_KEY,
                self.queue_backfill(
                    store::GROUP_ONLY_BACKFILL_KEY,
                    store::group_only_backfill_albums,
                    "queued albums matched to a release group only, to find their edition",
                ),
            ),
            (
                store::OLDER_CONFIRMATIONS_BACKFILL_KEY,
                self.queue_backfill(
                    store::OLDER_CONFIRMATIONS_BACKFILL_KEY,
                    store::older_confirmation_albums,
                    "queued confirmed albums outside the release group their files name",
                ),
            ),
            (
                store::EDITION_IDS_BACKFILL_KEY,
                self.queue_backfill(
                    store::EDITION_IDS_BACKFILL_KEY,
                    store::edition_id_albums,
                    "queued confirmed albums whose files' barcode or catalog number names another edition",
                ),
            ),
        ];
        let mut busy = None;
        for (key, result) in passes {
            match result {
                Ok(()) => {}
                Err(error @ OperationError::Busy(_)) => busy = Some(error),
                Err(error) => {
                    tracing::error!(key, error = %error, "a one-time re-identification could not be queued");
                }
            }
        }
        match busy {
            Some(error) => Err(error),
            None => {
                *seen = Some(run);
                Ok(())
            }
        }
    }

    /// Queue identification for the albums `set` lists, unless the
    /// marker under `key` says it was done, then write that marker.
    fn queue_backfill(
        &self,
        key: &'static str,
        set: fn(&rusqlite::Connection) -> rusqlite::Result<Vec<String>>,
        message: &'static str,
    ) -> Result<(), OperationError> {
        let albums = self.db.read_blocking("repairs.backfill_albums", |conn| {
            if store::queued(conn, key)? {
                return Ok::<_, OperationError>(Vec::new());
            }
            let mut albums = Vec::new();
            for album in set(conn)? {
                let revision = album_input_revision(conn, &album)?;
                albums.push((album, revision));
            }
            Ok(albums)
        })?;
        if albums.is_empty() {
            return Ok(());
        }
        // Each album is queued in a write of its own. One that fails stops
        // the mark, so the next scan queues them again; queueing an album
        // twice reuses its live job.
        for (album, revision) in &albums {
            self.identify.enqueue_album(
                &self.ids.new_id(),
                album,
                IdentifyKind::Historical,
                revision,
                None,
                self.clock.now_ms_u64(),
                Lane::Background,
            )?;
        }
        tracing::info!(albums = albums.len(), key, "{message}");
        self.db
            .write_blocking(Lane::Background, "repairs.mark_backfill", |tx| {
                Ok(store::mark_backfill(
                    tx,
                    &self.ids.new_id(),
                    key,
                    self.clock.now_secs(),
                )?)
            })
    }

    /// After each finished scan, queue a cleanup of the albums that scan
    /// changed and of empty album shells (v2's per-album hygiene). Checked
    /// once per run; nothing is queued when no album qualifies.
    fn queue_hygiene_after_scan(&self) -> Result<(), OperationError> {
        let Some(run) = self.db.read_blocking(
            "repairs.queue_hygiene_after_scan_latest_ended_scan",
            |conn| Ok::<_, OperationError>(store::latest_ended_scan(conn)?),
        )?
        else {
            return Ok(());
        };
        // A busy database leaves the run unseen.
        let mut seen = self
            .hygiene_scan
            .lock()
            .map_err(|_| OperationError::Store("hygiene marker poisoned".to_owned()))?;
        if seen.as_deref() == Some(run.as_str()) {
            return Ok(());
        }
        let queued =
            self.db
                .write_blocking(Lane::Background, "repairs.queue_hygiene_after_scan", |tx| {
                    if store::hygiene_count_after_scan(tx, &run)? == 0 {
                        return Ok(());
                    }
                    let new = NewRepair {
                        job_id: self.ids.new_id(),
                        purpose: Purpose::CatalogIdentityHygiene,
                        requested_by_user_id: None,
                        idempotency_key: store::hygiene_scan_key(&run),
                        root_ids: Vec::new(),
                        album_ids: Vec::new(),
                        scan_run_id: Some(run.clone()),
                        source_matcher_version: None,
                        target_matcher_version: HYGIENE_VERSION.to_owned(),
                    };
                    store::create(tx, &new, self.clock.now_secs()).map(|_| ())
                });
        if settled(&queued) {
            *seen = Some(run);
        }
        queued
    }

    /// Queue a filing of the plays that name no track: once at a server
    /// start, then after each scan that ends, since a scan is what brings
    /// the tracks older plays can be matched to. Nothing is queued when
    /// every play is filed.
    fn queue_play_filing(&self) -> Result<(), OperationError> {
        let run = self
            .db
            .read_blocking("repairs.queue_play_filing_latest_ended_scan", |conn| {
                Ok::<_, OperationError>(store::latest_ended_scan(conn)?)
            })?;
        // A busy database leaves the run unseen.
        let mut seen = self
            .plays_scan
            .lock()
            .map_err(|_| OperationError::Store("play filing marker poisoned".to_owned()))?;
        if seen.as_ref() == Some(&run) {
            return Ok(());
        }
        let after_scan = run.as_deref().filter(|_| seen.is_some());
        let queued = self
            .db
            .write_blocking(Lane::Background, "repairs.queue_play_filing", |tx| {
                plays::queue(tx, &self.ids.new_id(), after_scan, self.clock.now_secs())?;
                Ok::<_, OperationError>(())
            });
        if settled(&queued) {
            *seen = Some(run);
        }
        queued
    }

    /// Work on a play filing job: batch after batch until no row is left,
    /// a scan starts, the server stops, or the job is paused or stopped.
    /// Each batch is one transaction that also saves where the job is, so
    /// the next claim (or the next server start) goes on from there.
    fn file_plays(
        &self,
        job_id: &str,
        worker: &str,
    ) -> Result<Option<OperationJob>, OperationError> {
        loop {
            let yielding = self.must_yield();
            let left = self
                .db
                .write_blocking(Lane::Background, "repairs.file_plays", |tx| {
                    let now = self.clock.now_secs();
                    if let Some(left) = halt(tx, job_id, worker, yielding, now)? {
                        return Ok::<_, OperationError>(Some(left));
                    }
                    match plays::step(tx, job_id, plays::BATCH, now) {
                        Ok(false) => return Ok(None),
                        Ok(true) => {}
                        // Logged where it was read.
                        Err(error) if Unreadable::is(&error) => {
                            return Ok(Some(store::end(
                                tx,
                                job_id,
                                worker,
                                OperationState::Failed,
                                STATE_UNREADABLE,
                                now,
                            )?));
                        }
                        Err(error) => return Err(error.into()),
                    }
                    Ok(Some(store::end(
                        tx,
                        job_id,
                        worker,
                        OperationState::Succeeded,
                        plays::FILED,
                        now,
                    )?))
                })?;
            let Some(left) = left else {
                continue;
            };
            let filed = left
                .as_ref()
                .is_some_and(|job| job.terminal_code.as_deref() == Some(plays::FILED));
            if filed
                && let Some(summary) = self
                    .db
                    .read_blocking("repairs.play_filing_summary", |conn| {
                        Ok::<_, OperationError>(store::snapshot(conn, job_id)?.transpose()?)
                    })?
                    .and_then(|snap| snap.summary)
            {
                tracing::info!(
                    job_id,
                    rows = summary.total_identities,
                    outcomes = ?summary.counts_by_finding,
                    reasons = ?summary.counts_by_reason,
                    "plays that named no track were looked at"
                );
            }
            return Ok(left);
        }
    }

    /// Whether the album's root has automatic edition acceptance on: the
    /// root's Library Management profile with its overrides, or the
    /// default profile for a root without an assignment.
    fn auto_accept_enabled(&self, root_id: &str) -> bool {
        let settings = match self.management.get() {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, "library management settings unreadable; no automatic edition acceptance");
                return false;
            }
        };
        let profile = match settings
            .root_assignments
            .iter()
            .find(|assignment| assignment.root_id == root_id)
        {
            Some(assignment) => {
                crate::management::settings::pin_profile(&settings, assignment).ok()
            }
            None => settings
                .profiles
                .iter()
                .find(|profile| profile.id == settings.default_profile_id)
                .cloned(),
        };
        profile.is_some_and(|profile| profile.identity.automatic_edition_acceptance_enabled)
    }

    /// Requeue every repair job an earlier process left running, with its
    /// running work. The loop's first turn calls this. Blocking.
    pub fn recover_at_start(&self) -> Result<u64, OperationError> {
        self.db
            .write_blocking(Lane::Background, "repairs.recover_at_start", |tx| {
                Ok::<_, OperationError>(jobs::recover_at_start(
                    tx,
                    OperationKind::Repair,
                    self.clock.now_secs(),
                )?)
            })
    }

    /// Requeue the repair jobs whose worker vanished, claim the next due
    /// one and work on it until it is done, waits, or is halted. Returns
    /// the job as it was left, or `None` when nothing was due. Store calls
    /// block: run this on a blocking thread.
    pub async fn run_next(&self, worker: &str) -> Result<Option<OperationJob>, OperationError> {
        self.queue_hygiene_once()?;
        self.queue_backfills_once()?;
        self.queue_hygiene_after_scan()?;
        self.queue_hygiene_after_identification()?;
        self.queue_play_filing()?;
        let claimed = self
            .db
            .write_blocking(Lane::Background, "repairs.claim", |tx| {
                Ok::<_, OperationError>(jobs::claim(
                    tx,
                    OperationKind::Repair,
                    worker,
                    self.clock.now_secs(),
                )?)
            })?;
        let Some(job) = claimed else {
            return Ok(None);
        };
        match self.work(&job, worker).await {
            Err(OperationError::Busy(cause)) => self.wait_busy(&job.id, worker, cause),
            left => left,
        }
    }

    /// Work the database was too busy for: the job goes back to the queue
    /// as it stood and waits a few seconds, plus up to as much again. A
    /// put-back that fails leaves it to its lease.
    fn wait_busy(
        &self,
        job_id: &str,
        worker: &str,
        cause: String,
    ) -> Result<Option<OperationJob>, OperationError> {
        tracing::warn!(cause, job_id, "repair job waits: the database is busy");
        let now = self.clock.now_secs();
        let jitter = SplitMix64::new(SplitMix64::seed_from_time()).below_duration(BUSY_RETRY);
        let retry_at = now + (BUSY_RETRY + jitter).as_secs_f64();
        self.db
            .write_blocking(Lane::Background, "repairs.wait_busy", |tx| {
                jobs::release_until(tx, job_id, worker, retry_at, None, now)?;
                Ok::<_, OperationError>(jobs::job(tx, job_id)?)
            })
            .map_err(|error| {
                tracing::warn!(%error, job_id, "repair job not put back; its lease brings it back");
                OperationError::Busy(cause)
            })
    }

    /// Work on a claimed repair job, album by album.
    async fn work(
        &self,
        job: &OperationJob,
        worker: &str,
    ) -> Result<Option<OperationJob>, OperationError> {
        let snap = self.db.read_blocking("repairs.job_snapshot", |conn| {
            Ok::<_, OperationError>(store::snapshot(conn, &job.id)?)
        })?;
        let snap = match snap {
            Some(Ok(snap)) => snap,
            // Logged where it was read.
            unusable => {
                let code = match unusable {
                    None => "MISSING_SNAPSHOT",
                    Some(_) => STATE_UNREADABLE,
                };
                return self.db.write_blocking(
                    Lane::Background,
                    "repairs.end_unusable_snapshot",
                    |tx| {
                        Ok(store::end(
                            tx,
                            &job.id,
                            worker,
                            OperationState::Failed,
                            code,
                            self.clock.now_secs(),
                        )?)
                    },
                );
            }
        };
        let actor = job.requested_by_user_id.clone();
        if snap.purpose == Purpose::PlayHistoryFiling {
            return self.file_plays(&job.id, worker);
        }
        loop {
            let yielding = self.must_yield();
            let next = self
                .db
                .write_blocking(Lane::Background, "repairs.claim_album", |tx| {
                    let now = self.clock.now_secs();
                    if let Some(left) = halt(tx, &job.id, worker, yielding, now)? {
                        return Ok::<_, OperationError>(Err(left));
                    }
                    match store::claim_album(tx, &job.id, now)? {
                        Some(work) => Ok(Ok(work)),
                        None => Ok(Err(self.finish(tx, &job.id, worker, &snap, now)?)),
                    }
                })?;
            let (ordinal, album_id) = match next {
                Ok(work) => work,
                Err(left) => return Ok(left),
            };
            match (snap.purpose, snap.phase) {
                (Purpose::CatalogIdentityHygiene, _) => {
                    self.db
                        .write_blocking(Lane::Background, "repairs.clean_album", |tx| {
                            if !jobs::holds(tx, &job.id, worker)? {
                                return Ok(());
                            }
                            let now = self.clock.now_secs();
                            let changed = hygiene::clean_album(tx, &album_id, &job.id, now)?;
                            let state = if changed.is_some() {
                                WorkState::Succeeded
                            } else {
                                WorkState::Skipped
                            };
                            store::finish_work(
                                tx,
                                &job.id,
                                worker,
                                ordinal,
                                state,
                                None,
                                changed.as_ref(),
                                now,
                            )?;
                            Ok::<_, OperationError>(())
                        })?
                }
                (_, Phase::Apply) => {
                    let accepted =
                        self.db
                            .write_blocking(Lane::Background, "repairs.apply_album", |tx| {
                                if !jobs::holds(tx, &job.id, worker)? {
                                    return Ok(false);
                                }
                                apply_album(
                                    tx,
                                    &job.id,
                                    worker,
                                    ordinal,
                                    &album_id,
                                    actor.as_deref(),
                                    self.clock.now_secs(),
                                )
                            })?;
                    if accepted {
                        self.store.link_after_command(&album_id);
                    }
                }
                (purpose, Phase::DryRun) => match self.check(purpose, &album_id).await? {
                    Checked::Found(verdict, identity_revision) => {
                        let accepted = self.db.write_blocking(
                            Lane::Background,
                            "repairs.save_finding",
                            |tx| {
                                if !jobs::holds(tx, &job.id, worker)? {
                                    return Ok(false);
                                }
                                let now = self.clock.now_secs();
                                store::save_finding(
                                    tx,
                                    &job.id,
                                    worker,
                                    ordinal,
                                    &album_id,
                                    &verdict,
                                    identity_revision,
                                    now,
                                )?;
                                store::auto_accept(tx, &job.id, &album_id, &verdict, now)
                            },
                        )?;
                        if accepted {
                            self.store.link_after_command(&album_id);
                        }
                    }
                    Checked::Deferred => {
                        return self.db.write_blocking(
                            Lane::Background,
                            "repairs.provider_deferred",
                            |tx| {
                                let now = self.clock.now_secs();
                                jobs::release_until(
                                    tx,
                                    &job.id,
                                    worker,
                                    now + PROVIDER_RETRY_SECS,
                                    Some("PROVIDER_DEFERRED"),
                                    now,
                                )?;
                                Ok(jobs::job(tx, &job.id)?)
                            },
                        );
                    }
                },
            }
        }
    }

    /// No album left: a check's report is ready; Apply and cleanup are done.
    fn finish(
        &self,
        tx: &Transaction<'_>,
        job_id: &str,
        worker: &str,
        snap: &RepairSnapshot,
        now: f64,
    ) -> Result<Option<OperationJob>, OperationError> {
        Ok(match (snap.purpose, snap.phase) {
            (Purpose::CatalogIdentityHygiene, _) => store::end(
                tx,
                job_id,
                worker,
                OperationState::Succeeded,
                "CATALOG_IDENTITY_HYGIENE_COMPLETED",
                now,
            )?,
            (_, Phase::Apply) => store::end(
                tx,
                job_id,
                worker,
                OperationState::Succeeded,
                "APPLY_COMPLETED",
                now,
            )?,
            (_, Phase::DryRun) => store::mark_ready(tx, job_id, worker, snap, now)?,
        })
    }

    fn facts(&self, album_id: &str) -> Result<Option<LocalAlbumFacts>, OperationError> {
        Ok(self
            .store
            .album_facts(album_id)?
            .filter(|facts| !facts.tracks.is_empty()))
    }

    /// Check one album against MusicBrainz.
    async fn check(&self, purpose: Purpose, album_id: &str) -> Result<Checked, OperationError> {
        let subject = self.db.read_blocking("repairs.check", |conn| {
            Ok::<_, OperationError>(store::subject(conn, album_id)?)
        })?;
        let Some(subject) = subject else {
            return Ok(Checked::Found(
                Box::new(Verdict::new(FindingCode::Stale, "IDENTITY_CHANGED")),
                None,
            ));
        };
        let found = |mut verdict: Verdict| {
            verdict.payload.guard = Some(subject.guard.clone());
            Ok(Checked::Found(Box::new(verdict), subject.identity_revision))
        };
        // Folders kept to their own tags are never looked up.
        if subject.policies.iter().any(|policy| policy != "automatic") {
            return found(Verdict::new(
                FindingCode::Unverifiable,
                "LOCAL_METADATA_POLICY",
            ));
        }
        if purpose == Purpose::ManagementReadiness
            && let Some(current) = subject.custom_current
        {
            return found(if current {
                Verdict::new(FindingCode::Ready, "CUSTOM_EDITION_MANIFEST_VERIFIED")
            } else {
                Verdict::new(FindingCode::NeedsReview, "CUSTOM_MANIFEST_STALE")
            });
        }
        let identity = subject.identity.clone();
        if purpose == Purpose::ExistingMatches {
            match &identity {
                None => return found(Verdict::new(FindingCode::Stale, "IDENTITY_CHANGED")),
                Some(identity) if identity.manual => {
                    return found(classify::existing_match(identity, None, 0, 0));
                }
                Some(identity) if identity.release_mbid.is_none() => {
                    return found(Verdict::new(
                        FindingCode::Unverifiable,
                        "EVIDENCE_UNAVAILABLE",
                    ));
                }
                Some(_) => {}
            }
        }
        let Some(facts) = self.facts(album_id)? else {
            return Ok(Checked::Found(
                Box::new(Verdict::new(FindingCode::Stale, "IDENTITY_CHANGED")),
                None,
            ));
        };
        let release = identity.as_ref().and_then(|id| id.release_mbid.clone());
        let recall = self
            .identify
            .recall(
                &facts,
                release.as_deref(),
                Lane::Background,
                RequestPriority::BackgroundSync,
            )
            .await?;
        if recall.provider_deferred {
            return Ok(Checked::Deferred);
        }
        let Some(ranking) = self.identify.rank(&facts, &recall) else {
            return Ok(Checked::Deferred);
        };
        let candidates = evaluation(&facts, &recall, &ranking, false).candidates;
        let identity = identity.unwrap_or_default();
        let verdict = match release {
            Some(release) => {
                let held = recall
                    .releases
                    .iter()
                    .find(|candidate| candidate.answers_to(&release));
                let candidate = held.and_then(|held| {
                    candidates.iter().find(|candidate| {
                        candidate
                            .evidence
                            .release_mbid
                            .as_deref()
                            .is_some_and(|id| id.eq_ignore_ascii_case(&held.id))
                    })
                });
                if purpose == Purpose::ExistingMatches {
                    classify::existing_match(
                        &identity,
                        candidate,
                        held.map_or(0, |held| held.tracks.len()),
                        facts.tracks.len(),
                    )
                } else {
                    classify::readiness_exact(
                        &identity,
                        candidate,
                        &subject.tracks,
                        &recall.recording_aliases,
                    )
                }
            }
            None => classify::readiness_suggest(
                &identity,
                &candidates,
                self.auto_accept_enabled(&subject.root_id),
                |candidate| summary_for(&recall, candidate),
            ),
        };
        found(verdict)
    }
}

/// What the readiness report shows about a suggested edition.
fn summary_for(recall: &RecallResult, candidate: &ReidentificationCandidate) -> EditionSummary {
    let release = candidate.evidence.release_mbid.as_deref().and_then(|id| {
        recall
            .releases
            .iter()
            .find(|release| release.answers_to(id))
    });
    EditionSummary {
        title: release.map_or_else(
            || candidate.evidence.album_title.clone(),
            |release| release.title.clone(),
        ),
        date: release
            .and_then(|release| release.date.clone())
            .or_else(|| candidate.release_date.clone()),
        country: release.and_then(|release| release.country.clone()),
        status: release.and_then(|release| release.status.clone()),
        track_count: release.map_or(
            (candidate.tracks.len() + candidate.unmatched_expected_tracks.len()) as u32,
            |release| release.tracks.len() as u32,
        ),
        competing_count: 1,
        auto_gate: None,
    }
}

/// Before the next album or batch: hand the job back when `yielding` (a
/// scan started or the server is stopping), honour a pause or stop, and
/// keep the lease. `Some` with the job as it was left when the worker
/// must let go of it.
fn halt(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    yielding: bool,
    now: f64,
) -> Result<Option<Option<OperationJob>>, OperationError> {
    if yielding {
        jobs::release(tx, job_id, worker, now)?;
        return Ok(Some(jobs::job(tx, job_id)?));
    }
    if let Some(left) = jobs::checkpoint(tx, job_id, worker, now)? {
        return Ok(Some(Some(left)));
    }
    if !jobs::heartbeat(tx, job_id, worker, now)? {
        return Ok(Some(jobs::job(tx, job_id)?));
    }
    Ok(None)
}

/// Idempotency key of the cleanup of identifications after `after` up
/// to and including `last`.
fn identified_key(after: &Watermark, last: &Watermark) -> String {
    format!(
        "catalog-identity-hygiene:v1:identified:{}:{}:{}:{}",
        after.at_ms, after.job_id, last.at_ms, last.job_id
    )
}

/// Why a job whose stored state does not decode ended.
const STATE_UNREADABLE: &str = "REPAIR_STATE_UNREADABLE";

/// Whether a queueing step is over for its marker: anything but a busy
/// database, which the next turn tries again. Another error is returned
/// as before, but never holds every later repair back.
fn settled(result: &Result<(), OperationError>) -> bool {
    !matches!(result, Err(OperationError::Busy(_)))
}

fn dedupe(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

/// A `"<float>:<id>"` page cursor.
fn parse_cursor(raw: &str) -> Result<(f64, String), OperationError> {
    raw.split_once(':')
        .and_then(|(at, id)| Some((at.parse::<f64>().ok()?, id.to_owned())))
        .filter(|(at, id)| at.is_finite() && !id.is_empty())
        .ok_or(OperationError::Invalid(reasons::CURSOR_INVALID))
}
