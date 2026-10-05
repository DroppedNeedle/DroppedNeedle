//! Config-backed settings and the follow bridges between modules.
//!
//! Credentials and URLs live in the config store (encrypted at
//! rest, never in plaintext): [`ConfigLidarrSettings`] and
//! [`ConfigSpotifySettings`] implement the imports settings traits over
//! the `lidarr_import` and `spotify_settings` secret sections. The mask
//! sentinels match the imports models exactly, so a masked save preserves
//! the stored secret through the store's positional pairing.
//!
//! The follow bridges share one follow/approval truth across modules:
//!
//! - [`CollectionsFollowBridge`] serves the imports `FollowStore` from
//!   the reads collections follow rows (Lidarr imports land as real
//!   follows instead of rows private to the imports code).
//! - [`RequestsApprovalBridge`] serves the imports `ApprovalSink` from
//!   the requests approval store (import batches are actionable through
//!   the batch mutations).
//! - [`FollowDecisionBridge`] carries requests approval verdicts back
//!   into the collections follow rows.
//! - [`RequestsPendingSource`] and [`ApprovalSeedBridge`] close the loop
//!   the other way: collections reads share the requests approval store,
//!   and follow toggles file the approvals the admin mutations decide.

use std::collections::HashSet;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::db::now_epoch;
use super::imports::lidarr::{ApprovalSink, FollowStore, LidarrSettingsStore};
use super::imports::models::{LidarrConnectionSettings, SpotifySettings as ImportsSpotifySettings};
use super::imports::spotify::SpotifySettingsStore;
use super::requests::bridges::FollowDecisionSink;
use super::requests::sqlite::FollowApprovalStore;
use crate::reads::collections::state::{
    ApprovalSeedSink, AutoDownloadState, FollowRow, FollowStore as CollectionsFollowStore,
    PendingApproval, PendingApprovalsSource, PendingBatch,
};
use crate::runtime_config::secret_sections::{LidarrImportConnection, SpotifySettings};
use crate::runtime_config::{ConfigStore, Secret};

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
/// collections code keys rows verbatim while the acquire code
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

/// Imports approval sink over the requests approval store.
pub struct RequestsApprovalBridge {
    approvals: FollowApprovalStore,
}

impl RequestsApprovalBridge {
    /// Sink over the shared requests approval store.
    pub fn new(approvals: FollowApprovalStore) -> Self {
        Self { approvals }
    }
}

impl ApprovalSink for RequestsApprovalBridge {
    fn create_import_batch<'a>(
        &'a self,
        user_id: &'a str,
        pairs: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let id = format!("batch-{}", uuid::Uuid::new_v4().simple());
            self.approvals
                .create_batch(&id, user_id, pairs, "lidarr_import", now_epoch())
                .await
                .map_err(|error| format!("{error:?}"))?;
            Ok(id)
        })
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
            Err(error) => {
                tracing::warn!(%error, "follow rows lock failed; auto-download not armed");
                return;
            }
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
            Err(error) => {
                tracing::warn!(%error, "follow rows lock failed; auto-download not cleared");
                return;
            }
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
    approvals: FollowApprovalStore,
}

impl RequestsPendingSource {
    /// Source over the shared requests approval store.
    pub fn new(approvals: FollowApprovalStore) -> Self {
        Self { approvals }
    }
}

impl PendingApprovalsSource for RequestsPendingSource {
    fn pending_approvals(&self) -> BoxFuture<'_, Result<Vec<PendingApproval>, String>> {
        Box::pin(async move {
            let rows = self
                .approvals
                .pending()
                .await
                .map_err(|error| format!("{error:?}"))?;
            Ok(rows
                .into_iter()
                .map(|row| PendingApproval {
                    user_id: row.user_id,
                    user_name: row.user_name,
                    artist_mbid: row.artist_mbid,
                    artist_name: row.artist_name,
                    requested_at: row.requested_at,
                })
                .collect())
        })
    }

    fn pending_batches(&self) -> BoxFuture<'_, Result<Vec<PendingBatch>, String>> {
        Box::pin(async move {
            let rows = self
                .approvals
                .pending_batches()
                .await
                .map_err(|error| format!("{error:?}"))?;
            Ok(rows
                .into_iter()
                .map(|batch| PendingBatch {
                    batch_id: batch.batch_id,
                    user_id: batch.user_id,
                    user_name: batch.user_name,
                    artists: batch.artists,
                    requested_at: batch.requested_at,
                })
                .collect())
        })
    }
}

/// Collections seed sink over the requests approval store.
pub struct ApprovalSeedBridge {
    approvals: FollowApprovalStore,
}

impl ApprovalSeedBridge {
    /// Sink over the shared requests approval store.
    pub fn new(approvals: FollowApprovalStore) -> Self {
        Self { approvals }
    }
}

impl ApprovalSeedSink for ApprovalSeedBridge {
    fn seed_approval<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        artist_name: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.approvals
                .file_pending(user_id, artist_mbid, artist_name, now_epoch())
                .await
                .map_err(|error| format!("{error:?}"))
        })
    }

    fn withdraw_approval<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.approvals
                .withdraw(user_id, artist_mbid)
                .await
                .map(|_| ())
                .map_err(|error| format!("{error:?}"))
        })
    }
}
