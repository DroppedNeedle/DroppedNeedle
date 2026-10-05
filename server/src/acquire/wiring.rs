//! Acquire bundle: one setup the app mounts, following the stage-6 shape.
//!
//! [`AcquireSetup`] owns the requests state, the imports deps, the flows
//! stores and loop deps, the unified dispatch, the source adapters, the
//! health probes, and the download worker. `build` binds everything to
//! the stage-2 config store (credentials stay encrypted at rest);
//! `for_tests` binds the same shape over memory stores and a scratch
//! journal. The routers nest under `/api/v3` inside the deny-by-default
//! session gate; only the Spotify OAuth callback mounts outside it (it is
//! state-token identified, like v2's ungated route).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{Router, extract::Request, middleware::Next, response::Response};

use super::dispatch::{Journal, UnifiedDispatch};
use super::downloads::orphans::RecycleBin;
use super::downloads::watchdog::{RetryPolicy, WatchdogConfig};
use super::flows::loops::{
    FollowDeps, SweepDeps, SyncDeps, ThreadJitter, TokioSleeper, WantedDeps,
    register_ephemeral_loop, spawn_follow_loop, spawn_sweep_loop, spawn_sync_loop,
    spawn_wanted_loop,
};
use super::flows::operations::{DropImportDeps, OpStore, register_durable_ops};
use super::flows::seams::{
    Clock, DropVerify, LibraryOrganise, MemoryHandoff, MemoryTicks, VerifyVerdict,
};
use super::flows::stores::{
    AdminDirectory, FollowStore as FlowsFollowStore, LibraryPresence, QuarantineStore,
    RequestLedger, UpgradePolicy, UpgradeWorklist, WantedStore as FlowsWantedStore,
};
use super::imports::handlers::{ImportsDeps, imports_callback_router, imports_gated_router};
use super::imports::jobs::{JobRegistry, QueuedSpotifyImport, TaskExecutor};
use super::imports::lidarr::LidarrClient;
use super::imports::spotify::{
    FixedMbidResolver, MemoryPlaylistIndex, MemorySpotifyConnections, MemorySpotifyStates,
    MemoryTrackSink, SpotifyClient, SpotifyImportService,
};
use super::mirror::mirror_requests_into_flows;
use super::probes::{LiveProbes, ProbeCache, ProbeInputs, refresh_probes, seed_from_config};
use super::requests::quota::{QuotaLedger, QuotaPolicy};
use super::requests::state::RequestsState;
use super::search::{EmptyPoll, FanoutSearch};
use super::settings::{
    ApprovalSeedBridge, CollectionsFollowBridge, ConfigLidarrSettings, ConfigSpotifySettings,
    FlowsWatchBridge, FollowDecisionBridge, RequestsApprovalBridge, RequestsPendingSource,
};
use super::slskd::{DownloadPolicy as SlskdPolicy, ReqwestSlskdHttp, SlskdClient, SlskdRepository};
use super::sources::{SabnzbdSource, SlskdSource};
use super::usenet::newznab::{NewznabClient, NewznabIndexer, NewznabIndexerEntry};
use super::usenet::policy::{QualityTier, UsenetPolicy};
use super::usenet::prowlarr::{ProwlarrClient, ProwlarrIndexer};
use super::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
use super::worker::{DownloadWorker, Source, WorkerConfig, run_startup_recovery};
use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::UsersDeps;
use crate::auth::users::roles::Role as AuthRole;
use crate::auth::users::stores::StoreError;
use crate::config::AppConfig;
use crate::db::{DurableWorkWakeups, JobState, WriteLane};
use crate::ids::IdGenerator;
use crate::reads::collections::state::CollectionsState;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::{
    DownloadClients, LidarrImportConnection, NewznabIndexer as ConfigIndexer, ProwlarrConnection,
    SlskdConnection,
};
use crate::runtime_config::sections::{
    DownloadPolicy, FreeMusic, SourcePriority, UsenetBackendSetting, WantedWatcher,
};

/// Spotify API bases (v2 `spotify_client.py`).
const SPOTIFY_API_BASE: &str = "https://api.spotify.com/v1";
const SPOTIFY_ACCOUNTS_BASE: &str = "https://accounts.spotify.com";

/// Health-probe refresh cadence.
const PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Registry name for the probe refresh loop.
const PROBE_JOB: &str = "acquire-probe-refresh";

/// Follow-poll accepted primary types (v2 user-preference defaults, in
/// MusicBrainz capitalization).
const FOLLOW_INCLUDE_TYPES: &[&str] = &["Album", "Single", "EP"];

/// Current unix time in epoch seconds.
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

/// Clamp a config integer to a non-negative `u32` (0 = unlimited).
fn clamp_u32(value: i64) -> u32 {
    value.max(0) as u32
}

/// Clamp a config integer to a non-negative `u64`.
fn clamp_u64(value: i64) -> u64 {
    value.max(0) as u64
}

/// Clamp a config integer to a non-negative `usize`.
fn clamp_usize(value: i64) -> usize {
    value.max(0) as usize
}

/// Everything `create_app` needs to mount the acquire slices, built once.
#[derive(Clone)]
pub struct AcquireSetup {
    /// Request intake state (memory ledgers, unified dispatch).
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
    /// Live health probes.
    pub probes: Arc<LiveProbes>,
    /// Probe cache plus refresh inputs, for the probe loop.
    pub probe_cache: Arc<ProbeCache>,
    /// Probe refresh inputs.
    pub probe_inputs: Arc<ProbeInputs>,
    /// Per-task staging root (manifests, drop jobs, quarantine).
    pub staging_root: PathBuf,
}

/// Flows stores plus the loop deps built over them.
#[derive(Clone)]
pub struct FlowsBundle {
    /// Wanted-watch registry (the loop's own).
    pub watches: Arc<FlowsWantedStore>,
    /// Flows request ledger (mirrored from requests).
    pub ledger: Arc<RequestLedger>,
    /// Follow-poll cursors.
    pub follows: Arc<FlowsFollowStore>,
    /// Upgrade worklist (empty until the stage-8 scan fills it).
    pub worklist: Arc<UpgradeWorklist>,
    /// Quarantine registry.
    pub quarantine: Arc<QuarantineStore>,
    /// Library presence (empty until the stage-8 port fills it).
    pub library: Arc<LibraryPresence>,
    /// Sweep ownership directory (refreshed from auth at boot).
    pub admins: Arc<AdminDirectory>,
    /// Durable ticks.
    pub ticks: Arc<MemoryTicks>,
    /// Free-music landing handoff (memory records; file staging waits on
    /// the path-carrying handoff).
    pub handoff: Arc<MemoryHandoff>,
    /// Durable operation records.
    pub ops: Arc<OpStore>,
    /// Wanted-watcher deps.
    pub wanted_deps: Arc<WantedDeps>,
    /// Follow-poll deps.
    pub follow_deps: Arc<FollowDeps>,
    /// Upgrade-sweep deps.
    pub sweep_deps: Arc<SweepDeps>,
    /// Status-sync deps.
    pub sync_deps: Arc<SyncDeps>,
}

impl FlowsBundle {
    /// Production drop-import deps over the shared stores: extension
    /// verify plus resolve-into-staging organise (stage 8 owns real
    /// identification and library placement).
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

/// System clock for production flow deps.
#[derive(Debug, Clone, Copy, Default)]
struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        now_epoch() as i64
    }
}

/// Drop-file verify by audio extension. Unknown types fail open as local
/// faults (never quarantined): stage 8 owns real identification.
struct ExtensionVerify;

impl DropVerify for ExtensionVerify {
    fn verify(&self, file_name: &str) -> VerifyVerdict {
        let ext = file_name
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_lowercase();
        match ext.as_str() {
            "flac" | "mp3" | "ogg" | "oga" | "opus" | "m4a" | "mp4" | "aac" | "wav" | "aiff"
            | "aif" | "wma" | "alac" => VerifyVerdict::Ok,
            _ => VerifyVerdict::LocalFault(format!("unsupported file type: {ext}")),
        }
    }
}

/// Library organise into the staging `resolved` dir. Stage 8 replaces
/// this with real library placement; until then drops resolve out of
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
        std::fs::rename(&source, &dest).map_err(|error| format!("cannot resolve drop: {error}"))?;
        Ok(dest.to_string_lossy().into_owned())
    }
}

impl AcquireSetup {
    /// Build the production bundle. Reads credentials and tuning from the
    /// config store (decrypted in memory only) and connects the
    /// collections/requests bridges both ways.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        db_path: &Path,
        config: &AppConfig,
        users: UsersDeps,
        http: reqwest::Client,
        ids: Arc<dyn IdGenerator>,
        config_store: Arc<ConfigStore>,
        collections: &mut CollectionsState,
    ) -> Result<Self, String> {
        let staging_root = config.root_app_dir.join("staging");
        let journal = Arc::new(Journal::open(db_path)?);
        let dispatch = Arc::new(UnifiedDispatch::new(
            journal.clone(),
            ids.clone(),
            staging_root.clone(),
        ));

        let slskd_section: SlskdConnection = config_store.get_raw().unwrap_or_default();
        let sab_section: DownloadClients = config_store.get_raw().unwrap_or_default();
        let indexers: Vec<ConfigIndexer> = config_store.get_indexers_raw().unwrap_or_default();
        let prowlarr_section: ProwlarrConnection = config_store.get_raw().unwrap_or_default();
        let lidarr_section: LidarrImportConnection = config_store.get_raw().unwrap_or_default();
        let policy: DownloadPolicy = config_store.get().unwrap_or_default();
        let free_section: FreeMusic = config_store.get().unwrap_or_default();
        let backend: UsenetBackendSetting = config_store.get().unwrap_or_default();
        let source_priority: SourcePriority = config_store.get().unwrap_or_default();

        let usenet_policy = usenet_policy_from(&policy);
        let slskd_repo = slskd_repository(&http, &slskd_section, &config.root_app_dir);
        let sab_queue = sabnzbd_queue(&http, &sab_section, &usenet_policy);
        let newznab = Arc::new(newznab_indexer(&http, &indexers));
        let prowlarr = Arc::new(prowlarr_indexer(&http, &prowlarr_section));
        let search = Arc::new(FanoutSearch::new(
            slskd_repo.clone(),
            newznab.clone(),
            prowlarr.clone(),
            backend,
            usenet_policy.indexer_timeout,
        ));

        // Requests state: real quotas, bridged follows and watches.
        let mut requests = RequestsState::new(dispatch.clone());
        requests.quota = Arc::new(QuotaLedger::new(QuotaPolicy {
            request_count: clamp_u32(policy.default_request_quota_count),
            request_days: clamp_u32(policy.default_request_quota_days).max(1),
            storage_gb_per_user: clamp_u64(policy.default_storage_quota_gb),
            max_library_gb: clamp_u64(policy.max_library_size_gb),
        }));
        requests.follow_sink = Some(
            Arc::new(FollowDecisionBridge::new(collections.follows.clone()))
                as Arc<dyn super::requests::bridges::FollowDecisionSink>,
        );
        collections.acquire_approvals = Some(Arc::new(RequestsPendingSource::new(
            requests.follows.clone(),
        ))
            as Arc<dyn crate::reads::collections::state::PendingApprovalsSource>);
        collections.approval_seeds =
            Some(Arc::new(ApprovalSeedBridge::new(requests.follows.clone()))
                as Arc<
                    dyn crate::reads::collections::state::ApprovalSeedSink,
                >);

        // Flows bundle over fresh memory stores.
        let flows = Arc::new(flows_bundle(
            &config_store,
            search.clone(),
            dispatch.clone(),
        ));
        requests.watch_view = Some(Arc::new(FlowsWatchBridge::new(flows.watches.clone()))
            as Arc<dyn super::requests::bridges::WatchView>);

        // Imports deps.
        let lidarr_settings = Arc::new(ConfigLidarrSettings::new(config_store.clone()));
        let spotify_settings = Arc::new(ConfigSpotifySettings::new(config_store.clone()));
        let spotify_states = Arc::new(MemorySpotifyStates::new());
        let spotify_links = Arc::new(MemorySpotifyConnections::new());
        let playlists = Arc::new(MemoryPlaylistIndex::new());
        let tracks = Arc::new(MemoryTrackSink::new());
        let resolver = Arc::new(FixedMbidResolver::new());
        let spotify_client =
            SpotifyClient::new(http.clone(), SPOTIFY_API_BASE, SPOTIFY_ACCOUNTS_BASE);
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
        let executor = Arc::new(TaskExecutor::new(
            jobs.clone(),
            move |job: QueuedSpotifyImport| {
                let service = runner.clone();
                tokio::spawn(async move {
                    let result = service
                        .populate_playlist(&job.user_id, &job.spotify_playlist_id, &job.playlist_id)
                        .await
                        .map_err(|error| match error {
                            super::imports::spotify::SpotifyError::NotLinked => {
                                "Spotify account not linked".to_owned()
                            }
                            super::imports::spotify::SpotifyError::Unavailable(_) => {
                                "Failed to fetch playlist from Spotify".to_owned()
                            }
                        });
                    (job.playlist_id.clone(), result)
                })
            },
        ));
        let probe_cache = Arc::new(ProbeCache::new(seed_from_config(
            &slskd_section,
            &sab_section,
            &indexers,
            &prowlarr_section,
            &lidarr_section,
            &free_section,
        )));
        let probes = Arc::new(LiveProbes::new(probe_cache.clone()));
        let probe_inputs = Arc::new(ProbeInputs {
            slskd: slskd_repo.clone(),
            slskd_enabled: slskd_section.enabled,
            sabnzbd: sab_queue.clone(),
            sabnzbd_section: sab_section,
            newznab: newznab.clone(),
            newznab_entries: indexers,
            prowlarr: prowlarr.clone(),
            prowlarr_section,
            lidarr: LidarrClient::new(http.clone()),
            lidarr_section,
            free: free_section,
        });
        let imports = ImportsDeps {
            http: http.clone(),
            lidarr: LidarrClient::new(http.clone()),
            lidarr_settings,
            follows: Arc::new(CollectionsFollowBridge::new(collections.follows.clone())),
            approvals: Arc::new(RequestsApprovalBridge::new(requests.follows.clone())),
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

        // Worker over the configured sources.
        let mut sources = Vec::new();
        if let Some(repo) = slskd_repo {
            sources.push(Source::Slskd(SlskdSource::new(repo, journal.clone())));
        }
        if let Some(queue) = sab_queue.clone() {
            sources.push(Source::Sab(SabnzbdSource::new(
                queue,
                newznab,
                prowlarr,
                backend,
                usenet_policy,
                journal.clone(),
                Some(probe_inputs.sabnzbd_section.sabnzbd.category.clone()),
                Duration::from_secs(30),
            )));
        }
        let sab_mount = sab_queue
            .as_ref()
            .map(|_| PathBuf::from(probe_inputs.sabnzbd_section.sabnzbd.downloads_mount.clone()));
        let worker = Arc::new(DownloadWorker::new(
            journal.clone(),
            sources,
            worker_config(&policy, &source_priority, &staging_root, sab_mount),
        ));

        Ok(Self {
            requests,
            imports,
            users,
            dispatch,
            journal,
            flows,
            worker,
            probes,
            probe_cache,
            probe_inputs,
            staging_root,
        })
    }

    /// Test bundle over memory stores and a scratch journal. Bridges stay
    /// connected (collections reads share the requests approval store) so
    /// hooked-state apps behave like production.
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

        let staging_root = std::env::temp_dir().join(format!(
            "dn-acquire-test-{}-{}",
            std::process::id(),
            now_epoch()
        ));
        let journal = Arc::new(Journal::memory()?);
        let dispatch = Arc::new(UnifiedDispatch::new(
            journal.clone(),
            ids.clone(),
            staging_root.clone(),
        ));
        let mut requests = RequestsState::new(dispatch.clone());
        requests.follow_sink = Some(
            Arc::new(FollowDecisionBridge::new(collections.follows.clone()))
                as Arc<dyn super::requests::bridges::FollowDecisionSink>,
        );
        collections.acquire_approvals = Some(Arc::new(RequestsPendingSource::new(
            requests.follows.clone(),
        ))
            as Arc<dyn crate::reads::collections::state::PendingApprovalsSource>);
        collections.approval_seeds =
            Some(Arc::new(ApprovalSeedBridge::new(requests.follows.clone()))
                as Arc<
                    dyn crate::reads::collections::state::ApprovalSeedSink,
                >);
        let search = Arc::new(FanoutSearch::new(
            None,
            Arc::new(NewznabIndexer::new(
                Vec::new(),
                Duration::from_secs(300),
                Duration::from_secs(60),
                Duration::from_secs(300),
                Duration::from_secs(5),
            )),
            Arc::new(ProwlarrIndexer::new(
                None,
                Vec::new(),
                false,
                Duration::from_secs(300),
                Duration::from_secs(300),
                Duration::from_secs(5),
            )),
            UsenetBackendSetting::default(),
            Duration::from_secs(5),
        ));
        let flows = Arc::new(flows_bundle_memory(search, dispatch.clone()));
        requests.watch_view = Some(Arc::new(FlowsWatchBridge::new(flows.watches.clone()))
            as Arc<dyn super::requests::bridges::WatchView>);
        let http = reqwest::Client::new();
        let spotify_client =
            SpotifyClient::new(http.clone(), SPOTIFY_API_BASE, SPOTIFY_ACCOUNTS_BASE);
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
        let executor = Arc::new(TaskExecutor::new(
            jobs.clone(),
            move |job: QueuedSpotifyImport| {
                let service = service.clone();
                tokio::spawn(async move {
                    let result = service
                        .populate_playlist(&job.user_id, &job.spotify_playlist_id, &job.playlist_id)
                        .await
                        .map_err(|_| "spotify import failed".to_owned());
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
        let worker = Arc::new(DownloadWorker::new(
            journal.clone(),
            Vec::new(),
            WorkerConfig {
                staging_root: staging_root.clone(),
                ..WorkerConfig::default()
            },
        ));
        let probe_cache = Arc::new(ProbeCache::new(seed_from_config(
            &SlskdConnection::default(),
            &DownloadClients::default(),
            &[],
            &ProwlarrConnection::default(),
            &LidarrImportConnection::default(),
            &FreeMusic::default(),
        )));
        let probes = Arc::new(LiveProbes::new(probe_cache.clone()));
        // Probe inputs over empty clients; the test loop never runs them.
        let probe_inputs = Arc::new(ProbeInputs {
            slskd: None,
            slskd_enabled: false,
            sabnzbd: None,
            sabnzbd_section: DownloadClients::default(),
            newznab: Arc::new(NewznabIndexer::new(
                Vec::new(),
                Duration::from_secs(300),
                Duration::from_secs(60),
                Duration::from_secs(300),
                Duration::from_secs(5),
            )),
            newznab_entries: Vec::new(),
            prowlarr: Arc::new(ProwlarrIndexer::new(
                None,
                Vec::new(),
                false,
                Duration::from_secs(300),
                Duration::from_secs(300),
                Duration::from_secs(5),
            )),
            prowlarr_section: ProwlarrConnection::default(),
            lidarr: LidarrClient::new(reqwest::Client::new()),
            lidarr_section: LidarrImportConnection::default(),
            free: FreeMusic::default(),
        });
        Ok(Self {
            requests,
            imports,
            users,
            dispatch,
            journal,
            flows,
            worker,
            probes,
            probe_cache,
            probe_inputs,
            staging_root,
        })
    }

    /// Refresh the sweep ownership directory from auth (oldest admin
    /// first). Boot calls this once; the sweep skips honestly when no
    /// admin exists.
    pub async fn refresh_admins(&self) {
        let jonka = self.users.users.list(10_000, 0).await;
        let Ok((rows, _)) = jonka else {
            return;
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
        let legs = super::requests::requests_core_routes(self.requests.clone()).merge(
            super::downloads::downloads_core_routes(self.journal.clone()),
        );
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

    /// Run startup recovery: durable-op registration plus the journal
    /// classification. Must run before serving traffic.
    pub async fn run_recovery(
        &self,
        wakeups: &DurableWorkWakeups,
        lane: &WriteLane,
    ) -> Result<super::worker::RecoveryReport, String> {
        register_durable_ops(wakeups, lane).await?;
        // Startup recovery is synchronous sqlite + staging reads; keep it
        // off the async runtime.
        let journal = self.journal.clone();
        let staging_root = self.staging_root.clone();
        tokio::task::spawn_blocking(move || run_startup_recovery(&journal, &staging_root))
            .await
            .map_err(|error| format!("acquire recovery join failed: {error}"))?
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
        // Mirror hook ahead of the wanted and sync passes.
        let mirror = {
            let store = self.requests.store.clone();
            let wanted = self.requests.wanted.clone();
            let ledger = self.flows.ledger.clone();
            let watches = self.flows.watches.clone();
            Arc::new(move || {
                mirror_requests_into_flows(
                    &store,
                    &wanted,
                    &ledger,
                    &watches,
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|span| span.as_secs() as i64)
                        .unwrap_or(0),
                );
            })
        };
        let mut out = Vec::new();
        let (handle, _) = spawn_wanted_loop(
            TokioSleeper::new(shutdown.clone()),
            ThreadJitter,
            wakeups.clone(),
            lane.clone(),
            system_clock.clone(),
            self.flows.wanted_deps.clone(),
            Some(mirror.clone()),
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
            None,
        )
        .await?;
        out.push((handle.name, handle.task));
        let (handle, _) = spawn_sweep_loop(
            TokioSleeper::new(shutdown.clone()),
            wakeups.clone(),
            lane.clone(),
            system_clock.clone(),
            self.flows.sweep_deps.clone(),
            None,
        )
        .await?;
        out.push((handle.name, handle.task));
        let (handle, _) = spawn_sync_loop(
            TokioSleeper::new(shutdown.clone()),
            wakeups.clone(),
            lane.clone(),
            system_clock,
            self.flows.sync_deps.clone(),
            Some(mirror),
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
    /// shutdown. Registered ephemeral for liveness.
    async fn spawn_probe_loop(
        &self,
        wakeups: DurableWorkWakeups,
        lane: WriteLane,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<tokio::task::JoinHandle<()>, String> {
        register_ephemeral_loop(&wakeups, &lane, PROBE_JOB).await?;
        let cache = self.probe_cache.clone();
        let inputs = self.probe_inputs.clone();
        Ok(tokio::spawn(async move {
            // A pre-signaled shutdown skips the first pass entirely.
            if *shutdown.borrow() {
                let _ = wakeups
                    .set_job_state(&lane, PROBE_JOB, JobState::Stopped)
                    .await;
                return;
            }
            loop {
                refresh_probes(&cache, &inputs).await;
                let _ = wakeups.heartbeat(&lane, PROBE_JOB).await;
                tokio::select! {
                    () = tokio::time::sleep(PROBE_INTERVAL) => {}
                    _ = shutdown.changed() => break,
                }
                if *shutdown.borrow() {
                    break;
                }
            }
            let _ = wakeups
                .set_job_state(&lane, PROBE_JOB, JobState::Stopped)
                .await;
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
    use super::requests::{auth::Principal, error::RequestsError};
    use axum::response::IntoResponse;

    let missing = || RequestsError::Unauthorized {
        message: "Authentication required".to_owned(),
    };
    let Some(session) = request.extensions().get::<CurrentSession>().cloned() else {
        return missing().into_response();
    };
    let user = match users.users.get_by_id(&session.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return missing().into_response(),
        Err(StoreError::Conflict) => {
            return RequestsError::Conflict {
                message: "Conflicting state".to_owned(),
            }
            .into_response();
        }
        Err(StoreError::Internal(cause)) => {
            return RequestsError::internal(&format_args!("user lookup failed: {cause}"))
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

/// Flows bundle with live config-backed settings closures.
fn flows_bundle(
    config_store: &Arc<ConfigStore>,
    search: Arc<FanoutSearch>,
    dispatch: Arc<UnifiedDispatch>,
) -> FlowsBundle {
    let watches = Arc::new(FlowsWantedStore::new());
    let ledger = Arc::new(RequestLedger::new());
    let follows = Arc::new(FlowsFollowStore::new());
    let worklist = Arc::new(UpgradeWorklist::new());
    let quarantine = Arc::new(QuarantineStore::new());
    let library = Arc::new(LibraryPresence::new());
    let admins = Arc::new(AdminDirectory::new());
    let ticks = Arc::new(MemoryTicks::new());
    let handoff = Arc::new(MemoryHandoff::new());
    let ops = Arc::new(OpStore::new());
    let wanted_store = config_store.clone();
    let wanted_deps = Arc::new(WantedDeps {
        settings: Arc::new(move || {
            let section: WantedWatcher = wanted_store.get().unwrap_or_default();
            super::flows::loops::WantedSettings {
                enabled: section.enabled,
                watch_partial_albums: section.watch_partial_albums,
                max_checks_per_sweep: clamp_usize(section.max_checks_per_sweep).max(1),
                auto_download_on_find: section.auto_download_on_find,
            }
        }),
        watches: watches.clone(),
        ledger: ledger.clone(),
        search: search.clone(),
        downloads: dispatch.clone(),
        library: library.clone(),
        ticks: ticks.clone(),
    });
    let follow_deps = Arc::new(FollowDeps {
        follows: follows.clone(),
        poll: Arc::new(EmptyPoll::new()),
        downloads: dispatch.clone(),
        ticks: ticks.clone(),
        include_types: FOLLOW_INCLUDE_TYPES
            .iter()
            .map(|kind| kind.to_string())
            .collect(),
        today: Arc::new(|| {
            // Days since epoch to a UTC calendar date.
            let days = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|span| span.as_secs() / 86_400)
                .unwrap_or(0);
            utc_ymd(days)
        }),
    });
    let sweep_store = config_store.clone();
    let sweep_deps = Arc::new(SweepDeps {
        policy: Arc::new(move || {
            let section: DownloadPolicy = sweep_store.get().unwrap_or_default();
            UpgradePolicy {
                upgrade_allowed: section.upgrade_allowed,
                scan_enabled: section.background_upgrade_scan_enabled,
                max_per_run: clamp_usize(section.background_upgrade_max_per_run).max(1),
                interval_hours: clamp_u64(section.background_upgrade_scan_interval_hours).max(1),
            }
        }),
        worklist: worklist.clone(),
        admins: admins.clone(),
        downloads: dispatch.clone(),
        ticks: ticks.clone(),
    });
    let sync_deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        downloads: dispatch.clone(),
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
        sweep_deps,
        sync_deps,
    }
}

/// Flows bundle with static defaults (tests).
#[cfg(any(test, feature = "test-support"))]
fn flows_bundle_memory(search: Arc<FanoutSearch>, dispatch: Arc<UnifiedDispatch>) -> FlowsBundle {
    let watches = Arc::new(FlowsWantedStore::new());
    let ledger = Arc::new(RequestLedger::new());
    let follows = Arc::new(FlowsFollowStore::new());
    let ticks = Arc::new(MemoryTicks::new());
    let wanted_deps = Arc::new(WantedDeps {
        settings: Arc::new(super::flows::loops::WantedSettings::default),
        watches: watches.clone(),
        ledger: ledger.clone(),
        search,
        downloads: dispatch.clone(),
        library: Arc::new(LibraryPresence::new()),
        ticks: ticks.clone(),
    });
    let follow_deps = Arc::new(FollowDeps {
        follows: follows.clone(),
        poll: Arc::new(EmptyPoll::new()),
        downloads: dispatch.clone(),
        ticks: ticks.clone(),
        include_types: Vec::new(),
        today: Arc::new(|| "2024-01-01".to_owned()),
    });
    let sweep_deps = Arc::new(SweepDeps {
        policy: Arc::new(UpgradePolicy::default),
        worklist: Arc::new(UpgradeWorklist::new()),
        admins: Arc::new(AdminDirectory::new()),
        downloads: dispatch.clone(),
        ticks: ticks.clone(),
    });
    let sync_deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        downloads: dispatch,
        library: Arc::new(LibraryPresence::new()),
        ticks: ticks.clone(),
    });
    FlowsBundle {
        watches,
        ledger,
        follows,
        worklist: Arc::new(UpgradeWorklist::new()),
        quarantine: Arc::new(QuarantineStore::new()),
        library: Arc::new(LibraryPresence::new()),
        admins: Arc::new(AdminDirectory::new()),
        ticks,
        handoff: Arc::new(MemoryHandoff::new()),
        ops: Arc::new(OpStore::new()),
        wanted_deps,
        follow_deps,
        sweep_deps,
        sync_deps,
    }
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

/// slskd repository, when the section configures one. The mount resolves
/// to `<root>/downloads/slskd` plus the confined subpath; review the
/// deployment mapping for the slskd volume before relying on it.
fn slskd_repository(
    http: &reqwest::Client,
    section: &SlskdConnection,
    root: &Path,
) -> Option<Arc<SlskdRepository<ReqwestSlskdHttp>>> {
    if section.url.is_empty() || section.api_key.expose().is_empty() {
        return None;
    }
    let transport = ReqwestSlskdHttp::new(http.clone(), &section.url, section.api_key.expose());
    let client = SlskdClient::new(transport);
    let mut mount = root.join("downloads").join("slskd");
    for part in section.downloads_subpath.split(['/', '\\']) {
        let part = part.trim();
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        mount = mount.join(part);
    }
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
        .filter(|source| *source == "soulseek" || *source == "usenet")
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
        None => {
            tracing::info!("recycle bin unresolved; prune skipped");
            None
        }
        Some(root) => RecycleBin::guarded(root, policy.recycle_retention_days.max(0), &protected),
    };
    WorkerConfig {
        interval: super::worker::WORKER_INTERVAL,
        max_concurrent_downloads: clamp_usize(policy.max_concurrent_downloads).max(1),
        max_failover_attempts: policy.max_failover_attempts.max(0),
        retry: RetryPolicy {
            enabled: policy.auto_retry_enabled,
            max_attempts: clamp_u32(policy.auto_retry_max_attempts),
            base_interval_minutes: policy.auto_retry_base_interval_minutes.max(0) as f64,
            cap_seconds: 86_400.0,
        },
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
