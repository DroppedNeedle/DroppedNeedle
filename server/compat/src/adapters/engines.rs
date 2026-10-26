//! Compat streaming over the stream engine: both protocol seams call the
//! same [`StreamEngine`](droppedneedle_media::stream::routes::StreamEngine) the native
//! routes use, so leases and transcode execution are shared.
//!
//! Each request opens the engine once with `open_stream`. A direct file is
//! read by range as the response streams, a transcode streams ffmpeg's
//! output chunk by chunk, and HEAD answers from file metadata without
//! reading a byte or starting ffmpeg. The direct lease the open took moves
//! into the body, so it stays held until the last byte is sent or the
//! client goes away. Ranges, lengths and the 200/206/416 verdict come from
//! the stream routes' framing, the same as the native route; each protocol
//! only renders its own errors.
//!
//! Compat file ids pass through as local stream keys. Unknown ids
//! fail exactly like native unknown ids (404/70), and exhausted leases
//! surface as 429 + `Retry-After: 1`.

use std::sync::Arc;

use axum::http::HeaderMap;
use futures_util::StreamExt as _;

use crate::body::{AudioBody, LiveBody};
use crate::jellyfin::seams::{ByteOutcome, StreamEngine as JellyfinStreamEngine};
use crate::subsonic::stream::{AudioBackend, AudioFacts, BackendError, OpenedAudio, StreamPlan};
use droppedneedle_media::stream::leases::RETRY_AFTER_SECONDS;
use droppedneedle_media::stream::routes::{
    AudioSource, FramedBody, StreamEngine, StreamError, StreamFault, StreamMedia, StreamOpen,
    StreamParams, extension_for_content_type, frame, read_span,
};

/// Audio facts for one opened object (direct-file facts only: the
/// engine reports no source bitrate or duration at this layer).
fn facts_for(media: &StreamMedia) -> AudioFacts {
    AudioFacts {
        size: media.total_len,
        suffix: extension_for_content_type(&media.content_type)
            .unwrap_or("bin")
            .to_owned(),
        bitrate_kbps: None,
        duration_seconds: None,
    }
}

/// A framed body as a protocol body. A body that could not be read fails
/// as a stream, so the client sees a cut-off answer, never a short one.
fn audio_body(body: Result<FramedBody, StreamError>) -> AudioBody {
    match body {
        Ok(FramedBody::Empty) => AudioBody::empty(),
        Ok(FramedBody::Bytes(bytes)) => AudioBody::Bytes(bytes),
        Ok(FramedBody::Stream(chunks)) => AudioBody::Live(LiveBody::new(chunks)),
        Err(error) => {
            tracing::error!(?error, "compat audio body could not be read");
            let failed = futures_util::stream::once(async {
                Err(std::io::Error::other("audio body could not be read"))
            })
            .boxed();
            AudioBody::Live(LiveBody::new(failed))
        }
    }
}

/// Framing headers as protocol header pairs.
fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
        })
        .collect()
}

/// Map an engine fault onto the Subsonic backend error. The message is
/// shown to clients inside a 200 envelope, so internal causes are logged
/// here and replaced by the fixed server-fault text.
fn backend_error(fault: StreamFault) -> BackendError {
    match fault {
        StreamFault::Capacity => BackendError::full(),
        StreamFault::NotFound => BackendError::failed("audio item not found"),
        StreamFault::Forbidden { message } => BackendError::failed(message),
        StreamFault::InvalidInput { message } => BackendError::failed(message),
        StreamFault::Upstream { source, cause } => {
            tracing::warn!(%cause, "compat audio upstream failed");
            BackendError::failed(format!("upstream {} failed", source.as_str()))
        }
        StreamFault::Internal { cause } => {
            tracing::error!(%cause, "compat audio backend failed");
            BackendError::failed(droppedneedle_kernel::error::FIXED_INTERNAL_MESSAGE)
        }
    }
}

/// One open for a lease principal: the local library first, then, on a
/// miss, any `streaming_source` plugin that claims the id (v2's plugin
/// fallback; a plugin can never shadow a local track).
async fn open<E: StreamEngine>(
    engine: &E,
    lease_user: &str,
    file_id: &str,
    params: StreamParams,
    head_only: bool,
) -> Result<StreamMedia, StreamFault> {
    let request = |source| StreamOpen {
        source,
        key: file_id.to_owned(),
        user_id: lease_user.to_owned(),
        params: params.clone(),
    };
    match engine
        .open_stream(request(AudioSource::Local), head_only)
        .await
    {
        Err(StreamFault::NotFound) => {
            engine
                .open_stream(request(AudioSource::Plugin), head_only)
                .await
        }
        other => other,
    }
}

/// Engine params for a compat transcode verdict.
fn transcode_params(format: Option<String>, bitrate_kbps: Option<i64>, start: f64) -> StreamParams {
    StreamParams {
        format,
        max_bitrate_kbps: bitrate_kbps,
        estimate_content_length: false,
        start_seconds: start,
        force_transcode: true,
    }
}

/// Subsonic audio backend over any stream engine.
pub struct GatewayAudio<E> {
    engine: Arc<E>,
    lease_user: String,
}

impl<E> Clone for GatewayAudio<E> {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            lease_user: self.lease_user.clone(),
        }
    }
}

impl<E> GatewayAudio<E> {
    /// Wrap the engine. Leases run under `compat:subsonic` until the
    /// dispatcher scopes the backend to the caller.
    pub fn new(engine: Arc<E>) -> Self {
        Self {
            engine,
            lease_user: "compat:subsonic".to_owned(),
        }
    }
}

impl<E: StreamEngine + 'static> AudioBackend for GatewayAudio<E> {
    fn for_caller(&self, user_id: &str) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            lease_user: user_id.to_owned(),
        }
    }

    async fn audio_facts(&self, file_id: &str) -> Result<Option<AudioFacts>, BackendError> {
        let opened = open(
            self.engine.as_ref(),
            &self.lease_user,
            file_id,
            StreamParams::default(),
            true,
        )
        .await;
        match opened {
            Ok(media) => Ok(Some(facts_for(&media))),
            Err(StreamFault::NotFound) => Ok(None),
            Err(fault) => Err(backend_error(fault)),
        }
    }

    async fn open_original(&self, file_id: &str) -> Result<Option<OpenedAudio>, BackendError> {
        let opened = open(
            self.engine.as_ref(),
            &self.lease_user,
            file_id,
            StreamParams::default(),
            false,
        )
        .await;
        match opened {
            Ok(media) => {
                let facts = facts_for(&media);
                let StreamMedia { body, lease, .. } = media;
                Ok(Some(OpenedAudio::new(facts, move |start, len| {
                    audio_body(read_span(body, lease, start, len))
                })))
            }
            Err(StreamFault::NotFound) => Ok(None),
            Err(fault) => Err(backend_error(fault)),
        }
    }

    async fn transcode(
        &self,
        file_id: &str,
        plan: &StreamPlan,
    ) -> Result<(AudioBody, String), BackendError> {
        let params = transcode_params(
            plan.out_format.clone(),
            plan.out_bitrate_kbps,
            plan.start_seconds,
        );
        let media = open(
            self.engine.as_ref(),
            &self.lease_user,
            file_id,
            params,
            false,
        )
        .await
        .map_err(backend_error)?;
        let content_type = media.content_type.clone();
        let body = frame(media, None, false, false).map(|framed| framed.body);
        Ok((audio_body(body), content_type))
    }
}

/// Jellyfin stream engine over any stream engine.
pub struct GatewayStream<E> {
    engine: Arc<E>,
    lease_user: String,
}

impl<E> Clone for GatewayStream<E> {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            lease_user: self.lease_user.clone(),
        }
    }
}

/// A status-only outcome.
fn status(code: u16, headers: Vec<(String, String)>) -> ByteOutcome {
    ByteOutcome {
        status: code,
        headers,
        body: AudioBody::empty(),
    }
}

impl<E: StreamEngine + 'static> GatewayStream<E> {
    /// Wrap the engine. Leases run under `compat:jellyfin` until the audio
    /// routes scope the engine to the authenticated caller.
    pub fn new(engine: Arc<E>) -> Self {
        Self {
            engine,
            lease_user: "compat:jellyfin".to_owned(),
        }
    }

    /// Open and frame one object, rendering failures in Jellyfin's shapes:
    /// 416 with the file's type, `Content-Range: bytes */N` and
    /// `Accept-Ranges`; 429 with `Retry-After`; anything else 404.
    async fn framed(
        &self,
        file_id: &str,
        params: StreamParams,
        range: Option<&str>,
        head_only: bool,
    ) -> ByteOutcome {
        let media = match open(
            self.engine.as_ref(),
            &self.lease_user,
            file_id,
            params,
            head_only,
        )
        .await
        {
            Ok(media) => media,
            Err(StreamFault::Capacity) => {
                return status(
                    429,
                    vec![("Retry-After".to_owned(), RETRY_AFTER_SECONDS.to_string())],
                );
            }
            Err(StreamFault::Upstream { source, cause }) => {
                tracing::warn!(source = source.as_str(), %cause, "jellyfin audio upstream failed");
                return status(404, Vec::new());
            }
            Err(StreamFault::Internal { cause }) => {
                tracing::error!(%cause, "jellyfin audio backend failed");
                return status(404, Vec::new());
            }
            Err(_) => return status(404, Vec::new()),
        };
        let content_type = media.content_type.clone();
        match frame(media, range, head_only, false) {
            Ok(framed) => ByteOutcome {
                status: framed.status.as_u16(),
                headers: header_pairs(&framed.headers),
                body: audio_body(Ok(framed.body)),
            },
            Err(StreamError::Unsatisfiable { total_len }) => status(
                416,
                vec![
                    ("Content-Type".to_owned(), content_type),
                    ("Content-Range".to_owned(), format!("bytes */{total_len}")),
                    ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                ],
            ),
            Err(error) => {
                tracing::error!(?error, "jellyfin audio could not be framed");
                status(404, Vec::new())
            }
        }
    }
}

impl<E: StreamEngine + 'static> JellyfinStreamEngine for GatewayStream<E> {
    fn for_caller(&self, user_id: &str) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            lease_user: user_id.to_owned(),
        }
    }

    async fn direct(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        self.framed(file_id, StreamParams::default(), range, false)
            .await
    }

    async fn head(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        self.framed(file_id, StreamParams::default(), range, true)
            .await
    }

    /// Estimate off on Jellyfin: a transcode never carries a
    /// Content-Length.
    async fn transcode(
        &self,
        file_id: &str,
        format: &str,
        bitrate_kbps: u32,
        start_seconds: f64,
    ) -> ByteOutcome {
        let params = transcode_params(
            Some(format.to_owned()),
            Some(i64::from(bitrate_kbps)),
            start_seconds,
        );
        self.framed(file_id, params, None, false).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_causes_never_reach_the_client() {
        let error = backend_error(StreamFault::Internal {
            cause: "open /srv/music/secret.flac: permission denied".to_owned(),
        });
        assert_eq!(
            error.to_string(),
            droppedneedle_kernel::error::FIXED_INTERNAL_MESSAGE
        );
    }
}
