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

use crate::bootstrap::Outbound;
use crate::settings::effects::Effects;
use crate::settings::library_catalog::SqliteLibraryPolicyCatalog;
use crate::settings::library_policy_service::LibraryPolicyService;
use crate::settings::models::LibraryScanScheduleResponse;
use crate::settings::musicbrainz::MusicBrainzLifecycle;
use crate::settings::section_prefs::SectionPrefs;
use crate::settings::services::SettingsService;
use crate::settings::verify::{LiveProbes, VerifyProbes};
use droppedneedle_auth::sqlite::LastFmLinks;
use droppedneedle_catalog::metadata::library::LocalCatalog;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
use droppedneedle_library::management::settings::service::LibraryManagementService;
use droppedneedle_library::publish::bin::BinSource;
use droppedneedle_library::settings_models::LibraryManagementSettingsResponse;
use droppedneedle_providers::Providers;

/// Everything the settings routers need, built once at boot.
#[derive(Clone)]
pub struct SettingsSetup {
    service: Arc<SettingsService>,
    lifecycle: Arc<MusicBrainzLifecycle>,
    management: Arc<LibraryManagementService>,
    prefs: Arc<SectionPrefs>,
    policy: Arc<LibraryPolicyService>,
    /// Checks the recycle bin path the Library Management settings show.
    bins: BinSource,
    /// The scan history the schedule shows its last scan from.
    scans: Arc<LocalCatalog>,
}

impl SettingsSetup {
    /// Build over the platform's settings, database, ids and deployment
    /// timezone, with the wired fan-out in `effects`. The Library
    /// Management routes save through `management`, the library's own
    /// service, so an activation dry run and a page save never save over
    /// each other. The Verify routes check keys through `probes`, the
    /// section prefs read Last.fm links through `lastfm`, and the Library
    /// Management settings say through `bins` when the recycle bin path is
    /// not used.
    pub fn build(
        platform: &Platform,
        effects: Effects,
        outbound: &Outbound,
        management: Arc<LibraryManagementService>,
        probes: Arc<dyn VerifyProbes>,
        lastfm: LastFmLinks,
        bins: BinSource,
    ) -> Self {
        let store = platform.settings.clone();
        let service = Arc::new(
            SettingsService::new(store.clone(), effects.clone(), platform.ids.clone(), probes)
                .with_timezone(platform.config.timezone.clone()),
        );
        Self {
            policy: Arc::new(LibraryPolicyService::new(
                service.clone(),
                Arc::new(SqliteLibraryPolicyCatalog::new(platform.db.clone())),
            )),
            service,
            lifecycle: Arc::new(MusicBrainzLifecycle::new(store, effects, outbound.clone())),
            management,
            prefs: Arc::new(SectionPrefs::new(
                platform.db.clone(),
                platform.clock.clone(),
                lastfm,
            )),
            bins,
            scans: Arc::new(LocalCatalog::new(platform.db.clone())),
        }
    }

    /// The Verify probes production uses: keys are checked through the
    /// clients in `providers` and the platform's shared HTTP client.
    pub fn live_probes(platform: &Platform, providers: &Providers) -> Arc<dyn VerifyProbes> {
        Arc::new(
            LiveProbes::new(platform.http.shared().clone(), providers).presenting(
                droppedneedle_auth::federated::settings::InstallIds::new(platform.settings.clone()),
            ),
        )
    }

    /// The wired section service.
    pub fn service(&self) -> &SettingsService {
        &self.service
    }

    /// `response` with what keeps a recycle bin from being used, if
    /// anything does. The check resolves paths, so it runs on the blocking
    /// pool.
    pub async fn with_bin_warning(
        &self,
        mut response: LibraryManagementSettingsResponse,
    ) -> LibraryManagementSettingsResponse {
        let bins = self.bins.clone();
        let configured = response.settings.recycle_bin_path.clone();
        match tokio::task::spawn_blocking(move || bins.warning(&configured)).await {
            Ok(warning) => response.recycle_bin_warning = warning,
            Err(error) => tracing::warn!(%error, "recycle bin check did not run"),
        }
        response
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
        match self.scans.stats_extras().await {
            Ok(extras) => schedule.last_scan_at = extras.last_scan_at,
            Err(error) => {
                tracing::warn!(%error, "could not read the last scan for the schedule");
            }
        }
        schedule
    }

    /// The per-user section prefs.
    pub fn prefs(&self) -> &SectionPrefs {
        &self.prefs
    }
}

/// Every settings route: the section registry's, the hand-written
/// admin-only `/settings/*` routes, and the per-user section prefs.
pub fn routes() -> Routes<SettingsSetup> {
    use super::handlers as h;
    super::registry::routes()
        .admin(routes!(h::get_schedule, h::put_schedule))
        .admin(routes!(h::get_library, h::put_library))
        .admin(routes!(h::get_library_policy_tree))
        .admin(routes!(h::preview_library_policy_impact))
        .admin(routes!(h::preview_library_policy_apply))
        .admin(routes!(h::get_restorable_library_roots))
        .admin(routes!(h::restore_library_roots))
        .admin(routes!(h::verify_jellyfin))
        .admin(routes!(h::verify_navidrome))
        .admin(routes!(h::verify_plex))
        .admin(routes!(h::get_plex_libraries))
        .admin(routes!(h::verify_listenbrainz))
        .admin(routes!(h::verify_lastfm))
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
        .admin(routes!(h::list_indexers, h::create_indexer))
        .admin(routes!(h::get_search_backend, h::put_search_backend))
        .admin(routes!(h::update_indexer, h::delete_indexer))
        .admin(routes!(h::reorder_indexers))
        .admin(routes!(h::test_indexer))
        .admin(routes!(h::test_prowlarr))
        .admin(routes!(
            h::get_library_management,
            h::put_library_management
        ))
        .admin(routes!(h::get_library_management_activation_health))
        .admin(routes!(h::preview_library_management_impact))
        .admin(routes!(h::validate_library_management))
        .admin(routes!(h::delete_library_management_profile))
        .admin(routes!(h::copy_library_management_profile))
        .admin(routes!(h::get_library_management_preset_diff))
        .admin(routes!(h::export_library_management_profile))
        .admin(routes!(h::preview_library_management_profile_import))
        .admin(routes!(h::import_library_management_profile))
        .user(routes!(h::get_section_prefs, h::put_section_prefs))
}
