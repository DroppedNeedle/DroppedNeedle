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
    ApprovalSeedSink, PendingApproval, PendingApprovalsSource, PendingBatch,
};
use crate::reads::collections::store::FollowStore as CollectionsFollowStore;
use crate::runtime_config::secret_sections::{LidarrImportConnection, SpotifySettings};
use crate::runtime_config::{ConfigStore, Masked, Secret};

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
        match self
            .store
            .get_masked::<LidarrImportConnection>()
            .map(Masked::into_inner)
        {
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
        match self
            .store
            .get_masked::<SpotifySettings>()
            .map(Masked::into_inner)
        {
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

/// Imports follow store over the collections follow rows.
pub struct CollectionsFollowBridge {
    follows: CollectionsFollowStore,
}

impl CollectionsFollowBridge {
    /// Bridge over the shared collections follow store.
    pub fn new(follows: CollectionsFollowStore) -> Self {
        Self { follows }
    }
}

impl FollowStore for CollectionsFollowBridge {
    fn existing_followed_lower<'a>(
        &'a self,
        user_id: &'a str,
        candidates: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, String>> {
        Box::pin(async move {
            self.follows
                .followed_among(user_id, candidates)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn follow_artists_bulk<'a>(
        &'a self,
        user_id: &'a str,
        pairs: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let artists = pairs
                .iter()
                .map(|(mbid, name)| (mbid.clone(), Some(name.clone())))
                .collect::<Vec<_>>();
            self.follows
                .follow(user_id, &artists)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn set_auto_download_intent_bulk<'a>(
        &'a self,
        user_id: &'a str,
        mbids: &'a [String],
        intent: bool,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.follows
                .set_intent(user_id, mbids, intent)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
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
    follows: CollectionsFollowStore,
}

impl FollowDecisionBridge {
    /// Sink over the shared collections follow store.
    pub fn new(follows: CollectionsFollowStore) -> Self {
        Self { follows }
    }
}

impl FollowDecisionSink for FollowDecisionBridge {
    fn arm_auto_download<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        artist_name: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.follows
                .arm(user_id, artist_mbid, artist_name)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn clear_auto_download<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.follows
                .set_intent(user_id, &[artist_mbid.to_owned()], false)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
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
