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
//! decision reads. The open hands the routes a file body read by range,
//! or the transcode output chunk by chunk; HEAD answers from file metadata
//! and never starts ffmpeg. Range slicing, 206/416 decisions, and headers
//! stay in [`framing`](super::framing).
//!
//! The open hands its direct lease back inside [`StreamMedia`], so a caller
//! that moves it into the response body bounds response concurrency, not
//! only opens (compat's `GatewayAudio` does). A transcode takes the lease into
//! the transcode instead, which frees it with the ffmpeg slot. Remote reads
//! stream by range. A remote read that names a format the plan transcodes
//! to (the web player asking for a format the browser can play) is spooled
//! to a temp file first, as a plugin URL is, because ffmpeg reads only
//! local files. The source format is the upstream content type's, and the
//! same [`decide()`] call and limits apply.
//!
//! [`StreamEngine`]: super::engine::StreamEngine
//! [`StreamParams`]: super::engine::StreamParams
//! [`decide()`]: super::transcode::decide

use std::path::PathBuf;
use std::sync::Arc;

use super::leases::DirectGate;
use super::local_files::{CatalogFile, LibraryFiles, LocateError, LocatedFile, PathRefusal};
use super::plugin_url;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::ConnectApps;
use droppedneedle_plugins::capabilities::stream::{open_url, proxy_chunks};
use futures_util::StreamExt as _;

use super::engine::{
    AudioSource, Conversion, MediaBody, StreamEngine, StreamFault, StreamMedia, StreamOpen,
    content_type_for_extension, format_for_content_type,
};
use super::leases::OwnedDirectLease;
use super::transcode::{
    StreamPlan, TrackInfo, TranscodeError, Transcoder, decide, estimate_size, out_media_type,
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

/// What the gateway serves `streaming_source` plugins under
/// [`AudioSource::Plugin`] through.
pub struct PluginStreams {
    /// The host whose `streaming_source` plugins answer.
    pub host: Arc<droppedneedle_plugins::host::PluginHost>,
    /// Proxies plugin URLs. It must not follow redirects on its own,
    /// because every hop is re-checked.
    pub no_redirect: reqwest::Client,
    /// Holds the temp files plugin transcodes read.
    pub spool_dir: PathBuf,
}

/// One source-keyed gateway: local reads confined to the library, injected
/// remote reads, and injected ffmpeg execution behind the routes' engine
/// seam.
pub struct Gateway<R, T> {
    library: LibraryFiles,
    plugins: PluginStreams,
    remote: R,
    transcoder: T,
    settings: Live<ConnectApps>,
    ffmpeg_present: bool,
    direct: Arc<DirectGate>,
}

impl<R, T> Gateway<R, T> {
    /// Build a gateway over the live library, the plugin sources, one
    /// remote reader and one ffmpeg service. Local keys resolve as catalog
    /// track ids, and the root registry is read again on every open, so
    /// root changes apply without a restart. The transcode policy is read
    /// from `settings` on every open, so a change applies to the next
    /// stream. `ffmpeg_present` is whether the builder was given an ffmpeg.
    pub fn new(
        library: LibraryFiles,
        plugins: PluginStreams,
        remote: R,
        transcoder: T,
        settings: Live<ConnectApps>,
        ffmpeg_present: bool,
    ) -> Self {
        Self {
            library,
            plugins,
            remote,
            transcoder,
            settings,
            ffmpeg_present,
            direct: Arc::new(DirectGate::new()),
        }
    }

    /// Whether this gateway can transcode: it was built with an ffmpeg.
    /// The compat APIs read it to decide what they offer.
    pub fn can_transcode(&self) -> bool {
        self.ffmpeg_present
    }

    /// Whether a stream that asks for a format is converted now; `None`
    /// while the `connect_apps` section does not read.
    pub fn conversion(&self) -> Option<Conversion> {
        let settings = crate::transcode_policy(&self.settings)?;
        Some(if !self.ffmpeg_present {
            Conversion::Unavailable
        } else if settings.transcoding_enabled {
            Conversion::On
        } else {
            Conversion::Off
        })
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
            source => self.stream_remote(source, &request, head_only, lease).await,
        }
    }
}

/// A resolved local read: the file and the folder it is read beneath, its
/// content type, and the transcode verdict.
struct LocalRead {
    file: LocatedFile,
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
            file,
            content_type,
            plan,
        } = read;
        match plan {
            StreamPlan::Direct { .. } => {
                let (_, metadata) = open_local(&file).await?;
                Ok(StreamMedia {
                    content_type: content_type.to_owned(),
                    total_len: metadata.len(),
                    transcoded: false,
                    estimated_len: None,
                    body: MediaBody::File(file),
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
                    let (source, _) = open_local(&file).await?;
                    let chunks = self
                        .transcoder
                        .stream(source, &plan, &request.user_id, Some(lease))
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
        let (row, file) = catalog_file(&self.library, &request.key).await?;
        let track = TrackInfo {
            file_format: row.file_format,
            bitrate_kbps: row.bitrate_kbps,
            duration_seconds: row.duration_seconds.unwrap_or(0.0),
        };
        self.read_plan(file, Some(track), request)
    }

    /// Content type and transcode plan for one resolved local file. A file
    /// with no catalog facts is known only by its extension.
    fn read_plan(
        &self,
        file: LocatedFile,
        track: Option<TrackInfo>,
        request: &StreamOpen,
    ) -> Result<LocalRead, StreamFault> {
        let extension = file
            .path
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
            file,
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
        let settings =
            crate::transcode_policy(&self.settings).ok_or_else(|| StreamFault::Internal {
                cause: "connect_apps settings cannot be read".to_owned(),
            })?;
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

    /// One media server read: the upstream bytes as they come, or, when the
    /// request names a format the plan transcodes to, ffmpeg's output over
    /// a spooled copy. HEAD answers without spooling.
    async fn stream_remote(
        &self,
        source: AudioSource,
        request: &StreamOpen,
        head_only: bool,
        lease: OwnedDirectLease,
    ) -> Result<StreamMedia, StreamFault> {
        let mut media = self
            .remote
            .open_stream(source, &request.key, &request.user_id, head_only)
            .await?;
        let asked = request
            .params
            .format
            .as_deref()
            .is_some_and(|format| !format.is_empty() && !format.eq_ignore_ascii_case("raw"));
        if !asked {
            media.lease = Some(lease);
            return Ok(media);
        }
        let format = format_for_content_type(&media.content_type);
        let track = TrackInfo {
            file_format: format.clone(),
            bitrate_kbps: None,
            duration_seconds: 0.0,
        };
        let plan = self.plan_for(&track, request)?;
        let Some(out_format) = plan.out_format() else {
            media.lease = Some(lease);
            return Ok(media);
        };
        let mut out = StreamMedia {
            content_type: out_media_type(&out_format).to_owned(),
            total_len: 0,
            transcoded: true,
            estimated_len: estimated_len(request, &plan),
            body: MediaBody::Empty,
            lease: None,
        };
        if head_only {
            out.lease = Some(lease);
            return Ok(out);
        }
        let upstream = |cause: String| StreamFault::Upstream { source, cause };
        let chunks = match media.body {
            MediaBody::Spans(open) if plugin_url::within_cap(media.total_len) => {
                open(0, media.total_len)
            }
            MediaBody::Spans(_) => {
                return Err(upstream(format!(
                    "the stream is over the size cap ({} bytes)",
                    media.total_len
                )));
            }
            MediaBody::Chunks(chunks) => chunks,
            MediaBody::Bytes(bytes) => futures_util::stream::once(async { Ok(bytes) }).boxed(),
            MediaBody::File(_) | MediaBody::Empty => {
                return Err(StreamFault::Internal {
                    cause: "a remote read answered no body to transcode".to_owned(),
                });
            }
        };
        let spool = plugin_url::Spool::fill(chunks, &format, &self.plugins.spool_dir)
            .await
            .map_err(|error| upstream(format!("could not be fetched for transcoding: {error}")))?;
        out.body = self.transcode_spool(spool, &plan, request, lease).await?;
        Ok(out)
    }

    /// ffmpeg's output over a spooled file. The spool lives inside the body
    /// stream and goes with it.
    async fn transcode_spool(
        &self,
        spool: plugin_url::Spool,
        plan: &StreamPlan,
        request: &StreamOpen,
        lease: OwnedDirectLease,
    ) -> Result<MediaBody, StreamFault> {
        let source = tokio::fs::File::open(spool.path())
            .await
            .map_err(io_fault)?
            .into_std()
            .await;
        let chunks = self
            .transcoder
            .stream(source, plan, &request.user_id, Some(lease))
            .await
            .map_err(transcode_fault)?;
        Ok(MediaBody::Chunks(
            chunks
                .map(move |chunk| {
                    let _keep = &spool;
                    chunk
                })
                .boxed(),
        ))
    }

    /// Every library root a plugin path may point into.
    fn all_library_roots(&self) -> Vec<PathBuf> {
        self.library
            .registry()
            .roots()
            .iter()
            .map(|root| root.path.clone())
            .collect()
    }

    /// Ask `streaming_source` plugins for one recording. A file answer is
    /// served like a local file (ranges, transcodes). A URL answer is
    /// proxied with every hop re-checked: by range when its size is known,
    /// and through a temp file when the plan transcodes.
    async fn stream_plugin(
        &self,
        request: &StreamOpen,
        head_only: bool,
        lease: OwnedDirectLease,
    ) -> Result<StreamMedia, StreamFault> {
        use droppedneedle_plugins::capabilities::stream::PluginStream;

        let PluginStreams {
            host,
            no_redirect: client,
            ..
        } = &self.plugins;
        let roots = self.all_library_roots();
        let resolved = host
            .resolve_stream(&request.key, &request.user_id, &roots)
            .await
            .ok_or(StreamFault::NotFound)?;
        match resolved {
            PluginStream::File { path, .. } => {
                // The plugin layer resolved the path to its real spelling,
                // inside a folder the plugin may serve from.
                let root = path.parent().ok_or(StreamFault::NotFound)?.to_path_buf();
                let read = self.read_plan(LocatedFile { root, path }, None, request)?;
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
        let chunks = proxy_chunks(response).boxed();
        let spool = plugin_url::Spool::fill(chunks, format, &self.plugins.spool_dir)
            .await
            .map_err(|error| StreamFault::Upstream {
                source: AudioSource::Plugin,
                cause: format!(
                    "plugin {plugin} stream URL could not be fetched for transcoding: {error}"
                ),
            })?;
        media.body = self.transcode_spool(spool, &plan, request, lease).await?;
        Ok(media)
    }
}

/// The catalog row and file of track `track_id`. Unknown tracks and files
/// gone from disk are 404; a file that resolves outside the library roots
/// is 403 with the message in [`OUTSIDE_MESSAGE`].
async fn catalog_file(
    library: &LibraryFiles,
    track_id: &str,
) -> Result<(CatalogFile, LocatedFile), StreamFault> {
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

/// Refusal for a library file that resolves outside every root.
pub const OUTSIDE_MESSAGE: &str = "Access denied: path is outside the music directory";

/// Fixed sandbox refusal: the key never reaches the wire.
fn forbidden() -> StreamFault {
    StreamFault::Forbidden {
        message: "Playback not allowed".to_owned(),
    }
}

/// Open a local file beneath its folder, off the async workers, with its
/// metadata. A link met below the folder is refused like a key that
/// escapes it.
async fn open_local(file: &LocatedFile) -> Result<(std::fs::File, std::fs::Metadata), StreamFault> {
    let file = file.clone();
    tokio::task::spawn_blocking(move || {
        let opened = file.open()?;
        let metadata = opened.metadata()?;
        Ok((opened, metadata))
    })
    .await
    .map_err(|error| StreamFault::Internal {
        cause: error.to_string(),
    })?
    .map_err(|error: std::io::Error| {
        if droppedneedle_kernel::beneath::met_a_link(&error) {
            forbidden()
        } else if error.kind() == std::io::ErrorKind::InvalidInput {
            // Not a regular file.
            StreamFault::NotFound
        } else {
            io_fault(error)
        }
    })
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
