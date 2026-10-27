//! Library bundle: one setup the app mounts.
//!
//! [`LibrarySetup`] only builds: the scan coordinator, the identify
//! service and queue, the contribution service and worker, the publish
//! cell, and the services over them ([`Scans`], [`Activity`],
//! [`Identification`], [`Publishing`], [`Imports`]), which it hands out.
//! `build` binds everything over the production provider clients, or over
//! the provider ports a test gives in [`Endpoints`]. [`LibraryServices`]
//! adds the services built over the setup; it is the state of the routes
//! [`routes`] lists for the app to mount, and it starts the background
//! loops.
//!
//! Store durability: roots, scan state, identification, contributions,
//! and the publish journal live in the application database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::FromRef;

use super::activity::Activity;
use super::adapters::LoftyTagReader;
use super::contrib::ContribProviders;
use super::contrib::service::ContributionService;
use super::contrib::worker::VerificationWorker;
use super::corrections::service::Corrections;
use super::identify::service::{IdentifyDeps, IdentifyService};
use super::identify::sqlite::SqliteIdentifyStore;
use super::import::Imports;
use super::manage::{PublishCell, Publishing};
use super::management::enrich::EnrichmentPort;
use super::management::service::Management;
use super::management::settings::service::LibraryManagementService;
use super::mutations::Mutations;
use super::operations::service::Operations;
use super::reconcile::service::Reconcile;
use super::repairs::service::{RepairWorker, Repairs};
use super::scan::coordinator::{
    LibraryScanCoordinator, ResolverSource, ScanEventPublisher, SharedResolver,
};
use super::scan::pool::BlockingPool;
use super::scan::roots::RootRegistry;
use super::scan::sqlite_store::SqliteScanStore;
use super::scan::watcher::{DirtyScopes, WatcherState, WorkWakeups};
use super::scans::Scans;
use super::service::{Identification, ServiceError};
use super::settings::LibrarySettings;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_kernel::runtime_config::{Live, Plain};

/// Scan coordinator over the wired seams.
pub type ScanCoordinator = LibraryScanCoordinator<LoftyTagReader>;

/// The provider ports a test may put in place of the live clients.
#[derive(Clone)]
pub struct Endpoints {
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
pub fn routes() -> droppedneedle_kernel::mount::Routes<LibraryServices> {
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
    /// Genres, lyrics, covers and AcoustID ids Library Management plans
    /// with. `None` only in a test bundle with no outside sources:
    /// planning then asks nothing.
    pub enrichment: Option<Arc<dyn EnrichmentPort>>,
    /// Called after each scan run that changed the library completes
    /// (the library image refresh).
    pub scan_completed: ScanCompleted,
    /// The library's write side, shared with the plugin host's library
    /// writes.
    pub core: LibraryCore,
}

/// The library's write side: the publish cell, the roots it opens over
/// (with the dirty marks and wakeups a change to them sends the scan)
/// and the recycle bins. Built once, before the plugin
/// host, which adds plugin files through it, and handed to
/// [`LibrarySetup::build`], so the library and the host hold one cell.
#[derive(Clone)]
pub struct LibraryCore {
    pub(crate) ids: Arc<dyn IdGenerator>,
    /// Roots and path rules, re-read on every refresh.
    pub(crate) library: Live<TypedLibrary>,
    pub(crate) registry: Arc<SharedResolver>,
    pub(crate) root_dirs: RootDirs,
    pub(crate) dirty: DirtyScopes,
    pub(crate) wakeups: WorkWakeups,
    pub(crate) publish: Arc<std::sync::Mutex<PublishCell>>,
    pub(crate) placement: super::manage::RootPlacement,
    pub(crate) held_bundles: super::manage::HeldBundles,
    pub(crate) bins: super::publish::bin::BinSource,
    /// Wall time: the kernel clock.
    pub(crate) clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    /// The undo copies, in the folder beside the database.
    pub(crate) blobs: super::publish::BlobStore,
}

impl LibraryCore {
    /// Load the roots and open nothing yet: the cell opens on its first
    /// refresh. A commit queues its follow-up work on `queue`; a
    /// configured recycle bin stays clear of `download_trees`.
    pub fn build(
        platform: &droppedneedle_kernel::platform::Platform,
        queue: droppedneedle_kernel::jobs::queue::JobQueue,
        download_trees: super::publish::bin::DownloadTrees,
    ) -> Result<Self, String> {
        let settings =
            LibrarySettings::new(&platform.settings, platform.config.timezone.as_deref());
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
        let bins = super::publish::bin::BinSource::new(
            settings.download_policy.clone(),
            settings.library.clone(),
            download_trees,
        );
        let blobs = super::publish::BlobStore::new(droppedneedle_kernel::db::UndoDir::new(
            platform.config.undo_dir(),
        ));
        let publish = PublishCell::new(
            platform.db.clone(),
            bins.clone(),
            queue,
            platform.clock.clone(),
            blobs.clone(),
        );
        Ok(Self {
            ids: platform.ids.clone(),
            library: settings.library,
            registry,
            root_dirs,
            dirty: DirtyScopes::new(),
            wakeups: WorkWakeups::new(),
            placement: publish.placement(),
            publish: Arc::new(std::sync::Mutex::new(publish)),
            held_bundles: super::manage::HeldBundles::new(platform.db.clone()),
            bins,
            clock: platform.clock.clone(),
            blobs,
        })
    }

    /// Re-read the roots from the settings and swap them in when they
    /// changed. Roots that are new or changed get a dirty mark, so the
    /// next scan pass walks them. A settings read that fails keeps the
    /// current registry.
    pub fn refresh_registry(&self) -> RootRegistry {
        let current = self.registry.resolver().registry().clone();
        let fresh = match super::settings::registry(&self.library) {
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
        self.library
            .get()
            .map_err(|error| ServiceError::internal(&error))
    }

    /// The shared root registry the stream gateway resolves local reads
    /// against; every refresh swaps the roots in for all its holders.
    pub fn root_source(&self) -> SharedResolver {
        (*self.registry).clone()
    }
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
    /// Shared root-directory lookup.
    pub root_dirs: RootDirs,
    /// The jobs table: a removed album's clean-up is queued here in the
    /// removal's own commit.
    pub jobs: droppedneedle_kernel::jobs::queue::JobQueue,
    /// The recycle bins removals, upgrades and replaced files go to.
    pub bins: super::publish::bin::BinSource,
    /// Wall time: the kernel clock, which recycle-bin expiry reads.
    pub clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
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
    /// The write side the fields above share, which the plugin host holds
    /// too.
    pub core: LibraryCore,
    /// Scan requests, history and controls, and the scan loop's turns.
    pub scans: Scans,
    /// The activity feed and the identification pause switch.
    pub activity: Activity,
    /// Identification requests and reviews, and the identify loop's turns.
    pub identification: Identification,
    /// Previews, apply, undo, the other managed writes and publish
    /// upkeep.
    pub publishing: Publishing,
    /// Finished downloads brought into the library.
    pub imports: Imports,
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

        // MusicBrainz on the source the settings name, read per request.
        // Identification and imports are background work; a person waiting
        // (an edition plan, a curator's edition choice, a drop matched by
        // hand) goes ahead of it at the limiter. Both clients count their
        // failures toward system health, so an outage during a background
        // lookup shows on the health dot too.
        let mb_client =
            |priority: RequestPriority| providers.musicbrainz.live(mb_source.clone(), priority);
        // Identification and imports keep what MusicBrainz says about
        // merged recording ids in the one provider cache, whatever the
        // priority.
        let cached = |priority: RequestPriority| {
            super::identify::sources::CachedMusicBrainz::new(
                &providers.musicbrainz,
                mb_source.clone(),
                providers.cache.clone(),
                priority,
            )
        };
        let musicbrainz = super::identify::sources::Prioritised::new(
            cached(RequestPriority::BackgroundSync),
            cached(RequestPriority::UserInitiated),
        );
        // Downloads being imported look releases up through their own
        // clients on the same limiter and source setting.
        let imports: super::identify::sources::Prioritised<
            Arc<dyn super::identify::sources::ReleaseSource>,
        > = super::identify::sources::Prioritised::new(
            Arc::new(cached(RequestPriority::BackgroundSync)),
            Arc::new(cached(RequestPriority::UserInitiated)),
        );
        // Contributions read MusicBrainz on both lanes (the curator's page
        // and the verification worker) and Discogs through the one Discogs
        // client.
        let contrib = endpoints
            .contribution_providers
            .unwrap_or_else(|| ContribProviders {
                discogs: Arc::new(super::contrib::live::LiveDiscogsContrib::new(
                    providers.discogs.clone(),
                    platform.clock.clone(),
                )),
                musicbrainz: Arc::new(super::contrib::live::LiveMusicBrainzContrib::new(
                    mb_client(RequestPriority::UserInitiated),
                    mb_client(RequestPriority::BackgroundSync),
                )),
            });
        // Identification and imports look prints up through the one
        // AcoustID client.
        let import_acoustid = providers.acoustid.clone();
        let acoustid = providers.acoustid.clone();
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
        releases: super::identify::sources::Prioritised<
            Arc<dyn super::identify::sources::ReleaseSource>,
        >,
        import_acoustid: droppedneedle_providers::acoustid::AcoustIdClient,
    ) -> Result<Self, String> {
        let db = &platform.db;
        let settings =
            LibrarySettings::new(&platform.settings, platform.config.timezone.as_deref());
        let scan_store = Arc::new(SqliteScanStore::new(db.clone(), platform.clock.clone()));
        let LibraryPorts {
            enrichment,
            scan_completed,
            core,
        } = ports;
        let registry = core.registry.clone();
        let root_dirs = core.root_dirs.clone();
        let identify_store = Arc::new(SqliteIdentifyStore::new(db.clone(), platform.clock.clone()));
        let pool = BlockingPool::new(4);
        let wakeups = core.wakeups.clone();
        let coordinator = Arc::new(
            ScanCoordinator::new(
                scan_store.clone(),
                pool.clone(),
                Arc::new(LoftyTagReader),
                registry.clone(),
                platform.clock.clone(),
            )
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
        let jobs = queue.clone();
        let (contrib, contrib_worker) = super::contrib::assemble(
            db,
            queue,
            contrib_providers,
            Arc::new(super::contrib::live::IdentifyFollowUp::new(
                identify_store.clone(),
            )),
            platform.clock.clone(),
        );
        let fingerprints: Arc<dyn super::identify::sources::FingerprintSource> =
            Arc::new(super::identify::sources::AcoustIdFingerprints::new(
                import_acoustid,
                settings.library.clone(),
                (*registry).clone(),
                pool.clone(),
                identify_store.clone(),
            ));
        let scans = Scans {
            coordinator: coordinator.clone(),
            scan_store: scan_store.clone(),
            core: core.clone(),
            settings: settings.clone(),
            pool: pool.clone(),
            watcher_state: Arc::new(std::sync::Mutex::new(WatcherState::new())),
        };
        let activity = Activity {
            coordinator: coordinator.clone(),
            scan_store: scan_store.clone(),
            clock: platform.clock.clone(),
            wakeups: wakeups.clone(),
        };
        let identification = Identification {
            db: db.clone(),
            identify_store: identify_store.clone(),
            identify: identify.clone(),
            settings: settings.clone(),
            ids: platform.ids.clone(),
            clock: platform.clock.clone(),
        };
        let publishing = Publishing {
            db: db.clone(),
            core: core.clone(),
            identify_store: identify_store.clone(),
            scan_store: scan_store.clone(),
            settings: settings.clone(),
            previews: Arc::new(std::sync::Mutex::new(HashMap::new())),
            expiry: Arc::default(),
            jobs: jobs.clone(),
        };
        let imports = Imports {
            db: db.clone(),
            core: core.clone(),
            identify_store: identify_store.clone(),
            settings: settings.clone(),
            releases,
            fingerprints,
        };
        Ok(Self {
            ids: platform.ids.clone(),
            management_settings: Arc::new(LibraryManagementService::new(platform.settings.clone())),
            settings,
            registry,
            db: db.clone(),
            scan_store,
            coordinator,
            pool,
            dirty: core.dirty.clone(),
            wakeups,
            identify_store,
            identify,
            contrib,
            contrib_worker,
            publish: core.publish.clone(),
            placement: core.placement.clone(),
            held_bundles: core.held_bundles.clone(),
            leftover_files: super::manage::LeftoverFiles::new(db.clone(), platform.clock.clone()),
            root_dirs,
            jobs,
            bins: core.bins.clone(),
            clock: platform.clock.clone(),
            http: platform.http.shared().clone(),
            enrichment,
            core,
            scans,
            activity,
            identification,
            publishing,
            imports,
        })
    }
}

/// The library's services, built once over the setup. This is the library
/// router's state: a handler takes the service it calls as its state.
#[derive(Clone)]
pub struct LibraryServices {
    /// What the services are built over, for the loops and the acquire
    /// ports. No route takes it.
    pub setup: LibrarySetup,
    pub scans: Scans,
    pub activity: Activity,
    pub identification: Identification,
    pub publishing: Publishing,
    pub operations: Operations,
    pub repairs: Repairs,
    pub corrections: Corrections,
    pub reconcile: Reconcile,
    pub management: Management,
    pub mutations: Mutations,
    pub contrib: Arc<ContributionService>,
}

impl LibraryServices {
    /// Build every service once over `setup`.
    pub fn build(setup: &LibrarySetup) -> Self {
        let operations = operations(setup);
        Self {
            repairs: Repairs::new(
                setup.db.clone(),
                operations.clone(),
                setup.ids.clone(),
                setup.wakeups.clone(),
                setup.clock.clone(),
            ),
            corrections: Corrections::new(
                setup.db.clone(),
                setup.settings.clone(),
                setup.clock.clone(),
            ),
            reconcile: Reconcile::new(setup.db.clone(), setup.clock.clone()),
            management: Management::new(setup),
            mutations: Mutations::new(setup),
            contrib: setup.contrib.clone(),
            scans: setup.scans.clone(),
            activity: setup.activity.clone(),
            identification: setup.identification.clone(),
            publishing: setup.publishing.clone(),
            operations,
            setup: setup.clone(),
        }
    }

    /// Startup recovery: publish reconcile (resume-or-compensate, never
    /// half-apply) plus identify job requeue. Scan recovery runs in the
    /// supervisor preamble. Re-running after a clean shutdown is a no-op.
    /// Never fails boot: a publish journal that cannot be reconciled is
    /// logged and left for an administrator, and managed writes stay
    /// closed until it clears.
    pub async fn run_recovery(&self) -> LibraryRecovery {
        let recoveries = self.publishing.recover().await;
        self.identification.recover().await;
        LibraryRecovery {
            publish_recoveries: recoveries,
        }
    }

    /// A repairs worker that has followed nothing up yet. The repairs loop
    /// builds one at start and keeps it.
    pub fn repair_worker(&self) -> RepairWorker {
        let setup = &self.setup;
        RepairWorker::new(
            &self.repairs,
            setup.identify_store.clone(),
            setup.identify.clone(),
            setup.coordinator.clone(),
            setup.settings.management.clone(),
        )
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

/// The operations service over the setup's identify store and service.
pub(crate) fn operations(setup: &LibrarySetup) -> Operations {
    Operations::new(
        setup.db.clone(),
        setup.identify_store.clone(),
        setup.identify.clone(),
        setup.ids.clone(),
        setup.settings.management.clone(),
        setup.wakeups.clone(),
        setup.clock.clone(),
    )
}

/// Each service is a sub-state of [`LibraryServices`], so a handler takes
/// `State<Service>` for the one it calls.
macro_rules! substate {
    ($($field:ident: $service:ty),* $(,)?) => {
        $(impl FromRef<LibraryServices> for $service {
            fn from_ref(services: &LibraryServices) -> Self {
                services.$field.clone()
            }
        })*
    };
}

substate!(
    scans: Scans,
    activity: Activity,
    identification: Identification,
    publishing: Publishing,
    operations: Operations,
    repairs: Repairs,
    corrections: Corrections,
    reconcile: Reconcile,
    management: Management,
    mutations: Mutations,
    contrib: Arc<ContributionService>,
);
