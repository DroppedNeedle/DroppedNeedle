//! Transcode policy and execution engine.
//!
//! A faithful port of v2's compat transcode service, with leases from its
//! stream concurrency module. `decide()` keeps the v2
//! rule order and quirk citations; `build_cmd()` keeps the exact ffmpeg argv;
//! the streaming body keeps the read timeout and the stderr capture.
//!
//! Engine only: there are no routes here. The gateway owns the HTTP
//! surface and consumes the [`Transcoder`] seam defined below.
//!
//! # Execution
//!
//! ffmpeg runs as a `tokio::process` child with `kill_on_drop`. One task
//! per transcode pumps its stdout into a one-chunk channel that the
//! response body reads, and owns the child and both leases (the ffmpeg
//! slot and the direct slot the open took). Every way the body can end
//! goes through that task: natural EOF settles the exit, a dropped body
//! or a failed read kills and reaps ffmpeg, and a body nobody reads for
//! [`IDLE_DEADLINE`] is stopped the same way. Leases release when the task
//! ends, exactly once. A second task drains stderr into the bounded
//! capture for the nonzero-exit warning.
//!
//! # Seams
//!
//! * Leases: [`TranscodeGate`] (see [`super::leases`]).
//! * Service: [`Transcoder`] is the trait the gateway calls,
//!   [`FfmpegTranscoder`] the implementation. [`FfmpegSpawner`] and
//!   [`FfmpegChild`] put the process behind a trait so tests script it.
//! * Inputs: [`TrackInfo`] holds the source facts `decide()` reads, which
//!   the gateway takes from the catalog row (`file_format`, `bit_rate` in
//!   kbps, `duration_seconds`);
//!   [`TranscodeSettings`] mirrors the `connect_apps` section
//!   (`transcoding_enabled`, `transcode_default_format`,
//!   `transcode_max_bitrate_kbps`).
//! * Failures: [`TranscodeError::Capacity`] is the gateway's 429 with
//!   `Retry-After: 1`, as v2's routers answer it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt as _;
use thiserror::Error;
use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc;

use super::leases::{OwnedDirectLease, TranscodeGate, TranscodeLease};
use super::routes::ChunkStream;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

// ---------------------------------------------------------------------------
// Constants (v2 values, unchanged)
// ---------------------------------------------------------------------------

/// Supported transcode output codecs, matching v2 `_SUPPORTED_OUT`.
pub const SUPPORTED_OUT_FORMATS: &[&str] = &["mp3", "opus"];

/// Internal format name for AAC in MPEG-TS, the segment format the
/// Jellyfin HLS playlist serves. [`decide`] produces it only when it is
/// asked for by this name.
pub const HLS_SEGMENT_FORMAT: &str = "aac-ts";

/// Output bitrate when the client sets no cap, per codec (Navidrome's
/// defaults): enough for transparent playback without spending the full
/// server ceiling on every stream. Unknown names read as MP3.
pub fn default_bitrate_kbps(format: &str) -> i64 {
    match format.to_ascii_lowercase().as_str() {
        "opus" => 128,
        "aac" | HLS_SEGMENT_FORMAT => 256,
        _ => 192,
    }
}

/// Output bitrate for a transcode: the client cap when it set one, else the
/// codec default; never above the server ceiling, never below
/// [`MIN_BITRATE_KBPS`].
pub fn out_bitrate_kbps(format: &str, client_cap_kbps: Option<i64>, server_max_kbps: i64) -> i64 {
    let wanted = match client_cap_kbps {
        Some(cap) if cap > 0 => cap,
        _ => default_bitrate_kbps(format),
    };
    wanted.min(server_max_kbps).max(MIN_BITRATE_KBPS)
}

/// Lowest output bitrate v2 will emit, in kbps.
pub const MIN_BITRATE_KBPS: i64 = 64;

/// "No cap" sentinel for a 0 or unset client bitrate, matching v2 `_HUGE`.
/// Finamp sends 999999999 to mean uncapped, which lands under this the same way.
pub const NO_CAP_BITRATE_KBPS: i64 = 1_000_000_000;

/// Stream read size, matching v2 `STREAM_CHUNK_SIZE` (64 KiB).
pub const STREAM_CHUNK_SIZE: usize = 64 * 1024;

/// Per-chunk ffmpeg read timeout, matching v2 `_READ_TIMEOUT_S`.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Wait for natural ffmpeg exit after EOF, matching v2's 5s `proc.wait()`.
pub const EXIT_WAIT: Duration = Duration::from_secs(5);

/// How long a transcode waits for its reader before it stops ffmpeg and
/// frees both slots. Long enough for an ordinary pause and for a gapless
/// player that opened the next track at the start of a long one; a
/// forgotten tab or a client that stopped reading without closing gives
/// its slots back instead of holding them until the connection dies.
/// Transcodes cannot resume at an offset, so the track may need playing
/// again after that.
pub const IDLE_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// Bounded stderr capture for the nonzero-exit warning, v2's 2000 bytes.
pub const STDERR_CAPTURE_BYTES: usize = 2000;

// ---------------------------------------------------------------------------
// Policy types
// ---------------------------------------------------------------------------

/// Transcode output codec. Only these two exist; anything else a client asks
/// for falls back to the configured default (v2 `_SUPPORTED_OUT` behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutFormat {
    /// MP3 via libmp3lame, muxed as mp3.
    #[default]
    Mp3,
    /// Opus via libopus, muxed as ogg.
    Opus,
    /// AAC muxed as MPEG-TS: the Jellyfin HLS segment format.
    AacTs,
}

impl OutFormat {
    /// The format name clients and the policy use.
    pub fn name(self) -> &'static str {
        match self {
            OutFormat::Mp3 => "mp3",
            OutFormat::Opus => "opus",
            OutFormat::AacTs => HLS_SEGMENT_FORMAT,
        }
    }

    /// The container the output rides in, as players name it.
    pub fn container(self) -> &'static str {
        match self {
            OutFormat::Mp3 => "mp3",
            OutFormat::Opus => "ogg",
            OutFormat::AacTs => "ts",
        }
    }
}

/// The source facts `decide()` reads.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    /// Source container/codec tag, e.g. "flac". Compared case-insensitively.
    pub file_format: String,
    /// Source bitrate in kbps, when known.
    pub bitrate_kbps: Option<i64>,
    /// Source duration in seconds.
    pub duration_seconds: f64,
}

/// The settings fields `decide()` reads. Minimal mirror of the `connect_apps`
/// section; the gateway maps its three fields onto this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeSettings {
    /// Master switch (`transcoding_enabled`).
    pub transcoding_enabled: bool,
    /// Output codec when the client request names none usable
    /// (`transcode_default_format`).
    pub default_format: OutFormat,
    /// Quality ceiling applied while transcoding, never a trigger
    /// (`transcode_max_bitrate_kbps`, validated 32-1411 upstream).
    pub max_bitrate_kbps: i64,
}

impl Default for TranscodeSettings {
    fn default() -> Self {
        Self {
            transcoding_enabled: true,
            default_format: OutFormat::Mp3,
            max_bitrate_kbps: 320,
        }
    }
}

/// Direct-play-vs-transcode outcome. The gateway serves the file itself on
/// [`StreamPlan::Direct`] and calls [`Transcoder::stream`] on
/// [`StreamPlan::Transcode`].
#[derive(Debug, Clone, PartialEq)]
pub enum StreamPlan {
    /// Serve the original bytes. Covers every landing: explicit original
    /// request, transcoding disabled, ffmpeg absent, and "no reason to
    /// transcode".
    Direct {
        /// Source duration, carried for the response headers.
        source_duration_seconds: f64,
    },
    /// Run ffmpeg with the decided codec, bitrate, and seek offset.
    Transcode {
        /// Decided output codec.
        out_format: OutFormat,
        /// Decided output bitrate in kbps (client cap, server ceiling, floor).
        out_bitrate_kbps: i64,
        /// Seek offset in seconds (`-ss`, transcode only).
        start_seconds: f64,
        /// Source duration, carried for the size estimate.
        source_duration_seconds: f64,
    },
}

impl StreamPlan {
    /// True for the transcode landing.
    pub fn is_transcode(&self) -> bool {
        matches!(self, StreamPlan::Transcode { .. })
    }

    /// Decided output codec on the transcode landing, `None` when direct.
    /// The gateway reads this for the response content type.
    pub fn out_format(&self) -> Option<OutFormat> {
        match self {
            StreamPlan::Transcode { out_format, .. } => Some(*out_format),
            StreamPlan::Direct { .. } => None,
        }
    }

    /// Source duration on either landing.
    pub fn source_duration_seconds(&self) -> f64 {
        match self {
            StreamPlan::Direct {
                source_duration_seconds,
            }
            | StreamPlan::Transcode {
                source_duration_seconds,
                ..
            } => *source_duration_seconds,
        }
    }
}

// ---------------------------------------------------------------------------
// decide()
// ---------------------------------------------------------------------------

/// Direct-play-vs-transcode policy. Rules run in order, exactly as in v2:
///
/// 1. Hard outs: `force_original`, transcoding disabled, or ffmpeg absent
///    always land direct (ffmpeg-absent is a silent fallback, never an error).
/// 2. The client bitrate cap below the source bitrate triggers a transcode;
///    a 0 or unset cap means "no cap" (Feishin sends 0; Finamp sends
///    999999999; both must direct-play).
/// 3. A codec change triggers a transcode, except `raw`, which is never a
///    mismatch (Subsonic `format=raw` short-circuits to the file upstream of
///    `decide()`; this rule is the backstop inside it).
/// 4. With no client cap the output takes the codec default (opus 128,
///    mp3 192), not the server max; the output never drops below
///    [`MIN_BITRATE_KBPS`].
/// 5. The server max is a quality ceiling applied while transcoding, never a
///    trigger: a lossless file over the server cap still direct-plays when
///    nothing was requested (Jellify regression), while an explicit codec
///    request on that same file transcodes clamped to the server cap (Manet
///    `/universal?AudioCodec=mp3`).
#[allow(clippy::too_many_arguments)]
pub fn decide(
    track: &TrackInfo,
    requested_format: Option<&str>,
    max_bitrate_kbps: Option<i64>,
    force_original: bool,
    start_seconds: f64,
    settings: &TranscodeSettings,
    ffmpeg_available: bool,
) -> StreamPlan {
    let duration = track.duration_seconds;
    if force_original || !settings.transcoding_enabled || !ffmpeg_available {
        return StreamPlan::Direct {
            source_duration_seconds: duration,
        };
    }

    let client_ceiling = match max_bitrate_kbps {
        Some(capped) if capped > 0 => capped,
        _ => NO_CAP_BITRATE_KBPS,
    };
    let source_format = track.file_format.to_lowercase();
    let requested = requested_format.unwrap_or("").to_lowercase();
    let codec_mismatch = !requested.is_empty() && requested != "raw" && requested != source_format;
    let over_ceiling = client_ceiling < track.bitrate_kbps.unwrap_or(0);

    if !codec_mismatch && !over_ceiling {
        return StreamPlan::Direct {
            source_duration_seconds: duration,
        };
    }

    let out_format = match requested.as_str() {
        "mp3" => OutFormat::Mp3,
        "opus" => OutFormat::Opus,
        HLS_SEGMENT_FORMAT => OutFormat::AacTs,
        _ => settings.default_format,
    };
    let bitrate = out_bitrate_kbps(
        out_format.name(),
        max_bitrate_kbps,
        settings.max_bitrate_kbps,
    );
    StreamPlan::Transcode {
        out_format,
        out_bitrate_kbps: bitrate,
        start_seconds: start_seconds.max(0.0),
        source_duration_seconds: duration,
    }
}

// ---------------------------------------------------------------------------
// ffmpeg argv contract
// ---------------------------------------------------------------------------

/// Build the exact ffmpeg argv for a transcode plan, ported argument by
/// argument from v2 `_mp3_cmd` / `_opus_cmd`:
///
/// ```text
/// ffmpeg -hide_banner -loglevel error -nostdin [-ss <s.ms>] -i <path>
///   -vn -map 0:a:0 -c:a <libmp3lame|libopus> -b:a <n>k
///   -f <mp3|ogg> pipe:1
/// ```
///
/// `-ss` comes before `-i` (fast input seek) and only when the offset is
/// positive; output always goes to piped stdout. Returns `None` for a direct
/// plan, which never spawns ffmpeg.
pub fn build_cmd(source_path: &Path, plan: &StreamPlan) -> Option<Vec<String>> {
    let StreamPlan::Transcode {
        out_format,
        out_bitrate_kbps,
        start_seconds,
        ..
    } = plan
    else {
        return None;
    };
    let mut argv = vec![
        "ffmpeg".to_owned(),
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
        "-nostdin".to_owned(),
    ];
    if *start_seconds > 0.0 {
        argv.push("-ss".to_owned());
        argv.push(format!("{start_seconds:.3}"));
    }
    argv.push("-i".to_owned());
    argv.push(source_path.to_string_lossy().into_owned());
    argv.push("-vn".to_owned());
    argv.push("-map".to_owned());
    argv.push("0:a:0".to_owned());
    argv.push("-c:a".to_owned());
    match out_format {
        OutFormat::Mp3 => {
            argv.push("libmp3lame".to_owned());
            argv.push("-b:a".to_owned());
            argv.push(format!("{out_bitrate_kbps}k"));
            argv.push("-f".to_owned());
            argv.push("mp3".to_owned());
        }
        OutFormat::Opus => {
            argv.push("libopus".to_owned());
            argv.push("-b:a".to_owned());
            argv.push(format!("{out_bitrate_kbps}k"));
            argv.push("-f".to_owned());
            argv.push("ogg".to_owned());
        }
        OutFormat::AacTs => {
            argv.push("aac".to_owned());
            argv.push("-b:a".to_owned());
            argv.push(format!("{out_bitrate_kbps}k"));
            argv.push("-f".to_owned());
            argv.push("mpegts".to_owned());
        }
    }
    argv.push("pipe:1".to_owned());
    Some(argv)
}

/// Response content type for a transcode output codec (v2 `out_media_type`).
pub fn out_media_type(out_format: &OutFormat) -> &'static str {
    match out_format {
        OutFormat::Mp3 => "audio/mpeg",
        OutFormat::Opus => "audio/ogg",
        OutFormat::AacTs => "video/mp2t",
    }
}

/// Back-of-envelope transcode size in bytes, for the optional Content-Length
/// (`estimateContentLength`): bitrate over the remaining duration after the
/// seek offset. `None` for a direct plan.
pub fn estimate_size(plan: &StreamPlan) -> Option<u64> {
    let StreamPlan::Transcode {
        out_bitrate_kbps,
        start_seconds,
        source_duration_seconds,
        ..
    } = plan
    else {
        return None;
    };
    let seconds = (source_duration_seconds - start_seconds).max(0.0);
    let bytes = *out_bitrate_kbps as f64 * 1000.0 / 8.0 * seconds;
    Some(bytes.max(0.0) as u64)
}

/// The first `ffmpeg` executable on `PATH`, or `None` when there is none
/// (v2 `ffmpeg_available`, the `shutil.which` half). The server looks once,
/// at boot; whether it found one is whether it can transcode.
pub fn ffmpeg_on_path() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("ffmpeg"))
            .find(|candidate| is_executable_file(candidate))
    })
}

/// Executable-bit check behind the `PATH` scan, matching `shutil.which`.
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Non-unix fallback for the `PATH` scan.
#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Transcode engine failures. Messages stay generic on purpose: they must
/// never carry source paths or other request details (v2 keeps paths out of
/// its ffmpeg log lines, and 5xx bodies stay fixed upstream).
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TranscodeError {
    /// The transcode gate is full for this caller. The gateway answers 429
    /// plus `Retry-After`.
    #[error("transcode capacity exhausted")]
    Capacity,
    /// ffmpeg would not start. The detail is an OS error kind, never a path.
    #[error("ffmpeg failed to start: {0}")]
    Spawn(String),
    /// One ffmpeg stdout read ran past [`READ_TIMEOUT`]; ffmpeg was reaped.
    #[error("ffmpeg read timed out")]
    ReadTimeout,
    /// Nobody read the output for [`IDLE_DEADLINE`]; ffmpeg was reaped.
    #[error("transcode stopped: nobody read it")]
    Idle,
    /// ffmpeg I/O failed. The detail is a static context string, never a path.
    #[error("transcode I/O failed: {0}")]
    Io(String),
    /// `stream` was called with a direct plan; a programming error, since the
    /// gateway serves direct plans itself without touching this engine.
    #[error("plan is direct; nothing to transcode")]
    NotTranscoding,
}

/// OS error kinds are safe to surface; full I/O messages can quote paths.
fn sanitized_io_detail(err: &std::io::Error) -> String {
    err.kind().to_string()
}

// ---------------------------------------------------------------------------
// Process seam (dependency-injected ffmpeg)
// ---------------------------------------------------------------------------

/// One running ffmpeg child, behind a trait so tests script it instead of
/// needing a real binary on `PATH`.
pub trait FfmpegChild: Send + 'static {
    /// Read the next stdout chunk; `None` is natural EOF.
    fn read_chunk(
        &mut self,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, TranscodeError>> + Send;

    /// Settle after EOF: wait for the exit (at most [`EXIT_WAIT`], then
    /// kill) and log a nonzero one with the captured stderr.
    fn finish(&mut self) -> impl Future<Output = ()> + Send;

    /// Kill the child and wait until it is reaped.
    fn stop(&mut self) -> impl Future<Output = ()> + Send;
}

/// Spawns ffmpeg children. The engine depends only on this trait; production
/// passes [`ProcessSpawner`], tests pass a scripted fake.
pub trait FfmpegSpawner: Send + Sync {
    /// Child type this spawner produces.
    type Child: FfmpegChild;

    /// Start one child for already-built argv (see [`build_cmd`]).
    fn spawn(&self, argv: &[String]) -> Result<Self::Child, TranscodeError>;
}

/// Production spawner: one `tokio::process` child per transcode.
pub struct ProcessSpawner {
    ffmpeg: PathBuf,
}

impl ProcessSpawner {
    /// Spawner pinned to one binary path.
    pub fn with_path(path: PathBuf) -> Self {
        Self { ffmpeg: path }
    }
}

impl FfmpegSpawner for ProcessSpawner {
    type Child = ProcessChild;

    fn spawn(&self, argv: &[String]) -> Result<ProcessChild, TranscodeError> {
        let Some((_, args)) = argv.split_first() else {
            return Err(TranscodeError::Spawn("empty ffmpeg argv".to_owned()));
        };
        let mut child = tokio::process::Command::new(&self.ffmpeg)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| TranscodeError::Spawn(sanitized_io_detail(&err)))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TranscodeError::Spawn("ffmpeg stdout unavailable".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| TranscodeError::Spawn("ffmpeg stderr unavailable".to_owned()))?;
        let stderr_text = Arc::new(Mutex::new(String::new()));
        tokio::spawn(drain_stderr(stderr, Arc::clone(&stderr_text)));
        Ok(ProcessChild {
            child,
            stdout,
            stderr_text,
        })
    }
}

/// Drain a child's stderr to EOF, keeping only the first bytes for the
/// nonzero-exit warning. Port of v2 `_drain_stderr` with its 2000-byte
/// bound. Ends when ffmpeg closes stderr, which it does when it exits.
async fn drain_stderr(mut stderr: tokio::process::ChildStderr, captured: Arc<Mutex<String>>) {
    let mut buf = [0u8; 8192];
    loop {
        let read = match stderr.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut guard = captured.lock().unwrap_or_else(|poison| poison.into_inner());
        let take = read.min(STDERR_CAPTURE_BYTES.saturating_sub(guard.len()));
        if take > 0 {
            guard.push_str(&String::from_utf8_lossy(&buf[..take]));
        }
    }
}

/// Production ffmpeg child. Dropping it kills the process, and the runtime
/// reaps it.
#[derive(Debug)]
pub struct ProcessChild {
    child: tokio::process::Child,
    stdout: tokio::process::ChildStdout,
    stderr_text: Arc<Mutex<String>>,
}

impl ProcessChild {
    /// OS process id while the child has not been reaped.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }
}

impl FfmpegChild for ProcessChild {
    async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, TranscodeError> {
        let mut buf = vec![0u8; STREAM_CHUNK_SIZE];
        match self.stdout.read(&mut buf).await {
            Ok(0) => Ok(None),
            Ok(n) => {
                buf.truncate(n);
                Ok(Some(buf))
            }
            Err(err) => Err(TranscodeError::Io(sanitized_io_detail(&err))),
        }
    }

    async fn finish(&mut self) {
        match tokio::time::timeout(EXIT_WAIT, self.child.wait()).await {
            Ok(Ok(status)) if !status.success() => {
                let detail = self
                    .stderr_text
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .clone();
                tracing::warn!(
                    exit_code = ?status.code(),
                    stderr = %detail,
                    "ffmpeg exited nonzero",
                );
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "ffmpeg exit could not be read"),
            Err(_) => self.stop().await,
        }
    }

    async fn stop(&mut self) {
        // `kill` sends SIGKILL and then waits, so the child is reaped here.
        // An already reaped child refuses the signal, which is fine.
        if let Err(error) = self.child.kill().await {
            tracing::debug!(%error, "ffmpeg kill failed");
        }
    }
}

// ---------------------------------------------------------------------------
// Transcoder seam (consumed by the gateway)
// ---------------------------------------------------------------------------

/// Transcode service seam the gateway consumes. The gateway calls
/// [`decide()`] first and only reaches `stream` on a transcode plan; the
/// engine rejects direct plans defensively.
pub trait Transcoder: Send + Sync {
    /// Take an ffmpeg slot, spawn ffmpeg for the plan, and hand back its
    /// output as a body stream. The ffmpeg slot and `direct`, the direct
    /// slot the open took, are held until the transcode ends, and released
    /// exactly once however it ends: EOF, failure, a dropped body, or
    /// [`IDLE_DEADLINE`] without a read.
    fn stream(
        &self,
        source_path: &Path,
        plan: &StreamPlan,
        principal: &str,
        direct: Option<OwnedDirectLease>,
    ) -> impl Future<Output = Result<ChunkStream, TranscodeError>> + Send;
}

/// The [`Transcoder`] implementation: a spawner plus the ffmpeg gate.
pub struct FfmpegTranscoder<S> {
    spawner: S,
    gate: Arc<TranscodeGate>,
}

impl<S> FfmpegTranscoder<S> {
    /// Build the service from its spawner and gate.
    pub fn new(spawner: S, gate: Arc<TranscodeGate>) -> Self {
        Self { spawner, gate }
    }
}

impl<S: FfmpegSpawner> Transcoder for FfmpegTranscoder<S> {
    async fn stream(
        &self,
        source_path: &Path,
        plan: &StreamPlan,
        principal: &str,
        direct: Option<OwnedDirectLease>,
    ) -> Result<ChunkStream, TranscodeError> {
        let Some(argv) = build_cmd(source_path, plan) else {
            return Err(TranscodeError::NotTranscoding);
        };
        // Lease before spawning, as v2 does; a failed start drops it, so it
        // never wedges a slot.
        let lease = self
            .gate
            .acquire_owned(principal)
            .await
            .map_err(|_| TranscodeError::Capacity)?;
        let child = self.spawner.spawn(&argv)?;
        Ok(start(
            child,
            Held {
                _lease: lease,
                _direct: direct,
            },
        ))
    }
}

/// The slots one transcode holds. Dropping it releases both.
struct Held {
    _lease: TranscodeLease,
    _direct: Option<OwnedDirectLease>,
}

/// Start the pump for `child` and return the body that reads it. The body
/// ends cleanly only after a natural EOF; a transcode stopped any other way
/// ends the body with an error, so a client never takes a cut-off stream
/// for a whole one.
fn start<C: FfmpegChild>(child: C, held: Held) -> ChunkStream {
    let (chunks, receiver) = mpsc::channel(1);
    let complete = Arc::new(AtomicBool::new(false));
    tokio::spawn(pump(child, chunks, held, Arc::clone(&complete)));
    futures_util::stream::unfold(Some(receiver), move |state| {
        let complete = Arc::clone(&complete);
        async move {
            let mut receiver = state?;
            match receiver.recv().await {
                Some(Ok(bytes)) => Some((Ok(bytes), Some(receiver))),
                Some(Err(error)) => Some((Err(std::io::Error::other(error.to_string())), None)),
                None if complete.load(Ordering::Acquire) => None,
                None => Some((
                    Err(std::io::Error::other(TranscodeError::Idle.to_string())),
                    None,
                )),
            }
        }
    })
    .fuse()
    .boxed()
}

/// How a pump ended.
enum End {
    /// ffmpeg reached EOF.
    Eof,
    /// The body was dropped.
    Gone,
    /// A read failed or timed out.
    Failed(TranscodeError),
    /// Nobody read for [`IDLE_DEADLINE`].
    Idle,
}

/// Move ffmpeg's stdout into `chunks` until EOF, failure, a dropped body or
/// the idle deadline, then settle or stop the child and drop `held`. It
/// logs how it ended when that was not EOF or a dropped body.
async fn pump<C: FfmpegChild>(
    mut child: C,
    chunks: mpsc::Sender<Result<Vec<u8>, TranscodeError>>,
    held: Held,
    complete: Arc<AtomicBool>,
) {
    let end = loop {
        let read = tokio::select! {
            () = chunks.closed() => break End::Gone,
            read = tokio::time::timeout(READ_TIMEOUT, child.read_chunk()) => read,
        };
        let bytes = match read {
            Ok(Ok(Some(bytes))) => bytes,
            Ok(Ok(None)) => break End::Eof,
            Ok(Err(error)) => break End::Failed(error),
            Err(_) => break End::Failed(TranscodeError::ReadTimeout),
        };
        match tokio::time::timeout(IDLE_DEADLINE, chunks.send(Ok(bytes))).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => break End::Gone,
            Err(_) => break End::Idle,
        }
    };
    match end {
        End::Eof => {
            child.finish().await;
            drop(held);
            complete.store(true, Ordering::Release);
        }
        End::Gone => {
            child.stop().await;
            drop(held);
        }
        End::Failed(error) => {
            tracing::warn!(%error, "transcode failed");
            child.stop().await;
            drop(held);
            let _ = chunks.try_send(Err(error));
        }
        End::Idle => {
            tracing::info!(
                idle_seconds = IDLE_DEADLINE.as_secs(),
                "transcode stopped: nobody read it"
            );
            child.stop().await;
            drop(held);
        }
    }
}
