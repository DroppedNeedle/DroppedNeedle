//! Settings reads, writes, and connection checks over the config store.
//!
//! Sections cross the wire as their `runtime_config` types: plain
//! sections as they are, secret sections as [`Masked`] (secrets swapped
//! for their mask on read; a mask that comes back keeps the stored
//! secret). Reads never mutate the store. Saves run on the blocking pool
//! (one durable write on the settings database) and answer once the
//! section's effects have run (see [`Effects`]).
//!
//! Verify methods test the submitted values, not the stored config, so
//! Test works before the first save and reflects edits. A secret that
//! still holds its mask resolves to the stored one through
//! [`ConfigStore::unmask`], the same rule a save applies; an empty secret
//! fails the verdict without a network round trip.

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;

use super::effects::Effects;
use super::models::{
    DownloadPolicyView, FrontendCacheTTLs, IndexerSavedResponse, IndexerTestResponse,
    JellyfinUserInfo, JellyfinVerifyResponse, LibraryScanScheduleResponse,
    LibrarySettingsSaveRequest, PlexLibrarySectionInfo, PlexVerifyResponse, PolicySummaryResponse,
    ProwlarrTestResponse, SabnzbdTestResponse, SourcePriorityOrder, TestConnectionResponse,
    UsenetSearchBackend, VerifyConnectionResponse,
};
use super::quality;
use super::verify::{VerifyProbes, check_hibp_file, require_service_url};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::runtime_config::secret_sections::{
    AdvancedSettings, DownloadClients, EventsSettings, IdentificationPolicy, JellyfinConnection,
    LibraryRoot, ListenBrainzConnection, NavidromeConnection, NewznabIndexer, OidcConnection,
    PlexConnection, ProwlarrConnection, SabnzbdConnection, SlskdConnection, TypedLibrary,
    YouTubeConnection,
};
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, LibraryScanSchedule, PlainSection, SecuritySettings, SourcePriority,
    UsenetBackendSetting, derive_default_order, validate_quality_recipe,
};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
use droppedneedle_kernel::runtime_config::{
    ConfigError, ConfigStore, Masked, Plain, SecretSection, Section, UpdateError,
};
use droppedneedle_library::policy as library_policy;
use droppedneedle_library::settings_models::LibrarySettingsResponse;

/// Settings reads, writes, and verify probes. Built once at boot; every
/// call reads the store, so a save takes effect on the next call.
pub struct SettingsService {
    /// Config store.
    pub store: Arc<ConfigStore>,
    /// Tells when a save's effects have run.
    pub effects: Effects,
    /// Id mint for new library roots.
    pub ids: Arc<dyn IdGenerator>,
    /// `TZ` from the deployment config, when set.
    pub timezone: Option<String>,
    /// Connection probes behind the verify methods.
    probes: Arc<dyn VerifyProbes>,
    /// Wall time, which picks the zone label (summer or winter time).
    clock: Arc<dyn Clock>,
}

impl SettingsService {
    /// Build over the shared store, effects, ids, probes and clock.
    pub fn new(
        store: Arc<ConfigStore>,
        effects: Effects,
        ids: Arc<dyn IdGenerator>,
        probes: Arc<dyn VerifyProbes>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            effects,
            ids,
            timezone: None,
            probes,
            clock,
        }
    }

    /// Label scan schedules with this timezone name (`TZ`).
    #[must_use]
    pub fn with_timezone(mut self, timezone: Option<String>) -> Self {
        self.timezone = timezone;
        self
    }

    fn config(&self, error: ConfigError) -> SettingsError {
        SettingsError::from_config(error)
    }

    /// Run one store write on the blocking pool (it waits for its turn on
    /// the writer lane and syncs the commit to disk).
    async fn write<T, F>(&self, op: F) -> Result<T, SettingsError>
    where
        T: Send + 'static,
        F: FnOnce(&ConfigStore) -> Result<T, ConfigError> + Send + 'static,
    {
        let store = self.store.clone();
        match tokio::task::spawn_blocking(move || op(&store)).await {
            Ok(result) => result.map_err(|error| self.config(error)),
            Err(cause) => Err(SettingsError::internal(&format!(
                "settings write task failed: {cause}"
            ))),
        }
    }

    // --- generic section access -------------------------------------------------

    /// Read one plain section. Secret sections go through
    /// [`SettingsService::get_masked`].
    pub fn get<S: PlainSection>(&self) -> Result<S, SettingsError> {
        self.store.get().map_err(|error| self.config(error))
    }

    /// Validate, normalize, and save one plain section, then run its
    /// fan-out. Returns the normalized stored form.
    pub async fn save<S: PlainSection + Send + 'static>(
        &self,
        section: S,
    ) -> Result<S, SettingsError> {
        let mark = self.effects.mark(S::KEY);
        let saved = self
            .write(move |store| store.save_blocking(section))
            .await?;
        self.effects.saved(mark).await;
        Ok(saved)
    }

    /// Read one secret section with its secrets masked.
    pub fn get_masked<S: SecretSection>(&self) -> Result<Masked<S>, SettingsError> {
        self.store.get_masked().map_err(|error| self.config(error))
    }

    /// Save one secret section (a mask keeps the stored secret), then run
    /// its fan-out. Returns the saved section masked.
    pub async fn save_masked<S: SecretSection + Send + 'static>(
        &self,
        incoming: Masked<S>,
    ) -> Result<Masked<S>, SettingsError> {
        self.save_submitted(incoming.into_inner()).await
    }

    /// Save a secret section built server-side from submitted values (a
    /// masked read with non-secret fields edited, or a request body).
    async fn save_submitted<S: SecretSection + Send + 'static>(
        &self,
        incoming: S,
    ) -> Result<Masked<S>, SettingsError> {
        let mark = self.effects.mark(S::KEY);
        let saved = self
            .write(move |store| store.save_secret_blocking(incoming))
            .await?;
        self.effects.saved(mark).await;
        Ok(saved)
    }

    fn unmask<S: SecretSection>(&self, incoming: Masked<S>) -> Result<Plain<S>, SettingsError> {
        self.store
            .unmask(incoming)
            .map_err(|error| self.config(error))
    }

    // --- sections with a wire shape of their own ----------------------------------

    /// The scan schedule plus the server timezone label.
    pub fn get_schedule(&self) -> Result<LibraryScanScheduleResponse, SettingsError> {
        Ok(LibraryScanScheduleResponse {
            schedule: self.get()?,
            server_timezone: server_timezone(self.timezone.as_deref(), self.clock.now_unix()),
            last_scan_at: None,
        })
    }

    /// Save the scan schedule.
    pub async fn save_schedule(
        &self,
        schedule: LibraryScanSchedule,
    ) -> Result<LibraryScanScheduleResponse, SettingsError> {
        Ok(LibraryScanScheduleResponse {
            schedule: self.save(schedule).await?,
            server_timezone: server_timezone(self.timezone.as_deref(), self.clock.now_unix()),
            last_scan_at: None,
        })
    }

    /// The acquisition source try-order.
    pub fn get_source_priority(&self) -> Result<SourcePriorityOrder, SettingsError> {
        let stored: SourcePriority = self.get()?;
        Ok(SourcePriorityOrder { order: stored.0 })
    }

    /// Save the try-order. The store cleans it (bundled sources present,
    /// well-formed plugin keys kept, the rest dropped) and the echo shows
    /// the cleaned order.
    pub async fn save_source_priority(
        &self,
        body: SourcePriorityOrder,
    ) -> Result<SourcePriorityOrder, SettingsError> {
        let saved = self.save(SourcePriority(body.order)).await?;
        Ok(SourcePriorityOrder { order: saved.0 })
    }

    /// The active Usenet search backend.
    pub fn get_usenet_backend(&self) -> Result<UsenetSearchBackend, SettingsError> {
        let stored: UsenetBackendSetting = self.get()?;
        Ok(UsenetSearchBackend { backend: stored.0 })
    }

    /// Save the active Usenet search backend.
    pub async fn save_usenet_backend(
        &self,
        body: UsenetSearchBackend,
    ) -> Result<UsenetSearchBackend, SettingsError> {
        let saved = self.save(UsenetBackendSetting(body.backend)).await?;
        Ok(UsenetSearchBackend { backend: saved.0 })
    }

    /// The SABnzbd connection (the one client in `download_clients`).
    pub fn get_sabnzbd(&self) -> Result<Masked<SabnzbdConnection>, SettingsError> {
        Ok(self
            .get_masked::<DownloadClients>()?
            .map(|clients| clients.sabnzbd))
    }

    /// Save the SABnzbd connection (a masked key keeps the stored one).
    pub async fn save_sabnzbd(
        &self,
        incoming: Masked<SabnzbdConnection>,
    ) -> Result<Masked<SabnzbdConnection>, SettingsError> {
        let incoming = incoming.into_inner();
        let store = self.store.clone();
        let mark = self.effects.mark(DownloadClients::KEY);
        let saved = self
            .write(move |_| {
                store
                    .update_secret_blocking::<DownloadClients, ConfigError, _>(|stored| {
                        let mut clients = stored.into_inner();
                        clients.sabnzbd = incoming;
                        Ok(clients)
                    })
                    .map_err(|error| match error {
                        UpdateError::Rejected(error) | UpdateError::Config(error) => error,
                    })
            })
            .await?;
        self.effects.saved(mark).await;
        Ok(saved.map(|clients| clients.sabnzbd))
    }

    /// The advanced settings the web app reads at start.
    pub fn get_cache_ttls(&self) -> Result<FrontendCacheTTLs, SettingsError> {
        let stored: Masked<AdvancedSettings> = self.get_masked()?;
        Ok(FrontendCacheTTLs {
            discover_queue_auto_generate: stored.discover_queue_auto_generate,
        })
    }

    // --- library settings -----------------------------------------------------------

    /// The library settings: normalized roots plus the policy revision,
    /// the reconciliation projection, and warnings (AcoustID key masked).
    /// Normalizing checks each root on disk, so it runs on the blocking
    /// pool.
    pub async fn get_library(&self) -> Result<LibrarySettingsResponse, SettingsError> {
        let store = self.store.clone();
        self.library_blocking(move || {
            let stored = store
                .get_masked::<TypedLibrary>()
                .map_err(SettingsError::from_config)?;
            let resolved = stored.map(|library| library_policy::resolve_stored(&library));
            Ok(library_policy::settings_response(resolved))
        })
        .await
    }

    /// Save the library settings. Under the store's write lock: a stale
    /// expected revision is a 409 before anything else, then the
    /// candidate is normalized, then dropping every root while the
    /// catalog holds tracks is a 400. A changed import naming script must
    /// pass the naming script checks. A masked AcoustID key keeps the
    /// stored one.
    pub async fn save_library(
        &self,
        request: LibrarySettingsSaveRequest,
        catalog_has_tracks: bool,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        self.update_library(move |stored| {
            library_policy::check_revision(&stored, &request.expected_policy_revision)?;
            // Only a script the person changed is checked, so a stored one a
            // later build refuses never blocks saving the rest.
            let script = request.settings.naming_script.trim();
            if !script.is_empty() && script != stored.naming_script.trim() {
                droppedneedle_library::import::naming::check(script)?;
            }
            let normalized = library_policy::normalize(&request.settings)?;
            library_policy::guard_last_root(&normalized, catalog_has_tracks)?;
            Ok(normalized)
        })
        .await
    }

    /// Add one library root path. The path must be a directory on this
    /// machine; adding a path that is already a root is a silent no-op.
    pub async fn add_library_path(
        &self,
        path: &str,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let candidate = path.trim().to_owned();
        if candidate.is_empty() {
            return Err(SettingsError::InvalidInput {
                message: "Library path is required.".to_owned(),
            });
        }
        let is_dir = tokio::fs::metadata(&candidate)
            .await
            .is_ok_and(|meta| meta.is_dir());
        if !is_dir {
            return Err(SettingsError::InvalidInput {
                message: format!("Path does not exist or is not a directory: {candidate}"),
            });
        }
        let id = self.ids.new_id();
        self.update_library(move |mut library| {
            if !library
                .library_roots
                .iter()
                .any(|root| root.path == candidate)
            {
                let label = std::path::Path::new(&candidate)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|name| !name.is_empty())
                    .unwrap_or(&candidate)
                    .to_owned();
                library.library_roots.push(LibraryRoot {
                    id,
                    path: candidate,
                    label,
                    policy: IdentificationPolicy::Automatic,
                    rules: Vec::new(),
                });
            }
            library_policy::normalize(&library)
        })
        .await
    }

    /// Remove every library root at one path. Unknown paths are a silent
    /// no-op; removing the last root while the catalog holds tracks is a
    /// 400.
    pub async fn remove_library_path(
        &self,
        path: &str,
        catalog_has_tracks: bool,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let path = path.to_owned();
        self.update_library(move |mut library| {
            let before = library.library_roots.len();
            library.library_roots.retain(|root| root.path != path);
            if library.library_roots.len() < before {
                library_policy::guard_last_root(&library, catalog_has_tracks)?;
            }
            library_policy::normalize(&library)
        })
        .await
    }

    /// Read, check, and save the library settings in one settings
    /// transaction, on the blocking pool. `update` gets the stored settings
    /// with the AcoustID key masked and returns the settings to save; an
    /// `Err` writes nothing. `update` runs inside the write, so it touches
    /// no file: it normalizes with `library_policy::normalize`, and the
    /// availability warnings are worked out after the commit. Every
    /// library-settings writer works this way, the scan engine's add-root
    /// included.
    pub async fn update_library<F>(
        &self,
        update: F,
    ) -> Result<LibrarySettingsResponse, SettingsError>
    where
        F: FnOnce(TypedLibrary) -> Result<TypedLibrary, SettingsError> + Send + 'static,
    {
        let store = self.store.clone();
        let mark = self.effects.mark(TypedLibrary::KEY);
        let response = self
            .library_blocking(move || {
                let saved = store
                    .update_secret_blocking::<TypedLibrary, SettingsError, _>(|stored| {
                        update(stored.into_inner())
                    })
                    .map_err(|error| match error {
                        UpdateError::Rejected(error) => error,
                        UpdateError::Config(error) => SettingsError::from_config(error),
                    })?;
                let resolved = saved.try_map(|library| library_policy::resolve(&library))?;
                Ok(library_policy::settings_response(resolved))
            })
            .await?;
        self.effects.saved(mark).await;
        Ok(response)
    }

    async fn library_blocking<T, F>(&self, op: F) -> Result<T, SettingsError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, SettingsError> + Send + 'static,
    {
        match tokio::task::spawn_blocking(op).await {
            Ok(result) => result,
            Err(cause) => Err(SettingsError::internal(&format!(
                "library settings task failed: {cause}"
            ))),
        }
    }

    // --- download policy --------------------------------------------------------------

    /// The acquisition policy plus its recipe verdict. A corrupt section
    /// degrades to the default policy with an `invalid` verdict instead
    /// of failing the read.
    pub fn get_policy(&self) -> Result<DownloadPolicyView, SettingsError> {
        match self.store.get::<DownloadPolicy>() {
            Ok(policy) => Ok(policy_view(policy)),
            Err(ConfigError::SectionDecode { reason, .. }) => Ok(DownloadPolicyView {
                policy: DownloadPolicy::default(),
                quality_recipe_status: "invalid".to_owned(),
                quality_recipe_error: Some(reason),
                recycle_bin_warning: None,
            }),
            Err(other) => Err(self.config(other)),
        }
    }

    /// Save the acquisition policy. A submitted recipe derives the legacy
    /// range, the legacy order, and the cutoff clamp (v2 save semantics).
    pub async fn save_policy(
        &self,
        policy: DownloadPolicy,
    ) -> Result<DownloadPolicyView, SettingsError> {
        let policy = self.apply_recipe(policy, true)?;
        Ok(policy_view(self.save(policy).await?))
    }

    /// A submitted recipe owns the legacy fields: the range, the FLAC/MP3
    /// switch, and (on save) the cutoff clamp.
    fn apply_recipe(
        &self,
        mut policy: DownloadPolicy,
        clamp_cutoff: bool,
    ) -> Result<DownloadPolicy, SettingsError> {
        if policy.quality_recipe.is_empty() {
            return Ok(policy);
        }
        if !policy.flac_mp3_only {
            return Err(SettingsError::InvalidInput {
                message: "A v2 recipe cannot be saved while non-FLAC/MP3 formats are enabled"
                    .to_owned(),
            });
        }
        validate_quality_recipe(&policy.quality_recipe).map_err(|error| self.config(error))?;
        let (quality_min, quality_max) = quality::legacy_range_from_recipe(&policy.quality_recipe);
        policy.quality_min = quality_min;
        policy.quality_max = quality_max;
        policy.flac_mp3_only = true;
        if clamp_cutoff {
            policy.quality_cutoff = quality::clamp_cutoff(
                &policy.quality_cutoff,
                &policy.quality_min,
                &policy.quality_max,
            );
        }
        Ok(policy)
    }

    /// Safe acquisition-policy summary for any signed-in user: the
    /// composed contract sentence and the recipe verdict, no admin
    /// internals.
    pub fn policy_summary(&self) -> Result<PolicySummaryResponse, SettingsError> {
        let stored: DownloadPolicy = self.get()?;
        let inputs = summary_inputs(&stored);
        let summary = quality::compose_summary(&inputs);
        let recipe_error = validate_quality_recipe(&stored.quality_recipe)
            .err()
            .map(|error| error.to_string());
        let (status, error) = quality::recipe_status(
            &stored.quality_recipe,
            stored.flac_mp3_only,
            recipe_error.is_none(),
            recipe_error.as_deref(),
        );
        Ok(PolicySummaryResponse {
            summary,
            legacy_rollback_compatible: stored.quality_recipe.is_empty(),
            quality_recipe_status: status,
            quality_recipe_error: error,
        })
    }

    // --- indexers ---------------------------------------------------------------------

    /// The configured Newznab indexers (keys masked unless unset).
    pub fn list_indexers(&self) -> Result<Vec<Masked<NewznabIndexer>>, SettingsError> {
        self.store
            .get_indexers()
            .map_err(|error| self.config(error))
    }

    /// Save one indexer: create when the id is blank, else update. A path
    /// id, when given, wins over the body id. A masked key keeps the
    /// stored one.
    pub async fn save_indexer(
        &self,
        incoming: Masked<NewznabIndexer>,
        path_id: Option<String>,
    ) -> Result<IndexerSavedResponse, SettingsError> {
        let mut indexer = incoming.into_inner();
        if let Some(id) = path_id {
            indexer.id = id;
        }
        let id = self
            .write(move |store| store.save_indexer_blocking(indexer))
            .await?;
        Ok(IndexerSavedResponse { id })
    }

    /// Delete one indexer. Unknown ids are a silent no-op.
    pub async fn delete_indexer(&self, indexer_id: &str) -> Result<(), SettingsError> {
        let indexer_id = indexer_id.to_owned();
        self.write(move |store| store.delete_indexer_blocking(&indexer_id))
            .await?;
        Ok(())
    }

    /// Persist a dragged-card priority order (1-based on save).
    pub async fn reorder_indexers(&self, ordered_ids: Vec<String>) -> Result<(), SettingsError> {
        self.write(move |store| store.reorder_indexers_blocking(&ordered_ids))
            .await?;
        Ok(())
    }

    // --- connection checks ------------------------------------------------------------

    /// Test the submitted Jellyfin values. The user list rides along on
    /// success for the admin picker.
    pub async fn verify_jellyfin(
        &self,
        incoming: Masked<JellyfinConnection>,
    ) -> Result<JellyfinVerifyResponse, SettingsError> {
        let url = require_service_url(&incoming.jellyfin_url, "Jellyfin URL")?;
        let resolved = self.unmask(incoming)?;
        let verdict = self.probes.jellyfin(&url, resolved.api_key.expose()).await;
        Ok(JellyfinVerifyResponse {
            success: verdict.valid,
            message: verdict.message,
            users: verdict
                .users
                .into_iter()
                .map(|(id, name)| JellyfinUserInfo { id, name })
                .collect(),
        })
    }

    /// Test the submitted Navidrome values.
    pub async fn verify_navidrome(
        &self,
        incoming: Masked<NavidromeConnection>,
    ) -> Result<VerifyConnectionResponse, SettingsError> {
        let url = require_service_url(&incoming.navidrome_url, "Navidrome URL")?;
        let resolved = self.unmask(incoming)?;
        let verdict = self
            .probes
            .navidrome(&url, &resolved.username, resolved.password.expose())
            .await;
        Ok(verdict_response(verdict.valid, verdict.message))
    }

    /// Test the submitted Plex values. Music libraries ride along on
    /// success.
    pub async fn verify_plex(
        &self,
        incoming: Masked<PlexConnection>,
    ) -> Result<PlexVerifyResponse, SettingsError> {
        let url = require_service_url(&incoming.plex_url, "Plex URL")?;
        let resolved = self.unmask(incoming)?;
        let verdict = self.probes.plex(&url, resolved.plex_token.expose()).await;
        Ok(PlexVerifyResponse {
            valid: verdict.valid,
            message: verdict.message,
            libraries: plex_sections(verdict.libraries),
        })
    }

    /// List Plex music libraries over the stored connection. Unconfigured
    /// is a 400; an unreachable Plex is a 502.
    pub async fn plex_libraries(&self) -> Result<Vec<PlexLibrarySectionInfo>, SettingsError> {
        let stored = self
            .store
            .get_raw::<PlexConnection>()
            .map_err(|error| self.config(error))?;
        let libraries = self
            .probes
            .plex_libraries(&stored.plex_url, stored.plex_token.expose())
            .await
            .map_err(|message| {
                if message == "Plex is not configured." {
                    SettingsError::InvalidInput { message }
                } else {
                    SettingsError::Upstream { cause: message }
                }
            })?;
        Ok(plex_sections(libraries))
    }

    /// Test the submitted ListenBrainz values. Rate limiting is a 429.
    pub async fn verify_listenbrainz(
        &self,
        incoming: Masked<ListenBrainzConnection>,
    ) -> Result<VerifyConnectionResponse, SettingsError> {
        let resolved = self.unmask(incoming)?;
        let verdict = self
            .probes
            .listenbrainz(&resolved.username, resolved.user_token.expose())
            .await;
        if verdict.rate_limited {
            return Err(SettingsError::RateLimited {
                message:
                    "ListenBrainz is temporarily rate-limiting this server. Try again shortly."
                        .to_owned(),
            });
        }
        Ok(verdict_response(verdict.valid, verdict.message))
    }

    /// Test the submitted YouTube key.
    pub async fn verify_youtube(
        &self,
        incoming: Masked<YouTubeConnection>,
    ) -> Result<VerifyConnectionResponse, SettingsError> {
        let resolved = self.unmask(incoming)?;
        let verdict = self.probes.youtube(resolved.api_key.expose()).await;
        Ok(verdict_response(verdict.valid, verdict.message))
    }

    /// Test the submitted Ticketmaster key.
    pub async fn verify_ticketmaster(
        &self,
        incoming: Masked<EventsSettings>,
    ) -> Result<VerifyConnectionResponse, SettingsError> {
        let resolved = self.unmask(incoming)?;
        let verdict = self
            .probes
            .ticketmaster(resolved.ticketmaster_api_key.expose())
            .await;
        Ok(verdict_response(verdict.valid, verdict.message))
    }

    /// Test the submitted Skiddle key.
    pub async fn verify_skiddle(
        &self,
        incoming: Masked<EventsSettings>,
    ) -> Result<VerifyConnectionResponse, SettingsError> {
        let resolved = self.unmask(incoming)?;
        let verdict = self.probes.skiddle(resolved.skiddle_api_key.expose()).await;
        Ok(verdict_response(verdict.valid, verdict.message))
    }

    /// Check the submitted HIBP hash-list path (it must exist and start
    /// with a `40-char-SHA1:count` line). Reads the file on the blocking
    /// pool.
    pub async fn verify_hibp(
        &self,
        security: SecuritySettings,
    ) -> Result<VerifyConnectionResponse, SettingsError> {
        let verdict =
            tokio::task::spawn_blocking(move || check_hibp_file(&security.hibp_local_path))
                .await
                .map_err(|cause| {
                    SettingsError::internal(&format!("hibp check task failed: {cause}"))
                })?;
        Ok(verdict_response(verdict.valid, verdict.message))
    }

    /// Test the submitted OIDC issuer (fetches its discovery document).
    pub async fn verify_oidc(
        &self,
        incoming: Masked<OidcConnection>,
    ) -> Result<VerifyConnectionResponse, SettingsError> {
        let verdict = self.probes.oidc(&incoming.issuer).await;
        Ok(verdict_response(verdict.valid, verdict.message))
    }

    /// Test the submitted slskd values.
    pub async fn verify_slskd(
        &self,
        incoming: Masked<SlskdConnection>,
    ) -> Result<TestConnectionResponse, SettingsError> {
        let url = require_service_url(&incoming.url, "Download client URL")?;
        let resolved = self.unmask(incoming)?;
        let verdict = self.probes.slskd(&url, resolved.api_key.expose()).await;
        Ok(TestConnectionResponse {
            valid: verdict.valid,
            version: verdict.version,
            message: verdict.message,
        })
    }

    /// Test the submitted SABnzbd values. The submitted downloads mount
    /// is diagnosed, not the stored one.
    pub async fn verify_sabnzbd(
        &self,
        incoming: Masked<SabnzbdConnection>,
    ) -> Result<SabnzbdTestResponse, SettingsError> {
        let url = require_service_url(&incoming.url, "SABnzbd URL")?;
        let clients = incoming.map(|sabnzbd| DownloadClients { sabnzbd });
        let unmasked = self.unmask(clients)?;
        let resolved = &unmasked.sabnzbd;
        let verdict = self
            .probes
            .sabnzbd(&url, resolved.api_key.expose(), &resolved.downloads_mount)
            .await;
        let diagnosis = verdict.diagnosis;
        Ok(SabnzbdTestResponse {
            valid: verdict.valid,
            version: verdict.version,
            message: verdict.message,
            categories: verdict.categories,
            complete_dir: verdict.complete_dir,
            mount_has_files: Some(diagnosis.as_ref().is_none_or(|d| d.mount_has_files)),
            resolvable_downloads: Some(diagnosis.as_ref().map_or(0, |d| d.resolvable_downloads)),
            sampled_downloads: Some(diagnosis.as_ref().map_or(0, |d| d.sampled_downloads)),
            mount_message: diagnosis.and_then(|d| d.mount_message),
        })
    }

    /// Test the submitted Prowlarr values.
    pub async fn verify_prowlarr(
        &self,
        incoming: Masked<ProwlarrConnection>,
    ) -> Result<ProwlarrTestResponse, SettingsError> {
        let url = require_service_url(&incoming.url, "Prowlarr URL")?;
        let resolved = self.unmask(incoming)?;
        let verdict = self.probes.prowlarr(&url, resolved.api_key.expose()).await;
        Ok(ProwlarrTestResponse {
            valid: verdict.valid,
            version: verdict.version,
            message: verdict.message,
            indexer_count: verdict.indexer_count,
        })
    }

    /// Test one indexer's caps with the submitted URL and key (a masked
    /// key tests the stored key of the indexer with the same id).
    pub async fn verify_indexer(
        &self,
        incoming: Masked<NewznabIndexer>,
    ) -> Result<IndexerTestResponse, SettingsError> {
        let url = require_service_url(&incoming.url, "Indexer URL")?;
        let resolved = self
            .store
            .unmask_indexer(incoming)
            .map_err(|error| self.config(error))?;
        let verdict = self.probes.newznab(&url, resolved.api_key.expose()).await;
        Ok(IndexerTestResponse {
            valid: verdict.valid,
            version: verdict.version,
            message: verdict.message,
            supports_audio_search: verdict.supports_audio_search,
            category_count: verdict.category_count,
            suggested_url: verdict.suggested_url,
        })
    }

    /// The probes, for the MusicBrainz lifecycle's verify step.
    pub fn probes(&self) -> &dyn VerifyProbes {
        self.probes.as_ref()
    }
}

fn verdict_response(valid: bool, message: String) -> VerifyConnectionResponse {
    VerifyConnectionResponse { valid, message }
}

fn plex_sections(libraries: Vec<(String, String)>) -> Vec<PlexLibrarySectionInfo> {
    libraries
        .into_iter()
        .map(|(key, title)| PlexLibrarySectionInfo { key, title })
        .collect()
}

/// The policy plus its recipe verdict, recomputed on every read.
fn policy_view(policy: DownloadPolicy) -> DownloadPolicyView {
    let recipe_error = validate_quality_recipe(&policy.quality_recipe)
        .err()
        .map(|error| error.to_string());
    let (status, error) = quality::recipe_status(
        &policy.quality_recipe,
        policy.flac_mp3_only,
        recipe_error.is_none(),
        recipe_error.as_deref(),
    );
    DownloadPolicyView {
        policy,
        quality_recipe_status: status,
        quality_recipe_error: error,
        recycle_bin_warning: None,
    }
}

fn summary_inputs(policy: &DownloadPolicy) -> quality::SummaryInputs {
    quality::SummaryInputs {
        quality_preference_order: derive_default_order(&policy.quality_min, &policy.quality_max),
        quality_recipe: policy.quality_recipe.clone(),
        flac_mp3_only: policy.flac_mp3_only,
    }
}

/// Server-local timezone label for the daily-scan picker. Prefers the
/// configured IANA name (`TZ`), else the local abbreviation, else
/// "server time".
pub fn server_timezone(configured: Option<&str>, now_unix: i64) -> String {
    if let Some(tz) = configured {
        return tz.to_owned();
    }
    local_timezone_label(now_unix).unwrap_or_else(|| "server time".to_owned())
}

fn local_timezone_label(now_unix: i64) -> Option<String> {
    // libc localtime names the zone the same C library call v2's tzname
    // bottoms out in; only the abbreviation is used, never the offset.
    let now = libc::time_t::try_from(now_unix).ok()?;
    let mut broken = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: localtime_r writes only into the caller-owned `broken`
    // struct; a null return means the input was out of range.
    let filled = unsafe { libc::localtime_r(&now, broken.as_mut_ptr()) };
    if filled.is_null() {
        return None;
    }
    let zone = unsafe { (*filled).tm_zone };
    if zone.is_null() {
        return None;
    }
    // SAFETY: tm_zone points at static storage that localtime_r owns; it
    // is a NUL-terminated C string for the process lifetime.
    let name = unsafe { std::ffi::CStr::from_ptr(zone) }.to_str().ok()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}
