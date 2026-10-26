//! Gateway-engine tests: the stream gates, sandboxing, and the engine seam.
//!
//! The engine (`Gateway`) is tested directly with a scripted remote reader
//! and a scripted ffmpeg service. No live servers, no real ffmpeg, no
//! network. Local reads use a per-test scratch dir under the system temp
//! dir, removed afterwards. The last test puts the real engine behind the
//! stream routes: the per-user cap on concurrent direct streams.

use droppedneedle_media::stream;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use droppedneedle_auth::users::roles::{Role, SessionKind};
use futures_util::StreamExt as _;
use serde_json::Value;
use stream::gateway::{Gateway, RemoteMedia, RemoteReader};
use stream::leases::{
    CapacityExhausted, DIRECT_PRINCIPAL_LIMIT, DirectGate, GateLimits, OwnedDirectLease,
    TranscodeGate, WAIT_TIMEOUT,
};
use stream::routes::{
    AudioSource, ChunkStream, MediaBody, StreamEngine, StreamFault, StreamMedia, StreamOpen,
    StreamParams, StreamState, file_range, stream_routes,
};
use stream::transcode::{StreamPlan, TranscodeError, TranscodeSettings, Transcoder};
use tower::ServiceExt as _;

use crate::common::ScratchDir;
use crate::common::auth::{test_principal, with_test_principal};

/// Write one file into a scratch root.
fn put(root: &Path, name: &str, bytes: &[u8]) {
    std::fs::write(root.join(name), bytes).unwrap();
}

/// Scripted remote reader: canned objects plus canned faults by key.
#[derive(Clone, Default)]
struct FakeRemote {
    objects: HashMap<String, RemoteMedia>,
    faults: HashMap<String, StreamFault>,
}

impl RemoteReader for FakeRemote {
    async fn open_stream(
        &self,
        _source: AudioSource,
        key: &str,
        _user_id: &str,
        _head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        let canned: Result<RemoteMedia, StreamFault> = async {
            if let Some(fault) = self.faults.get(key) {
                return Err(fault.clone());
            }
            self.objects.get(key).cloned().ok_or(StreamFault::NotFound)
        }
        .await;
        canned.map(Into::into)
    }
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
    async fn stream(
        &self,
        _source_path: &Path,
        plan: &StreamPlan,
        _principal: &str,
        _direct: Option<OwnedDirectLease>,
    ) -> Result<ChunkStream, TranscodeError> {
        self.calls.lock().unwrap().push(plan.clone());
        if let Some(fault) = &self.fault {
            return Err(fault.clone());
        }
        let chunks = futures_util::stream::iter(self.chunks.clone().into_iter().map(Ok));
        // A stalling body never ends after its chunks, like a transcode
        // still running.
        Ok(if self.stall {
            chunks.chain(futures_util::stream::pending()).boxed()
        } else {
            chunks.boxed()
        })
    }
}

/// One engine over a scratch root, a fake remote, and a fake ffmpeg.
fn engine(
    root: &ScratchDir,
    remote: FakeRemote,
    transcoder: FakeTranscoder,
    ffmpeg_present: bool,
) -> Gateway<FakeRemote, FakeTranscoder> {
    Gateway::new(
        root.to_path_buf(),
        remote,
        transcoder,
        TranscodeSettings::default(),
        ffmpeg_present,
    )
}

/// Whole bytes of a local direct open, read through its file body.
async fn file_bytes(media: StreamMedia) -> Vec<u8> {
    let MediaBody::File(path) = media.body else {
        panic!("a local direct read streams from the file");
    };
    file_range(path, 0, media.total_len)
        .map(|chunk| chunk.unwrap())
        .concat()
        .await
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
        },
    }
}

#[tokio::test]
async fn local_escape_attempts_are_forbidden() {
    let root = ScratchDir::new("sandbox");
    put(&root, "song.mp3", b"MP3");
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::succeeding(vec![]),
        true,
    );

    for key in ["../song.mp3", "/etc/hostname", "", "sub/../../song.mp3"] {
        let fault = app
            .open_stream(open(AudioSource::Local, key), false)
            .await
            .unwrap_err();
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
    let root = ScratchDir::new("symlink-root");
    put(&root, "song.mp3", b"MP3");
    let outside = ScratchDir::new("symlink-outside");
    put(&outside, "secret.mp3", b"SECRET");
    std::os::unix::fs::symlink(outside.join("secret.mp3"), root.join("link.mp3")).unwrap();
    std::os::unix::fs::symlink(root.join("song.mp3"), root.join("inner.mp3")).unwrap();
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::succeeding(vec![]),
        true,
    );

    let fault = app
        .open_stream(open(AudioSource::Local, "link.mp3"), false)
        .await
        .unwrap_err();
    assert_eq!(
        fault,
        StreamFault::Forbidden {
            message: "Playback not allowed".to_owned()
        }
    );

    let media = app
        .open_stream(open(AudioSource::Local, "inner.mp3"), false)
        .await
        .unwrap();
    assert_eq!(file_bytes(media).await, b"MP3");
}

#[tokio::test]
async fn wma_never_streams() {
    let root = ScratchDir::new("wma");
    put(&root, "song.wma", b"WMA");
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::succeeding(vec![]),
        true,
    );

    let fault = app
        .open_stream(open(AudioSource::Local, "song.wma"), false)
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
async fn raw_format_stays_direct() {
    let root = ScratchDir::new("raw");
    put(&root, "song.flac", b"FLAC-BYTES");
    let transcoder = FakeTranscoder::succeeding(vec![b"SHOULD-NOT-RUN".to_vec()]);
    let calls = Arc::clone(&transcoder.calls);
    let app = engine(&root, FakeRemote::default(), transcoder, true);

    let media = app
        .open_stream(open_transcode("song.flac", "raw"), false)
        .await
        .unwrap();
    assert!(!media.transcoded);
    assert_eq!(file_bytes(media).await, b"FLAC-BYTES");
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn ffmpeg_absent_falls_back_to_direct() {
    let root = ScratchDir::new("no-ffmpeg");
    put(&root, "song.flac", b"FLAC-BYTES");
    let transcoder = FakeTranscoder::succeeding(vec![b"SHOULD-NOT-RUN".to_vec()]);
    let calls = Arc::clone(&transcoder.calls);
    let app = engine(&root, FakeRemote::default(), transcoder, false);

    let media = app
        .open_stream(open_transcode("song.flac", "mp3"), false)
        .await
        .unwrap();
    assert!(!media.transcoded);
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn transcode_capacity_maps_to_fault() {
    let root = ScratchDir::new("transcode-cap");
    put(&root, "song.flac", b"FLAC-BYTES");
    let app = engine(
        &root,
        FakeRemote::default(),
        FakeTranscoder::failing(TranscodeError::Capacity),
        true,
    );

    let fault = app
        .open_stream(open_transcode("song.flac", "mp3"), false)
        .await
        .unwrap_err();
    assert_eq!(fault, StreamFault::Capacity);
}

#[tokio::test]
async fn seek_then_transcode_starts_at_offset() {
    // The seek offset rides the params into decide(), so the ffmpeg
    // plan starts at T instead of 0.
    let root = ScratchDir::new("seek-transcode");
    put(&root, "song.flac", b"FLAC-BYTES");
    let transcoder = FakeTranscoder::succeeding(vec![b"OUT".to_vec()]);
    let calls = Arc::clone(&transcoder.calls);
    let app = engine(&root, FakeRemote::default(), transcoder, true);

    let mut request = open_transcode("song.flac", "mp3");
    request.params.start_seconds = 42.5;
    let media = app.open_stream(request, false).await.unwrap();
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

/// Both pools are one gate type: a global cap, a per-key cap, a bounded
/// wait that ends in `CapacityExhausted` (the routers' 429), and a slot
/// that comes back when its lease drops.
#[tokio::test]
async fn gate_bounds_globally_and_per_key_then_frees() {
    let gate = Arc::new(TranscodeGate::with_limits(GateLimits {
        global: 2,
        per_key: 1,
        max_waiters: 4,
        wait: std::time::Duration::from_millis(50),
    }));
    let alice = gate.acquire_owned("alice").await.unwrap();
    let _bob = gate.acquire_owned("bob").await.unwrap();
    assert_eq!(gate.active(), 2);
    assert_eq!(
        gate.acquire_owned("carol").await.unwrap_err(),
        CapacityExhausted,
        "global cap"
    );
    drop(alice);
    assert_eq!(
        gate.acquire_owned("bob").await.unwrap_err(),
        CapacityExhausted,
        "per-key cap with a global slot free"
    );
    assert_eq!(gate.waiter_count(), 0, "timed-out waiters leave no trace");
    let _alice = gate.acquire_owned("alice").await.unwrap();
    assert_eq!(gate.active(), 2);
}

#[tokio::test]
async fn cancelled_acquire_leaves_no_waiter() {
    let gate = Arc::new(DirectGate::with_limits(GateLimits {
        global: 1,
        per_key: 8,
        max_waiters: 4,
        wait: std::time::Duration::from_secs(5),
    }));
    let _held = gate.acquire_owned("user-1").await.unwrap();

    // Poll once so the waiter registers, then drop the future: the cancel
    // guard must remove the waiter instead of blocking the queue forever.
    let waker = futures_util::task::noop_waker();
    let mut context = std::task::Context::from_waker(&waker);
    let mut waiter = Box::pin(gate.acquire_owned("user-2"));
    assert!(matches!(
        waiter.as_mut().poll(&mut context),
        std::task::Poll::Pending
    ));
    assert_eq!(gate.waiter_count(), 1);
    drop(waiter);
    assert_eq!(gate.waiter_count(), 0);
}

// The open reads lazily: a direct file comes back as the file and its
// size, HEAD of a transcode never starts ffmpeg, and transcode output
// arrives before the transcode ends.
#[tokio::test]
async fn streaming_open_reads_lazily() {
    let root = ScratchDir::new("lazy");
    put(&root, "song.flac", b"FLAC-BYTES");
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
    assert_eq!(live.content_type, "audio/mpeg");
    let MediaBody::Chunks(mut chunks) = live.body else {
        panic!("transcodes stream chunk by chunk");
    };
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), chunks.next())
        .await
        .expect("the first chunk arrives while the transcode runs");
    assert_eq!(first.unwrap().unwrap(), b"FIRST");
}

// ---------------------------------------------------------------------------
// Through the stream routes: the per-user caps
// ---------------------------------------------------------------------------

/// One request returning raw bytes (stream bodies are not JSON).
async fn call_raw(
    app: Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .oneshot(builder.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .expect("body reads")
        .to_vec();
    (status, headers, bytes)
}

/// Stream routes signed in as `user-1`.
fn stream_app<E: StreamEngine + Send + Sync + 'static>(engine: E) -> Router {
    let state = StreamState {
        engine: Arc::new(engine),
    };
    with_test_principal(
        stream_routes(state),
        test_principal("user-1", Role::User, "sess-1", SessionKind::Standard),
    )
}

fn stream_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Transcoder that never runs: the caps test only opens remote keys.
struct NeverTranscoder;

impl Transcoder for NeverTranscoder {
    async fn stream(
        &self,
        _source_path: &Path,
        _plan: &StreamPlan,
        _principal: &str,
        _direct: Option<OwnedDirectLease>,
    ) -> Result<ChunkStream, TranscodeError> {
        Err(TranscodeError::Capacity)
    }
}

/// Remote reader serving one canned object.
struct CannedRemote {
    media: RemoteMedia,
}

impl RemoteReader for CannedRemote {
    async fn open_stream(
        &self,
        _source: AudioSource,
        _key: &str,
        _user_id: &str,
        _head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        let canned: Result<RemoteMedia, StreamFault> = async { Ok(self.media.clone()) }.await;
        canned.map(Into::into)
    }
}

/// Remote reader that holds every fetch until released, so the direct-gate
/// slots stay occupied while the N+1th request arrives. Clones share one
/// gate, so the test holds a control handle beside the engine's copy.
#[derive(Clone)]
struct BlockingRemote {
    inner: Arc<BlockingRemoteInner>,
}

struct BlockingRemoteInner {
    media: RemoteMedia,
    entries: tokio::sync::watch::Sender<usize>,
    release: tokio::sync::Notify,
}

impl BlockingRemote {
    fn new(media: RemoteMedia) -> Self {
        Self {
            inner: Arc::new(BlockingRemoteInner {
                media,
                entries: tokio::sync::watch::Sender::new(0),
                release: tokio::sync::Notify::new(),
            }),
        }
    }

    async fn wait_for_entries(&self, want: usize) {
        let mut entries = self.inner.entries.subscribe();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            entries.wait_for(|entered| *entered >= want),
        )
        .await
        .expect("holders reach the remote")
        .expect("the remote is alive");
    }
}

impl RemoteReader for BlockingRemote {
    async fn open_stream(
        &self,
        _source: AudioSource,
        _key: &str,
        _user_id: &str,
        _head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        let canned: Result<RemoteMedia, StreamFault> = async {
            self.inner.entries.send_modify(|entered| *entered += 1);
            self.inner.release.notified().await;
            Ok(self.inner.media.clone())
        }
        .await;
        canned.map(Into::into)
    }
}

#[tokio::test]
async fn media_concurrent_streams_under_caps() {
    assert_eq!(
        DIRECT_PRINCIPAL_LIMIT, 8,
        "the eight join arms below track the per-principal cap"
    );
    let bytes: Vec<u8> = (0..1024).map(|index| (index % 251) as u8).collect();
    let media = RemoteMedia {
        content_type: "audio/mpeg".to_owned(),
        bytes: bytes.clone(),
    };
    let scratch = ScratchDir::new("media-caps");
    let root = scratch.to_path_buf();

    // Under the caps, N concurrent reads all succeed.
    let engine = Gateway::new(
        root.clone(),
        CannedRemote {
            media: media.clone(),
        },
        NeverTranscoder,
        TranscodeSettings::default(),
        false,
    );
    let app = stream_app(engine);
    let (r0, r1, r2, r3, r4, r5, r6, r7) = tokio::join!(
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
        call_raw(
            app.clone(),
            "GET",
            "/stream/plex/part-1",
            &[("range", "bytes=0-9")]
        ),
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
        call_raw(
            app.clone(),
            "GET",
            "/stream/plex/part-1",
            &[("range", "bytes=0-9")]
        ),
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
        call_raw(
            app.clone(),
            "GET",
            "/stream/plex/part-1",
            &[("range", "bytes=0-9")]
        ),
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
        call_raw(
            app.clone(),
            "GET",
            "/stream/plex/part-1",
            &[("range", "bytes=0-9")]
        ),
    );
    for (index, (status, headers, body)) in [r0, r1, r2, r3, r4, r5, r6, r7].into_iter().enumerate()
    {
        if index % 2 == 0 {
            assert_eq!(status, StatusCode::OK, "reader {index}");
            assert_eq!(body, bytes, "reader {index}");
        } else {
            assert_eq!(status, StatusCode::PARTIAL_CONTENT, "reader {index}");
            assert_eq!(body, bytes[0..10], "reader {index}");
            assert_eq!(
                stream_header(&headers, "content-range"),
                Some("bytes 0-9/1024".to_owned()),
                "reader {index}"
            );
        }
    }

    // With all N slots held, the N+1th request waits out the gate and 429s.
    // The leases release before the responses send, so the 429 only shows
    // while the N holders are still inside their opens.
    let blocking = BlockingRemote::new(media.clone());
    let engine = Gateway::new(
        root,
        blocking.clone(),
        NeverTranscoder,
        TranscodeSettings::default(),
        false,
    );
    let app = stream_app(engine);
    let mut holders = Vec::new();
    for _ in 0..DIRECT_PRINCIPAL_LIMIT {
        let app = app.clone();
        holders.push(tokio::spawn(async move {
            call_raw(app, "GET", "/stream/plex/part-1", &[]).await
        }));
    }
    blocking.wait_for_entries(DIRECT_PRINCIPAL_LIMIT).await;
    let (status, headers, body) = tokio::time::timeout(
        WAIT_TIMEOUT + std::time::Duration::from_secs(10),
        call_raw(app.clone(), "GET", "/stream/plex/part-1", &[]),
    )
    .await
    .expect("the N+1th request answers");
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(stream_header(&headers, "retry-after"), Some("1".to_owned()));
    let envelope: Value = serde_json::from_slice(&body).expect("429 envelope is json");
    assert_eq!(envelope["error"]["code"], "STREAM_CAPACITY_EXHAUSTED");

    blocking.inner.release.notify_waiters();
    let [h0, h1, h2, h3, h4, h5, h6, h7]: [_; 8] = holders.try_into().expect("eight holders");
    let (r0, r1, r2, r3, r4, r5, r6, r7) = tokio::join!(h0, h1, h2, h3, h4, h5, h6, h7);
    for result in [r0, r1, r2, r3, r4, r5, r6, r7] {
        let (status, _, body) = result.expect("holder completes");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, bytes);
    }
}
