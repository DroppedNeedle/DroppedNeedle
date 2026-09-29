//! Stage-6 transcode policy and execution engine.
//!
//! A faithful port of the v2 Python reference
//! (`backend/services/compat/transcode_service.py`, with leases from
//! `backend/services/compat/stream_concurrency.py`). `decide()` keeps the v2
//! rule order and quirk citations; `build_cmd()` keeps the exact ffmpeg argv;
//! the streaming body keeps the disconnect, timeout, and stderr-drain
//! behavior. Nothing is removed or "improved": where v2 has a wart, the port
//! keeps it and says so.
//!
//! Engine only: there are no routes in this slice. The gateway slice owns the
//! HTTP surface and consumes the [`Transcoder`] seam defined below.
//!
//! # Integrator seams
//!
//! * Leases: this slice talks to [`TranscodeLeasePool`] / [`TranscodeLease`]
//!   only. [`LocalTranscodeGate`] is a minimal standalone gate (same 2 global
//!   / 1 per-principal limits as v2) so the engine and its briefs run without
//!   the gateway; unify it with the shared gateway gate by implementing
//!   [`TranscodeLeasePool`] for the shared type and passing that as `P`.
//! * Service: [`Transcoder`] is the trait the gateway calls,
//!   [`FfmpegTranscoder`] the implementation, [`TranscodeBody`] /
//!   [`TranscodeStream`] the per-request byte stream. If the gateway slice
//!   lands its own spelling first, keep one and delete the other.
//! * Inputs: [`TrackInfo`] mirrors the track fields `decide()` reads from the
//!   gateway view model (`file_format`, `bitrate` in kbps, `duration_seconds`);
//!   [`TranscodeSettings`] mirrors the `connect_apps` section
//!   (`transcoding_enabled`, `transcode_default_format`,
//!   `transcode_max_bitrate_kbps`). Map and delete when the shared types land.
//! * Failures: [`TranscodeError::Capacity`] is the gateway's 429. v2 routers
//!   answer capacity exhaustion with status 429 plus `Retry-After: 1`
//!   (`backend/api/compat/subsonic/router.py`, `_stream_decided`); see
//!   [`CAPACITY_STATUS_CODE`] and [`RETRY_AFTER_SECONDS`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::Notify;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

// ---------------------------------------------------------------------------
// Constants (v2 values, unchanged)
// ---------------------------------------------------------------------------

/// Supported transcode output codecs, matching v2 `_SUPPORTED_OUT`.
pub const SUPPORTED_OUT_FORMATS: &[&str] = &["mp3", "opus"];

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

/// Grace period between SIGTERM and SIGKILL, matching v2 `_terminate`.
pub const TERMINATE_WAIT: Duration = Duration::from_secs(3);

/// Bounded stderr capture for the nonzero-exit warning, v2's 2000 bytes.
pub const STDERR_CAPTURE_BYTES: usize = 2000;

/// Transcode pool limits, matching v2 `StreamConcurrencyService` defaults:
/// 2 ffmpeg jobs globally, 1 per principal.
pub const TRANSCODE_GLOBAL_LIMIT: usize = 2;
/// Transcode pool limits, matching v2 `StreamConcurrencyService` defaults:
/// 2 ffmpeg jobs globally, 1 per principal.
pub const TRANSCODE_PRINCIPAL_LIMIT: usize = 1;

/// Bounded lease queue, matching v2 `max_waiters`.
pub const TRANSCODE_MAX_WAITERS: usize = 128;

/// Lease wait deadline before capacity exhaustion, matching v2's 5s timeout.
pub const TRANSCODE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Status the gateway answers on [`TranscodeError::Capacity`], per v2 routers.
pub const CAPACITY_STATUS_CODE: u16 = 429;

/// Retry hint the gateway sends with the 429, per v2 routers.
pub const RETRY_AFTER_SECONDS: u64 = 1;

/// Poll step while waiting on a child process to change state.
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
}

/// The track fields `decide()` reads. Minimal mirror of the gateway view
/// model; unify when the shared type lands (integrator seam).
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
/// section; the gateway maps its three fields onto this (integrator seam).
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
/// 4. The server max is a quality ceiling applied while transcoding, never a
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
        _ => settings.default_format,
    };
    let bitrate = client_ceiling
        .min(settings.max_bitrate_kbps)
        .max(MIN_BITRATE_KBPS);
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
    }
    argv.push("pipe:1".to_owned());
    Some(argv)
}

/// Response content type for a transcode output codec (v2 `out_media_type`).
pub fn out_media_type(out_format: &OutFormat) -> &'static str {
    match out_format {
        OutFormat::Mp3 => "audio/mpeg",
        OutFormat::Opus => "audio/ogg",
    }
}

/// Filename suffix for a transcode output codec (v2 `out_suffix`).
pub fn out_suffix(out_format: &OutFormat) -> &'static str {
    match out_format {
        OutFormat::Mp3 => "mp3",
        OutFormat::Opus => "opus",
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

/// Response headers the gateway sets on a transcode response, ported from v2
/// `TranscodeService.stream`: no ranges, no caching, identity encoding, and
/// the estimated length only when the client asked for it. Empty for direct.
pub fn transcode_response_headers(
    plan: &StreamPlan,
    estimate_content_length: bool,
) -> Vec<(String, String)> {
    if !plan.is_transcode() {
        return Vec::new();
    }
    let mut headers = vec![
        ("Accept-Ranges".to_owned(), "none".to_owned()),
        ("Cache-Control".to_owned(), "no-store".to_owned()),
        ("Content-Encoding".to_owned(), "identity".to_owned()),
    ];
    if estimate_content_length && let Some(size) = estimate_size(plan) {
        headers.push(("Content-Length".to_owned(), size.to_string()));
    }
    headers
}

/// True when an `ffmpeg` executable is on `PATH` (v2 `ffmpeg_available`, the
/// `shutil.which` half). Unlike v2 this is uncached, so a mid-process install
/// is picked up; `decide()` takes the value as a parameter either way.
pub fn ffmpeg_available() -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("ffmpeg"))
            .any(|candidate| is_executable_file(&candidate))
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
    /// Both transcode slots (or the caller's single slot) are busy. The
    /// gateway answers [`CAPACITY_STATUS_CODE`] plus `Retry-After`.
    #[error("transcode capacity exhausted")]
    Capacity,
    /// The client went away mid-transcode; ffmpeg was reaped.
    #[error("client disconnected")]
    ClientDisconnected,
    /// ffmpeg would not start. The detail is an OS error kind, never a path.
    #[error("ffmpeg failed to start: {0}")]
    Spawn(String),
    /// One ffmpeg stdout read ran past [`READ_TIMEOUT`]; ffmpeg was reaped.
    #[error("ffmpeg read timed out")]
    ReadTimeout,
    /// ffmpeg I/O failed. The detail is a static context string, never a path.
    #[error("transcode I/O failed: {0}")]
    Io(String),
    /// `stream` was called with a direct plan; a programming error, since the
    /// gateway serves direct plans itself without touching this engine.
    #[error("plan is direct; nothing to transcode")]
    NotTranscoding,
}

impl TranscodeError {
    /// True for the exhaustion case the gateway maps to 429.
    pub fn is_capacity(&self) -> bool {
        matches!(self, TranscodeError::Capacity)
    }
}

/// OS error kinds are safe to surface; full I/O messages can quote paths.
fn sanitized_io_detail(err: &std::io::Error) -> String {
    err.kind().to_string()
}

// ---------------------------------------------------------------------------
// Lease seam (shared with the gateway slice)
// ---------------------------------------------------------------------------

/// One held transcode slot. Consuming release makes double-release
/// unrepresentable: each lease value releases exactly one slot, once.
pub trait TranscodeLease: Send + 'static {
    /// Free the slot.
    fn release(self);
}

/// Source of transcode leases. The engine depends only on this trait; the
/// integrator unifies it with the shared gateway lease scheme by implementing
/// it for the shared gate type. Exhaustion surfaces as
/// [`TranscodeError::Capacity`] so the gateway can answer its 429.
pub trait TranscodeLeasePool: Send + Sync {
    /// Lease type this pool hands out.
    type Lease: TranscodeLease;

    /// Take a transcode slot for one principal, waiting briefly before
    /// giving up with [`TranscodeError::Capacity`].
    fn acquire_transcode(
        &self,
        principal: &str,
    ) -> impl Future<Output = Result<Self::Lease, TranscodeError>> + Send;
}

/// Gate counters behind [`LocalTranscodeGate`].
#[derive(Debug)]
struct TranscodeGateState {
    /// Slots currently held.
    active: usize,
    /// Slots held per principal.
    active_by_principal: HashMap<String, usize>,
    /// Acquires currently waiting (bounded by [`TRANSCODE_MAX_WAITERS`]).
    waiters: usize,
}

/// Minimal standalone transcode gate: 2 global slots, 1 per principal, a
/// bounded waiter queue, and a 5s wait deadline, matching v2
/// `StreamConcurrencyService` transcode-pool defaults. Waiting acquirers race
/// on a broadcast wake rather than v2's first-eligible scan; at this pool
/// size that is close enough for the standalone engine, and the shared
/// gateway gate remains the real fairness story (integrator seam).
#[derive(Debug)]
pub struct LocalTranscodeGate {
    state: Mutex<TranscodeGateState>,
    changed: Notify,
    global_limit: usize,
    principal_limit: usize,
    max_waiters: usize,
    wait_timeout: Duration,
}

impl LocalTranscodeGate {
    /// Gate with the v2 transcode-pool defaults.
    pub fn new() -> Self {
        Self::with_limits(
            TRANSCODE_GLOBAL_LIMIT,
            TRANSCODE_PRINCIPAL_LIMIT,
            TRANSCODE_MAX_WAITERS,
            TRANSCODE_WAIT_TIMEOUT,
        )
    }

    /// Gate with explicit limits, for briefs that need a short wait deadline.
    pub fn with_limits(
        global_limit: usize,
        principal_limit: usize,
        max_waiters: usize,
        wait_timeout: Duration,
    ) -> Self {
        Self {
            state: Mutex::new(TranscodeGateState {
                active: 0,
                active_by_principal: HashMap::new(),
                waiters: 0,
            }),
            changed: Notify::new(),
            global_limit,
            principal_limit,
            max_waiters,
            wait_timeout,
        }
    }

    /// Slots currently held. Test and observability hook.
    pub fn active(&self) -> usize {
        self.lock_state().active
    }

    /// Acquires currently waiting. Test and observability hook.
    pub fn waiter_count(&self) -> usize {
        self.lock_state().waiters
    }

    /// Lock the counters, tolerating a poisoned mutex by carrying on with
    /// the guarded state (a panicking holder must not wedge the pool).
    fn lock_state(&self) -> MutexGuard<'_, TranscodeGateState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Default for LocalTranscodeGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Decrements the waiter count unless the acquire already accounted for it,
/// so a cancelled acquire cannot leak a waiter slot (v2 drains its waiter
/// deque on every exit path for the same reason).
struct WaiterGuard<'a> {
    gate: &'a LocalTranscodeGate,
    armed: bool,
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut state = self.gate.lock_state();
            state.waiters = state.waiters.saturating_sub(1);
        }
    }
}

/// A slot held on a [`LocalTranscodeGate`].
#[derive(Debug)]
pub struct LocalTranscodeLease {
    gate: Arc<LocalTranscodeGate>,
    principal: String,
}

impl TranscodeLease for LocalTranscodeLease {
    fn release(self) {
        let mut state = self.gate.lock_state();
        state.active = state.active.saturating_sub(1);
        match state.active_by_principal.get(&self.principal).copied() {
            Some(held) if held > 1 => {
                state
                    .active_by_principal
                    .insert(self.principal.clone(), held - 1);
            }
            _ => {
                state.active_by_principal.remove(&self.principal);
            }
        }
        drop(state);
        self.gate.changed.notify_waiters();
    }
}

impl TranscodeLeasePool for Arc<LocalTranscodeGate> {
    type Lease = LocalTranscodeLease;

    async fn acquire_transcode(
        &self,
        principal: &str,
    ) -> Result<LocalTranscodeLease, TranscodeError> {
        {
            let mut state = self.lock_state();
            if state.waiters >= self.max_waiters {
                return Err(TranscodeError::Capacity);
            }
            state.waiters += 1;
        }
        let mut waiter = WaiterGuard {
            gate: self,
            armed: true,
        };
        let granted = tokio::time::timeout(self.wait_timeout, async {
            loop {
                let changed = self.changed.notified();
                {
                    let mut state = self.lock_state();
                    let held = state
                        .active_by_principal
                        .get(principal)
                        .copied()
                        .unwrap_or(0);
                    if state.active < self.global_limit && held < self.principal_limit {
                        state.active += 1;
                        state
                            .active_by_principal
                            .insert(principal.to_owned(), held + 1);
                        state.waiters = state.waiters.saturating_sub(1);
                        waiter.armed = false;
                        return;
                    }
                }
                changed.await;
            }
        })
        .await;
        match granted {
            Ok(()) => Ok(LocalTranscodeLease {
                gate: Arc::clone(self),
                principal: principal.to_owned(),
            }),
            Err(_) => {
                let mut state = self.lock_state();
                state.waiters = state.waiters.saturating_sub(1);
                waiter.armed = false;
                Err(TranscodeError::Capacity)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Process seam (dependency-injected ffmpeg)
// ---------------------------------------------------------------------------

/// One running ffmpeg child, behind a trait so briefs script it instead of
/// needing a real binary on `PATH`.
pub trait FfmpegChild: Send + 'static {
    /// Read the next stdout chunk; `None` is natural EOF.
    fn read_chunk(
        &mut self,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, TranscodeError>> + Send;

    /// Settle after EOF: wait for natural exit (bounded) and note a nonzero
    /// exit. Port of the tail of v2 `_body`.
    fn finish(&mut self) -> impl Future<Output = ()> + Send;

    /// Graceful stop: terminate, brief wait, then kill. Port of v2
    /// `_terminate`.
    fn shutdown(&mut self) -> impl Future<Output = ()> + Send;

    /// Immediate best-effort kill for drop paths, where nothing can wait.
    fn kill(&mut self);
}

/// Spawns ffmpeg children. The engine depends only on this trait; production
/// passes [`StdFfmpegSpawner`], briefs pass a scripted fake.
pub trait FfmpegSpawner: Send + Sync {
    /// Child type this spawner produces.
    type Child: FfmpegChild;

    /// Start one child for already-built argv (see [`build_cmd`]).
    fn spawn(&self, argv: &[String]) -> Result<Self::Child, TranscodeError>;
}

/// Production spawner on top of `std::process`. `tokio::process` is off the
/// table (the `process` feature is not enabled for this tree), so blocking
/// stdout reads run on the blocking pool and stderr drains on a thread.
pub struct StdFfmpegSpawner {
    ffmpeg: PathBuf,
}

impl StdFfmpegSpawner {
    /// Spawner using the `ffmpeg` found on `PATH`, or `None` when there is
    /// none (the `decide()` ffmpeg-absent landing covers that case upstream).
    pub fn detect() -> Option<Self> {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("ffmpeg"))
                .find(|candidate| is_executable_file(candidate))
                .map(|ffmpeg| Self { ffmpeg })
        })
    }

    /// Spawner pinned to one binary path.
    pub fn with_path(path: PathBuf) -> Self {
        Self { ffmpeg: path }
    }

    /// The binary this spawner executes.
    pub fn ffmpeg_path(&self) -> &Path {
        &self.ffmpeg
    }
}

impl FfmpegSpawner for StdFfmpegSpawner {
    type Child = StdFfmpegChild;

    fn spawn(&self, argv: &[String]) -> Result<StdFfmpegChild, TranscodeError> {
        let Some((_, args)) = argv.split_first() else {
            return Err(TranscodeError::Spawn("empty ffmpeg argv".to_owned()));
        };
        let mut child = std::process::Command::new(&self.ffmpeg)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
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
        std::thread::spawn({
            let stderr_text = Arc::clone(&stderr_text);
            move || drain_stderr(stderr, stderr_text)
        });
        Ok(StdFfmpegChild {
            child,
            stdout: Arc::new(Mutex::new(stdout)),
            stderr_text,
        })
    }
}

/// Drain a child stderr to EOF, keeping only the first bytes for the
/// nonzero-exit warning. Port of v2 `_drain_stderr` with its 2000-byte bound.
fn drain_stderr(stderr: ChildStderr, captured: Arc<Mutex<String>>) {
    use std::io::Read as _;
    let mut reader = stderr;
    let mut buf = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        let mut guard = captured.lock().unwrap_or_else(|poison| poison.into_inner());
        let remaining = STDERR_CAPTURE_BYTES.saturating_sub(guard.len());
        if remaining == 0 {
            continue;
        }
        let take = read.min(remaining);
        guard.push_str(&String::from_utf8_lossy(&buf[..take]));
    }
}

/// Production ffmpeg child. Stdout reads hop onto the blocking pool; exit
/// waits poll on `try_wait` so nothing blocks a runtime thread.
#[derive(Debug)]
pub struct StdFfmpegChild {
    child: Child,
    stdout: Arc<Mutex<ChildStdout>>,
    stderr_text: Arc<Mutex<String>>,
}

impl FfmpegChild for StdFfmpegChild {
    async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, TranscodeError> {
        let stdout = Arc::clone(&self.stdout);
        tokio::task::spawn_blocking(move || {
            use std::io::Read as _;
            let mut guard = stdout.lock().unwrap_or_else(|poison| poison.into_inner());
            let mut buf = vec![0u8; STREAM_CHUNK_SIZE];
            match guard.read(&mut buf) {
                Ok(0) => Ok(None),
                Ok(n) => {
                    buf.truncate(n);
                    Ok(Some(buf))
                }
                Err(err) => Err(TranscodeError::Io(sanitized_io_detail(&err))),
            }
        })
        .await
        .map_err(|_| TranscodeError::Io("ffmpeg reader failed".to_owned()))?
    }

    async fn finish(&mut self) {
        let settled = tokio::time::timeout(EXIT_WAIT, async {
            loop {
                match self.child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
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
                        return;
                    }
                    Ok(None) => tokio::time::sleep(CHILD_POLL_INTERVAL).await,
                    Err(_) => return,
                }
            }
        })
        .await;
        if settled.is_err() {
            self.kill();
        }
    }

    async fn shutdown(&mut self) {
        if self
            .child
            .try_wait()
            .map(|ended| ended.is_some())
            .unwrap_or(true)
        {
            return;
        }
        terminate_child(&mut self.child);
        let exited = tokio::time::timeout(TERMINATE_WAIT, async {
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => tokio::time::sleep(CHILD_POLL_INTERVAL).await,
                    Err(_) => return,
                }
            }
        })
        .await
        .is_ok();
        if !exited {
            self.kill();
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.try_wait();
    }
}

/// SIGTERM on unix, matching v2 `proc.terminate()`; plain kill elsewhere.
#[cfg(unix)]
fn terminate_child(child: &mut Child) {
    let pid = child.id();
    // SAFETY: signalling a child pid has no memory effects; a stale pid only
    // fails the call, which is ignored because shutdown always falls back to
    // kill.
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
}

/// Non-unix graceful stop degrades to an immediate kill.
#[cfg(not(unix))]
fn terminate_child(child: &mut Child) {
    let _ = child.kill();
}

// ---------------------------------------------------------------------------
// Transcoder seam (consumed by the gateway slice)
// ---------------------------------------------------------------------------

/// Byte stream for one transcode, behind a trait so the gateway programs to
/// the seam rather than the struct. Integrator seam: if the gateway slice
/// lands its own spelling first, keep one and delete the other.
pub trait TranscodeBody: Send {
    /// Pull the next output chunk; `None` is the end of the stream and fuses
    /// (later calls keep returning `None`). The disconnect probe, when given,
    /// runs after each read and before the chunk is handed out, matching v2's
    /// per-chunk `is_disconnected` check: a gone client aborts with
    /// [`TranscodeError::ClientDisconnected`] and ffmpeg is reaped.
    fn next_chunk(
        &mut self,
        is_disconnected: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, TranscodeError>> + Send;

    /// Stop early and free the slot. Idempotent; dropping the body without
    /// closing releases everything anyway.
    fn close(&mut self) -> impl Future<Output = ()> + Send;
}

/// Transcode service seam the gateway slice consumes. The gateway calls
/// [`decide()`] first and only reaches `stream` on a transcode plan; the
/// engine rejects direct plans defensively. Integrator seam: if the gateway
/// slice lands its own spelling first, keep one and delete the other.
pub trait Transcoder: Send + Sync {
    /// Per-request byte stream this service produces.
    type Body: TranscodeBody;

    /// Take a lease, spawn ffmpeg for the plan, and hand back the byte
    /// stream. The lease travels inside the body and releases exactly once
    /// when the stream ends, fails, closes, or drops. Port of v2
    /// `TranscodeService.stream`, minus the HTTP response wrapping, which is
    /// the gateway's job (see [`transcode_response_headers`]).
    fn stream(
        &self,
        source_path: &Path,
        plan: &StreamPlan,
        principal: &str,
    ) -> impl Future<Output = Result<Self::Body, TranscodeError>> + Send;
}

/// The [`Transcoder`] implementation: a spawner plus a lease pool, both
/// injected. Production wires [`StdFfmpegSpawner`] with the shared gateway
/// gate; the standalone [`LocalTranscodeGate`] covers engine-only use.
pub struct FfmpegTranscoder<S, P> {
    spawner: S,
    leases: P,
}

impl<S, P> FfmpegTranscoder<S, P> {
    /// Build the service from its two seams.
    pub fn new(spawner: S, leases: P) -> Self {
        Self { spawner, leases }
    }

    /// The injected spawner.
    pub fn spawner(&self) -> &S {
        &self.spawner
    }

    /// The injected lease pool.
    pub fn leases(&self) -> &P {
        &self.leases
    }
}

impl<S: FfmpegSpawner, P: TranscodeLeasePool> Transcoder for FfmpegTranscoder<S, P> {
    type Body = TranscodeStream<P::Lease, S::Child>;

    async fn stream(
        &self,
        source_path: &Path,
        plan: &StreamPlan,
        principal: &str,
    ) -> Result<TranscodeStream<P::Lease, S::Child>, TranscodeError> {
        if !plan.is_transcode() {
            return Err(TranscodeError::NotTranscoding);
        }
        // Lease first, exactly as v2 acquires before spawning; every exit
        // below frees it, so a failed start never wedges a slot.
        let lease = self.leases.acquire_transcode(principal).await?;
        let Some(argv) = build_cmd(source_path, plan) else {
            lease.release();
            return Err(TranscodeError::NotTranscoding);
        };
        match self.spawner.spawn(&argv) {
            Ok(child) => Ok(TranscodeStream::new(lease, child)),
            Err(err) => {
                lease.release();
                Err(err)
            }
        }
    }
}

/// One live transcode: the lease plus the child. The lease releases exactly
/// once no matter how the stream ends (EOF, error, `close`, or drop),
/// because every path funnels through `Option::take` — the structural
/// version of v2's idempotent `lease.release()` plus `BackgroundTask`.
pub struct TranscodeStream<L: TranscodeLease, C: FfmpegChild> {
    lease: Option<L>,
    child: Option<C>,
    closed: bool,
}

impl<L: TranscodeLease, C: FfmpegChild> std::fmt::Debug for TranscodeStream<L, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscodeStream")
            .field("lease_held", &self.lease.is_some())
            .field("child_live", &self.child.is_some())
            .field("closed", &self.closed)
            .finish()
    }
}

impl<L: TranscodeLease, C: FfmpegChild> TranscodeStream<L, C> {
    fn new(lease: L, child: C) -> Self {
        Self {
            lease: Some(lease),
            child: Some(child),
            closed: false,
        }
    }

    /// True once the stream ended, failed, or closed.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Kill the child and free the slot, at most once each.
    fn release_all(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill();
        }
        if let Some(lease) = self.lease.take() {
            lease.release();
        }
    }

    /// Fail the stream: graceful child shutdown first (v2 reaps via
    /// `_terminate` on every failure path), then the exactly-once release.
    async fn fail<T>(&mut self, err: TranscodeError) -> Result<T, TranscodeError> {
        if let Some(child) = self.child.as_mut() {
            child.shutdown().await;
        }
        self.release_all();
        self.closed = true;
        Err(err)
    }
}

impl<L: TranscodeLease, C: FfmpegChild> TranscodeBody for TranscodeStream<L, C> {
    async fn next_chunk(
        &mut self,
        is_disconnected: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<Option<Vec<u8>>, TranscodeError> {
        if self.closed {
            return Ok(None);
        }
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };
        let chunk = match tokio::time::timeout(READ_TIMEOUT, child.read_chunk()).await {
            Ok(Ok(chunk)) => chunk,
            Ok(Err(err)) => return self.fail(err).await,
            Err(_) => return self.fail(TranscodeError::ReadTimeout).await,
        };
        let Some(bytes) = chunk else {
            // Natural EOF: settle the process, then free the slot eagerly so
            // the gateway does not have to drop the body to unblock others.
            if let Some(child) = self.child.as_mut() {
                child.finish().await;
            }
            self.release_all();
            self.closed = true;
            return Ok(None);
        };
        if is_disconnected.is_some_and(|gone| gone()) {
            return self.fail(TranscodeError::ClientDisconnected).await;
        }
        Ok(Some(bytes))
    }

    async fn close(&mut self) {
        if let Some(child) = self.child.as_mut() {
            child.shutdown().await;
        }
        self.release_all();
        self.closed = true;
    }
}

impl<L: TranscodeLease, C: FfmpegChild> Drop for TranscodeStream<L, C> {
    fn drop(&mut self) {
        // Backstop for streams dropped mid-flight: prompt paths already took
        // the lease and child, so the takes below are usually empty, and the
        // release still lands exactly once either way.
        self.release_all();
    }
}
