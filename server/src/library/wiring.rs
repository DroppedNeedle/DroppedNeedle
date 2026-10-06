//! Library bundle: one setup the app mounts.
//!
//! [`LibrarySetup`] owns the scan coordinator, the identify service
//! and queue, the contribution service and worker, and the publish
//! cell, plus the HTTP state (preview seals) and the background
//! loops. `build` binds everything over the production provider
//! clients; `for_tests` binds the same shape over scripted providers
//! and memory stores. The routers nest under `/api/v3` inside the
//! deny-by-default session gate.
//!
//! Store durability: scan state runs on SQLite over the application
//! database, while identify and contribution state run on in-memory
//! stores. Durable SQLite ports for those, durable roots, the rolling
//! schedule settings, and the AcoustID key config do not exist yet. Publish journals, snapshots, baselines, and the catalog
//! shadow run on a dedicated rusqlite database under the primary
//! root with idempotent schema.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use tokio::sync::watch;

use super::adapters::{
    EmptyContributionIdentity, IdentifyEnqueue, LoftyTagReader, MinimalAttachmentEvidence,
    NoopContributionCatalog, TrackAlbumMap, UnavailableMusicBrainz,
};
use super::clock::now_unix;
use super::contrib::memory::MemoryStore as ContribMemoryStore;
use super::contrib::seams::SystemClock as ContribSystemClock;
use super::contrib::service::ContributionService;
use super::contrib::worker::{VerificationWorker, VerificationWorkerConfig};
use super::identify::memory::{
    MemoryAliasStore, MemoryIdentityStore, MemoryPinStore, MemoryProofStore, MemoryQueueStore,
    MemoryReviewStore,
};
use super::identify::providers::FakeProviders;
use super::identify::service::{IdentifyDeps, IdentifyService};
use super::loops::{identify_loop, publish_loop, scan_loop, watcher_loop};
use super::manage::{PreviewEntry, PublishCell};
use super::scan::coordinator::{
    LibraryScanCoordinator, ResolverSource, ScanEventPublisher, SharedResolver,
};
use super::scan::fs::FsCoordinator;
use super::scan::pool::BlockingPool;
use super::scan::roots::RootRegistry;
use super::scan::sqlite_store::SqliteScanStore;
use super::scan::watcher::{DirtyScopes, WatcherState, WorkWakeups};
use crate::auth::users::UsersDeps;
use crate::ids::IdGenerator;

/// Scan coordinator over the wired seams.
pub type ScanCoordinator = LibraryScanCoordinator<SqliteScanStore, LoftyTagReader, IdentifyEnqueue>;

/// Live root-registry source shared with the stream gateway.
pub type RootSource = Arc<dyn Fn() -> RootRegistry + Send + Sync>;

/// Shared root-directory lookup for the space probe.
pub(crate) type RootDirs = Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>;

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
    /// Build the production bundle over the live provider clients.
    /// MusicBrainz recall runs identity-critical; AcoustID support
    /// evidence runs without a key until the key config lands (the
    /// client answers `Missing` on an empty key, never dialing out).
    pub fn build(
        users: UsersDeps,
        http: &crate::http_client::HttpClientFactory,
        ids: Arc<dyn IdGenerator>,
        providers: Arc<crate::providers::Providers>,
        mb_source: crate::providers::musicbrainz::SourceFn,
        db_path: &Path,
    ) -> Result<Self, String> {
        use crate::providers::RequestPriority;
        use crate::providers::acoustid::{AcoustIdClient, DEFAULT_BASE_URL};
        use crate::providers::adapters::{CorePacer, CoreSink};
        use crate::providers::musicbrainz::{MbPacing, MusicBrainzClient, ReqwestMbTransport};

        // Identification is background work on the source settings name,
        // read per request; user page loads go ahead of it at the limiter.
        let musicbrainz = MusicBrainzClient::official(
            ReqwestMbTransport::new(http.no_redirect().clone()),
            MbPacing::new(providers.clone()),
        )
        .with_source_fn(mb_source)
        .with_priority(RequestPriority::BackgroundSync)
        .with_sink(CoreSink);
        let pacer = CorePacer::for_source(providers, "acoustid")
            .ok_or_else(|| "acoustid has no verified rate row".to_owned())?;
        let acoustid =
            AcoustIdClient::new(http.shared().clone(), DEFAULT_BASE_URL, pacer, CoreSink);
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

    /// Test bundle over scripted providers whose scan state lives in
    /// the application database at `db_path`, exactly like production.
    /// Journeys that run requests as real users need this: scan runs
    /// reference `auth_users`, which only the application database has.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests_at(
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        db_path: &Path,
    ) -> Result<Self, String> {
        let scripted = Arc::new(FakeProviders::default());
        let scan_store = Arc::new(
            SqliteScanStore::open(db_path).map_err(|error| format!("scan store: {error}"))?,
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
    /// shutdown tests script provider timing here; the shared
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

    /// Relative-path routers for nesting under `/api/v3` inside the
    /// session gate. The leg carries the principal-translation layer
    /// so handlers keep their `Principal` extractor.
    pub fn gated_router(&self) -> Router {
        super::http::routes::library_router(self.clone()).layer(
            axum::middleware::from_fn_with_state(
                self.users.clone(),
                super::http::auth::translate_principal,
            ),
        )
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
}
