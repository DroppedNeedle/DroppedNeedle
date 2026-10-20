//! Library bundle: one setup the app mounts.
//!
//! [`LibrarySetup`] owns the scan coordinator, the identify service
//! and queue, the contribution service and worker, and the publish
//! cell, plus the HTTP state (preview seals) and the background
//! loops. `build` binds everything over the production provider
//! clients, or over the provider ports a test gives in [`Endpoints`].
//! [`routes`] lists the library's routes for the app to mount.
//!
//! Store durability: roots, scan state, identification, contributions,
//! and the publish journal live in the application database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::adapters::LoftyTagReader;
use super::contrib::ContribProviders;
use super::contrib::service::ContributionService;
use super::contrib::worker::VerificationWorker;
use super::identify::service::{IdentifyDeps, IdentifyService};
use super::identify::sqlite::SqliteIdentifyStore;
use super::manage::{PreviewEntry, PublishCell};
use super::management::enrich::EnrichmentPort;
use super::management::settings::service::LibraryManagementService;
use super::mutations::AlbumRemovalHook;
use super::scan::coordinator::{
    LibraryScanCoordinator, ResolverSource, ScanEventPublisher, SharedResolver,
};
use super::scan::fs::FsCoordinator;
use super::scan::pool::BlockingPool;
use super::scan::roots::RootRegistry;
use super::scan::sqlite_store::SqliteScanStore;
use super::scan::watcher::{DirtyScopes, WatcherState, WorkWakeups};
use super::service::ServiceError;
use super::settings::LibrarySettings;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_kernel::runtime_config::{Live, Plain};

/// Scan coordinator over the wired seams.
pub type ScanCoordinator = LibraryScanCoordinator<LoftyTagReader>;

/// Where the library reaches its outside services, and the provider ports a
/// test may put in place of the live clients.
#[derive(Clone)]
pub struct Endpoints {
    /// Discogs API origin (contributions).
    pub discogs: String,
    /// AcoustID API origin (identification and imports).
    pub acoustid: String,
    /// What identification asks MusicBrainz and AcoustID through; `None`
    /// builds the live clients.
    pub identify_providers: Option<Arc<dyn super::identify::providers::IdentifyProviders>>,
    /// What contributions ask Discogs and MusicBrainz through; `None` builds
    /// the live clients.
    pub contribution_providers: Option<ContribProviders>,
}

/// Shared root-directory lookup for the space probe.
pub(crate) type RootDirs = Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>;

/// What the identify providers are built from, once the bundle's
/// stores, pool, and roots exist.
struct ProviderParts {
    store: Arc<SqliteIdentifyStore>,
    pool: BlockingPool,
    roots: SharedResolver,
    library: Live<TypedLibrary>,
}

type ProviderFactory =
    Box<dyn FnOnce(ProviderParts) -> Arc<dyn super::identify::providers::IdentifyProviders>>;

/// Every library route, grouped by who may call it.
pub fn routes() -> droppedneedle_kernel::mount::Routes<LibrarySetup> {
    super::http::routes::library_routes()
}

/// Startup recovery report.
#[derive(Debug, Clone, Default)]
pub struct LibraryRecovery {
    /// Per-bundle publish recovery actions.
    pub publish_recoveries: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// Bundle.
// ---------------------------------------------------------------------------

/// What runs after a scan that changed the library completes.
pub type ScanCompleted = Arc<dyn Fn() + Send + Sync>;

/// What the library calls outside itself, given at construction.
pub struct LibraryPorts {
    /// Download and wanted cleanup after an album removal.
    pub removal: Arc<dyn AlbumRemovalHook>,
    /// Genres, lyrics, covers and AcoustID ids Library Management plans
    /// with. `None` only in a test bundle with no outside sources:
    /// planning then asks nothing.
    pub enrichment: Option<Arc<dyn EnrichmentPort>>,
    /// Called after each scan run that changed the library completes
    /// (the library image refresh).
    pub scan_completed: ScanCompleted,
    /// Root leases, shared with the plugin host's library writes.
    pub fs: FsCoordinator,
}

/// Everything `create_app` needs to mount the library engine, built once.
#[derive(Clone)]
pub struct LibrarySetup {
    /// Id generator for jobs, runs, and preview tokens.
    pub ids: Arc<dyn IdGenerator>,
    /// Live handles on the settings the library reads: roots, schedule
    /// and watcher are read every tick.
    pub settings: LibrarySettings,
    /// Root registry the scan checkpoints and the stream gateway read;
    /// refreshed from the settings on every tick.
    pub registry: Arc<SharedResolver>,
    /// The application database: the library's services read and write
    /// through it.
    pub db: droppedneedle_kernel::db::Db,
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
    /// MusicBrainz releases for importing finished downloads: search and
    /// release lookups, identity-critical like identification.
    pub releases: Arc<dyn super::identify::sources::ReleaseSource>,
    /// AcoustID for files being imported, keyed per call and keeping
    /// nothing; off while no AcoustID key is set.
    pub fingerprints: Arc<dyn super::identify::sources::FingerprintSource>,
    /// Contribution service.
    pub contrib: Arc<ContributionService>,
    /// Contribution verification worker.
    pub contrib_worker: Arc<VerificationWorker>,
    /// Publish cell (empty until a usable root exists).
    pub publish: Arc<std::sync::Mutex<PublishCell>>,
    /// Placement modes of the open publish session, readable without the
    /// cell.
    pub placement: super::manage::RootPlacement,
    /// Unsettled publish bundles, read from the journal; their tracks
    /// take no new managed writes until a pass settles them.
    pub held_bundles: super::manage::HeldBundles,
    /// Hidden files no journal row names, from the pass after startup.
    pub leftover_files: super::manage::LeftoverFiles,
    /// Sealed previews awaiting apply, keyed by token hash.
    pub previews: Arc<std::sync::Mutex<HashMap<String, PreviewEntry>>>,
    /// Watcher state across ticks.
    pub watcher_state: Arc<std::sync::Mutex<WatcherState>>,
    /// Shared root-directory lookup.
    pub root_dirs: RootDirs,
    /// Download and wanted cleanup after an album removal.
    pub removal_hook: Arc<dyn AlbumRemovalHook>,
    /// The shared outbound HTTP client, for media-server refreshes after a
    /// management run.
    pub http: reqwest::Client,
    /// Library Management's settings service. The settings page uses this
    /// one too, so an activation dry run and a page save never save over
    /// each other.
    pub management_settings: Arc<LibraryManagementService>,
    /// Genres, lyrics and covers Library Management plans with (the
    /// artwork cache lives with the reads). `None` in a test bundle with
    /// no outside sources.
    pub enrichment: Option<Arc<dyn EnrichmentPort>>,
}

impl LibrarySetup {
    /// Build the production bundle over the live provider clients.
    /// Identification reads MusicBrainz identity-critical and keeps the
    /// release documents it fetches; AcoustID runs only for albums whose
    /// tags are weak, with the key read from the settings per job. The
    /// publisher and its journal readers still open their own connections
    /// on the platform database's file. What the library calls outside
    /// itself comes in `ports`.
    pub fn build(
        platform: &droppedneedle_kernel::platform::Platform,
        providers: Arc<droppedneedle_providers::Providers>,
        mb_source: droppedneedle_providers::musicbrainz::LiveSource,
        queue: droppedneedle_kernel::jobs::queue::JobQueue,
        endpoints: Endpoints,
        ports: LibraryPorts,
    ) -> Result<Self, String> {
        use droppedneedle_providers::RequestPriority;
        use droppedneedle_providers::acoustid::AcoustIdClient;
        use droppedneedle_providers::adapters::{CorePacer, HealthSink};

        let http = &platform.http;
        // Identification is background work on the source settings name,
        // read per request; user page loads go ahead of it at the limiter.
        // Both clients count their failures toward system health, so an
        // outage during a background lookup shows on the health dot too.
        let health = HealthSink::new(&providers);
        let mb_client =
            |priority: RequestPriority| providers.musicbrainz.live(mb_source.clone(), priority);
        let musicbrainz = mb_client(RequestPriority::BackgroundSync);
        // Downloads being imported look releases up through their own
        // client on the same limiter and source setting.
        let imports: Arc<dyn super::identify::sources::ReleaseSource> =
            Arc::new(mb_client(RequestPriority::BackgroundSync));
        // Contributions read MusicBrainz on both lanes (the curator's page
        // and the verification worker) and Discogs through the shared GET
        // port.
        let contrib = endpoints
            .contribution_providers
            .unwrap_or_else(|| ContribProviders {
                discogs: Arc::new(super::adapters::LiveDiscogsContrib::new(
                    droppedneedle_providers::ReqwestGet::shared(http),
                    &endpoints.discogs,
                )),
                musicbrainz: Arc::new(super::adapters::LiveMusicBrainzContrib::new(
                    mb_client(RequestPriority::UserInitiated),
                    mb_client(RequestPriority::BackgroundSync),
                )),
            });
        let import_pacer = CorePacer::for_source(providers.clone(), "acoustid")
            .ok_or_else(|| "acoustid has no verified rate row".to_owned())?;
        let import_acoustid = AcoustIdClient::new(
            http.shared().clone(),
            &endpoints.acoustid,
            import_pacer,
            health.clone(),
        );
        let pacer = CorePacer::for_source(providers, "acoustid")
            .ok_or_else(|| "acoustid has no verified rate row".to_owned())?;
        let acoustid =
            AcoustIdClient::new(http.shared().clone(), &endpoints.acoustid, pacer, health);
        // Identification asks the port `endpoints` gives, or the live
        // clients when it gives none.
        let make: ProviderFactory = match endpoints.identify_providers {
            Some(given) => Box::new(move |_| given),
            None => Box::new(move |parts| {
                let fingerprints = super::identify::sources::AcoustIdFingerprints::new(
                    acoustid,
                    parts.library,
                    parts.roots,
                    parts.pool,
                    parts.store.clone(),
                );
                Arc::new(super::identify::providers::LiveProviders::new(
                    musicbrainz,
                    parts.store,
                    fingerprints,
                ))
            }),
        };
        Self::assemble(
            platform,
            make,
            contrib,
            queue,
            ports,
            imports,
            import_acoustid,
        )
    }

    fn assemble(
        platform: &droppedneedle_kernel::platform::Platform,
        make_providers: ProviderFactory,
        contrib_providers: ContribProviders,
        queue: droppedneedle_kernel::jobs::queue::JobQueue,
        ports: LibraryPorts,
        releases: Arc<dyn super::identify::sources::ReleaseSource>,
        import_acoustid: droppedneedle_providers::acoustid::AcoustIdClient<
            droppedneedle_providers::adapters::CorePacer,
            droppedneedle_providers::adapters::HealthSink,
        >,
    ) -> Result<Self, String> {
        let db = &platform.db;
        let settings = LibrarySettings::new(&platform.settings);
        let scan_store = Arc::new(SqliteScanStore::new(db.clone()));
        // The roots load before anything can scan or stream: a restart
        // comes back with exactly the roots the settings hold.
        let registry: Arc<SharedResolver> = Arc::new(SharedResolver::new(
            super::settings::registry(&settings.library)
                .map_err(|error| format!("library roots: {error}"))?,
        ));
        let root_dirs: RootDirs = {
            let registry = registry.clone();
            Arc::new(move || registry.resolver().registry().root_paths())
        };
        let identify_store = Arc::new(SqliteIdentifyStore::new(db.clone()));
        let pool = BlockingPool::new(4);
        let wakeups = WorkWakeups::new();
        let LibraryPorts {
            removal,
            enrichment,
            scan_completed,
            fs,
        } = ports;
        let coordinator = Arc::new(
            ScanCoordinator::new(
                scan_store.clone(),
                pool.clone(),
                Arc::new(LoftyTagReader),
                registry.clone(),
            )
            .with_filesystem(fs.clone())
            .with_wakeups(wakeups.clone())
            .with_events(ScanEventPublisher::new(Arc::new(move |event| {
                tracing::debug!(
                    run_id = event.run_id,
                    event = event.event,
                    "library scan event"
                );
                if event.state == crate::scan::models::ScanState::Completed
                    && event.event == "scan.transition"
                    && event.changed
                {
                    scan_completed();
                }
            }))),
        );
        let providers = make_providers(ProviderParts {
            store: identify_store.clone(),
            pool: pool.clone(),
            roots: (*registry).clone(),
            library: settings.library.clone(),
        });
        let identify = Arc::new(
            IdentifyService::new(IdentifyDeps {
                store: identify_store.clone(),
                providers,
            })
            .with_preferences(settings.editions.clone()),
        );
        let (contrib, contrib_worker) = super::contrib::assemble(
            db,
            queue,
            contrib_providers,
            Arc::new(super::adapters::IdentifyFollowUp::new(
                identify_store.clone(),
            )),
        );
        let fingerprints: Arc<dyn super::identify::sources::FingerprintSource> =
            Arc::new(super::identify::sources::AcoustIdFingerprints::new(
                import_acoustid,
                settings.library.clone(),
                (*registry).clone(),
                pool.clone(),
                identify_store.clone(),
            ));
        let publish = PublishCell::new(db.clone());
        let placement = publish.placement();
        Ok(Self {
            ids: platform.ids.clone(),
            management_settings: Arc::new(LibraryManagementService::new(platform.settings.clone())),
            settings,
            registry,
            db: db.clone(),
            scan_store,
            coordinator,
            fs,
            pool,
            dirty: DirtyScopes::new(),
            wakeups,
            identify_store,
            identify,
            releases,
            fingerprints,
            contrib,
            contrib_worker,
            publish: Arc::new(std::sync::Mutex::new(publish)),
            placement,
            held_bundles: super::manage::HeldBundles::new(db.clone()),
            leftover_files: super::manage::LeftoverFiles::new(db.clone()),
            previews: Arc::new(std::sync::Mutex::new(HashMap::new())),
            watcher_state: Arc::new(std::sync::Mutex::new(WatcherState::new())),
            root_dirs,
            removal_hook: removal,
            http: platform.http.shared().clone(),
            enrichment,
        })
    }

    /// Live root registry: re-read from the settings, never cached.
    pub fn live_registry(&self) -> RootRegistry {
        self.refresh_registry()
    }

    /// The policy revision pages are shown and send back as
    /// `expected_policy_revision`: the saved settings' revision of the
    /// roots and rules, as `GET /settings/library` shows it. The
    /// registry's own fingerprint is a different hash, kept internally.
    pub fn policy_revision(&self) -> Result<String, ServiceError> {
        let settings = self.library_settings()?;
        Ok(crate::policy::resolve_stored(&settings).policy_revision)
    }

    /// The live registry, or `None` when the saved roots and rules no
    /// longer carry `expected` as their policy revision (see
    /// [`Self::policy_revision`]) or the registry does not match them.
    pub fn registry_at(&self, expected: &str) -> Result<Option<RootRegistry>, ServiceError> {
        let live = self.live_registry();
        let settings = self.library_settings()?;
        let current = crate::policy::resolve_stored(&settings).policy_revision == expected
            && super::settings::registry_from(&settings).policy_revision()
                == live.policy_revision();
        Ok(current.then_some(live))
    }

    fn library_settings(&self) -> Result<Arc<Plain<TypedLibrary>>, ServiceError> {
        self.settings
            .library
            .get()
            .map_err(|error| ServiceError::internal(&error))
    }

    /// Re-read the roots from the settings and swap them in when they
    /// changed. Roots that are new or changed get a dirty mark, so the
    /// supervisor scans them; an unreadable settings section keeps the
    /// current registry.
    pub fn refresh_registry(&self) -> RootRegistry {
        let current = self.registry.resolver().registry().clone();
        let fresh = match super::settings::registry(&self.settings.library) {
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

    /// The shared root registry the stream gateway resolves local reads
    /// against; every refresh swaps the roots in for all its holders.
    pub fn root_source(&self) -> SharedResolver {
        (*self.registry).clone()
    }

    /// Startup recovery: publish reconcile (resume-or-compensate,
    /// never half-apply) plus identify job requeue. Scan
    /// recovery runs in the supervisor preamble. Re-running after a
    /// clean shutdown is a no-op. Never fails boot: a publish journal
    /// that cannot be reconciled is logged and left for an
    /// administrator, and managed writes stay closed until it clears.
    pub async fn run_recovery(&self) -> LibraryRecovery {
        // Recovery waits on the database and holds the cell's mutex, so it
        // runs off the async runtime.
        let setup = self.clone();
        let recovered = tokio::task::spawn_blocking(move || {
            let registry = setup.live_registry();
            // Startup is uncontended, so the guards cost nothing; taken
            // first so reconcile can never race a scan.
            let _guards = setup.publish_guards(&registry);
            let mut cell = setup
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let recoveries = cell
                .refresh(&registry, &setup.root_dirs)
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
            let held = cell.has_held().unwrap_or_else(|error| {
                tracing::warn!(%error, "publish journal unreadable; import leftovers kept");
                true
            });
            let settled = cell.cell.is_some() && !held;
            super::import::sweep_import_leftovers(&roots, settled);
            recoveries
        })
        .await;
        let recoveries = recovered.unwrap_or_else(|error| {
            tracing::error!(%error, "publish recovery panicked; managed writes stay closed");
            Vec::new()
        });
        let identify = self.identify_store.clone();
        match tokio::task::spawn_blocking(move || identify.recover()).await {
            Ok(Ok(0)) => {}
            Ok(Ok(requeued)) => {
                tracing::info!(requeued, "identify jobs from the previous run requeued");
            }
            // The jobs' leases run out and the queue takes them again.
            Ok(Err(error)) => tracing::warn!(%error, "identify jobs not requeued at startup"),
            Err(error) => tracing::warn!(%error, "identify job recovery did not run"),
        }
        LibraryRecovery {
            publish_recoveries: recoveries
                .into_iter()
                .map(|recovery| (recovery.bundle_id, format!("{:?}", recovery.action)))
                .collect(),
        }
    }

    /// Start the library background loops on `supervisor`. Called after
    /// startup recovery, from inside the runtime.
    pub fn start_loops(
        &self,
        supervisor: &droppedneedle_kernel::jobs::Supervisor,
    ) -> Result<(), droppedneedle_kernel::jobs::AlreadyRunning> {
        super::loops::start_loops(self, supervisor)
    }
}
