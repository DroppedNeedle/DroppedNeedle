//! Per-section settings reads and writes over the config store.
//!
//! Every kept section round-trips here: GET decodes the stored section
//! into its wire DTO (secrets masked), PUT decodes the wire DTO back and
//! saves it (masks preserved, values validated). Reads never mutate the
//! store; the timezone label on the schedule GET is computed per request.
//! After a successful save the service fans out through [`SaveEffects`]
//! (provider-cache invalidation, events kick).

use std::sync::Arc;

use super::effects::{SaveEffects, SavedSection};
use super::error::SettingsError;
use super::models::{
    AdvancedSettingsDto, AudioFormatDto, ConnectAppsDto, DiscoverModeDto, DownloadAccessDto,
    EventsSweepScopeDto, FilesystemWatcherDto, FreeMusicDto, GetItDto, IdentificationPolicyDto,
    LastFmSettingsDto, LibraryPathRuleDto, LibraryRootDto, LibraryScanScheduleDto,
    LibraryScanScheduleResponse, LibrarySettingsDto, LibrarySettingsResponse,
    LibrarySettingsSaveRequest, MbSourceModeDto, MusicSourceDto, PrimaryMusicSourceDto,
    ScanFrequencyDto, ScrobbleSettingsDto, SecuritySettingsDto, SourcePriorityDto,
    UsenetBackendDto, UsenetSearchBackendDto, UserPreferencesDto, WantedWatcherDto,
};
use crate::ids::IdGenerator;
use crate::runtime_config::secret_sections::{EventsSweepScope, IdentificationPolicy};
use crate::runtime_config::sections::{
    AudioFormat, ConnectApps, DiscoverMode, DownloadAccess, FilesystemWatcher, FreeMusic, GetIt,
    LastFmSettings, LibraryScanSchedule, MbSourceMode, MusicSource, PrimaryMusicSource,
    ScanFrequency, ScrobbleSettings, SecuritySettings, SourcePriority, UsenetBackend,
    UsenetBackendSetting, UserPreferences, WantedWatcher,
};
use crate::runtime_config::{ConfigError, ConfigStore};

// --- enum bridges (variant-to-variant; wire strings stay v2-shaped) ----------

impl From<ScanFrequencyDto> for ScanFrequency {
    fn from(value: ScanFrequencyDto) -> Self {
        match value {
            ScanFrequencyDto::Manual => Self::Manual,
            ScanFrequencyDto::Min5 => Self::Min5,
            ScanFrequencyDto::Min10 => Self::Min10,
            ScanFrequencyDto::Min30 => Self::Min30,
            ScanFrequencyDto::Hr1 => Self::Hr1,
            ScanFrequencyDto::Hr6 => Self::Hr6,
            ScanFrequencyDto::Hr12 => Self::Hr12,
            ScanFrequencyDto::Hr24 => Self::Hr24,
            ScanFrequencyDto::Days3 => Self::Days3,
            ScanFrequencyDto::Days7 => Self::Days7,
            ScanFrequencyDto::Daily => Self::Daily,
        }
    }
}

impl From<ScanFrequency> for ScanFrequencyDto {
    fn from(value: ScanFrequency) -> Self {
        match value {
            ScanFrequency::Manual => Self::Manual,
            ScanFrequency::Min5 => Self::Min5,
            ScanFrequency::Min10 => Self::Min10,
            ScanFrequency::Min30 => Self::Min30,
            ScanFrequency::Hr1 => Self::Hr1,
            ScanFrequency::Hr6 => Self::Hr6,
            ScanFrequency::Hr12 => Self::Hr12,
            ScanFrequency::Hr24 => Self::Hr24,
            ScanFrequency::Days3 => Self::Days3,
            ScanFrequency::Days7 => Self::Days7,
            ScanFrequency::Daily => Self::Daily,
        }
    }
}

impl From<MusicSourceDto> for MusicSource {
    fn from(value: MusicSourceDto) -> Self {
        match value {
            MusicSourceDto::Listenbrainz => Self::Listenbrainz,
            MusicSourceDto::Lastfm => Self::Lastfm,
        }
    }
}

impl From<MusicSource> for MusicSourceDto {
    fn from(value: MusicSource) -> Self {
        match value {
            MusicSource::Listenbrainz => Self::Listenbrainz,
            MusicSource::Lastfm => Self::Lastfm,
        }
    }
}

impl From<AudioFormatDto> for AudioFormat {
    fn from(value: AudioFormatDto) -> Self {
        match value {
            AudioFormatDto::Flac => Self::Flac,
            AudioFormatDto::Mp3 => Self::Mp3,
            AudioFormatDto::Opus => Self::Opus,
        }
    }
}

impl From<AudioFormat> for AudioFormatDto {
    fn from(value: AudioFormat) -> Self {
        match value {
            AudioFormat::Flac => Self::Flac,
            AudioFormat::Mp3 => Self::Mp3,
            AudioFormat::Opus => Self::Opus,
        }
    }
}

impl From<DownloadAccessDto> for DownloadAccess {
    fn from(value: DownloadAccessDto) -> Self {
        match value {
            DownloadAccessDto::Everyone => Self::Everyone,
            DownloadAccessDto::Trusted => Self::Trusted,
            DownloadAccessDto::Admin => Self::Admin,
        }
    }
}

impl From<DownloadAccess> for DownloadAccessDto {
    fn from(value: DownloadAccess) -> Self {
        match value {
            DownloadAccess::Everyone => Self::Everyone,
            DownloadAccess::Trusted => Self::Trusted,
            DownloadAccess::Admin => Self::Admin,
        }
    }
}

impl From<DiscoverModeDto> for DiscoverMode {
    fn from(value: DiscoverModeDto) -> Self {
        match value {
            DiscoverModeDto::LocalOnly => Self::LocalOnly,
            DiscoverModeDto::LazyMb => Self::LazyMb,
            DiscoverModeDto::UseScrobbleTargets => Self::UseScrobbleTargets,
        }
    }
}

impl From<DiscoverMode> for DiscoverModeDto {
    fn from(value: DiscoverMode) -> Self {
        match value {
            DiscoverMode::LocalOnly => Self::LocalOnly,
            DiscoverMode::LazyMb => Self::LazyMb,
            DiscoverMode::UseScrobbleTargets => Self::UseScrobbleTargets,
        }
    }
}

impl From<UsenetBackendDto> for UsenetBackend {
    fn from(value: UsenetBackendDto) -> Self {
        match value {
            UsenetBackendDto::Indexers => Self::Indexers,
            UsenetBackendDto::Prowlarr => Self::Prowlarr,
        }
    }
}

impl From<UsenetBackend> for UsenetBackendDto {
    fn from(value: UsenetBackend) -> Self {
        match value {
            UsenetBackend::Indexers => Self::Indexers,
            UsenetBackend::Prowlarr => Self::Prowlarr,
        }
    }
}

impl From<EventsSweepScopeDto> for EventsSweepScope {
    fn from(value: EventsSweepScopeDto) -> Self {
        match value {
            EventsSweepScopeDto::Followed => Self::Followed,
            EventsSweepScopeDto::Library => Self::Library,
        }
    }
}

impl From<EventsSweepScope> for EventsSweepScopeDto {
    fn from(value: EventsSweepScope) -> Self {
        match value {
            EventsSweepScope::Followed => Self::Followed,
            EventsSweepScope::Library => Self::Library,
        }
    }
}

impl From<MbSourceModeDto> for MbSourceMode {
    fn from(value: MbSourceModeDto) -> Self {
        match value {
            MbSourceModeDto::Official => Self::Official,
            MbSourceModeDto::Mirror => Self::Mirror,
            MbSourceModeDto::Community => Self::Community,
            MbSourceModeDto::Brainzmash => Self::Brainzmash,
        }
    }
}

impl From<MbSourceMode> for MbSourceModeDto {
    fn from(value: MbSourceMode) -> Self {
        match value {
            MbSourceMode::Official => Self::Official,
            MbSourceMode::Mirror => Self::Mirror,
            MbSourceMode::Community => Self::Community,
            MbSourceMode::Brainzmash => Self::Brainzmash,
        }
    }
}

impl From<IdentificationPolicyDto> for IdentificationPolicy {
    fn from(value: IdentificationPolicyDto) -> Self {
        match value {
            IdentificationPolicyDto::LocalMetadata => Self::LocalMetadata,
            IdentificationPolicyDto::Automatic => Self::Automatic,
            IdentificationPolicyDto::Excluded => Self::Excluded,
        }
    }
}

impl From<IdentificationPolicy> for IdentificationPolicyDto {
    fn from(value: IdentificationPolicy) -> Self {
        match value {
            IdentificationPolicy::LocalMetadata => Self::LocalMetadata,
            IdentificationPolicy::Automatic => Self::Automatic,
            IdentificationPolicy::Excluded => Self::Excluded,
        }
    }
}

// --- service -----------------------------------------------------------------

/// Per-section reads and writes. Holds the store, the post-save fan-out,
/// and the id generator for error ids.
pub struct SettingsService {
    /// Config store.
    pub store: Arc<ConfigStore>,
    /// Post-save fan-out.
    pub effects: Arc<dyn SaveEffects>,
    /// Error-id mint.
    pub ids: Arc<dyn IdGenerator>,
}

impl SettingsService {
    /// Build over the shared store, effects, and ids.
    pub fn new(
        store: Arc<ConfigStore>,
        effects: Arc<dyn SaveEffects>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            store,
            effects,
            ids,
        }
    }

    fn config(&self, error: ConfigError) -> SettingsError {
        SettingsError::from_config(error, self.ids.as_ref())
    }

    // --- user_preferences ----------------------------------------------------

    /// Read the release-type filters.
    pub fn get_preferences(&self) -> Result<UserPreferencesDto, SettingsError> {
        let stored: UserPreferences = self.store.get().map_err(|e| self.config(e))?;
        Ok(UserPreferencesDto {
            primary_types: stored.primary_types,
            secondary_types: stored.secondary_types,
        })
    }

    /// Save the release-type filters.
    pub async fn save_preferences(
        &self,
        dto: &UserPreferencesDto,
    ) -> Result<UserPreferencesDto, SettingsError> {
        let saved: UserPreferences = self
            .store
            .save(UserPreferences {
                primary_types: dto.primary_types.clone(),
                secondary_types: dto.secondary_types.clone(),
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(UserPreferencesDto {
            primary_types: saved.primary_types,
            secondary_types: saved.secondary_types,
        })
    }

    // --- library_scan_schedule -------------------------------------------------

    /// Read the scan schedule plus the server timezone label.
    pub fn get_schedule(&self) -> Result<LibraryScanScheduleResponse, SettingsError> {
        let stored: LibraryScanSchedule = self.store.get().map_err(|e| self.config(e))?;
        Ok(LibraryScanScheduleResponse {
            scan_frequency: stored.scan_frequency.into(),
            daily_scan_time: stored.daily_scan_time,
            last_scan: stored.last_scan,
            last_scan_success: stored.last_scan_success,
            server_timezone: server_timezone(),
        })
    }

    /// Save the scan schedule.
    pub async fn save_schedule(
        &self,
        dto: &LibraryScanScheduleDto,
    ) -> Result<LibraryScanScheduleResponse, SettingsError> {
        let saved: LibraryScanSchedule = self
            .store
            .save(LibraryScanSchedule {
                scan_frequency: dto.scan_frequency.into(),
                daily_scan_time: dto.daily_scan_time.clone(),
                last_scan: dto.last_scan,
                last_scan_success: dto.last_scan_success,
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(LibraryScanScheduleResponse {
            scan_frequency: saved.scan_frequency.into(),
            daily_scan_time: saved.daily_scan_time,
            last_scan: saved.last_scan,
            last_scan_success: saved.last_scan_success,
            server_timezone: server_timezone(),
        })
    }

    // --- library_scan_filesystem_watcher ----------------------------------------

    /// Read the filesystem poller knobs.
    pub fn get_watcher(&self) -> Result<FilesystemWatcherDto, SettingsError> {
        let stored: FilesystemWatcher = self.store.get().map_err(|e| self.config(e))?;
        Ok(FilesystemWatcherDto {
            enabled: stored.enabled,
            poll_interval_seconds: stored.poll_interval_seconds,
            batch_window_seconds: stored.batch_window_seconds,
        })
    }

    /// Save the filesystem poller knobs.
    pub async fn save_watcher(
        &self,
        dto: &FilesystemWatcherDto,
    ) -> Result<FilesystemWatcherDto, SettingsError> {
        let saved: FilesystemWatcher = self
            .store
            .save(FilesystemWatcher {
                enabled: dto.enabled,
                poll_interval_seconds: dto.poll_interval_seconds,
                batch_window_seconds: dto.batch_window_seconds,
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(FilesystemWatcherDto {
            enabled: saved.enabled,
            poll_interval_seconds: saved.poll_interval_seconds,
            batch_window_seconds: saved.batch_window_seconds,
        })
    }

    // --- wanted -------------------------------------------------------------------

    /// Read the wanted watcher toggles.
    pub fn get_wanted(&self) -> Result<WantedWatcherDto, SettingsError> {
        let stored: WantedWatcher = self.store.get().map_err(|e| self.config(e))?;
        Ok(WantedWatcherDto {
            enabled: stored.enabled,
            auto_download_on_find: stored.auto_download_on_find,
            watch_partial_albums: stored.watch_partial_albums,
            max_checks_per_sweep: stored.max_checks_per_sweep,
            dormant_after_days: stored.dormant_after_days,
        })
    }

    /// Save the wanted watcher toggles.
    pub async fn save_wanted(
        &self,
        dto: &WantedWatcherDto,
    ) -> Result<WantedWatcherDto, SettingsError> {
        let saved: WantedWatcher = self
            .store
            .save(WantedWatcher {
                enabled: dto.enabled,
                auto_download_on_find: dto.auto_download_on_find,
                watch_partial_albums: dto.watch_partial_albums,
                max_checks_per_sweep: dto.max_checks_per_sweep,
                dormant_after_days: dto.dormant_after_days,
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(WantedWatcherDto {
            enabled: saved.enabled,
            auto_download_on_find: saved.auto_download_on_find,
            watch_partial_albums: saved.watch_partial_albums,
            max_checks_per_sweep: saved.max_checks_per_sweep,
            dormant_after_days: saved.dormant_after_days,
        })
    }

    // --- source_priority ----------------------------------------------------------

    /// Read the acquisition source try-order.
    pub fn get_source_priority(&self) -> Result<SourcePriorityDto, SettingsError> {
        let stored: SourcePriority = self.store.get().map_err(|e| self.config(e))?;
        Ok(SourcePriorityDto { order: stored.0 })
    }

    /// Save the acquisition source try-order. The store cleans the order
    /// (bundled sources present, well-formed plugin keys kept, the rest
    /// dropped) and echoes the cleaned order back.
    pub async fn save_source_priority(
        &self,
        dto: &SourcePriorityDto,
    ) -> Result<SourcePriorityDto, SettingsError> {
        let saved: SourcePriority = self
            .store
            .save(SourcePriority(dto.order.clone()))
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(SourcePriorityDto { order: saved.0 })
    }

    // --- usenet_search_backend ------------------------------------------------------

    /// Read the active Usenet search backend.
    pub fn get_usenet_backend(&self) -> Result<UsenetSearchBackendDto, SettingsError> {
        let stored: UsenetBackendSetting = self.store.get().map_err(|e| self.config(e))?;
        Ok(UsenetSearchBackendDto {
            backend: stored.0.into(),
        })
    }

    /// Save the active Usenet search backend. The closed enum decodes at
    /// the boundary, so unknown values are a 400, never a silent reset.
    pub async fn save_usenet_backend(
        &self,
        dto: &UsenetSearchBackendDto,
    ) -> Result<UsenetSearchBackendDto, SettingsError> {
        let saved: UsenetBackendSetting = self
            .store
            .save(UsenetBackendSetting(dto.backend.into()))
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(UsenetSearchBackendDto {
            backend: saved.0.into(),
        })
    }

    // --- scrobble_settings ------------------------------------------------------------

    /// Read the scrobble targets.
    pub fn get_scrobble(&self) -> Result<ScrobbleSettingsDto, SettingsError> {
        let stored: ScrobbleSettings = self.store.get().map_err(|e| self.config(e))?;
        Ok(ScrobbleSettingsDto {
            scrobble_to_lastfm: stored.scrobble_to_lastfm,
            scrobble_to_listenbrainz: stored.scrobble_to_listenbrainz,
        })
    }

    /// Save the scrobble targets.
    pub async fn save_scrobble(
        &self,
        dto: &ScrobbleSettingsDto,
    ) -> Result<ScrobbleSettingsDto, SettingsError> {
        let saved: ScrobbleSettings = self
            .store
            .save(ScrobbleSettings {
                scrobble_to_lastfm: dto.scrobble_to_lastfm,
                scrobble_to_listenbrainz: dto.scrobble_to_listenbrainz,
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(ScrobbleSettingsDto {
            scrobble_to_lastfm: saved.scrobble_to_lastfm,
            scrobble_to_listenbrainz: saved.scrobble_to_listenbrainz,
        })
    }

    // --- primary_music_source -----------------------------------------------------------

    /// Read the primary music source.
    pub fn get_primary_source(&self) -> Result<PrimaryMusicSourceDto, SettingsError> {
        let stored: PrimaryMusicSource = self.store.get().map_err(|e| self.config(e))?;
        Ok(PrimaryMusicSourceDto {
            source: stored.source.into(),
        })
    }

    /// Save the primary music source.
    pub async fn save_primary_source(
        &self,
        dto: &PrimaryMusicSourceDto,
    ) -> Result<PrimaryMusicSourceDto, SettingsError> {
        let saved: PrimaryMusicSource = self
            .store
            .save(PrimaryMusicSource {
                source: dto.source.into(),
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(PrimaryMusicSourceDto {
            source: saved.source.into(),
        })
    }

    // --- free_music ---------------------------------------------------------------------

    /// Read the free-music settings.
    pub fn get_free_music(&self) -> Result<FreeMusicDto, SettingsError> {
        let stored: FreeMusic = self.store.get().map_err(|e| self.config(e))?;
        Ok(FreeMusicDto {
            enabled: stored.enabled,
            preferred_format: stored.preferred_format.into(),
        })
    }

    /// Save the free-music settings.
    pub async fn save_free_music(&self, dto: &FreeMusicDto) -> Result<FreeMusicDto, SettingsError> {
        let saved: FreeMusic = self
            .store
            .save(FreeMusic {
                enabled: dto.enabled,
                preferred_format: dto.preferred_format.into(),
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(FreeMusicDto {
            enabled: saved.enabled,
            preferred_format: saved.preferred_format.into(),
        })
    }

    // --- get_it -------------------------------------------------------------------------

    /// Read the store-region settings.
    pub fn get_get_it(&self) -> Result<GetItDto, SettingsError> {
        let stored: GetIt = self.store.get().map_err(|e| self.config(e))?;
        Ok(GetItDto {
            store_region: stored.store_region,
        })
    }

    /// Save the store-region settings.
    pub async fn save_get_it(&self, dto: &GetItDto) -> Result<GetItDto, SettingsError> {
        let saved: GetIt = self
            .store
            .save(GetIt {
                store_region: dto.store_region.clone(),
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(GetItDto {
            store_region: saved.store_region,
        })
    }

    // --- security_settings ----------------------------------------------------------------

    /// Read the security posture settings.
    pub fn get_security(&self) -> Result<SecuritySettingsDto, SettingsError> {
        let stored: SecuritySettings = self.store.get().map_err(|e| self.config(e))?;
        Ok(SecuritySettingsDto {
            hibp_check: stored.hibp_check,
            hibp_local_path: stored.hibp_local_path,
            hsts_max_age: stored.hsts_max_age,
            hsts_include_subdomains: stored.hsts_include_subdomains,
            hsts_preload: stored.hsts_preload,
            library_download_access: stored.library_download_access.into(),
        })
    }

    /// Save the security posture settings.
    pub async fn save_security(
        &self,
        dto: &SecuritySettingsDto,
    ) -> Result<SecuritySettingsDto, SettingsError> {
        let saved: SecuritySettings = self
            .store
            .save(SecuritySettings {
                hibp_check: dto.hibp_check,
                hibp_local_path: dto.hibp_local_path.clone(),
                hsts_max_age: dto.hsts_max_age,
                hsts_include_subdomains: dto.hsts_include_subdomains,
                hsts_preload: dto.hsts_preload,
                library_download_access: dto.library_download_access.into(),
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(SecuritySettingsDto {
            hibp_check: saved.hibp_check,
            hibp_local_path: saved.hibp_local_path,
            hsts_max_age: saved.hsts_max_age,
            hsts_include_subdomains: saved.hsts_include_subdomains,
            hsts_preload: saved.hsts_preload,
            library_download_access: saved.library_download_access.into(),
        })
    }

    // --- connect_apps ---------------------------------------------------------------------

    /// Read the inbound Connect Apps config.
    pub fn get_connect_apps(&self) -> Result<ConnectAppsDto, SettingsError> {
        let stored: ConnectApps = self.store.get().map_err(|e| self.config(e))?;
        Ok(ConnectAppsDto {
            subsonic_enabled: stored.subsonic_enabled,
            jellyfin_enabled: stored.jellyfin_enabled,
            exact_track_approval_supported: stored.exact_track_approval_supported,
            transcoding_enabled: stored.transcoding_enabled,
            transcode_default_format: stored.transcode_default_format.into(),
            transcode_max_bitrate_kbps: stored.transcode_max_bitrate_kbps,
            advertise_server_name: stored.advertise_server_name,
            advertise_server_version: stored.advertise_server_version,
            discover_mode: stored.discover_mode.into(),
        })
    }

    /// Save the inbound Connect Apps config.
    pub async fn save_connect_apps(
        &self,
        dto: &ConnectAppsDto,
    ) -> Result<ConnectAppsDto, SettingsError> {
        let saved: ConnectApps = self
            .store
            .save(ConnectApps {
                subsonic_enabled: dto.subsonic_enabled,
                jellyfin_enabled: dto.jellyfin_enabled,
                exact_track_approval_supported: dto.exact_track_approval_supported,
                transcoding_enabled: dto.transcoding_enabled,
                transcode_default_format: dto.transcode_default_format.into(),
                transcode_max_bitrate_kbps: dto.transcode_max_bitrate_kbps,
                advertise_server_name: dto.advertise_server_name.clone(),
                advertise_server_version: dto.advertise_server_version.clone(),
                discover_mode: dto.discover_mode.into(),
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(ConnectAppsDto {
            subsonic_enabled: saved.subsonic_enabled,
            jellyfin_enabled: saved.jellyfin_enabled,
            exact_track_approval_supported: saved.exact_track_approval_supported,
            transcoding_enabled: saved.transcoding_enabled,
            transcode_default_format: saved.transcode_default_format.into(),
            transcode_max_bitrate_kbps: saved.transcode_max_bitrate_kbps,
            advertise_server_name: saved.advertise_server_name,
            advertise_server_version: saved.advertise_server_version,
            discover_mode: saved.discover_mode.into(),
        })
    }

    // --- lastfm_settings (switch only) -------------------------------------------------------

    /// Read the Last.fm master switch.
    pub fn get_lastfm(&self) -> Result<LastFmSettingsDto, SettingsError> {
        let stored: LastFmSettings = self.store.get().map_err(|e| self.config(e))?;
        Ok(LastFmSettingsDto {
            enabled: stored.enabled,
        })
    }

    /// Save the Last.fm master switch.
    pub async fn save_lastfm(
        &self,
        dto: &LastFmSettingsDto,
    ) -> Result<LastFmSettingsDto, SettingsError> {
        let saved: LastFmSettings = self
            .store
            .save(LastFmSettings {
                enabled: dto.enabled,
            })
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(LastFmSettingsDto {
            enabled: saved.enabled,
        })
    }

    // --- download_client (slskd) ----------------------------------------------------------

    /// Read the slskd connection (key masked unless unset).
    pub fn get_slskd(&self) -> Result<super::models::SlskdConnectionDto, SettingsError> {
        let stored: SlskdConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the slskd connection (a masked key keeps the stored one).
    pub async fn save_slskd(
        &self,
        dto: &super::models::SlskdConnectionDto,
    ) -> Result<super::models::SlskdConnectionDto, SettingsError> {
        let _: SlskdConnection = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: SlskdConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the slskd connection with the key DECRYPTED (verify probes
    /// only; never leaves the server).
    pub fn get_slskd_raw(&self) -> Result<SlskdConnection, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- download_clients.sabnzbd ------------------------------------------------------------

    /// Read the SABnzbd connection (key masked unless unset).
    pub fn get_sabnzbd(&self) -> Result<super::models::SabnzbdConnectionDto, SettingsError> {
        let stored: DownloadClients = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.sabnzbd.into())
    }

    /// Save the SABnzbd connection (a masked key keeps the stored one).
    pub async fn save_sabnzbd(
        &self,
        dto: &super::models::SabnzbdConnectionDto,
    ) -> Result<super::models::SabnzbdConnectionDto, SettingsError> {
        let mut current: DownloadClients = self.store.get_raw().map_err(|e| self.config(e))?;
        current.sabnzbd = dto.clone().into();
        let _: DownloadClients = self
            .store
            .save_secret(current)
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: DownloadClients = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.sabnzbd.into())
    }

    /// Read the SABnzbd connection with the key DECRYPTED (verify probes
    /// only; never leaves the server).
    pub fn get_sabnzbd_raw(&self) -> Result<SabnzbdConnection, SettingsError> {
        let stored: DownloadClients = self.store.get_raw().map_err(|e| self.config(e))?;
        Ok(stored.sabnzbd)
    }

    // --- prowlarr --------------------------------------------------------------------------------

    /// Read the Prowlarr connection (key masked unless unset).
    pub fn get_prowlarr(&self) -> Result<super::models::ProwlarrConnectionDto, SettingsError> {
        let stored: ProwlarrConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the Prowlarr connection (a masked key keeps the stored one).
    pub async fn save_prowlarr(
        &self,
        dto: &super::models::ProwlarrConnectionDto,
    ) -> Result<super::models::ProwlarrConnectionDto, SettingsError> {
        let _: ProwlarrConnection = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: ProwlarrConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the Prowlarr connection with the key DECRYPTED (verify probes
    /// only; never leaves the server).
    pub fn get_prowlarr_raw(&self) -> Result<ProwlarrConnection, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- jellyfin_settings ----------------------------------------------------------------------------

    /// Read the Jellyfin connection (key masked unless unset).
    pub fn get_jellyfin(&self) -> Result<super::models::JellyfinConnectionDto, SettingsError> {
        let stored: JellyfinConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the Jellyfin connection (a masked key keeps the stored one).
    pub async fn save_jellyfin(
        &self,
        dto: &super::models::JellyfinConnectionDto,
    ) -> Result<super::models::JellyfinConnectionDto, SettingsError> {
        let _: JellyfinConnection = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: JellyfinConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the Jellyfin connection with the key DECRYPTED (verify probes
    /// only; never leaves the server).
    pub fn get_jellyfin_raw(&self) -> Result<JellyfinConnection, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- navidrome_settings ------------------------------------------------------------------------------

    /// Read the Navidrome connection (password masked unless unset).
    pub fn get_navidrome(&self) -> Result<super::models::NavidromeConnectionDto, SettingsError> {
        let stored: NavidromeConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the Navidrome connection (a masked password keeps the stored one).
    pub async fn save_navidrome(
        &self,
        dto: &super::models::NavidromeConnectionDto,
    ) -> Result<super::models::NavidromeConnectionDto, SettingsError> {
        let _: NavidromeConnection = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: NavidromeConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the Navidrome connection with the password DECRYPTED (verify
    /// probes only; never leaves the server).
    pub fn get_navidrome_raw(&self) -> Result<NavidromeConnection, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- plex_settings -----------------------------------------------------------------------------------

    /// Read the Plex connection (token masked unless unset).
    pub fn get_plex(&self) -> Result<super::models::PlexConnectionDto, SettingsError> {
        let stored: PlexConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the Plex connection (a masked token keeps the stored one).
    pub async fn save_plex(
        &self,
        dto: &super::models::PlexConnectionDto,
    ) -> Result<super::models::PlexConnectionDto, SettingsError> {
        let _: PlexConnection = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: PlexConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the Plex connection with the token DECRYPTED (verify probes
    /// only; never leaves the server).
    pub fn get_plex_raw(&self) -> Result<PlexConnection, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- listenbrainz_settings --------------------------------------------------------------------------------

    /// Read the ListenBrainz connection (token masked unless unset).
    pub fn get_listenbrainz(
        &self,
    ) -> Result<super::models::ListenBrainzConnectionDto, SettingsError> {
        let stored: ListenBrainzConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the ListenBrainz connection (a masked token keeps the stored one).
    pub async fn save_listenbrainz(
        &self,
        dto: &super::models::ListenBrainzConnectionDto,
    ) -> Result<super::models::ListenBrainzConnectionDto, SettingsError> {
        let _: ListenBrainzConnection = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::ListenBrainz).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: ListenBrainzConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the ListenBrainz connection with the token DECRYPTED (verify
    /// probes only; never leaves the server).
    pub fn get_listenbrainz_raw(&self) -> Result<ListenBrainzConnection, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- youtube_settings -------------------------------------------------------------------------------------

    /// Read the YouTube connection (key masked unless unset).
    pub fn get_youtube(&self) -> Result<super::models::YouTubeConnectionDto, SettingsError> {
        let stored: YouTubeConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the YouTube connection (a masked key keeps the stored one).
    pub async fn save_youtube(
        &self,
        dto: &super::models::YouTubeConnectionDto,
    ) -> Result<super::models::YouTubeConnectionDto, SettingsError> {
        let _: YouTubeConnection = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: YouTubeConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the YouTube connection with the key DECRYPTED (verify probes
    /// only; never leaves the server).
    pub fn get_youtube_raw(&self) -> Result<YouTubeConnection, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- events -------------------------------------------------------------------------------------------------

    /// Read the events sources (keys masked unless unset).
    pub fn get_events(&self) -> Result<super::models::EventsSettingsDto, SettingsError> {
        let stored: EventsSettings = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the events sources (masked keys keep the stored ones) and kick
    /// the sweep.
    pub async fn save_events(
        &self,
        dto: &super::models::EventsSettingsDto,
    ) -> Result<super::models::EventsSettingsDto, SettingsError> {
        let _: EventsSettings = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Events).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: EventsSettings = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the events sources with keys DECRYPTED (test probes only;
    /// never leaves the server).
    pub fn get_events_raw(&self) -> Result<EventsSettings, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- wrapped_settings --------------------------------------------------------------------------------------------

    /// Read the wrapped settings (key masked unless unset).
    pub fn get_wrapped(&self) -> Result<super::models::WrappedSettingsDto, SettingsError> {
        let stored: WrappedSettings = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the wrapped settings (a masked key keeps the stored one).
    pub async fn save_wrapped(
        &self,
        dto: &super::models::WrappedSettingsDto,
    ) -> Result<super::models::WrappedSettingsDto, SettingsError> {
        let _: WrappedSettings = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: WrappedSettings = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the wrapped settings with the key DECRYPTED (verify probes
    /// only; never leaves the server).
    pub fn get_wrapped_raw(&self) -> Result<WrappedSettings, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- oidc_settings -----------------------------------------------------------------------------------------------

    /// Read the OIDC connection (secret masked unless unset).
    pub fn get_oidc(&self) -> Result<super::models::OidcConnectionDto, SettingsError> {
        let stored: OidcConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(stored.into())
    }

    /// Save the OIDC connection (a masked secret keeps the stored one).
    pub async fn save_oidc(
        &self,
        dto: &super::models::OidcConnectionDto,
    ) -> Result<super::models::OidcConnectionDto, SettingsError> {
        let _: OidcConnection = self
            .store
            .save_secret(dto.clone().into())
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: OidcConnection = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(masked.into())
    }

    /// Read the OIDC connection with the secret DECRYPTED (verify probes
    /// only; never leaves the server).
    pub fn get_oidc_raw(&self) -> Result<OidcConnection, SettingsError> {
        self.store.get_raw().map_err(|e| self.config(e))
    }

    // --- advanced_settings -----------------------------------------------------------------------------

    /// Read the advanced tunables in frontend units (AudioDB key masked
    /// unless unset). Backend seconds/milliseconds floor back to the
    /// human units the DTO documents.
    pub fn get_advanced(&self) -> Result<AdvancedSettingsDto, SettingsError> {
        let stored: AdvancedSettings = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(advanced_to_dto(&stored))
    }

    /// Save the advanced tunables. Frontend units scale to backend units
    /// (a masked AudioDB key keeps the stored one) and the save
    /// invalidates the AudioDB cache root.
    pub async fn save_advanced(
        &self,
        dto: &AdvancedSettingsDto,
    ) -> Result<AdvancedSettingsDto, SettingsError> {
        let _: AdvancedSettings = self
            .store
            .save_secret(dto_to_advanced(dto))
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Advanced).await;
        // The save return holds ciphertext; the echo re-reads masked so a
        // client that re-saves the echo keeps the secret, not encrypts it.
        let masked: AdvancedSettings = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(advanced_to_dto(&masked))
    }

    /// Read the frontend cache TTLs in backend units (milliseconds),
    /// verbatim from the stored advanced section.
    pub fn get_cache_ttls(&self) -> Result<super::models::FrontendCacheTTLs, SettingsError> {
        let stored: AdvancedSettings = self.store.get_masked().map_err(|e| self.config(e))?;
        Ok(super::models::FrontendCacheTTLs {
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

    // --- library_settings -----------------------------------------------------------------------------

    /// Read the typed library settings: normalized roots plus the policy
    /// revision, the reconciliation projection, and warnings. The
    /// AcoustID key arrives masked unless unset.
    pub fn get_library(&self) -> Result<LibrarySettingsResponse, SettingsError> {
        let stored: TypedLibrary = self.store.get_masked().map_err(|e| self.config(e))?;
        let dto = library_to_dto(&stored);
        let resolved = super::library_policy::resolve(&dto)?;
        Ok(super::library_policy::settings_response(
            &resolved,
            dto.acoustid_api_key,
        ))
    }

    /// Save the typed library settings. The expected revision must match
    /// the stored one or the save is a 409; a masked AcoustID key keeps
    /// the stored one. The save invalidates the AcoustID cache root.
    pub async fn save_library(
        &self,
        request: &LibrarySettingsSaveRequest,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let stored: TypedLibrary = self.store.get_masked().map_err(|e| self.config(e))?;
        let current_revision = super::library_policy::revision(&library_to_dto(&stored));
        if request.expected_policy_revision != current_revision {
            return Err(SettingsError::StaleRevision {
                message: "Library settings changed since this page loaded. Refresh and retry."
                    .to_owned(),
            });
        }
        let resolved = super::library_policy::resolve(&request.settings)?;
        let candidate = dto_to_library(&resolved.settings);
        let _: TypedLibrary = self
            .store
            .save_secret(candidate)
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Library).await;
        let reread: TypedLibrary = self.store.get_masked().map_err(|e| self.config(e))?;
        let dto = library_to_dto(&reread);
        let resolved = super::library_policy::resolve(&dto)?;
        Ok(super::library_policy::settings_response(
            &resolved,
            dto.acoustid_api_key,
        ))
    }

    /// Add one library root path. The path must be a directory on this
    /// machine; adding a path that is already a root is a silent no-op.
    /// Returns the full GET view.
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
        let stored: TypedLibrary = self.store.get_raw().map_err(|e| self.config(e))?;
        let mut dto = library_to_dto(&stored);
        // The raw read decrypted the key; re-mask it so the save below
        // resolves it back to the stored ciphertext.
        dto.acoustid_api_key = masked_acoustid(&stored);
        if !dto.library_roots.iter().any(|root| root.path == candidate) {
            let label = std::path::Path::new(&candidate)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or(&candidate)
                .to_owned();
            dto.library_roots.push(LibraryRootDto {
                id: self.ids.new_id(),
                path: candidate,
                label,
                policy: IdentificationPolicyDto::Automatic,
                rules: Vec::new(),
            });
        }
        let resolved = super::library_policy::resolve(&dto)?;
        let _: TypedLibrary = self
            .store
            .save_secret(dto_to_library(&resolved.settings))
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Library).await;
        self.get_library()
    }

    /// Remove every library root at one path. Unknown paths are a silent
    /// no-op. Returns the full GET view.
    pub async fn remove_library_path(
        &self,
        path: &str,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let stored: TypedLibrary = self.store.get_raw().map_err(|e| self.config(e))?;
        let mut dto = library_to_dto(&stored);
        // The raw read decrypted the key; re-mask it so the save below
        // resolves it back to the stored ciphertext.
        dto.acoustid_api_key = masked_acoustid(&stored);
        dto.library_roots.retain(|root| root.path != path);
        let resolved = super::library_policy::resolve(&dto)?;
        let _: TypedLibrary = self
            .store
            .save_secret(dto_to_library(&resolved.settings))
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Library).await;
        self.get_library()
    }

    // --- download_policy ----------------------------------------------------------------------------

    /// Read the acquisition policy. The read-only recipe verdict is
    /// recomputed on every read, never persisted. A corrupt section
    /// degrades to the default policy with an `invalid` verdict instead
    /// of failing the read.
    pub fn get_policy(&self) -> Result<super::models::DownloadPolicyDto, SettingsError> {
        let stored: DownloadPolicy = match self.store.get() {
            Ok(policy) => policy,
            Err(ConfigError::SectionDecode { reason, .. }) => {
                return Ok(super::models::DownloadPolicyDto {
                    quality_recipe_status: "invalid".to_owned(),
                    quality_recipe_error: Some(reason),
                    ..super::models::DownloadPolicyDto::default()
                });
            }
            Err(other) => return Err(self.config(other)),
        };
        Ok(policy_to_dto(&stored))
    }

    /// Save the acquisition policy. A submitted recipe derives the legacy
    /// range, the legacy order, and the cutoff clamp (v2 save semantics);
    /// the read-only verdict fields are ignored and recomputed on the
    /// echo.
    pub async fn save_policy(
        &self,
        dto: &super::models::DownloadPolicyDto,
    ) -> Result<super::models::DownloadPolicyDto, SettingsError> {
        let mut policy = dto_to_policy(dto);
        if !policy.quality_recipe.is_empty() {
            if !policy.flac_mp3_only {
                return Err(SettingsError::InvalidInput {
                    message: "A v2 recipe cannot be saved while non-FLAC/MP3 formats are enabled"
                        .to_owned(),
                });
            }
            validate_quality_recipe(&policy.quality_recipe).map_err(|e| self.config(e))?;
            let (quality_min, quality_max) =
                super::quality::legacy_range_from_recipe(&policy.quality_recipe);
            policy.quality_min = quality_min;
            policy.quality_max = quality_max;
            policy.quality_preference_order =
                super::quality::legacy_recipe_order(&policy.quality_recipe);
            policy.flac_mp3_only = true;
            policy.quality_cutoff = super::quality::clamp_cutoff(
                &policy.quality_cutoff,
                &policy.quality_min,
                &policy.quality_max,
            );
        }
        let saved: DownloadPolicy = self.store.save(policy).map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(policy_to_dto(&saved))
    }

    /// Safe acquisition-policy summary for any signed-in user: the
    /// backend-composed contract sentence plus the source-mode label
    /// only, no admin internals.
    pub fn policy_summary(&self) -> Result<super::models::PolicySummaryResponse, SettingsError> {
        let stored: DownloadPolicy = self.store.get().map_err(|e| self.config(e))?;
        let inputs = summary_inputs(&stored);
        let summary = super::quality::compose_summary(&inputs);
        let order = if inputs.quality_recipe.is_empty() || !inputs.flac_mp3_only {
            if stored.quality_preference_order.is_empty() {
                derive_default_order(&stored.quality_min, &stored.quality_max)
            } else {
                stored.quality_preference_order.clone()
            }
        } else {
            Vec::new()
        };
        let legacy_shape = super::quality::is_legacy_default_shape(
            &order,
            &stored.quality_min,
            &stored.quality_max,
            &stored.lossless_preference,
            stored.lossless_max_bit_depth,
            stored.lossless_max_sample_rate_hz,
            stored.preferred_lossy_bitrate_kbps,
            stored.lossy_min_bitrate_kbps,
            stored.lossy_max_bitrate_kbps,
            &stored.unknown_quality_behavior,
        );
        let (status, error) = super::quality::recipe_status(
            &stored.quality_recipe,
            stored.flac_mp3_only,
            validate_quality_recipe(&stored.quality_recipe).is_ok(),
            validate_quality_recipe(&stored.quality_recipe)
                .err()
                .map(|e| e.to_string())
                .as_deref(),
        );
        Ok(super::models::PolicySummaryResponse {
            summary,
            source_mode: stored.source_selection_mode.clone(),
            legacy_rollback_compatible: stored.quality_recipe.is_empty() && legacy_shape,
            quality_recipe_status: status,
            quality_recipe_error: error,
        })
    }

    /// Admin impact preview of an UNSAVED policy body against persisted
    /// rows: persisted-state bucket counts only. Shares the strict
    /// validation with the save path.
    pub async fn policy_impact(
        &self,
        buckets: &dyn PolicyImpactBuckets,
        dto: &super::models::DownloadPolicyDto,
    ) -> Result<super::models::PolicyImpactResponse, SettingsError> {
        let mut candidate = dto_to_policy(dto);
        if !candidate.quality_recipe.is_empty() {
            if !candidate.flac_mp3_only {
                return Err(SettingsError::InvalidInput {
                    message: "A v2 recipe cannot be saved while non-FLAC/MP3 formats are enabled"
                        .to_owned(),
                });
            }
            validate_quality_recipe(&candidate.quality_recipe).map_err(|e| self.config(e))?;
            let (quality_min, quality_max) =
                super::quality::legacy_range_from_recipe(&candidate.quality_recipe);
            candidate.quality_min = quality_min;
            candidate.quality_max = quality_max;
            candidate.quality_preference_order =
                super::quality::legacy_recipe_order(&candidate.quality_recipe);
            candidate.flac_mp3_only = true;
        }
        // Strict submitted validation, same as the save path.
        candidate.validate().map_err(|e| self.config(e))?;
        let order = if candidate.quality_preference_order.is_empty() {
            derive_default_order(&candidate.quality_min, &candidate.quality_max)
        } else {
            candidate.quality_preference_order.clone()
        };
        let legacy_representable = super::quality::is_legacy_default_shape(
            &order,
            &candidate.quality_min,
            &candidate.quality_max,
            &candidate.lossless_preference,
            candidate.lossless_max_bit_depth,
            candidate.lossless_max_sample_rate_hz,
            candidate.preferred_lossy_bitrate_kbps,
            candidate.lossy_min_bitrate_kbps,
            candidate.lossy_max_bitrate_kbps,
            &candidate.unknown_quality_behavior,
        ) && candidate.source_selection_mode == "source_first";
        let counts = buckets
            .counts()
            .await
            .map_err(|cause| SettingsError::internal(&cause, self.ids.as_ref()))?;
        Ok(super::models::PolicyImpactResponse {
            manual_search_jobs: counts.manual_search_jobs,
            queued_without_attempts: counts.queued_without_attempts,
            awaiting_review: counts.awaiting_review,
            remote_queued_zero_byte: counts.remote_queued_zero_byte,
            transferring_immutable: counts.transferring,
            held_reviews: counts.held_reviews,
            legacy_representable,
        })
    }

    // --- indexers ---------------------------------------------------------------------------------------

    /// List the configured Newznab indexers (keys masked unless unset).
    pub fn list_indexers(&self) -> Result<Vec<super::models::NewznabIndexerDto>, SettingsError> {
        let stored = self.store.get_indexers().map_err(|e| self.config(e))?;
        Ok(stored.into_iter().map(|indexer| indexer.into()).collect())
    }

    /// Save one indexer (create when the id is blank, else update; the
    /// path id wins). A masked key keeps the stored one per element.
    pub async fn save_indexer(
        &self,
        dto: &super::models::NewznabIndexerDto,
    ) -> Result<super::models::IndexerSavedResponse, SettingsError> {
        let mut indexer: NewznabIndexer = dto.clone().into();
        if indexer.indexer_type.trim().is_empty() {
            indexer.indexer_type = "newznab".to_owned();
        }
        let id = self
            .store
            .save_indexer(indexer)
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(super::models::IndexerSavedResponse { id })
    }

    /// Delete one indexer. Unknown ids are a silent no-op.
    pub async fn delete_indexer(&self, indexer_id: &str) -> Result<(), SettingsError> {
        self.store
            .delete_indexer(indexer_id)
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(())
    }

    /// Persist a dragged-card priority order (1-based on save). Unknown
    /// or duplicate ids are a 400, never a partial reorder.
    pub async fn reorder_indexers(&self, ordered_ids: &[String]) -> Result<(), SettingsError> {
        self.store
            .reorder_indexers(ordered_ids)
            .map_err(|e| self.config(e))?;
        self.effects.after_save(SavedSection::Other).await;
        Ok(())
    }

    /// Read one indexer with its key DECRYPTED (test probes only; never
    /// leaves the server).
    pub fn get_indexer_raw(
        &self,
        indexer_id: &str,
    ) -> Result<Option<NewznabIndexer>, SettingsError> {
        let stored = self.store.get_indexers_raw().map_err(|e| self.config(e))?;
        Ok(stored.into_iter().find(|indexer| indexer.id == indexer_id))
    }
}

use crate::runtime_config::Section as _;
use crate::runtime_config::secret_sections::NewznabIndexer;
use crate::runtime_config::sections::{
    DownloadPolicy, QualityRecipeEntry, derive_default_order, validate_quality_recipe,
};

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

/// Masked AcoustID display for a decrypted section: the mask when a key
/// is set, empty when unset. Mirrors the store's own read masking so
/// path edits round-trip the key without touching it.
fn masked_acoustid(stored: &TypedLibrary) -> String {
    use crate::runtime_config::mask::{ACOUSTID_KEY_MASK, display_mask};
    display_mask(stored.acoustid_api_key.expose(), ACOUSTID_KEY_MASK).to_owned()
}

fn library_to_dto(stored: &TypedLibrary) -> LibrarySettingsDto {
    LibrarySettingsDto {
        library_roots: stored
            .library_roots
            .iter()
            .map(|root| LibraryRootDto {
                id: root.id.clone(),
                path: root.path.clone(),
                label: root.label.clone(),
                policy: root.policy.into(),
                rules: root
                    .rules
                    .iter()
                    .map(|rule| LibraryPathRuleDto {
                        id: rule.id.clone(),
                        relative_path: rule.relative_path.clone(),
                        policy: rule.policy.into(),
                    })
                    .collect(),
            })
            .collect(),
        staging_path: stored.staging_path.clone(),
        naming_template: stored.naming_template.clone(),
        acoustid_api_key: stored.acoustid_api_key.expose().to_owned(),
        enabled: stored.enabled,
    }
}

fn dto_to_library(dto: &LibrarySettingsDto) -> TypedLibrary {
    TypedLibrary {
        library_roots: dto
            .library_roots
            .iter()
            .map(|root| LibraryRoot {
                id: root.id.clone(),
                path: root.path.clone(),
                label: root.label.clone(),
                policy: root.policy.into(),
                rules: root
                    .rules
                    .iter()
                    .map(|rule| LibraryPathRule {
                        id: rule.id.clone(),
                        relative_path: rule.relative_path.clone(),
                        policy: rule.policy.into(),
                    })
                    .collect(),
            })
            .collect(),
        staging_path: dto.staging_path.clone(),
        naming_template: dto.naming_template.clone(),
        acoustid_api_key: dto.acoustid_api_key.clone().into(),
        enabled: dto.enabled,
    }
}

/// Backend seconds/milliseconds floored back to the frontend units the
/// DTO documents (v2 `from_backend`: `// 3600`, `// 60`, `// 60000`,
/// `// 1000`; the rest pass through).
fn advanced_to_dto(stored: &AdvancedSettings) -> AdvancedSettingsDto {
    AdvancedSettingsDto {
        cache_ttl_album_library: stored.cache_ttl_album_library / 3600,
        cache_ttl_album_non_library: stored.cache_ttl_album_non_library / 3600,
        cache_ttl_artist_library: stored.cache_ttl_artist_library / 3600,
        cache_ttl_artist_non_library: stored.cache_ttl_artist_non_library / 3600,
        cache_ttl_artist_discovery_library: stored.cache_ttl_artist_discovery_library / 3600,
        cache_ttl_artist_discovery_non_library: stored.cache_ttl_artist_discovery_non_library
            / 3600,
        cache_ttl_search: stored.cache_ttl_search / 60,
        cache_ttl_jellyfin_recently_played: stored.cache_ttl_jellyfin_recently_played / 60,
        cache_ttl_jellyfin_favorites: stored.cache_ttl_jellyfin_favorites / 60,
        cache_ttl_jellyfin_genres: stored.cache_ttl_jellyfin_genres / 60,
        cache_ttl_jellyfin_library_stats: stored.cache_ttl_jellyfin_library_stats / 60,
        cache_ttl_navidrome_albums: stored.cache_ttl_navidrome_albums / 60,
        cache_ttl_navidrome_artists: stored.cache_ttl_navidrome_artists / 60,
        cache_ttl_navidrome_recent: stored.cache_ttl_navidrome_recent / 60,
        cache_ttl_navidrome_favorites: stored.cache_ttl_navidrome_favorites / 60,
        cache_ttl_navidrome_search: stored.cache_ttl_navidrome_search / 60,
        cache_ttl_navidrome_genres: stored.cache_ttl_navidrome_genres / 60,
        cache_ttl_navidrome_stats: stored.cache_ttl_navidrome_stats / 60,
        cache_ttl_plex_albums: stored.cache_ttl_plex_albums / 60,
        cache_ttl_plex_search: stored.cache_ttl_plex_search / 60,
        cache_ttl_plex_genres: stored.cache_ttl_plex_genres / 60,
        cache_ttl_plex_stats: stored.cache_ttl_plex_stats / 60,
        http_timeout: stored.http_timeout,
        http_connect_timeout: stored.http_connect_timeout,
        http_max_connections: stored.http_max_connections,
        batch_artist_images: stored.batch_artist_images,
        batch_albums: stored.batch_albums,
        delay_artist: stored.delay_artist,
        delay_albums: stored.delay_albums,
        memory_cache_max_entries: stored.memory_cache_max_entries,
        memory_cache_cleanup_interval: stored.memory_cache_cleanup_interval,
        cover_memory_cache_max_entries: stored.cover_memory_cache_max_entries,
        cover_memory_cache_max_size_mb: stored.cover_memory_cache_max_size_mb,
        disk_cache_cleanup_interval: stored.disk_cache_cleanup_interval / 60,
        recent_metadata_max_size_mb: stored.recent_metadata_max_size_mb,
        recent_covers_max_size_mb: stored.recent_covers_max_size_mb,
        persistent_metadata_ttl_hours: stored.persistent_metadata_ttl_hours,
        discover_queue_size: stored.discover_queue_size,
        discover_queue_ttl: stored.discover_queue_ttl / 3600,
        discover_queue_auto_generate: stored.discover_queue_auto_generate,
        discover_queue_polling_interval: stored.discover_queue_polling_interval / 1000,
        discover_queue_seed_artists: stored.discover_queue_seed_artists,
        discover_queue_wildcard_slots: stored.discover_queue_wildcard_slots,
        discover_picks_genre_affinity_weight: stored.discover_picks_genre_affinity_weight,
        discover_picks_count: stored.discover_picks_count,
        frontend_ttl_home: stored.frontend_ttl_home / 60000,
        frontend_ttl_discover: stored.frontend_ttl_discover / 60000,
        frontend_ttl_library: stored.frontend_ttl_library / 60000,
        frontend_ttl_recently_added: stored.frontend_ttl_recently_added / 60000,
        frontend_ttl_discover_queue: stored.frontend_ttl_discover_queue / 60000,
        frontend_ttl_search: stored.frontend_ttl_search / 60000,
        frontend_ttl_local_files_sidebar: stored.frontend_ttl_local_files_sidebar / 60000,
        frontend_ttl_jellyfin_sidebar: stored.frontend_ttl_jellyfin_sidebar / 60000,
        frontend_ttl_plex_sidebar: stored.frontend_ttl_plex_sidebar / 60000,
        frontend_ttl_playlist_sources: stored.frontend_ttl_playlist_sources / 60000,
        audiodb_enabled: stored.audiodb_enabled,
        audiodb_name_search_fallback: stored.audiodb_name_search_fallback,
        direct_remote_images_enabled: stored.direct_remote_images_enabled,
        prefer_local_cover_art: stored.prefer_local_cover_art,
        audiodb_api_key: stored.audiodb_api_key.expose().to_owned(),
        cache_ttl_audiodb_found: stored.cache_ttl_audiodb_found / 3600,
        cache_ttl_audiodb_not_found: stored.cache_ttl_audiodb_not_found / 3600,
        cache_ttl_audiodb_library: stored.cache_ttl_audiodb_library / 3600,
        genre_section_ttl: stored.genre_section_ttl / 3600,
        request_history_retention_days: stored.request_history_retention_days,
        ignored_releases_retention_days: stored.ignored_releases_retention_days,
        orphan_cover_demote_interval_hours: stored.orphan_cover_demote_interval_hours,
        store_prune_interval_hours: stored.store_prune_interval_hours,
        sync_stall_timeout_minutes: stored.sync_stall_timeout_minutes,
        sync_max_timeout_hours: stored.sync_max_timeout_hours,
        request_concurrency: stored.request_concurrency,
    }
}

/// Frontend units scaled to backend units (v2 `to_backend`: `* 3600`,
/// `* 60`, `* 60000`, `* 1000`; the rest pass through).
fn dto_to_advanced(dto: &AdvancedSettingsDto) -> AdvancedSettings {
    AdvancedSettings {
        cache_ttl_album_library: dto.cache_ttl_album_library * 3600,
        cache_ttl_album_non_library: dto.cache_ttl_album_non_library * 3600,
        cache_ttl_artist_library: dto.cache_ttl_artist_library * 3600,
        cache_ttl_artist_non_library: dto.cache_ttl_artist_non_library * 3600,
        cache_ttl_artist_discovery_library: dto.cache_ttl_artist_discovery_library * 3600,
        cache_ttl_artist_discovery_non_library: dto.cache_ttl_artist_discovery_non_library * 3600,
        cache_ttl_search: dto.cache_ttl_search * 60,
        cache_ttl_jellyfin_recently_played: dto.cache_ttl_jellyfin_recently_played * 60,
        cache_ttl_jellyfin_favorites: dto.cache_ttl_jellyfin_favorites * 60,
        cache_ttl_jellyfin_genres: dto.cache_ttl_jellyfin_genres * 60,
        cache_ttl_jellyfin_library_stats: dto.cache_ttl_jellyfin_library_stats * 60,
        cache_ttl_navidrome_albums: dto.cache_ttl_navidrome_albums * 60,
        cache_ttl_navidrome_artists: dto.cache_ttl_navidrome_artists * 60,
        cache_ttl_navidrome_recent: dto.cache_ttl_navidrome_recent * 60,
        cache_ttl_navidrome_favorites: dto.cache_ttl_navidrome_favorites * 60,
        cache_ttl_navidrome_search: dto.cache_ttl_navidrome_search * 60,
        cache_ttl_navidrome_genres: dto.cache_ttl_navidrome_genres * 60,
        cache_ttl_navidrome_stats: dto.cache_ttl_navidrome_stats * 60,
        cache_ttl_plex_albums: dto.cache_ttl_plex_albums * 60,
        cache_ttl_plex_search: dto.cache_ttl_plex_search * 60,
        cache_ttl_plex_genres: dto.cache_ttl_plex_genres * 60,
        cache_ttl_plex_stats: dto.cache_ttl_plex_stats * 60,
        http_timeout: dto.http_timeout,
        http_connect_timeout: dto.http_connect_timeout,
        http_max_connections: dto.http_max_connections,
        batch_artist_images: dto.batch_artist_images,
        batch_albums: dto.batch_albums,
        delay_artist: dto.delay_artist,
        delay_albums: dto.delay_albums,
        memory_cache_max_entries: dto.memory_cache_max_entries,
        memory_cache_cleanup_interval: dto.memory_cache_cleanup_interval,
        cover_memory_cache_max_entries: dto.cover_memory_cache_max_entries,
        cover_memory_cache_max_size_mb: dto.cover_memory_cache_max_size_mb,
        disk_cache_cleanup_interval: dto.disk_cache_cleanup_interval * 60,
        recent_metadata_max_size_mb: dto.recent_metadata_max_size_mb,
        recent_covers_max_size_mb: dto.recent_covers_max_size_mb,
        persistent_metadata_ttl_hours: dto.persistent_metadata_ttl_hours,
        discover_queue_size: dto.discover_queue_size,
        discover_queue_ttl: dto.discover_queue_ttl * 3600,
        discover_queue_auto_generate: dto.discover_queue_auto_generate,
        discover_queue_polling_interval: dto.discover_queue_polling_interval * 1000,
        discover_queue_seed_artists: dto.discover_queue_seed_artists,
        discover_queue_wildcard_slots: dto.discover_queue_wildcard_slots,
        discover_picks_genre_affinity_weight: dto.discover_picks_genre_affinity_weight,
        discover_picks_count: dto.discover_picks_count,
        frontend_ttl_home: dto.frontend_ttl_home * 60000,
        frontend_ttl_discover: dto.frontend_ttl_discover * 60000,
        frontend_ttl_library: dto.frontend_ttl_library * 60000,
        frontend_ttl_recently_added: dto.frontend_ttl_recently_added * 60000,
        frontend_ttl_discover_queue: dto.frontend_ttl_discover_queue * 60000,
        frontend_ttl_search: dto.frontend_ttl_search * 60000,
        frontend_ttl_local_files_sidebar: dto.frontend_ttl_local_files_sidebar * 60000,
        frontend_ttl_jellyfin_sidebar: dto.frontend_ttl_jellyfin_sidebar * 60000,
        frontend_ttl_plex_sidebar: dto.frontend_ttl_plex_sidebar * 60000,
        frontend_ttl_playlist_sources: dto.frontend_ttl_playlist_sources * 60000,
        audiodb_enabled: dto.audiodb_enabled,
        audiodb_name_search_fallback: dto.audiodb_name_search_fallback,
        direct_remote_images_enabled: dto.direct_remote_images_enabled,
        prefer_local_cover_art: dto.prefer_local_cover_art,
        audiodb_api_key: dto.audiodb_api_key.clone().into(),
        cache_ttl_audiodb_found: dto.cache_ttl_audiodb_found * 3600,
        cache_ttl_audiodb_not_found: dto.cache_ttl_audiodb_not_found * 3600,
        cache_ttl_audiodb_library: dto.cache_ttl_audiodb_library * 3600,
        genre_section_ttl: dto.genre_section_ttl * 3600,
        request_history_retention_days: dto.request_history_retention_days,
        ignored_releases_retention_days: dto.ignored_releases_retention_days,
        orphan_cover_demote_interval_hours: dto.orphan_cover_demote_interval_hours,
        store_prune_interval_hours: dto.store_prune_interval_hours,
        sync_stall_timeout_minutes: dto.sync_stall_timeout_minutes,
        sync_max_timeout_hours: dto.sync_max_timeout_hours,
        request_concurrency: dto.request_concurrency,
    }
}

fn policy_to_dto(policy: &DownloadPolicy) -> super::models::DownloadPolicyDto {
    let recipe = policy
        .quality_recipe
        .iter()
        .map(|entry| super::models::QualityRecipeEntryDto {
            format: entry.format.clone(),
            quality: entry.quality.clone(),
            min_bitrate_kbps: entry.min_bitrate_kbps,
            target_bitrate_kbps: entry.target_bitrate_kbps,
            max_bitrate_kbps: entry.max_bitrate_kbps,
            bit_depth: entry.bit_depth,
            sample_rate_hz: entry.sample_rate_hz,
        })
        .collect::<Vec<_>>();
    let recipe_error = validate_quality_recipe(&policy.quality_recipe)
        .err()
        .map(|e| e.to_string());
    let (status, error) = super::quality::recipe_status(
        &policy.quality_recipe,
        policy.flac_mp3_only,
        recipe_error.is_none(),
        recipe_error.as_deref(),
    );
    super::models::DownloadPolicyDto {
        quality_min: policy.quality_min.clone(),
        quality_max: policy.quality_max.clone(),
        flac_mp3_only: policy.flac_mp3_only,
        verify_downloads: policy.verify_downloads,
        preflight_score_auto_accept: policy.preflight_score_auto_accept,
        preflight_score_manual_min: policy.preflight_score_manual_min,
        download_stall_timeout_minutes: policy.download_stall_timeout_minutes,
        download_queued_timeout_minutes: policy.download_queued_timeout_minutes,
        preferred_quality_wait_minutes: policy.preferred_quality_wait_minutes,
        max_failover_attempts: policy.max_failover_attempts,
        max_concurrent_downloads: policy.max_concurrent_downloads,
        auto_retry_enabled: policy.auto_retry_enabled,
        auto_retry_max_attempts: policy.auto_retry_max_attempts,
        auto_retry_base_interval_minutes: policy.auto_retry_base_interval_minutes,
        usenet_min_release_age_minutes: policy.usenet_min_release_age_minutes,
        max_size_mb: policy.max_size_mb,
        usenet_retention_days: policy.usenet_retention_days,
        ignored_terms: policy.ignored_terms.clone(),
        required_terms: policy.required_terms.clone(),
        quality_cutoff: policy.quality_cutoff.clone(),
        upgrade_allowed: policy.upgrade_allowed,
        recycle_bin_path: policy.recycle_bin_path.clone(),
        recycle_retention_days: policy.recycle_retention_days,
        max_library_size_gb: policy.max_library_size_gb,
        default_request_quota_count: policy.default_request_quota_count,
        default_request_quota_days: policy.default_request_quota_days,
        default_storage_quota_gb: policy.default_storage_quota_gb,
        background_upgrade_scan_enabled: policy.background_upgrade_scan_enabled,
        background_upgrade_scan_interval_hours: policy.background_upgrade_scan_interval_hours,
        background_upgrade_max_per_run: policy.background_upgrade_max_per_run,
        quality_recipe: recipe,
        quality_preference_order: policy.quality_preference_order.clone(),
        preferred_lossy_bitrate_kbps: policy.preferred_lossy_bitrate_kbps,
        lossy_min_bitrate_kbps: policy.lossy_min_bitrate_kbps,
        lossy_max_bitrate_kbps: policy.lossy_max_bitrate_kbps,
        lossless_preference: policy.lossless_preference.clone(),
        lossless_max_bit_depth: policy.lossless_max_bit_depth,
        lossless_max_sample_rate_hz: policy.lossless_max_sample_rate_hz,
        unknown_quality_behavior: policy.unknown_quality_behavior.clone(),
        source_selection_mode: policy.source_selection_mode.clone(),
        quality_recipe_status: status,
        quality_recipe_error: error,
    }
}

fn dto_to_policy(dto: &super::models::DownloadPolicyDto) -> DownloadPolicy {
    DownloadPolicy {
        quality_min: dto.quality_min.clone(),
        quality_max: dto.quality_max.clone(),
        flac_mp3_only: dto.flac_mp3_only,
        verify_downloads: dto.verify_downloads,
        preflight_score_auto_accept: dto.preflight_score_auto_accept,
        preflight_score_manual_min: dto.preflight_score_manual_min,
        download_stall_timeout_minutes: dto.download_stall_timeout_minutes,
        download_queued_timeout_minutes: dto.download_queued_timeout_minutes,
        preferred_quality_wait_minutes: dto.preferred_quality_wait_minutes,
        max_failover_attempts: dto.max_failover_attempts,
        max_concurrent_downloads: dto.max_concurrent_downloads,
        auto_retry_enabled: dto.auto_retry_enabled,
        auto_retry_max_attempts: dto.auto_retry_max_attempts,
        auto_retry_base_interval_minutes: dto.auto_retry_base_interval_minutes,
        usenet_min_release_age_minutes: dto.usenet_min_release_age_minutes,
        max_size_mb: dto.max_size_mb,
        usenet_retention_days: dto.usenet_retention_days,
        ignored_terms: dto.ignored_terms.clone(),
        required_terms: dto.required_terms.clone(),
        quality_cutoff: dto.quality_cutoff.clone(),
        upgrade_allowed: dto.upgrade_allowed,
        recycle_bin_path: dto.recycle_bin_path.clone(),
        recycle_retention_days: dto.recycle_retention_days,
        max_library_size_gb: dto.max_library_size_gb,
        default_request_quota_count: dto.default_request_quota_count,
        default_request_quota_days: dto.default_request_quota_days,
        default_storage_quota_gb: dto.default_storage_quota_gb,
        background_upgrade_scan_enabled: dto.background_upgrade_scan_enabled,
        background_upgrade_scan_interval_hours: dto.background_upgrade_scan_interval_hours,
        background_upgrade_max_per_run: dto.background_upgrade_max_per_run,
        quality_recipe: dto
            .quality_recipe
            .iter()
            .map(|entry| QualityRecipeEntry {
                format: entry.format.clone(),
                quality: entry.quality.clone(),
                min_bitrate_kbps: entry.min_bitrate_kbps,
                target_bitrate_kbps: entry.target_bitrate_kbps,
                max_bitrate_kbps: entry.max_bitrate_kbps,
                bit_depth: entry.bit_depth,
                sample_rate_hz: entry.sample_rate_hz,
            })
            .collect(),
        quality_preference_order: dto.quality_preference_order.clone(),
        preferred_lossy_bitrate_kbps: dto.preferred_lossy_bitrate_kbps,
        lossy_min_bitrate_kbps: dto.lossy_min_bitrate_kbps,
        lossy_max_bitrate_kbps: dto.lossy_max_bitrate_kbps,
        lossless_preference: dto.lossless_preference.clone(),
        lossless_max_bit_depth: dto.lossless_max_bit_depth,
        lossless_max_sample_rate_hz: dto.lossless_max_sample_rate_hz,
        unknown_quality_behavior: dto.unknown_quality_behavior.clone(),
        source_selection_mode: dto.source_selection_mode.clone(),
    }
}

fn summary_inputs(policy: &DownloadPolicy) -> super::quality::SummaryInputs {
    let mut order = policy.quality_preference_order.clone();
    if order.is_empty() && policy.quality_recipe.is_empty() {
        order = derive_default_order(&policy.quality_min, &policy.quality_max);
    }
    super::quality::SummaryInputs {
        quality_preference_order: order,
        quality_recipe: policy.quality_recipe.clone(),
        flac_mp3_only: policy.flac_mp3_only,
        lossless_preference: policy.lossless_preference.clone(),
        lossless_max_bit_depth: policy.lossless_max_bit_depth,
        lossless_max_sample_rate_hz: policy.lossless_max_sample_rate_hz,
        unknown_quality_behavior: policy.unknown_quality_behavior.clone(),
    }
}

impl From<NewznabIndexer> for super::models::NewznabIndexerDto {
    fn from(value: NewznabIndexer) -> Self {
        Self {
            id: value.id,
            indexer_type: value.indexer_type,
            name: value.name,
            url: value.url,
            api_key: value.api_key.expose().to_owned(),
            categories: value.categories,
            enabled: value.enabled,
            priority: value.priority,
        }
    }
}

impl From<super::models::NewznabIndexerDto> for NewznabIndexer {
    fn from(value: super::models::NewznabIndexerDto) -> Self {
        Self {
            id: value.id,
            indexer_type: value.indexer_type,
            name: value.name,
            url: value.url,
            api_key: value.api_key.into(),
            categories: value.categories,
            enabled: value.enabled,
            priority: value.priority,
        }
    }
}

// --- connection bridges (DTO <-> section; secrets are plain strings both sides) ---

use crate::runtime_config::secret_sections::{
    AdvancedSettings, DownloadClients, EventsSettings, JellyfinConnection, LibraryPathRule,
    LibraryRoot, ListenBrainzConnection, NavidromeConnection, OidcConnection, PlexConnection,
    ProwlarrConnection, SabnzbdConnection, SlskdConnection, TypedLibrary, WrappedSettings,
    YouTubeConnection,
};

impl From<SlskdConnection> for super::models::SlskdConnectionDto {
    fn from(value: SlskdConnection) -> Self {
        Self {
            enabled: value.enabled,
            client_type: value.client_type,
            url: value.url,
            api_key: value.api_key.expose().to_owned(),
            verify_downloads: value.verify_downloads,
            quality_min: value.quality_min,
            quality_max: value.quality_max,
            flac_mp3_only: value.flac_mp3_only,
            downloads_subpath: value.downloads_subpath,
            slskd_incomplete_mount: value.slskd_incomplete_mount,
            preflight_score_auto_accept: value.preflight_score_auto_accept,
            preflight_score_manual_min: value.preflight_score_manual_min,
            download_stall_timeout_minutes: value.download_stall_timeout_minutes,
            download_queued_timeout_minutes: value.download_queued_timeout_minutes,
            preferred_quality_wait_minutes: value.preferred_quality_wait_minutes,
            max_failover_attempts: value.max_failover_attempts,
            max_concurrent_downloads: value.max_concurrent_downloads,
            auto_retry_enabled: value.auto_retry_enabled,
            auto_retry_max_attempts: value.auto_retry_max_attempts,
            auto_retry_base_interval_minutes: value.auto_retry_base_interval_minutes,
        }
    }
}

impl From<super::models::SlskdConnectionDto> for SlskdConnection {
    fn from(value: super::models::SlskdConnectionDto) -> Self {
        Self {
            enabled: value.enabled,
            client_type: value.client_type,
            url: value.url,
            api_key: value.api_key.into(),
            verify_downloads: value.verify_downloads,
            quality_min: value.quality_min,
            quality_max: value.quality_max,
            flac_mp3_only: value.flac_mp3_only,
            downloads_subpath: value.downloads_subpath,
            slskd_incomplete_mount: value.slskd_incomplete_mount,
            preflight_score_auto_accept: value.preflight_score_auto_accept,
            preflight_score_manual_min: value.preflight_score_manual_min,
            download_stall_timeout_minutes: value.download_stall_timeout_minutes,
            download_queued_timeout_minutes: value.download_queued_timeout_minutes,
            preferred_quality_wait_minutes: value.preferred_quality_wait_minutes,
            max_failover_attempts: value.max_failover_attempts,
            max_concurrent_downloads: value.max_concurrent_downloads,
            auto_retry_enabled: value.auto_retry_enabled,
            auto_retry_max_attempts: value.auto_retry_max_attempts,
            auto_retry_base_interval_minutes: value.auto_retry_base_interval_minutes,
        }
    }
}

impl From<SabnzbdConnection> for super::models::SabnzbdConnectionDto {
    fn from(value: SabnzbdConnection) -> Self {
        Self {
            enabled: value.enabled,
            client_type: value.client_type,
            url: value.url,
            api_key: value.api_key.expose().to_owned(),
            category: value.category,
            priority: value.priority,
            post_processing: value.post_processing,
            downloads_mount: value.downloads_mount,
        }
    }
}

impl From<super::models::SabnzbdConnectionDto> for SabnzbdConnection {
    fn from(value: super::models::SabnzbdConnectionDto) -> Self {
        Self {
            enabled: value.enabled,
            client_type: value.client_type,
            url: value.url,
            api_key: value.api_key.into(),
            category: value.category,
            priority: value.priority,
            post_processing: value.post_processing,
            downloads_mount: value.downloads_mount,
        }
    }
}

impl From<ProwlarrConnection> for super::models::ProwlarrConnectionDto {
    fn from(value: ProwlarrConnection) -> Self {
        Self {
            enabled: value.enabled,
            url: value.url,
            api_key: value.api_key.expose().to_owned(),
        }
    }
}

impl From<super::models::ProwlarrConnectionDto> for ProwlarrConnection {
    fn from(value: super::models::ProwlarrConnectionDto) -> Self {
        Self {
            enabled: value.enabled,
            url: value.url,
            api_key: value.api_key.into(),
        }
    }
}

impl From<JellyfinConnection> for super::models::JellyfinConnectionDto {
    fn from(value: JellyfinConnection) -> Self {
        Self {
            jellyfin_url: value.jellyfin_url,
            api_key: value.api_key.expose().to_owned(),
            user_id: value.user_id,
            enabled: value.enabled,
            login_enabled: value.login_enabled,
        }
    }
}

impl From<super::models::JellyfinConnectionDto> for JellyfinConnection {
    fn from(value: super::models::JellyfinConnectionDto) -> Self {
        Self {
            jellyfin_url: value.jellyfin_url,
            api_key: value.api_key.into(),
            user_id: value.user_id,
            enabled: value.enabled,
            login_enabled: value.login_enabled,
        }
    }
}

impl From<NavidromeConnection> for super::models::NavidromeConnectionDto {
    fn from(value: NavidromeConnection) -> Self {
        Self {
            navidrome_url: value.navidrome_url,
            username: value.username,
            password: value.password.expose().to_owned(),
            enabled: value.enabled,
            playlist_sync_enabled: value.playlist_sync_enabled,
            playlist_sync_path: value.playlist_sync_path,
            playlist_sync_scope: value.playlist_sync_scope,
            playlist_sync_remove_deleted: value.playlist_sync_remove_deleted,
        }
    }
}

impl From<super::models::NavidromeConnectionDto> for NavidromeConnection {
    fn from(value: super::models::NavidromeConnectionDto) -> Self {
        Self {
            navidrome_url: value.navidrome_url,
            username: value.username,
            password: value.password.into(),
            enabled: value.enabled,
            playlist_sync_enabled: value.playlist_sync_enabled,
            playlist_sync_path: value.playlist_sync_path,
            playlist_sync_scope: value.playlist_sync_scope,
            playlist_sync_remove_deleted: value.playlist_sync_remove_deleted,
        }
    }
}

impl From<PlexConnection> for super::models::PlexConnectionDto {
    fn from(value: PlexConnection) -> Self {
        Self {
            plex_url: value.plex_url,
            plex_token: value.plex_token.expose().to_owned(),
            enabled: value.enabled,
            login_enabled: value.login_enabled,
            music_library_ids: value.music_library_ids,
            scrobble_to_plex: value.scrobble_to_plex,
        }
    }
}

impl From<super::models::PlexConnectionDto> for PlexConnection {
    fn from(value: super::models::PlexConnectionDto) -> Self {
        Self {
            plex_url: value.plex_url,
            plex_token: value.plex_token.into(),
            enabled: value.enabled,
            login_enabled: value.login_enabled,
            music_library_ids: value.music_library_ids,
            scrobble_to_plex: value.scrobble_to_plex,
        }
    }
}

impl From<ListenBrainzConnection> for super::models::ListenBrainzConnectionDto {
    fn from(value: ListenBrainzConnection) -> Self {
        Self {
            username: value.username,
            user_token: value.user_token.expose().to_owned(),
            enabled: value.enabled,
        }
    }
}

impl From<super::models::ListenBrainzConnectionDto> for ListenBrainzConnection {
    fn from(value: super::models::ListenBrainzConnectionDto) -> Self {
        Self {
            username: value.username,
            user_token: value.user_token.into(),
            enabled: value.enabled,
        }
    }
}

impl From<YouTubeConnection> for super::models::YouTubeConnectionDto {
    fn from(value: YouTubeConnection) -> Self {
        Self {
            api_key: value.api_key.expose().to_owned(),
            enabled: value.enabled,
            api_enabled: value.api_enabled,
            daily_quota_limit: value.daily_quota_limit,
        }
    }
}

impl From<super::models::YouTubeConnectionDto> for YouTubeConnection {
    fn from(value: super::models::YouTubeConnectionDto) -> Self {
        Self {
            api_key: value.api_key.into(),
            enabled: value.enabled,
            api_enabled: value.api_enabled,
            daily_quota_limit: value.daily_quota_limit,
        }
    }
}

impl From<EventsSettings> for super::models::EventsSettingsDto {
    fn from(value: EventsSettings) -> Self {
        Self {
            enabled: value.enabled,
            ticketmaster_enabled: value.ticketmaster_enabled,
            ticketmaster_api_key: value.ticketmaster_api_key.expose().to_owned(),
            skiddle_enabled: value.skiddle_enabled,
            skiddle_api_key: value.skiddle_api_key.expose().to_owned(),
            poll_time: value.poll_time,
            sweep_scope: value.sweep_scope.into(),
        }
    }
}

impl From<super::models::EventsSettingsDto> for EventsSettings {
    fn from(value: super::models::EventsSettingsDto) -> Self {
        Self {
            enabled: value.enabled,
            ticketmaster_enabled: value.ticketmaster_enabled,
            ticketmaster_api_key: value.ticketmaster_api_key.into(),
            skiddle_enabled: value.skiddle_enabled,
            skiddle_api_key: value.skiddle_api_key.into(),
            poll_time: value.poll_time,
            sweep_scope: value.sweep_scope.into(),
        }
    }
}

impl From<WrappedSettings> for super::models::WrappedSettingsDto {
    fn from(value: WrappedSettings) -> Self {
        Self {
            api_key: value.api_key.expose().to_owned(),
        }
    }
}

impl From<super::models::WrappedSettingsDto> for WrappedSettings {
    fn from(value: super::models::WrappedSettingsDto) -> Self {
        Self {
            api_key: value.api_key.into(),
        }
    }
}

impl From<OidcConnection> for super::models::OidcConnectionDto {
    fn from(value: OidcConnection) -> Self {
        Self {
            enabled: value.enabled,
            issuer: value.issuer,
            client_id: value.client_id,
            client_secret: value.client_secret.expose().to_owned(),
            scopes: value.scopes,
            redirect_uri: value.redirect_uri,
        }
    }
}

impl From<super::models::OidcConnectionDto> for OidcConnection {
    fn from(value: super::models::OidcConnectionDto) -> Self {
        Self {
            enabled: value.enabled,
            issuer: value.issuer,
            client_id: value.client_id,
            client_secret: value.client_secret.into(),
            scopes: value.scopes,
            redirect_uri: value.redirect_uri,
        }
    }
}

/// Server-local timezone label for the daily-scan picker. Prefers the
/// IANA name from `TZ`, else the local abbreviation, else "server time".
pub fn server_timezone() -> String {
    if let Some(tz) = std::env::var("TZ")
        .ok()
        .map(|tz| tz.trim().to_owned())
        .filter(|tz| !tz.is_empty())
    {
        return tz;
    }
    v2_timezone_label().unwrap_or_else(|| "server time".to_owned())
}

fn v2_timezone_label() -> Option<String> {
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
