//! Transcode module tests: decide policy, ffmpeg argv, leases, cancel.
//!
//! No routes live in this module, so every test drives the engine. ffmpeg itself is
//! always scripted through the injected spawner seam: no test needs a real
//! binary on `PATH`, and none touches the network.

use droppedneedle_media::stream::transcode;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use droppedneedle_media::stream::gateway::{Gateway, RemoteReader};
use droppedneedle_media::stream::leases::{DirectGate, TranscodeGate};
use droppedneedle_media::stream::routes::{
    AudioSource, MediaBody, StreamEngine as _, StreamFault, StreamMedia, StreamOpen, StreamParams,
};
use futures_util::StreamExt as _;
use transcode::{
    FfmpegChild, FfmpegSpawner, FfmpegTranscoder, IDLE_DEADLINE, OutFormat, ProcessSpawner,
    StreamPlan, TrackInfo, TranscodeError, TranscodeSettings, Transcoder as _, build_cmd, decide,
};

use crate::common::ScratchDir;

// ---------------------------------------------------------------------------
// Scripted ffmpeg
// ---------------------------------------------------------------------------

/// Everything a test can observe about what the engine did to its ffmpeg.
#[derive(Debug, Clone, Default)]
struct Probe {
    argv_seen: Arc<Mutex<Vec<Vec<String>>>>,
    stops: Arc<AtomicUsize>,
    finishes: Arc<AtomicUsize>,
}

impl Probe {
    fn argv(&self) -> Vec<Vec<String>> {
        self.argv_seen.lock().expect("probe mutex").clone()
    }

    fn stops(&self) -> usize {
        self.stops.load(Ordering::SeqCst)
    }

    fn finishes(&self) -> usize {
        self.finishes.load(Ordering::SeqCst)
    }
}

/// Scripted child: serves canned chunks, then EOF, or chunks forever.
struct FakeChild {
    chunks: VecDeque<Vec<u8>>,
    endless: bool,
    probe: Probe,
}

impl FfmpegChild for FakeChild {
    async fn read_chunk(&mut self) -> Result<Option<Vec<u8>>, TranscodeError> {
        if self.endless {
            return Ok(Some(b"chunk".to_vec()));
        }
        Ok(self.chunks.pop_front())
    }

    async fn finish(&mut self) {
        self.probe.finishes.fetch_add(1, Ordering::SeqCst);
    }

    async fn stop(&mut self) {
        self.probe.stops.fetch_add(1, Ordering::SeqCst);
    }
}

/// Scripted spawner: records argv and replays one script per spawn.
struct FakeSpawner {
    script: Vec<Vec<u8>>,
    endless: bool,
    probe: Probe,
}

impl FakeSpawner {
    fn scripted(script: Vec<Vec<u8>>, probe: Probe) -> Self {
        Self {
            script,
            endless: false,
            probe,
        }
    }

    /// A transcode that never ends by itself.
    fn endless(probe: Probe) -> Self {
        Self {
            script: Vec::new(),
            endless: true,
            probe,
        }
    }
}

impl FfmpegSpawner for FakeSpawner {
    type Child = FakeChild;

    fn spawn(&self, argv: &[String]) -> Result<FakeChild, TranscodeError> {
        self.probe
            .argv_seen
            .lock()
            .expect("probe mutex")
            .push(argv.to_vec());
        Ok(FakeChild {
            chunks: self.script.iter().cloned().collect(),
            endless: self.endless,
            probe: self.probe.clone(),
        })
    }
}

/// Remote reader for a local-only gateway.
struct NoRemote;

impl RemoteReader for NoRemote {
    async fn open_stream(
        &self,
        _source: AudioSource,
        _key: &str,
        _user_id: &str,
        _head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        Err(StreamFault::NotFound)
    }
}

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
fn codec_request_transcodes_at_codec_default_without_a_cap() {
    // Manet: /universal?AudioCodec=mp3 on a FLAC transcodes to mp3. The
    // client gave no bitrate, so the output takes the mp3 default (192),
    // not the 320 server ceiling.
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
    assert_eq!(bitrate, 192);
    let opus = decide(
        &track(Some(900), "flac"),
        Some("opus"),
        None,
        false,
        0.0,
        &settings(),
        true,
    );
    assert_eq!(transcode_parts(&opus).1, 128);
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
// Execution tests (scripted ffmpeg, real gates)
// ---------------------------------------------------------------------------

fn transcode_gate() -> Arc<TranscodeGate> {
    Arc::new(TranscodeGate::new())
}

/// Wait until the pump has let go of every slot. The pump runs as its own
/// task, so a dropped body frees its slots a moment later.
async fn settled(gate: &TranscodeGate) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while gate.active() > 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the transcode lets go of its slot");
}

#[tokio::test]
async fn scripted_stream_serves_chunks_then_frees_lease_at_eof() {
    let gate = transcode_gate();
    let probe = Probe::default();
    let service = FfmpegTranscoder::new(
        FakeSpawner::scripted(vec![b"first".to_vec(), b"second".to_vec()], probe.clone()),
        Arc::clone(&gate),
    );
    let mut body = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
            None,
        )
        .await
        .expect("stream starts");
    assert_eq!(gate.active(), 1);

    assert_eq!(body.next().await.unwrap().unwrap(), b"first");
    assert_eq!(body.next().await.unwrap().unwrap(), b"second");
    assert!(body.next().await.is_none(), "EOF ends the body cleanly");
    assert_eq!(probe.finishes(), 1, "natural exit settles once");
    assert_eq!(probe.stops(), 0);
    assert_eq!(gate.active(), 0, "slot frees at EOF, before drop");
    assert!(body.next().await.is_none(), "the body stays ended");

    let argv = probe.argv();
    assert_eq!(argv.len(), 1);
    assert_eq!(argv[0][0], "ffmpeg");
    assert!(argv[0].contains(&"libmp3lame".to_owned()), "{argv:?}");
    assert_eq!(&argv[0][argv[0].len() - 3..], &["-f", "mp3", "pipe:1"]);
}

#[tokio::test]
async fn dropped_stream_kills_child_and_releases_once() {
    let gate = transcode_gate();
    let direct = Arc::new(DirectGate::new());
    let probe = Probe::default();
    let service = FfmpegTranscoder::new(FakeSpawner::endless(probe.clone()), Arc::clone(&gate));
    let mut body = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
            Some(direct.acquire_owned("alice").await.expect("direct slot")),
        )
        .await
        .expect("stream starts");
    assert!(body.next().await.expect("chunk").is_ok());
    assert_eq!(gate.active(), 1);
    assert_eq!(direct.active(), 1);

    drop(body);
    settled(&gate).await;
    assert_eq!(probe.stops(), 1, "a dropped body stops ffmpeg once");
    assert_eq!(probe.finishes(), 0);
    assert_eq!(direct.active(), 0, "the direct slot goes with it");
}

/// A client that goes away mid-transcode drops the response body; through
/// the gateway that stops ffmpeg and frees the slot the open took.
#[tokio::test]
async fn disconnect_kills_ffmpeg_and_releases_lease() {
    let root = ScratchDir::new("transcode-disconnect");
    std::fs::write(root.join("song.flac"), b"FLAC").unwrap();
    let gate = transcode_gate();
    let probe = Probe::default();
    let gateway = Gateway::new(
        root.to_path_buf(),
        NoRemote,
        FfmpegTranscoder::new(FakeSpawner::endless(probe.clone()), Arc::clone(&gate)),
        settings(),
        true,
    );
    let media = gateway
        .open_stream(
            StreamOpen {
                source: AudioSource::Local,
                key: "song.flac".to_owned(),
                user_id: "alice".to_owned(),
                params: StreamParams {
                    format: Some("mp3".to_owned()),
                    ..StreamParams::default()
                },
            },
            false,
        )
        .await
        .expect("transcode opens");
    assert!(media.lease.is_none(), "the transcode holds the direct slot");
    let MediaBody::Chunks(mut chunks) = media.body else {
        panic!("a transcode streams chunks");
    };
    assert!(chunks.next().await.expect("chunk").is_ok());
    assert_eq!(gate.active(), 1);

    drop(chunks);
    settled(&gate).await;
    assert_eq!(probe.stops(), 1, "disconnect stops ffmpeg");
}

/// A body nobody reads (a paused-forever client, a forgotten tab) is
/// stopped after the idle deadline, and both its slots come back.
#[tokio::test(start_paused = true)]
async fn unread_body_is_stopped_after_the_idle_deadline() {
    let gate = transcode_gate();
    let direct = Arc::new(DirectGate::new());
    let probe = Probe::default();
    let service = FfmpegTranscoder::new(FakeSpawner::endless(probe.clone()), Arc::clone(&gate));
    let mut body = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
            Some(direct.acquire_owned("alice").await.expect("direct slot")),
        )
        .await
        .expect("stream starts");
    assert!(body.next().await.expect("chunk").is_ok());

    tokio::time::sleep(IDLE_DEADLINE - Duration::from_secs(1)).await;
    assert_eq!(
        probe.stops(),
        0,
        "a pause shorter than the deadline resumes"
    );
    assert_eq!(gate.active(), 1);

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(probe.stops(), 1, "ffmpeg is stopped");
    assert_eq!(gate.active(), 0, "the ffmpeg slot is free");
    assert_eq!(direct.active(), 0, "the direct slot is free");

    assert!(body.next().await.expect("buffered chunk").is_ok());
    assert!(
        body.next().await.expect("an end").is_err(),
        "a stopped transcode never ends like a whole one"
    );
}

/// A missing binary fails with a path-free error and wedges no slot.
#[tokio::test]
async fn spawn_failure_releases_slot() {
    let gate = transcode_gate();
    let service = FfmpegTranscoder::new(
        ProcessSpawner::with_path(PathBuf::from("/nonexistent/ffmpeg")),
        Arc::clone(&gate),
    );
    let Err(err) = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
            None,
        )
        .await
    else {
        panic!("a missing binary cannot spawn");
    };
    assert!(matches!(err, TranscodeError::Spawn(_)), "{err:?}");
    assert_eq!(gate.active(), 0, "failed start wedges no slot");
    assert!(
        !err.to_string().contains("input.flac"),
        "errors never quote paths: {err}"
    );
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
    let gate = transcode_gate();
    let service = FfmpegTranscoder::new(ProcessSpawner::with_path(binary), Arc::clone(&gate));
    let mut body = service
        .stream(
            Path::new("/validated/input.flac"),
            &transcode_plan(),
            "alice",
            None,
        )
        .await
        .expect("trivial process starts");
    assert!(body.next().await.is_none(), "EOF ends the body cleanly");
    assert_eq!(gate.active(), 0);
}

/// A stopped child is reaped, not left behind as a zombie, and so is one
/// that is only dropped.
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
    let spawner = ProcessSpawner::with_path(binary);
    let argv = ["ffmpeg".to_owned(), "30".to_owned()];

    let mut stopped = spawner.spawn(&argv).expect("sleep starts");
    let proc_dir = PathBuf::from(format!("/proc/{}", stopped.id().expect("pid")));
    stopped.stop().await;
    assert!(!proc_dir.exists(), "stop reaps the child");

    let dropped = spawner.spawn(&argv).expect("sleep starts");
    let proc_dir = PathBuf::from(format!("/proc/{}", dropped.id().expect("pid")));
    drop(dropped);
    tokio::time::timeout(Duration::from_secs(5), async {
        while proc_dir.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the runtime reaps a dropped child");
}
