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
//! Store durability: roots, scan state, identification, contributions,
//! and the publish journal live in the application database.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use tokio::sync::watch;

use super::adapters::LoftyTagReader;
use super::clock::now_unix;
use super::contrib::ContribProviders;
use super::contrib::service::ContributionService;
use super::contrib::worker::VerificationWorker;
use super::identify::providers::FakeProviders;
use super::identify::service::{IdentifyDeps, IdentifyService};
use super::identify::sqlite::SqliteIdentifyStore;
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
use crate::runtime_config::ConfigStore;

/// Scratch database (every migration applied) plus config store in a
/// directory that goes away when the last bundle clone drops.
#[cfg(any(test, feature = "test-support"))]
fn scratch_state() -> Result<(Arc<crate::tooling::scratch::ScratchDir>, Arc<ConfigStore>), String> {
    let dir =
        crate::tooling::scratch::ScratchDir::new("library").map_err(|error| error.to_string())?;
    let db_path = dir.path().join("app.db");
    let connection = crate::db::open_connection(&db_path).map_err(|error| error.to_string())?;
    crate::schema::apply_migrations_blocking(&connection).map_err(|error| error.to_string())?;
    let crypto = crate::runtime_config::Crypto::from_key_bytes(&[7u8; 32])
        .map_err(|error| error.to_string())?;
    let config = ConfigStore::open(&dir.path().join("config.json"), crypto)
        .map_err(|error| error.to_string())?;
    Ok((Arc::new(dir), Arc::new(config)))
}

/// Scan coordinator over the wired seams.
pub type ScanCoordinator = LibraryScanCoordinator<SqliteScanStore, LoftyTagReader>;

/// Live root-registry source shared with the stream gateway.
pub type RootSource = Arc<dyn Fn() -> RootRegistry + Send + Sync>;

/// Shared root-directory lookup for the space probe.
pub(crate) type RootDirs = Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>;

/// What the identify providers are built from, once the bundle's
/// stores, pool, and roots exist.
struct ProviderParts {
    store: Arc<SqliteIdentifyStore>,
    pool: BlockingPool,
    roots: super::identify::sources::Roots,
    config: Arc<ConfigStore>,
}

type ProviderFactory =
    Box<dyn FnOnce(ProviderParts) -> Arc<dyn super::identify::providers::IdentifyProviders>>;

/// A factory handing out the scripted providers of a test bundle.
#[cfg(any(test, feature = "test-support"))]
fn scripted_factory(scripted: Arc<FakeProviders>) -> ProviderFactory {
    Box::new(move |_| scripted as Arc<dyn super::identify::providers::IdentifyProviders>)
}

/// Scripted Discogs and MusicBrainz reads behind a test bundle's
/// contribution service; journeys script them through this handle.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Default)]
pub struct TestContribProviders {
    pub discogs: Arc<super::contrib::memory::ScriptedDiscogs>,
    pub musicbrainz: Arc<super::contrib::memory::ScriptedMusicBrainz>,
}

#[cfg(any(test, feature = "test-support"))]
impl TestContribProviders {
    fn providers(&self) -> ContribProviders {
        ContribProviders {
            discogs: self.discogs.clone(),
            musicbrainz: self.musicbrainz.clone(),
        }
    }
}

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

/// What runs after a scan that changed the library completes. Boot fills
/// it once the jobs exist;
/// until then completed scans call nothing.
pub type ScanCompletedSlot = Arc<std::sync::OnceLock<Arc<dyn Fn() + Send + Sync>>>;

/// Everything `create_app` needs to mount the library engine, built once.
#[derive(Clone)]
pub struct LibrarySetup {
    /// User store for principal translation.
    pub users: UsersDeps,
    /// Id generator for jobs, runs, and preview tokens.
    pub ids: Arc<dyn IdGenerator>,
    /// Runtime settings: roots, schedule, and watcher are re-read from
    /// here every tick.
    pub config: Arc<ConfigStore>,
    /// Root registry the scan checkpoints and the stream gateway read;
    /// refreshed from the settings on every tick.
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
    /// Durable identify state: queue, identities, reviews, pins,
    /// aliases, and the catalog-backed album facts.
    pub identify_store: Arc<SqliteIdentifyStore>,
    /// Identify service.
    pub identify: Arc<IdentifyService>,
    /// Scripted providers (test bundles only).
    pub test_providers: Option<Arc<FakeProviders>>,
    /// MusicBrainz releases for importing finished downloads: search and
    /// release lookups, identity-critical like identification. `None` in
    /// test bundles unless a test scripts one.
    pub releases: Option<Arc<dyn super::identify::sources::ReleaseSource>>,
    /// AcoustID for files being imported, keyed per call and keeping
    /// nothing; off while no AcoustID key is set. `None` in test bundles.
    pub fingerprints: Option<Arc<dyn super::identify::sources::FingerprintSource>>,
    /// Contribution service.
    pub contrib: Arc<ContributionService>,
    /// Contribution verification worker.
    pub contrib_worker: Arc<VerificationWorker>,
    /// Scripted contribution providers (test bundles only).
    #[cfg(any(test, feature = "test-support"))]
    pub test_contrib: Option<TestContribProviders>,
    /// Publish cell (empty until a usable root exists).
    pub publish: Arc<std::sync::Mutex<PublishCell>>,
    /// Unsettled publish bundles, read from the journal; their tracks
    /// take no new managed writes until a pass settles them.
    pub held_bundles: super::manage::HeldBundles,
    /// Sealed previews awaiting apply, keyed by token hash.
    pub previews: Arc<std::sync::Mutex<HashMap<String, PreviewEntry>>>,
    /// Watcher state across ticks.
    pub watcher_state: Arc<std::sync::Mutex<WatcherState>>,
    /// Shared root-directory lookup.
    pub root_dirs: RootDirs,
    /// Live event hub handle (scan events poke the revision poller).
    pub events: crate::events::EventSink,
    /// Download and wanted cleanup after an album removal, set at boot.
    pub removal_hook: super::mutations::AlbumRemovalSlot,
    /// Called after each scan run completes (the library image refresh
    /// kick), set at boot.
    pub scan_completed: ScanCompletedSlot,
    /// Scratch state of a test bundle, removed with the last clone.
    #[cfg(any(test, feature = "test-support"))]
    pub scratch: Option<Arc<crate::tooling::scratch::ScratchDir>>,
}

impl LibrarySetup {
    /// Build the production bundle over the live provider clients.
    /// Identification reads MusicBrainz identity-critical and keeps the
    /// release documents it fetches; AcoustID runs only for albums whose
    /// tags are weak, with the key read from the settings per job.
    pub fn build(
        users: UsersDeps,
        http: &crate::http_client::HttpClientFactory,
        ids: Arc<dyn IdGenerator>,
        providers: Arc<crate::providers::Providers>,
        mb_source: crate::providers::musicbrainz::SourceFn,
        db_path: &Path,
        config: Arc<ConfigStore>,
    ) -> Result<Self, String> {
        use crate::providers::RequestPriority;
        use crate::providers::acoustid::{AcoustIdClient, DEFAULT_BASE_URL};
        use crate::providers::adapters::{CorePacer, HealthSink};
        use crate::providers::musicbrainz::{MbPacing, MusicBrainzClient, ReqwestMbTransport};

        // Identification is background work on the source settings name,
        // read per request; user page loads go ahead of it at the limiter.
        // Both clients count their failures toward system health, so an
        // outage during a background lookup shows on the health dot too.
        let health = HealthSink::new(&providers);
        let mb_client = |priority: RequestPriority| {
            MusicBrainzClient::official(
                ReqwestMbTransport::new(http.no_redirect().clone()),
                MbPacing::new(providers.clone()),
            )
            .with_source_fn(mb_source.clone())
            .with_priority(priority)
            .with_sink(health.clone())
        };
        let musicbrainz = mb_client(RequestPriority::BackgroundSync);
        // Downloads being imported look releases up through their own
        // client on the same limiter and source setting.
        let imports: Arc<dyn super::identify::sources::ReleaseSource> =
            Arc::new(mb_client(RequestPriority::BackgroundSync));
        // Contributions read MusicBrainz on both lanes (the curator's page
        // and the verification worker) and Discogs through the shared GET
        // port.
        let contrib = ContribProviders {
            discogs: Arc::new(super::adapters::LiveDiscogsContrib::new(
                crate::providers::ReqwestGet::shared(http),
            )),
            musicbrainz: Arc::new(super::adapters::LiveMusicBrainzContrib::new(
                mb_client(RequestPriority::UserInitiated),
                mb_client(RequestPriority::BackgroundSync),
            )),
        };
        let import_pacer = CorePacer::for_source(providers.clone(), "acoustid")
            .ok_or_else(|| "acoustid has no verified rate row".to_owned())?;
        let import_acoustid = AcoustIdClient::new(
            http.shared().clone(),
            DEFAULT_BASE_URL,
            import_pacer,
            health.clone(),
        );
        let pacer = CorePacer::for_source(providers, "acoustid")
            .ok_or_else(|| "acoustid has no verified rate row".to_owned())?;
        let acoustid = AcoustIdClient::new(http.shared().clone(), DEFAULT_BASE_URL, pacer, health);
        let make: ProviderFactory = Box::new(move |parts| {
            let fingerprints = super::identify::sources::AcoustIdFingerprints::new(
                acoustid,
                parts.config,
                parts.roots,
                parts.pool,
                parts.store.clone(),
            );
            Arc::new(super::identify::providers::LiveProviders::new(
                musicbrainz,
                parts.store,
                fingerprints,
            ))
        });
        let mut setup = Self::assemble(users, ids, make, None, contrib, db_path, config)?;
        setup.releases = Some(imports);
        let registry = setup.registry.clone();
        setup.fingerprints = Some(Arc::new(
            super::identify::sources::AcoustIdFingerprints::new(
                import_acoustid,
                setup.config.clone(),
                Arc::new(move || registry.resolver().registry().clone()),
                setup.pool.clone(),
                setup.identify_store.clone(),
            ),
        ));
        Ok(setup)
    }

    /// Test bundle over scripted providers on a fresh scratch database
    /// and config file under the temp dir.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(users: UsersDeps, ids: Arc<dyn IdGenerator>) -> Result<Self, String> {
        let scripted = Arc::new(FakeProviders::default());
        let (dir, config) = scratch_state()?;
        let contrib = TestContribProviders::default();
        let mut setup = Self::assemble(
            users,
            ids,
            scripted_factory(scripted.clone()),
            Some(scripted),
            contrib.providers(),
            &dir.path().join("app.db"),
            config,
        )?;
        setup.scratch = Some(dir);
        setup.test_contrib = Some(contrib);
        Ok(setup)
    }

    /// Test bundle over scripted providers on an existing application
    /// database and config store, exactly like production. Journeys
    /// that act as real users need this (scan runs reference
    /// `auth_users`), and so do restart tests that rebuild the bundle
    /// over the same state.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests_at(
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        db_path: &Path,
        config: Arc<ConfigStore>,
    ) -> Result<Self, String> {
        let scripted = Arc::new(FakeProviders::default());
        let contrib = TestContribProviders::default();
        let mut setup = Self::assemble(
            users,
            ids,
            scripted_factory(scripted.clone()),
            Some(scripted),
            contrib.providers(),
            db_path,
            config,
        )?;
        setup.test_contrib = Some(contrib);
        Ok(setup)
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
        let (dir, config) = scratch_state()?;
        let contrib = TestContribProviders::default();
        let mut setup = Self::assemble(
            users,
            ids,
            Box::new(move |_| providers),
            None,
            contrib.providers(),
            &dir.path().join("app.db"),
            config,
        )?;
        setup.scratch = Some(dir);
        setup.test_contrib = Some(contrib);
        Ok(setup)
    }

    fn assemble(
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        make_providers: ProviderFactory,
        test_providers: Option<Arc<FakeProviders>>,
        contrib_providers: ContribProviders,
        db_path: &Path,
        config: Arc<ConfigStore>,
    ) -> Result<Self, String> {
        let scan_store = Arc::new(
            SqliteScanStore::open(db_path).map_err(|error| format!("scan store: {error}"))?,
        );
        // The roots load before anything can scan or stream: a restart
        // comes back with exactly the roots the settings hold.
        let registry: Arc<SharedResolver> = Arc::new(SharedResolver::new(
            super::settings::registry(&config)
                .map_err(|error| format!("library roots: {error}"))?,
        ));
        let root_dirs: RootDirs = {
            let registry = registry.clone();
            Arc::new(move || registry.resolver().registry().root_paths())
        };
        let identify_store = Arc::new(
            SqliteIdentifyStore::open(db_path)
                .map_err(|error| format!("identify store: {error}"))?,
        );
        let pool = BlockingPool::new(4);
        let wakeups = WorkWakeups::new();
        let fs = FsCoordinator::new();
        let events = crate::events::EventSink::default();
        let scan_events = events.clone();
        let scan_completed: ScanCompletedSlot = Default::default();
        let completed_hook = Arc::clone(&scan_completed);
        let coordinator = Arc::new(
            ScanCoordinator::new(
                scan_store.clone(),
                pool.clone(),
                Arc::new(LoftyTagReader),
                registry.clone(),
            )
            .with_filesystem(fs.clone())
            .with_wakeups(wakeups.clone())
            // A scan event follows a run write: the revision poller
            // reads at once, so a moved revision goes out as
            // `activity.changed` without waiting for its next tick.
            .with_events(ScanEventPublisher::new(Arc::new(move |event| {
                tracing::debug!(
                    run_id = event.run_id,
                    event = event.event,
                    "library scan event"
                );
                scan_events.poke_activity();
                if event.state == crate::library::scan::models::ScanState::Completed
                    && event.event == "scan.transition"
                    && event.changed
                    && let Some(hook) = completed_hook.get()
                {
                    hook();
                }
            }))),
        );
        let providers = make_providers(ProviderParts {
            store: identify_store.clone(),
            pool: pool.clone(),
            roots: {
                let registry = registry.clone();
                Arc::new(move || registry.resolver().registry().clone())
            },
            config: config.clone(),
        });
        let identify = Arc::new(
            IdentifyService::new(IdentifyDeps {
                identities: identify_store.clone(),
                facts: identify_store.clone(),
                proofs: identify_store.clone(),
                aliases: identify_store.clone(),
                queue: identify_store.clone(),
                reviews: identify_store.clone(),
                releases: identify_store.clone(),
                providers,
            })
            .with_preferences(edition_preferences(config.clone())),
        );
        let (contrib, contrib_worker) = super::contrib::assemble(
            db_path,
            contrib_providers,
            Arc::new(super::adapters::IdentifyFollowUp::new(
                identify_store.clone(),
            )),
        )?;
        Ok(Self {
            users,
            ids,
            config,
            registry,
            scan_store,
            coordinator,
            fs,
            pool,
            dirty: DirtyScopes::new(),
            wakeups,
            identify_store,
            identify,
            test_providers,
            releases: None,
            fingerprints: None,
            contrib,
            contrib_worker,
            #[cfg(any(test, feature = "test-support"))]
            test_contrib: None,
            publish: Arc::new(std::sync::Mutex::new(PublishCell::new(db_path))),
            held_bundles: super::manage::HeldBundles::new(db_path),
            previews: Arc::new(std::sync::Mutex::new(HashMap::new())),
            watcher_state: Arc::new(std::sync::Mutex::new(WatcherState::new())),
            root_dirs,
            events,
            removal_hook: Default::default(),
            scan_completed,
            #[cfg(any(test, feature = "test-support"))]
            scratch: None,
        })
    }

    /// Live root registry: re-read from the settings, never cached.
    pub fn live_registry(&self) -> RootRegistry {
        self.refresh_registry()
    }

    /// Re-read the roots from the settings and swap them in when they
    /// changed. Roots that are new or changed get a dirty mark, so the
    /// supervisor scans them; an unreadable settings section keeps the
    /// current registry.
    pub fn refresh_registry(&self) -> RootRegistry {
        let current = self.registry.resolver().registry().clone();
        let fresh = match super::settings::registry(&self.config) {
            Ok(fresh) => fresh,
            Err(error) => {
                tracing::warn!(%error, "cannot read the library roots; keeping the current set");
                return current;
            }
        };
        if fresh.policy_revision() == current.policy_revision() {
            return current;
        }
        self.registry.update(fresh.clone());
        let mut marked = false;
        for root in fresh.roots() {
            if current.resolve(&root.id) != Some(root) {
                self.dirty.mark(&root.id);
                marked = true;
            }
        }
        if marked {
            self.wakeups.notify("scan");
        }
        fresh
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
    /// clean shutdown is a no-op. Never fails boot: a publish journal
    /// that cannot be reconciled is logged and left for an
    /// administrator, and managed writes stay closed until it clears.
    pub async fn run_recovery(&self) -> LibraryRecovery {
        let registry = self.live_registry();
        // Startup is uncontended, so the guards cost nothing; taken
        // first so reconcile can never race a scan.
        let _guards = self.publish_guards_async(&registry).await;
        let recoveries = {
            let mut cell = self
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let recoveries = cell
                .refresh(&registry, &self.root_dirs)
                .unwrap_or_else(|error| {
                    tracing::error!(%error, "publish recovery did not run; managed writes stay closed");
                    Vec::new()
                });
            // Copies an interrupted download import left in the hidden
            // import folders, once no bundle still waits on them.
            let roots: Vec<PathBuf> = registry
                .roots()
                .iter()
                .map(|root| root.path.clone())
                .collect();
            let settled = cell.cell.is_some() && !cell.has_held();
            super::import::sweep_import_leftovers(&roots, settled);
            recoveries
        };
        let requeued = {
            use super::identify::stores::QueueStore as _;
            self.identify_store.recover()
        };
        if requeued > 0 {
            tracing::info!(requeued, "identify jobs from the previous run requeued");
        }
        let recovered = self.contrib_worker.recover(now_unix()).await;
        LibraryRecovery {
            publish_recoveries: recoveries
                .into_iter()
                .map(|recovery| (recovery.bundle_id, format!("{:?}", recovery.action)))
                .collect(),
            contrib_recovered: recovered,
        }
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
            super::operations::spawn_loop(self, shutdown.clone()),
            (
                "library-publish",
                tokio::spawn(publish_loop(self.clone(), shutdown)),
            ),
        ]
    }
}

/// The saved edition preferences, read on every identification so a saved
/// change applies to the next album. A broken settings file falls back to
/// the defaults with a log line.
pub fn edition_preferences(
    config: Arc<ConfigStore>,
) -> crate::library::identify::service::PreferenceSource {
    use crate::runtime_config::sections::{EditionPreferences, GetIt};
    Arc::new(move || {
        let saved = config.get::<EditionPreferences>().unwrap_or_else(|error| {
            tracing::warn!(%error, "edition preferences unreadable; using the defaults");
            EditionPreferences::default()
        });
        let region = config
            .get::<GetIt>()
            .map(|section| section.store_region)
            .unwrap_or_else(|_| GetIt::default().store_region);
        crate::library::edition_prefs::Preferences::from_settings(&saved, &region)
    })
}
