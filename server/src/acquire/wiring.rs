//! Acquire bundle: one setup the app mounts.
//!
//! [`AcquireSetup`] owns the requests state, the imports deps, the flows
//! stores and loop deps, the unified dispatch, the live client set, the
//! health probes, and the download worker. Every durable piece sits on the
//! shared SQLite runtime ([`AcquireDb`]). Clients, quotas and the download
//! policy are resolved from the config store on use, so saving settings
//! takes effect without a restart. `for_tests` binds the same shape over a
//! scratch database. The routers nest under `/api/v3` inside the
//! deny-by-default session gate; only the Spotify OAuth callback mounts
//! outside it (it is state-token identified, like v2's ungated route).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{Router, extract::Request, middleware::Next, response::Response};
use futures_util::future::BoxFuture;

use super::db::AcquireDb;
use super::dispatch::{Journal, RetryPolicySource, UnifiedDispatch};
use super::downloads::orphans::RecycleBin;
use super::downloads::watchdog::{RetryPolicy, WatchdogConfig};
use super::flows::loops::{
    FollowDeps, PruneDeps, PruneSettings, SweepDeps, SyncDeps, ThreadJitter, TokioSleeper,
    WantedDeps, WantedSettings, register_ephemeral_loop, spawn_follow_loop, spawn_prune_loop,
    spawn_sweep_loop, spawn_sync_loop, spawn_wanted_loop,
};
use super::flows::operations::{DropImportDeps, OpStore, register_durable_ops};
use super::flows::seams::{
    Candidate, CandidateSearch, Clock, DropVerify, LibraryOrganise, MemoryHandoff, SystemClock,
    TickSink, VerifyVerdict,
};
use super::flows::stores::{
    AdminDirectory, FollowStore as FlowsFollowStore, LibraryPresence, QuarantineStore,
    UpgradePolicy, UpgradeWorklist,
};
use super::imports::handlers::{
    ImportsDeps, imports_callback_router, imports_gated_router, imports_legacy_callback_router,
};
use super::imports::jobs::{JobRegistry, QueuedSpotifyImport, TaskExecutor};
use super::imports::lidarr::LidarrClient;
use super::imports::spotify::{
    FixedMbidResolver, PlaylistIndex, PlaylistTrackSink, SpotifyClient, SpotifyImportService,
};
#[cfg(any(test, feature = "test-support"))]
use super::imports::spotify::{
    MemoryPlaylistIndex, MemorySpotifyConnections, MemorySpotifyStates, MemoryTrackSink,
};
use super::imports::spotify_store::{
    CollectionsPlaylistBridge, SqliteSpotifyLinks, SqliteSpotifyStates,
};
use super::landing::ports::LandingLibrary;
use super::landing::{LandingService, LandingSettings, LibrarySlot};
use super::probes::{LiveProbes, ProbeCache, ProbeInputs, refresh_probes, seed_from_config};
use super::requests::quota::{QuotaLedger, QuotaPolicy};
use super::requests::sqlite::{RequestStore, WantedStore};
use super::requests::state::RequestsState;
use super::search::{FanoutSearch, ReleasePollSlot, SlotPoll};
use super::settings::{
    ApprovalSeedBridge, CollectionsFollowBridge, ConfigLidarrSettings, ConfigSpotifySettings,
    FollowDecisionBridge, RequestsApprovalBridge, RequestsPendingSource,
};
use super::slskd::{DownloadPolicy as SlskdPolicy, ReqwestSlskdHttp, SlskdClient, SlskdRepository};
use super::sources::{SabnzbdSource, SlskdSource};
use super::target::Targets;
use super::target::lookup::AlbumLookup;
use super::usenet::newznab::{NewznabClient, NewznabIndexer, NewznabIndexerEntry};
use super::usenet::policy::{QualityTier, UsenetPolicy};
use super::usenet::prowlarr::{ProwlarrClient, ProwlarrIndexer};
use super::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
use super::worker::{DownloadWorker, Source, SourceSet, WorkerConfig, run_startup_recovery};
use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::UsersDeps;
use crate::auth::users::roles::Role as AuthRole;
use crate::auth::users::stores::StoreError;
use crate::config::AppConfig;
use crate::db::{DurableWorkWakeups, JobState, WriteLane};
use crate::events::{EventSink, PlaylistImported, UserNotice, new_event_id};
use crate::ids::IdGenerator;
use crate::reads::collections::state::CollectionsState;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::{
    AdvancedSettings, DownloadClients, LidarrImportConnection, NewznabIndexer as ConfigIndexer,
    ProwlarrConnection, SecretSection, SlskdConnection, TypedLibrary,
};
use crate::runtime_config::sections::{
    DownloadPolicy, FreeMusic, Section, SourcePriority, UsenetBackendSetting, UserPreferences,
    WantedWatcher,
};

/// Spotify API bases (v2 `spotify_client.py`).
const SPOTIFY_API_BASE: &str = "https://api.spotify.com/v1";
const SPOTIFY_ACCOUNTS_BASE: &str = "https://accounts.spotify.com";

/// Health-probe refresh cadence.
const PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Registry name for the probe refresh loop.
const PROBE_JOB: &str = "acquire-probe-refresh";

/// Clamp a config integer to a non-negative `u32` (0 = unlimited).
fn clamp_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// Clamp a config integer to a non-negative `u64`.
fn clamp_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(0)
}

/// Clamp a config integer to a non-negative `usize`.
fn clamp_usize(value: i64) -> usize {
    usize::try_from(value.max(0)).unwrap_or(usize::MAX)
}

/// Read one plain section, logging and falling back to the defaults when
/// the stored value cannot be read.
fn plain<S: Section>(store: &ConfigStore) -> S {
    store.get::<S>().unwrap_or_else(|error| {
        tracing::warn!(section = S::KEY, %error, "settings read failed; using defaults");
        S::default()
    })
}

/// Read one secret section with its real secrets, logging and falling back
/// to the defaults (unconfigured) when it cannot be read.
fn secret<S: SecretSection>(store: &ConfigStore) -> S {
    store.get_raw::<S>().unwrap_or_else(|error| {
        tracing::warn!(section = S::KEY, %error, "settings read failed; treating as unconfigured");
        S::default()
    })
}

/// The settings every acquisition client is built from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ClientSettings {
    /// slskd connection.
    pub slskd: SlskdConnection,
    /// SABnzbd connection.
    pub sabnzbd: DownloadClients,
    /// Native Newznab indexers.
    pub indexers: Vec<ConfigIndexer>,
    /// Prowlarr connection.
    pub prowlarr: ProwlarrConnection,
    /// Lidarr import connection.
    pub lidarr: LidarrImportConnection,
    /// Download policy (quality gates, timeouts).
    pub policy: DownloadPolicy,
    /// Free Music settings.
    pub free: FreeMusic,
    /// Active Usenet search side.
    pub backend: UsenetBackendSetting,
    /// Library root folders (the mount probe compares filesystems).
    pub library_roots: Vec<PathBuf>,
}

impl ClientSettings {
    /// Current settings from the config store.
    pub fn read(store: &ConfigStore) -> Self {
        let indexers = store.get_indexers_raw().unwrap_or_else(|error| {
            tracing::warn!(%error, "indexer settings read failed; treating as none");
            Vec::new()
        });
        Self {
            slskd: secret(store),
            sabnzbd: secret(store),
            indexers,
            prowlarr: secret(store),
            lidarr: secret(store),
            policy: plain(store),
            free: plain(store),
            backend: plain(store),
            library_roots: plain::<TypedLibrary>(store)
                .library_roots
                .into_iter()
                .map(|root| PathBuf::from(root.path))
                .collect(),
        }
    }
}

/// One built set of clients for the current settings.
pub struct ClientSet {
    /// Candidate search for the flows loops.
    pub search: Arc<FanoutSearch>,
    /// Download sources for the worker.
    pub sources: SourceSet,
    /// Health probe inputs.
    pub probe_inputs: Arc<ProbeInputs>,
}

/// Acquisition clients resolved from the live settings. The set rebuilds
/// only when the settings it was built from change, so per-client caches
/// survive between uses while a saved setting takes effect on the next use.
pub struct LiveClients {
    http: reqwest::Client,
    settings: Arc<dyn Fn() -> ClientSettings + Send + Sync>,
    slskd_downloads: PathBuf,
    journal: Arc<Journal>,
    built: Mutex<Option<(ClientSettings, Arc<ClientSet>)>>,
    plugins: PluginSlot,
    targets: Arc<Targets>,
}

/// The plugin host, set once boot has built it. Plugin sources and
/// usenet-targeting plugin indexers join the pipeline through it.
pub type PluginSlot = Arc<std::sync::OnceLock<Arc<crate::plugins::host::PluginHost>>>;

impl LiveClients {
    /// Resolver over a settings source. `slskd_downloads` is the slskd
    /// downloads mount (`SLSKD_DOWNLOADS_PATH`).
    pub fn new(
        http: reqwest::Client,
        settings: Arc<dyn Fn() -> ClientSettings + Send + Sync>,
        slskd_downloads: PathBuf,
        journal: Arc<Journal>,
        targets: Arc<Targets>,
    ) -> Self {
        Self {
            http,
            settings,
            slskd_downloads,
            journal,
            built: Mutex::new(None),
            plugins: Arc::new(std::sync::OnceLock::new()),
            targets,
        }
    }

    /// What each task fetches, shared by every source.
    pub fn targets(&self) -> &Arc<Targets> {
        &self.targets
    }

    /// The slot the plugin host goes into.
    pub fn plugins(&self) -> &PluginSlot {
        &self.plugins
    }

    /// The built-in sources for the current settings plus one source per
    /// enabled plugin download client.
    pub fn sources_with_plugins(&self, policy: &DownloadPolicy) -> super::worker::SourceSet {
        let built = self.current().sources.clone();
        let Some(host) = self.plugins.get() else {
            return built;
        };
        let keys = host.download_source_keys();
        if keys.is_empty() {
            return built;
        }
        let release_policy = super::plugin_source::ReleasePolicy {
            usenet: usenet_policy_from(policy),
            ignored_terms: policy.ignored_terms.clone(),
            required_terms: policy.required_terms.clone(),
        };
        let mut sources: Vec<Source> = built.iter().cloned().collect();
        for key in keys {
            sources.push(Source::Plugin(Arc::new(
                super::plugin_source::PluginDownloadSource::new(
                    Arc::clone(host),
                    key,
                    self.journal.clone(),
                    release_policy.clone(),
                )
                .with_targets(self.targets.clone()),
            )));
        }
        Arc::new(sources)
    }

    /// The client set for the current settings.
    pub fn current(&self) -> Arc<ClientSet> {
        let settings = (self.settings)();
        let mut built = match self.built.lock() {
            Ok(built) => built,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some((seen, set)) = built.as_ref()
            && *seen == settings
        {
            return set.clone();
        }
        let set = Arc::new(self.build(&settings));
        *built = Some((settings, set.clone()));
        set
    }

    fn build(&self, settings: &ClientSettings) -> ClientSet {
        let http = &self.http;
        let usenet_policy = usenet_policy_from(&settings.policy);
        let slskd_repo = slskd_repository(http, &settings.slskd, &self.slskd_downloads);
        let sab_queue = sabnzbd_queue(http, &settings.sabnzbd, &usenet_policy);
        let newznab = Arc::new(newznab_indexer(http, &settings.indexers));
        let prowlarr = Arc::new(prowlarr_indexer(http, &settings.prowlarr));
        let search = Arc::new(FanoutSearch::new(
            slskd_repo.clone(),
            newznab.clone(),
            prowlarr.clone(),
            settings.backend,
            usenet_policy.indexer_timeout,
        ));
        let mut sources = Vec::new();
        if let Some(repo) = slskd_repo.clone()
            && settings.slskd.enabled
        {
            sources.push(Source::Slskd(Arc::new(
                SlskdSource::new(repo, self.journal.clone()).with_targets(self.targets.clone()),
            )));
        }
        if let Some(queue) = sab_queue.clone()
            && settings.sabnzbd.sabnzbd.enabled
        {
            sources.push(Source::Sab(Arc::new(
                SabnzbdSource::new(
                    queue,
                    newznab.clone(),
                    prowlarr.clone(),
                    settings.backend,
                    usenet_policy,
                    self.journal.clone(),
                    Some(settings.sabnzbd.sabnzbd.category.clone()),
                    Duration::from_secs(30),
                )
                .with_plugins(self.plugins.clone())
                .with_targets(self.targets.clone()),
            )));
        }
        let probe_inputs = Arc::new(ProbeInputs {
            slskd: slskd_repo,
            slskd_enabled: settings.slskd.enabled,
            slskd_mount_base: self.slskd_downloads.clone(),
            slskd_subpath: settings.slskd.downloads_subpath.clone(),
            library_roots: settings.library_roots.clone(),
            sabnzbd: sab_queue,
            sabnzbd_section: settings.sabnzbd.clone(),
            newznab,
            newznab_entries: settings.indexers.clone(),
            prowlarr,
            prowlarr_section: settings.prowlarr.clone(),
            lidarr: LidarrClient::new(http.clone()),
            lidarr_section: settings.lidarr.clone(),
            free: settings.free,
        });
        ClientSet {
            search,
            sources: Arc::new(sources),
            probe_inputs,
        }
    }
}

/// Candidate search over the live client set.
struct LiveSearch {
    clients: Arc<LiveClients>,
}

impl CandidateSearch for LiveSearch {
    fn search_album<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        Box::pin(async move {
            let search = self.clients.current().search.clone();
            search.search_album(artist, title).await
        })
    }
}

/// Everything `create_app` needs to mount the acquisition routes, built once.
#[derive(Clone)]
pub struct AcquireSetup {
    /// Shared database handle.
    pub db: AcquireDb,
    /// Request intake state over the durable stores.
    pub requests: RequestsState,
    /// Imports deps (config-backed settings, bridged follows).
    pub imports: ImportsDeps,
    /// Auth bundle, for principal translation and admin refresh.
    pub users: UsersDeps,
    /// Unified dispatch behind both dispatch spellings.
    pub dispatch: Arc<UnifiedDispatch>,
    /// Shared download journal.
    pub journal: Arc<Journal>,
    /// Flows stores and loop deps.
    pub flows: Arc<FlowsBundle>,
    /// Steady-state download worker.
    pub worker: Arc<DownloadWorker>,
    /// Acquisition clients over the live settings.
    pub clients: Arc<LiveClients>,
    /// Live health probes.
    pub probes: Arc<LiveProbes>,
    /// Probe cache the refresh loop fills.
    pub probe_cache: Arc<ProbeCache>,
    /// Per-task staging root (manifests, drop jobs, quarantine).
    pub staging_root: PathBuf,
    /// Store-prune deps (request history and wanted rows).
    pub prune: Arc<PruneDeps>,
    /// Live event hub handle shared by the flows and the imports.
    pub events: EventSink,
    /// Finished-download import: verify, match, publish or hold.
    pub landing: Arc<LandingService>,
}

/// Flows stores plus the loop deps built over them.
#[derive(Clone)]
pub struct FlowsBundle {
    /// Wanted watches (shared with the wanted view).
    pub watches: WantedStore,
    /// Request ledger (shared with intake).
    pub ledger: RequestStore,
    /// Follow-poll cursors.
    pub follows: FlowsFollowStore,
    /// Upgrade worklist (empty until a library scan fills it).
    pub worklist: UpgradeWorklist,
    /// Drop-import quarantine.
    pub quarantine: QuarantineStore,
    /// Library presence, read from the catalog.
    pub library: Arc<LibraryPresence>,
    /// Sweep ownership directory (refreshed from auth at boot).
    pub admins: Arc<AdminDirectory>,
    /// Plugin ticks.
    pub ticks: Arc<dyn TickSink>,
    /// Free-music landing handoff (memory records; file staging waits on
    /// the path-carrying handoff).
    pub handoff: Arc<MemoryHandoff>,
    /// Durable operation records.
    pub ops: OpStore,
    /// Wanted-watcher deps.
    pub wanted_deps: Arc<WantedDeps>,
    /// Follow-poll deps.
    pub follow_deps: Arc<FollowDeps>,
    /// Where boot attaches the follow poll's MusicBrainz reader.
    pub release_poll: ReleasePollSlot,
    /// Upgrade-sweep deps.
    pub sweep_deps: Arc<SweepDeps>,
    /// Status-sync deps.
    pub sync_deps: Arc<SyncDeps>,
}

impl FlowsBundle {
    /// Production drop-import deps over the shared stores: extension
    /// verify plus resolve-into-staging organise (the library engine owns
    /// real identification and placement).
    pub fn drop_deps(&self, staging_root: &Path) -> DropImportDeps {
        DropImportDeps {
            verify: Arc::new(ExtensionVerify),
            organise: Arc::new(StagingOrganise::new(staging_root.to_owned())),
            quarantine: self.quarantine.clone(),
            ledger: self.ledger.clone(),
            ticks: self.ticks.clone(),
            clock: Arc::new(SystemClock),
        }
    }
}

/// Drop-file verify by audio extension, matching the formats the library
/// imports. Unknown types fail open as local faults (never quarantined):
/// the library engine owns identification.
struct ExtensionVerify;

impl DropVerify for ExtensionVerify {
    fn verify(&self, file_name: &str) -> VerifyVerdict {
        let ext = file_name
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_lowercase();
        match ext.as_str() {
            "flac" | "mp3" | "ogg" | "oga" | "opus" | "m4a" | "m4b" | "mp4" | "aac" | "wav" => {
                VerifyVerdict::Ok
            }
            _ => VerifyVerdict::LocalFault(format!("unsupported file type: {ext}")),
        }
    }
}

/// Library organise into the staging `resolved` dir. Real library
/// placement belongs to the library engine; until it is wired, drops resolve out of
/// quarantine into a visible staging area instead of the library.
struct StagingOrganise {
    staging_root: PathBuf,
}

impl StagingOrganise {
    fn new(staging_root: PathBuf) -> Self {
        Self { staging_root }
    }
}

impl LibraryOrganise for StagingOrganise {
    fn organise(&self, job_id: &str, staged_path: &str) -> Result<String, String> {
        let source = PathBuf::from(staged_path);
        let name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "drop.bin".to_owned());
        let dir = self.staging_root.join("resolved").join(job_id);
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("cannot create resolve dir: {error}"))?;
        let dest = dir.join(name);
        if dest.exists() {
            return Err(format!(
                "resolve destination is occupied: {}",
                dest.display()
            ));
        }
        std::fs::rename(&source, &dest).map_err(|error| format!("cannot resolve drop: {error}"))?;
        Ok(dest.to_string_lossy().into_owned())
    }
}

/// After a landing settles a task, resolve its requests and wanted watch
/// right away through the status-sync rules.
pub fn settled_hook(flows: Arc<FlowsBundle>) -> super::worker::SettledHook {
    Arc::new(move |task, status| {
        let flows = flows.clone();
        Box::pin(async move {
            let now = super::db::to_i64(super::db::now_epoch());
            let completed_album = (status == super::downloads::state::TaskStatus::Completed
                && task.download_type == "album")
                .then_some(task.release_group_mbid.as_str());
            super::flows::loops::settle_task(
                now,
                &flows.sync_deps,
                &flows.watches,
                &task.id,
                completed_album,
            )
            .await;
        })
    })
}

/// Landing settings from the download-policy section: the quality band
/// landed files must sit in.
fn landing_settings(policy: &DownloadPolicy) -> LandingSettings {
    LandingSettings {
        quality_min: policy.quality_min.clone(),
        quality_max: policy.quality_max.clone(),
    }
}

/// Live retry policy from the download-policy section.
fn retry_policy_from(policy: &DownloadPolicy) -> RetryPolicy {
    RetryPolicy {
        enabled: policy.auto_retry_enabled,
        max_attempts: clamp_u32(policy.auto_retry_max_attempts),
        base_interval_minutes: policy.auto_retry_base_interval_minutes.max(0) as f64,
        cap_seconds: 86_400.0,
    }
}

/// Live quota policy from the download-policy section.
fn quota_policy_from(policy: &DownloadPolicy) -> QuotaPolicy {
    QuotaPolicy {
        request_count: clamp_u32(policy.default_request_quota_count),
        request_days: clamp_u32(policy.default_request_quota_days).max(1),
        storage_gb_per_user: clamp_u64(policy.default_storage_quota_gb),
        max_library_gb: clamp_u64(policy.max_library_size_gb),
    }
}

/// The pieces shared by the production and test bundles.
struct Core {
    requests: RequestsState,
    dispatch: Arc<UnifiedDispatch>,
    journal: Arc<Journal>,
    flows: Arc<FlowsBundle>,
}

/// Build the durable core: journal, dispatch, requests state and the flows
/// bundle, all over one database, with the bridges into collections
/// connected both ways.
#[allow(clippy::too_many_arguments)]
fn core(
    db: &AcquireDb,
    ids: Arc<dyn IdGenerator>,
    staging_root: &Path,
    retry: RetryPolicySource,
    quota: Arc<QuotaLedger>,
    search: Arc<dyn CandidateSearch>,
    wanted_settings: Arc<dyn Fn() -> WantedSettings + Send + Sync>,
    upgrade_policy: Arc<dyn Fn() -> UpgradePolicy + Send + Sync>,
    preferences: Arc<dyn Fn() -> UserPreferences + Send + Sync>,
    collections: &mut CollectionsState,
    plugins: PluginSlot,
    events: EventSink,
) -> Core {
    let journal = Arc::new(Journal::new(db.clone()));
    let dispatch = Arc::new(UnifiedDispatch::new(
        journal.clone(),
        ids,
        staging_root.to_owned(),
        retry,
    ));
    let mut requests = RequestsState::new(db, quota, dispatch.clone());
    requests.follow_sink = Some(Arc::new(FollowDecisionBridge::new(
        collections.stores.follows.clone(),
    ))
        as Arc<dyn super::requests::bridges::FollowDecisionSink>);
    collections.acquire_approvals = Some(Arc::new(RequestsPendingSource::new(
        requests.follows.clone(),
    ))
        as Arc<dyn crate::reads::collections::state::PendingApprovalsSource>);
    collections.approval_seeds = Some(Arc::new(ApprovalSeedBridge::new(requests.follows.clone()))
        as Arc<dyn crate::reads::collections::state::ApprovalSeedSink>);
    requests.plugins = plugins.clone();
    let ticks: Arc<dyn TickSink> = Arc::new(super::plugin_events::FlowTicks::new(plugins, events));
    let flows = Arc::new(flows_bundle(
        db,
        &requests,
        search,
        dispatch.clone(),
        wanted_settings,
        upgrade_policy,
        preferences,
        ticks,
    ));
    Core {
        requests,
        dispatch,
        journal,
        flows,
    }
}

impl AcquireSetup {
    /// Build the production bundle over the serving database. Credentials
    /// and tuning are read from the config store on use (decrypted in
    /// memory only).
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        db: AcquireDb,
        config: &AppConfig,
        users: UsersDeps,
        http: &crate::http_client::HttpClientFactory,
        ids: Arc<dyn IdGenerator>,
        config_store: Arc<ConfigStore>,
        collections: &mut CollectionsState,
    ) -> Result<Self, String> {
        let (http, no_redirect) = (http.shared().clone(), http.no_redirect().clone());
        let staging_root = config.imports_dir();
        let policy_store = config_store.clone();
        let retry: RetryPolicySource =
            Arc::new(move || retry_policy_from(&plain::<DownloadPolicy>(&policy_store)));
        let quota_store = config_store.clone();
        let quota = Arc::new(QuotaLedger::new(
            Arc::new(move || quota_policy_from(&plain::<DownloadPolicy>(&quota_store))),
            db.clone(),
        ));
        let wanted_store = config_store.clone();
        let wanted_settings = Arc::new(move || {
            let section: WantedWatcher = plain(&wanted_store);
            WantedSettings {
                enabled: section.enabled,
                watch_partial_albums: section.watch_partial_albums,
                max_checks_per_sweep: clamp_usize(section.max_checks_per_sweep).max(1),
                auto_download_on_find: section.auto_download_on_find,
            }
        });
        let sweep_store = config_store.clone();
        let upgrade_policy = Arc::new(move || {
            let section: DownloadPolicy = plain(&sweep_store);
            UpgradePolicy {
                upgrade_allowed: section.upgrade_allowed,
                scan_enabled: section.background_upgrade_scan_enabled,
                max_per_run: clamp_usize(section.background_upgrade_max_per_run).max(1),
                interval_hours: clamp_u64(section.background_upgrade_scan_interval_hours).max(1),
            }
        });
        let journal_for_clients = Arc::new(Journal::new(db.clone()));
        let settings_store = config_store.clone();
        let clients = Arc::new(LiveClients::new(
            http.clone(),
            Arc::new(move || ClientSettings::read(&settings_store)),
            config.slskd_downloads_path.clone(),
            journal_for_clients.clone(),
            Arc::new(Targets::new(journal_for_clients, staging_root.clone())),
        ));
        let search: Arc<dyn CandidateSearch> = Arc::new(LiveSearch {
            clients: clients.clone(),
        });
        let events = EventSink::default();
        let core = core(
            &db,
            ids.clone(),
            &staging_root,
            retry,
            quota,
            search,
            wanted_settings,
            upgrade_policy,
            {
                let preferences_store = config_store.clone();
                Arc::new(move || plain::<UserPreferences>(&preferences_store))
            },
            collections,
            clients.plugins().clone(),
            events.clone(),
        );
        let prune_store = config_store.clone();
        let prune = Arc::new(PruneDeps {
            settings: Arc::new(move || {
                let section: AdvancedSettings = plain(&prune_store);
                PruneSettings {
                    retention_days: clamp_u64(section.request_history_retention_days),
                    interval_hours: clamp_u64(section.store_prune_interval_hours),
                }
            }),
            ledger: core.requests.store.clone(),
            watches: core.requests.wanted.clone(),
        });

        // Imports deps.
        let lidarr_settings = Arc::new(ConfigLidarrSettings::new(config_store.clone()));
        let spotify_settings = Arc::new(ConfigSpotifySettings::new(config_store.clone()));
        let spotify_states = Arc::new(SqliteSpotifyStates::new(db.clone()));
        let spotify_links = Arc::new(SqliteSpotifyLinks::new(db.clone(), users.crypto.clone()));
        let spotify_playlists = Arc::new(CollectionsPlaylistBridge::new(
            collections.stores.playlists.clone(),
            db.clone(),
        ));
        let playlists: Arc<dyn PlaylistIndex> = spotify_playlists.clone();
        let tracks: Arc<dyn PlaylistTrackSink> = spotify_playlists;
        let resolver = Arc::new(FixedMbidResolver::new());
        let spotify_client = SpotifyClient::new(
            http.clone(),
            no_redirect,
            SPOTIFY_API_BASE,
            SPOTIFY_ACCOUNTS_BASE,
        );
        let spotify_service = Arc::new(SpotifyImportService::new(
            spotify_client.clone(),
            spotify_settings.clone(),
            spotify_links.clone(),
            playlists.clone(),
            tracks.clone(),
            resolver.clone(),
        ));
        let jobs = Arc::new(JobRegistry::new());
        let runner = spotify_service.clone();
        let import_events = events.clone();
        let executor = Arc::new(TaskExecutor::new(
            jobs.clone(),
            move |job: QueuedSpotifyImport| {
                let service = runner.clone();
                let events = import_events.clone();
                tokio::spawn(async move {
                    let result = service
                        .populate_playlist(&job.user_id, &job.spotify_playlist_id, &job.playlist_id)
                        .await;
                    if result.is_ok() {
                        announce_playlist_imported(&events, &job);
                    }
                    let result = result.map_err(|error| {
                        if let super::imports::spotify::SpotifyError::Store(cause) = &error {
                            tracing::error!(%cause, "spotify import write failed");
                        }
                        error.user_message()
                    });
                    (job.playlist_id.clone(), result)
                })
            },
        ));
        let boot_settings = ClientSettings::read(&config_store);
        let probe_cache = Arc::new(ProbeCache::new(seed_from_config(
            &boot_settings.slskd,
            &boot_settings.sabnzbd,
            &boot_settings.indexers,
            &boot_settings.prowlarr,
            &boot_settings.lidarr,
            &boot_settings.free,
        )));
        let probes = Arc::new(LiveProbes::new(probe_cache.clone()));
        let imports = ImportsDeps {
            http: http.clone(),
            lidarr: LidarrClient::new(http.clone()),
            lidarr_settings,
            follows: Arc::new(CollectionsFollowBridge::new(
                collections.stores.follows.clone(),
            )),
            approvals: Arc::new(RequestsApprovalBridge::new(core.requests.follows.clone())),
            spotify: spotify_client,
            spotify_settings,
            spotify_states,
            spotify_links,
            playlists,
            tracks,
            resolver,
            jobs,
            executor,
            probes: Arc::new(LiveProbes::bundle(&probes)),
            auth: users.clone(),
            ids: ids.clone(),
            base_path: config.base_path.clone(),
        };

        let landing_store = config_store.clone();
        let landing = Arc::new(LandingService::new(
            core.journal.clone(),
            LibrarySlot::default(),
            Arc::new(move || landing_settings(&plain::<DownloadPolicy>(&landing_store))),
            config.cache_dir.join("held"),
        ));

        // Worker over the live sources and tuning.
        let worker_clients = clients.clone();
        let worker_store = config_store.clone();
        let worker_staging = staging_root.clone();
        let sources_store = config_store.clone();
        let worker_plugins = clients.plugins().clone();
        let worker = Arc::new(
            DownloadWorker::new(
                core.journal.clone(),
                Arc::new(move || {
                    let policy: DownloadPolicy = plain(&sources_store);
                    worker_clients.sources_with_plugins(&policy)
                }),
                Arc::new(move || {
                    let policy: DownloadPolicy = plain(&worker_store);
                    let source_priority: SourcePriority = plain(&worker_store);
                    let sab: DownloadClients = secret(&worker_store);
                    let sab_mount = (!sab.sabnzbd.url.is_empty())
                        .then(|| PathBuf::from(sab.sabnzbd.downloads_mount.clone()));
                    worker_config(&policy, &source_priority, &worker_staging, sab_mount)
                }),
            )
            .with_plugin_events(worker_plugins)
            .with_events(events.clone())
            .with_landing(landing.clone())
            .with_settled(settled_hook(core.flows.clone())),
        );

        Ok(Self {
            db,
            requests: core.requests,
            imports,
            users,
            dispatch: core.dispatch,
            journal: core.journal,
            flows: core.flows,
            worker,
            clients,
            probes,
            probe_cache,
            staging_root,
            prune,
            events,
            landing,
        })
    }

    /// Send flow notices and import completions to `hub`.
    /// [`crate::AppState::with_events`] calls this.
    pub fn attach_events(&self, hub: &crate::events::EventHub) {
        self.events.attach(hub);
    }

    /// Let finished downloads reach the library: the landing looks
    /// releases up, checks what the library holds, and publishes through
    /// this port. Boot calls this once the library bundle exists.
    pub fn with_library(self, library: Arc<dyn LandingLibrary>) -> Self {
        if self.landing.library_slot().set(library).is_err() {
            tracing::warn!("library port was already attached to acquisition");
        }
        self
    }

    /// Let acquisition read MusicBrainz: edition tracklists, and the album
    /// a single track is on. Boot calls this once with the live client.
    pub fn with_album_lookup(self, lookup: Arc<dyn AlbumLookup>) -> Self {
        if self.clients.targets().lookup_slot().set(lookup).is_err() {
            tracing::warn!("MusicBrainz lookups were already attached to acquisition");
        }
        self
    }

    /// Let the follow poll read followed artists' releases from
    /// MusicBrainz. Boot calls this once with the live poll; until then
    /// the follow poll records each attempt as failed and retries later.
    pub fn with_release_poll(self, poll: Arc<dyn super::flows::seams::ReleasePoll>) -> Self {
        if self.flows.release_poll.set(poll).is_err() {
            tracing::warn!("follow release poll was already attached to acquisition");
        }
        self
    }

    /// Let enabled plugins act as download sources and feed usenet. Boot
    /// calls this once with the plugin host.
    pub fn with_plugins(self, host: Arc<crate::plugins::host::PluginHost>) -> Self {
        if self.clients.plugins().set(host).is_err() {
            tracing::warn!("plugin host was already attached to acquisition");
        }
        self
    }

    /// Test bundle over a scratch database, default settings and the
    /// memory import stores. Bridges stay connected (collections reads
    /// share the requests approval store) so hooked-state apps behave like
    /// production. Call inside a tokio runtime.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(
        users: UsersDeps,
        ids: Arc<dyn IdGenerator>,
        collections: &mut CollectionsState,
    ) -> Result<Self, String> {
        use super::imports::health::{
            HealthProbes, ScriptedFree, ScriptedLidarr, ScriptedNewznab, ScriptedSabnzbd,
            ScriptedSlskd,
        };
        use super::imports::lidarr::{MemoryApprovalSink, MemoryFollowStore, MemoryLidarrSettings};
        use super::imports::spotify::MemorySpotifySettings;

        let db = AcquireDb::scratch()?;
        // Under the scratch database's directory, so it goes with it.
        let staging_root = db
            .path()
            .parent()
            .map(|dir| dir.join("staging"))
            .ok_or_else(|| "scratch database has no directory".to_owned())?;
        let factory =
            crate::http_client::HttpClientFactory::new().map_err(|error| error.to_string())?;
        let http = factory.shared().clone();
        let clients = Arc::new(LiveClients::new(
            http.clone(),
            Arc::new(ClientSettings::default),
            staging_root.clone(),
            Arc::new(Journal::new(db.clone())),
            Arc::new(Targets::new(
                Arc::new(Journal::new(db.clone())),
                staging_root.clone(),
            )),
        ));
        let events = EventSink::default();
        let core = core(
            &db,
            ids.clone(),
            &staging_root,
            Arc::new(RetryPolicy::default),
            Arc::new(QuotaLedger::unlimited(db.clone())),
            Arc::new(LiveSearch {
                clients: clients.clone(),
            }),
            Arc::new(WantedSettings::default),
            Arc::new(UpgradePolicy::default),
            Arc::new(UserPreferences::default),
            collections,
            clients.plugins().clone(),
            events.clone(),
        );
        let spotify_client = SpotifyClient::new(
            http.clone(),
            factory.no_redirect().clone(),
            SPOTIFY_API_BASE,
            SPOTIFY_ACCOUNTS_BASE,
        );
        let spotify_settings = Arc::new(MemorySpotifySettings::new());
        let spotify_links = Arc::new(MemorySpotifyConnections::new());
        let playlists = Arc::new(MemoryPlaylistIndex::new());
        let tracks = Arc::new(MemoryTrackSink::new());
        let resolver = Arc::new(FixedMbidResolver::new());
        let service = Arc::new(SpotifyImportService::new(
            spotify_client.clone(),
            spotify_settings.clone(),
            spotify_links.clone(),
            playlists.clone(),
            tracks.clone(),
            resolver.clone(),
        ));
        let jobs = Arc::new(JobRegistry::new());
        let import_events = events.clone();
        let executor = Arc::new(TaskExecutor::new(
            jobs.clone(),
            move |job: QueuedSpotifyImport| {
                let service = service.clone();
                let events = import_events.clone();
                tokio::spawn(async move {
                    let result = service
                        .populate_playlist(&job.user_id, &job.spotify_playlist_id, &job.playlist_id)
                        .await
                        .map_err(|_| "spotify import failed".to_owned());
                    if result.is_ok() {
                        announce_playlist_imported(&events, &job);
                    }
                    (job.playlist_id.clone(), result)
                })
            },
        ));
        let imports = ImportsDeps {
            http: http.clone(),
            lidarr: LidarrClient::new(http),
            lidarr_settings: Arc::new(MemoryLidarrSettings::new()),
            follows: Arc::new(MemoryFollowStore::new()),
            approvals: Arc::new(MemoryApprovalSink::new()),
            spotify: spotify_client,
            spotify_settings,
            spotify_states: Arc::new(MemorySpotifyStates::new()),
            spotify_links,
            playlists,
            tracks,
            resolver,
            jobs,
            executor,
            probes: Arc::new(HealthProbes {
                slskd: Arc::new(ScriptedSlskd::new()),
                sabnzbd: Arc::new(ScriptedSabnzbd::new()),
                newznab: Arc::new(ScriptedNewznab::new()),
                lidarr: Arc::new(ScriptedLidarr::new()),
                free: Arc::new(ScriptedFree::new()),
            }),
            auth: users.clone(),
            ids: ids.clone(),
            base_path: String::new(),
        };
        let held_dir = db
            .path()
            .parent()
            .map(|dir| dir.join("held"))
            .ok_or_else(|| "scratch database has no directory".to_owned())?;
        let landing = Arc::new(LandingService::new(
            core.journal.clone(),
            LibrarySlot::default(),
            Arc::new(LandingSettings::default),
            held_dir,
        ));
        let worker = Arc::new(
            DownloadWorker::fixed(
                core.journal.clone(),
                Vec::new(),
                WorkerConfig {
                    staging_root: staging_root.clone(),
                    ..WorkerConfig::default()
                },
            )
            .with_landing(landing.clone())
            .with_settled(settled_hook(core.flows.clone())),
        );
        let probe_cache = Arc::new(ProbeCache::new(seed_from_config(
            &SlskdConnection::default(),
            &DownloadClients::default(),
            &[],
            &ProwlarrConnection::default(),
            &LidarrImportConnection::default(),
            &FreeMusic::default(),
        )));
        let probes = Arc::new(LiveProbes::new(probe_cache.clone()));
        let prune = Arc::new(PruneDeps {
            settings: Arc::new(PruneSettings::default),
            ledger: core.requests.store.clone(),
            watches: core.requests.wanted.clone(),
        });
        Ok(Self {
            db,
            requests: core.requests,
            imports,
            users,
            dispatch: core.dispatch,
            journal: core.journal,
            flows: core.flows,
            worker,
            clients,
            probes,
            probe_cache,
            staging_root,
            prune,
            events,
            landing,
        })
    }

    /// Refresh the sweep ownership directory from auth (oldest admin
    /// first). Boot calls this once; the sweep skips cleanly when no
    /// admin exists.
    pub async fn refresh_admins(&self) {
        let rows = match self.users.users.list(10_000, 0).await {
            Ok((rows, _)) => rows,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "admin directory refresh failed; upgrade sweep idles"
                );
                return;
            }
        };
        let mut admins: Vec<_> = rows
            .iter()
            .filter(|row| row.role == AuthRole::Admin)
            .collect();
        admins.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        self.flows
            .admins
            .set(admins.iter().map(|row| row.id.clone()).collect());
    }

    /// Relative-path routers for nesting under `/api/v3` inside the
    /// session gate. The requests and task legs share one
    /// principal-translation layer (both extract the requests
    /// `Principal`) so their handlers keep working unchanged.
    pub fn gated_router(&self) -> Router {
        let legs = super::requests::requests_core_routes(self.requests.clone())
            .merge(super::downloads::downloads_core_routes(self.worker.clone()));
        let requests = legs.layer(axum::middleware::from_fn_with_state(
            self.users.clone(),
            translate_principal,
        ));
        Router::new()
            .merge(requests)
            .merge(imports_gated_router(self.imports.clone()))
    }

    /// Spotify OAuth callback for mounting under `/api/v3` outside the
    /// session gate (state-token identified).
    pub fn callback_router(&self) -> Router {
        imports_callback_router(self.imports.clone())
    }

    /// The Spotify OAuth callback at its v2 path, at full path: mount it at
    /// the root, outside the session gate, like the OIDC legacy callback.
    pub fn legacy_callback_router(&self) -> Router {
        imports_legacy_callback_router(self.imports.clone())
    }

    /// Run startup recovery before serving traffic: durable-op
    /// registration and recovery, the download journal classification,
    /// and the request ledger (interrupted cancels and dispatches).
    /// Re-running after a clean shutdown is a no-op.
    pub async fn run_recovery(
        &self,
        wakeups: &DurableWorkWakeups,
        lane: &WriteLane,
    ) -> Result<super::worker::RecoveryReport, String> {
        register_durable_ops(wakeups, lane).await?;
        let now = super::db::now_epoch();
        let ops = self.flows.ops.recover(super::db::to_i64(now)).await?;
        if ops.interrupted > 0 || ops.resumed > 0 {
            tracing::info!(
                interrupted = ops.interrupted,
                resumed = ops.resumed,
                "flow operations recovered"
            );
        }
        let report = run_startup_recovery(&self.journal, &self.staging_root).await?;
        let requests = super::requests::service::RequestsService::new(&self.requests)
            .recover()
            .await
            .map_err(|error| format!("request recovery failed: {error:?}"))?;
        if requests.cancelled > 0 || requests.relinked > 0 || requests.redispatched > 0 {
            tracing::info!(
                cancelled = requests.cancelled,
                relinked = requests.relinked,
                redispatched = requests.redispatched,
                "requests recovered"
            );
        }
        Ok(report)
    }

    /// Spawn the flows loops, the download worker, and the probe refresh
    /// loop. Returns `(scope, task)` pairs the caller awaits after serve.
    pub async fn spawn_loops(
        &self,
        wakeups: DurableWorkWakeups,
        lane: WriteLane,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<Vec<(&'static str, tokio::task::JoinHandle<()>)>, String> {
        let system_clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let mut out = Vec::new();
        let (handle, _) = spawn_wanted_loop(
            TokioSleeper::new(shutdown.clone()),
            ThreadJitter,
            wakeups.clone(),
            lane.clone(),
            system_clock.clone(),
            self.flows.wanted_deps.clone(),
        )
        .await?;
        out.push((handle.name, handle.task));
        let (handle, _) = spawn_follow_loop(
            TokioSleeper::new(shutdown.clone()),
            ThreadJitter,
            wakeups.clone(),
            lane.clone(),
            system_clock.clone(),
            self.flows.follow_deps.clone(),
        )
        .await?;
        out.push((handle.name, handle.task));
        let (handle, _) = spawn_sweep_loop(
            TokioSleeper::new(shutdown.clone()),
            wakeups.clone(),
            lane.clone(),
            system_clock.clone(),
            self.flows.sweep_deps.clone(),
        )
        .await?;
        out.push((handle.name, handle.task));
        let (handle, _) = spawn_sync_loop(
            TokioSleeper::new(shutdown.clone()),
            wakeups.clone(),
            lane.clone(),
            system_clock.clone(),
            self.flows.sync_deps.clone(),
        )
        .await?;
        out.push((handle.name, handle.task));
        let (handle, _) = spawn_prune_loop(
            TokioSleeper::new(shutdown.clone()),
            wakeups.clone(),
            lane.clone(),
            system_clock,
            self.prune.clone(),
        )
        .await?;
        out.push((handle.name, handle.task));
        let worker = super::worker::spawn_download_worker(
            self.worker.clone(),
            wakeups.clone(),
            lane.clone(),
            shutdown.clone(),
        )
        .await?;
        out.push((super::worker::DOWNLOAD_WORKER_JOB, worker));
        out.push((
            PROBE_JOB,
            self.spawn_probe_loop(wakeups, lane, shutdown).await?,
        ));
        Ok(out)
    }

    /// Probe refresh loop: one pass now, then every five minutes until
    /// shutdown, each pass over the clients the current settings build.
    /// Registered ephemeral for liveness.
    async fn spawn_probe_loop(
        &self,
        wakeups: DurableWorkWakeups,
        lane: WriteLane,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<tokio::task::JoinHandle<()>, String> {
        register_ephemeral_loop(&wakeups, &lane, PROBE_JOB).await?;
        let cache = self.probe_cache.clone();
        let clients = self.clients.clone();
        Ok(tokio::spawn(async move {
            // A pre-signaled shutdown skips the first pass entirely.
            if !*shutdown.borrow() {
                loop {
                    let inputs = clients.current().probe_inputs.clone();
                    refresh_probes(&cache, &inputs).await;
                    if let Err(error) = wakeups.heartbeat(&lane, PROBE_JOB).await {
                        tracing::warn!(%error, "probe refresh heartbeat failed");
                    }
                    tokio::select! {
                        () = tokio::time::sleep(PROBE_INTERVAL) => {}
                        _ = shutdown.changed() => break,
                    }
                    if *shutdown.borrow() {
                        break;
                    }
                }
            }
            if let Err(error) = wakeups
                .set_job_state(&lane, PROBE_JOB, JobState::Stopped)
                .await
            {
                tracing::warn!(%error, "probe refresh stop state write failed");
            }
        }))
    }
}

/// Resolve the requests principal from the stashed session, mirroring the
/// reads collections translation: the role rereads the user row every
/// request, and a session whose account is gone reads as stale (401).
async fn translate_principal(
    axum::extract::State(users): axum::extract::State<UsersDeps>,
    mut request: Request,
    next: Next,
) -> Response {
    use super::requests::{auth::Principal, error::RequestsError, http::HttpError};
    use axum::response::IntoResponse;

    let missing = || {
        HttpError(RequestsError::Unauthorized {
            message: "Authentication required".to_owned(),
        })
    };
    let Some(session) = request.extensions().get::<CurrentSession>().cloned() else {
        return missing().into_response();
    };
    let user = match users.users.get_by_id(&session.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return missing().into_response(),
        Err(StoreError::Conflict) => {
            return HttpError(RequestsError::Conflict {
                message: "Conflicting state".to_owned(),
            })
            .into_response();
        }
        Err(StoreError::Internal(cause)) => {
            return HttpError(RequestsError::internal(&format_args!(
                "user lookup failed: {cause}"
            )))
            .into_response();
        }
    };
    request.extensions_mut().insert(Principal {
        user_id: user.id,
        username: user.username_display.or(user.username),
        role: match user.role {
            AuthRole::User => super::requests::auth::Role::User,
            AuthRole::Trusted => super::requests::auth::Role::Trusted,
            AuthRole::Admin => super::requests::auth::Role::Admin,
        },
    });
    next.run(request).await
}

/// Flows bundle over the shared durable stores and live settings.
#[allow(clippy::too_many_arguments)]
fn flows_bundle(
    db: &AcquireDb,
    requests: &RequestsState,
    search: Arc<dyn CandidateSearch>,
    dispatch: Arc<UnifiedDispatch>,
    wanted_settings: Arc<dyn Fn() -> WantedSettings + Send + Sync>,
    upgrade_policy: Arc<dyn Fn() -> UpgradePolicy + Send + Sync>,
    preferences: Arc<dyn Fn() -> UserPreferences + Send + Sync>,
    ticks: Arc<dyn TickSink>,
) -> FlowsBundle {
    let watches = requests.wanted.clone();
    let ledger = requests.store.clone();
    let follows = FlowsFollowStore::new(db.clone());
    let worklist = UpgradeWorklist::new(db.clone());
    let quarantine = QuarantineStore::new(db.clone());
    let library = Arc::new(LibraryPresence::over_catalog(db.pool().clone()));
    let admins = Arc::new(AdminDirectory::new());
    let handoff = Arc::new(MemoryHandoff::new());
    let ops = OpStore::new(db.clone());
    let wanted_deps = Arc::new(WantedDeps {
        settings: wanted_settings,
        watches: watches.clone(),
        ledger: ledger.clone(),
        search,
        downloads: dispatch.clone(),
        library: library.clone(),
        ticks: ticks.clone(),
    });
    let poll = SlotPoll::new();
    let release_poll = poll.slot().clone();
    let follow_deps = Arc::new(FollowDeps {
        follows: follows.clone(),
        poll: Arc::new(poll),
        downloads: dispatch.clone(),
        library: library.clone(),
        ticks: ticks.clone(),
        preferences,
        today: Arc::new(|| {
            // Days since epoch to a UTC calendar date.
            let days = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|span| span.as_secs() / 86_400)
                .unwrap_or(0);
            utc_ymd(days)
        }),
    });
    let sweep_deps = Arc::new(SweepDeps {
        policy: upgrade_policy,
        worklist: worklist.clone(),
        admins: admins.clone(),
        downloads: dispatch.clone(),
        ticks: ticks.clone(),
    });
    let sync_deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        downloads: dispatch,
        library: library.clone(),
        ticks: ticks.clone(),
    });
    FlowsBundle {
        watches,
        ledger,
        follows,
        worklist,
        quarantine,
        library,
        admins,
        ticks,
        handoff,
        ops,
        wanted_deps,
        follow_deps,
        release_poll,
        sweep_deps,
        sync_deps,
    }
}

/// Tell the importing user's open tabs the playlist has its tracks, so the
/// list and detail views refresh (v2 `spotify.py` import completion).
fn announce_playlist_imported(events: &EventSink, job: &QueuedSpotifyImport) {
    events.notify(
        &job.user_id,
        UserNotice::PlaylistImported(PlaylistImported {
            playlist_id: job.playlist_id.clone(),
            event_id: new_event_id(),
        }),
    );
}

/// Days since epoch to `YYYY-MM-DD` (Howard Hinnant's civil algorithm).
fn utc_ymd(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{day:02}")
}

/// slskd repository, when the section configures one. The mount is
/// `SLSKD_DOWNLOADS_PATH` plus the confined subpath from settings.
fn slskd_repository(
    http: &reqwest::Client,
    section: &SlskdConnection,
    downloads: &Path,
) -> Option<Arc<SlskdRepository<ReqwestSlskdHttp>>> {
    if section.url.is_empty() || section.api_key.expose().is_empty() {
        return None;
    }
    let transport = ReqwestSlskdHttp::new(http.clone(), &section.url, section.api_key.expose());
    let client = SlskdClient::new(transport);
    let mount = super::imports::mount::effective_path(downloads, &section.downloads_subpath);
    let mut repo = SlskdRepository::new(
        client,
        &section.url,
        section.api_key.expose(),
        mount,
        SlskdPolicy::default(),
    );
    if !section.slskd_incomplete_mount.trim().is_empty() {
        repo = repo.with_incomplete_mount(PathBuf::from(section.slskd_incomplete_mount.trim()));
    }
    Some(Arc::new(repo))
}

/// SABnzbd queue, when the section configures one.
fn sabnzbd_queue(
    http: &reqwest::Client,
    section: &DownloadClients,
    policy: &UsenetPolicy,
) -> Option<Arc<SabnzbdQueue>> {
    let sab = &section.sabnzbd;
    if sab.url.is_empty() || sab.api_key.expose().is_empty() {
        return None;
    }
    let client = SabnzbdClient::new(
        http.clone(),
        &sab.url,
        sab.api_key.expose(),
        3,
        Duration::from_secs(1),
    );
    Some(Arc::new(SabnzbdQueue::new(
        client,
        &sab.url,
        sab.api_key.expose(),
        PathBuf::from(sab.downloads_mount.clone()),
        policy.clone(),
    )))
}

/// Native indexer fan-out over the configured entries.
fn newznab_indexer(http: &reqwest::Client, entries: &[ConfigIndexer]) -> NewznabIndexer {
    let entries = entries
        .iter()
        .map(|entry| NewznabIndexerEntry {
            client: NewznabClient::new(
                http.clone(),
                &entry.url,
                entry.api_key.expose(),
                &entry.id,
                &entry.name,
            ),
            id: entry.id.clone(),
            name: entry.name.clone(),
            categories: entry.categories.iter().map(|cat| *cat as i32).collect(),
            enabled: entry.enabled,
            priority: entry.priority.max(0) as u32,
            limit: 100,
        })
        .collect();
    NewznabIndexer::new(
        entries,
        Duration::from_secs(300),
        Duration::from_secs(60),
        Duration::from_secs(300),
        Duration::from_secs(30),
    )
}

/// Prowlarr fan-out over the single connection.
fn prowlarr_indexer(http: &reqwest::Client, section: &ProwlarrConnection) -> ProwlarrIndexer {
    let client = if section.url.is_empty() || section.api_key.expose().is_empty() {
        None
    } else {
        Some(ProwlarrClient::new(
            http.clone(),
            &section.url,
            section.api_key.expose(),
            "prowlarr",
        ))
    };
    ProwlarrIndexer::new(
        client,
        vec![3000, 3010, 3040],
        section.enabled,
        Duration::from_secs(300),
        Duration::from_secs(300),
        Duration::from_secs(30),
    )
}

/// Usenet policy bound from the download-policy section.
fn usenet_policy_from(policy: &DownloadPolicy) -> UsenetPolicy {
    let mut out = UsenetPolicy::v2_defaults();
    if let Some(tier) = QualityTier::parse(&policy.quality_min) {
        out.quality_min = tier;
    }
    if let Some(tier) = QualityTier::parse(&policy.quality_max) {
        out.quality_max = tier;
    }
    if let Some(tier) = QualityTier::parse(&policy.quality_cutoff) {
        out.quality_cutoff = tier;
    }
    out.flac_mp3_only = policy.flac_mp3_only;
    out.retention_days = clamp_u32(policy.usenet_retention_days);
    out.min_release_age =
        Duration::from_secs(clamp_u64(policy.usenet_min_release_age_minutes) * 60);
    out.max_size_mb = clamp_u64(policy.max_size_mb);
    out.stall_timeout =
        Duration::from_secs(clamp_u64(policy.download_stall_timeout_minutes).max(1) * 60);
    out.queued_timeout =
        Duration::from_secs(clamp_u64(policy.download_queued_timeout_minutes).max(1) * 60);
    out
}

/// Worker tuning bound from the download-policy section. Only the
/// SABnzbd complete dir is walked: slskd folders cannot prove client
/// inactivity from the folder name, so they always keep.
fn worker_config(
    policy: &DownloadPolicy,
    source_priority: &SourcePriority,
    staging_root: &Path,
    sab_mount: Option<PathBuf>,
) -> WorkerConfig {
    let mut source_order: Vec<String> = source_priority
        .0
        .iter()
        .filter(|source| {
            *source == "soulseek" || *source == "usenet" || source.starts_with("plugin:")
        })
        .cloned()
        .collect();
    if source_order.is_empty() {
        source_order = vec!["soulseek".to_owned(), "usenet".to_owned()];
    }
    let orphan_roots = sab_mount
        .clone()
        .map(|mount| vec![("usenet".to_owned(), mount)])
        .unwrap_or_default();
    let mut protected = vec![staging_root.to_owned()];
    if let Some(mount) = &sab_mount {
        protected.push(mount.clone());
    }
    let recycle = match RecycleBin::resolve(&policy.recycle_bin_path, &[]) {
        None => None,
        Some(root) => RecycleBin::guarded(root, policy.recycle_retention_days.max(0), &protected),
    };
    WorkerConfig {
        interval: super::worker::WORKER_INTERVAL,
        max_concurrent_downloads: clamp_usize(policy.max_concurrent_downloads).max(1),
        max_failover_attempts: policy.max_failover_attempts.max(0),
        retry: retry_policy_from(policy),
        watchdog: WatchdogConfig {
            poll_interval_seconds: 2.0,
            stall_timeout_seconds: (clamp_u64(policy.download_stall_timeout_minutes).max(1) * 60)
                as f64,
            queued_timeout_seconds: (clamp_u64(policy.download_queued_timeout_minutes).max(1) * 60)
                as f64,
            deadline_seconds: 6.0 * 3600.0,
            materialize_seconds: 90.0,
            reap_threshold_seconds: 1800.0,
        },
        source_order,
        staging_root: staging_root.to_owned(),
        recycle,
        orphan_roots,
        worker_id: "download-worker".to_owned(),
    }
}
