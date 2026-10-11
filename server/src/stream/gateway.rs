//! Stream-gateway engine: one source-keyed byte source behind the routes.
//!
//! [`Gateway`] implements the routes' [`StreamEngine`] seam: it takes a
//! direct lease, resolves local files or proxied remote bytes, and runs the
//! transcode [`decide()`] policy for local files.
//!
//! A local key is a catalog track id. It resolves through
//! [`LibraryFiles::locate_track`], the same lookup and root confinement
//! that library downloads use, so playback and downloads agree on which
//! files exist. A gateway without the library wired (tests, unwired
//! builds) joins the key onto its constructor root as a relative path
//! instead, sandboxed to that root.
//! The streaming open hands the routes a file body read by range, or the
//! transcode output chunk by chunk; HEAD answers from file metadata and
//! never starts ffmpeg. The whole-object open collects a streamed open in
//! memory; nothing in the server calls it (the compat adapters stream too),
//! it stays for the engine seam's tests. Range slicing, 206/416 decisions,
//! and headers stay in the routes file.
//!
//! The streaming open hands its direct lease back inside [`StreamMedia`],
//! so a caller that moves it into the response body bounds response
//! concurrency, not only opens (the compat adapters do). Remote reads
//! stream by range and are direct-only (the ffmpeg service takes a local
//! path; per-source server-side transcode stays a remotes-adapter concern).
//!
//! [`StreamEngine`]: super::routes::StreamEngine
//! [`decide()`]: super::transcode::decide

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::leases::DirectGate;
use super::local_files::{LibraryFiles, LocateError, PathRefusal};
use super::plugin_url;
use crate::plugins::capabilities::stream::{open_url, proxy_chunks};
use futures_util::StreamExt as _;

use super::routes::{
    AudioSource, ChunkStream, MediaBody, OpenMedia, StreamEngine, StreamFault, StreamMedia,
    StreamOpen, StreamParams, content_type_for_extension,
};
use super::transcode::{
    OutFormat, StreamPlan, TrackInfo, TranscodeBody, TranscodeError, TranscodeSettings, Transcoder,
    decide, estimate_size, out_media_type,
};

/// A canned remote object, for scripted readers: upstream bytes plus the
/// upstream content type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMedia {
    /// Upstream-resolved content type (upstream wins for remotes).
    pub content_type: String,
    /// Whole object bytes.
    pub bytes: Vec<u8>,
}

impl From<RemoteMedia> for StreamMedia {
    fn from(media: RemoteMedia) -> Self {
        StreamMedia::from(OpenMedia {
            content_type: media.content_type,
            total_len: media.bytes.len() as u64,
            transcoded: false,
            estimated_len: None,
            bytes: media.bytes,
        })
    }
}

/// Remote byte source. `media` implements this over the remotes
/// adapter (per-user connections + credential store); tests script it.
pub trait RemoteReader: Send + Sync {
    /// Open one remote object for streaming, or fail with a routable
    /// [`StreamFault`]; `head_only` needs only its metadata. A sized
    /// answer should stream by range, so nothing holds the whole file.
    fn open_stream(
        &self,
        source: AudioSource,
        key: &str,
        user_id: &str,
        head_only: bool,
    ) -> impl Future<Output = Result<StreamMedia, StreamFault>> + Send;
}

/// Where the transcode policy comes from: read again on every open, so a
/// settings change applies to the next stream without a restart.
pub type SettingsSource = Arc<dyn Fn() -> TranscodeSettings + Send + Sync>;

/// One source-keyed gateway: sandboxed local reads, injected remote reads,
/// and injected ffmpeg execution behind the routes' engine seam.
pub struct Gateway<R, T> {
    local_root: PathBuf,
    library: Option<LibraryFiles>,
    plugins: std::sync::OnceLock<(Arc<crate::plugins::host::PluginHost>, reqwest::Client)>,
    remote: R,
    transcoder: T,
    settings: SettingsSource,
    ffmpeg_present: bool,
    direct: Arc<DirectGate>,
}

impl<R, T> Gateway<R, T> {
    /// Build a gateway over one local root, one remote reader, and one
    /// ffmpeg service. Production passes the real transcode settings and
    /// [`ffmpeg_available()`]; tests script every seam.
    pub fn new(
        local_root: PathBuf,
        remote: R,
        transcoder: T,
        settings: TranscodeSettings,
        ffmpeg_present: bool,
    ) -> Self {
        Self {
            local_root,
            library: None,
            plugins: std::sync::OnceLock::new(),
            remote,
            transcoder,
            settings: Arc::new(move || settings.clone()),
            ffmpeg_present,
            direct: Arc::new(DirectGate::new()),
        }
    }

    /// Read the transcode policy from `source` on every open instead of
    /// the fixed settings the gateway was built with.
    pub fn with_settings_source(mut self, source: SettingsSource) -> Self {
        self.settings = source;
        self
    }

    /// Resolve local keys as catalog track ids against the live library
    /// instead of paths under the constructor root. The root registry is
    /// read again on every open, so root changes apply without a restart.
    pub fn with_library(mut self, library: LibraryFiles) -> Self {
        self.library = Some(library);
        self
    }

    /// Serve `streaming_source` plugins under [`AudioSource::Plugin`].
    /// `no_redirect` proxies plugin URLs; it must not follow redirects on
    /// its own, because every hop is re-checked. Boot attaches the host
    /// once, after the shared engine exists.
    pub fn attach_plugins(
        &self,
        host: Arc<crate::plugins::host::PluginHost>,
        no_redirect: reqwest::Client,
    ) {
        if self.plugins.set((host, no_redirect)).is_err() {
            tracing::warn!("plugin host was already attached to the stream gateway");
        }
    }
}

impl<R: RemoteReader, T: Transcoder> StreamEngine for Gateway<R, T> {
    async fn open(&self, request: StreamOpen) -> Result<OpenMedia, StreamFault> {
        let _lease = self
            .direct
            .acquire(&request.user_id)
            .await
            .map_err(|_| StreamFault::Capacity)?;
        match request.source {
            AudioSource::Local => self.open_local(&request).await,
            AudioSource::Plugin => {
                let media = self.stream_plugin(&request, false).await?;
                whole_media(media).await
            }
            source => {
                let media = self
                    .remote
                    .open_stream(source, &request.key, &request.user_id, false)
                    .await?;
                whole_media(media).await
            }
        }
    }

    async fn open_stream(
        &self,
        request: StreamOpen,
        head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        let lease = self
            .direct
            .acquire_owned(&request.user_id)
            .await
            .map_err(|_| StreamFault::Capacity)?;
        let mut media = match request.source {
            AudioSource::Local => self.stream_local(&request, head_only).await?,
            AudioSource::Plugin => self.stream_plugin(&request, head_only).await?,
            source => {
                self.remote
                    .open_stream(source, &request.key, &request.user_id, head_only)
                    .await?
            }
        };
        media.lease = Some(lease);
        Ok(media)
    }
}

/// A resolved local read: the sandboxed path, its content type, and the
/// transcode verdict.
struct LocalRead {
    path: PathBuf,
    content_type: &'static str,
    plan: StreamPlan,
}

impl<R: RemoteReader, T: Transcoder> Gateway<R, T> {
    /// Sandboxed local read, whole, with the transcode policy applied.
    async fn open_local(&self, request: &StreamOpen) -> Result<OpenMedia, StreamFault> {
        let LocalRead {
            path,
            content_type,
            plan,
        } = self.local_read(request).await?;
        match plan {
            StreamPlan::Direct { .. } => {
                let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
                    .await
                    .map_err(|cause| StreamFault::Internal {
                        cause: cause.to_string(),
                    })?
                    .map_err(io_fault)?;
                Ok(OpenMedia {
                    content_type: content_type.to_owned(),
                    total_len: bytes.len() as u64,
                    transcoded: false,
                    estimated_len: None,
                    bytes,
                })
            }
            StreamPlan::Transcode { .. } => {
                let out_format = plan.out_format().ok_or_else(|| StreamFault::Internal {
                    cause: "transcode plan carries no codec".to_owned(),
                })?;
                let mut body = self
                    .transcoder
                    .stream(&path, &plan, &request.user_id)
                    .await
                    .map_err(transcode_fault)?;
                let mut bytes = Vec::new();
                while let Some(chunk) = body.next_chunk(None).await.map_err(transcode_fault)? {
                    bytes.extend_from_slice(&chunk);
                }
                Ok(OpenMedia {
                    content_type: out_media_type(&out_format).to_owned(),
                    total_len: bytes.len() as u64,
                    transcoded: true,
                    estimated_len: estimated_len(request, &plan),
                    bytes,
                })
            }
        }
    }

    /// Sandboxed local read for streaming: the file is read by range as
    /// the response goes out, and a transcode streams chunk by chunk.
    async fn stream_local(
        &self,
        request: &StreamOpen,
        head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        let read = self.local_read(request).await?;
        self.stream_read(read, request, head_only).await
    }

    /// Stream one resolved read: a file body by range, or live transcode
    /// output.
    async fn stream_read(
        &self,
        read: LocalRead,
        request: &StreamOpen,
        head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        let LocalRead {
            path,
            content_type,
            plan,
        } = read;
        match plan {
            StreamPlan::Direct { .. } => {
                let probe = path.clone();
                let metadata = tokio::task::spawn_blocking(move || std::fs::metadata(probe))
                    .await
                    .map_err(|cause| StreamFault::Internal {
                        cause: cause.to_string(),
                    })?
                    .map_err(io_fault)?;
                if !metadata.is_file() {
                    return Err(StreamFault::NotFound);
                }
                Ok(StreamMedia {
                    content_type: content_type.to_owned(),
                    total_len: metadata.len(),
                    transcoded: false,
                    estimated_len: None,
                    body: MediaBody::File(path),
                    lease: None,
                })
            }
            StreamPlan::Transcode { .. } => {
                let out_format = plan.out_format().ok_or_else(|| StreamFault::Internal {
                    cause: "transcode plan carries no codec".to_owned(),
                })?;
                let body = if head_only {
                    MediaBody::Empty
                } else {
                    let stream = self
                        .transcoder
                        .stream(&path, &plan, &request.user_id)
                        .await
                        .map_err(transcode_fault)?;
                    MediaBody::Chunks(transcode_chunks(stream))
                };
                Ok(StreamMedia {
                    content_type: out_media_type(&out_format).to_owned(),
                    total_len: 0,
                    transcoded: true,
                    estimated_len: estimated_len(request, &plan),
                    body,
                    lease: None,
                })
            }
        }
    }

    /// Resolve the file, its content type, and the plan.
    async fn local_read(&self, request: &StreamOpen) -> Result<LocalRead, StreamFault> {
        let path = match &self.library {
            Some(library) => catalog_path(library, &request.key).await?,
            None => self.sandboxed_path(&request.key)?,
        };
        self.read_plan(path, request)
    }

    /// Content type and transcode plan for one resolved local path.
    fn read_plan(&self, path: PathBuf, request: &StreamOpen) -> Result<LocalRead, StreamFault> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_owned();
        let content_type =
            content_type_for_extension(&extension).ok_or_else(|| StreamFault::InvalidInput {
                message: "Unsupported audio format".to_owned(),
            })?;
        let plan = self.plan_for(&extension, None, request);
        Ok(LocalRead {
            path,
            content_type,
            plan,
        })
    }

    /// The transcode plan for a source in `format` (a file extension),
    /// with its length when known.
    fn plan_for(&self, format: &str, duration: Option<f64>, request: &StreamOpen) -> StreamPlan {
        // Track facts for decide(): the format is exact; bitrate is unknown
        // at this layer, so a client cap alone never triggers a transcode
        // here; only a codec mismatch does.
        let track = TrackInfo {
            file_format: format.to_owned(),
            bitrate_kbps: None,
            duration_seconds: duration.unwrap_or(0.0),
        };
        let force_original = request
            .params
            .format
            .as_deref()
            .is_some_and(|format| format.eq_ignore_ascii_case("raw"));
        let start_seconds = request.params.start_seconds;
        let settings = (self.settings)();
        // Compat verdict carry: the protocol modules decide with real source
        // bitrate facts, while this re-run sees `bitrate_kbps: None`, so a
        // bitrate-triggered same-codec plan would land direct here. When the
        // adapter carries `force_transcode`, honor the compat verdict (codec
        // + bitrate + offset) instead of re-deciding; the hard outs still
        // win, since a forced plan must never spawn ffmpeg when the policy
        // says direct.
        if request.params.force_transcode
            && settings.transcoding_enabled
            && self.ffmpeg_present
            && !force_original
        {
            return forced_plan(&request.params, &settings, start_seconds);
        }
        decide(
            &track,
            request.params.format.as_deref(),
            request.params.max_bitrate_kbps,
            force_original,
            start_seconds,
            &settings,
            self.ffmpeg_present,
        )
    }

    /// Every library root a plugin path may point into.
    fn all_library_roots(&self) -> Vec<PathBuf> {
        match &self.library {
            Some(library) => library
                .registry()
                .roots()
                .iter()
                .map(|root| root.path.clone())
                .collect(),
            None => vec![self.local_root.clone()],
        }
    }

    /// Ask `streaming_source` plugins for one recording. A file answer is
    /// served like a local file (ranges, transcodes). A URL answer is
    /// proxied with every hop re-checked: by range when its size is known,
    /// and through a temp file when the plan transcodes (v2 behaviour).
    async fn stream_plugin(
        &self,
        request: &StreamOpen,
        head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        use crate::plugins::capabilities::stream::PluginStream;

        let Some((host, client)) = self.plugins.get() else {
            return Err(StreamFault::NotFound);
        };
        let roots = self.all_library_roots();
        let resolved = host
            .resolve_stream(&request.key, &request.user_id, &roots)
            .await
            .ok_or(StreamFault::NotFound)?;
        match resolved {
            PluginStream::File { path, .. } => {
                let read = self.read_plan(path, request)?;
                self.stream_read(read, request, head_only).await
            }
            PluginStream::Remote {
                plugin,
                url,
                content_type,
                duration_seconds,
            } => {
                let format = plugin_url::url_format(&url, &content_type);
                let plan = self.plan_for(&format, duration_seconds, request);
                if plan.is_transcode() {
                    return self
                        .transcode_plugin_url(
                            client, &plugin, url, &format, plan, request, head_only,
                        )
                        .await;
                }
                let response = plugin_url_response(client, &plugin, url.clone(), None).await?;
                let upstream_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| value.starts_with("audio/") || *value == "application/ogg")
                    .map(str::to_owned);
                let content_type = upstream_type
                    .or_else(|| (!content_type.is_empty()).then_some(content_type))
                    .unwrap_or_else(|| "application/octet-stream".to_owned());
                let length = response.content_length();
                match length {
                    // A sized answer is served like a local file: ranged,
                    // so players can seek.
                    Some(total) if plugin_url::within_cap(total) => Ok(StreamMedia {
                        content_type,
                        total_len: total,
                        transcoded: false,
                        estimated_len: None,
                        body: if head_only {
                            MediaBody::Empty
                        } else {
                            MediaBody::Spans(plugin_url::spans(client.clone(), url, response))
                        },
                        lease: None,
                    }),
                    Some(total) => {
                        tracing::warn!(%plugin, bytes = total, "plugin stream URL is over the size cap");
                        Err(StreamFault::Upstream {
                            source: AudioSource::Plugin,
                        })
                    }
                    // No size: served like a landing, whole and unranged.
                    None => Ok(StreamMedia {
                        content_type,
                        total_len: 0,
                        transcoded: true,
                        estimated_len: None,
                        body: if head_only {
                            MediaBody::Empty
                        } else {
                            MediaBody::Chunks(proxy_chunks(response).boxed())
                        },
                        lease: None,
                    }),
                }
            }
        }
    }

    /// Transcode a plugin URL: the bytes are spooled to a temp file that
    /// ffmpeg reads (it never fetches the network itself), and the file is
    /// removed when the transcode body ends. HEAD answers without fetching.
    #[allow(clippy::too_many_arguments)]
    async fn transcode_plugin_url(
        &self,
        client: &reqwest::Client,
        plugin: &str,
        url: reqwest::Url,
        format: &str,
        plan: StreamPlan,
        request: &StreamOpen,
        head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        let out_format = plan.out_format().ok_or_else(|| StreamFault::Internal {
            cause: "transcode plan carries no codec".to_owned(),
        })?;
        let mut media = StreamMedia {
            content_type: out_media_type(&out_format).to_owned(),
            total_len: 0,
            transcoded: true,
            estimated_len: estimated_len(request, &plan),
            body: MediaBody::Empty,
            lease: None,
        };
        if head_only {
            return Ok(media);
        }
        let response = plugin_url_response(client, plugin, url, None).await?;
        let spool = plugin_url::Spool::fill(response, format)
            .await
            .map_err(|error| {
                tracing::warn!(%plugin, %error, "plugin stream URL could not be fetched for transcoding");
                StreamFault::Upstream {
                    source: AudioSource::Plugin,
                }
            })?;
        let stream = self
            .transcoder
            .stream(spool.path(), &plan, &request.user_id)
            .await
            .map_err(transcode_fault)?;
        // The spool lives inside the body stream and goes with it.
        media.body = MediaBody::Chunks(
            transcode_chunks(stream)
                .map(move |chunk| {
                    let _keep = &spool;
                    chunk
                })
                .boxed(),
        );
        Ok(media)
    }

    /// Join `key` under the constructor root, refusing anything that
    /// escapes it. Only gateways without the library wired read this way.
    /// The refusal message is fixed: the key never reaches the wire.
    /// Component screening runs first; then both sides canonicalize so a
    /// symlink inside the root cannot point at a file outside it. Paths
    /// that do not resolve (missing files, missing root) skip the prefix
    /// check and fall through to the read, which reports them.
    fn sandboxed_path(&self, key: &str) -> Result<PathBuf, StreamFault> {
        if key.is_empty() || Path::new(key).is_absolute() {
            return Err(forbidden());
        }
        let root = &self.local_root;
        let mut path = root.clone();
        for component in Path::new(key).components() {
            match component {
                std::path::Component::Normal(part) => path.push(part),
                _ => {
                    return Err(forbidden());
                }
            }
        }
        if let (Ok(canonical_root), Ok(resolved)) = (root.canonicalize(), path.canonicalize())
            && !resolved.starts_with(&canonical_root)
        {
            return Err(forbidden());
        }
        Ok(path)
    }
}

/// The file of catalog track `track_id`. Unknown tracks and files gone
/// from disk are 404; a file that resolves outside the library roots is
/// 403 with v2's message.
async fn catalog_path(library: &LibraryFiles, track_id: &str) -> Result<PathBuf, StreamFault> {
    match library.locate_track(track_id).await {
        Ok((_, path)) => Ok(path),
        Err(LocateError::Unknown | LocateError::Refused(PathRefusal::Missing)) => {
            Err(StreamFault::NotFound)
        }
        Err(LocateError::Refused(PathRefusal::Outside)) => {
            tracing::warn!(
                track_id,
                "stream refused: file resolves outside the library roots"
            );
            Err(StreamFault::Forbidden {
                message: OUTSIDE_MESSAGE.to_owned(),
            })
        }
        Err(LocateError::Internal(cause)) => Err(StreamFault::Internal { cause }),
    }
}

/// Build the transcode plan a compat verdict dictates: the adapter's
/// codec (`mp3`, `opus`, or the Jellyfin HLS segment format `aac-ts`, which
/// only this path accepts) and its already-decided bitrate, plus the seek
/// offset. Duration is unknown at this layer, so
/// the size estimate degrades to zero remaining seconds.
fn forced_plan(
    params: &StreamParams,
    settings: &TranscodeSettings,
    start_seconds: f64,
) -> StreamPlan {
    let out_format = match params
        .format
        .as_deref()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "opus" => OutFormat::Opus,
        super::transcode::HLS_SEGMENT_FORMAT => OutFormat::AacTs,
        _ => OutFormat::Mp3,
    };
    let out_bitrate_kbps = super::transcode::out_bitrate_kbps(
        out_format.name(),
        params.max_bitrate_kbps,
        settings.max_bitrate_kbps,
    );
    StreamPlan::Transcode {
        out_format,
        out_bitrate_kbps,
        start_seconds: start_seconds.max(0.0),
        source_duration_seconds: 0.0,
    }
}

/// Open a plugin URL (re-checking every redirect hop) and require a
/// success status. Refusals read as a miss; upstream failures as 502.
async fn plugin_url_response(
    client: &reqwest::Client,
    plugin: &str,
    url: reqwest::Url,
    range: Option<&str>,
) -> Result<reqwest::Response, StreamFault> {
    let response = open_url(client, url, range).await.map_err(|reason| {
        tracing::warn!(%plugin, %reason, "plugin stream URL refused");
        StreamFault::NotFound
    })?;
    if !response.status().is_success() {
        tracing::warn!(%plugin, status = %response.status(), "plugin stream URL failed");
        return Err(StreamFault::Upstream {
            source: AudioSource::Plugin,
        });
    }
    Ok(response)
}

/// The size estimate a client asked for, when the plan yields one.
fn estimated_len(request: &StreamOpen, plan: &StreamPlan) -> Option<u64> {
    request
        .params
        .estimate_content_length
        .then(|| estimate_size(plan))
        .flatten()
        .filter(|size| *size > 0)
}

/// Transcode output as a body stream. Dropping the stream (a client that
/// went away) drops the transcode, which kills and reaps ffmpeg and frees
/// its slot.
fn transcode_chunks<B: TranscodeBody + 'static>(body: B) -> ChunkStream {
    futures_util::stream::unfold(Some(body), |state| async move {
        let mut body = state?;
        match body.next_chunk(None).await {
            Ok(Some(chunk)) => Some((Ok(chunk), Some(body))),
            Ok(None) => None,
            Err(error) => Some((Err(std::io::Error::other(error.to_string())), None)),
        }
    })
    .boxed()
}

/// Collect a streamed open into a whole object (the compat whole-object
/// path).
async fn whole_media(media: StreamMedia) -> Result<OpenMedia, StreamFault> {
    let body = match media.body {
        MediaBody::Spans(open) => MediaBody::Chunks(open(0, media.total_len)),
        body => body,
    };
    let bytes = match body {
        MediaBody::Bytes(bytes) => bytes,
        MediaBody::File(path) => tokio::task::spawn_blocking(move || std::fs::read(path))
            .await
            .map_err(|cause| StreamFault::Internal {
                cause: cause.to_string(),
            })?
            .map_err(io_fault)?,
        MediaBody::Chunks(mut chunks) => {
            let cap = crate::plugins::capabilities::stream::PROXY_MAX_BYTES as usize;
            let mut bytes = Vec::new();
            while let Some(chunk) = chunks.next().await {
                bytes.extend_from_slice(&chunk.map_err(|error| StreamFault::Internal {
                    cause: error.to_string(),
                })?);
                if bytes.len() > cap {
                    return Err(StreamFault::Internal {
                        cause: "stream is over 500 MiB".to_owned(),
                    });
                }
            }
            bytes
        }
        MediaBody::Spans(_) | MediaBody::Empty => Vec::new(),
    };
    Ok(OpenMedia {
        content_type: media.content_type,
        total_len: bytes.len() as u64,
        transcoded: media.transcoded,
        estimated_len: media.estimated_len,
        bytes,
    })
}

/// v2's refusal for a library file that resolves outside every root.
pub const OUTSIDE_MESSAGE: &str = "Access denied: path is outside the music directory";

/// Fixed sandbox refusal: the key never reaches the wire.
fn forbidden() -> StreamFault {
    StreamFault::Forbidden {
        message: "Playback not allowed".to_owned(),
    }
}

/// Map a local-read failure onto the routes' fault spelling. Missing files
/// 404; unreadable files 403 (the fixed message leaks nothing); anything
/// else is a fixed-message 5xx with the kind in the log only.
fn io_fault(error: std::io::Error) -> StreamFault {
    match error.kind() {
        std::io::ErrorKind::NotFound => StreamFault::NotFound,
        std::io::ErrorKind::PermissionDenied => forbidden(),
        kind => StreamFault::Internal {
            cause: kind.to_string(),
        },
    }
}

/// Map a transcode failure onto the routes' fault spelling. Capacity keeps
/// its 429 meaning; everything else is a fixed-message 5xx with the cause
/// in the log only (callers log through `StreamError::internal`).
fn transcode_fault(error: TranscodeError) -> StreamFault {
    match error {
        TranscodeError::Capacity => StreamFault::Capacity,
        other => StreamFault::Internal {
            cause: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_failures_map_to_routable_faults() {
        assert_eq!(
            io_fault(std::io::Error::new(std::io::ErrorKind::NotFound, "gone")),
            StreamFault::NotFound
        );
        assert_eq!(
            io_fault(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied"
            )),
            forbidden()
        );
        assert!(matches!(
            io_fault(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "slow disk"
            )),
            StreamFault::Internal { .. }
        ));
    }
}
