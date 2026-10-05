//! Compat streaming over the stream engine: both protocol seams call the
//! same [`StreamEngine`](crate::stream::routes::StreamEngine) the native
//! routes use, so leases and transcode execution are shared.
//!
//! Each request opens the engine once with `open_stream`. A direct file is
//! read by range as the response streams, a transcode streams ffmpeg's
//! output chunk by chunk, and HEAD answers from file metadata without
//! reading a byte or starting ffmpeg. The direct lease the open took moves
//! into the body, so it stays held until the last byte is sent or the
//! client goes away. Range rules stay in each protocol layer.
//!
//! Compat file ids pass through as local stream keys. Unknown ids
//! fail exactly like native unknown ids (404/70), and exhausted leases
//! surface as 429 + `Retry-After: 1`.

use std::sync::Arc;

use crate::compat::body::{AudioBody, LiveBody};
use crate::compat::jellyfin::seams::{ByteOutcome, StreamEngine as JellyfinStreamEngine};
use crate::compat::subsonic::stream::{
    AudioBackend, AudioFacts, BackendError, OpenedAudio, StreamPlan,
};
use crate::stream::leases::OwnedDirectLease;
use crate::stream::routes::{
    AudioSource, MediaBody, StreamEngine, StreamFault, StreamMedia, StreamOpen, StreamParams,
    file_range,
};

/// Suffix for an engine-resolved content type (direct-file facts only).
fn suffix_for_content_type(content_type: &str) -> String {
    let suffix = match content_type {
        "audio/flac" => "flac",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/mp4" => "m4a",
        "audio/aac" => "aac",
        "audio/wav" => "wav",
        "audio/x-ms-wma" => "wma",
        "audio/opus" => "opus",
        _ => "bin",
    };
    suffix.to_owned()
}

/// Audio facts for one opened object (direct-file facts only: the
/// engine reports no source bitrate or duration at this layer).
fn facts_for(media: &StreamMedia) -> AudioFacts {
    AudioFacts {
        size: media.total_len,
        suffix: suffix_for_content_type(&media.content_type),
        bitrate_kbps: None,
        duration_seconds: None,
    }
}

/// `len` bytes from `start` of an opened body. File spans and transcode
/// chunks stream with the lease inside; in-memory bytes are sliced (the
/// lease then ends with the open, as the bytes are already read).
fn span(body: MediaBody, lease: Option<OwnedDirectLease>, start: u64, len: u64) -> AudioBody {
    match body {
        MediaBody::File(path) => {
            AudioBody::Live(LiveBody::new(file_range(path, start, len), lease))
        }
        MediaBody::Chunks(chunks) => AudioBody::Live(LiveBody::new(chunks, lease)),
        MediaBody::Bytes(bytes) => {
            let from = usize::try_from(start)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let to = from
                .saturating_add(usize::try_from(len).unwrap_or(usize::MAX))
                .min(bytes.len());
            AudioBody::Bytes(bytes[from..to].to_vec())
        }
        MediaBody::Empty => AudioBody::empty(),
    }
}

/// The whole of an opened body.
fn whole(media: StreamMedia) -> AudioBody {
    let len = if media.transcoded {
        u64::MAX
    } else {
        media.total_len
    };
    span(media.body, media.lease, 0, len)
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
        StreamFault::Upstream { source } => {
            BackendError::failed(format!("upstream {} failed", source.as_str()))
        }
        StreamFault::Internal { cause } => {
            tracing::error!(%cause, "compat audio backend failed");
            BackendError::failed(crate::error::FIXED_INTERNAL_MESSAGE)
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
                    span(body, lease, start, len)
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
        Ok((whole(media), content_type))
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

/// Headers of a whole transcode landing: never ranged, never sized.
fn landing_headers(content_type: String) -> Vec<(String, String)> {
    vec![
        ("Content-Type".to_owned(), content_type),
        ("Accept-Ranges".to_owned(), "none".to_owned()),
        ("Cache-Control".to_owned(), "no-store".to_owned()),
        ("Content-Encoding".to_owned(), "identity".to_owned()),
    ]
}

impl<E> GatewayStream<E> {
    /// Wrap the engine. Leases run under `compat:jellyfin` until the audio
    /// routes scope the engine to the authenticated caller.
    pub fn new(engine: Arc<E>) -> Self {
        Self {
            engine,
            lease_user: "compat:jellyfin".to_owned(),
        }
    }

    async fn open_media(
        &self,
        file_id: &str,
        params: StreamParams,
        head_only: bool,
    ) -> Result<StreamMedia, ByteOutcome>
    where
        E: StreamEngine + 'static,
    {
        open(
            self.engine.as_ref(),
            &self.lease_user,
            file_id,
            params,
            head_only,
        )
        .await
        .map_err(|fault| match fault {
            StreamFault::Capacity => status(429, vec![("Retry-After".to_owned(), "1".to_owned())]),
            _ => status(404, Vec::new()),
        })
    }

    /// GET and HEAD of the original: the same status and headers, and for
    /// GET the asked span streamed.
    async fn original(&self, file_id: &str, range: Option<&str>, head_only: bool) -> ByteOutcome
    where
        E: StreamEngine + 'static,
    {
        let media = match self
            .open_media(file_id, StreamParams::default(), head_only)
            .await
        {
            Ok(media) => media,
            Err(outcome) => return outcome,
        };
        // Transcode landings never serve ranges; the policy layer only
        // calls `direct` for direct plans, so a landing here is served
        // whole with the transcode header set.
        if media.transcoded {
            let headers = landing_headers(media.content_type.clone());
            let body = if head_only {
                AudioBody::empty()
            } else {
                whole(media)
            };
            return ByteOutcome {
                status: 200,
                headers,
                body,
            };
        }
        let total = media.total_len;
        let span_asked = match range {
            None => None,
            Some(header) => match crate::stream::routes::parse_range(header, total) {
                Some(resolved) => Some(resolved),
                None => {
                    return status(
                        416,
                        vec![("Content-Range".to_owned(), format!("bytes */{total}"))],
                    );
                }
            },
        };
        let (code, start, len, content_range) = match span_asked {
            None => (200, 0, total, None),
            Some(resolved) => (
                206,
                resolved.start,
                resolved.len(),
                Some(format!(
                    "bytes {}-{}/{}",
                    resolved.start, resolved.end, total
                )),
            ),
        };
        let mut headers = vec![
            ("Content-Type".to_owned(), media.content_type.clone()),
            ("Content-Length".to_owned(), len.to_string()),
        ];
        if let Some(content_range) = content_range {
            headers.push(("Content-Range".to_owned(), content_range));
        }
        headers.push(("Accept-Ranges".to_owned(), "bytes".to_owned()));
        headers.push(("Content-Encoding".to_owned(), "identity".to_owned()));
        let body = if head_only {
            AudioBody::empty()
        } else {
            span(media.body, media.lease, start, len)
        };
        ByteOutcome {
            status: code,
            headers,
            body,
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
        self.original(file_id, range, false).await
    }

    async fn head(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        self.original(file_id, range, true).await
    }

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
        let media = match self.open_media(file_id, params, false).await {
            Ok(media) => media,
            Err(outcome) => return outcome,
        };
        // Estimate off on Jellyfin: never a Content-Length (the router
        // streams the body unsized).
        ByteOutcome {
            status: 200,
            headers: landing_headers(media.content_type.clone()),
            body: whole(media),
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
        assert_eq!(error.to_string(), crate::error::FIXED_INTERNAL_MESSAGE);
    }
}
