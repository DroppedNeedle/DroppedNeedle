//! Gateway-engine tests: leases, sandboxing, and the engine seam.
//!
//! The engine (`Gateway`) is tested directly with a scripted remote reader
//! and a scripted ffmpeg service. No live servers, no real ffmpeg, no
//! network. Local reads use a per-test scratch dir under the system temp
//! dir, removed afterwards.

use droppedneedle::stream;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt as _;
use stream::gateway::{Gateway, RemoteMedia, RemoteReader};
use stream::leases::DirectGate;
use stream::routes::{
    AudioSource, MediaBody, OpenMedia, StreamEngine, StreamFault, StreamOpen, StreamParams,
    file_range,
};
use stream::transcode::{StreamPlan, TranscodeBody, TranscodeError, TranscodeSettings, Transcoder};

/// Unique scratch root per test. Removed by [`ScratchRoot::drop`].
struct ScratchRoot {
    path: PathBuf,
}

impl ScratchRoot {
    fn new(tag: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "droppedneedle-stream-{}-{}-{}",
            std::process::id(),
            id,
            tag
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.path.join(name), bytes).unwrap();
    }
}

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Scripted remote reader: canned objects plus canned faults by key.
#[derive(Clone, Default)]
struct FakeRemote {
    objects: HashMap<String, RemoteMedia>,
    faults: HashMap<String, StreamFault>,
}

impl RemoteReader for FakeRemote {
    async fn fetch(
        &self,
        _source: AudioSource,
        key: &str,
        _user_id: &str,
    ) -> Result<RemoteMedia, StreamFault> {
        if let Some(fault) = self.faults.get(key) {
            return Err(fault.clone());
        }
        self.objects.get(key).cloned().ok_or(StreamFault::NotFound)
    }
}

/// Scripted ffmpeg body: canned chunks, no process. A stalling body never
/// ends after its chunks, like a transcode still running.
struct FakeBody {
    chunks: Vec<Vec<u8>>,
    stall: bool,
}

impl TranscodeBody for FakeBody {
    async fn next_chunk(
        &mut self,
        _is_disconnected: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<Option<Vec<u8>>, TranscodeError> {
        if self.chunks.is_empty() {
            if self.stall {
                std::future::pending::<()>().await;
            }
            Ok(None)
        } else {
            Ok(Some(self.chunks.remove(0)))
        }
    }

    async fn close(&mut self) {}
}

/// Scripted ffmpeg service: canned output per call, or a canned fault.
#[derive(Clone)]
struct FakeTranscoder {
    chunks: Vec<Vec<u8>>,
    stall: bool,
    fault: Option<TranscodeError>,
    calls: Arc<std::sync::Mutex<Vec<StreamPlan>>>,
}

impl FakeTranscoder {
    fn succeeding(chunks: Vec<Vec<u8>>) -> Self {
        Self {
            chunks,
            stall: false,
            fault: None,
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    fn failing(fault: TranscodeError) -> Self {
        Self {
            chunks: Vec::new(),
            stall: false,
            fault: Some(fault),
            calls: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

impl Transcoder for FakeTranscoder {
    type Body = FakeBody;

    async fn stream(
        &self,
        _source_path: &Path,
        plan: &StreamPlan,
        _principal: &str,
    ) -> Result<Self::Body, TranscodeError> {
        self.calls.lock().unwrap().push(plan.clone());
        if let Some(fault) = &self.fault {
            return Err(fault.clone());
        }
        Ok(FakeBody {
            chunks: self.chunks.clone(),
            stall: self.stall,
        })
    }
}

/// One engine over a scratch root, a fake remote, and a fake ffmpeg.
fn engine(
    root: &ScratchRoot,
    remote: FakeRemote,
    transcoder: FakeTranscoder,
    ffmpeg_present: bool,
) -> Gateway<FakeRemote, FakeTranscoder> {
    Gateway::new(
        root.path.clone(),
        remote,
        transcoder,
        TranscodeSettings::default(),
        ffmpeg_present,
    )
}

/// One open request with default (direct) params.
fn open(source: AudioSource, key: &str) -> StreamOpen {
    StreamOpen {
        source,
        key: key.to_owned(),
        user_id: "user-1".to_owned(),
        params: StreamParams::default(),
    }
}

/// One open request with transcode hints.
fn open_transcode(key: &str, format: &str) -> StreamOpen {
    StreamOpen {
        source: AudioSource::Local,
        key: key.to_owned(),
        user_id: "user-1".to_owned(),
        params: StreamParams {
            format: Some(format.to_owned()),
            max_bitrate_kbps: None,
            estimate_content_length: false,
            start_seconds: 0.0,
            force_transcode: false,
        },
    }
}

#[tokio::test]
async fn local_direct_reads_file_bytes() {
    let root = ScratchRoot::new("direct");
    root.write("song.flac", b"FLAC-BYTES");
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::succeeding(vec![]),
        true,
    );

    let media: OpenMedia = app
        .open(open(AudioSource::Local, "song.flac"))
        .await
        .unwrap();
    assert_eq!(media.content_type, "audio/flac");
    assert_eq!(media.total_len, 10);
    assert!(!media.transcoded);
    assert_eq!(media.bytes, b"FLAC-BYTES");
}

#[tokio::test]
async fn local_escape_attempts_are_forbidden() {
    let root = ScratchRoot::new("sandbox");
    root.write("song.mp3", b"MP3");
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::succeeding(vec![]),
        true,
    );

    for key in ["../song.mp3", "/etc/hostname", "", "sub/../../song.mp3"] {
        let fault = app.open(open(AudioSource::Local, key)).await.unwrap_err();
        assert_eq!(
            fault,
            StreamFault::Forbidden {
                message: "Playback not allowed".to_owned()
            },
            "key: {key}"
        );
    }
}

#[tokio::test]
#[cfg(unix)]
async fn symlink_escape_is_forbidden() {
    let root = ScratchRoot::new("symlink-root");
    root.write("song.mp3", b"MP3");
    let outside = ScratchRoot::new("symlink-outside");
    outside.write("secret.mp3", b"SECRET");
    std::os::unix::fs::symlink(outside.path.join("secret.mp3"), root.path.join("link.mp3"))
        .unwrap();
    std::os::unix::fs::symlink(root.path.join("song.mp3"), root.path.join("inner.mp3")).unwrap();
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::succeeding(vec![]),
        true,
    );

    let fault = app
        .open(open(AudioSource::Local, "link.mp3"))
        .await
        .unwrap_err();
    assert_eq!(
        fault,
        StreamFault::Forbidden {
            message: "Playback not allowed".to_owned()
        }
    );

    let media = app
        .open(open(AudioSource::Local, "inner.mp3"))
        .await
        .unwrap();
    assert_eq!(media.bytes, b"MP3");
}

#[tokio::test]
async fn wma_never_streams() {
    let root = ScratchRoot::new("wma");
    root.write("song.wma", b"WMA");
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::succeeding(vec![]),
        true,
    );

    let fault = app
        .open(open(AudioSource::Local, "song.wma"))
        .await
        .unwrap_err();
    assert_eq!(
        fault,
        StreamFault::InvalidInput {
            message: "Unsupported audio format".to_owned()
        }
    );
}

#[tokio::test]
async fn remote_direct_proxies_upstream_bytes() {
    let root = ScratchRoot::new("remote");
    let remote = FakeRemote {
        objects: HashMap::from([(
            "item-9".to_owned(),
            RemoteMedia {
                content_type: "audio/x-upstream".to_owned(),
                bytes: b"UP".to_vec(),
            },
        )]),
        faults: HashMap::new(),
    };
    let app = engine(&root, remote, FakeTranscoder::succeeding(vec![]), true);

    let media = app
        .open(open(AudioSource::Jellyfin, "item-9"))
        .await
        .unwrap();
    assert_eq!(media.content_type, "audio/x-upstream");
    assert_eq!(media.bytes, b"UP");
    assert!(!media.transcoded);
}

#[tokio::test]
async fn codec_mismatch_transcodes_through_ffmpeg() {
    let root = ScratchRoot::new("transcode");
    root.write("song.flac", b"FLAC-BYTES");
    let transcoder = FakeTranscoder::succeeding(vec![b"MP3-".to_vec(), b"OUT".to_vec()]);
    let app = engine(&root, FakeRemote::default(), transcoder, true);

    let media = app.open(open_transcode("song.flac", "mp3")).await.unwrap();
    assert!(media.transcoded);
    assert_eq!(media.content_type, "audio/mpeg");
    assert_eq!(media.bytes, b"MP3-OUT");
}

#[tokio::test]
async fn raw_format_stays_direct() {
    let root = ScratchRoot::new("raw");
    root.write("song.flac", b"FLAC-BYTES");
    let transcoder = FakeTranscoder::succeeding(vec![b"SHOULD-NOT-RUN".to_vec()]);
    let calls = Arc::clone(&transcoder.calls);
    let app = engine(&root, FakeRemote::default(), transcoder, true);

    let media = app.open(open_transcode("song.flac", "raw")).await.unwrap();
    assert!(!media.transcoded);
    assert_eq!(media.bytes, b"FLAC-BYTES");
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ffmpeg_absent_falls_back_to_direct() {
    let root = ScratchRoot::new("no-ffmpeg");
    root.write("song.flac", b"FLAC-BYTES");
    let transcoder = FakeTranscoder::succeeding(vec![b"SHOULD-NOT-RUN".to_vec()]);
    let calls = Arc::clone(&transcoder.calls);
    let app = engine(&root, FakeRemote::default(), transcoder, false);

    let media = app.open(open_transcode("song.flac", "mp3")).await.unwrap();
    assert!(!media.transcoded);
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn transcode_capacity_maps_to_fault() {
    let root = ScratchRoot::new("transcode-cap");
    root.write("song.flac", b"FLAC-BYTES");
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::failing(TranscodeError::Capacity),
        true,
    );

    let fault = app
        .open(open_transcode("song.flac", "mp3"))
        .await
        .unwrap_err();
    assert_eq!(fault, StreamFault::Capacity);
}

#[tokio::test]
async fn seek_then_transcode_starts_at_offset() {
    // M1: the seek offset rides the params into decide(), so the ffmpeg
    // plan starts at T instead of 0.
    let root = ScratchRoot::new("seek-transcode");
    root.write("song.flac", b"FLAC-BYTES");
    let transcoder = FakeTranscoder::succeeding(vec![b"OUT".to_vec()]);
    let calls = Arc::clone(&transcoder.calls);
    let app = engine(&root, FakeRemote::default(), transcoder, true);

    let mut request = open_transcode("song.flac", "mp3");
    request.params.start_seconds = 42.5;
    let media = app.open(request).await.unwrap();
    assert!(media.transcoded);
    let plans = calls.lock().unwrap();
    assert_eq!(plans.len(), 1);
    match &plans[0] {
        StreamPlan::Transcode { start_seconds, .. } => {
            assert_eq!(*start_seconds, 42.5);
        }
        StreamPlan::Direct { .. } => panic!("seek-transcode must not land direct"),
    }
}

#[tokio::test]
async fn forced_verdict_transcodes_same_codec_despite_unknown_source_bitrate() {
    // M2: the gateway re-decide sees no source bitrate, so a same-codec
    // bitrate plan lands direct, unless the compat adapter carries the
    // explicit `force_transcode` verdict, which the gateway then honors.
    let root = ScratchRoot::new("forced");
    root.write("song.mp3", b"MP3-BYTES");
    let transcoder = FakeTranscoder::succeeding(vec![b"OUT".to_vec()]);
    let calls = Arc::clone(&transcoder.calls);
    let app = engine(&root, FakeRemote::default(), transcoder, true);

    let mut request = open_transcode("song.mp3", "mp3");
    request.params.max_bitrate_kbps = Some(128);
    let media = app.open(request.clone()).await.unwrap();
    assert!(
        !media.transcoded,
        "no carry: unknown source bitrate lands direct"
    );

    request.params.force_transcode = true;
    request.params.start_seconds = 7.0;
    let media = app.open(request).await.unwrap();
    assert!(media.transcoded);
    assert_eq!(media.content_type, "audio/mpeg");
    let plans = calls.lock().unwrap();
    assert_eq!(plans.len(), 1);
    match &plans[0] {
        StreamPlan::Transcode {
            out_bitrate_kbps,
            start_seconds,
            ..
        } => {
            assert_eq!(*out_bitrate_kbps, 128);
            assert_eq!(*start_seconds, 7.0);
        }
        StreamPlan::Direct { .. } => panic!("forced verdict must transcode"),
    }
}

#[tokio::test]
async fn direct_gate_bounds_and_releases() {
    let gate = DirectGate::with_limits(1, 1, 4, std::time::Duration::from_millis(50));
    let first = gate.acquire("user-1").await.unwrap();
    assert_eq!(gate.active(), 1);

    let denied = gate.acquire("user-1").await.unwrap_err();
    assert_eq!(denied, stream::leases::CapacityExhausted);

    drop(first);
    assert_eq!(gate.active(), 0);
    let _reacquired = gate.acquire("user-1").await.unwrap();
}

#[tokio::test]
async fn cancelled_acquire_leaves_no_waiter() {
    let gate = DirectGate::with_limits(1, 8, 4, std::time::Duration::from_secs(5));
    let _held = gate.acquire("user-1").await.unwrap();

    // Poll once so the waiter registers, then drop the future: the cancel
    // guard must remove the waiter instead of blocking the queue forever.
    let waker = futures_util::task::noop_waker();
    let mut context = std::task::Context::from_waker(&waker);
    let mut waiter = Box::pin(gate.acquire("user-2"));
    assert!(matches!(
        waiter.as_mut().poll(&mut context),
        std::task::Poll::Pending
    ));
    assert_eq!(gate.waiter_count(), 1);
    drop(waiter);
    assert_eq!(gate.waiter_count(), 0);
}

// The streaming open reads lazily: a direct file comes back as the file
// and its size, HEAD of a transcode never starts ffmpeg, and transcode
// output arrives before the transcode ends.
#[tokio::test]
async fn streaming_open_reads_lazily() {
    let root = ScratchRoot::new("lazy");
    root.write("song.flac", b"FLAC-BYTES");
    let mut transcoder = FakeTranscoder::succeeding(vec![b"FIRST".to_vec()]);
    transcoder.stall = true;
    let calls = Arc::clone(&transcoder.calls);
    let app = engine(&root, FakeRemote::default(), transcoder, true);

    let direct = app
        .open_stream(open(AudioSource::Local, "song.flac"), false)
        .await
        .unwrap();
    assert_eq!(direct.total_len, 10);
    let MediaBody::File(path) = direct.body else {
        panic!("direct reads stream from the file");
    };
    let slice: Vec<u8> = file_range(path, 2, 3)
        .map(|chunk| chunk.unwrap())
        .concat()
        .await;
    assert_eq!(slice, b"AC-");

    let head = app
        .open_stream(open_transcode("song.flac", "mp3"), true)
        .await
        .unwrap();
    assert!(head.transcoded);
    assert!(matches!(head.body, MediaBody::Empty));
    assert!(calls.lock().unwrap().is_empty(), "HEAD runs no ffmpeg");

    let live = app
        .open_stream(open_transcode("song.flac", "mp3"), false)
        .await
        .unwrap();
    let MediaBody::Chunks(mut chunks) = live.body else {
        panic!("transcodes stream chunk by chunk");
    };
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), chunks.next())
        .await
        .expect("the first chunk arrives while the transcode runs");
    assert_eq!(first.unwrap().unwrap(), b"FIRST");
}
