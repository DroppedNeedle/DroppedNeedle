//! Config-backed settings and cross-slice follow bridges.
//!
//! Credentials and URLs live in the stage-2 config store (encrypted at
//! rest, never in plaintext): [`ConfigLidarrSettings`] and
//! [`ConfigSpotifySettings`] implement the imports settings traits over
//! the `lidarr_import` and `spotify_settings` secret sections. The mask
//! sentinels match the imports models exactly, so a masked save preserves
//! the stored secret through the store's positional pairing.
//!
//! The follow bridges share one follow/approval truth across slices:
//!
//! - [`CollectionsFollowBridge`] serves the imports `FollowStore` from
//!   the reads collections follow rows (Lidarr imports land as real
//!   follows instead of slice-local rows).
//! - [`RequestsApprovalBridge`] serves the imports `ApprovalSink` from
//!   the requests approval store (import batches are actionable through
//!   the batch mutations).
//! - [`FollowDecisionBridge`] carries requests approval verdicts back
//!   into the collections follow rows.
//! - [`RequestsPendingSource`] and [`ApprovalSeedBridge`] close the loop
//!   the other way: collections reads share the requests approval store,
//!   and follow toggles file the approvals the admin mutations decide.
//! - [`FlowsWatchBridge`] feeds the wanted view with the flows loop's
//!   watches.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::imports::lidarr::{ApprovalSink, FollowStore, LidarrSettingsStore};
use super::imports::models::{LidarrConnectionSettings, SpotifySettings as ImportsSpotifySettings};
use super::imports::spotify::SpotifySettingsStore;
use super::requests::bridges::{FollowDecisionSink, WatchView, WatchedAlbum};
use super::requests::ledger::{ApprovalBatch, FollowApproval, FollowApprovalStore};
use crate::reads::collections::state::{
    ApprovalSeedSink, AutoDownloadState, FollowRow, FollowStore as CollectionsFollowStore,
    PendingApproval, PendingApprovalsSource, PendingBatch,
};
use crate::runtime_config::secret_sections::{LidarrImportConnection, SpotifySettings};
use crate::runtime_config::{ConfigStore, Secret};

/// Current unix time in epoch seconds.
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0)
}

/// Lidarr connection settings over the `lidarr_import` secret section.
/// Read failures fail closed to empty (the import then reports Lidarr as
/// not connected, exactly like an unconfigured deployment).
pub struct ConfigLidarrSettings {
    store: Arc<ConfigStore>,
}

impl ConfigLidarrSettings {
    /// Settings over a shared config store.
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self { store }
    }
}

impl LidarrSettingsStore for ConfigLidarrSettings {
    fn get(&self) -> LidarrConnectionSettings {
        match self.store.get_masked::<LidarrImportConnection>() {
            Ok(section) => LidarrConnectionSettings {
                url: section.url,
                api_key: section.api_key.expose().to_owned(),
            },
            Err(error) => {
                tracing::warn!(%error, "lidarr settings read failed; treating as unconfigured");
                LidarrConnectionSettings::default()
            }
        }
    }

    fn get_raw(&self) -> LidarrConnectionSettings {
        match self.store.get_raw::<LidarrImportConnection>() {
            Ok(section) => LidarrConnectionSettings {
                url: section.url,
                api_key: section.api_key.expose().to_owned(),
            },
            Err(error) => {
                tracing::warn!(%error, "lidarr settings read failed; treating as unconfigured");
                LidarrConnectionSettings::default()
            }
        }
    }

    fn save(&self, settings: &LidarrConnectionSettings) {
        let incoming = LidarrImportConnection {
            url: settings.url.clone(),
            api_key: Secret::new(settings.api_key.clone()),
        };
        if let Err(error) = self.store.save_secret(incoming) {
            tracing::warn!(%error, "lidarr settings save failed");
        }
    }
}

/// Spotify app settings over the `spotify_settings` secret section. Read
/// failures fail closed to empty (OAuth then reports the app as
/// unconfigured).
pub struct ConfigSpotifySettings {
    store: Arc<ConfigStore>,
}

impl ConfigSpotifySettings {
    /// Settings over a shared config store.
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self { store }
    }
}

impl SpotifySettingsStore for ConfigSpotifySettings {
    fn get(&self) -> ImportsSpotifySettings {
        match self.store.get_masked::<SpotifySettings>() {
            Ok(section) => ImportsSpotifySettings {
                client_id: section.client_id,
                client_secret: section.client_secret.expose().to_owned(),
                enabled: section.enabled,
                spotify_redirect_origin: section.spotify_redirect_origin,
            },
            Err(error) => {
                tracing::warn!(%error, "spotify settings read failed; treating as unconfigured");
                ImportsSpotifySettings::default()
            }
        }
    }

    fn get_raw(&self) -> ImportsSpotifySettings {
        match self.store.get_raw::<SpotifySettings>() {
            Ok(section) => ImportsSpotifySettings {
                client_id: section.client_id,
                client_secret: section.client_secret.expose().to_owned(),
                enabled: section.enabled,
                spotify_redirect_origin: section.spotify_redirect_origin,
            },
            Err(error) => {
                tracing::warn!(%error, "spotify settings read failed; treating as unconfigured");
                ImportsSpotifySettings::default()
            }
        }
    }

    fn save(&self, settings: &ImportsSpotifySettings) -> Result<(), String> {
        let incoming = SpotifySettings {
            client_id: settings.client_id.clone(),
            client_secret: Secret::new(settings.client_secret.clone()),
            enabled: settings.enabled,
            spotify_redirect_origin: settings.spotify_redirect_origin.clone(),
        };
        self.store
            .save_secret(incoming)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// Find a collections follow row by exact then lowercased MBID. The
/// collections slice keys rows verbatim while the acquire slices
/// canonicalize to lowercase; both spellings resolve to one row.
fn find_follow_key(
    rows: &std::collections::HashMap<(String, String), FollowRow>,
    user_id: &str,
    artist_mbid: &str,
) -> Option<(String, String)> {
    let exact = (user_id.to_owned(), artist_mbid.to_owned());
    if rows.contains_key(&exact) {
        return Some(exact);
    }
    let lower = (user_id.to_owned(), artist_mbid.to_lowercase());
    if rows.contains_key(&lower) {
        return Some(lower);
    }
    None
}

/// Imports follow store over the collections follow rows.
pub struct CollectionsFollowBridge {
    follows: Arc<CollectionsFollowStore>,
}

impl CollectionsFollowBridge {
    /// Bridge over the shared collections follow store.
    pub fn new(follows: Arc<CollectionsFollowStore>) -> Self {
        Self { follows }
    }
}

impl FollowStore for CollectionsFollowBridge {
    fn existing_followed_lower(&self, user_id: &str, candidates: &[String]) -> HashSet<String> {
        let rows = match self.follows.follows.read() {
            Ok(rows) => rows,
            Err(_) => return HashSet::new(),
        };
        candidates
            .iter()
            .filter(|candidate| find_follow_key(&rows, user_id, candidate).is_some())
            .map(|candidate| candidate.to_lowercase())
            .collect()
    }

    fn follow_artists_bulk(&self, user_id: &str, pairs: &[(String, String)]) {
        let mut rows = match self.follows.follows.write() {
            Ok(rows) => rows,
            Err(_) => return,
        };
        for (mbid, name) in pairs {
            // The import service carries no display name for the importer,
            // so a fresh follow keeps the user id where the follow toggle
            // would keep the username.
            let key = (user_id.to_owned(), mbid.to_lowercase());
            if find_follow_key(&rows, user_id, mbid).is_none() {
                rows.insert(
                    key,
                    FollowRow {
                        user_id: user_id.to_owned(),
                        user_name: user_id.to_owned(),
                        artist_mbid: mbid.to_lowercase(),
                        artist_name: name.clone(),
                        auto_download: false,
                        auto_download_state: AutoDownloadState::Off,
                        followed_at: now_epoch(),
                        requested_at: None,
                    },
                );
            }
        }
    }

    fn set_auto_download_intent_bulk(&self, user_id: &str, mbids: &[String], intent: bool) {
        let mut rows = match self.follows.follows.write() {
            Ok(rows) => rows,
            Err(_) => return,
        };
        for mbid in mbids {
            let key = find_follow_key(&rows, user_id, mbid);
            if let Some(key) = key
                && let Some(row) = rows.get_mut(&key)
            {
                row.auto_download = intent;
                row.auto_download_state = if intent {
                    AutoDownloadState::Active
                } else {
                    AutoDownloadState::Off
                };
                if intent {
                    row.requested_at = Some(now_epoch());
                }
            }
        }
    }

    fn auto_download_intent(&self, user_id: &str, mbid_lower: &str) -> bool {
        let rows = match self.follows.follows.read() {
            Ok(rows) => rows,
            Err(_) => return false,
        };
        find_follow_key(&rows, user_id, mbid_lower)
            .and_then(|key| rows.get(&key))
            .is_some_and(|row| row.auto_download)
    }
}

/// Imports approval sink over the requests approval store. Batch ids run
/// `batch-{n}` like the memory sink, so fixtures and production agree.
pub struct RequestsApprovalBridge {
    approvals: Arc<FollowApprovalStore>,
    next_batch: AtomicU64,
}

impl RequestsApprovalBridge {
    /// Sink over the shared requests approval store.
    pub fn new(approvals: Arc<FollowApprovalStore>) -> Self {
        Self {
            approvals,
            next_batch: AtomicU64::new(1),
        }
    }
}

impl ApprovalSink for RequestsApprovalBridge {
    fn create_import_batch(&self, user_id: &str, pairs: &[(String, String)]) -> String {
        let id = format!("batch-{}", self.next_batch.fetch_add(1, Ordering::Relaxed));
        self.approvals.seed_batch(ApprovalBatch {
            batch_id: id.clone(),
            user_id: user_id.to_owned(),
            user_name: user_id.to_owned(),
            artists: pairs.to_vec(),
            state: "pending".to_owned(),
            requested_at: now_epoch(),
        });
        id
    }
}

/// Requests verdict sink over the collections follow rows.
pub struct FollowDecisionBridge {
    follows: Arc<CollectionsFollowStore>,
}

impl FollowDecisionBridge {
    /// Sink over the shared collections follow store.
    pub fn new(follows: Arc<CollectionsFollowStore>) -> Self {
        Self { follows }
    }
}

impl FollowDecisionSink for FollowDecisionBridge {
    fn arm_auto_download(
        &self,
        user_id: &str,
        user_name: &str,
        artist_mbid: &str,
        artist_name: &str,
    ) {
        let mut rows = match self.follows.follows.write() {
            Ok(rows) => rows,
            Err(_) => return,
        };
        let now = now_epoch();
        if let Some(key) = find_follow_key(&rows, user_id, artist_mbid) {
            if let Some(row) = rows.get_mut(&key) {
                row.auto_download = true;
                row.auto_download_state = AutoDownloadState::Active;
                row.requested_at = Some(now);
            }
            return;
        }
        rows.insert(
            (user_id.to_owned(), artist_mbid.to_lowercase()),
            FollowRow {
                user_id: user_id.to_owned(),
                user_name: user_name.to_owned(),
                artist_mbid: artist_mbid.to_lowercase(),
                artist_name: artist_name.to_owned(),
                auto_download: true,
                auto_download_state: AutoDownloadState::Active,
                followed_at: now,
                requested_at: Some(now),
            },
        );
    }

    fn clear_auto_download(&self, user_id: &str, artist_mbid: &str) {
        let mut rows = match self.follows.follows.write() {
            Ok(rows) => rows,
            Err(_) => return,
        };
        let key = find_follow_key(&rows, user_id, artist_mbid);
        if let Some(key) = key
            && let Some(row) = rows.get_mut(&key)
        {
            row.auto_download = false;
            row.auto_download_state = AutoDownloadState::Off;
            row.requested_at = None;
        }
    }
}

/// Collections approval reads over the requests approval store.
pub struct RequestsPendingSource {
    approvals: Arc<FollowApprovalStore>,
}

impl RequestsPendingSource {
    /// Source over the shared requests approval store.
    pub fn new(approvals: Arc<FollowApprovalStore>) -> Self {
        Self { approvals }
    }
}

impl PendingApprovalsSource for RequestsPendingSource {
    fn pending_approvals(&self) -> Vec<PendingApproval> {
        match self.approvals.pending() {
            Ok(rows) => rows
                .into_iter()
                .map(|row| PendingApproval {
                    user_id: row.user_id,
                    user_name: row.user_name,
                    artist_mbid: row.artist_mbid,
                    artist_name: row.artist_name,
                    requested_at: row.requested_at,
                })
                .collect(),
            Err(error) => {
                tracing::warn!(?error, "pending approvals read failed");
                Vec::new()
            }
        }
    }

    fn pending_batches(&self) -> Vec<PendingBatch> {
        match self.approvals.pending_batches() {
            Ok(rows) => rows
                .into_iter()
                .map(|batch| PendingBatch {
                    batch_id: batch.batch_id,
                    user_id: batch.user_id,
                    user_name: batch.user_name,
                    artists: batch.artists,
                    requested_at: batch.requested_at,
                })
                .collect(),
            Err(error) => {
                tracing::warn!(?error, "pending batches read failed");
                Vec::new()
            }
        }
    }
}

/// Collections seed sink over the requests approval store.
pub struct ApprovalSeedBridge {
    approvals: Arc<FollowApprovalStore>,
}

impl ApprovalSeedBridge {
    /// Sink over the shared requests approval store.
    pub fn new(approvals: Arc<FollowApprovalStore>) -> Self {
        Self { approvals }
    }
}

impl ApprovalSeedSink for ApprovalSeedBridge {
    fn seed_approval(&self, user_id: &str, user_name: &str, artist_mbid: &str, artist_name: &str) {
        self.approvals.seed_pending(FollowApproval {
            user_id: user_id.to_owned(),
            user_name: user_name.to_owned(),
            artist_mbid: artist_mbid.to_lowercase(),
            artist_name: artist_name.to_owned(),
            state: "pending".to_owned(),
            requested_at: now_epoch(),
        });
    }

    fn withdraw_approval(&self, user_id: &str, artist_mbid: &str) {
        if let Err(error) = self.approvals.withdraw(user_id, artist_mbid) {
            tracing::warn!(?error, "approval withdraw failed");
        }
    }
}

/// Requests watch view over the flows loop's watch registry. The loop
/// tracks next-check times rather than creation times, so creation reads
/// as the next check due.
pub struct FlowsWatchBridge {
    watches: Arc<super::flows::stores::WantedStore>,
}

impl FlowsWatchBridge {
    /// View over the shared flows watch registry.
    pub fn new(watches: Arc<super::flows::stores::WantedStore>) -> Self {
        Self { watches }
    }
}

impl WatchView for FlowsWatchBridge {
    fn watching(&self) -> Vec<WatchedAlbum> {
        self.watches
            .list_due(i64::MAX, usize::MAX)
            .into_iter()
            .map(|watch| {
                let next_check_at = watch.next_check_at.max(0) as u64;
                WatchedAlbum {
                    key: watch.rg_mbid,
                    user_id: watch.user_id,
                    artist_name: watch.artist,
                    album_title: watch.title,
                    created_at: next_check_at,
                    next_check_at,
                }
            })
            .collect()
    }
}
