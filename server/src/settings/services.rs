//! Settings reads, writes, and connection checks over the config store.
//!
//! Sections cross the wire as their `runtime_config` types: plain
//! sections as they are, secret sections as [`Masked`] (secrets swapped
//! for their mask on read; a mask that comes back keeps the stored
//! secret). Reads never mutate the store. Saves run on the blocking pool
//! (the store writes and fsyncs the config file) and then fan out
//! through [`SaveEffects`] (provider-cache invalidation, events kick).
//!
//! Verify methods test the submitted values, not the stored config, so
//! Test works before the first save and reflects edits. A secret that
//! still holds its mask resolves to the stored one through
//! [`ConfigStore::unmask`], the same rule a save applies; an empty secret
//! fails the verdict without a network round trip.

use std::sync::Arc;

use super::effects::{SaveEffects, SavedSection};
use super::error::SettingsError;
use super::library_policy;
use super::models::{
    AdvancedSettingsForm, DownloadPolicyView, FrontendCacheTTLs, IndexerSavedResponse,
    IndexerTestResponse, JellyfinUserInfo, JellyfinVerifyResponse, LibraryScanScheduleResponse,
    LibrarySettingsResponse, LibrarySettingsSaveRequest, PlexLibrarySectionInfo,
    PlexVerifyResponse, PolicyImpactResponse, PolicySummaryResponse, ProwlarrTestResponse,
    SabnzbdTestResponse, SourcePriorityOrder, TestConnectionResponse, UsenetSearchBackend,
    VerifyConnectionResponse,
};
use super::quality;
use super::verify::{
    SKIDDLE_BASE_URL, TICKETMASTER_BASE_URL, VerifyProbes, YOUTUBE_BASE_URL, check_hibp_file,
    require_service_url,
};
use crate::ids::IdGenerator;
use crate::providers::listenbrainz::DEFAULT_BASE_URL as LISTENBRAINZ_BASE_URL;
use crate::runtime_config::secret_sections::{
    AdvancedSettings, DownloadClients, EventsSettings, IdentificationPolicy, JellyfinConnection,
    LibraryRoot, ListenBrainzConnection, NavidromeConnection, NewznabIndexer, OidcConnection,
    PlexConnection, ProwlarrConnection, SabnzbdConnection, SlskdConnection, TypedLibrary,
    YouTubeConnection,
};
use crate::runtime_config::sections::{
    DownloadPolicy, LibraryScanSchedule, PlainSection, SecuritySettings, SourcePriority,
    UsenetBackendSetting, derive_default_order, validate_quality_recipe,
};
use crate::runtime_config::{ConfigError, ConfigStore, Masked, SecretSection, Section};

/// Settings reads, writes, and verify probes. Built once at boot; every
/// call reads the store, so a save takes effect on the next call.
pub struct SettingsService {
    /// Config store.
    pub store: Arc<ConfigStore>,
    /// Post-save fan-out.
    pub effects: Arc<dyn SaveEffects>,
    /// Error-id mint.
    pub ids: Arc<dyn IdGenerator>,
    /// `TZ` from the deployment config, when set.
    pub timezone: Option<String>,
    /// Connection probes behind the verify methods.
    probes: Arc<dyn VerifyProbes>,
    /// Serializes library settings writes: the revision check and the
    /// write of one save happen with no other library save in between.
    library_writes: tokio::sync::Mutex<()>,
}

impl SettingsService {
    /// Build over the shared store, effects, ids, and probes.
    pub fn new(
        store: Arc<ConfigStore>,
        effects: Arc<dyn SaveEffects>,
        ids: Arc<dyn IdGenerator>,
        probes: Arc<dyn VerifyProbes>,
    ) -> Self {
        Self {
            store,
            effects,
            ids,
            timezone: None,
            probes,
            library_writes: tokio::sync::Mutex::new(()),
        }
    }

    /// Label scan schedules with this timezone name (`TZ`).
    #[must_use]
    pub fn with_timezone(mut self, timezone: Option<String>) -> Self {
        self.timezone = timezone;
        self
    }

    fn config(&self, error: ConfigError) -> SettingsError {
        SettingsError::from_config(error, self.ids.as_ref())
    }

    /// Run one store write on the blocking pool (it writes and fsyncs the
    /// config file).
    async fn write<T, F>(&self, op: F) -> Result<T, SettingsError>
    where
        T: Send + 'static,
        F: FnOnce(&ConfigStore) -> Result<T, ConfigError> + Send + 'static,
    {
        let store = self.store.clone();
        match tokio::task::spawn_blocking(move || op(&store)).await {
            Ok(result) => result.map_err(|error| self.config(error)),
            Err(cause) => Err(SettingsError::internal(
                &format!("settings write task failed: {cause}"),
                self.ids.as_ref(),
            )),
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
        let saved = self.write(move |store| store.save(section)).await?;
        self.effects.after_save(SavedSection::for_key(S::KEY)).await;
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
        let saved = self.write(move |store| store.save_secret(incoming)).await?;
        self.effects.after_save(SavedSection::for_key(S::KEY)).await;
        Ok(saved)
    }

    fn unmask<S: SecretSection>(&self, incoming: Masked<S>) -> Result<S, SettingsError> {
        self.store
            .unmask(incoming)
            .map_err(|error| self.config(error))
    }

    // --- sections with a wire shape of their own ----------------------------------

    /// The scan schedule plus the server timezone label.
    pub fn get_schedule(&self) -> Result<LibraryScanScheduleResponse, SettingsError> {
        Ok(LibraryScanScheduleResponse {
            schedule: self.get()?,
            server_timezone: server_timezone(self.timezone.as_deref()),
        })
    }

    /// Save the scan schedule.
    pub async fn save_schedule(
        &self,
        schedule: LibraryScanSchedule,
    ) -> Result<LibraryScanScheduleResponse, SettingsError> {
        Ok(LibraryScanScheduleResponse {
            schedule: self.save(schedule).await?,
            server_timezone: server_timezone(self.timezone.as_deref()),
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
        let mut clients = self.get_masked::<DownloadClients>()?.into_inner();
        clients.sabnzbd = incoming.into_inner();
        let saved = self.save_submitted(clients).await?;
        Ok(saved.map(|clients| clients.sabnzbd))
    }

    /// The advanced tunables in form units.
    pub fn get_advanced(&self) -> Result<AdvancedSettingsForm, SettingsError> {
        let stored: Masked<AdvancedSettings> = self.get_masked()?;
        Ok(AdvancedSettingsForm::from_section(&stored))
    }

    /// Save the advanced tunables (form units scale to stored units; a
    /// masked AudioDB key keeps the stored one). The save invalidates the
    /// AudioDB cache root.
    pub async fn save_advanced(
        &self,
        form: AdvancedSettingsForm,
    ) -> Result<AdvancedSettingsForm, SettingsError> {
        let saved = self.save_submitted(form.into_section()).await?;
        Ok(AdvancedSettingsForm::from_section(&saved))
    }

    /// The frontend cache TTLs in stored units (milliseconds).
    pub fn get_cache_ttls(&self) -> Result<FrontendCacheTTLs, SettingsError> {
        let stored: Masked<AdvancedSettings> = self.get_masked()?;
        Ok(FrontendCacheTTLs {
            home: stored.frontend_ttl_home,
            discover: stored.frontend_ttl_discover,
            library: stored.frontend_ttl_library,
            recently_added: stored.frontend_ttl_recently_added,
            discover_queue: stored.frontend_ttl_discover_queue,
            search: stored.frontend_ttl_search,
            local_files_sidebar: stored.frontend_ttl_local_files_sidebar,
            jellyfin_sidebar: stored.frontend_ttl_jellyfin_sidebar,
            plex_sidebar: stored.frontend_ttl_plex_sidebar,
            playlist_sources: stored.frontend_ttl_playlist_sources,
            discover_queue_polling_interval: stored.discover_queue_polling_interval,
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
        let ids = self.ids.clone();
        let task = tokio::task::spawn_blocking(move || {
            let stored = store
                .get_masked::<TypedLibrary>()
                .map_err(|error| SettingsError::from_config(error, ids.as_ref()))?;
            let resolved = stored.try_map(|library| library_policy::resolve(&library))?;
            Ok(library_policy::settings_response(resolved))
        });
        match task.await {
            Ok(result) => result,
            Err(cause) => Err(SettingsError::internal(
                &format!("library settings read failed: {cause}"),
                self.ids.as_ref(),
            )),
        }
    }

    /// Save the library settings. The expected revision must match the
    /// stored one or the save is a 409; a masked AcoustID key keeps the
    /// stored one. The check and the write run under the library write
    /// lock, so two saves holding the same revision cannot both land.
    /// The save invalidates the AcoustID cache root.
    pub async fn save_library(
        &self,
        request: LibrarySettingsSaveRequest,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let _writing = self.library_writes.lock().await;
        let stored = self.get_masked::<TypedLibrary>()?.into_inner();
        if request.expected_policy_revision != library_policy::revision(&stored) {
            return Err(SettingsError::StaleRevision {
                message: "Library settings changed since this page loaded. Refresh and retry."
                    .to_owned(),
            });
        }
        let resolved = library_policy::resolve(&request.settings)?;
        self.save_library_settings(resolved.settings).await
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
        let _writing = self.library_writes.lock().await;
        let mut library = self.get_masked::<TypedLibrary>()?.into_inner();
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
                id: self.ids.new_id(),
                path: candidate,
                label,
                policy: IdentificationPolicy::Automatic,
                rules: Vec::new(),
            });
        }
        self.save_library_roots(library).await
    }

    /// Remove every library root at one path. Unknown paths are a silent
    /// no-op.
    pub async fn remove_library_path(
        &self,
        path: &str,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let _writing = self.library_writes.lock().await;
        let mut library = self.get_masked::<TypedLibrary>()?.into_inner();
        library.library_roots.retain(|root| root.path != path);
        self.save_library_roots(library).await
    }

    async fn save_library_roots(
        &self,
        library: TypedLibrary,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let resolved = library_policy::resolve(&library)?;
        self.save_library_settings(resolved.settings).await
    }

    /// Save normalized library settings and answer with the masked view.
    async fn save_library_settings(
        &self,
        settings: TypedLibrary,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let saved = self.save_submitted(settings).await?;
        let resolved = saved.try_map(|library| library_policy::resolve(&library))?;
        Ok(library_policy::settings_response(resolved))
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

    /// A submitted recipe owns the legacy fields: the range, the order,
    /// the FLAC/MP3 switch, and (on save) the cutoff clamp.
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
        policy.quality_preference_order = quality::legacy_recipe_order(&policy.quality_recipe);
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
    /// composed contract sentence plus the source-mode label, no admin
    /// internals.
    pub fn policy_summary(&self) -> Result<PolicySummaryResponse, SettingsError> {
        let stored: DownloadPolicy = self.get()?;
        let inputs = summary_inputs(&stored);
        let summary = quality::compose_summary(&inputs);
        let order = if inputs.quality_recipe.is_empty() || !inputs.flac_mp3_only {
            if stored.quality_preference_order.is_empty() {
                derive_default_order(&stored.quality_min, &stored.quality_max)
            } else {
                stored.quality_preference_order.clone()
            }
        } else {
            Vec::new()
        };
        let legacy_shape = is_legacy_default_shape(&stored, &order);
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
            source_mode: stored.source_selection_mode.clone(),
            legacy_rollback_compatible: stored.quality_recipe.is_empty() && legacy_shape,
            quality_recipe_status: status,
            quality_recipe_error: error,
        })
    }

    /// Impact preview of an unsaved policy against persisted rows:
    /// persisted-state bucket counts only. Shares the strict validation
    /// with the save path.
    pub async fn policy_impact(
        &self,
        buckets: &dyn PolicyImpactBuckets,
        policy: DownloadPolicy,
    ) -> Result<PolicyImpactResponse, SettingsError> {
        let candidate = self.apply_recipe(policy, false)?;
        candidate.validate().map_err(|error| self.config(error))?;
        let order = if candidate.quality_preference_order.is_empty() {
            derive_default_order(&candidate.quality_min, &candidate.quality_max)
        } else {
            candidate.quality_preference_order.clone()
        };
        let legacy_representable = is_legacy_default_shape(&candidate, &order)
            && candidate.source_selection_mode == "source_first";
        let counts = buckets
            .counts()
            .await
            .map_err(|cause| SettingsError::internal(&cause, self.ids.as_ref()))?;
        Ok(PolicyImpactResponse {
            manual_search_jobs: counts.manual_search_jobs,
            queued_without_attempts: counts.queued_without_attempts,
            awaiting_review: counts.awaiting_review,
            remote_queued_zero_byte: counts.remote_queued_zero_byte,
            transferring_immutable: counts.transferring,
            held_reviews: counts.held_reviews,
            legacy_representable,
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
        if indexer.indexer_type.trim().is_empty() {
            indexer.indexer_type = "newznab".to_owned();
        }
        let id = self.write(move |store| store.save_indexer(indexer)).await?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(IndexerSavedResponse { id })
    }

    /// Delete one indexer. Unknown ids are a silent no-op.
    pub async fn delete_indexer(&self, indexer_id: &str) -> Result<(), SettingsError> {
        let indexer_id = indexer_id.to_owned();
        self.write(move |store| store.delete_indexer(&indexer_id))
            .await?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(())
    }

    /// Persist a dragged-card priority order (1-based on save).
    pub async fn reorder_indexers(&self, ordered_ids: Vec<String>) -> Result<(), SettingsError> {
        self.write(move |store| store.reorder_indexers(&ordered_ids))
            .await?;
        self.effects.after_save(SavedSection::Other).await;
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
        let stored: PlexConnection = self.store.get_raw().map_err(|error| self.config(error))?;
        let libraries = self
            .probes
            .plex_libraries(&stored.plex_url, stored.plex_token.expose())
            .await
            .map_err(|message| {
                if message == "Plex is not configured." {
                    SettingsError::InvalidInput { message }
                } else {
                    SettingsError::Upstream { message }
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
            .listenbrainz(
                LISTENBRAINZ_BASE_URL,
                &resolved.username,
                resolved.user_token.expose(),
            )
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
        let verdict = self
            .probes
            .youtube(YOUTUBE_BASE_URL, resolved.api_key.expose())
            .await;
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
            .ticketmaster(
                TICKETMASTER_BASE_URL,
                resolved.ticketmaster_api_key.expose(),
            )
            .await;
        Ok(verdict_response(verdict.valid, verdict.message))
    }

    /// Test the submitted Skiddle key.
    pub async fn verify_skiddle(
        &self,
        incoming: Masked<EventsSettings>,
    ) -> Result<VerifyConnectionResponse, SettingsError> {
        let resolved = self.unmask(incoming)?;
        let verdict = self
            .probes
            .skiddle(SKIDDLE_BASE_URL, resolved.skiddle_api_key.expose())
            .await;
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
                    SettingsError::internal(
                        &format!("hibp check task failed: {cause}"),
                        self.ids.as_ref(),
                    )
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
        let resolved = self.unmask(clients)?.sabnzbd;
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
    }
}

fn is_legacy_default_shape(policy: &DownloadPolicy, order: &[String]) -> bool {
    quality::is_legacy_default_shape(
        order,
        &policy.quality_min,
        &policy.quality_max,
        &policy.lossless_preference,
        policy.lossless_max_bit_depth,
        policy.lossless_max_sample_rate_hz,
        policy.preferred_lossy_bitrate_kbps,
        policy.lossy_min_bitrate_kbps,
        policy.lossy_max_bitrate_kbps,
        &policy.unknown_quality_behavior,
    )
}

fn summary_inputs(policy: &DownloadPolicy) -> quality::SummaryInputs {
    let mut order = policy.quality_preference_order.clone();
    if order.is_empty() && policy.quality_recipe.is_empty() {
        order = derive_default_order(&policy.quality_min, &policy.quality_max);
    }
    quality::SummaryInputs {
        quality_preference_order: order,
        quality_recipe: policy.quality_recipe.clone(),
        flac_mp3_only: policy.flac_mp3_only,
        lossless_preference: policy.lossless_preference.clone(),
        lossless_max_bit_depth: policy.lossless_max_bit_depth,
        lossless_max_sample_rate_hz: policy.lossless_max_sample_rate_hz,
        unknown_quality_behavior: policy.unknown_quality_behavior.clone(),
    }
}

/// Persisted-state bucket counts for the policy impact preview.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImpactBucketCounts {
    /// `searching` jobs with no task yet.
    pub manual_search_jobs: i64,
    /// Queued zero-byte rows with no attempts.
    pub queued_without_attempts: i64,
    /// Queued rows on completed jobs with no candidate picked.
    pub awaiting_review: i64,
    /// Remote-queued zero-byte rows.
    pub remote_queued_zero_byte: i64,
    /// Downloading/processing rows (immutable under the new policy).
    pub transferring: i64,
    /// Held imports awaiting a decision.
    pub held_reviews: i64,
}

/// Bucket-count port. The production implementation counts the download
/// tables; tests use a canned struct.
pub trait PolicyImpactBuckets: Send + Sync {
    /// Count the six persisted-state buckets.
    fn counts<'a>(
        &'a self,
    ) -> futures_util::future::BoxFuture<'a, Result<ImpactBucketCounts, String>>;
}

/// Production bucket counts over the download tables.
pub struct SqliteImpactBuckets {
    /// Reader pool.
    pub pool: sqlx::SqlitePool,
}

impl PolicyImpactBuckets for SqliteImpactBuckets {
    fn counts<'a>(
        &'a self,
    ) -> futures_util::future::BoxFuture<'a, Result<ImpactBucketCounts, String>> {
        Box::pin(async move {
            async fn one(pool: &sqlx::SqlitePool, query: &str) -> Result<i64, String> {
                sqlx::query_scalar::<_, i64>(query)
                    .fetch_one(pool)
                    .await
                    .map_err(|cause| cause.to_string())
            }
            Ok(ImpactBucketCounts {
                manual_search_jobs: one(
                    &self.pool,
                    "SELECT COUNT(*) FROM search_jobs sj WHERE sj.status = 'searching'
                     AND NOT EXISTS (SELECT 1 FROM download_tasks t WHERE t.search_job_id = sj.id)",
                )
                .await?,
                queued_without_attempts: one(
                    &self.pool,
                    "SELECT COUNT(*) FROM download_tasks t WHERE t.status = 'queued'
                     AND t.downloaded_bytes = 0
                     AND NOT EXISTS (SELECT 1 FROM download_attempts a WHERE a.task_id = t.id)",
                )
                .await?,
                awaiting_review: one(
                    &self.pool,
                    "SELECT COUNT(*) FROM download_tasks t JOIN search_jobs sj ON sj.id = t.search_job_id
                     WHERE t.status = 'queued' AND sj.status = 'completed' AND t.candidate_index IS NULL",
                )
                .await?,
                remote_queued_zero_byte: one(
                    &self.pool,
                    "SELECT COUNT(*) FROM download_tasks t
                     WHERE t.remote_queued = 1 AND t.downloaded_bytes = 0 AND t.status = 'queued'",
                )
                .await?,
                transferring: one(
                    &self.pool,
                    "SELECT COUNT(*) FROM download_tasks t WHERE t.status IN ('downloading','processing')",
                )
                .await?,
                held_reviews: one(
                    &self.pool,
                    "SELECT COUNT(*) FROM held_imports WHERE status = 'held'",
                )
                .await?,
            })
        })
    }
}

/// Server-local timezone label for the daily-scan picker. Prefers the
/// configured IANA name (`TZ`), else the local abbreviation, else
/// "server time".
pub fn server_timezone(configured: Option<&str>) -> String {
    if let Some(tz) = configured {
        return tz.to_owned();
    }
    local_timezone_label().unwrap_or_else(|| "server time".to_owned())
}

fn local_timezone_label() -> Option<String> {
    // libc localtime names the zone the same C library call v2's tzname
    // bottoms out in; only the abbreviation is used, never the offset.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as libc::time_t;
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
