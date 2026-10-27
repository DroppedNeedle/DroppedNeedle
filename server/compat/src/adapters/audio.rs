//! Player-app audio over the stream gateway. Both protocols translate
//! their parameters into [`StreamParams`] and the gateway makes the one
//! transcode decision; these adapters frame what it opened, by the plan it
//! ran, and map its faults onto each protocol.
//!
//! A file id is tried as a local track first and, on a miss, as a
//! `streaming_source` plugin recording (v2's plugin fallback; a plugin can
//! never shadow a local track). The direct lease the open took moves into
//! the body, so it stays held until the last byte is sent or the client
//! goes away. Leases count against the caller.

use std::sync::Arc;

use futures_util::StreamExt as _;

use crate::body::{AudioBody, LiveBody};
use crate::jellyfin::seams::ByteOutcome;
use crate::subsonic::stream::{
    AudioFacts, BackendError, OpenedAudio, PlayedAudio, TranscodedAudio,
};
use droppedneedle_media::stream::leases::RETRY_AFTER_SECONDS;
use droppedneedle_media::stream::routes::{
    AudioSource, FramedBody, StreamEngine, StreamError, StreamFault, StreamMedia, StreamOpen,
    StreamParams, frame, read_span,
};

/// Open `file_id` for `lease_user`: the local library first, then any
/// plugin that claims the id.
async fn open<E: StreamEngine>(
    engine: &E,
    lease_user: &str,
    file_id: &str,
    params: &StreamParams,
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

/// Subsonic and Jellyfin audio over the gateway, scoped to one lease
/// principal.
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
    /// The adapter scoped to one authenticated caller, so stream leases
    /// count against that user.
    pub fn for_caller(&self, user_id: &str) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
            lease_user: user_id.to_owned(),
        }
    }
}

impl<E: StreamEngine + 'static> GatewayAudio<E> {
    /// Wrap the engine. Leases run under `compat` until a route scopes the
    /// adapter to its authenticated caller.
    pub fn new(engine: Arc<E>) -> Self {
        Self {
            engine,
            lease_user: "compat".to_owned(),
        }
    }
}

/// Map a gateway fault onto the Subsonic backend error. The message is
/// shown to clients inside a 200 envelope, so internal causes are logged
/// here and replaced by the fixed server-fault text.
fn backend_error(fault: StreamFault) -> BackendError {
    match fault {
        StreamFault::Capacity => BackendError::full(),
        StreamFault::NotFound => BackendError::failed("audio item not found"),
        StreamFault::Forbidden { message } => BackendError::forbidden(message),
        StreamFault::InvalidInput { message } => BackendError::failed(message),
        StreamFault::Upstream { source, cause } => {
            tracing::warn!(source = source.as_str(), %cause, "subsonic audio upstream failed");
            BackendError::failed(droppedneedle_kernel::error::FIXED_INTERNAL_MESSAGE)
        }
        StreamFault::Internal { cause } => {
            tracing::error!(%cause, "subsonic audio failed");
            BackendError::failed(droppedneedle_kernel::error::FIXED_INTERNAL_MESSAGE)
        }
    }
}

impl<E: StreamEngine + 'static> GatewayAudio<E> {
    /// Open one file id for Subsonic as `params` ask: the gateway decides
    /// whether the original or a transcode plays. `head_only` needs the
    /// facts alone and never starts a transcode. `None` when nothing can
    /// serve the track.
    pub async fn play_subsonic(
        &self,
        file_id: &str,
        params: &StreamParams,
        head_only: bool,
    ) -> Result<Option<PlayedAudio>, BackendError> {
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
            Err(StreamFault::NotFound) => return Ok(None),
            Err(fault) => return Err(backend_error(fault)),
        };
        if media.transcoded {
            let content_type = media.content_type.clone();
            let estimated_len = media
                .estimated_len
                .filter(|_| params.estimate_content_length);
            let body = frame(media, None, head_only, false).map(|framed| framed.body);
            return Ok(Some(PlayedAudio::Transcoded(TranscodedAudio {
                content_type,
                estimated_len,
                body: audio_body(body),
            })));
        }
        let StreamMedia {
            content_type,
            total_len,
            body,
            lease,
            ..
        } = media;
        let facts = AudioFacts {
            size: total_len,
            content_type,
        };
        Ok(Some(PlayedAudio::Original(OpenedAudio::new(
            facts,
            move |start, len| audio_body(read_span(body, lease, start, len)),
        ))))
    }
}

/// A status-only Jellyfin outcome.
fn status(code: u16, headers: Vec<(String, String)>) -> ByteOutcome {
    ByteOutcome {
        status: code,
        headers,
        body: AudioBody::empty(),
    }
}

/// Map a gateway fault onto Jellyfin's status-only answers: unknown ids
/// 404, refusals 403, a bad format 400, a full pool 429 with
/// `Retry-After`, and anything else a logged 500, which the request
/// boundary answers in Jellyfin's failure shape.
fn jellyfin_fault(fault: StreamFault) -> ByteOutcome {
    match fault {
        StreamFault::NotFound => status(404, Vec::new()),
        StreamFault::Forbidden { .. } => status(403, Vec::new()),
        StreamFault::InvalidInput { .. } => status(400, Vec::new()),
        StreamFault::Capacity => status(
            429,
            vec![("Retry-After".to_owned(), RETRY_AFTER_SECONDS.to_string())],
        ),
        StreamFault::Upstream { source, cause } => {
            tracing::warn!(source = source.as_str(), %cause, "jellyfin audio upstream failed");
            status(500, Vec::new())
        }
        StreamFault::Internal { cause } => {
            tracing::error!(%cause, "jellyfin audio failed");
            status(500, Vec::new())
        }
    }
}

impl<E: StreamEngine + 'static> GatewayAudio<E> {
    /// Play one file id for Jellyfin as `params` ask, framed by the plan
    /// the gateway ran: the original with the 200/206/416 contract, or a
    /// transcode, which never carries a Content-Length and streams
    /// unsized. A 416 carries the file's type, `Content-Range: bytes */N`
    /// and `Accept-Ranges`. `head_only` answers the same status and headers
    /// with an empty body and never starts a transcode. Unknown ids are
    /// 404, refusals 403, a full pool 429 with `Retry-After`, and failures
    /// 500.
    pub async fn play_jellyfin(
        &self,
        file_id: &str,
        params: &StreamParams,
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
            Err(fault) => return jellyfin_fault(fault),
        };
        let content_type = media.content_type.clone();
        match frame(media, range, head_only, false) {
            Ok(framed) => ByteOutcome {
                status: framed.status.as_u16(),
                headers: framed
                    .headers
                    .iter()
                    .filter_map(|(name, value)| {
                        Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
                    })
                    .collect(),
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
                status(500, Vec::new())
            }
        }
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
