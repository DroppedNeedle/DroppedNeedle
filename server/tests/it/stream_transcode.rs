//! Transcode module tests: decide policy, ffmpeg argv, leases, cancel.
//!
//! No routes live in this module, so every test drives the engine. ffmpeg itself is
//! always scripted through the injected spawner seam: no test needs a real
//! binary on `PATH`, and none touches the network.

use droppedneedle::stream::transcode;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use transcode::{
    CAPACITY_STATUS_CODE, FfmpegChild, FfmpegSpawner, FfmpegTranscoder, LocalTranscodeGate,
    OutFormat, RETRY_AFTER_SECONDS, STDERR_CAPTURE_BYTES, SUPPORTED_OUT_FORMATS, StdFfmpegSpawner,
    StreamPlan, TRANSCODE_GLOBAL_LIMIT, TRANSCODE_PRINCIPAL_LIMIT, TrackInfo, TranscodeBody,
    TranscodeError, TranscodeLease as _, TranscodeLeasePool as _, TranscodeSettings, Transcoder,
    build_cmd, decide, ffmpeg_available,
};

// ---------------------------------------------------------------------------
// Scripted ffmpeg
// ---------------------------------------------------------------------------

/// Everything a test can observe about what the engine did to its ffmpeg.
#[derive(Debug, Clone, Default)]
struct Probe {
    argv_seen: Arc<Mutex<Vec<Vec<String>>>>,
    killed: Arc<AtomicBool>,
    shutdowns: Arc<AtomicUsize>,
    finishes: Arc<AtomicUsize>,
}

impl Probe {
    fn argv(&self) -> Vec<Vec<String>> {
        self.argv_seen.lock().expect("probe mutex").clone()
    }

    fn killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }

    fn shutdowns(&self) -> usize {
        self.shutdowns.load(Ordering::SeqCst)
    }

    fn finishes(&self) -> usize {
        self.finishes.load(Ordering::SeqCst)
    }
}

/// Scripted child: serves canned chunks, then EOF.
struct FakeChild {
    chunks: VecDeque<Vec<u8>>,
    probe: Probe,
}

impl FfmpegChild for FakeChild {
    async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, TranscodeError> {
        Ok(self.chunks.pop_front())
    }

    async fn finish(&mut self) {
        self.probe.finishes.fetch_add(1, Ordering::SeqCst);
    }

    async fn shutdown(&mut self) {
        self.probe.shutdowns.fetch_add(1, Ordering::SeqCst);
    }

    fn kill(&mut self) {
        self.probe.killed.store(true, Ordering::SeqCst);
    }
}

/// Scripted spawner: records argv, replays one script per spawn, or fails.
struct FakeSpawner {
    script: Vec<Vec<u8>>,
    fail_spawn: bool,
    probe: Probe,
}

impl FakeSpawner {
    fn scripted(script: Vec<Vec<u8>>, probe: Probe) -> Self {
        Self {
            script,
            fail_spawn: false,
            probe,
        }
    }

    fn failing(probe: Probe) -> Self {
        Self {
            script: Vec::new(),
            fail_spawn: true,
            probe,
        }
    }
}

impl FfmpegSpawner for FakeSpawner {
    type Child = FakeChild;

    fn spawn(&self, argv: &[String]) -> Result<FakeChild, TranscodeError> {
        if self.fail_spawn {
            return Err(TranscodeError::Spawn("scripted ffmpeg failure".to_owned()));
        }
        self.probe
            .argv_seen
            .lock()
            .expect("probe mutex")
            .push(argv.to_vec());
        Ok(FakeChild {
            chunks: self.script.iter().cloned().collect(),
            probe: self.probe.clone(),
        })
    }
}

type FakeTranscoder = FfmpegTranscoder<FakeSpawner, Arc<LocalTranscodeGate>>;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn track(bitrate_kbps: Option<i64>, file_format: &str) -> TrackInfo {
    TrackInfo {
        file_format: file_format.to_owned(),
        bitrate_kbps,
        duration_seconds: 200.0,
    }
}

fn settings() -> TranscodeSettings {
    TranscodeSettings::default()
}

fn transcode_plan() -> StreamPlan {
    let plan = decide(
        &track(Some(900), "flac"),
        Some("mp3"),
        Some(128),
        false,
        0.0,
        &settings(),
        true,
    );
    assert!(plan.is_transcode(), "fixture must transcode: {plan:?}");
    plan
}

fn transcode_parts(plan: &StreamPlan) -> (OutFormat, i64, f64) {
    match plan {
        StreamPlan::Transcode {
            out_format,
            out_bitrate_kbps,
            start_seconds,
            ..
        } => (*out_format, *out_bitrate_kbps, *start_seconds),
        StreamPlan::Direct { .. } => panic!("expected a transcode plan, got {plan:?}"),
    }
}

// ---------------------------------------------------------------------------
// decide() policy tests
// ---------------------------------------------------------------------------

#[test]
fn client_cap_below_source_transcodes_and_honors_client_cap() {
    let plan = decide(
        &track(Some(900), "flac"),
        None,
        Some(128),
        false,
        0.0,
        &settings(),
        true,
    );
    let (format, bitrate, _) = transcode_parts(&plan);
    assert_eq!(format, OutFormat::Mp3);
    assert_eq!(bitrate, 128);
}

#[test]
fn lossless_over_server_cap_still_direct_plays() {
    // Jellify regression: the server ceiling must never trigger a transcode,
    // or direct-play clients break on every lossless file.
    let plan = decide(
        &track(Some(900), "flac"),
        None,
        Some(0),
        false,
        0.0,
        &settings(),
        true,
    );
    assert!(
        !plan.is_transcode(),
        "server cap must not trigger: {plan:?}"
    );
}

#[test]
fn codec_request_transcodes_and_clamps_to_server_cap() {
    // Manet: /universal?AudioCodec=mp3 on a FLAC transcodes to mp3 clamped to
    // the server ceiling even though the client gave no bitrate.
    let plan = decide(
        &track(Some(900), "flac"),
        Some("mp3"),
        Some(0),
        false,
        0.0,
        &settings(),
        true,
    );
    let (format, bitrate, _) = transcode_parts(&plan);
    assert_eq!(format, OutFormat::Mp3);
    assert_eq!(bitrate, 320);
}

#[test]
fn raw_is_never_a_codec_mismatch() {
    // Subsonic format=raw short-circuits to the file upstream of decide();
    // inside decide() "raw" still never counts as a codec change.
    let plan = decide(
        &track(Some(900), "flac"),
        Some("raw"),
        None,
        false,
        0.0,
        &settings(),
        true,
    );
    assert!(!plan.is_transcode(), "{plan:?}");

    // Exact-port pin: raw only exempts the mismatch half. A client ceiling
    // breach underneath still transcodes here; the router's raw short-circuit
    // is what keeps real raw requests direct.
    let plan = decide(
        &track(Some(900), "flac"),
        Some("raw"),
        Some(128),
        false,
        0.0,
        &settings(),
        true,
    );
    assert!(plan.is_transcode(), "{plan:?}");
}

#[test]
fn force_original_lands_direct() {
    // Jellyfin static=true (and Subsonic download-style) landings arrive as
    // force_original and always serve the file, whatever else was asked.
    let plan = decide(
        &track(Some(900), "flac"),
        Some("mp3"),
        Some(128),
        true,
        30.0,
        &settings(),
        true,
    );
    assert!(!plan.is_transcode(), "{plan:?}");
    assert_eq!(plan.source_duration_seconds(), 200.0);
}

#[test]
fn disabled_or_ffmpeg_absent_falls_back_to_direct_silently() {
    // Both landings return a direct plan with no error: a missing ffmpeg is
    // a degradation, not a failure, and direct play keeps working.
    for (enabled, ffmpeg) in [(false, true), (true, false)] {
        let mut settings = settings();
        settings.transcoding_enabled = enabled;
        let plan = decide(
            &track(Some(900), "flac"),
            Some("mp3"),
            Some(128),
            false,
            0.0,
            &settings,
            ffmpeg,
        );
        assert!(
            !plan.is_transcode(),
            "enabled={enabled} ffmpeg={ffmpeg}: {plan:?}"
        );
    }
    let plan = decide(
        &track(Some(900), "flac"),
        Some("mp3"),
        Some(128),
        false,
        30.0,
        &settings(),
        false,
    );
    assert!(!plan.is_transcode(), "{plan:?}");
}

#[test]
fn zero_unset_and_finamp_bitrates_mean_no_cap() {
    // Feishin sends 0 and Finamp sends 999999999 to mean "no cap"; both must
    // direct-play a matching file.
    for max in [None, Some(0), Some(999_999_999)] {
        let plan = decide(
            &track(Some(192), "mp3"),
            None,
            max,
            false,
            0.0,
            &settings(),
            true,
        );
        assert!(!plan.is_transcode(), "max={max:?}: {plan:?}");
    }
}

#[test]
fn start_offset_survives_only_on_transcode() {
    let plan = decide(
        &track(Some(900), "flac"),
        Some("mp3"),
        Some(128),
        false,
        30.0,
        &settings(),
        true,
    );
    let (_, _, start) = transcode_parts(&plan);
    assert_eq!(start, 30.0);

    let plan = decide(
        &track(Some(192), "mp3"),
        None,
        Some(0),
        false,
        30.0,
        &settings(),
        true,
    );
    assert!(!plan.is_transcode(), "{plan:?}");

    let plan = decide(
        &track(Some(900), "flac"),
        Some("mp3"),
        Some(128),
        false,
        -5.0,
        &settings(),
        true,
    );
    let (_, _, start) = transcode_parts(&plan);
    assert_eq!(start, 0.0, "negative seeks clamp to zero");
}

// ---------------------------------------------------------------------------
// ffmpeg argv contract tests
// ---------------------------------------------------------------------------

#[test]
fn mp3_argv_is_exact() {
    let plan = StreamPlan::Transcode {
        out_format: OutFormat::Mp3,
        out_bitrate_kbps: 128,
        start_seconds: 0.0,
        source_duration_seconds: 200.0,
    };
    let argv = build_cmd(Path::new("/m/x.flac"), &plan).expect("transcode builds argv");
    let expected: Vec<String> = [
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-i",
        "/m/x.flac",
        "-vn",
        "-map",
        "0:a:0",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "128k",
        "-f",
        "mp3",
        "pipe:1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(argv, expected);
    assert!(!argv.iter().any(|arg| arg == "-ss"), "no seek, no -ss");
}

#[test]
fn opus_argv_seeks_before_input() {
    let plan = StreamPlan::Transcode {
        out_format: OutFormat::Opus,
        out_bitrate_kbps: 160,
        start_seconds: 12.5,
        source_duration_seconds: 200.0,
    };
    let argv = build_cmd(Path::new("/m/x.flac"), &plan).expect("transcode builds argv");
    let seek = argv.iter().position(|arg| arg == "-ss").expect("seek flag");
    let input = argv.iter().position(|arg| arg == "-i").expect("input flag");
    assert!(seek < input, "-ss must come before -i: {argv:?}");
    assert_eq!(argv[seek + 1], "12.500");
    assert!(argv.contains(&"libopus".to_owned()));
    assert_eq!(&argv[argv.len() - 3..], &["-f", "ogg", "pipe:1"]);
}

// ---------------------------------------------------------------------------
// Execution tests (scripted ffmpeg, real local gate)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scripted_stream_serves_chunks_then_frees_lease_at_eof() {
    let gate = Arc::new(LocalTranscodeGate::new());
    let probe = Probe::default();
    let service = FakeTranscoder::new(
        FakeSpawner::scripted(vec![b"first".to_vec(), b"second".to_vec()], probe.clone()),
        Arc::clone(&gate),
    );
    let mut body = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
        )
        .await
        .expect("stream starts");
    assert_eq!(gate.active(), 1);

    let no_probe: Option<&(dyn Fn() -> bool + Sync)> = None;
    assert_eq!(
        body.next_chunk(no_probe).await.expect("chunk").as_deref(),
        Some(b"first".as_slice())
    );
    assert_eq!(
        body.next_chunk(no_probe).await.expect("chunk").as_deref(),
        Some(b"second".as_slice())
    );
    assert_eq!(body.next_chunk(no_probe).await.expect("eof"), None);
    assert_eq!(probe.finishes(), 1, "natural exit settles once");
    assert_eq!(gate.active(), 0, "slot frees at EOF, before drop");
    assert!(body.is_closed());
    assert_eq!(body.next_chunk(no_probe).await.expect("fused"), None);
    assert_eq!(probe.finishes(), 1, "EOF fuses; no second settle");

    let argv = probe.argv();
    assert_eq!(argv.len(), 1);
    assert_eq!(argv[0][0], "ffmpeg");
    assert!(argv[0].contains(&"libmp3lame".to_owned()), "{argv:?}");
    assert_eq!(&argv[0][argv[0].len() - 3..], &["-f", "mp3", "pipe:1"]);
}

#[tokio::test]
async fn lease_exhaustion_returns_capacity() {
    assert_eq!(TRANSCODE_GLOBAL_LIMIT, 2);
    assert_eq!(TRANSCODE_PRINCIPAL_LIMIT, 1);
    assert_eq!(CAPACITY_STATUS_CODE, 429, "gateway maps capacity to 429");
    assert_eq!(RETRY_AFTER_SECONDS, 1);

    // Global ceiling: two holders from different principals, third waits out
    // the short deadline and exhausts.
    let gate = Arc::new(LocalTranscodeGate::with_limits(
        2,
        1,
        128,
        Duration::from_millis(50),
    ));
    let first = gate.acquire_transcode("alice").await.expect("slot 1");
    let second = gate.acquire_transcode("bob").await.expect("slot 2");
    assert_eq!(gate.active(), 2);
    let err = gate
        .acquire_transcode("carol")
        .await
        .expect_err("pool is full");
    assert!(err.is_capacity());
    assert_eq!(err, TranscodeError::Capacity);
    assert_eq!(gate.active(), 2, "failed acquire takes nothing");
    assert_eq!(gate.waiter_count(), 0, "timed-out waiter leaves no trace");

    // Per-principal ceiling: one holder, same principal again exhausts.
    let gate = Arc::new(LocalTranscodeGate::with_limits(
        2,
        1,
        128,
        Duration::from_millis(50),
    ));
    let held = gate.acquire_transcode("alice").await.expect("slot");
    let err = gate
        .acquire_transcode("alice")
        .await
        .expect_err("principal capped at one");
    assert_eq!(err, TranscodeError::Capacity);

    // A freed slot is reusable.
    held.release();
    assert_eq!(gate.active(), 0);
    gate.acquire_transcode("alice")
        .await
        .expect("slot back")
        .release();

    first.release();
    second.release();
}

#[tokio::test]
async fn spawn_failure_releases_slot() {
    let gate = Arc::new(LocalTranscodeGate::new());
    let probe = Probe::default();
    let service = FakeTranscoder::new(FakeSpawner::failing(probe), Arc::clone(&gate));
    let err = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
        )
        .await
        .expect_err("spawn fails");
    assert!(matches!(err, TranscodeError::Spawn(_)), "{err:?}");
    assert_eq!(gate.active(), 0, "failed start wedges no slot");
    assert!(
        !err.to_string().contains("input.flac"),
        "errors never quote paths: {err}"
    );
}

#[tokio::test]
async fn disconnect_kills_ffmpeg_and_releases_lease() {
    let gate = Arc::new(LocalTranscodeGate::new());
    let probe = Probe::default();
    let service = FakeTranscoder::new(
        FakeSpawner::scripted(vec![b"first".to_vec(), b"second".to_vec()], probe.clone()),
        Arc::clone(&gate),
    );
    let mut body = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
        )
        .await
        .expect("stream starts");
    assert_eq!(gate.active(), 1);

    let gone = || true;
    let err = body
        .next_chunk(Some(&gone))
        .await
        .expect_err("disconnect aborts the stream");
    assert_eq!(err, TranscodeError::ClientDisconnected);
    assert!(probe.killed(), "disconnect reaps ffmpeg");
    assert_eq!(probe.shutdowns(), 1, "failure path shuts down first");
    assert_eq!(gate.active(), 0, "slot frees on disconnect, before drop");

    drop(body);
    assert_eq!(gate.active(), 0, "release lands exactly once");
}

#[tokio::test]
async fn dropped_stream_kills_child_and_releases_once() {
    let gate = Arc::new(LocalTranscodeGate::new());
    let probe = Probe::default();
    let service = FakeTranscoder::new(
        FakeSpawner::scripted(vec![b"first".to_vec(), b"second".to_vec()], probe.clone()),
        Arc::clone(&gate),
    );
    let mut body = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
        )
        .await
        .expect("stream starts");
    let no_probe: Option<&(dyn Fn() -> bool + Sync)> = None;
    assert!(body.next_chunk(no_probe).await.expect("chunk").is_some());
    assert_eq!(gate.active(), 1);

    drop(body);
    assert!(probe.killed(), "drop reaps a mid-flight ffmpeg");
    assert_eq!(
        probe.shutdowns(),
        0,
        "drop kills; it cannot wait on graceful"
    );
    assert_eq!(gate.active(), 0, "drop frees the slot exactly once");
}

#[test]
fn production_spawner_contract_without_ffmpeg() {
    assert_eq!(SUPPORTED_OUT_FORMATS, &["mp3", "opus"]);
    assert_eq!(STDERR_CAPTURE_BYTES, 2000, "bounded stderr capture");

    // detect() and the decide() flag agree: both answer "is ffmpeg on PATH".
    assert_eq!(StdFfmpegSpawner::detect().is_some(), ffmpeg_available());

    // A pinned path round-trips, and a missing binary fails to spawn with a
    // path-free error and no lease involvement at all.
    let spawner = StdFfmpegSpawner::with_path(PathBuf::from("/nonexistent/ffmpeg"));
    assert_eq!(spawner.ffmpeg_path(), Path::new("/nonexistent/ffmpeg"));
    let argv = build_cmd(Path::new("/m/x.flac"), &transcode_plan()).expect("argv");
    let err = spawner
        .spawn(&argv)
        .expect_err("missing binary cannot spawn");
    assert!(matches!(err, TranscodeError::Spawn(_)), "{err:?}");
    assert!(!err.to_string().contains("x.flac"), "{err}");
}

/// The real child against a trivial process: it exits at once with empty
/// stdout, so the body sees immediate EOF and settles. This is not ffmpeg
/// (still never required), and the test skips itself where no `true`
/// binary exists.
#[cfg(unix)]
#[tokio::test]
async fn real_child_settles_a_trivial_process() {
    let binary = ["/bin/true", "/usr/bin/true"]
        .into_iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file());
    let Some(binary) = binary else {
        return;
    };
    let gate = Arc::new(LocalTranscodeGate::new());
    let service = FfmpegTranscoder::new(StdFfmpegSpawner::with_path(binary), Arc::clone(&gate));
    assert_eq!(service.leases().active(), 0);
    assert!(service.spawner().ffmpeg_path().is_file());
    let mut body = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
        )
        .await
        .expect("trivial process starts");
    let no_probe: Option<&(dyn Fn() -> bool + Sync)> = None;
    assert_eq!(body.next_chunk(no_probe).await.expect("eof"), None);
    assert!(body.is_closed());
    assert_eq!(gate.active(), 0);
}

/// A killed child is reaped, not left behind as a zombie.
#[cfg(unix)]
#[tokio::test]
async fn killed_child_is_reaped() {
    let Some(binary) = ["/bin/sleep", "/usr/bin/sleep"]
        .into_iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
    else {
        return;
    };
    let child = StdFfmpegSpawner::with_path(binary)
        .spawn(&["ffmpeg".to_owned(), "30".to_owned()])
        .expect("sleep starts");
    let proc_dir = PathBuf::from(format!("/proc/{}", child.id().expect("pid")));
    drop(child);
    for _ in 0..100 {
        if !proc_dir.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!proc_dir.exists(), "the killed child was reaped");
}
