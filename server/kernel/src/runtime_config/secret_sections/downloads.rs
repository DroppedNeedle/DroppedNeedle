//! Download clients and indexers: slskd, SABnzbd, Newznab indexers,
//! Prowlarr, and the Lidarr import connection.

use super::*;

// --- download_client (slskd) ------------------------------------------------
// Dropped: min_bitrate_kbps (v2-deprecated, superseded by quality_min/max, zero consumers).

/// slskd download-client connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SlskdConnection {
    /// Master switch.
    pub enabled: bool,
    /// Client type tag.
    pub client_type: String,
    /// slskd base URL.
    pub url: String,
    /// API key (encrypted at rest).
    #[schema(value_type = String)]
    pub api_key: Secret,
    /// Verify downloads after landing.
    pub verify_downloads: bool,
    /// Worst accepted tier label.
    pub quality_min: String,
    /// Best accepted tier label.
    pub quality_max: String,
    /// FLAC/MP3 only.
    pub flac_mp3_only: bool,
    /// Relative subpath inside the mount where slskd saves (confined).
    pub downloads_subpath: String,
    /// Absolute incomplete-downloads dir ("" disables the fallback).
    pub slskd_incomplete_mount: String,
    /// Preflight auto-accept score.
    pub preflight_score_auto_accept: f64,
    /// Preflight manual-review floor.
    pub preflight_score_manual_min: f64,
    /// Frozen-transfer stall timeout.
    pub download_stall_timeout_minutes: i64,
    /// Remote-queue wait timeout.
    pub download_queued_timeout_minutes: i64,
    /// Preferred-quality wait.
    pub preferred_quality_wait_minutes: i64,
    /// Failover attempts per request.
    pub max_failover_attempts: i64,
    /// Concurrent downloads.
    pub max_concurrent_downloads: i64,
    /// Automatic retry switch.
    pub auto_retry_enabled: bool,
    /// Retry attempts.
    pub auto_retry_max_attempts: i64,
    /// Retry base interval.
    pub auto_retry_base_interval_minutes: i64,
}

impl Default for SlskdConnection {
    fn default() -> Self {
        Self {
            enabled: false,
            client_type: "slskd".to_owned(),
            url: String::new(),
            api_key: Secret::default(),
            verify_downloads: true,
            quality_min: "mp3_320".to_owned(),
            quality_max: "lossless".to_owned(),
            flac_mp3_only: true,
            downloads_subpath: String::new(),
            slskd_incomplete_mount: String::new(),
            preflight_score_auto_accept: 0.70,
            preflight_score_manual_min: 0.50,
            download_stall_timeout_minutes: 30,
            download_queued_timeout_minutes: 120,
            preferred_quality_wait_minutes: 15,
            max_failover_attempts: 3,
            max_concurrent_downloads: 3,
            auto_retry_enabled: true,
            auto_retry_max_attempts: 6,
            auto_retry_base_interval_minutes: 15,
        }
    }
}

impl Section for SlskdConnection {
    const KEY: &'static str = "download_client";

    fn validate(&self) -> Result<(), ConfigError> {
        let key = Self::KEY;
        check_range(
            key,
            "download_stall_timeout_minutes",
            self.download_stall_timeout_minutes,
            2,
            240,
        )?;
        check_range(
            key,
            "download_queued_timeout_minutes",
            self.download_queued_timeout_minutes,
            5,
            1440,
        )?;
        check_range(
            key,
            "preferred_quality_wait_minutes",
            self.preferred_quality_wait_minutes,
            1,
            1440,
        )?;
        check_range(
            key,
            "max_failover_attempts",
            self.max_failover_attempts,
            1,
            10,
        )?;
        check_range(
            key,
            "max_concurrent_downloads",
            self.max_concurrent_downloads,
            1,
            10,
        )?;
        check_range(
            key,
            "auto_retry_max_attempts",
            self.auto_retry_max_attempts,
            0,
            20,
        )?;
        check_range(
            key,
            "auto_retry_base_interval_minutes",
            self.auto_retry_base_interval_minutes,
            1,
            1440,
        )?;
        Ok(())
    }

    fn normalize(&mut self) {
        normalize_http_url(&mut self.url, "https://");
        if tier_rank(&self.quality_min).is_none() {
            self.quality_min = "mp3_320".to_owned();
        }
        if tier_rank(&self.quality_max).is_none() {
            self.quality_max = "lossless".to_owned();
        }
        let min_rank = tier_rank(&self.quality_min).unwrap_or(3);
        let max_rank = tier_rank(&self.quality_max).unwrap_or(4);
        if min_rank > max_rank {
            self.quality_min = self.quality_max.clone();
        }
        self.downloads_subpath = sanitize_subpath(&self.downloads_subpath);
        self.slskd_incomplete_mount = sanitize_absolute_mount(&self.slskd_incomplete_mount);
    }
}

impl SecretSection for SlskdConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: SLSKD_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- download_clients (sabnzbd sub-object) ---------------------------------

/// SABnzbd connection. The key is the full key (the add-only nzbkey cannot
/// do queue/history/delete).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SabnzbdConnection {
    /// Master switch.
    pub enabled: bool,
    /// Client type tag.
    pub client_type: String,
    /// SABnzbd base URL.
    pub url: String,
    /// Full API key (encrypted at rest).
    #[schema(value_type = String)]
    pub api_key: Secret,
    /// Category (`*`: a fresh SABnzbd has no `droppedneedle` category).
    pub category: String,
    /// Job priority.
    pub priority: i64,
    /// Post-processing level.
    pub post_processing: i64,
    /// Where DroppedNeedle sees SABnzbd's completed dir.
    pub downloads_mount: String,
}

impl Default for SabnzbdConnection {
    fn default() -> Self {
        Self {
            enabled: false,
            client_type: "sabnzbd".to_owned(),
            url: String::new(),
            api_key: Secret::default(),
            category: "*".to_owned(),
            priority: 0,
            post_processing: 3,
            downloads_mount: "/sabnzbd-downloads".to_owned(),
        }
    }
}

/// The `download_clients` section (SABnzbd sub-object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DownloadClients {
    /// SABnzbd connection.
    pub sabnzbd: SabnzbdConnection,
}

impl Section for DownloadClients {
    const KEY: &'static str = "download_clients";

    fn normalize(&mut self) {
        normalize_http_url(&mut self.sabnzbd.url, "https://");
    }
}

impl SecretSection for DownloadClients {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.sabnzbd.api_key,
            mask: SABNZBD_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- indexers (order-preserved Newznab list) --------------------------------

/// One configured Newznab indexer. DroppedNeedle ships none; the user adds
/// their own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NewznabIndexer {
    /// Stable id.
    pub id: String,
    /// Indexer type tag (stored as `type`; renamed for the keyword).
    #[serde(rename = "type")]
    pub indexer_type: String,
    /// Display name (list identity for import).
    pub name: String,
    /// Indexer base URL.
    pub url: String,
    /// API key (encrypted at rest, per element).
    #[schema(value_type = String)]
    pub api_key: Secret,
    /// Newznab categories.
    pub categories: Vec<i64>,
    /// Master switch.
    pub enabled: bool,
    /// Priority (lower first).
    pub priority: i64,
}

impl Default for NewznabIndexer {
    fn default() -> Self {
        Self {
            id: String::new(),
            indexer_type: "newznab".to_owned(),
            name: String::new(),
            url: String::new(),
            api_key: Secret::default(),
            categories: vec![3000, 3010, 3040],
            enabled: true,
            priority: 1,
        }
    }
}

/// The `indexers` section: order-preserved list (priority order must
/// survive export/import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Indexers(pub Vec<NewznabIndexer>);

impl Section for Indexers {
    const KEY: &'static str = "indexers";

    fn normalize(&mut self) {
        for indexer in &mut self.0 {
            normalize_http_url(&mut indexer.url, "https://");
        }
    }
}

// No SecretSection impl on purpose: masks resolve per element matched BY ID
// (upsert, delete, reorder), never by list position. `ConfigStore` carries
// the dedicated indexer methods instead, so a whole-list save cannot pair
// the wrong ciphertext with the wrong row.

// --- prowlarr ---------------------------------------------------------------

/// Single Prowlarr connection (Prowlarr multiplexes its own indexers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct ProwlarrConnection {
    /// Master switch.
    pub enabled: bool,
    /// Prowlarr base URL (LAN service, `http://` default).
    pub url: String,
    /// API key (encrypted at rest).
    #[schema(value_type = String)]
    pub api_key: Secret,
}

impl Section for ProwlarrConnection {
    const KEY: &'static str = "prowlarr";

    fn normalize(&mut self) {
        normalize_http_url(&mut self.url, "http://");
        strip_api_suffix(&mut self.url);
    }
}

impl SecretSection for ProwlarrConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: PROWLARR_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- lidarr_import (read-only import connection) ----------------------------

/// Read-only Lidarr import connection: a single admin-configured Lidarr the
/// monitored-artist importer reads from. Not a management integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LidarrImportConnection {
    /// Lidarr base URL (plain-http LAN service, `http://` default).
    pub url: String,
    /// API key (encrypted at rest).
    pub api_key: Secret,
}

impl Section for LidarrImportConnection {
    const KEY: &'static str = "lidarr_import";

    fn normalize(&mut self) {
        normalize_http_url(&mut self.url, "http://");
        strip_api_suffix(&mut self.url);
    }
}

impl SecretSection for LidarrImportConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: LIDARR_API_KEY_MASK,
            strip: true,
        }]
    }
}
