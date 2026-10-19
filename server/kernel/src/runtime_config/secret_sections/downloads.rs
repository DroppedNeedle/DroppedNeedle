//! Download clients and indexers: slskd, SABnzbd, Newznab indexers,
//! Prowlarr, and the Lidarr import connection.

use super::*;

// --- download_client (slskd) ------------------------------------------------

/// slskd download-client connection. Quality, timeouts and retries live in
/// the download policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct SlskdConnection {
    /// Master switch.
    pub enabled: bool,
    /// slskd base URL.
    pub url: String,
    /// API key (encrypted at rest).
    #[schema(value_type = String)]
    pub api_key: Secret,
    /// Relative subpath inside the mount where slskd saves (confined).
    pub downloads_subpath: String,
    /// Absolute incomplete-downloads dir ("" disables the fallback).
    pub slskd_incomplete_mount: String,
}

impl Section for SlskdConnection {
    const KEY: &'static str = "download_client";
    const EFFECTS: &'static [Effect] = &[Effect::WakeDownloads];

    fn normalize(&mut self) {
        normalize_http_url(&mut self.url, "https://");
        self.downloads_subpath = sanitize_subpath(&self.downloads_subpath);
        self.slskd_incomplete_mount = sanitize_absolute_mount(&self.slskd_incomplete_mount);
    }
}

impl SecretSection for SlskdConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
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
    /// SABnzbd base URL.
    pub url: String,
    /// Full API key (encrypted at rest).
    #[schema(value_type = String)]
    pub api_key: Secret,
    /// Category (`*`: a fresh SABnzbd has no `droppedneedle` category).
    pub category: String,
    /// Job priority.
    pub priority: i64,
    /// Where DroppedNeedle sees SABnzbd's completed dir.
    pub downloads_mount: String,
}

impl Default for SabnzbdConnection {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            api_key: Secret::default(),
            category: "*".to_owned(),
            priority: 0,
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
    const EFFECTS: &'static [Effect] = &[Effect::WakeDownloads];

    fn normalize(&mut self) {
        normalize_http_url(&mut self.sabnzbd.url, "https://");
    }
}

impl SecretSection for DownloadClients {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.sabnzbd.api_key,
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
            strip: true,
        }]
    }
}
