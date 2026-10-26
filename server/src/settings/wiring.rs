//! Settings bundle: the section service, its save effects,
//! the verify probes, Library Management, and the HTTP surface.
//!
//! [`SettingsSetup`] is the one `AppState` field settings adds. It holds
//! the [`SettingsService`](crate::settings::services::SettingsService)
//! with its post-save fan-out wired (provider-cache invalidation over
//! the shared cache plus the jobs-owned events kick) and the connection
//! probes behind the verify endpoints, the MusicBrainz lifecycle and the
//! Library Management service over the same store.
//!
//! [`routes`] declares `/api/v3/settings/*`, the plain sections from the
//! section [`registry`](super::registry) and the rest by hand, every
//! handler taking `Admin` except the Connect Apps read, plus the per-user
//! section prefs (`/me/section-prefs`) for any signed-in user.

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;

use crate::bootstrap::Outbound;
use crate::settings::effects::Effects;
use crate::settings::library_policy_service::LibraryPolicyService;
use crate::settings::models::{DownloadPolicyView, LibraryScanScheduleResponse};
use crate::settings::musicbrainz::MusicBrainzLifecycle;
use crate::settings::section_prefs::{LinkStatus, SectionPrefsStore};
use crate::settings::services::SettingsService;
use crate::settings::verify::{LiveProbes, VerifyProbes};
use droppedneedle_catalog::reads::catalog::library::LocalCatalog;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
use droppedneedle_library::management::settings::service::LibraryManagementService;
use droppedneedle_library::policy::LibraryPolicyCatalog;
use droppedneedle_library::publish::bin::BinSource;
use droppedneedle_providers::Providers;

/// Section-prefs backends: the toggle store plus per-user link state.
pub struct SectionPrefsDeps {
    /// Toggle store.
    pub store: Arc<dyn SectionPrefsStore>,
    /// Link state.
    pub links: Arc<dyn LinkStatus>,
}

/// Everything the settings routers need, built once at boot.
#[derive(Clone)]
pub struct SettingsSetup {
    service: Arc<SettingsService>,
    lifecycle: Arc<MusicBrainzLifecycle>,
    management: Arc<LibraryManagementService>,
    prefs: Option<Arc<SectionPrefsDeps>>,
    policy: Arc<LibraryPolicyService>,
    /// Checks the recycle bin path the download settings show; test
    /// bundles have none.
    bins: Option<BinSource>,
    /// The scan history the schedule shows its last scan from; test
    /// bundles have none.
    scans: Option<Arc<LocalCatalog>>,
    /// Test bundles own their scratch config directory and remove it
    /// when the last clone drops.
    #[cfg(any(test, feature = "test-support"))]
    _scratch: Option<Arc<ScratchDir>>,
}

impl SettingsSetup {
    /// Build over the platform's settings, ids, shared HTTP client and
    /// deployment timezone, with the wired fan-out in `effects`. The
    /// Library Management routes save through `management`, the library's
    /// own service, so an activation dry run and a page save never save
    /// over each other. The schedule reads its last scan from `scans`, the
    /// Verify probes check keys through the clients in `providers`, and the
    /// download settings say through `bins` when the recycle bin path is
    /// not used.
    pub fn build(
        platform: &Platform,
        effects: Effects,
        outbound: &Outbound,
        management: Arc<LibraryManagementService>,
        scans: Arc<LocalCatalog>,
        providers: &Providers,
        bins: BinSource,
    ) -> Self {
        let mut setup = Self::assemble(
            platform.settings.clone(),
            effects,
            platform.ids.clone(),
            Arc::new(
                LiveProbes::new(platform.http.shared().clone(), providers).presenting(
                    droppedneedle_auth::federated::settings::InstallIds::new(
                        platform.settings.clone(),
                    ),
                ),
            ),
            platform.config.timezone.clone(),
            outbound.clone(),
            management,
            platform.clock.clone(),
        );
        setup.scans = Some(scans);
        setup.bins = Some(bins);
        setup
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        store: Arc<ConfigStore>,
        effects: Effects,
        ids: Arc<dyn IdGenerator>,
        probes: Arc<dyn VerifyProbes>,
        timezone: Option<String>,
        outbound: Outbound,
        management: Arc<LibraryManagementService>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let service = Arc::new(
            SettingsService::new(store.clone(), effects.clone(), ids, probes, clock)
                .with_timezone(timezone),
        );
        Self {
            policy: Arc::new(LibraryPolicyService::new(service.clone(), None)),
            service,
            lifecycle: Arc::new(MusicBrainzLifecycle::new(store, effects, outbound)),
            management,
            prefs: None,
            bins: None,
            scans: None,
            #[cfg(any(test, feature = "test-support"))]
            _scratch: None,
        }
    }

    /// Attach the section-prefs backends.
    #[must_use]
    pub fn with_section_prefs(
        mut self,
        store: Arc<dyn SectionPrefsStore>,
        links: Arc<dyn LinkStatus>,
    ) -> Self {
        self.prefs = Some(Arc::new(SectionPrefsDeps { store, links }));
        self
    }

    /// Attach the catalog reads behind the library policy routes.
    #[must_use]
    pub fn with_library_catalog(mut self, catalog: Arc<dyn LibraryPolicyCatalog>) -> Self {
        self.policy = Arc::new(LibraryPolicyService::new(
            self.service.clone(),
            Some(catalog),
        ));
        self
    }

    /// Test bundle: scratch store, no save effects (the bundle shares no
    /// cache, sweep or worker with anything), live probes, and memory
    /// prefs. Tests that verify
    /// connections use [`SettingsSetup::for_tests_with_probes`] with a
    /// scripted fake instead, so no test touches live networks.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(ids: Arc<dyn IdGenerator>) -> Result<Self, String> {
        let providers = Providers::closed();
        Self::for_tests_with_probes(
            ids,
            Arc::new(LiveProbes::new(reqwest::Client::new(), &providers)),
        )
    }

    /// Test bundle with scripted verify probes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests_with_probes(
        ids: Arc<dyn IdGenerator>,
        probes: Arc<dyn VerifyProbes>,
    ) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};

        use crate::settings::effects::LiveSaveEffects;
        use crate::settings::section_prefs::{MemorySectionPrefsStore, StaticLinkStatus};
        use droppedneedle_kernel::jobs::events_kick::NoopKick;
        use droppedneedle_kernel::runtime_config::Crypto;

        /// Scratch-dir sequence so parallel test states never share a store.
        static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let scratch = ScratchDir(std::env::temp_dir().join(format!(
            "droppedneedle-settings-test-{}-{seq}",
            std::process::id()
        )));
        let crypto = Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| error.to_string())?;
        let store = Arc::new(ConfigStore::scratch(&scratch.0, crypto)?);
        // No runner: a save answers at once and its effects never run.
        let (effects, _) = Effects::new(
            store.clone(),
            Arc::new(LiveSaveEffects::new(Arc::new(NoopKick))),
        );
        let management = Arc::new(LibraryManagementService::new(store.clone()));
        let mut setup = Self::assemble(
            store,
            effects,
            ids,
            probes,
            None,
            Outbound::live(),
            management,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        );
        setup.prefs = Some(Arc::new(SectionPrefsDeps {
            store: Arc::new(MemorySectionPrefsStore::new()),
            links: Arc::new(StaticLinkStatus {
                listenbrainz: false,
                lastfm: false,
            }),
        }));
        setup._scratch = Some(Arc::new(scratch));
        Ok(setup)
    }

    /// The wired section service.
    pub fn service(&self) -> &SettingsService {
        &self.service
    }

    /// `view` with the reason its recycle bin path is not used, if it is
    /// not. The check resolves paths, so it runs on the blocking pool.
    pub async fn with_bin_warning(&self, mut view: DownloadPolicyView) -> DownloadPolicyView {
        let Some(bins) = self.bins.clone() else {
            return view;
        };
        let configured = view.policy.recycle_bin_path.clone();
        match tokio::task::spawn_blocking(move || bins.refusal(&configured)).await {
            Ok(refusal) => {
                view.recycle_bin_warning = refusal.map(|refusal| refusal.message().to_owned());
            }
            Err(error) => tracing::warn!(%error, "recycle bin check did not run"),
        }
        view
    }

    /// The library policy routes' service.
    pub fn library_policy(&self) -> &LibraryPolicyService {
        &self.policy
    }

    /// The MusicBrainz lifecycle.
    pub fn lifecycle(&self) -> &MusicBrainzLifecycle {
        &self.lifecycle
    }

    /// Run one Library Management operation on the blocking pool: every
    /// operation reads and may write the config file, and validation
    /// checks the library roots on disk.
    pub async fn management<T, F>(&self, op: F) -> Result<T, SettingsError>
    where
        T: Send + 'static,
        F: FnOnce(&LibraryManagementService) -> Result<T, SettingsError> + Send + 'static,
    {
        let management = self.management.clone();
        match tokio::task::spawn_blocking(move || op(&management)).await {
            Ok(result) => result,
            Err(cause) => Err(SettingsError::internal(&format!(
                "library management task failed: {cause}"
            ))),
        }
    }

    /// Fill in when the last scan finished, from the scan history. The
    /// value is shown only: a history that cannot be read leaves it absent
    /// and is logged, and never fails the read or a save that already landed.
    pub async fn with_last_scan(
        &self,
        mut schedule: LibraryScanScheduleResponse,
    ) -> LibraryScanScheduleResponse {
        if let Some(scans) = &self.scans {
            match scans.stats_extras().await {
                Ok(extras) => schedule.last_scan_at = extras.last_scan_at,
                Err(error) => {
                    tracing::warn!(%error, "could not read the last scan for the schedule");
                }
            }
        }
        schedule
    }

    /// The section-prefs backends, when wired.
    pub fn prefs(&self) -> Option<&Arc<SectionPrefsDeps>> {
        self.prefs.as_ref()
    }
}

/// Every settings route: the section registry's, the hand-written
/// admin-only `/settings/*` routes, and the per-user section prefs.
pub fn routes() -> Routes<SettingsSetup> {
    use super::handlers as h;
    super::registry::routes()
        .admin(routes!(h::get_schedule, h::put_schedule))
        .admin(routes!(h::get_library, h::put_library))
        .admin(routes!(h::add_library_path, h::remove_library_path))
        .admin(routes!(h::get_library_policy_tree))
        .admin(routes!(h::preview_library_policy_impact))
        .admin(routes!(h::preview_library_policy_apply))
        .admin(routes!(h::get_restorable_library_roots))
        .admin(routes!(h::restore_library_roots))
        .admin(routes!(h::get_library_path_mapping))
        .admin(routes!(h::get_cache_ttls))
        .admin(routes!(h::verify_jellyfin))
        .admin(routes!(h::verify_navidrome))
        .admin(routes!(h::verify_plex))
        .admin(routes!(h::get_plex_libraries))
        .admin(routes!(h::verify_listenbrainz))
        .admin(routes!(h::verify_youtube))
        .admin(routes!(h::test_ticketmaster))
        .admin(routes!(h::test_skiddle))
        .admin(routes!(h::verify_hibp))
        .admin(routes!(h::verify_oidc))
        .admin(routes!(h::get_musicbrainz, h::put_musicbrainz))
        .admin(routes!(h::stage_brainzmash))
        .admin(routes!(h::consent_brainzmash))
        .admin(routes!(h::verify_musicbrainz))
        .admin(routes!(h::activate_brainzmash))
        .admin(routes!(h::test_slskd))
        .admin(routes!(h::get_sabnzbd, h::put_sabnzbd))
        .admin(routes!(h::test_sabnzbd))
        .admin(routes!(h::get_source_priority, h::put_source_priority))
        .admin(routes!(h::get_policy, h::put_policy))
        .admin(routes!(h::get_policy_summary))
        .admin(routes!(h::list_indexers, h::create_indexer))
        .admin(routes!(h::get_search_backend, h::put_search_backend))
        .admin(routes!(h::update_indexer, h::delete_indexer))
        .admin(routes!(h::reorder_indexers))
        .admin(routes!(h::test_indexer))
        .admin(routes!(h::test_prowlarr))
        .admin(routes!(
            h::dropped_library_sync,
            h::dropped_library_sync_put
        ))
        .admin(routes!(
            h::get_library_management,
            h::put_library_management
        ))
        .admin(routes!(h::get_library_management_activation_health))
        .admin(routes!(h::preview_library_management_impact))
        .admin(routes!(h::validate_library_management))
        .admin(routes!(h::create_library_management_profile))
        .admin(routes!(
            h::get_library_management_profile,
            h::update_library_management_profile,
            h::delete_library_management_profile
        ))
        .admin(routes!(h::copy_library_management_profile))
        .admin(routes!(h::get_library_management_preset_diff))
        .admin(routes!(h::export_library_management_profile))
        .admin(routes!(h::preview_library_management_profile_import))
        .admin(routes!(h::import_library_management_profile))
        .user(routes!(h::get_section_prefs, h::put_section_prefs))
}

/// A scratch directory removed on drop (test bundles only).
#[cfg(any(test, feature = "test-support"))]
struct ScratchDir(std::path::PathBuf);

#[cfg(any(test, feature = "test-support"))]
impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "settings test scratch dir was not removed");
        }
    }
}
