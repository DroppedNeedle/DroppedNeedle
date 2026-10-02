//! Compat streaming over the stage-6 engine: both protocol seams call the
//! same [`StreamEngine`](crate::stream::routes::StreamEngine) the native
//! routes use, so bytes, leases, and transcode execution are shared. Range
//! slicing stays in each protocol layer (already byte-identical rules);
//! the adapters map whole-object opens onto the seam outcomes.
//!
//! Compat file ids pass through as stage-6 local stream keys. Unknown ids
//! fail exactly like native unknown ids (404/70), and exhausted leases
//! surface as 429 + `Retry-After: 1`.

use std::sync::Arc;

use crate::compat::jellyfin::seams::{ByteOutcome, StreamEngine as JellyfinStreamEngine};
use crate::compat::subsonic::stream::{AudioBackend, AudioFacts, BackendError, StreamPlan};
use crate::stream::routes::{
    AudioSource, OpenMedia, StreamEngine, StreamFault, StreamOpen, StreamParams,
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
fn facts_for(media: &OpenMedia) -> AudioFacts {
    AudioFacts {
        size: media.total_len,
        suffix: suffix_for_content_type(&media.content_type),
        bitrate_kbps: None,
        duration_seconds: None,
    }
}

fn backend_error(fault: StreamFault) -> BackendError {
    match fault {
        StreamFault::Capacity => BackendError::full(),
        StreamFault::NotFound => BackendError::failed("audio item not found"),
        StreamFault::Forbidden { message } => BackendError::failed(message),
        StreamFault::InvalidInput { message } => BackendError::failed(message),
        StreamFault::Upstream { source } => {
            BackendError::failed(format!("upstream {} failed", source.as_str()))
        }
        StreamFault::Internal { cause } => BackendError::failed(cause),
    }
}

/// Subsonic audio backend over any stage-6 engine.
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
    /// Wrap the engine; `lease_user` is the fixed lease principal (the
    /// audio seam carries no caller, so per-user fairness needs a seam
    /// parameter; see `setup.rs`).
    pub fn new(engine: Arc<E>, lease_user: String) -> Self {
        Self { engine, lease_user }
    }
}

impl<E: StreamEngine + 'static> AudioBackend for GatewayAudio<E> {
    async fn audio_facts(&self, file_id: &str) -> Result<Option<AudioFacts>, BackendError> {
        match self
            .engine
            .as_ref()
            .open(StreamOpen {
                source: AudioSource::Local,
                key: file_id.to_owned(),
                user_id: self.lease_user.clone(),
                params: StreamParams::default(),
            })
            .await
        {
            Ok(media) => Ok(Some(facts_for(&media))),
            Err(StreamFault::NotFound) => Ok(None),
            Err(fault) => Err(backend_error(fault)),
        }
    }

    async fn read_object(
        &self,
        file_id: &str,
    ) -> Result<Option<(AudioFacts, Vec<u8>)>, BackendError> {
        // Single engine open: `serve_original` slices facts and range from
        // this one read instead of opening once for facts and again for
        // bytes.
        match self
            .engine
            .as_ref()
            .open(StreamOpen {
                source: AudioSource::Local,
                key: file_id.to_owned(),
                user_id: self.lease_user.clone(),
                params: StreamParams::default(),
            })
            .await
        {
            Ok(media) => {
                let facts = facts_for(&media);
                Ok(Some((facts, media.bytes)))
            }
            Err(StreamFault::NotFound) => Ok(None),
            Err(fault) => Err(backend_error(fault)),
        }
    }

    async fn read_range(
        &self,
        file_id: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, BackendError> {
        let media = self
            .engine
            .as_ref()
            .open(StreamOpen {
                source: AudioSource::Local,
                key: file_id.to_owned(),
                user_id: self.lease_user.clone(),
                params: StreamParams::default(),
            })
            .await
            .map_err(backend_error)?;
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        let end = usize::try_from(end).unwrap_or(0);
        media
            .bytes
            .get(start..=end)
            .map(|slice| slice.to_vec())
            .ok_or_else(|| BackendError::failed("engine short-read the ranged object"))
    }

    async fn transcode(
        &self,
        file_id: &str,
        plan: &StreamPlan,
    ) -> Result<(Vec<u8>, String), BackendError> {
        let media = self
            .engine
            .as_ref()
            .open(StreamOpen {
                source: AudioSource::Local,
                key: file_id.to_owned(),
                user_id: self.lease_user.clone(),
                params: StreamParams {
                    format: plan.out_format.clone(),
                    max_bitrate_kbps: plan.out_bitrate_kbps,
                    estimate_content_length: false,
                    start_seconds: plan.start_seconds,
                    force_transcode: true,
                },
            })
            .await
            .map_err(backend_error)?;
        Ok((media.bytes, media.content_type))
    }
}

/// Jellyfin stream engine over any stage-6 engine.
pub struct GatewayStream<E> {
    engine: Arc<E>,
}

impl<E> Clone for GatewayStream<E> {
    fn clone(&self) -> Self {
        Self {
            engine: Arc::clone(&self.engine),
        }
    }
}

impl<E> GatewayStream<E> {
    /// Wrap the engine. Jellyfin audio is anonymous, so leases run under
    /// the fixed `compat:jellyfin` principal (per-user lease fairness
    /// needs a seam change; see the module docs in `setup.rs`).
    pub fn new(engine: Arc<E>) -> Self {
        Self { engine }
    }

    async fn open_media(
        &self,
        file_id: &str,
        params: StreamParams,
    ) -> Result<OpenMedia, ByteOutcome>
    where
        E: StreamEngine + 'static,
    {
        self.engine
            .as_ref()
            .open(StreamOpen {
                source: AudioSource::Local,
                key: file_id.to_owned(),
                user_id: "compat:jellyfin".to_owned(),
                params,
            })
            .await
            .map_err(|fault| match fault {
                StreamFault::Capacity => ByteOutcome {
                    status: 429,
                    headers: vec![("Retry-After".to_owned(), "1".to_owned())],
                    body: Vec::new(),
                },
                _ => ByteOutcome {
                    status: 404,
                    headers: Vec::new(),
                    body: Vec::new(),
                },
            })
    }
}

impl<E: StreamEngine + 'static> JellyfinStreamEngine for GatewayStream<E> {
    async fn direct(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        let media = match self.open_media(file_id, StreamParams::default()).await {
            Ok(media) => media,
            Err(outcome) => return outcome,
        };
        // Transcode landings never serve ranges; the policy layer only
        // calls `direct` for direct plans, so a landing here is served
        // whole with the transcode header set.
        if media.transcoded {
            return ByteOutcome {
                status: 200,
                headers: vec![
                    ("Content-Type".to_owned(), media.content_type),
                    ("Accept-Ranges".to_owned(), "none".to_owned()),
                    ("Cache-Control".to_owned(), "no-store".to_owned()),
                    ("Content-Encoding".to_owned(), "identity".to_owned()),
                ],
                body: media.bytes,
            };
        }
        let total = media.total_len;
        let span = match range {
            None => None,
            Some(header) => match crate::stream::routes::parse_range(header, total) {
                Some(resolved) => Some(resolved),
                None => {
                    return ByteOutcome {
                        status: 416,
                        headers: vec![("Content-Range".to_owned(), format!("bytes */{total}"))],
                        body: Vec::new(),
                    };
                }
            },
        };
        match span {
            None => ByteOutcome {
                status: 200,
                headers: vec![
                    ("Content-Type".to_owned(), media.content_type),
                    ("Content-Length".to_owned(), total.to_string()),
                    ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                    ("Content-Encoding".to_owned(), "identity".to_owned()),
                ],
                body: media.bytes,
            },
            Some(resolved) => {
                let start = usize::try_from(resolved.start).unwrap_or(usize::MAX);
                let end = usize::try_from(resolved.end).unwrap_or(0);
                let Some(slice) = media.bytes.get(start..=end) else {
                    return ByteOutcome {
                        status: 404,
                        headers: Vec::new(),
                        body: Vec::new(),
                    };
                };
                ByteOutcome {
                    status: 206,
                    headers: vec![
                        ("Content-Type".to_owned(), media.content_type),
                        ("Content-Length".to_owned(), slice.len().to_string()),
                        (
                            "Content-Range".to_owned(),
                            format!("bytes {}-{}/{}", resolved.start, resolved.end, total),
                        ),
                        ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                        ("Content-Encoding".to_owned(), "identity".to_owned()),
                    ],
                    body: slice.to_vec(),
                }
            }
        }
    }

    async fn head(&self, file_id: &str, range: Option<&str>) -> ByteOutcome {
        let media = match self.open_media(file_id, StreamParams::default()).await {
            Ok(media) => media,
            Err(outcome) => return outcome,
        };
        // GET-equivalent headers, empty body (transcode landings serve the
        // whole-object header set, like `direct` does).
        if media.transcoded {
            return ByteOutcome {
                status: 200,
                headers: vec![
                    ("Content-Type".to_owned(), media.content_type),
                    ("Accept-Ranges".to_owned(), "none".to_owned()),
                    ("Cache-Control".to_owned(), "no-store".to_owned()),
                    ("Content-Encoding".to_owned(), "identity".to_owned()),
                ],
                body: Vec::new(),
            };
        }
        let total = media.total_len;
        let span = match range {
            None => None,
            Some(header) => match crate::stream::routes::parse_range(header, total) {
                Some(resolved) => Some(resolved),
                None => {
                    return ByteOutcome {
                        status: 416,
                        headers: vec![("Content-Range".to_owned(), format!("bytes */{total}"))],
                        body: Vec::new(),
                    };
                }
            },
        };
        match span {
            None => ByteOutcome {
                status: 200,
                headers: vec![
                    ("Content-Type".to_owned(), media.content_type),
                    ("Content-Length".to_owned(), total.to_string()),
                    ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                    ("Content-Encoding".to_owned(), "identity".to_owned()),
                ],
                body: Vec::new(),
            },
            Some(resolved) => {
                // Same short-read guard as `direct`: a HEAD must never
                // promise bytes GET cannot serve.
                let start = usize::try_from(resolved.start).unwrap_or(usize::MAX);
                let end = usize::try_from(resolved.end).unwrap_or(0);
                if media.bytes.get(start..=end).is_none() {
                    return ByteOutcome {
                        status: 404,
                        headers: Vec::new(),
                        body: Vec::new(),
                    };
                }
                ByteOutcome {
                    status: 206,
                    headers: vec![
                        ("Content-Type".to_owned(), media.content_type),
                        (
                            "Content-Length".to_owned(),
                            (resolved.end - resolved.start + 1).to_string(),
                        ),
                        (
                            "Content-Range".to_owned(),
                            format!("bytes {}-{}/{}", resolved.start, resolved.end, total),
                        ),
                        ("Accept-Ranges".to_owned(), "bytes".to_owned()),
                        ("Content-Encoding".to_owned(), "identity".to_owned()),
                    ],
                    body: Vec::new(),
                }
            }
        }
    }

    async fn transcode(
        &self,
        file_id: &str,
        format: &str,
        bitrate_kbps: u32,
        start_seconds: f64,
    ) -> ByteOutcome {
        let media = match self
            .open_media(
                file_id,
                StreamParams {
                    format: Some(format.to_owned()),
                    max_bitrate_kbps: Some(i64::from(bitrate_kbps)),
                    estimate_content_length: false,
                    start_seconds,
                    force_transcode: true,
                },
            )
            .await
        {
            Ok(media) => media,
            Err(outcome) => return outcome,
        };
        // Estimate off on Jellyfin: never a Content-Length (the router
        // streams the body unsized).
        ByteOutcome {
            status: 200,
            headers: vec![
                ("Content-Type".to_owned(), media.content_type),
                ("Accept-Ranges".to_owned(), "none".to_owned()),
                ("Cache-Control".to_owned(), "no-store".to_owned()),
                ("Content-Encoding".to_owned(), "identity".to_owned()),
            ],
            body: media.bytes,
        }
    }
}
