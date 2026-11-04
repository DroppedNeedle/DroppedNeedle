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

use droppedneedle_media::stream::engine::{
    AudioSource, MediaBody, StreamEngine as _, StreamFault, StreamMedia, StreamOpen, StreamParams,
};
use droppedneedle_media::stream::gateway::{Gateway, RemoteReader};
use droppedneedle_media::stream::leases::{DirectGate, TranscodeGate};
use futures_util::StreamExt as _;
use transcode::{
    FfmpegChild, FfmpegSpawner, FfmpegTranscoder, IDLE_DEADLINE, OutFormat, ProcessSpawner,
    StreamPlan, TrackInfo, TranscodeError, TranscodeSettings, Transcoder as _, build_cmd, decide,
    estimate_size, out_media_type,
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

/// One `decide()` call: the file (kbps, format), what the client asked and
/// the server's state. `Ask::of` is transcoding on with ffmpeg present.
#[derive(Clone, Copy)]
struct Ask {
    file: (i64, &'static str),
    format: Option<&'static str>,
    cap: Option<i64>,
    force: bool,
    start: f64,
    enabled: bool,
    ffmpeg: bool,
}

impl Ask {
    fn of(kbps: i64, file: &'static str, format: Option<&'static str>, cap: Option<i64>) -> Self {
        Self {
            file: (kbps, file),
            format,
            cap,
            force: false,
            start: 0.0,
            enabled: true,
            ffmpeg: true,
        }
    }

    fn plan(self) -> StreamPlan {
        let settings = TranscodeSettings {
            transcoding_enabled: self.enabled,
            ..settings()
        };
        decide(
            &track(Some(self.file.0), self.file.1),
            self.format,
            self.cap,
            self.force,
            self.start,
            &settings,
            self.ffmpeg,
        )
    }
}

/// The plan a row expects. A transcode names the format, bitrate and start
/// it pins; `None` leaves that part unchecked.
enum Want {
    Direct,
    Transcode(Option<OutFormat>, Option<i64>, Option<f64>),
}

/// The one stream decision every protocol shares: direct unless a codec
/// change or a client ceiling asks otherwise, and never when forced original,
/// disabled or without ffmpeg (a degradation, not an error).
#[test]
fn decide_picks_direct_or_transcode() {
    use Want::{Direct, Transcode};
    let mp3 = Some(OutFormat::Mp3);
    let opus = Some(OutFormat::Opus);
    let rows: Vec<(&str, Ask, Want)> = vec![
        (
            "a client cap below the source",
            Ask::of(900, "flac", None, Some(128)),
            Transcode(mp3, Some(128), None),
        ),
        // Jellify: the server ceiling must never trigger a transcode, or
        // direct-play clients break on every lossless file.
        (
            "lossless over the server cap",
            Ask::of(900, "flac", None, Some(0)),
            Direct,
        ),
        // Manet: /universal?AudioCodec=mp3 on a FLAC transcodes to mp3 at the
        // mp3 default (192), not the 320 server ceiling.
        (
            "a codec ask without a cap",
            Ask::of(900, "flac", Some("mp3"), Some(0)),
            Transcode(mp3, Some(192), None),
        ),
        (
            "opus at its codec default",
            Ask::of(900, "flac", Some("opus"), None),
            Transcode(None, Some(128), None),
        ),
        // Subsonic format=raw short-circuits to the file upstream of
        // decide(); inside decide() "raw" still never counts as a codec change.
        (
            "raw is no codec change",
            Ask::of(900, "flac", Some("raw"), None),
            Direct,
        ),
        // Exact-port pin: raw only exempts the mismatch half. A client
        // ceiling breach underneath still transcodes here; the router's raw
        // short-circuit is what keeps real raw requests direct.
        (
            "raw over a client ceiling",
            Ask::of(900, "flac", Some("raw"), Some(128)),
            Transcode(None, None, None),
        ),
        // Jellyfin static=true (and Subsonic download-style) landings arrive
        // as force_original and always serve the file, whatever else was asked.
        (
            "force original",
            Ask {
                force: true,
                start: 30.0,
                ..Ask::of(900, "flac", Some("mp3"), Some(128))
            },
            Direct,
        ),
        (
            "transcoding disabled",
            Ask {
                enabled: false,
                ..Ask::of(900, "flac", Some("mp3"), Some(128))
            },
            Direct,
        ),
        (
            "ffmpeg absent",
            Ask {
                ffmpeg: false,
                ..Ask::of(900, "flac", Some("mp3"), Some(128))
            },
            Direct,
        ),
        (
            "ffmpeg absent, seeking",
            Ask {
                ffmpeg: false,
                start: 30.0,
                ..Ask::of(900, "flac", Some("mp3"), Some(128))
            },
            Direct,
        ),
        // Feishin sends 0 and Finamp sends 999999999 to mean "no cap"; both
        // must direct-play a matching file.
        ("no cap: unset", Ask::of(192, "mp3", None, None), Direct),
        (
            "no cap: Feishin's 0",
            Ask::of(192, "mp3", None, Some(0)),
            Direct,
        ),
        (
            "no cap: Finamp's 999999999",
            Ask::of(192, "mp3", None, Some(999_999_999)),
            Direct,
        ),
        (
            "a seek survives on a transcode",
            Ask {
                start: 30.0,
                ..Ask::of(900, "flac", Some("mp3"), Some(128))
            },
            Transcode(None, None, Some(30.0)),
        ),
        (
            "a seek on a direct play",
            Ask {
                start: 30.0,
                ..Ask::of(192, "mp3", None, Some(0))
            },
            Direct,
        ),
        (
            "a negative seek clamps to zero",
            Ask {
                start: -5.0,
                ..Ask::of(900, "flac", Some("mp3"), Some(128))
            },
            Transcode(None, None, Some(0.0)),
        ),
        // The server maximum caps a client's ask but never triggers.
        (
            "the server maximum caps a 999 kbps ask",
            Ask::of(320, "mp3", Some("opus"), Some(999)),
            Transcode(opus, Some(320), None),
        ),
        // An unusable format falls back to the default, and the seek clamps.
        (
            "wav falls back to mp3",
            Ask {
                start: -5.0,
                ..Ask::of(320, "mp3", Some("wav"), None)
            },
            Transcode(mp3, None, Some(0.0)),
        ),
        // The floor is 64 kbps on every protocol.
        (
            "a 48 kbps ceiling meets the floor",
            Ask::of(900, "flac", None, Some(48)),
            Transcode(None, Some(64), None),
        ),
        (
            "the file's own codec streams direct",
            Ask::of(320, "mp3", Some("mp3"), None),
            Direct,
        ),
    ];
    for (case, ask, want) in rows {
        let plan = ask.plan();
        assert_eq!(plan.source_duration_seconds(), 200.0, "{case}: {plan:?}");
        match want {
            Direct => assert!(!plan.is_transcode(), "{case}: {plan:?}"),
            Transcode(format, bitrate, start) => {
                assert!(plan.is_transcode(), "{case}: {plan:?}");
                let (out_format, out_bitrate, out_start) = transcode_parts(&plan);
                if let Some(format) = format {
                    assert_eq!(out_format, format, "{case}");
                }
                if let Some(bitrate) = bitrate {
                    assert_eq!(out_bitrate, bitrate, "{case}");
                }
                if let Some(start) = start {
                    assert_eq!(out_start, start, "{case}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ffmpeg argv contract tests
// ---------------------------------------------------------------------------

/// The ffmpeg argv per output format: the exact mp3 argv without a seek,
/// the FLAC the web player asks for when a browser refuses a lossless
/// source, and opus seeking before its input.
#[test]
fn ffmpeg_argv_per_format() {
    // mp3, no seek: the whole argv.
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
    assert_eq!(argv, expected, "mp3");
    assert!(!argv.iter().any(|arg| arg == "-ss"), "mp3: no seek, no -ss");

    // flac seeks like the others, carries no bitrate, and has no size to
    // estimate.
    let plan = decide(
        &track(Some(900), "m4a"),
        Some("flac"),
        None,
        false,
        12.5,
        &settings(),
        true,
    );
    assert_eq!(plan.out_format(), Some(OutFormat::Flac), "flac");
    assert_eq!(estimate_size(&plan), None, "flac");
    let argv = build_cmd(Path::new("/m/x.m4a"), &plan).expect("transcode builds argv");
    let seek = argv
        .iter()
        .position(|arg| arg == "-ss")
        .expect("flac seek flag");
    assert_eq!(argv[seek + 1], "12.500", "flac");
    assert!(!argv.iter().any(|arg| arg == "-b:a"), "flac: {argv:?}");
    assert_eq!(
        &argv[argv.len() - 5..],
        &["-c:a", "flac", "-f", "flac", "pipe:1"],
        "flac"
    );
    assert_eq!(out_media_type(&OutFormat::Flac), "audio/flac");

    // opus seeks before its input.
    let plan = StreamPlan::Transcode {
        out_format: OutFormat::Opus,
        out_bitrate_kbps: 160,
        start_seconds: 12.5,
        source_duration_seconds: 200.0,
    };
    let argv = build_cmd(Path::new("/m/x.flac"), &plan).expect("transcode builds argv");
    let seek = argv
        .iter()
        .position(|arg| arg == "-ss")
        .expect("opus seek flag");
    let input = argv
        .iter()
        .position(|arg| arg == "-i")
        .expect("opus input flag");
    assert!(seek < input, "opus: -ss must come before -i: {argv:?}");
    assert_eq!(argv[seek + 1], "12.500", "opus");
    assert!(argv.contains(&"libopus".to_owned()), "opus: {argv:?}");
    assert_eq!(&argv[argv.len() - 3..], &["-f", "ogg", "pipe:1"], "opus");
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

/// A client that goes away mid-transcode drops the body: ffmpeg is stopped
/// once and both the ffmpeg slot and the direct slot come back, on the
/// service itself and through the gateway.
#[tokio::test]
async fn a_dropped_transcode_stops_ffmpeg_and_frees_both_slots() {
    // The service, holding a direct slot.
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
    assert_eq!(
        probe.stops(),
        1,
        "service: a dropped body stops ffmpeg once"
    );
    assert_eq!(probe.finishes(), 0, "service");
    assert_eq!(direct.active(), 0, "service: the direct slot goes with it");

    // Through the gateway, which takes the direct slot on open.
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
    assert!(
        media.lease.is_none(),
        "gateway: the transcode holds the direct slot"
    );
    let MediaBody::Chunks(mut chunks) = media.body else {
        panic!("gateway: a transcode streams chunks");
    };
    assert!(chunks.next().await.expect("chunk").is_ok());
    assert_eq!(gate.active(), 1, "gateway");

    drop(chunks);
    settled(&gate).await;
    assert_eq!(probe.stops(), 1, "gateway: disconnect stops ffmpeg");
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
