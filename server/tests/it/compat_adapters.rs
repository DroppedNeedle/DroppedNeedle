//! Compat engine-adapter briefs: the REAL `GatewayAudio`/`GatewayStream`
//! adapters over a stub stage-6 engine.
//!
//! The journeys prove the wire contract through the protocol fakes; these
//! briefs prove the adapters themselves forward opens faithfully: direct
//! 200/206/416/HEAD parity per protocol, transcode param forwarding
//! (codec + bitrate + seek offset + forced verdict), single-open reads,
//! and the 429/404 fault mappings.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use droppedneedle::compat::adapters::engines::{GatewayAudio, GatewayStream};
use droppedneedle::compat::jellyfin::seams::StreamEngine as JellyfinEngine;
use droppedneedle::compat::subsonic::stream::{AudioBackend, StreamPlan, serve_original};
use droppedneedle::stream::routes::{OpenMedia, StreamEngine, StreamFault, StreamOpen};

/// Stub stage-6 engine: scripted files, recorded opens, scripted faults.
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
    async fn open(&self, request: StreamOpen) -> Result<OpenMedia, StreamFault> {
        self.opens.lock().expect("opens lock").push(request.clone());
        if request.key == "busy.mp3" {
            return Err(StreamFault::Capacity);
        }
        let (bytes, content_type, transcoded) = self
            .files
            .get(&request.key)
            .cloned()
            .ok_or(StreamFault::NotFound)?;
        Ok(OpenMedia {
            content_type,
            total_len: bytes.len() as u64,
            transcoded,
            estimated_len: None,
            bytes,
        })
    }
}

fn audio(engine: &StubEngine) -> GatewayAudio<StubEngine> {
    GatewayAudio::new(Arc::new(engine.clone()), "test-user".to_owned())
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

// --- Subsonic adapter ---

#[tokio::test]
async fn gateway_audio_serves_identity_and_ranges_in_one_open_each() {
    let engine = StubEngine::new();
    let audio = audio(&engine);

    // Identity: full object, one open.
    let served = serve_original(&audio, "song.mp3", None, false, None)
        .await
        .expect("identity serves");
    assert_eq!(served.status, 200);
    assert_eq!(served.content_type, "audio/mpeg");
    assert_eq!(served.body.len(), 100);
    assert_eq!(engine.recorded().len(), 1, "one open per stream");

    // Seek slice: exact bytes, one more open (facts + range sliced from
    // the single read, never two opens).
    let served = serve_original(&audio, "song.mp3", Some("bytes=10-19"), false, None)
        .await
        .expect("range serves");
    assert_eq!(served.status, 206);
    assert_eq!(served.body.len(), 10);
    assert_eq!(served.body[0], 10);
    assert_eq!(
        header(&served.headers, "Content-Range").as_deref(),
        Some("bytes 10-19/100")
    );
    assert_eq!(engine.recorded().len(), 2);

    // Trailing whitespace seeks like the bare header (m8: every parser
    // trims, so this is a 206 everywhere, never a 416).
    let served = serve_original(&audio, "song.mp3", Some("bytes=0-1 "), false, None)
        .await
        .expect("spaced range serves");
    assert_eq!(served.status, 206);
    assert_eq!(served.body, vec![0, 1]);
}

#[tokio::test]
async fn gateway_audio_answers_416_and_head_without_bodies() {
    let engine = StubEngine::new();
    let audio = audio(&engine);

    let err = serve_original(&audio, "song.mp3", Some("bytes=100-"), false, None)
        .await
        .expect_err("past-the-end is 416");
    assert!(matches!(
        err,
        droppedneedle::compat::subsonic::stream::ServeError::RangeUnsatisfiable(100)
    ));

    // HEAD: facts only, empty body, GET-equivalent headers.
    let served = serve_original(&audio, "song.mp3", None, true, None)
        .await
        .expect("head serves");
    assert_eq!(served.status, 200);
    assert!(served.body.is_empty());
    assert_eq!(
        header(&served.headers, "Content-Length").as_deref(),
        Some("100")
    );
    let served = serve_original(&audio, "song.mp3", Some("bytes=5-9"), true, None)
        .await
        .expect("head with range serves");
    assert_eq!(served.status, 206);
    assert_eq!(
        header(&served.headers, "Content-Range").as_deref(),
        Some("bytes 5-9/100")
    );
    assert!(served.body.is_empty());
}

#[tokio::test]
async fn gateway_audio_forwards_transcode_codec_bitrate_and_offset() {
    // M1/M2 adapter half: the compat plan's codec, bitrate, seek offset,
    // and forced verdict all reach the engine open.
    let engine = StubEngine::new();
    let audio = audio(&engine);
    let plan = StreamPlan {
        transcode: true,
        out_format: Some("opus".to_owned()),
        out_bitrate_kbps: Some(128),
        start_seconds: 30.0,
    };
    let (bytes, content_type) = audio
        .transcode("song.mp3", &plan)
        .await
        .expect("transcode forwards");
    assert_eq!(content_type, "audio/mpeg");
    assert_eq!(bytes.len(), 100);
    let opens = engine.recorded();
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0].params.format.as_deref(), Some("opus"));
    assert_eq!(opens[0].params.max_bitrate_kbps, Some(128));
    assert_eq!(opens[0].params.start_seconds, 30.0);
    assert!(opens[0].params.force_transcode);
}

// --- Jellyfin adapter ---

#[tokio::test]
async fn gateway_stream_serves_identity_and_ranges() {
    let engine = StubEngine::new();
    let stream = stream(&engine);

    let outcome = stream.direct("song.mp3", None).await;
    assert_eq!(outcome.status, 200);
    assert_eq!(outcome.body.len(), 100);
    assert_eq!(
        header(&outcome.headers, "Content-Type").as_deref(),
        Some("audio/mpeg")
    );
    assert_eq!(
        header(&outcome.headers, "Accept-Ranges").as_deref(),
        Some("bytes")
    );

    let outcome = stream.direct("song.mp3", Some("bytes=10-19")).await;
    assert_eq!(outcome.status, 206);
    assert_eq!(outcome.body, (10..20u8).collect::<Vec<_>>());
    assert_eq!(
        header(&outcome.headers, "Content-Range").as_deref(),
        Some("bytes 10-19/100")
    );

    // Trailing whitespace seeks like the bare header (m8).
    let outcome = stream.direct("song.mp3", Some("bytes=0-1 ")).await;
    assert_eq!(outcome.status, 206);
    assert_eq!(outcome.body, vec![0, 1]);

    let outcome = stream.direct("song.mp3", Some("bytes=100-")).await;
    assert_eq!(outcome.status, 416);
    assert_eq!(
        header(&outcome.headers, "Content-Range").as_deref(),
        Some("bytes */100")
    );
    assert!(outcome.body.is_empty());
}

#[tokio::test]
async fn gateway_stream_head_mirrors_get_without_bodies() {
    let engine = StubEngine::new();
    let stream = stream(&engine);

    let outcome = stream.head("song.mp3", None).await;
    assert_eq!(outcome.status, 200);
    assert_eq!(
        header(&outcome.headers, "Content-Length").as_deref(),
        Some("100")
    );
    assert!(outcome.body.is_empty());

    let outcome = stream.head("song.mp3", Some("bytes=5-9")).await;
    assert_eq!(outcome.status, 206);
    assert_eq!(
        header(&outcome.headers, "Content-Range").as_deref(),
        Some("bytes 5-9/100")
    );
    assert!(outcome.body.is_empty());

    let outcome = stream.head("song.mp3", Some("bytes=100-")).await;
    assert_eq!(outcome.status, 416);
    assert!(outcome.body.is_empty());
}

#[tokio::test]
async fn gateway_stream_maps_faults_and_transcode_landings() {
    let engine = StubEngine::new();
    let stream = stream(&engine);

    // Unknown id: 404, empty.
    let outcome = stream.direct("missing.mp3", None).await;
    assert_eq!(outcome.status, 404);
    assert!(outcome.body.is_empty());
    let outcome = stream.head("missing.mp3", None).await;
    assert_eq!(outcome.status, 404);

    // Exhausted leases: 429 + Retry-After, empty.
    let outcome = stream.direct("busy.mp3", None).await;
    assert_eq!(outcome.status, 429);
    assert_eq!(
        header(&outcome.headers, "Retry-After").as_deref(),
        Some("1")
    );
    assert!(outcome.body.is_empty());

    // Transcode landings serve whole with the transcode header set and
    // never honor ranges.
    let outcome = stream.direct("landed.mp3", Some("bytes=0-1")).await;
    assert_eq!(outcome.status, 200);
    assert_eq!(
        header(&outcome.headers, "Accept-Ranges").as_deref(),
        Some("none")
    );
    assert_eq!(outcome.body, b"TRANSCODED");
    let outcome = stream.head("landed.mp3", None).await;
    assert_eq!(outcome.status, 200);
    assert!(outcome.body.is_empty());
}

#[tokio::test]
async fn gateway_stream_forwards_transcode_format_bitrate_and_offset() {
    // M1/M2 adapter half: codec, bitrate, seek offset, and forced
    // verdict all reach the engine open; the landing never carries a
    // Content-Length.
    let engine = StubEngine::new();
    let stream = stream(&engine);
    let outcome = stream.transcode("song.mp3", "opus", 96, 12.5).await;
    assert_eq!(outcome.status, 200);
    assert_eq!(
        header(&outcome.headers, "Accept-Ranges").as_deref(),
        Some("none")
    );
    assert_eq!(
        header(&outcome.headers, "Cache-Control").as_deref(),
        Some("no-store")
    );
    assert!(header(&outcome.headers, "Content-Length").is_none());
    let opens = engine.recorded();
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0].params.format.as_deref(), Some("opus"));
    assert_eq!(opens[0].params.max_bitrate_kbps, Some(96));
    assert_eq!(opens[0].params.start_seconds, 12.5);
    assert!(opens[0].params.force_transcode);
}
