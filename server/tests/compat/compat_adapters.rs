//! The production `GatewayAudio` adapter over a scripted stream engine:
//! each protocol's fault mapping, that the translated parameters reach the
//! engine unchanged, and that the answer is framed by the plan the engine
//! ran. Range and HEAD framing is pinned on every stream route by the
//! library journey's range matrix.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use droppedneedle_compat::adapters::audio::GatewayAudio;
use droppedneedle_compat::body::AudioBody;
use droppedneedle_compat::subsonic::stream::{ServeError, serve};
use droppedneedle_media::stream::routes::{
    MediaBody, StreamEngine, StreamFault, StreamMedia, StreamOpen, StreamParams,
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
                    (b"TRANSCODED".to_vec(), "audio/ogg".to_owned(), true),
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
        match request.key.as_str() {
            "busy.mp3" => return Err(StreamFault::Capacity),
            "denied.mp3" => {
                return Err(StreamFault::Forbidden {
                    message: "Playback not allowed".to_owned(),
                });
            }
            "broken.mp3" => {
                return Err(StreamFault::Internal {
                    cause: "read /srv/music/broken.mp3: I/O error".to_owned(),
                });
            }
            _ => {}
        }
        let (bytes, content_type, transcoded) = self
            .files
            .get(&request.key)
            .cloned()
            .ok_or(StreamFault::NotFound)?;
        Ok(StreamMedia {
            content_type,
            total_len: if transcoded { 0 } else { bytes.len() as u64 },
            transcoded,
            estimated_len: transcoded.then_some(4_000),
            body: MediaBody::Bytes(bytes),
            lease: None,
        })
    }
}

fn adapter(engine: &StubEngine) -> GatewayAudio<StubEngine> {
    GatewayAudio::new(Arc::new(engine.clone()))
}

fn header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn opus(estimate: bool) -> StreamParams {
    StreamParams {
        format: Some("opus".to_owned()),
        max_bitrate_kbps: Some(96),
        start_seconds: 12.5,
        estimate_content_length: estimate,
    }
}

#[tokio::test]
async fn adapters_map_faults_once_and_frame_by_the_plan_run() {
    let engine = StubEngine::new();
    let audio = adapter(&engine);
    let original = StreamParams::original();

    // Each fault maps once: a full pool is 429 with Retry-After, an
    // unknown id 404 or 70, a refusal 403 or 50, a server fault a logged
    // 500 or code 0 with the fixed text.
    let busy = serve(&audio, "busy.mp3", &original, None, false, None)
        .await
        .expect_err("pool full");
    assert!(matches!(busy, ServeError::CapacityFull));
    let busy = audio
        .play_jellyfin("busy.mp3", &original, None, false)
        .await;
    assert_eq!(busy.status, 429);
    assert_eq!(header(&busy.headers, "Retry-After").as_deref(), Some("1"));
    for (key, jellyfin, subsonic) in [
        ("missing.mp3", 404, 70),
        ("denied.mp3", 403, 50),
        ("broken.mp3", 500, 0),
    ] {
        let answer = audio.play_jellyfin(key, &original, None, false).await;
        assert_eq!(answer.status, jellyfin, "{key}");
        let Err(ServeError::Subsonic(error)) =
            serve(&audio, key, &original, None, false, None).await
        else {
            panic!("{key}: a protocol error");
        };
        assert_eq!(error.code, subsonic, "{key}");
        assert!(!error.message.contains("/srv/"), "{key}: {}", error.message);
    }

    // The translated parameters reach the engine unchanged.
    let before = engine.recorded().len();
    audio
        .play_jellyfin("landed.mp3", &opus(false), None, false)
        .await;
    assert_eq!(engine.recorded()[before].params, opus(false));

    // A transcode landing is served whole and never honours a range; the
    // estimate rides only when asked for, and never on Jellyfin.
    let landed = audio
        .play_jellyfin("landed.mp3", &opus(true), Some("bytes=0-1"), false)
        .await;
    assert_eq!(landed.status, 200);
    assert_eq!(
        header(&landed.headers, "Accept-Ranges").as_deref(),
        Some("none")
    );
    assert!(header(&landed.headers, "Content-Length").is_none());
    assert_eq!(landed.body, AudioBody::Bytes(b"TRANSCODED".to_vec()));
    for (estimate, length) in [(true, Some("4000")), (false, None)] {
        let served = serve(&audio, "landed.mp3", &opus(estimate), None, false, None)
            .await
            .expect("transcodes");
        assert_eq!(served.content_type, "audio/ogg");
        assert_eq!(header(&served.headers, "Content-Length").as_deref(), length);
        assert_eq!(
            header(&served.headers, "Cache-Control").as_deref(),
            Some("no-store")
        );
    }

    // A transcode request the engine served direct is framed as direct.
    let direct = audio
        .play_jellyfin("song.mp3", &opus(false), Some("bytes=0-1"), false)
        .await;
    assert_eq!(direct.status, 206);
    assert_eq!(
        header(&direct.headers, "Accept-Ranges").as_deref(),
        Some("bytes")
    );
    let served = serve(
        &audio,
        "song.mp3",
        &opus(true),
        Some("bytes=0-1"),
        false,
        None,
    )
    .await
    .expect("serves");
    assert_eq!(
        (served.status, served.content_type.as_str()),
        (206, "audio/mpeg")
    );
    assert_eq!(
        header(&served.headers, "Content-Length").as_deref(),
        Some("2")
    );
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
    let audio = GatewayAudio::new(gateway).for_caller("u1");

    let got = audio
        .play_jellyfin(
            "song.mp3",
            &StreamParams::original(),
            Some("bytes=10-19"),
            false,
        )
        .await;
    assert_eq!(got.status, 206);
    assert!(
        matches!(got.body, AudioBody::Live(_)),
        "files stream, never load whole"
    );
    assert_eq!(
        got.body.collect().await.unwrap(),
        (10..20u8).collect::<Vec<_>>()
    );

    let served = serve(
        &audio,
        "song.mp3",
        &StreamParams::original(),
        Some("bytes=-5"),
        false,
        None,
    )
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
