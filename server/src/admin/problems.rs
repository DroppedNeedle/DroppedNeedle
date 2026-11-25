//! `GET /api/v3/admin/problems`: what is wrong right now, in one list, for
//! the admin's Health page and header mark.
//!
//! Every fact comes from what already holds it: the settings sections, the
//! loop supervisor's memory, the jobs table, the download client probes,
//! the recycle bin and import folder checks, the publish journal, the disk
//! watch, the backups folder and the outside services' health. Memory and
//! cheap local reads only, never an outside call. The reads that touch the
//! filesystem (the recycle bin, the import folder, the backups folder) run
//! one at a time under a bound and are kept for a minute, so a dead mount
//! never holds the route: while one is still running and nothing is kept,
//! those rows are left out ("could not check").

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::{Json, extract::State};
use serde::Serialize;
use utoipa::ToSchema;

use crate::admin::backups::{BACKUP_INTEGRITY_FAILED, BACKUP_JOB, BACKUP_NO_ROOM};
use crate::settings::wiring::SettingsSetup;
use crate::state::AppState;
use droppedneedle_acquire::import_folder::{ImportFolderState, ImportFolders};
use droppedneedle_acquire::probes::ProbeCache;
use droppedneedle_auth::Admin;
use droppedneedle_kernel::db::{BackupService, Db};
use droppedneedle_kernel::disk::{DiskLevel, DiskWatch, FolderRole};
use droppedneedle_kernel::jobs::{JobState, Supervisor, queue};
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::Backups;
use droppedneedle_library::management::service::Management;
use droppedneedle_providers::health::ServiceHealth;

/// How long a loop fails before it is listed.
const LOOP_FAILING_FOR_MS: i64 = 10 * 60 * 1000;
/// The window "failed jobs" counts over.
const JOBS_FAILED_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// A nightly backup older than this is listed while nightly backups are on.
const BACKUP_OLD_MS: i64 = 48 * 60 * 60 * 1000;
/// How long the filesystem reads may take before the route answers
/// without them.
const SLOW_BOUND: Duration = Duration::from_secs(2);
/// How long the filesystem reads are kept.
const SLOW_KEPT: Duration = Duration::from_secs(60);

/// What is wrong. Each kind has one severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    /// A settings section does not read; the work it controls waits.
    SettingsUnreadable,
    /// A background loop has failed for ten minutes or stopped failed.
    LoopFailing,
    /// A download client that is on and set up does not answer.
    DownloadClientDown,
    /// slskd's downloads folder is not there or cannot be written.
    DownloadFolderUnusable,
    /// The recycle bin cannot be used; removals and upgrades wait.
    RecycleBinUnavailable,
    /// The import folder cannot be used.
    ImportFolderUnusable,
    /// File changes in the publish journal need a person.
    PublishNeedsAttention,
    /// The data disk has no room for a backup or an undo copy.
    DiskNearlyFull,
    /// The nightly backup fails its integrity check or has no room.
    BackupFailing,
    /// Jobs failed for good in the last 24 hours.
    JobsFailed,
    /// A download client is on but not set up, or not signed in.
    DownloadClientSetup,
    /// A disk the app writes is running low.
    DiskLow,
    /// No nightly backup in 48 hours while nightly backups are on.
    BackupOld,
    /// The database lives in memory: nothing is kept across a restart.
    DatabaseInMemory,
    /// An outside service is not answering.
    ProviderDegraded,
}

/// How much a problem needs the admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemSeverity {
    /// Something has stopped working.
    Broken,
    /// Worth a look before it stops something.
    Warning,
    /// Outside the server; for information.
    Info,
}

impl ProblemKind {
    /// The kind's fixed severity.
    pub fn severity(self) -> ProblemSeverity {
        match self {
            Self::SettingsUnreadable
            | Self::LoopFailing
            | Self::DownloadClientDown
            | Self::DownloadFolderUnusable
            | Self::RecycleBinUnavailable
            | Self::ImportFolderUnusable
            | Self::PublishNeedsAttention
            | Self::DiskNearlyFull
            | Self::BackupFailing => ProblemSeverity::Broken,
            Self::JobsFailed
            | Self::DownloadClientSetup
            | Self::DiskLow
            | Self::BackupOld
            | Self::DatabaseInMemory => ProblemSeverity::Warning,
            Self::ProviderDegraded => ProblemSeverity::Info,
        }
    }
}

/// One problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Problem {
    /// What is wrong.
    pub kind: ProblemKind,
    /// How much it needs the admin, fixed by the kind.
    pub severity: ProblemSeverity,
    /// What it is about, a name the server gives: a settings section key,
    /// a download client (`slskd`, `sabnzbd`, `nzbget`, `deemix`), a loop
    /// name, a job kind, a folder role (`data`, `library`, ...) or an
    /// outside service key. Never text a person typed.
    pub subject: Option<String>,
    /// How many: failed jobs, journal rows, folders.
    pub count: u64,
    /// Since when, unix milliseconds, when that is known.
    pub since_at: Option<i64>,
    /// The recycle bin only: the folder may be accepted as the bin.
    pub accept_folder: bool,
}

impl Problem {
    fn new(kind: ProblemKind, subject: Option<&str>) -> Self {
        Self {
            kind,
            severity: kind.severity(),
            subject: subject.map(str::to_owned),
            count: 1,
            since_at: None,
            accept_folder: false,
        }
    }

    fn count(mut self, count: u64) -> Self {
        self.count = count;
        self
    }

    fn since(mut self, since_at: Option<i64>) -> Self {
        self.since_at = since_at;
        self
    }
}

/// The problems list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProblemsResponse {
    /// Broken first, then warnings, then information.
    pub problems: Vec<Problem>,
    /// When the list was worked out, unix milliseconds.
    pub checked_at: i64,
}

/// What the filesystem reads found, kept for [`SLOW_KEPT`].
#[derive(Debug, Clone, Default)]
struct SlowFacts {
    /// The recycle bin's warning, and whether its folder may be accepted.
    bin: Option<bool>,
    /// The import folder's state when it cannot be used.
    import_folder: Option<ImportFolderState>,
    /// Publish journal rows that need a person.
    needs_attention: i64,
    /// When the newest nightly backup was taken.
    newest_nightly_at: Option<i64>,
}

struct Kept {
    at: Instant,
    facts: SlowFacts,
}

/// The problems service, built once from the app's state.
#[derive(Clone)]
pub struct Problems {
    settings: SettingsSetup,
    supervisor: Supervisor,
    db: Db,
    probes: Arc<ProbeCache>,
    import_folder: Arc<ImportFolders>,
    management: Management,
    disk: DiskWatch,
    backups: Arc<BackupService>,
    nightly: Live<Backups>,
    health: Arc<ServiceHealth>,
    kept: Arc<Mutex<Option<Kept>>>,
    reading: Arc<AtomicBool>,
}

impl Problems {
    /// The service over what `state` holds.
    pub fn from_state(state: &AppState) -> Self {
        Self {
            settings: state.settings.clone(),
            supervisor: state.admin.supervisor.clone(),
            db: state.admin.db.db.clone(),
            probes: state.acquire.probe_cache.clone(),
            import_folder: state.acquire.import_folder.clone(),
            management: state.library_services.management.clone(),
            disk: state.admin.disk.clone(),
            backups: state.admin.backups.clone(),
            nightly: state.settings.service().store.live(),
            health: state.admin.providers.health.clone(),
            kept: Arc::new(Mutex::new(None)),
            reading: Arc::new(AtomicBool::new(false)),
        }
    }

    /// What is wrong now, broken first.
    pub async fn list(&self) -> ProblemsResponse {
        let now = self.supervisor.clock().now_ms();
        let mut problems = Vec::new();
        for key in self.settings.unreadable_sections() {
            problems.push(Problem::new(ProblemKind::SettingsUnreadable, Some(key)));
        }
        self.loops(now, &mut problems);
        self.jobs(now, &mut problems).await;
        self.download_clients(&mut problems);
        self.disks(&mut problems);
        if let Some(slow) = self.slow().await {
            self.slow_rows(&slow, now, &mut problems);
        }
        if self.backups.database_in_memory() {
            problems.push(Problem::new(ProblemKind::DatabaseInMemory, None));
        }
        let seen = Instant::now();
        for degraded in self.health.current(seen) {
            let since = now.saturating_sub(
                i64::try_from(degraded.degraded_for.as_millis()).unwrap_or(i64::MAX),
            );
            problems.push(
                Problem::new(ProblemKind::ProviderDegraded, Some(degraded.service))
                    .since(Some(since)),
            );
        }
        problems.sort_by_key(|problem| match problem.severity {
            ProblemSeverity::Broken => 0,
            ProblemSeverity::Warning => 1,
            ProblemSeverity::Info => 2,
        });
        ProblemsResponse {
            problems,
            checked_at: now,
        }
    }

    /// Loops failing ten minutes or more, or stopped failed. The backup
    /// loop's own codes are listed as a failing backup at once.
    fn loops(&self, now: i64, problems: &mut Vec<Problem>) {
        for record in self.supervisor.loops() {
            let code = record.last_error.as_deref();
            if record.name == BACKUP_JOB
                && matches!(code, Some(BACKUP_INTEGRITY_FAILED | BACKUP_NO_ROOM))
            {
                problems.push(
                    Problem::new(ProblemKind::BackupFailing, code).since(record.failing_since),
                );
                continue;
            }
            let failing_long = record
                .failing_since
                .is_some_and(|since| now.saturating_sub(since) >= LOOP_FAILING_FOR_MS);
            if failing_long || record.state == JobState::Failed {
                problems.push(
                    Problem::new(ProblemKind::LoopFailing, Some(&record.name))
                        .since(record.failing_since),
                );
            }
        }
    }

    /// Jobs that failed for good in the last day, by kind.
    async fn jobs(&self, now: i64, problems: &mut Vec<Problem>) {
        match queue::failed_since(&self.db, now.saturating_sub(JOBS_FAILED_WINDOW_MS)).await {
            Ok(kinds) => {
                for (kind, failed) in kinds {
                    problems.push(Problem::new(ProblemKind::JobsFailed, Some(&kind)).count(failed));
                }
            }
            Err(error) => tracing::warn!(%error, "problems: failed jobs not counted"),
        }
    }

    /// The download clients' last probe.
    fn download_clients(&self, problems: &mut Vec<Problem>) {
        let probes = self.probes.snapshot();
        let clients = [
            ("slskd", &probes.slskd),
            ("sabnzbd", &probes.sabnzbd.status),
            ("nzbget", &probes.nzbget.status),
        ];
        for (name, probe) in clients {
            if !probe.enabled {
                continue;
            }
            if !probe.configured {
                problems.push(Problem::new(ProblemKind::DownloadClientSetup, Some(name)));
            } else if !probe.reachable {
                problems.push(Problem::new(ProblemKind::DownloadClientDown, Some(name)));
            }
        }
        let deemix = &probes.deemix;
        if deemix.enabled {
            if !deemix.configured {
                problems.push(Problem::new(
                    ProblemKind::DownloadClientSetup,
                    Some("deemix"),
                ));
            } else if !deemix.reachable {
                problems.push(Problem::new(
                    ProblemKind::DownloadClientDown,
                    Some("deemix"),
                ));
            } else if !deemix.signed_in {
                problems.push(Problem::new(
                    ProblemKind::DownloadClientSetup,
                    Some("deemix"),
                ));
            }
        }
        if probes.slskd.enabled
            && probes.slskd.configured
            && probes
                .slskd_mount
                .as_ref()
                .is_some_and(|report| !report.mount.ok)
        {
            problems.push(Problem::new(
                ProblemKind::DownloadFolderUnusable,
                Some("slskd"),
            ));
        }
    }

    /// The disk watch's latest reading: one row per filesystem that is low
    /// or nearly full, named by its first folder's role.
    fn disks(&self, problems: &mut Vec<Problem>) {
        let Some(sample) = self.disk.latest() else {
            return;
        };
        for filesystem in &sample.filesystems {
            let kind = match filesystem.level {
                DiskLevel::Critical => ProblemKind::DiskNearlyFull,
                DiskLevel::Low => ProblemKind::DiskLow,
                DiskLevel::Ok | DiskLevel::Unknown => continue,
            };
            let role = filesystem
                .folders
                .first()
                .map(|folder| role_name(folder.role));
            let folders = u64::try_from(filesystem.folders.len()).unwrap_or(u64::MAX);
            problems.push(
                Problem::new(kind, role)
                    .count(folders)
                    .since(Some(sample.checked_at_ms)),
            );
        }
    }

    fn slow_rows(&self, slow: &SlowFacts, now: i64, problems: &mut Vec<Problem>) {
        if let Some(accept_folder) = slow.bin {
            let mut problem = Problem::new(ProblemKind::RecycleBinUnavailable, None);
            problem.accept_folder = accept_folder;
            problems.push(problem);
        }
        if let Some(state) = slow.import_folder {
            problems.push(Problem::new(
                ProblemKind::ImportFolderUnusable,
                Some(import_state_name(state)),
            ));
        }
        if slow.needs_attention > 0 {
            problems.push(
                Problem::new(ProblemKind::PublishNeedsAttention, None)
                    .count(u64::try_from(slow.needs_attention).unwrap_or(0)),
            );
        }
        let nightly_on = self.nightly.read().is_some_and(|section| section.nightly);
        if nightly_on
            && let Some(at) = slow.newest_nightly_at
            && now.saturating_sub(at) >= BACKUP_OLD_MS
        {
            problems.push(Problem::new(ProblemKind::BackupOld, None).since(Some(at)));
        }
    }

    /// The filesystem facts: kept ones under a minute old, else read now
    /// under [`SLOW_BOUND`]. `None` while an earlier read still runs and
    /// nothing is kept, or when this one ran past its bound.
    async fn slow(&self) -> Option<SlowFacts> {
        let kept = {
            let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
            kept.as_ref()
                .map(|kept| (kept.at.elapsed() < SLOW_KEPT, kept.facts.clone()))
        };
        if let Some((true, facts)) = &kept {
            return Some(facts.clone());
        }
        let stale = kept.map(|(_, facts)| facts);
        if self.reading.swap(true, Ordering::AcqRel) {
            return stale;
        }
        let read = self.clone();
        let run = tokio::spawn(async move {
            let _done = ReadDone(Arc::clone(&read.reading));
            let facts = read.read_slow().await;
            *read.kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(Kept {
                at: Instant::now(),
                facts: facts.clone(),
            });
            facts
        });
        match tokio::time::timeout(SLOW_BOUND, run).await {
            Ok(Ok(facts)) => Some(facts),
            Ok(Err(error)) => {
                tracing::warn!(%error, "problems: the folder checks did not finish");
                stale
            }
            Err(_) => stale,
        }
    }

    async fn read_slow(&self) -> SlowFacts {
        let state = self.import_folder.status().await.state;
        let import_folder = matches!(
            state,
            ImportFolderState::SettingsUnreadable
                | ImportFolderState::Unreachable
                | ImportFolderState::ReadOnly
                | ImportFolderState::Overlaps
        )
        .then_some(state);
        let (settings, management, backups) = (
            self.settings.clone(),
            self.management.clone(),
            Arc::clone(&self.backups),
        );
        let blocking = tokio::task::spawn_blocking(move || {
            let bin = settings
                .recycle_bin_warning_blocking()
                .map(|warning| warning.accept_folder);
            let needs_attention = management
                .recovery_diagnostics()
                .map(|diagnostics| diagnostics.needs_attention_count)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "problems: the publish journal was not read");
                    0
                });
            let newest_nightly_at = backups.newest_scheduled_at().unwrap_or_else(|error| {
                tracing::warn!(%error, "problems: the backups folder was not read");
                None
            });
            (bin, needs_attention, newest_nightly_at)
        })
        .await;
        match blocking {
            Ok((bin, needs_attention, newest_nightly_at)) => SlowFacts {
                bin,
                import_folder,
                needs_attention,
                newest_nightly_at,
            },
            Err(error) => {
                tracing::warn!(%error, "problems: the folder checks failed");
                SlowFacts {
                    import_folder,
                    ..SlowFacts::default()
                }
            }
        }
    }
}

/// Clears the reading flag when the folder checks end or panic.
struct ReadDone(Arc<AtomicBool>);

impl Drop for ReadDone {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// A folder role as the list names it.
fn role_name(role: FolderRole) -> &'static str {
    match role {
        FolderRole::Data => "data",
        FolderRole::Config => "config",
        FolderRole::Temp => "temp",
        FolderRole::Imports => "imports",
        FolderRole::Library => "library",
        FolderRole::RecycleBin => "recycle_bin",
        FolderRole::Downloads => "downloads",
        FolderRole::ImportFolder => "import_folder",
    }
}

/// The import folder's state as the list names it.
fn import_state_name(state: ImportFolderState) -> &'static str {
    match state {
        ImportFolderState::SettingsUnreadable => "settings_unreadable",
        ImportFolderState::Unreachable => "unreachable",
        ImportFolderState::ReadOnly => "read_only",
        ImportFolderState::Overlaps => "overlaps",
        ImportFolderState::Off => "off",
        ImportFolderState::Watching => "watching",
        ImportFolderState::Importing => "importing",
        ImportFolderState::WaitingForScan => "waiting_for_scan",
        ImportFolderState::Retrying => "retrying",
        ImportFolderState::LibraryOff => "library_off",
    }
}

/// What is wrong right now, broken first. Memory and local reads only;
/// folder checks that do not answer within two seconds are left out.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/admin/problems",
    responses(
        (status = 200, description = "What is wrong now", body = ProblemsResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_problems(
    State(problems): State<Problems>,
    _caller: Admin,
) -> Json<ProblemsResponse> {
    Json(problems.list().await)
}

/// The problems route.
pub fn routes() -> Routes<Problems> {
    Routes::new().admin(routes!(list_problems))
}
