//! Stream-gateway remote reads over the unified adapter.
//!
//! [`RemotesRemoteReader`] implements the gateway's [`RemoteReader`] seam:
//! it resolves the caller's stored connection for the requested source,
//! builds that source's adapter, and fetches whole audio bytes. Folder
//! scoping does not apply (byte reads address ids, not catalogs), and
//! server-side transcode stays a later-stage concern, so transcode hints
//! never reach this layer.

use std::sync::Arc;

use crate::stream::gateway::{RemoteMedia, RemoteReader};
use crate::stream::routes::{AudioSource, StreamFault};

use super::adapter::AdapterError;
use super::connections::{ConnectionStore, CredentialCoder, ResolveError, resolve_connection};
use super::jellyfin::JellyfinAdapter;
use super::models::SourceName;
use super::navidrome::NavidromeAdapter;
use super::plex::PlexAdapter;

/// Gateway remote reads over per-user stored connections.
pub struct RemotesRemoteReader {
    http: reqwest::Client,
    connections: Arc<dyn ConnectionStore>,
    coder: Arc<CredentialCoder>,
}

impl RemotesRemoteReader {
    /// Build a reader over the shared client, connection rows, and coder.
    pub fn new(
        http: reqwest::Client,
        connections: Arc<dyn ConnectionStore>,
        coder: Arc<CredentialCoder>,
    ) -> Self {
        Self {
            http,
            connections,
            coder,
        }
    }
}

impl RemoteReader for RemotesRemoteReader {
    async fn fetch(
        &self,
        source: AudioSource,
        key: &str,
        user_id: &str,
    ) -> Result<RemoteMedia, StreamFault> {
        let name = match source {
            AudioSource::Local => {
                // The gateway serves local reads itself and never routes
                // them here; reaching this is a programming error.
                return Err(StreamFault::Internal {
                    cause: "local read reached the remote reader".to_owned(),
                });
            }
            AudioSource::Jellyfin => SourceName::Jellyfin,
            AudioSource::Navidrome => SourceName::Navidrome,
            AudioSource::Plex => SourceName::Plex,
        };
        let resolved = resolve_connection(self.connections.as_ref(), &self.coder, user_id, name)
            .await
            .map_err(|error| match error {
                // Without a stored connection the item is unresolvable,
                // which reads as unknown rather than forbidden.
                ResolveError::NotConfigured => StreamFault::NotFound,
                ResolveError::Stale => StreamFault::Upstream { source },
            })?;
        let handle = match name {
            SourceName::Jellyfin => super::adapter::RemoteHandle::Jellyfin(JellyfinAdapter::new(
                self.http.clone(),
                resolved.base_url,
                resolved.credential,
                resolved.user_id,
            )),
            SourceName::Navidrome => {
                super::adapter::RemoteHandle::Navidrome(NavidromeAdapter::new(
                    self.http.clone(),
                    resolved.base_url,
                    resolved.username,
                    resolved.credential,
                ))
            }
            SourceName::Plex => super::adapter::RemoteHandle::Plex(PlexAdapter::new(
                self.http.clone(),
                resolved.base_url,
                resolved.credential,
                resolved.client_id,
                resolved.section_ids,
            )),
        };
        let (bytes, content_type) = handle
            .audio_bytes(key)
            .await
            .map_err(|error| map_fault(source, error))?;
        Ok(RemoteMedia {
            content_type,
            bytes,
        })
    }
}

/// Map an adapter failure onto the gateway fault spelling. Upstream detail
/// stays in the log (callers log through `StreamError::internal`); only
/// the invalid-key message is user-facing.
fn map_fault(source: AudioSource, error: AdapterError) -> StreamFault {
    match error {
        AdapterError::NotConfigured | AdapterError::NotFound => StreamFault::NotFound,
        AdapterError::Auth | AdapterError::Api(_) | AdapterError::Transport(_) => {
            StreamFault::Upstream { source }
        }
        AdapterError::Unsupported(message) => StreamFault::InvalidInput { message },
    }
}
