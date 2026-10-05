//! The production `GatewayAudio` (Subsonic) and `GatewayStream` (Jellyfin)
//! adapters over a scripted stream engine. The journeys use protocol
//! fixtures, so these are what pin the adapters: one engine open per
//! request, Range and HEAD handling, fault mapping, and that the transcode
//! codec, bitrate and seek offset reach the engine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use droppedneedle::compat::adapters::engines::{GatewayAudio, GatewayStream};
use droppedneedle::compat::jellyfin::seams::StreamEngine as JellyfinEngine;
use droppedneedle::compat::subsonic::stream::{
    AudioBackend, ServeError, StreamPlan, serve_original,
};
use droppedneedle::stream::routes::{OpenMedia, StreamEngine, StreamFault, StreamOpen};

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

#[tokio::test]
async fn subsonic_adapter_serves_ranges_and_forwards_transcodes() {
    let engine = StubEngine::new();
    let audio = audio(&engine);
    for (range, head, status, content_range, body) in [
        (None, false, 200, None, (0..100u8).collect::<Vec<_>>()),
        (
            Some("bytes=10-19"),
            false,
            206,
            Some("bytes 10-19/100"),
            (10..20u8).collect(),
        ),
        // Every parser trims, so trailing whitespace still seeks.
        (
            Some("bytes=0-1 "),
            false,
            206,
            Some("bytes 0-1/100"),
            vec![0, 1],
        ),
        (None, true, 200, None, vec![]),
        (Some("bytes=5-9"), true, 206, Some("bytes 5-9/100"), vec![]),
    ] {
        let served = serve_original(&audio, "song.mp3", range, head, None)
            .await
            .expect("serves");
        let case = format!("{range:?} head={head}");
        assert_eq!(served.status, status, "{case}");
        assert_eq!(served.content_type, "audio/mpeg", "{case}");
        assert_eq!(
            header(&served.headers, "Content-Range").as_deref(),
            content_range,
            "{case}"
        );
        assert_eq!(served.body, body, "{case}");
    }
    assert_eq!(engine.recorded().len(), 5, "one engine open per request");
    let err = serve_original(&audio, "song.mp3", Some("bytes=100-"), false, None)
        .await
        .expect_err("past the end");
    assert!(matches!(err, ServeError::RangeUnsatisfiable(100)));

    let plan = StreamPlan {
        transcode: true,
        out_format: Some("opus".to_owned()),
        out_bitrate_kbps: Some(128),
        start_seconds: 30.0,
    };
    audio
        .transcode("song.mp3", &plan)
        .await
        .expect("transcodes");
    let params = &engine.recorded()[6].params;
    assert_eq!(params.format.as_deref(), Some("opus"));
    assert_eq!(params.max_bitrate_kbps, Some(128));
    assert_eq!(params.start_seconds, 30.0);
    assert!(params.force_transcode);
}

#[tokio::test]
async fn jellyfin_adapter_serves_ranges_maps_faults_and_forwards_transcodes() {
    let engine = StubEngine::new();
    let stream = stream(&engine);
    for (key, range, status, content_range, accept_ranges, body) in [
        (
            "song.mp3",
            None,
            200,
            None,
            Some("bytes"),
            (0..100u8).collect::<Vec<_>>(),
        ),
        (
            "song.mp3",
            Some("bytes=10-19"),
            206,
            Some("bytes 10-19/100"),
            Some("bytes"),
            (10..20u8).collect(),
        ),
        (
            "song.mp3",
            Some("bytes=0-1 "),
            206,
            Some("bytes 0-1/100"),
            Some("bytes"),
            vec![0, 1],
        ),
        (
            "song.mp3",
            Some("bytes=100-"),
            416,
            Some("bytes */100"),
            None,
            vec![],
        ),
        // Transcode landings serve whole and never honor ranges.
        (
            "landed.mp3",
            Some("bytes=0-1"),
            200,
            None,
            Some("none"),
            b"TRANSCODED".to_vec(),
        ),
        ("missing.mp3", None, 404, None, None, vec![]),
    ] {
        let case = format!("{key} {range:?}");
        let got = stream.direct(key, range).await;
        assert_eq!(got.status, status, "{case}");
        assert_eq!(
            header(&got.headers, "Content-Range").as_deref(),
            content_range,
            "{case}"
        );
        if accept_ranges.is_some() {
            assert_eq!(
                header(&got.headers, "Accept-Ranges").as_deref(),
                accept_ranges,
                "{case}"
            );
        }
        assert_eq!(got.body, body, "{case}");
        // HEAD mirrors GET without a body.
        let head = stream.head(key, range).await;
        assert_eq!(head.status, status, "HEAD {case}");
        assert!(head.body.is_empty(), "HEAD {case}");
    }
    let full = stream.head("song.mp3", None).await;
    assert_eq!(
        header(&full.headers, "Content-Length").as_deref(),
        Some("100")
    );

    // Exhausted stream leases: 429 with Retry-After.
    let busy = stream.direct("busy.mp3", None).await;
    assert_eq!(busy.status, 429);
    assert_eq!(header(&busy.headers, "Retry-After").as_deref(), Some("1"));

    // Transcodes forward codec, bitrate and offset, and carry no length.
    let before = engine.recorded().len();
    let landed = stream.transcode("song.mp3", "opus", 96, 12.5).await;
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
