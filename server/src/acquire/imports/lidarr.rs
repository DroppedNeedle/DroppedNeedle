//! Read-only Lidarr import client and service.
//!
//! Ports `backend/repositories/lidarr_import/` and
//! `backend/services/lidarr_import_service.py` exactly: two GET endpoints
//! (`/system/status`, `/artist`) under `{base}/api/v1` with the `X-Api-Key`
//! header (verified against live Lidarr 3.1.3.4968), monitored artists only,
//! the authoritative re-fetch, the pre-read counts, the D9 auto-download
//! rule, and the ordered crash-safe writes. The management tombstone holds:
//! no method here issues any Lidarr management call.

use std::collections::{HashMap, HashSet};
#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[cfg(any(test, feature = "test-support"))]
use super::models::LIDARR_API_KEY_MASK;
use super::models::{
    LidarrArtistCandidate, LidarrArtistListResponse, LidarrConnectionSettings, LidarrImportResponse,
};

/// True for a well-formed MusicBrainz id: `8-4-4-4-12` lowercase-or-upper
/// hex, surrounding whitespace ignored, `unknown_` ids rejected (v2
/// `infrastructure/validators.is_valid_mbid`).
pub fn is_valid_mbid(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with("unknown_") {
        return false;
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Normalise a Lidarr base URL to a bare origin (v2
/// `LidarrImportConnectionSettings.__post_init__`): trim, default a
/// schemeless host to `http://` (Lidarr is a plain-http LAN service, never
/// the SABnzbd https-forcing), strip one trailing slash, then strip a
/// pasted `/api/v1` or `/api` suffix. Silent, so Test needs no suggestion.
pub fn normalize_lidarr_url(raw: &str) -> String {
    let mut url = raw.trim().to_owned();
    if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("http://{url}");
    }
    url = url.trim_end_matches('/').to_owned();
    for suffix in ["/api/v1", "/api"] {
        if let Some(bare) = url.strip_suffix(suffix) {
            url = bare.trim_end_matches('/').to_owned();
            break;
        }
    }
    url
}

/// One Lidarr artist row (v2 `LidarrArtist`, camelCase, tolerant defaults).
/// `foreign_artist_id` is the MBID join key; Lidarr's `mbId` is never
/// populated live and is ignored entirely.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LidarrArtist {
    /// MusicBrainz artist MBID.
    pub foreign_artist_id: String,
    /// Artist name as Lidarr reports it.
    pub artist_name: String,
    /// Monitored in Lidarr.
    pub monitored: bool,
    /// `none` or `all`.
    pub monitor_new_items: String,
    /// `continuing`, `ended`, ... Imported regardless (v2 A3).
    pub status: String,
}

/// `system/status` probe shape (v2 `LidarrSystemStatus`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LidarrSystemStatus {
    /// Lidarr version string.
    pub version: String,
}

/// Classified Lidarr failure. Transports and decodes collapse to
/// unreachable; only 401/403 reads as auth (v2 `LidarrImportError(auth)`).
#[derive(Debug, Clone, PartialEq)]
pub enum LidarrError {
    /// Lidarr rejected the API key (HTTP 401/403).
    Auth,
    /// Unreachable, non-2xx, or undecodable. The detail reaches the log
    /// only; callers render a fixed user-safe summary.
    Unavailable(String),
}

/// Read-only Lidarr client: exactly the two sanctioned GETs.
#[derive(Debug, Clone)]
pub struct LidarrClient {
    http: reqwest::Client,
}

impl LidarrClient {
    /// Build over a shared outbound client.
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// Probe `system/status` with submitted-or-stored credentials.
    pub async fn system_status(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<LidarrSystemStatus, LidarrError> {
        let body = self.get(base_url, api_key, "/system/status").await?;
        serde_json::from_slice::<LidarrSystemStatus>(&body).map_err(|cause| {
            LidarrError::Unavailable(format!("lidarr status decode failed: {cause}"))
        })
    }

    /// Fetch the whole artist catalog for the import.
    pub async fn list_artists(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<LidarrArtist>, LidarrError> {
        let body = self.get(base_url, api_key, "/artist").await?;
        serde_json::from_slice::<Vec<LidarrArtist>>(&body).map_err(|cause| {
            LidarrError::Unavailable(format!("lidarr artist decode failed: {cause}"))
        })
    }

    async fn get(&self, base_url: &str, api_key: &str, path: &str) -> Result<Vec<u8>, LidarrError> {
        let endpoint = format!("{}/api/v1{path}", base_url.trim_end_matches('/'));
        let response = self
            .http
            .get(&endpoint)
            .header("X-Api-Key", api_key)
            .send()
            .await
            .map_err(|cause| LidarrError::Unavailable(format!("lidarr request failed: {cause}")))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(LidarrError::Auth);
        }
        if !status.is_success() {
            return Err(LidarrError::Unavailable(format!(
                "lidarr returned HTTP {}",
                status.as_u16()
            )));
        }
        response
            .bytes()
            .await
            .map(|body| body.to_vec())
            .map_err(|cause| LidarrError::Unavailable(format!("lidarr read failed: {cause}")))
    }
}

/// Follow rows the import reads and writes. The bulk store method keeps the
/// zero-MusicBrainz-call guarantee (v2 DR2): names come from Lidarr, never
/// the per-artist follow path.
pub trait FollowStore: Send + Sync {
    /// Lowercased MBIDs of `candidates` the user already follows.
    fn existing_followed_lower(&self, user_id: &str, candidates: &[String]) -> HashSet<String>;
    /// Follow every `(mbid, name)` pair idempotently, preserving
    /// `auto_download` and `followed_at` on conflicts (v2 DR4).
    fn follow_artists_bulk(&self, user_id: &str, pairs: &[(String, String)]);
    /// Flip auto-download intent for followed rows.
    fn set_auto_download_intent_bulk(&self, user_id: &str, mbids: &[String], intent: bool);
    /// Auto-download intent for one followed row, for brief assertions.
    fn auto_download_intent(&self, user_id: &str, mbid_lower: &str) -> bool;
}

/// Approval batches for non-admin auto-download mirrors (v2
/// `FollowService.create_import_batch`).
pub trait ApprovalSink: Send + Sync {
    /// Open one batch over the auto-download subset; returns its id.
    fn create_import_batch(&self, user_id: &str, pairs: &[(String, String)]) -> String;
}

/// Lidarr connection settings rows (v2 `PreferencesService` lidarr block).
pub trait LidarrSettingsStore: Send + Sync {
    /// Masked settings for reads.
    fn get(&self) -> LidarrConnectionSettings;
    /// Raw settings including the real key, for probes and imports.
    fn get_raw(&self) -> LidarrConnectionSettings;
    /// Save; a masked key preserves the stored one.
    fn save(&self, settings: &LidarrConnectionSettings);
}

/// In-memory follow rows for tests and the pre-persistence tier.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryFollowStore {
    inner: Mutex<HashMap<(String, String), MemoryFollow>>,
}

/// One in-memory follow row. The descriptive fields mirror the durable
/// row shape; only intent is read back.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MemoryFollow {
    name: String,
    auto_download: bool,
    followed_at: u64,
    seq: u64,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryFollowStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl FollowStore for MemoryFollowStore {
    fn existing_followed_lower(&self, user_id: &str, candidates: &[String]) -> HashSet<String> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        candidates
            .iter()
            .filter(|candidate| inner.contains_key(&(user_id.to_owned(), (*candidate).clone())))
            .cloned()
            .collect()
    }

    fn follow_artists_bulk(&self, user_id: &str, pairs: &[(String, String)]) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let seq = inner.len() as u64;
        for (index, (mbid, name)) in pairs.iter().enumerate() {
            inner
                .entry((user_id.to_owned(), mbid.to_lowercase()))
                .or_insert_with(|| MemoryFollow {
                    name: name.clone(),
                    auto_download: false,
                    followed_at: seq + index as u64,
                    seq: seq + index as u64,
                });
        }
    }

    fn set_auto_download_intent_bulk(&self, user_id: &str, mbids: &[String], intent: bool) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for mbid in mbids {
            if let Some(row) = inner.get_mut(&(user_id.to_owned(), mbid.to_lowercase())) {
                row.auto_download = intent;
            }
        }
    }

    fn auto_download_intent(&self, user_id: &str, mbid_lower: &str) -> bool {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner
            .get(&(user_id.to_owned(), mbid_lower.to_owned()))
            .is_some_and(|row| row.auto_download)
    }
}

/// One recorded approval batch: owning user plus the auto-download pairs.
#[cfg(any(test, feature = "test-support"))]
type ApprovalBatch = (String, Vec<(String, String)>);

/// In-memory approval batches.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryApprovalSink {
    inner: Mutex<Vec<ApprovalBatch>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryApprovalSink {
    /// Empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Batches opened so far, for brief assertions.
    pub fn batches(&self) -> Vec<ApprovalBatch> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ApprovalSink for MemoryApprovalSink {
    fn create_import_batch(&self, user_id: &str, pairs: &[(String, String)]) -> String {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let id = format!("batch-{}", inner.len() + 1);
        inner.push((user_id.to_owned(), pairs.to_vec()));
        id
    }
}

/// In-memory Lidarr settings rows.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryLidarrSettings {
    inner: Mutex<LidarrConnectionSettings>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryLidarrSettings {
    /// Empty rows.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed raw rows for briefs.
    pub fn seed(&self, url: &str, api_key: &str) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.url = normalize_lidarr_url(url);
        inner.api_key = api_key.to_owned();
    }
}

#[cfg(any(test, feature = "test-support"))]
impl LidarrSettingsStore for MemoryLidarrSettings {
    fn get(&self) -> LidarrConnectionSettings {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        LidarrConnectionSettings {
            url: inner.url.clone(),
            api_key: if inner.api_key.is_empty() {
                String::new()
            } else {
                LIDARR_API_KEY_MASK.to_owned()
            },
        }
    }

    fn get_raw(&self) -> LidarrConnectionSettings {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn save(&self, settings: &LidarrConnectionSettings) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        inner.url = normalize_lidarr_url(&settings.url);
        if settings.api_key != LIDARR_API_KEY_MASK {
            inner.api_key = settings.api_key.clone();
        }
    }
}

/// Read-only Lidarr-to-follows importer (v2 `LidarrImportService`).
pub struct LidarrImportService {
    client: LidarrClient,
    settings: std::sync::Arc<dyn LidarrSettingsStore>,
    follows: std::sync::Arc<dyn FollowStore>,
    approvals: std::sync::Arc<dyn ApprovalSink>,
}

/// Missing-connection failure. The message reaches the user (v2 400).
pub const LIDARR_NOT_CONNECTED: &str =
    "Lidarr is not connected. An admin must configure it in Settings first.";

impl LidarrImportService {
    /// Wire the service from its parts.
    pub fn new(
        client: LidarrClient,
        settings: std::sync::Arc<dyn LidarrSettingsStore>,
        follows: std::sync::Arc<dyn FollowStore>,
        approvals: std::sync::Arc<dyn ApprovalSink>,
    ) -> Self {
        Self {
            client,
            settings,
            follows,
            approvals,
        }
    }

    /// Re-fetch Lidarr and keep monitored artists with a valid MBID.
    async fn monitored_artists(&self) -> Result<Vec<LidarrArtist>, ServiceError> {
        let raw = self.settings.get_raw();
        if raw.url.is_empty() || raw.api_key.is_empty() {
            return Err(ServiceError::NotConnected);
        }
        let artists = self
            .client
            .list_artists(&raw.url, &raw.api_key)
            .await
            .map_err(ServiceError::Lidarr)?;
        Ok(artists
            .into_iter()
            .filter(|artist| artist.monitored && is_valid_mbid(&artist.foreign_artist_id))
            .collect())
    }

    /// Candidate list annotated for one user.
    pub async fn list_candidates(
        &self,
        user_id: &str,
    ) -> Result<LidarrArtistListResponse, ServiceError> {
        let monitored = self.monitored_artists().await?;
        let lowers: Vec<String> = monitored
            .iter()
            .map(|artist| artist.foreign_artist_id.to_lowercase())
            .collect();
        let existing = self.follows.existing_followed_lower(user_id, &lowers);
        let artists: Vec<LidarrArtistCandidate> = monitored
            .iter()
            .map(|artist| {
                let lower = artist.foreign_artist_id.to_lowercase();
                LidarrArtistCandidate {
                    mbid: artist.foreign_artist_id.clone(),
                    name: artist.artist_name.clone(),
                    monitor_new_items: artist.monitor_new_items.clone(),
                    already_following: existing.contains(&lower),
                    would_auto_download: artist.monitor_new_items == "all",
                }
            })
            .collect();
        Ok(LidarrArtistListResponse {
            total: artists.len() as i64,
            artists,
        })
    }

    /// Import a selection into one user's follows (v2 `import_artists`).
    pub async fn import_artists(
        &self,
        user_id: &str,
        is_admin: bool,
        selected_mbids: &[String],
    ) -> Result<LidarrImportResponse, ServiceError> {
        let monitored = self.monitored_artists().await?;
        // Authoritative map from the re-fetch (v2 DR3): a selected MBID not
        // currently monitored in Lidarr is silently ignored.
        let by_lower: HashMap<String, &LidarrArtist> = monitored
            .iter()
            .map(|artist| (artist.foreign_artist_id.to_lowercase(), artist))
            .collect();

        let mut selected_valid_lower: Vec<String> = Vec::new();
        let mut skipped_invalid: i64 = 0;
        let mut seen: HashSet<String> = HashSet::new();
        for mbid in selected_mbids {
            if !is_valid_mbid(mbid) {
                skipped_invalid += 1;
                continue;
            }
            let lower = mbid.trim().to_lowercase();
            if !seen.insert(lower.clone()) {
                continue;
            }
            if by_lower.contains_key(&lower) {
                selected_valid_lower.push(lower);
            }
        }

        // Pre-read existing follows BEFORE any write (v2 DR6): the bulk
        // upsert cannot tell a fresh insert from a conflict, so both the
        // counts and the D9 rule need the prior state.
        let existing = self
            .follows
            .existing_followed_lower(user_id, &selected_valid_lower);
        let new_lowers: Vec<&String> = selected_valid_lower
            .iter()
            .filter(|lower| !existing.contains(*lower))
            .collect();
        let imported = new_lowers.len() as i64;
        let already_following = selected_valid_lower.len() as i64 - imported;
        // D9: mirror auto-download ONLY for brand-new follows monitored `all`.
        let auto_dl_lowers: Vec<&String> = new_lowers
            .into_iter()
            .filter(|lower| {
                by_lower
                    .get(*lower)
                    .is_some_and(|artist| artist.monitor_new_items == "all")
            })
            .collect();

        // Writes, ORDER MATTERS (v2): (a) bulk-follow the whole valid
        // selection, (b) approvals for the auto-download subset BEFORE
        // flipping intent, (c) flip intent. A mid-sequence crash fails safe
        // with intent off rather than intent-on-without-approval.
        let pairs: Vec<(String, String)> = selected_valid_lower
            .iter()
            .filter_map(|lower| {
                by_lower
                    .get(lower)
                    .map(|artist| (artist.foreign_artist_id.clone(), artist.artist_name.clone()))
            })
            .collect();
        self.follows.follow_artists_bulk(user_id, &pairs);
        let mut approval_batch_id: Option<String> = None;
        if !auto_dl_lowers.is_empty() && !is_admin {
            let auto_pairs: Vec<(String, String)> = auto_dl_lowers
                .iter()
                .filter_map(|lower| {
                    by_lower.get(*lower).map(|artist| {
                        (artist.foreign_artist_id.clone(), artist.artist_name.clone())
                    })
                })
                .collect();
            approval_batch_id = Some(self.approvals.create_import_batch(user_id, &auto_pairs));
        }
        let auto_download_enabled = auto_dl_lowers.len() as i64;
        if !auto_dl_lowers.is_empty() {
            let mbids: Vec<String> = auto_dl_lowers
                .iter()
                .filter_map(|lower| by_lower.get(*lower).map(|a| a.foreign_artist_id.clone()))
                .collect();
            self.follows
                .set_auto_download_intent_bulk(user_id, &mbids, true);
        }

        Ok(LidarrImportResponse {
            imported,
            already_following,
            skipped_invalid,
            auto_download_enabled,
            approval_batch_id,
        })
    }
}

/// Service-level failure: missing connection or a Lidarr fetch error.
#[derive(Debug)]
pub enum ServiceError {
    /// No Lidarr connection configured. User-safe 400.
    NotConnected,
    /// Lidarr fetch failed.
    Lidarr(LidarrError),
}
