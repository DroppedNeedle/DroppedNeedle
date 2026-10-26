//! The production `GatewayAudio` (Subsonic) and `GatewayStream` (Jellyfin)
//! adapters over a scripted stream engine: fault mapping, and that the
//! transcode codec, bitrate and seek offset reach the engine. Range and
//! HEAD framing is pinned on every stream route by the library journey's
//! range matrix.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use droppedneedle_compat::adapters::engines::{GatewayAudio, GatewayStream};
use droppedneedle_compat::body::AudioBody;
use droppedneedle_compat::jellyfin::seams::StreamEngine as JellyfinEngine;
use droppedneedle_compat::subsonic::stream::{
    AudioBackend, ServeError, StreamPlan, serve_original,
};
use droppedneedle_media::stream::routes::{
    MediaBody, StreamEngine, StreamFault, StreamMedia, StreamOpen,
};

/// Scripted engine: scripted files, recorded opens, scripted faults.
#[derive(Clone)]
struct StubEngine {
    files: HashMap<String, (Vec<u8>, String, bool)>,
    opens: Arc<Mutex<Vec<StreamOpen>>>,
}

impl StubEngine {
    fn new() -> Self {
        let bytes: Vec<u8> = (0..100u8).collect();
        Self {
            files: HashMap::from([
                (
                    "song.mp3".to_owned(),
                    (bytes, "audio/mpeg".to_owned(), false),
                ),
                (
                    "landed.mp3".to_owned(),
                    (b"TRANSCODED".to_vec(), "audio/mpeg".to_owned(), true),
                ),
            ]),
            opens: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn recorded(&self) -> Vec<StreamOpen> {
        self.opens.lock().expect("opens lock").clone()
    }
}

impl StreamEngine for StubEngine {
    async fn open_stream(
        &self,
        request: StreamOpen,
        _head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        self.opens.lock().expect("opens lock").push(request.clone());
        if request.key == "busy.mp3" {
            return Err(StreamFault::Capacity);
        }
        let (bytes, content_type, transcoded) = self
            .files
            .get(&request.key)
            .cloned()
            .ok_or(StreamFault::NotFound)?;
        Ok(StreamMedia {
            content_type,
            total_len: bytes.len() as u64,
            transcoded,
            estimated_len: None,
            body: MediaBody::Bytes(bytes),
            lease: None,
        })
    }
}

fn audio(engine: &StubEngine) -> GatewayAudio<StubEngine> {
    use droppedneedle_compat::subsonic::AudioBackend as _;
    GatewayAudio::new(Arc::new(engine.clone())).for_caller("test-user")
}

fn stream(engine: &StubEngine) -> GatewayStream<StubEngine> {
    GatewayStream::new(Arc::new(engine.clone()))
}

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

#[tokio::test]
async fn adapters_map_faults_and_forward_transcodes() {
    let engine = StubEngine::new();
    let audio = audio(&engine);
    let stream = stream(&engine);

    // Exhausted stream leases: 429 with Retry-After; unknown ids 404/70.
    let busy = serve_original(&audio, "busy.mp3", None, false, None)
        .await
        .expect_err("pool full");
    assert!(matches!(busy, ServeError::CapacityFull));
    let busy = stream.direct("busy.mp3", None).await;
    assert_eq!(busy.status, 429);
    assert_eq!(header(&busy.headers, "Retry-After").as_deref(), Some("1"));
    assert_eq!(stream.direct("missing.mp3", None).await.status, 404);
    let missing = serve_original(&audio, "missing.mp3", None, false, None)
        .await
        .expect_err("unknown id");
    assert!(matches!(missing, ServeError::Subsonic(_)));

    // A transcode landing is served whole and never honours a range.
    let landed = stream.direct("landed.mp3", Some("bytes=0-1")).await;
    assert_eq!(landed.status, 200);
    assert_eq!(
        header(&landed.headers, "Accept-Ranges").as_deref(),
        Some("none")
    );
    assert_eq!(landed.body, AudioBody::Bytes(b"TRANSCODED".to_vec()));

    // Transcodes forward codec, bitrate and offset; Jellyfin's carry no
    // length.
    let plan = StreamPlan {
        transcode: true,
        out_format: Some("opus".to_owned()),
        out_bitrate_kbps: Some(128),
        start_seconds: 30.0,
    };
    let before = engine.recorded().len();
    audio
        .transcode("song.mp3", &plan)
        .await
        .expect("transcodes");
    let params = &engine.recorded()[before].params;
    assert_eq!(params.format.as_deref(), Some("opus"));
    assert_eq!(params.max_bitrate_kbps, Some(128));
    assert_eq!(params.start_seconds, 30.0);
    assert!(params.force_transcode);

    let before = engine.recorded().len();
    let landed = stream.transcode("landed.mp3", "opus", 96, 12.5).await;
    assert_eq!(
        header(&landed.headers, "Cache-Control").as_deref(),
        Some("no-store")
    );
    assert!(header(&landed.headers, "Content-Length").is_none());
    let params = &engine.recorded()[before].params;
    assert_eq!(params.format.as_deref(), Some("opus"));
    assert_eq!(params.max_bitrate_kbps, Some(96));
    assert_eq!(params.start_seconds, 12.5);
    assert!(params.force_transcode);
}

/// Remote reader for a local-only gateway.
struct NoRemote;

impl droppedneedle_media::stream::gateway::RemoteReader for NoRemote {
    async fn open_stream(
        &self,
        _source: droppedneedle_media::stream::routes::AudioSource,
        _key: &str,
        _user_id: &str,
        _head_only: bool,
    ) -> Result<droppedneedle_media::stream::routes::StreamMedia, StreamFault> {
        let canned: Result<droppedneedle_media::stream::gateway::RemoteMedia, StreamFault> =
            async { Err(StreamFault::NotFound) }.await;
        canned.map(Into::into)
    }
}

#[tokio::test]
async fn real_gateway_streams_the_asked_span_from_the_file() {
    use droppedneedle_media::stream::gateway::Gateway;
    use droppedneedle_media::stream::leases::TranscodeGate;
    use droppedneedle_media::stream::transcode::{
        FfmpegTranscoder, ProcessSpawner, TranscodeSettings,
    };

    let root = crate::common::ScratchDir::new("compat-stream");
    std::fs::write(root.join("song.mp3"), (0..100u8).collect::<Vec<_>>()).unwrap();
    let gateway = Arc::new(Gateway::new(
        root.to_path_buf(),
        NoRemote,
        FfmpegTranscoder::new(
            ProcessSpawner::with_path("ffmpeg".into()),
            Arc::new(TranscodeGate::new()),
        ),
        TranscodeSettings::default(),
        false,
    ));

    let jellyfin = GatewayStream::new(Arc::clone(&gateway)).for_caller("u1");
    let got = jellyfin.direct("song.mp3", Some("bytes=10-19")).await;
    assert_eq!(got.status, 206);
    assert!(
        matches!(got.body, AudioBody::Live(_)),
        "files stream, never load whole"
    );
    assert_eq!(
        got.body.collect().await.unwrap(),
        (10..20u8).collect::<Vec<_>>()
    );

    let subsonic = GatewayAudio::new(gateway).for_caller("u1");
    let served = serve_original(&subsonic, "song.mp3", Some("bytes=-5"), false, None)
        .await
        .expect("serves");
    assert_eq!(
        header(&served.headers, "Content-Length").as_deref(),
        Some("5")
    );
    assert_eq!(
        served.body.collect().await.unwrap(),
        (95..100u8).collect::<Vec<_>>()
    );
}
