//! Stream-gateway remote reads over the unified adapter.
//!
//! [`RemotesRemoteReader`] implements the gateway's [`RemoteReader`] seam:
//! it resolves the caller's stored connection for the requested source,
//! builds that source's adapter, and streams the audio by range (or reads
//! it whole for the compat whole-object path). Folder
//! scoping does not apply (byte reads address ids, not catalogs), and
//! server-side transcode is not supported for remotes, so transcode hints
//! never reach this layer.

use std::sync::Arc;

use crate::stream::gateway::{RemoteMedia, RemoteReader};
use crate::stream::ranged;
use crate::stream::routes::{AudioSource, MediaBody, StreamFault, StreamMedia};

use super::adapter::AdapterError;
use super::connections::{
    ConnectionResolver, ConnectionStore, CredentialCoder, NoServers, ResolveError,
};
use super::jellyfin::JellyfinAdapter;
use super::models::SourceName;
use super::navidrome::NavidromeAdapter;
use super::plex::PlexAdapter;

/// Gateway remote reads over the caller's connection (own link first,
/// then the admin's shared account, like every other remote read).
pub struct RemotesRemoteReader {
    http: reqwest::Client,
    resolver: Arc<ConnectionResolver>,
}

impl RemotesRemoteReader {
    /// A reader with no admin servers configured: every remote read is
    /// unknown. For builds that never stream remote media.
    pub fn new(
        http: reqwest::Client,
        connections: Arc<dyn ConnectionStore>,
        coder: Arc<CredentialCoder>,
    ) -> Self {
        Self::with_resolver(
            http,
            Arc::new(ConnectionResolver::new(
                connections,
                coder,
                Arc::new(NoServers),
            )),
        )
    }

    /// A reader over the shared resolver.
    pub fn with_resolver(http: reqwest::Client, resolver: Arc<ConnectionResolver>) -> Self {
        Self { http, resolver }
    }
}

impl RemotesRemoteReader {
    /// The caller's adapter for one remote source.
    async fn handle(
        &self,
        source: AudioSource,
        user_id: &str,
    ) -> Result<super::adapter::RemoteHandle, StreamFault> {
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
            AudioSource::Plugin => return Err(StreamFault::NotFound),
        };
        let resolved = self
            .resolver
            .resolve(user_id, name)
            .await
            .map_err(|error| match error {
                // Without a usable connection the item is unresolvable,
                // which reads as unknown rather than forbidden.
                ResolveError::NotConfigured => StreamFault::NotFound,
                ResolveError::Stale => StreamFault::Upstream { source },
                ResolveError::Store(cause) => StreamFault::Internal { cause },
            })?;
        Ok(match name {
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
        })
    }

    /// Resolve and send the audio request for one key. `head_only` asks
    /// with HEAD first and falls back to GET (dropped after the headers)
    /// for a server that does not answer HEAD with a size.
    async fn audio_response(
        &self,
        source: AudioSource,
        key: &str,
        user_id: &str,
        head_only: bool,
    ) -> Result<(reqwest::Response, Option<reqwest::Request>), StreamFault> {
        let request = self
            .handle(source, user_id)
            .await?
            .audio_request(key)
            .map_err(|error| map_fault(source, error))?;
        let again = request.try_clone();
        if head_only && let Some(mut head) = request.try_clone() {
            *head.method_mut() = reqwest::Method::HEAD;
            match send(&self.http, head).await {
                Ok(response) if response.content_length().is_some_and(|len| len > 0) => {
                    return Ok((response, again));
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(%error, source = source.as_str(), "remote HEAD failed; asking with GET");
                }
            }
        }
        let response = send(&self.http, request)
            .await
            .map_err(|error| map_fault(source, error))?;
        Ok((response, again))
    }
}

impl RemoteReader for RemotesRemoteReader {
    async fn fetch(
        &self,
        source: AudioSource,
        key: &str,
        user_id: &str,
    ) -> Result<RemoteMedia, StreamFault> {
        let (response, _) = self.audio_response(source, key, user_id, false).await?;
        let content_type = content_type(&response);
        let bytes = response.bytes().await.map_err(|cause| {
            tracing::warn!(cause = %cause.without_url(), source = source.as_str(), "remote audio read failed");
            StreamFault::Upstream { source }
        })?;
        Ok(RemoteMedia {
            content_type,
            bytes: bytes.to_vec(),
        })
    }

    async fn open_stream(
        &self,
        source: AudioSource,
        key: &str,
        user_id: &str,
        head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        let (response, again) = self.audio_response(source, key, user_id, head_only).await?;
        let content_type = content_type(&response);
        let sized = response.content_length().filter(|len| *len > 0);
        let media = match (sized, again) {
            // A sized answer streams by range, so players can seek.
            (Some(total), Some(again)) => StreamMedia {
                content_type,
                total_len: total,
                transcoded: false,
                estimated_len: None,
                body: if head_only {
                    MediaBody::Empty
                } else {
                    MediaBody::Spans(ranged::spans(
                        response,
                        ranged::response_chunks,
                        range_reopen(self.http.clone(), again),
                    ))
                },
                lease: None,
            },
            // No size: streamed whole and unranged, like a transcode.
            _ => StreamMedia {
                content_type,
                total_len: 0,
                transcoded: true,
                estimated_len: None,
                body: if head_only {
                    MediaBody::Empty
                } else {
                    MediaBody::Chunks(ranged::response_chunks(response))
                },
                lease: None,
            },
        };
        Ok(media)
    }
}

/// How long a remote server may take to start answering an audio request.
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Send one audio request and require a success status. `http` is the
/// remote-audio client (connect timeout only, no redirects), so only the
/// start of the answer is timed here, by [`SEND_TIMEOUT`]; the body is
/// paced by [`ranged::IDLE_TIMEOUT`] per chunk, so a long track is never
/// cut off mid-play. A redirect is refused as an error status, so a
/// server token never goes to another host.
async fn send(
    http: &reqwest::Client,
    request: reqwest::Request,
) -> Result<reqwest::Response, AdapterError> {
    let response = tokio::time::timeout(SEND_TIMEOUT, http.execute(request))
        .await
        .map_err(|_| AdapterError::Transport("the server did not answer in time".to_owned()))?
        .map_err(|cause| {
            AdapterError::Transport(cause.without_url().to_string().chars().take(200).collect())
        })?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(AdapterError::Auth);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(AdapterError::NotFound);
    }
    if !status.is_success() {
        return Err(AdapterError::Api(format!(
            "audio request failed ({status})"
        )));
    }
    Ok(response)
}

/// Re-send the audio request with a `Range` header.
fn range_reopen(http: reqwest::Client, request: reqwest::Request) -> ranged::RangeOpen {
    Box::new(move |range| {
        Box::pin(async move {
            let mut request = request;
            let value = reqwest::header::HeaderValue::from_str(&range)
                .map_err(|error| error.to_string())?;
            request.headers_mut().insert(reqwest::header::RANGE, value);
            send(&http, request)
                .await
                .map_err(|error| error.to_string())
        })
    })
}

/// The upstream content type, or `audio/mpeg` when it names none.
fn content_type(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("audio/mpeg")
        .to_owned()
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
