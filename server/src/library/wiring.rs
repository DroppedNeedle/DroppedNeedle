//! Library bundle: one setup the app mounts, following the stage-6 shape.
//!
//! [`LibrarySetup`] owns the scan coordinator, the identify service
//! and queue, the contribution service and worker, and the publish
//! cell, plus the HTTP state (preview seals) and the background
//! loops. `build` binds everything over the production provider
//! clients; `for_tests` binds the same shape over scripted providers
//! and memory stores. The routers nest under `/api/v3` inside the
//! deny-by-default session gate.
//!
//! Store durability follows the stage-6 precedent: scan state runs
//! on SQLite over the application database, while identify and
//! contribution state run on the slices' memory stores (durable
//! SQLite ports are a later persistence tier, as are durable root
//! persistence, the rolling schedule settings, and the AcoustID key
//! config). Publish journals, snapshots, baselines, and the catalog
//! shadow run on a dedicated rusqlite database under the primary
//! root with idempotent schema.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{Router, extract::Request, middleware::Next, response::Response};
use tokio::sync::watch;

use super::adapters::{
    EmptyContributionIdentity, FsSpaceProbe, IdentifyEnqueue, LoftyTagReader,
    MinimalAttachmentEvidence, NoopContributionCatalog, TrackAlbumMap, UnavailableMusicBrainz,
};
use super::contrib::memory::MemoryStore as ContribMemoryStore;
use super::contrib::seams::SystemClock as ContribSystemClock;
use super::contrib::service::ContributionService;
use super::contrib::worker::{VerificationWorker, VerificationWorkerConfig};
use super::http::error::LibraryError;
use super::identify::memory::{
    MemoryAliasStore, MemoryIdentityStore, MemoryPinStore, MemoryProofStore, MemoryQueueStore,
    MemoryReviewStore,
};
use super::identify::models::{
    AlbumIdentity, IdentifyJob, IdentifyKind, LocalAlbumFacts, LocalTrackFacts,
};
use super::identify::providers::FakeProviders;
use super::identify::service::{IdentifyDeps, IdentifyService};
use super::identify::stores::QueueStore;
use super::publish::planner::{
    Capability, CollisionGate, DiskPreflight, FileFingerprint, PlanBundle, PlanItem, PlanKind,
    ReleaseIdentity, SealRecheck, SealedPreview,
};
use super::publish::publisher::{PublishOutcome, Publisher, SqliteCatalog};
use super::publish::snapshots::{BaselineStore, BlobStore, SnapshotStore, sha256_hex};
use super::publish::tags_seam::TagDocument;
use super::publish::{PublishError, Sandbox};
use super::scan::coordinator::{
    LibraryScanCoordinator, ResolverSource, ScanEventPublisher, SharedResolver,
};
use super::scan::fs::{FsCoordinator, WriteGuard};
use super::scan::models::{
    EffectivePolicy, ScanInventoryItem, ScanKind, ScanRequest, ScanRequestResult, ScanRun,
    ScanScope, ScanTrigger,
};
use super::scan::pool::BlockingPool;
use super::scan::roots::{LibraryRoot, RootRegistry, fingerprint_roots};
use super::scan::scheduler::ScheduleSettings;
use super::scan::sqlite_store::SqliteScanStore;
use super::scan::store::ScanStore;
use super::scan::supervisor::SupervisorInputs;
use super::scan::supervisor::{startup_recovery, supervise_once, supervise_once_with_shutdown};
use super::scan::watcher::{
    DirtyScopes, WatcherAction, WatcherSettings, WatcherState, WorkWakeups, clear_pending,
    watcher_request,
};
use crate::auth::users::UsersDeps;
use crate::auth::users::stores::StoreError;
use crate::ids::IdGenerator;

/// Scan coordinator over the wired seams.
pub type ScanCoordinator = LibraryScanCoordinator<SqliteScanStore, LoftyTagReader, IdentifyEnqueue>;

/// Live root-registry source shared with the stream gateway.
pub type RootSource = Arc<dyn Fn() -> RootRegistry + Send + Sync>;

/// Scan supervisor idle ceiling. The slice's 47s recovery ceiling
/// cannot wait on shutdown, so the wired loop re-checks the watch
/// every 5s; operator-visible behavior is identical.
const SUPERVISOR_IDLE_CEILING: Duration = Duration::from_secs(5);

/// Identify queue poll cadence.
const IDENTIFY_POLL: Duration = Duration::from_secs(2);

/// Publish maintenance cadence (snapshot purge plus preview sweep).
const PUBLISH_MAINTENANCE: Duration = Duration::from_secs(3600);

/// Preview seals live until the next unix day.
const PREVIEW_TTL_DAYS: i64 = 1;

/// Staged-bytes headroom over the source size for disk preflight.
const STAGED_HEADROOM_BYTES: u64 = 65_536;

/// Profile/naming/settings revisions before those slices land. All
/// pinned at 1 so seal rechecks compare honestly against constants.
const PINNED_REVISION: u64 = 1;

/// Override revision before the override slice lands.
const PINNED_OVERRIDE: u64 = 0;

/// Current unix time in milliseconds.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_millis() as u64)
        .unwrap_or(0)
}

/// Current unix time in seconds.
fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// Current unix day.
fn today_day() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| (span.as_secs() / 86_400) as i64)
        .unwrap_or(0)
}

/// Policy revision as the seal's u64: the registry fingerprint is
/// 16 hex chars by construction.
fn policy_revision_u64(registry: &RootRegistry) -> u64 {
    u64::from_str_radix(registry.policy_revision(), 16).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Publish cell: one publisher over the live sandbox, reopened on change.
// ---------------------------------------------------------------------------

/// One sealed preview awaiting apply.
pub struct PreviewEntry {
    sealed: SealedPreview,
    docs: BTreeMap<String, TagDocument>,
}

/// Open publisher plus the sandbox it runs under.
struct OpenPublish {
    sandbox: Sandbox,
    publisher: Publisher<SqliteCatalog, FsSpaceProbe>,
    root_ids: Vec<String>,
}

/// Publish state. Empty until the first usable root is configured;
/// reopened (with reconciliation) whenever the usable root set
/// changes. One mutex serializes publishes against reopens.
pub struct PublishCell {
    cell: Option<OpenPublish>,
}

impl PublishCell {
    fn empty() -> Self {
        Self { cell: None }
    }

    fn root_dirs(registry: &RootRegistry) -> Vec<super::publish::paths::Root> {
        registry
            .roots()
            .iter()
            .filter(|root| root.policy != EffectivePolicy::Excluded)
            .map(|root| super::publish::paths::Root {
                id: root.id.clone(),
                dir: root.path.clone(),
            })
            .collect()
    }

    /// Reconcile and (re)open when the usable root set changed.
    /// Returns the bundle recoveries from the reconcile, if any.
    fn refresh(
        &mut self,
        registry: &RootRegistry,
        space_roots: &RootDirs,
    ) -> Result<Vec<super::publish::recovery::BundleRecovery>, PublishError> {
        let roots = Self::root_dirs(registry);
        let ids: Vec<String> = roots.iter().map(|root| root.id.clone()).collect();
        if roots.is_empty() {
            self.cell = None;
            return Ok(Vec::new());
        }
        if let Some(open) = self.cell.as_ref()
            && open.root_ids == ids
        {
            return Ok(Vec::new());
        }
        let primary = roots[0].dir.clone();
        let meta_dir = primary.join(format!("{}meta", super::publish::HIDDEN_PREFIX));
        let sandbox = Sandbox::new(roots, meta_dir)?;
        let db_path = sandbox.meta_dir().join("publish.db");
        // Reconcile through a short-lived connection first: the
        // publisher owns its connection privately, so recovery runs
        // before it opens. Schema is idempotent on both opens.
        let recoveries = {
            if let Some(parent) = db_path.parent() {
                std::fs::create_dir_all(parent).map_err(PublishError::from)?;
            }
            let mut conn = rusqlite::Connection::open(&db_path).map_err(PublishError::from)?;
            super::publish::journal::apply_schema(&conn)?;
            super::publish::recovery::reconcile(&mut conn, &sandbox, &SqliteCatalog)?
        };
        let publisher = Publisher::open(
            sandbox.clone(),
            db_path,
            SqliteCatalog,
            FsSpaceProbe::new(space_roots.clone()),
            today_day(),
        )?;
        self.cell = Some(OpenPublish {
            sandbox,
            publisher,
            root_ids: ids,
        });
        Ok(recoveries)
    }

    fn open(&mut self) -> Result<&mut OpenPublish, PublishError> {
        self.cell
            .as_mut()
            .ok_or_else(|| PublishError::Journal("no usable library root is configured".into()))
    }

    /// Root ids a refresh would open under.
    fn pending_ids(registry: &RootRegistry) -> Vec<String> {
        Self::root_dirs(registry)
            .iter()
            .map(|root| root.id.clone())
            .collect()
    }

    /// True when a refresh would reopen (and reconcile) under
    /// `registry`: exactly the case where reconcile can write.
    fn needs_refresh(&self, registry: &RootRegistry) -> bool {
        let ids = Self::pending_ids(registry);
        if ids.is_empty() {
            return false;
        }
        self.cell.as_ref().map(|open| &open.root_ids) != Some(&ids)
    }
}

/// Shared root-directory lookup for the space probe.
type RootDirs = Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>;

/// Startup recovery report.
#[derive(Debug, Clone, Default)]
pub struct LibraryRecovery {
    /// Per-bundle publish recovery actions.
    pub publish_recoveries: Vec<(String, String)>,
    /// Contribution leases recovered.
    pub contrib_recovered: u64,
}

// ---------------------------------------------------------------------------
// Bundle.
// ---------------------------------------------------------------------------

/// Everything `create_app` needs to mount the library engine, built once.
#[derive(Clone)]
pub struct LibrarySetup {
    /// User store for principal translation.
    pub users: UsersDeps,
    /// Id generator for jobs, runs, and preview tokens.
    pub ids: Arc<dyn IdGenerator>,
    /// Shared root registry (settings saves swap it in place).
    pub registry: Arc<SharedResolver>,
    /// Scan store.
    pub scan_store: Arc<SqliteScanStore>,
    /// Scan coordinator over the wired seams.
    pub coordinator: Arc<ScanCoordinator>,
    /// Filesystem leases shared by scan (read) and publish (write).
    /// Every publish holds the write guard across its refresh plus
    /// its commit so scans never interleave with managed writes.
    pub fs: FsCoordinator,
    /// Blocking pool shared by scan and the watcher.
    pub pool: BlockingPool,
    /// Dirty scope marks (Hook B).
    pub dirty: DirtyScopes,
    /// Work wakeups shared by the supervisor and the watcher.
    pub wakeups: WorkWakeups,
    /// Track-to-album join fed by scan enqueue.
    pub track_albums: Arc<TrackAlbumMap>,
    /// Identify stores.
    pub identities: Arc<MemoryIdentityStore>,
    /// Identify queue store.
    pub identify_queue: Arc<MemoryQueueStore>,
    /// Review store.
    pub reviews: Arc<MemoryReviewStore>,
    /// Identify service.
    pub identify: Arc<IdentifyService>,
    /// Scripted providers (test bundles only).
    pub test_providers: Option<Arc<FakeProviders>>,
    /// Contribution service.
    pub contrib: Arc<ContributionService>,
    /// Contribution verification worker.
    pub contrib_worker: Arc<VerificationWorker>,
    /// Publish cell (empty until a usable root exists).
    pub publish: Arc<std::sync::Mutex<PublishCell>>,
    /// Sealed previews awaiting apply, keyed by token hash.
    pub previews: Arc<std::sync::Mutex<HashMap<String, PreviewEntry>>>,
    /// Watcher state across ticks.
    pub watcher_state: Arc<std::sync::Mutex<WatcherState>>,
    /// Shared root-directory lookup.
    pub root_dirs: RootDirs,
}

impl LibrarySetup {
    /// Build the production bundle over live stage-5 provider clients.
    /// MusicBrainz recall runs identity-critical; AcoustID support
    /// evidence runs without a key until the key config lands (the
    /// client answers `Missing` on an empty key, never dialing out).
    pub fn build(
        users: UsersDeps,
        http: reqwest::Client,
        ids: Arc<dyn IdGenerator>,
        providers: Arc<crate::providers::Providers>,
        db_path: &Path,
    ) -> Result<Self, String> {
        use crate::providers::acoustid::{AcoustIdClient, DEFAULT_BASE_URL};
        use crate::providers::adapters::{CorePacer, CoreSink};
        use crate::providers::musicbrainz::{MusicBrainzClient, ReqwestMbTransport};

        let pacer = CorePacer::for_source(providers, "acoustid")
            .ok_or_else(|| "acoustid has no verified rate row".to_owned())?;
        let musicbrainz =
            MusicBrainzClient::official(ReqwestMbTransport::build()?).with_sink(CoreSink);
        let acoustid = AcoustIdClient::new(http, DEFAULT_BASE_URL, pacer, CoreSink);
        let live =
            super::identify::providers::LiveProviders::new(musicbrainz, acoustid, String::new());
        let scan_store = Arc::new(
            SqliteScanStore::open(db_path).map_err(|error| format!("scan store: {error}"))?,
        );
        Self::assemble(
            users,
            ids,
            Arc::new(live) as Arc<dyn super::identify::providers::IdentifyProviders>,
            None,
            scan_store,
        )
    }

    /// Test bundle over scripted providers and memory stores. The
    /// scan store is an ephemeral SQLite database and the publish
    /// cell opens under the first added root, so sandbox-only tests
    /// stay hermetic.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(users: UsersDeps, ids: Arc<dyn IdGenerator>) -> Result<Self, String> {
        let scripted = Arc::new(FakeProviders::default());
        let scan_store = Arc::new(
            SqliteScanStore::open_ephemeral().map_err(|error| format!("scan store: {error}"))?,
        );
        Self::assemble(
            users,
            ids,
            scripted.clone() as Arc<dyn super::identify::providers::IdentifyProviders>,
            Some(scripted),
            scan_store,
        )
    }

    /// Test bundle over caller-supplied identify providers. Focused
    /// shutdown briefs script provider timing here; the shared
    /// `for_tests` shape stays the default everywhere else.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests_with_providers(
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        providers: Arc<dyn super::identify::providers::IdentifyProviders>,
    ) -> Result<Self, String> {
        let scan_store = Arc::new(
            SqliteScanStore::open_ephemeral().map_err(|error| format!("scan store: {error}"))?,
        );
        Self::assemble(users, ids, providers, None, scan_store)
    }

    fn assemble(
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        providers: Arc<dyn super::identify::providers::IdentifyProviders>,
        test_providers: Option<Arc<FakeProviders>>,
        scan_store: Arc<SqliteScanStore>,
    ) -> Result<Self, String> {
        let registry: Arc<SharedResolver> = Arc::new(SharedResolver::default());
        let root_dirs: RootDirs = {
            let registry = registry.clone();
            Arc::new(move || registry.resolver().registry().root_paths())
        };
        let track_albums = Arc::new(TrackAlbumMap::new());
        let identify_queue = Arc::new(MemoryQueueStore::default());
        let enqueue = Arc::new(IdentifyEnqueue::new(
            identify_queue.clone(),
            track_albums.clone(),
            ids.clone(),
        ));
        let pool = BlockingPool::new(4);
        let wakeups = WorkWakeups::new();
        let fs = FsCoordinator::new();
        let coordinator = Arc::new(
            ScanCoordinator::new(
                scan_store.clone(),
                pool.clone(),
                Arc::new(LoftyTagReader),
                enqueue,
                registry.clone(),
            )
            .with_filesystem(fs.clone())
            .with_wakeups(wakeups.clone())
            .with_events(ScanEventPublisher::new(Arc::new(|event| {
                tracing::info!(
                    run_id = event.run_id,
                    event = event.event,
                    "library scan event"
                );
            }))),
        );
        let identities = Arc::new(MemoryIdentityStore::default());
        let proofs = Arc::new(MemoryProofStore::default());
        let aliases = Arc::new(MemoryAliasStore::default());
        let pins = Arc::new(MemoryPinStore::default());
        let reviews = Arc::new(MemoryReviewStore::default());
        let identify = Arc::new(IdentifyService::new(IdentifyDeps {
            identities: identities.clone(),
            proofs,
            aliases,
            pins,
            queue: identify_queue.clone(),
            reviews: reviews.clone(),
            providers,
        }));
        let contrib_store: Arc<ContribMemoryStore> = Arc::new(ContribMemoryStore::new());
        let contrib_identity = Arc::new(EmptyContributionIdentity);
        let contrib = Arc::new(
            ContributionService::new(
                contrib_store,
                contrib_identity.clone(),
                Arc::new(MinimalAttachmentEvidence),
                Arc::new(ContribSystemClock),
            )
            .with_catalog(Arc::new(NoopContributionCatalog)),
        );
        let contrib_worker = Arc::new(VerificationWorker::new(
            contrib.clone(),
            Arc::new(UnavailableMusicBrainz),
            contrib_identity,
            VerificationWorkerConfig::default(),
        ));
        Ok(Self {
            users,
            ids,
            registry,
            scan_store,
            coordinator,
            fs,
            pool,
            dirty: DirtyScopes::new(),
            wakeups,
            track_albums,
            identities,
            identify_queue,
            reviews,
            identify,
            test_providers,
            contrib,
            contrib_worker,
            publish: Arc::new(std::sync::Mutex::new(PublishCell::empty())),
            previews: Arc::new(std::sync::Mutex::new(HashMap::new())),
            watcher_state: Arc::new(std::sync::Mutex::new(WatcherState::new())),
            root_dirs,
        })
    }

    /// Live root registry (re-read, never cached).
    pub fn live_registry(&self) -> RootRegistry {
        self.registry.resolver().registry().clone()
    }

    /// Root source the stream gateway resolves local reads against.
    pub fn root_source(&self) -> RootSource {
        let registry = self.registry.clone();
        Arc::new(move || registry.resolver().registry().clone())
    }

    /// Blocking write guards over every usable root in sorted order.
    /// The caller holds them across its publish-cell refresh plus
    /// its publish so scans never interleave with managed writes.
    /// Blocking: callers run off the async runtime.
    fn publish_guards(&self, registry: &RootRegistry) -> Vec<WriteGuard> {
        let mut ids = PublishCell::pending_ids(registry);
        ids.sort();
        ids.into_iter()
            .map(|id| self.fs.blocking_write(&id))
            .collect()
    }

    /// Async form of [`publish_guards`](Self::publish_guards) for
    /// callers already on the runtime.
    async fn publish_guards_async(&self, registry: &RootRegistry) -> Vec<WriteGuard> {
        let mut ids = PublishCell::pending_ids(registry);
        ids.sort();
        let mut guards = Vec::with_capacity(ids.len());
        for id in ids {
            guards.push(self.fs.write(&id).await);
        }
        guards
    }

    /// Relative-path routers for nesting under `/api/v3` inside the
    /// session gate. The leg carries the principal-translation layer
    /// so handlers keep their `Principal` extractor.
    pub fn gated_router(&self) -> Router {
        super::http::routes::library_router(self.clone()).layer(
            axum::middleware::from_fn_with_state(self.users.clone(), translate_principal),
        )
    }

    /// Supervisor inputs, rebuilt per call so settings-swap rebuilds
    /// never strand the loop on stale getters.
    fn supervisor_inputs(&self) -> SupervisorInputs {
        SupervisorInputs {
            root_paths: self.root_dirs.clone(),
            schedule: Arc::new(ScheduleSettings::manual),
            inclusion_rules: Arc::new(Vec::new),
            dirty: self.dirty.clone(),
            wakeups: self.wakeups.clone(),
            now_unix: Arc::new(now_unix),
        }
    }

    /// Watcher settings before the settings slice lands.
    fn watcher_settings(&self) -> WatcherSettings {
        WatcherSettings {
            enabled: true,
            poll_interval_seconds: 30.0,
            batch_window_seconds: 60.0,
        }
    }

    /// Startup recovery: publish reconcile (resume-or-compensate,
    /// never half-apply) plus contribution lease recovery. Scan
    /// recovery runs in the supervisor preamble. Re-running after a
    /// clean shutdown is a no-op.
    pub async fn run_recovery(&self) -> Result<LibraryRecovery, String> {
        let registry = self.live_registry();
        // Startup is uncontended, so the guards cost nothing; taken
        // first so reconcile can never race a scan.
        let _guards = self.publish_guards_async(&registry).await;
        let recoveries = {
            let mut cell = self
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cell.refresh(&registry, &self.root_dirs)
                .map_err(|error| format!("publish recovery: {error}"))?
        };
        let recovered = self.contrib_worker.recover(now_unix()).await;
        Ok(LibraryRecovery {
            publish_recoveries: recoveries
                .into_iter()
                .map(|recovery| (recovery.bundle_id, format!("{:?}", recovery.action)))
                .collect(),
            contrib_recovered: recovered,
        })
    }

    /// Spawn the library background loops over one shutdown watch.
    /// Returns named handles the caller awaits after serve.
    pub fn spawn_loops(
        &self,
        shutdown: watch::Receiver<bool>,
    ) -> Vec<(&'static str, tokio::task::JoinHandle<()>)> {
        vec![
            (
                "library-scan",
                tokio::spawn(scan_loop(self.clone(), shutdown.clone())),
            ),
            (
                "library-watcher",
                tokio::spawn(watcher_loop(self.clone(), shutdown.clone())),
            ),
            (
                "library-identify",
                tokio::spawn(identify_loop(self.clone(), shutdown.clone())),
            ),
            (
                "library-contrib",
                super::contrib::worker::spawn_verification_worker(
                    self.contrib_worker.clone(),
                    shutdown.clone(),
                ),
            ),
            (
                "library-publish",
                tokio::spawn(publish_loop(self.clone(), shutdown)),
            ),
        ]
    }

    /// One-shot scan startup reconciliation (Hook A). The loop runs
    /// this in its preamble; tests drive it directly.
    pub async fn scan_startup_recovery(&self) {
        startup_recovery(&self.coordinator, &self.supervisor_inputs()).await;
    }

    /// One supervisor iteration. Returns true when a run was driven.
    pub async fn supervisor_tick(&self) -> bool {
        supervise_once(&self.coordinator, &self.supervisor_inputs()).await
    }

    /// One shutdown-aware supervisor iteration: same Hook B, schedule,
    /// and worker semantics as [`supervisor_tick`](Self::supervisor_tick),
    /// but a signalled shutdown stops the in-flight scan instead of
    /// waiting it out. The stop goes through the regular control latch,
    /// so the walk and index checkpoints settle the run to cancelled
    /// on their next check and the next start resumes cleanly. A
    /// pre-signalled shutdown claims no new work.
    pub async fn supervisor_tick_with_shutdown(&self, shutdown: &watch::Receiver<bool>) -> bool {
        supervise_once_with_shutdown(&self.coordinator, &self.supervisor_inputs(), shutdown).await
    }

    /// One watcher tick over the persistent watcher state.
    pub async fn watcher_tick(&self) -> WatcherAction {
        // The state swaps out and back so no mutex guard crosses the
        // snapshot await.
        let mut state = {
            let mut guard = self
                .watcher_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::replace(&mut *guard, WatcherState::new())
        };
        let settings = self.watcher_settings();
        let registry = self.live_registry();
        let action = super::scan::watcher::poll_once(
            &mut state,
            &settings,
            &registry,
            &(self.root_dirs)(),
            &self.pool,
            now_unix(),
        )
        .await;
        if matches!(action, WatcherAction::Due)
            && let Some(request) = watcher_request(&registry, &[])
        {
            match self.coordinator.request_run(&request) {
                Ok(result) => {
                    tracing::info!(
                        disposition = ?result.disposition,
                        "filesystem watcher requested incremental scan"
                    );
                    clear_pending(&mut state);
                    self.wakeups.notify("scan");
                }
                Err(error) => {
                    tracing::warn!(%error, "watcher scan request failed");
                    clear_pending(&mut state);
                }
            }
        }
        {
            let mut guard = self
                .watcher_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = state;
        }
        action
    }

    /// One identify tick: claim every due job, fill missing facts
    /// from the scan catalog plus disk tag reads, and run each
    /// attempt. Returns jobs attempted.
    pub async fn identify_tick(&self) -> usize {
        // No signal: the sender stays alive so the watch never fires
        // and the drain runs exactly as before.
        let (_live, quiet) = watch::channel(false);
        self.identify_tick_with_shutdown(&quiet).await
    }

    /// Shutdown-aware drain: same claim order and per-attempt
    /// semantics as [`identify_tick`](Self::identify_tick), but a
    /// signalled shutdown abandons the drain instead of pacing out
    /// the whole queue at one MusicBrainz gate slot per attempt. The
    /// signal is checked before each claim, before each gated
    /// attempt, and across the attempt itself, so SIGTERM mid-drain
    /// yields promptly. Claimed-but-unfinished jobs stay Running in
    /// memory; a restart clears them, same as any mid-drain crash
    /// today.
    pub async fn identify_tick_with_shutdown(&self, shutdown: &watch::Receiver<bool>) -> usize {
        let mut attempted = 0;
        let mut shutdown = shutdown.clone();
        loop {
            if *shutdown.borrow() {
                break;
            }
            let claimed = self
                .identify_queue
                .claim(now_ms(), super::identify::queue::LEASE_SECONDS * 1000);
            let Some(job) = claimed else { break };
            self.fill_facts(&job).await;
            if *shutdown.borrow() {
                break;
            }
            let attempt = self.identify.run_claimed_job(&job.id, now_ms());
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                report = attempt => match report {
                    Some(report) => {
                        attempted += 1;
                        tracing::info!(
                            job_id = report.job.id,
                            outcome = ?report.outcome,
                            reason = report.reason_code,
                            "identify attempt finished"
                        );
                    }
                    None => {
                        tracing::warn!(job_id = job.id, "identify job vanished mid-claim");
                    }
                },
            }
        }
        attempted
    }

    /// One publish maintenance tick: purge expired operation
    /// snapshots plus expired preview seals. Returns snapshots purged.
    pub fn publish_tick(&self) -> Result<usize, PublishError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        // Reconcile-on-reopen can resume renames; a no-op refresh
        // takes no guards and bumps no revisions.
        let _guards = if cell.needs_refresh(&registry) {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)?;
        let purged = match cell.cell.as_mut() {
            Some(open) => {
                SnapshotStore::new(open.publisher.connection()).purge_expired(today_day())?
            }
            None => 0,
        };
        drop(cell);
        self.sweep_previews();
        Ok(purged)
    }

    /// Fill missing album facts from the scan catalog plus live disk
    /// tag reads. Scan keys read `root::directory`; anything else
    /// keeps its seeded facts (HTTP manual enqueue always seeds).
    /// Albums with no surviving files seed empty facts so the job
    /// lands an honest terminal outcome instead of sticking.
    ///
    /// Tag reads and probes ride the blocking pool: a full decode on
    /// a slow disk must never stall the async runtime.
    async fn fill_facts(&self, job: &IdentifyJob) {
        use super::identify::stores::IdentityStore;

        if self.identities.album_facts(&job.local_album_id).is_some() {
            return;
        }
        let Some((root_id, parent)) = job.local_album_id.split_once("::") else {
            return;
        };
        let dirs = (self.root_dirs)();
        let Some(root_dir) = dirs.get(root_id) else {
            self.identities.save_album_facts(LocalAlbumFacts {
                local_album_id: job.local_album_id.clone(),
                ..LocalAlbumFacts::default()
            });
            return;
        };
        let prefix = if parent == "." {
            String::new()
        } else {
            format!("{parent}/")
        };
        // Memory-only fan-out first; every disk read below rides the pool.
        let wanted: Vec<(String, String)> = self
            .scan_store
            .catalog_entries(root_id)
            .into_iter()
            .filter(|(relative_path, _)| relative_path.starts_with(&prefix))
            .map(|(relative_path, entry)| (relative_path, entry.track_id))
            .collect();
        let mut tracks = Vec::new();
        for (relative_path, track_id) in wanted {
            let file = root_dir.join(&relative_path);
            let pool = self.pool.clone();
            let facts = pool.run(move || read_track_facts(&file)).await;
            tracks.push(LocalTrackFacts {
                local_track_id: track_id,
                title: facts.title,
                artist_name: facts.artist,
                track_number: facts.track_number,
                disc_number: facts.disc_number,
                duration_secs: facts.duration_secs,
                recording_mbid: facts.recording,
                release_track_mbid: facts.release_track,
                release_mbid: facts.release,
                release_group_mbid: facts.group,
                fingerprint: None,
            });
        }
        // Album title and artist from the first tagged track; empty
        // when nothing survived, which still terminates honestly.
        let first_tagged = tracks
            .iter()
            .find(|track| !track.title.is_empty())
            .map(|track| (track.local_track_id.clone(), track.artist_name.clone()));
        let (title, artist) = match first_tagged {
            Some((track_id, artist)) => {
                let file =
                    root_dir.join(self.track_relative(root_id, &track_id).unwrap_or_default());
                let pool = self.pool.clone();
                let album = pool
                    .run(move || {
                        super::tags::format_for_path(&file)
                            .ok()
                            .and_then(|format| super::tags::read::read_tag_only(&file, format).ok())
                            .map(|tag| tag.album)
                            .unwrap_or_default()
                    })
                    .await;
                (album, artist)
            }
            None => (String::new(), String::new()),
        };
        self.identities.save_album_facts(LocalAlbumFacts {
            local_album_id: job.local_album_id.clone(),
            title,
            album_artist_name: artist,
            tracks,
            locked_track_ids: Vec::new(),
            is_compilation: false,
        });
    }

    /// Relative path for one catalog track id, if still present.
    fn track_relative(&self, root_id: &str, track_id: &str) -> Option<String> {
        self.scan_store
            .catalog_entries(root_id)
            .into_iter()
            .find(|(_, entry)| entry.track_id == track_id)
            .map(|(relative_path, _)| relative_path)
    }

    /// Drop expired preview seals.
    fn sweep_previews(&self) {
        let today = today_day();
        let mut previews = self
            .previews
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        previews.retain(|_, entry| entry.sealed.expires_day > today);
    }
}

/// Disk facts for one catalog file: tag-only read plus probe.
/// Blocking (a probe fully decodes); always runs on the pool.
struct DiskTrackFacts {
    title: String,
    artist: String,
    track_number: u32,
    disc_number: u32,
    duration_secs: Option<u64>,
    recording: Option<String>,
    release_track: Option<String>,
    release: Option<String>,
    group: Option<String>,
}

fn read_track_facts(file: &std::path::Path) -> DiskTrackFacts {
    let (title, artist, track_number, disc_number, recording, release_track, release, group) =
        match super::tags::format_for_path(file)
            .ok()
            .and_then(|format| super::tags::read::read_tag_only(file, format).ok())
        {
            Some(tag) => (
                tag.title,
                tag.artist,
                tag.track_number,
                tag.disc_number,
                tag.musicbrainz_recording_id,
                tag.musicbrainz_release_track_id,
                tag.musicbrainz_release_id,
                tag.musicbrainz_release_group_id,
            ),
            None => (String::new(), String::new(), 0, 0, None, None, None, None),
        };
    let duration_secs = super::tags::probe(file)
        .ok()
        .map(|info| info.duration_seconds as u64);
    DiskTrackFacts {
        title,
        artist,
        track_number,
        disc_number,
        duration_secs,
        recording,
        release_track,
        release,
        group,
    }
}

// ---------------------------------------------------------------------------
// Background loops. Thin shutdown-aware shells over the tick methods.
// ---------------------------------------------------------------------------

/// Scan supervisor loop: Hook A preamble, then drive-until-idle with
/// a shutdown-checked ceiling. The tick itself is shutdown-aware, so a
/// SIGTERM landing mid-scan stops the run instead of waiting it out.
async fn scan_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    setup.scan_startup_recovery().await;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let revision = setup.wakeups.revision("scan");
        if setup.supervisor_tick_with_shutdown(&shutdown).await {
            continue;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = setup.wakeups.wait("scan", revision, SUPERVISOR_IDLE_CEILING) => {}
        }
    }
}

/// Filesystem watcher loop: snapshot, batch, request on due.
async fn watcher_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        let action = setup.watcher_tick().await;
        let sleep_secs = match action {
            WatcherAction::Idle { sleep_secs } | WatcherAction::Batching { sleep_secs } => {
                sleep_secs.max(0.0)
            }
            WatcherAction::Due => setup.watcher_settings().poll_interval_seconds.max(1.0),
        };
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(Duration::from_secs_f64(sleep_secs)) => {}
        }
    }
}

/// Identify queue loop: drain every due job each tick.
async fn identify_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        setup.identify_tick_with_shutdown(&shutdown).await;
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(IDENTIFY_POLL) => {}
        }
    }
}

/// Publish maintenance loop: snapshot purge plus preview sweep.
async fn publish_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(PUBLISH_MAINTENANCE) => {}
        }
        if *shutdown.borrow() {
            break;
        }
        // The tick is sync blocking work (mutexes, spin-guards, sqlite):
        // keep it off the async runtime.
        let tick_setup = setup.clone();
        match tokio::task::spawn_blocking(move || tick_setup.publish_tick()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "publish maintenance tick failed"),
            Err(error) => tracing::warn!(%error, "publish maintenance tick panicked"),
        }
    }
}

/// One file inside a management preview request (handler-mapped).
pub struct PreviewItemInput {
    /// Source root id.
    pub root_id: String,
    /// Source path relative to the root.
    pub rel_path: String,
    /// Destination rel path (organize only).
    pub dest_rel: Option<String>,
    /// Managed-field updates.
    pub managed_updates: BTreeMap<String, Vec<String>>,
}

/// One planned file inside a sealed preview.
pub struct PreviewFile {
    /// Stable local track id.
    pub track_id: String,
    /// Source `root/rel`.
    pub source: String,
    /// Destination `root/rel`.
    pub dest: String,
    /// `same_path` or `move`.
    pub kind: String,
}

/// Sealed preview awaiting apply.
pub struct PreviewSealed {
    /// Single-use confirmation token.
    pub token: String,
    /// Expiry day (unix day, exclusive).
    pub expires_day: i64,
    /// Bundle id the apply will publish under.
    pub bundle_id: String,
    /// Planned files.
    pub files: Vec<PreviewFile>,
}

/// One applied file.
pub struct AppliedFile {
    /// Stable local track id.
    pub track_id: String,
    /// Adopted root id.
    pub root_id: String,
    /// Adopted path relative to the root.
    pub rel_path: String,
}

/// Published bundle answer.
pub struct AppliedBundle {
    /// Published bundle id.
    pub bundle_id: String,
    /// `committed` or `cleanup_pending`.
    pub outcome: String,
    /// Applied files.
    pub files: Vec<AppliedFile>,
}

/// Map a publisher failure onto HTTP. Collisions, stale state, and
/// capability blocks are 409s (valid input against the wrong
/// state); unsafe paths are 400s; store and I/O faults are 5xx.
fn publish_error(error: PublishError) -> LibraryError {
    match error {
        PublishError::Collision(message)
        | PublishError::Validation(message)
        | PublishError::Snapshot(message)
        | PublishError::Journal(message)
        | PublishError::Catalog(message)
        | PublishError::Space(message)
        | PublishError::Capability(message)
        | PublishError::Cleanup(message) => LibraryError::Conflict { message },
        PublishError::UnsafePath(message) | PublishError::Archive(message) => {
            LibraryError::InvalidInput { message }
        }
        PublishError::CacheInvalidation(message)
        | PublishError::Store(message)
        | PublishError::Io(message)
        | PublishError::InjectedCrash(message) => LibraryError::internal(&message),
    }
}

impl LibrarySetup {
    /// Add a library root. The path must exist, be absolute, and be a
    /// directory; the id must be unused. Adding the first root
    /// enables the library and marks the root dirty so Hook B picks
    /// it up for an initial scan. Blocking file and database work:
    /// handlers run this off the async runtime.
    pub fn add_root(
        &self,
        id: Option<String>,
        path: String,
        policy: EffectivePolicy,
    ) -> Result<(LibraryRoot, String), LibraryError> {
        let dir = PathBuf::from(&path);
        if !dir.is_absolute() {
            return Err(LibraryError::InvalidInput {
                message: "Root path must be absolute".to_owned(),
            });
        }
        let meta = std::fs::symlink_metadata(&dir).map_err(|_| LibraryError::InvalidInput {
            message: "Root path does not exist".to_owned(),
        })?;
        if !meta.file_type().is_dir() {
            return Err(LibraryError::InvalidInput {
                message: "Root path is not a directory".to_owned(),
            });
        }
        let registry = self.live_registry();
        let id = id.unwrap_or_else(|| self.ids.new_id());
        if id.is_empty() || id.contains('/') || id.contains('\0') {
            return Err(LibraryError::InvalidInput {
                message: "Root id is not a plain name".to_owned(),
            });
        }
        if registry.resolve(&id).is_some() {
            return Err(LibraryError::Conflict {
                message: "Root id already exists".to_owned(),
            });
        }
        let root = LibraryRoot::new(&id, dir, policy);
        let mut roots = registry.roots().to_vec();
        roots.push(root.clone());
        let revision = fingerprint_roots(&roots, true);
        self.registry
            .update(RootRegistry::new(roots, true, &revision));
        self.dirty.mark(&id);
        self.wakeups.notify("scan");
        // Open (or reopen) the publish cell under the new root set.
        // A reconcile failure here is a 409: the root is registered
        // but publishing stays closed until recovery passes.
        {
            let mut cell = self
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let registry = self.live_registry();
            let _guards = if cell.needs_refresh(&registry) {
                self.publish_guards(&registry)
            } else {
                Vec::new()
            };
            cell.refresh(&registry, &self.root_dirs)
                .map_err(publish_error)?;
        }
        Ok((root, revision))
    }

    /// Request a manual scan over one root or every scheduled root.
    pub fn request_scan(
        &self,
        root_id: Option<&str>,
        user_id: &str,
    ) -> Result<ScanRequestResult, LibraryError> {
        let registry = self.live_registry();
        let mut scopes = registry.scheduled_root_scopes();
        if let Some(wanted) = root_id {
            scopes.retain(|scope| scope.root_id == wanted);
            if scopes.is_empty() {
                return Err(LibraryError::NotFound);
            }
        }
        let result = self
            .coordinator
            .request_run(&ScanRequest {
                kind: ScanKind::Incremental,
                trigger: ScanTrigger::Manual,
                scopes,
                requested_by_user_id: Some(user_id.to_owned()),
                policy_revision: registry.policy_revision().to_owned(),
            })
            .map_err(|error| match error {
                super::scan::coordinator::ScanRequestError::Disabled => LibraryError::Conflict {
                    message: error.to_string(),
                },
                super::scan::coordinator::ScanRequestError::EmptyScopes => {
                    LibraryError::InvalidInput {
                        message: error.to_string(),
                    }
                }
                super::scan::coordinator::ScanRequestError::StalePolicy
                | super::scan::coordinator::ScanRequestError::UnknownRoots => {
                    LibraryError::Conflict {
                        message: error.to_string(),
                    }
                }
                super::scan::coordinator::ScanRequestError::BadCursor => {
                    LibraryError::InvalidInput {
                        message: error.to_string(),
                    }
                }
            })?;
        Ok(result)
    }

    /// Run detail plus discovered files (bounded to the latest 500).
    pub fn run_detail(
        &self,
        run_id: &str,
    ) -> Result<(ScanRun, Vec<ScanScope>, Vec<ScanInventoryItem>), LibraryError> {
        let (run, scopes, _) = self
            .coordinator
            .snapshot(run_id)
            .map_err(|error| match error {
                super::scan::store::ScanStoreError::NotFound { .. } => LibraryError::NotFound,
                other => LibraryError::Conflict {
                    message: other.to_string(),
                },
            })?;
        let mut files = self.scan_store.inventory_for_run(run_id);
        if files.len() > 500 {
            files = files.split_off(files.len() - 500);
        }
        // Inventory rows freeze at discovery; track ids assign at
        // index time into the catalog, so join them for the view.
        let mut catalog: HashMap<(String, String), String> = HashMap::new();
        for scope in &scopes {
            for (relative_path, entry) in self.scan_store.catalog_entries(&scope.root_id) {
                catalog.insert(
                    (scope.root_id.clone(), relative_path),
                    entry.track_id.clone(),
                );
            }
        }
        for file in &mut files {
            if file.local_track_id.is_none() {
                file.local_track_id = catalog
                    .get(&(file.root_id.clone(), file.relative_path.clone()))
                    .cloned();
            }
        }
        Ok((run, scopes, files))
    }

    /// Enqueue one album for identification, seeding title/artist
    /// facts when the scan never saw the album.
    pub fn enqueue_identify(
        &self,
        album_id: &str,
        kind: IdentifyKind,
        title: Option<&str>,
        artist: Option<&str>,
        user_id: &str,
    ) -> IdentifyJob {
        use super::identify::stores::IdentityStore;

        if self.identities.album_facts(album_id).is_none() {
            self.identities.save_album_facts(LocalAlbumFacts {
                local_album_id: album_id.to_owned(),
                title: title.unwrap_or_default().to_owned(),
                album_artist_name: artist.unwrap_or_default().to_owned(),
                tracks: Vec::new(),
                locked_track_ids: Vec::new(),
                is_compilation: false,
            });
        }
        let job_id = self.ids.new_id();
        self.identify
            .enqueue_album(&job_id, album_id, kind, &job_id, Some(user_id), now_ms())
    }

    /// Approve a pending review with the curator's chosen candidate,
    /// sealing a manual identity. Unknown reviews and unknown
    /// candidates are 404s; settled reviews are 409s.
    pub fn approve_review(
        &self,
        review_id: &str,
        user_id: &str,
        candidate_key: &str,
    ) -> Result<(super::identify::models::ReviewItem, Option<AlbumIdentity>), LibraryError> {
        use super::identify::stores::{IdentityStore, ReviewStore};

        let Some(review) = self.reviews.get(review_id) else {
            return Err(LibraryError::NotFound);
        };
        if review.state != super::identify::models::ReviewState::Pending {
            return Err(LibraryError::Conflict {
                message: "Review is already settled".to_owned(),
            });
        }
        if !review
            .candidates
            .iter()
            .any(|candidate| candidate.candidate_key == candidate_key)
        {
            return Err(LibraryError::NotFound);
        }
        if !self
            .identify
            .approve_candidate(review_id, user_id, candidate_key)
        {
            return Err(LibraryError::Conflict {
                message: "Review could not be approved".to_owned(),
            });
        }
        let settled = self.reviews.get(review_id).unwrap_or(review);
        let identity = self.identities.album_identity(&settled.local_album_id);
        Ok((settled, identity))
    }

    /// Reject a pending review: the album keeps its tags, nothing seals.
    pub fn reject_review(
        &self,
        review_id: &str,
        user_id: &str,
    ) -> Result<super::identify::models::ReviewItem, LibraryError> {
        use super::identify::stores::ReviewStore;

        let Some(review) = self.reviews.get(review_id) else {
            return Err(LibraryError::NotFound);
        };
        if review.state != super::identify::models::ReviewState::Pending {
            return Err(LibraryError::Conflict {
                message: "Review is already settled".to_owned(),
            });
        }
        if !self.identify.reject_candidates(review_id, user_id) {
            return Err(LibraryError::Conflict {
                message: "Review could not be rejected".to_owned(),
            });
        }
        Ok(self.reviews.get(review_id).unwrap_or(review))
    }

    /// Album id for one track through the scan-fed join.
    fn track_album(&self, track_id: &str) -> Result<String, LibraryError> {
        self.track_albums
            .album_for_track(track_id)
            .ok_or_else(|| LibraryError::Conflict {
                message: format!("Track {track_id} was never scanned into an album"),
            })
    }

    /// Accepted exact identity for one track (D1: management needs an
    /// accepted exact MusicBrainz release plus a full track mapping).
    fn resolve_identity(
        &self,
        album_id: &str,
        track_id: &str,
    ) -> Result<ReleaseIdentity, LibraryError> {
        use super::identify::stores::IdentityStore;

        let missing = |message: String| LibraryError::Conflict { message };
        let album = self.identities.album_identity(album_id).ok_or_else(|| {
            missing(format!(
                "Album {album_id} has no accepted identity; identify it first"
            ))
        })?;
        let release_mbid = album.release_mbid.clone().ok_or_else(|| {
            missing(format!(
                "Album {album_id} has no accepted exact release; approve an exact edition"
            ))
        })?;
        let release_group_mbid = album
            .release_group_mbid
            .clone()
            .ok_or_else(|| missing(format!("Album {album_id} has no accepted release group")))?;
        let track = self
            .identities
            .track_identity(track_id)
            .ok_or_else(|| missing(format!("Track {track_id} has no accepted mapping")))?;
        let recording_mbid = track
            .recording_mbid
            .clone()
            .ok_or_else(|| missing(format!("Track {track_id} has an incomplete mapping")))?;
        let release_track_mbid = track
            .release_track_mbid
            .clone()
            .ok_or_else(|| missing(format!("Track {track_id} has an incomplete mapping")))?;
        Ok(ReleaseIdentity {
            release_mbid,
            release_group_mbid,
            recording_mbid,
            release_track_mbid,
            album_identity_revision: album.row_revision,
            mapping_revision: track.row_revision,
        })
    }

    /// Scan-assigned track id for one catalog file.
    fn scan_track(&self, root_id: &str, rel_path: &str) -> Result<String, LibraryError> {
        self.scan_store
            .catalog_entries(root_id)
            .into_iter()
            .find(|(relative_path, _)| relative_path == rel_path)
            .map(|(_, entry)| entry.track_id)
            .ok_or_else(|| LibraryError::Conflict {
                message: format!("{root_id}/{rel_path} is not indexed; scan the root first"),
            })
    }

    /// Live fingerprint for one sandbox file. Unreadable files are
    /// 409s: valid input against a moved file.
    fn live_fingerprint(
        sandbox: &Sandbox,
        root_id: &str,
        rel_path: &str,
    ) -> Result<FileFingerprint, LibraryError> {
        let path = sandbox
            .resolve_no_symlink(root_id, rel_path)
            .map_err(publish_error)?;
        let bytes =
            super::publish::paths::read_regular_file(&path).map_err(|error| match error {
                PublishError::Validation(message) => LibraryError::Conflict { message },
                other => publish_error(other),
            })?;
        Ok(FileFingerprint {
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        })
    }

    /// Current semantic tag document for one sandbox file.
    fn live_doc(
        sandbox: &Sandbox,
        root_id: &str,
        rel_path: &str,
    ) -> Result<TagDocument, LibraryError> {
        let path = sandbox
            .resolve_no_symlink(root_id, rel_path)
            .map_err(publish_error)?;
        super::publish::staging::document_from_file(&path).map_err(publish_error)
    }

    /// Lowercase container format for one rel path.
    fn live_format(rel_path: &str) -> Result<String, LibraryError> {
        super::tags::format_for_path(PathBuf::from(rel_path).as_path())
            .map(|format| format.as_str().to_owned())
            .map_err(|_| LibraryError::InvalidInput {
                message: format!("Unsupported audio format for {rel_path}"),
            })
    }

    /// Build and seal a management preview: retag writes in place,
    /// organize moves within the root. The preview dry-runs every
    /// apply gate (capability, collision, disk) so apply only fails
    /// on state that moved underneath. Blocking file and database
    /// work: handlers run this off the async runtime.
    pub fn plan_preview(
        &self,
        kind: PlanKind,
        album_id: &str,
        items: Vec<PreviewItemInput>,
    ) -> Result<PreviewSealed, LibraryError> {
        use super::identify::stores::IdentityStore as _;
        use super::publish::publisher::Catalog as _;

        if items.is_empty() {
            return Err(LibraryError::InvalidInput {
                message: "Preview needs at least one file".to_owned(),
            });
        }
        if items.len() > super::publish::MAX_PLAN_SUBJECTS {
            return Err(LibraryError::InvalidInput {
                message: "Preview exceeds the per-bundle file limit".to_owned(),
            });
        }
        let registry = self.live_registry();
        let (sandbox, catalog_revision) = {
            let mut cell = self
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _guards = if cell.needs_refresh(&registry) {
                self.publish_guards(&registry)
            } else {
                Vec::new()
            };
            cell.refresh(&registry, &self.root_dirs)
                .map_err(publish_error)?;
            let open = cell.open().map_err(publish_error)?;
            let revision = SqliteCatalog
                .revision(open.publisher.connection())
                .map_err(publish_error)?;
            (open.sandbox.clone(), revision)
        };
        // Accepted album identity first: without an exact release the
        // whole bundle blocks before any file is touched.
        let album =
            self.identities
                .album_identity(album_id)
                .ok_or_else(|| LibraryError::Conflict {
                    message: format!(
                        "Album {album_id} has no accepted identity; identify it first"
                    ),
                })?;
        if album.release_mbid.is_none() {
            return Err(LibraryError::Conflict {
                message: format!(
                    "Album {album_id} has no accepted exact release; approve an exact edition"
                ),
            });
        }
        let mut plan_items = Vec::with_capacity(items.len());
        let mut docs = BTreeMap::new();
        let mut files = Vec::with_capacity(items.len());
        for item in &items {
            if item.root_id.is_empty() || item.rel_path.is_empty() {
                return Err(LibraryError::InvalidInput {
                    message: "Preview items need a root and a relative path".to_owned(),
                });
            }
            super::publish::staging::check_managed_updates(&item.managed_updates)
                .map_err(publish_error)?;
            let track_id = self.scan_track(&item.root_id, &item.rel_path)?;
            let fingerprint = Self::live_fingerprint(&sandbox, &item.root_id, &item.rel_path)?;
            let format = Self::live_format(&item.rel_path)?;
            let identity = self.resolve_identity(album_id, &track_id)?;
            let (dest_root, dest_rel, item_kind) = match kind {
                PlanKind::SamePath => (
                    item.root_id.clone(),
                    item.rel_path.clone(),
                    PlanKind::SamePath,
                ),
                PlanKind::Move => {
                    let Some(dest_rel) = item.dest_rel.clone() else {
                        return Err(LibraryError::InvalidInput {
                            message: "Organize items need a destination path".to_owned(),
                        });
                    };
                    if dest_rel.is_empty() {
                        return Err(LibraryError::InvalidInput {
                            message: "Organize items need a destination path".to_owned(),
                        });
                    }
                    (item.root_id.clone(), dest_rel, PlanKind::Move)
                }
            };
            let mut capabilities = Vec::new();
            if !item.managed_updates.is_empty() {
                capabilities.push(Capability::Metadata);
            }
            if item_kind == PlanKind::Move {
                capabilities.push(Capability::SameRootMove);
            }
            let source = sandbox
                .resolve_no_symlink(&item.root_id, &item.rel_path)
                .map_err(publish_error)?;
            let size = std::fs::metadata(&source)
                .map(|meta| meta.len())
                .unwrap_or(fingerprint.size);
            plan_items.push(PlanItem {
                track_id: track_id.clone(),
                source_root: item.root_id.clone(),
                source_rel: item.rel_path.clone(),
                dest_root: dest_root.clone(),
                dest_rel: dest_rel.clone(),
                kind: item_kind,
                fingerprint,
                identity,
                override_revision: PINNED_OVERRIDE,
                capabilities,
                format,
                managed_updates: item.managed_updates.clone(),
                sidecars: Vec::new(),
                staged_bytes_estimate: size + STAGED_HEADROOM_BYTES,
            });
            docs.insert(
                track_id.clone(),
                Self::live_doc(&sandbox, &item.root_id, &item.rel_path)?,
            );
            files.push(PreviewFile {
                track_id,
                source: format!("{}/{}", item.root_id, item.rel_path),
                dest: format!("{dest_root}/{dest_rel}"),
                kind: match item_kind {
                    PlanKind::SamePath => "same_path".to_owned(),
                    PlanKind::Move => "move".to_owned(),
                },
            });
        }
        let bundle = PlanBundle {
            id: self.ids.new_id(),
            items: plan_items,
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision: policy_revision_u64(&registry),
            catalog_revision,
        };
        CollisionGate::check_bundle(&sandbox, &bundle).map_err(publish_error)?;
        DiskPreflight::check(&bundle, &FsSpaceProbe::new(self.root_dirs.clone()))
            .map_err(publish_error)?;
        let token = self.ids.new_id();
        let token_hash = sha256_hex(token.as_bytes());
        let expires_day = today_day() + PREVIEW_TTL_DAYS;
        let sealed = SealedPreview::seal(
            bundle.clone(),
            token_hash.clone(),
            PINNED_REVISION,
            expires_day,
        );
        {
            let mut previews = self
                .previews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            previews.insert(token_hash, PreviewEntry { sealed, docs });
        }
        Ok(PreviewSealed {
            token,
            expires_day,
            bundle_id: bundle.id,
            files,
        })
    }

    /// Apply a sealed preview exactly once. The write guards span
    /// the refresh plus the publish, so no scan interleaves with
    /// the commit. Blocking: handlers run this off the async runtime.
    pub fn apply_preview(&self, token: &str) -> Result<AppliedBundle, LibraryError> {
        use super::publish::publisher::Catalog as _;

        let token_hash = sha256_hex(token.as_bytes());
        let entry = {
            let mut previews = self
                .previews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            previews.remove(&token_hash).ok_or(LibraryError::NotFound)?
        };
        if today_day() >= entry.sealed.expires_day {
            return Err(LibraryError::Conflict {
                message: "Preview expired; plan it again".to_owned(),
            });
        }
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let live = self.seal_live(&open.sandbox, &entry.sealed.bundle, &token_hash)?;
        let catalog_revision = SqliteCatalog
            .revision(open.publisher.connection())
            .map_err(publish_error)?;
        let mut live = live;
        live.catalog_revision = catalog_revision;
        let outcome = open
            .publisher
            .publish(&entry.sealed, &live, &entry.docs)
            .map_err(publish_error)?;
        Ok(AppliedBundle {
            bundle_id: entry.sealed.bundle.id.clone(),
            outcome: match outcome {
                PublishOutcome::Committed => "committed".to_owned(),
                PublishOutcome::CleanupPending => "cleanup_pending".to_owned(),
            },
            files: entry
                .sealed
                .bundle
                .items
                .iter()
                .map(|item| AppliedFile {
                    track_id: item.track_id.clone(),
                    root_id: item.dest_root.clone(),
                    rel_path: item.dest_rel.clone(),
                })
                .collect(),
        })
    }

    /// Live seal recheck for one bundle: fresh fingerprints,
    /// identities, and revisions under the publish lock.
    fn seal_live(
        &self,
        sandbox: &Sandbox,
        bundle: &PlanBundle,
        token_hash: &str,
    ) -> Result<SealRecheck, LibraryError> {
        let mut fingerprints = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        for item in &bundle.items {
            fingerprints.insert(
                item.track_id.clone(),
                Self::live_fingerprint(sandbox, &item.source_root, &item.source_rel).map_err(
                    |_| LibraryError::Conflict {
                        message: format!("Track {} changed under the preview", item.track_id),
                    },
                )?,
            );
            let album_id = self.track_album(&item.track_id)?;
            identities.insert(
                item.track_id.clone(),
                self.resolve_identity(&album_id, &item.track_id)
                    .map_err(|_| LibraryError::Conflict {
                        message: format!("Identity for track {} changed", item.track_id),
                    })?,
            );
            overrides.insert(item.track_id.clone(), PINNED_OVERRIDE);
        }
        Ok(SealRecheck {
            fingerprints,
            identities,
            overrides,
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision: policy_revision_u64(&self.live_registry()),
            catalog_revision: bundle.catalog_revision,
            settings_revision: PINNED_REVISION,
            today_day: today_day(),
            token_hash: token_hash.to_owned(),
        })
    }

    /// Undo one published bundle as a new operation over the exact
    /// immediate before state. Blocked files (external edits, moves,
    /// identity drift, expired snapshots, occupied restores) 409 with
    /// per-file reasons; the bundle only writes when every sibling is
    /// eligible. The write guards span the refresh plus the
    /// restoration publish. Blocking: handlers run this off the
    /// async runtime.
    pub fn undo_bundle(&self, bundle_id: &str) -> Result<AppliedBundle, LibraryError> {
        use super::publish::publisher::Catalog as _;
        use super::publish::undo::{UndoInput, UndoLive, plan_undo};

        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let conn = open.publisher.connection();
        let source = super::publish::operations::load_operation(conn, bundle_id)
            .map_err(publish_error)?
            .ok_or(LibraryError::NotFound)?;
        let snapshots = SnapshotStore::new(conn)
            .bundle(bundle_id)
            .map_err(publish_error)?;
        if snapshots.is_empty() {
            return Err(LibraryError::Conflict {
                message: format!("Bundle {bundle_id} holds no undoable snapshots"),
            });
        }
        let journals = super::publish::journal::JournalStore::new(conn)
            .bundle(bundle_id)
            .map_err(publish_error)?;
        let blobs = BlobStore::open(open.sandbox.meta_dir()).map_err(publish_error)?;
        let by_track: HashMap<&str, &PlanItem> = source
            .items
            .iter()
            .map(|item| (item.track_id.as_str(), item))
            .collect();
        let mut inputs = Vec::new();
        for (_, track_id, blob_sha, expires_day) in &snapshots {
            let Some(item) = by_track.get(track_id.as_str()) else {
                continue;
            };
            let Some(journal) = journals
                .iter()
                .find(|journal| journal.track_id.as_deref() == Some(track_id.as_str()))
            else {
                continue;
            };
            let bytes = blobs.get(blob_sha).map_err(publish_error)?;
            let before =
                super::publish::undo::BeforeState::from_bytes(&bytes).map_err(publish_error)?;
            let staged_size = super::publish::paths::read_regular_file(
                &open
                    .sandbox
                    .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)
                    .unwrap_or_else(|_| PathBuf::from(&journal.staged)),
            )
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(item.fingerprint.size);
            inputs.push(UndoInput {
                track_id: track_id.clone(),
                before,
                published: FileFingerprint {
                    size: staged_size,
                    sha256: journal.staged_sha256.clone(),
                },
                published_root: journal.dest_root.clone(),
                published_rel: journal.dest_rel.clone(),
                identity: item.identity.clone(),
                override_revision: item.override_revision,
                expires_day: *expires_day,
            });
        }
        if inputs.is_empty() {
            return Err(LibraryError::Conflict {
                message: format!("Bundle {bundle_id} holds no undoable snapshots"),
            });
        }
        let mut fingerprints = BTreeMap::new();
        let mut locations = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        for input in &inputs {
            let (root_id, rel_path, _, _) = SqliteCatalog
                .locate(conn, &input.track_id)
                .map_err(publish_error)?
                .ok_or_else(|| LibraryError::Conflict {
                    message: format!("Track {} is no longer managed", input.track_id),
                })?;
            fingerprints.insert(
                input.track_id.clone(),
                Self::live_fingerprint(&open.sandbox, &root_id, &rel_path)?,
            );
            locations.insert(input.track_id.clone(), (root_id, rel_path));
            let album_id = self.track_album(&input.track_id)?;
            identities.insert(
                input.track_id.clone(),
                self.resolve_identity(&album_id, &input.track_id)?,
            );
            overrides.insert(input.track_id.clone(), PINNED_OVERRIDE);
        }
        let sandbox = open.sandbox.clone();
        let live = UndoLive {
            fingerprints,
            locations,
            identities,
            overrides,
            today_day: today_day(),
        };
        let plan = plan_undo(bundle_id, &inputs, &live, &|root_id, rel_path| {
            match sandbox.resolve_no_symlink(root_id, rel_path) {
                Ok(path) => path.symlink_metadata().is_ok(),
                // An unresolvable restore target fails closed as
                // occupied: undo never writes through a symlink.
                Err(_) => true,
            }
        });
        if !plan.writable() {
            return Err(LibraryError::Conflict {
                message: format!("Undo blocked: {}", undo_blocks(&plan)),
            });
        }
        // Restore bundle: each eligible file republishes its exact
        // before document onto its prior path.
        let mut plan_items = Vec::new();
        let mut docs = BTreeMap::new();
        for item in &plan.eligible {
            let live_loc =
                live.locations
                    .get(&item.track_id)
                    .ok_or_else(|| LibraryError::Conflict {
                        message: format!("Track {} moved during undo", item.track_id),
                    })?;
            let fingerprint = Self::live_fingerprint(&sandbox, &live_loc.0, &live_loc.1)?;
            let format = Self::live_format(&live_loc.1)?;
            let album_id = self.track_album(&item.track_id)?;
            let identity = self.resolve_identity(&album_id, &item.track_id)?;
            let same_path = live_loc.0 == item.restore_root && live_loc.1 == item.restore_rel;
            let item_kind = if same_path {
                PlanKind::SamePath
            } else {
                PlanKind::Move
            };
            let mut capabilities = Vec::new();
            if !item.doc.managed.is_empty() {
                capabilities.push(Capability::Metadata);
            }
            if item_kind == PlanKind::Move {
                if live_loc.0 == item.restore_root {
                    capabilities.push(Capability::SameRootMove);
                } else {
                    capabilities.push(Capability::CrossRootMove);
                }
            }
            plan_items.push(PlanItem {
                track_id: item.track_id.clone(),
                source_root: live_loc.0.clone(),
                source_rel: live_loc.1.clone(),
                dest_root: item.restore_root.clone(),
                dest_rel: item.restore_rel.clone(),
                kind: item_kind,
                fingerprint,
                identity,
                override_revision: PINNED_OVERRIDE,
                capabilities,
                format,
                managed_updates: item.doc.managed.clone(),
                sidecars: Vec::new(),
                staged_bytes_estimate: 0,
            });
            let size = sandbox
                .resolve_no_symlink(&live_loc.0, &live_loc.1)
                .ok()
                .and_then(|path| std::fs::metadata(&path).ok())
                .map(|meta| meta.len())
                .unwrap_or(0);
            if let Some(last) = plan_items.last_mut() {
                last.staged_bytes_estimate = size + STAGED_HEADROOM_BYTES;
            }
            docs.insert(
                item.track_id.clone(),
                Self::live_doc(&sandbox, &live_loc.0, &live_loc.1)?,
            );
        }
        self.publish_restoration(&mut cell, plan_items, docs)
    }

    /// Restore tracks to their immutable first-management baselines.
    /// Missing baselines, missing roots, changed files, and occupied
    /// originals 409 with per-file reasons. The write guards span
    /// the refresh plus the restoration publish. Blocking: handlers
    /// run this off the async runtime.
    pub fn baseline_restore(&self, track_ids: &[String]) -> Result<AppliedBundle, LibraryError> {
        use super::publish::publisher::Catalog as _;
        use super::publish::undo::{BaselineInput, plan_baseline_restore};

        if track_ids.is_empty() {
            return Err(LibraryError::InvalidInput {
                message: "Restore needs at least one track".to_owned(),
            });
        }
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(publish_error)?;
        let open = cell.open().map_err(publish_error)?;
        let conn = open.publisher.connection();
        let blobs = BlobStore::open(open.sandbox.meta_dir()).map_err(publish_error)?;
        let baselines = BaselineStore::new(conn);
        let mut inputs = Vec::new();
        let mut current_locs: HashMap<String, (String, String)> = HashMap::new();
        for track_id in track_ids {
            let baseline = match baselines.get(track_id).map_err(publish_error)? {
                Some((blob_sha, _, _)) => {
                    let bytes = blobs.get(&blob_sha).map_err(publish_error)?;
                    Some(
                        super::publish::undo::BeforeState::from_bytes(&bytes)
                            .map_err(publish_error)?,
                    )
                }
                None => None,
            };
            let (root_id, rel_path, _, _) = SqliteCatalog
                .locate(conn, track_id)
                .map_err(publish_error)?
                .ok_or_else(|| LibraryError::Conflict {
                    message: format!("Track {track_id} is not managed"),
                })?;
            let current = Self::live_fingerprint(&open.sandbox, &root_id, &rel_path)?;
            let format = Self::live_format(&rel_path)?;
            let album_id = self.track_album(track_id)?;
            let identity = self.resolve_identity(&album_id, track_id)?;
            current_locs.insert(track_id.clone(), (root_id, rel_path));
            inputs.push(BaselineInput {
                track_id: track_id.clone(),
                baseline,
                current: current.clone(),
                pinned_current: current,
                identity: identity.clone(),
                pinned_identity: identity,
                format: format.clone(),
                pinned_format: format,
            });
        }
        let sandbox = open.sandbox.clone();
        let registry = self.live_registry();
        // The original counts as occupied only when another file
        // holds it: a same-path restore whose subject still sits at
        // the original path is restoring tags in place, mirroring
        // undo's self-occupancy exemption.
        let plan = plan_baseline_restore(&inputs, &|root_id, rel_path| {
            registry.resolve(root_id)?;
            let occupied = match sandbox.resolve_no_symlink(root_id, rel_path) {
                Ok(path) => path.symlink_metadata().is_ok(),
                // An unresolvable original fails closed as occupied:
                // restore never writes through a symlink.
                Err(_) => true,
            };
            if !occupied {
                return Some(false);
            }
            let self_held = current_locs
                .values()
                .any(|(root, rel)| root == root_id && rel == rel_path);
            Some(!self_held)
        });
        if !plan.writable() {
            return Err(LibraryError::Conflict {
                message: format!("Baseline restore blocked: {}", baseline_blocks(&plan)),
            });
        }
        let mut plan_items = Vec::new();
        let mut docs = BTreeMap::new();
        for (track_id, before) in &plan.eligible {
            let live_loc = current_locs
                .get(track_id)
                .ok_or_else(|| LibraryError::Conflict {
                    message: format!("Track {track_id} moved during restore"),
                })?;
            let fingerprint = Self::live_fingerprint(&sandbox, &live_loc.0, &live_loc.1)?;
            let format = Self::live_format(&live_loc.1)?;
            let album_id = self.track_album(track_id)?;
            let identity = self.resolve_identity(&album_id, track_id)?;
            let same_path = live_loc.0 == before.source_root && live_loc.1 == before.source_rel;
            let item_kind = if same_path {
                PlanKind::SamePath
            } else {
                PlanKind::Move
            };
            let mut capabilities = Vec::new();
            if !before.doc.managed.is_empty() {
                capabilities.push(Capability::Metadata);
            }
            if item_kind == PlanKind::Move {
                if live_loc.0 == before.source_root {
                    capabilities.push(Capability::SameRootMove);
                } else {
                    capabilities.push(Capability::CrossRootMove);
                }
            }
            let size = sandbox
                .resolve_no_symlink(&live_loc.0, &live_loc.1)
                .ok()
                .and_then(|path| std::fs::metadata(&path).ok())
                .map(|meta| meta.len())
                .unwrap_or(0);
            plan_items.push(PlanItem {
                track_id: track_id.clone(),
                source_root: live_loc.0.clone(),
                source_rel: live_loc.1.clone(),
                dest_root: before.source_root.clone(),
                dest_rel: before.source_rel.clone(),
                kind: item_kind,
                fingerprint,
                identity,
                override_revision: PINNED_OVERRIDE,
                capabilities,
                format,
                managed_updates: before.doc.managed.clone(),
                sidecars: Vec::new(),
                staged_bytes_estimate: size + STAGED_HEADROOM_BYTES,
            });
            docs.insert(
                track_id.clone(),
                Self::live_doc(&sandbox, &live_loc.0, &live_loc.1)?,
            );
        }
        self.publish_restoration(&mut cell, plan_items, docs)
    }

    /// Seal and publish an internally planned restoration bundle
    /// (undo or baseline restore). The caller holds the publish
    /// lock plus the write guards; the seal and the commit run
    /// under both.
    fn publish_restoration(
        &self,
        cell: &mut PublishCell,
        plan_items: Vec<PlanItem>,
        docs: BTreeMap<String, TagDocument>,
    ) -> Result<AppliedBundle, LibraryError> {
        use super::publish::publisher::Catalog as _;

        let open = cell.open().map_err(publish_error)?;
        let catalog_revision = SqliteCatalog
            .revision(open.publisher.connection())
            .map_err(publish_error)?;
        let bundle = PlanBundle {
            id: self.ids.new_id(),
            items: plan_items,
            profile_revision: PINNED_REVISION,
            naming_revision: PINNED_REVISION,
            policy_revision: policy_revision_u64(&self.live_registry()),
            catalog_revision,
        };
        // Restoration bundles re-run the dry-run gates: a restore
        // onto an occupied path blocks instead of overwriting.
        CollisionGate::check_bundle(&open.sandbox, &bundle).map_err(publish_error)?;
        DiskPreflight::check(&bundle, &FsSpaceProbe::new(self.root_dirs.clone()))
            .map_err(publish_error)?;
        let token = self.ids.new_id();
        let token_hash = sha256_hex(token.as_bytes());
        let sealed = SealedPreview::seal(
            bundle,
            token_hash.clone(),
            PINNED_REVISION,
            today_day() + PREVIEW_TTL_DAYS,
        );
        let live = self.seal_live(&open.sandbox, &sealed.bundle, &token_hash)?;
        let outcome = open
            .publisher
            .publish(&sealed, &live, &docs)
            .map_err(publish_error)?;
        Ok(AppliedBundle {
            bundle_id: sealed.bundle.id.clone(),
            outcome: match outcome {
                PublishOutcome::Committed => "committed".to_owned(),
                PublishOutcome::CleanupPending => "cleanup_pending".to_owned(),
            },
            files: sealed
                .bundle
                .items
                .iter()
                .map(|item| AppliedFile {
                    track_id: item.track_id.clone(),
                    root_id: item.dest_root.clone(),
                    rel_path: item.dest_rel.clone(),
                })
                .collect(),
        })
    }
}

/// Per-file undo blocks as a compact message.
fn undo_blocks(plan: &super::publish::undo::UndoPlan) -> String {
    plan.blocked
        .iter()
        .map(|(track_id, block)| format!("{track_id} {}", undo_block_label(block)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn undo_block_label(block: &super::publish::undo::UndoBlock) -> &'static str {
    match block {
        super::publish::undo::UndoBlock::ExternallyChanged => "externally_changed",
        super::publish::undo::UndoBlock::Moved => "moved",
        super::publish::undo::UndoBlock::IdentityChanged => "identity_changed",
        super::publish::undo::UndoBlock::OverrideChanged => "override_changed",
        super::publish::undo::UndoBlock::SnapshotExpired => "snapshot_expired",
        super::publish::undo::UndoBlock::RestoreOccupied => "restore_occupied",
    }
}

/// Per-file baseline blocks as a compact message.
fn baseline_blocks(plan: &super::publish::undo::BaselineRestorePlan) -> String {
    plan.blocked
        .iter()
        .map(|(track_id, block)| format!("{track_id} {}", baseline_block_label(block)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn baseline_block_label(block: &super::publish::undo::BaselineBlock) -> &'static str {
    match block {
        super::publish::undo::BaselineBlock::MissingBaseline => "missing_baseline",
        super::publish::undo::BaselineBlock::MissingRoot => "missing_root",
        super::publish::undo::BaselineBlock::FormatMismatch => "format_mismatch",
        super::publish::undo::BaselineBlock::CurrentChanged => "current_changed",
        super::publish::undo::BaselineBlock::IdentityChanged => "identity_changed",
        super::publish::undo::BaselineBlock::OriginalOccupied => "original_occupied",
    }
}

/// Resolve the library principal from the stashed session, mirroring
/// the sibling role extractors: the role rereads the user row every
/// request. A session whose account is gone reads as stale (401).
async fn translate_principal(
    axum::extract::State(users): axum::extract::State<UsersDeps>,
    mut request: Request,
    next: Next,
) -> Response {
    use super::http::auth::{Principal, Role};
    use axum::response::IntoResponse;

    let missing = || LibraryError::Unauthorized {
        message: "Authentication required".to_owned(),
    };
    let Some(session) = request
        .extensions()
        .get::<crate::auth::session::middleware::CurrentSession>()
        .cloned()
    else {
        return missing().into_response();
    };
    let user = match users.users.get_by_id(&session.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return missing().into_response(),
        Err(StoreError::Conflict) => {
            return LibraryError::Conflict {
                message: "Conflicting state".to_owned(),
            }
            .into_response();
        }
        Err(StoreError::Internal(cause)) => {
            return LibraryError::internal(&cause).into_response();
        }
    };
    request.extensions_mut().insert(Principal {
        user_id: user.id,
        username: user.username,
        role: match user.role {
            crate::auth::users::roles::Role::User => Role::User,
            crate::auth::users::roles::Role::Trusted => Role::Trusted,
            crate::auth::users::roles::Role::Admin => Role::Admin,
        },
    });
    next.run(request).await
}
