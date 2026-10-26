//! Stream-gateway engine: one source-keyed byte source behind the routes.
//!
//! [`Gateway`] implements the routes' [`StreamEngine`] seam: it takes a
//! direct lease, resolves local files or proxied remote bytes, and makes
//! the one transcode [`decide()`] call for every protocol. The native,
//! Subsonic and Jellyfin routes only translate their parameters into
//! [`StreamParams`]; the plan the gateway ran comes back as
//! [`StreamMedia::transcoded`], so a direct landing is framed as direct.
//!
//! A local key is a catalog track id. It resolves through
//! [`LibraryFiles::locate_track`], the same lookup and root confinement
//! that library downloads use, so playback and downloads agree on which
//! files exist; the row's format, bitrate and length are the facts the
//! decision reads. A gateway without the library wired (tests, unwired
//! builds) joins the key onto its constructor root as a relative path
//! instead, sandboxed to that root, and knows only the file's extension.
//! The open hands the routes a file body read by range, or the transcode
//! output chunk by chunk; HEAD answers from file metadata and never starts
//! ffmpeg. Range slicing, 206/416 decisions, and headers stay in the routes
//! file.
//!
//! The open hands its direct lease back inside [`StreamMedia`], so a caller
//! that moves it into the response body bounds response concurrency, not
//! only opens (the compat adapters do). A transcode takes the lease into
//! the transcode instead, which frees it with the ffmpeg slot. Remote reads
//! stream by range and are direct-only (the ffmpeg service takes a local
//! path; per-source server-side transcode stays a remotes-adapter concern).
//!
//! [`StreamEngine`]: super::routes::StreamEngine
//! [`StreamParams`]: super::routes::StreamParams
//! [`decide()`]: super::transcode::decide

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::leases::DirectGate;
use super::local_files::{CatalogFile, LibraryFiles, LocateError, PathRefusal};
use super::plugin_url;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::ConnectApps;
use droppedneedle_plugins::capabilities::stream::{open_url, proxy_chunks};
use futures_util::StreamExt as _;

use super::leases::OwnedDirectLease;
use super::routes::{
    AudioSource, MediaBody, StreamEngine, StreamFault, StreamMedia, StreamOpen,
    content_type_for_extension,
};
use super::transcode::{
    StreamPlan, TrackInfo, TranscodeError, TranscodeSettings, Transcoder, decide, estimate_size,
    out_media_type,
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
        StreamMedia {
            content_type: media.content_type,
            total_len: media.bytes.len() as u64,
            transcoded: false,
            estimated_len: None,
            body: MediaBody::Bytes(media.bytes),
            lease: None,
        }
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

/// One source-keyed gateway: sandboxed local reads, injected remote reads,
/// and injected ffmpeg execution behind the routes' engine seam.
pub struct Gateway<R, T> {
    local_root: PathBuf,
    library: Option<LibraryFiles>,
    plugins: Option<(
        Arc<droppedneedle_plugins::host::PluginHost>,
        reqwest::Client,
    )>,
    remote: R,
    transcoder: T,
    fixed: TranscodeSettings,
    settings: Option<Live<ConnectApps>>,
    ffmpeg_present: bool,
    direct: Arc<DirectGate>,
}

impl<R, T> Gateway<R, T> {
    /// Build a gateway over one local root, one remote reader, and one
    /// ffmpeg service. `ffmpeg_present` is whether the builder was given an
    /// ffmpeg. Production reads the transcode policy live
    /// ([`Self::with_settings`]); tests script every seam.
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
            plugins: None,
            remote,
            transcoder,
            fixed: settings,
            settings: None,
            ffmpeg_present,
            direct: Arc::new(DirectGate::new()),
        }
    }

    /// Whether this gateway can transcode: it was built with an ffmpeg.
    /// The compat APIs read it to decide what they offer.
    pub fn can_transcode(&self) -> bool {
        self.ffmpeg_present
    }

    /// Read the transcode policy from `settings` on every open instead of
    /// the fixed settings the gateway was built with, so a change applies
    /// to the next stream without a restart.
    pub fn with_settings(mut self, settings: Live<ConnectApps>) -> Self {
        self.settings = Some(settings);
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
    /// its own, because every hop is re-checked.
    pub fn with_plugins(
        mut self,
        host: Arc<droppedneedle_plugins::host::PluginHost>,
        no_redirect: reqwest::Client,
    ) -> Self {
        self.plugins = Some((host, no_redirect));
        self
    }
}

impl<R: RemoteReader, T: Transcoder> StreamEngine for Gateway<R, T> {
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
        match request.source {
            AudioSource::Local => self.stream_local(&request, head_only, lease).await,
            AudioSource::Plugin => self.stream_plugin(&request, head_only, lease).await,
            source => {
                let mut media = self
                    .remote
                    .open_stream(source, &request.key, &request.user_id, head_only)
                    .await?;
                media.lease = Some(lease);
                Ok(media)
            }
        }
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
    /// Sandboxed local read for streaming: the file is read by range as
    /// the response goes out, and a transcode streams chunk by chunk.
    async fn stream_local(
        &self,
        request: &StreamOpen,
        head_only: bool,
        lease: OwnedDirectLease,
    ) -> Result<StreamMedia, StreamFault> {
        let read = self.local_read(request).await?;
        self.stream_read(read, request, head_only, lease).await
    }

    /// Stream one resolved read: a file body by range, or live transcode
    /// output.
    async fn stream_read(
        &self,
        read: LocalRead,
        request: &StreamOpen,
        head_only: bool,
        lease: OwnedDirectLease,
    ) -> Result<StreamMedia, StreamFault> {
        let LocalRead {
            path,
            content_type,
            plan,
        } = read;
        match plan {
            StreamPlan::Direct { .. } => {
                let metadata = tokio::fs::metadata(&path).await.map_err(io_fault)?;
                if !metadata.is_file() {
                    return Err(StreamFault::NotFound);
                }
                Ok(StreamMedia {
                    content_type: content_type.to_owned(),
                    total_len: metadata.len(),
                    transcoded: false,
                    estimated_len: None,
                    body: MediaBody::File(path),
                    lease: Some(lease),
                })
            }
            StreamPlan::Transcode { .. } => {
                let out_format = plan.out_format().ok_or_else(|| StreamFault::Internal {
                    cause: "transcode plan carries no codec".to_owned(),
                })?;
                let (body, lease) = if head_only {
                    (MediaBody::Empty, Some(lease))
                } else {
                    let chunks = self
                        .transcoder
                        .stream(&path, &plan, &request.user_id, Some(lease))
                        .await
                        .map_err(transcode_fault)?;
                    (MediaBody::Chunks(chunks), None)
                };
                Ok(StreamMedia {
                    content_type: out_media_type(&out_format).to_owned(),
                    total_len: 0,
                    transcoded: true,
                    estimated_len: estimated_len(request, &plan),
                    body,
                    lease,
                })
            }
        }
    }

    /// Resolve the file, its content type, and the plan.
    async fn local_read(&self, request: &StreamOpen) -> Result<LocalRead, StreamFault> {
        match &self.library {
            Some(library) => {
                let (file, path) = catalog_file(library, &request.key).await?;
                let track = TrackInfo {
                    file_format: file.file_format,
                    bitrate_kbps: file.bitrate_kbps,
                    duration_seconds: file.duration_seconds.unwrap_or(0.0),
                };
                self.read_plan(path, Some(track), request)
            }
            None => self.read_plan(self.sandboxed_path(&request.key)?, None, request),
        }
    }

    /// Content type and transcode plan for one resolved local path. A path
    /// with no catalog facts is known only by its extension.
    fn read_plan(
        &self,
        path: PathBuf,
        track: Option<TrackInfo>,
        request: &StreamOpen,
    ) -> Result<LocalRead, StreamFault> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_owned();
        let content_type =
            content_type_for_extension(&extension).ok_or_else(|| StreamFault::InvalidInput {
                message: "Unsupported audio format".to_owned(),
            })?;
        let track = track.unwrap_or(TrackInfo {
            file_format: extension,
            bitrate_kbps: None,
            duration_seconds: 0.0,
        });
        let plan = self.plan_for(&track, request)?;
        Ok(LocalRead {
            path,
            content_type,
            plan,
        })
    }

    /// The one transcode decision, from the source's own facts and the
    /// request's parameters. `format=raw` always lands direct.
    fn plan_for(&self, track: &TrackInfo, request: &StreamOpen) -> Result<StreamPlan, StreamFault> {
        let force_original = request
            .params
            .format
            .as_deref()
            .is_some_and(|format| format.eq_ignore_ascii_case("raw"));
        let settings = match &self.settings {
            Some(live) => crate::transcode_policy(live).ok_or_else(|| StreamFault::Internal {
                cause: "connect_apps settings cannot be read".to_owned(),
            })?,
            None => self.fixed.clone(),
        };
        Ok(decide(
            track,
            request.params.format.as_deref(),
            request.params.max_bitrate_kbps,
            force_original,
            request.params.start_seconds,
            &settings,
            self.ffmpeg_present,
        ))
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
        lease: OwnedDirectLease,
    ) -> Result<StreamMedia, StreamFault> {
        use droppedneedle_plugins::capabilities::stream::PluginStream;

        let Some((host, client)) = &self.plugins else {
            return Err(StreamFault::NotFound);
        };
        let roots = self.all_library_roots();
        let resolved = host
            .resolve_stream(&request.key, &request.user_id, &roots)
            .await
            .ok_or(StreamFault::NotFound)?;
        match resolved {
            PluginStream::File { path, .. } => {
                let read = self.read_plan(path, None, request)?;
                self.stream_read(read, request, head_only, lease).await
            }
            PluginStream::Remote {
                plugin,
                url,
                content_type,
                duration_seconds,
            } => {
                let format = plugin_url::url_format(&url, &content_type);
                let track = TrackInfo {
                    file_format: format.clone(),
                    bitrate_kbps: None,
                    duration_seconds: duration_seconds.unwrap_or(0.0),
                };
                let plan = self.plan_for(&track, request)?;
                if plan.is_transcode() {
                    return self
                        .transcode_plugin_url(
                            client, &plugin, url, &format, plan, request, head_only, lease,
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
                        lease: Some(lease),
                    }),
                    Some(total) => Err(StreamFault::Upstream {
                        source: AudioSource::Plugin,
                        cause: format!(
                            "plugin {plugin} stream URL is over the size cap ({total} bytes)"
                        ),
                    }),
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
                        lease: Some(lease),
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
        lease: OwnedDirectLease,
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
            media.lease = Some(lease);
            return Ok(media);
        }
        let response = plugin_url_response(client, plugin, url, None).await?;
        let spool = plugin_url::Spool::fill(response, format)
            .await
            .map_err(|error| StreamFault::Upstream {
                source: AudioSource::Plugin,
                cause: format!(
                    "plugin {plugin} stream URL could not be fetched for transcoding: {error}"
                ),
            })?;
        let chunks = self
            .transcoder
            .stream(spool.path(), &plan, &request.user_id, Some(lease))
            .await
            .map_err(transcode_fault)?;
        // The spool lives inside the body stream and goes with it.
        media.body = MediaBody::Chunks(
            chunks
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

/// The catalog row and file of track `track_id`. Unknown tracks and files
/// gone from disk are 404; a file that resolves outside the library roots
/// is 403 with v2's message.
async fn catalog_file(
    library: &LibraryFiles,
    track_id: &str,
) -> Result<(CatalogFile, PathBuf), StreamFault> {
    match library.locate_track(track_id).await {
        Ok(found) => Ok(found),
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
        return Err(StreamFault::Upstream {
            source: AudioSource::Plugin,
            cause: format!(
                "plugin {plugin} stream URL failed with {}",
                response.status()
            ),
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
/// its 429 meaning; everything else is a fixed-message 5xx whose cause
/// reaches the log only.
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
