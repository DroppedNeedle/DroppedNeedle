//! Stream-gateway engine: one source-keyed byte source behind the routes.
//!
//! [`Gateway`] implements the routes' [`StreamEngine`] seam: it takes a
//! direct lease, resolves local files under a sandboxed root or proxied
//! remote bytes, runs the transcode [`decide()`] policy for local files,
//! and hands the routes whole bytes plus response metadata. Range slicing,
//! 206/416 decisions, and headers stay in the routes file.
//!
//! Two honest limits, both forced by the whole-bytes seam: the direct lease
//! covers the open+read only (it releases before the response is sent, so
//! it bounds read concurrency, not response concurrency), and remote reads
//! are direct-only (the ffmpeg service takes a local path; per-source
//! server-side transcode stays a remotes-adapter concern).
//!
//! [`StreamEngine`]: super::routes::StreamEngine
//! [`decide()`]: super::transcode::decide

use std::path::{Path, PathBuf};

use super::leases::DirectGate;
use super::routes::{
    AudioSource, OpenMedia, StreamEngine, StreamFault, StreamOpen, content_type_for_extension,
};
use super::transcode::{
    StreamPlan, TrackInfo, TranscodeBody as _, TranscodeError, TranscodeSettings, Transcoder,
    decide, estimate_size, out_media_type,
};

/// One proxied remote read: upstream bytes plus the upstream content type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMedia {
    /// Upstream-resolved content type (upstream wins for remotes).
    pub content_type: String,
    /// Whole object bytes.
    pub bytes: Vec<u8>,
}

/// Remote byte source. The integrator implements this over the remotes
/// adapter (per-user connections + credential store); tests script it.
pub trait RemoteReader: Send + Sync {
    /// Fetch one remote object, or fail with a routable [`StreamFault`].
    fn fetch(
        &self,
        source: AudioSource,
        key: &str,
        user_id: &str,
    ) -> impl Future<Output = Result<RemoteMedia, StreamFault>> + Send;
}

/// One source-keyed gateway: sandboxed local reads, injected remote reads,
/// and injected ffmpeg execution behind the routes' engine seam.
pub struct Gateway<R, T> {
    local_root: PathBuf,
    library_roots: Option<droppedneedle::library::wiring::RootSource>,
    remote: R,
    transcoder: T,
    settings: TranscodeSettings,
    ffmpeg_present: bool,
    direct: DirectGate,
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
            library_roots: None,
            remote,
            transcoder,
            settings,
            ffmpeg_present,
            direct: DirectGate::new(),
        }
    }

    /// Resolve local reads against the live library root registry
    /// instead of the constructor root. The registry re-reads on
    /// every open, so root changes apply without a restart; with no
    /// usable root configured, local reads honestly 404. Root ids
    /// inside playback keys stay a catalog-slice concern: bare keys
    /// resolve under the primary root.
    pub fn with_library_roots(mut self, roots: droppedneedle::library::wiring::RootSource) -> Self {
        self.library_roots = Some(roots);
        self
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
            source => self.open_remote(source, &request).await,
        }
    }
}

impl<R: RemoteReader, T: Transcoder> Gateway<R, T> {
    /// Sandboxed local read with the transcode policy applied.
    async fn open_local(&self, request: &StreamOpen) -> Result<OpenMedia, StreamFault> {
        let path = self.sandboxed_path(&request.key)?;
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default();
        let content_type =
            content_type_for_extension(extension).ok_or_else(|| StreamFault::InvalidInput {
                message: "Unsupported audio format".to_owned(),
            })?;
        // Track facts for decide(): the extension is exact; bitrate and
        // duration are unknown at this layer, so a client cap alone never
        // triggers a transcode here — only a codec mismatch does.
        let track = TrackInfo {
            file_format: extension.to_owned(),
            bitrate_kbps: None,
            duration_seconds: 0.0,
        };
        let force_original = request
            .params
            .format
            .as_deref()
            .is_some_and(|format| format.eq_ignore_ascii_case("raw"));
        let plan = decide(
            &track,
            request.params.format.as_deref(),
            request.params.max_bitrate_kbps,
            force_original,
            0.0,
            &self.settings,
            self.ffmpeg_present,
        );
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
                let Some(out_format) = plan.out_format() else {
                    return Err(StreamFault::Internal {
                        cause: "transcode plan carries no codec".to_owned(),
                    });
                };
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
                    estimated_len: request
                        .params
                        .estimate_content_length
                        .then(|| estimate_size(&plan))
                        .flatten()
                        .filter(|size| *size > 0),
                    bytes,
                })
            }
        }
    }

    /// Proxied remote read. Direct-only: the ffmpeg service takes a local
    /// path, so transcode hints are ignored here and server-side transcode
    /// stays a remotes-adapter concern.
    async fn open_remote(
        &self,
        source: AudioSource,
        request: &StreamOpen,
    ) -> Result<OpenMedia, StreamFault> {
        let media = self
            .remote
            .fetch(source, &request.key, &request.user_id)
            .await?;
        Ok(OpenMedia {
            content_type: media.content_type,
            total_len: media.bytes.len() as u64,
            transcoded: false,
            estimated_len: None,
            bytes: media.bytes,
        })
    }

    /// Join `key` under the music root, refusing anything that escapes it.
    /// The refusal message is fixed: the key never reaches the wire.
    /// Component screening runs first; then both sides canonicalize so a
    /// symlink inside the root cannot point at a file outside it. Paths
    /// that do not resolve (missing files, missing root) skip the prefix
    /// check and fall through to the read, which reports them honestly.
    /// With library roots wired, the primary registry root replaces the
    /// constructor root; an empty registry 404s instead of reading the
    /// stale fallback.
    fn sandboxed_path(&self, key: &str) -> Result<PathBuf, StreamFault> {
        if key.is_empty() || Path::new(key).is_absolute() {
            return Err(forbidden());
        }
        let root = match &self.library_roots {
            Some(source) => {
                match droppedneedle::library::scan::roots::StreamRootSeam::new(source())
                    .primary_music_root()
                {
                    Some(primary) => primary,
                    None => return Err(StreamFault::NotFound),
                }
            }
            None => self.local_root.clone(),
        };
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
