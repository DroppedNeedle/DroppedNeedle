//! Gateway tests: range matrix, HEAD, content types, identity
//! encoding, and lease exhaustion over a scripted fake engine.
//!
//! No live servers: local fixtures are deterministic byte vectors and remote
//! bodies are scripted per key. The fake resolves local content types through
//! the real [`gateway::content_type_for_extension`] helper, so the table is
//! exercised on the request path, not copied.

use droppedneedle_media::stream::routes as gateway;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use droppedneedle_auth::users::roles::{Role, SessionKind};
use gateway::{
    AudioSource, MediaBody, StreamEngine, StreamFault, StreamMedia, StreamOpen, StreamState,
    content_type_for_extension, stream_routes,
};
use serde_json::Value;
use tower::ServiceExt as _;

use crate::common::auth::{test_principal, with_test_principal};

/// What the fake engine observed on one open (principal + hints).
#[derive(Debug, Clone)]
struct Seen {
    user_id: String,
    source: AudioSource,
    key: String,
    format: Option<String>,
    max_bitrate_kbps: Option<i64>,
    estimate_content_length: bool,
}

/// One scripted object, served from memory.
#[derive(Debug, Clone)]
struct Canned {
    content_type: String,
    transcoded: bool,
    estimated_len: Option<u64>,
    bytes: Vec<u8>,
}

impl Canned {
    fn direct(content_type: &str, bytes: Vec<u8>) -> Self {
        Self {
            content_type: content_type.to_owned(),
            transcoded: false,
            estimated_len: None,
            bytes,
        }
    }

    fn media(self) -> StreamMedia {
        StreamMedia {
            content_type: self.content_type,
            total_len: self.bytes.len() as u64,
            transcoded: self.transcoded,
            estimated_len: self.estimated_len,
            body: MediaBody::Bytes(self.bytes),
            lease: None,
        }
    }
}

/// Scripted per-key behavior. `LocalFile` resolves its content type through
/// the real gateway helper, returning InvalidInput for unstreamable formats.
#[derive(Debug, Clone)]
enum Script {
    LocalFile { ext: String, len: usize },
    Media(Canned),
    Fault(StreamFault),
}

/// Scripted engine: no disk, no network, no ffmpeg.
struct FakeEngine {
    scripts: HashMap<String, Script>,
    seen: Mutex<Vec<Seen>>,
}

impl FakeEngine {
    fn with(scripts: Vec<(&str, Script)>) -> Arc<Self> {
        Arc::new(Self {
            scripts: scripts
                .into_iter()
                .map(|(key, script)| (key.to_owned(), script))
                .collect(),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn observed(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen unlocks").clone()
    }
}

impl StreamEngine for FakeEngine {
    async fn open_stream(
        &self,
        request: StreamOpen,
        _head_only: bool,
    ) -> Result<StreamMedia, StreamFault> {
        self.seen.lock().expect("seen unlocks").push(Seen {
            user_id: request.user_id.clone(),
            source: request.source,
            key: request.key.clone(),
            format: request.params.format.clone(),
            max_bitrate_kbps: request.params.max_bitrate_kbps,
            estimate_content_length: request.params.estimate_content_length,
        });
        let lookup = format!("{}/{}", request.source.as_str(), request.key);
        match self.scripts.get(&lookup) {
            None => Err(StreamFault::NotFound),
            Some(Script::Media(media)) => Ok(media.clone().media()),
            Some(Script::Fault(fault)) => Err(fault.clone()),
            Some(Script::LocalFile { ext, len }) => match content_type_for_extension(ext) {
                Some(content_type) => Ok(Canned::direct(content_type, fixture_bytes(*len)).media()),
                None => Err(StreamFault::InvalidInput {
                    message: format!("Unsupported audio format: {ext}"),
                }),
            },
        }
    }
}

/// Deterministic fixture bytes (prime stride avoids alignment illusions).
fn fixture_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Router signed in as `user-1`, or anonymous when false.
fn app(engine: Arc<FakeEngine>, authed: bool) -> Router {
    let state = StreamState { engine };
    let router = stream_routes(state);
    if !authed {
        return router;
    }
    with_test_principal(
        router,
        test_principal("user-1", Role::User, "sess-1", SessionKind::Standard),
    )
}

async fn call(
    router: Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .oneshot(builder.body(Body::empty()).expect("request builds"))
        .await
        .expect("router answers");
    let status = response.status();
    let header_map = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads")
        .to_vec();
    (status, header_map, body)
}

async fn get(
    router: Router,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    call(router, Method::GET, uri, headers).await
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn envelope(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("error body is json")
}

fn local_app(ext: &str, len: usize) -> (Arc<FakeEngine>, Router) {
    let engine = FakeEngine::with(vec![(
        "local/song",
        Script::LocalFile {
            ext: ext.to_owned(),
            len,
        },
    )]);
    let router = app(Arc::clone(&engine), true);
    (engine, router)
}

// ---------------------------------------------------------------------------
// Full reads and identity encoding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_get_returns_exact_bytes_with_identity_headers() {
    let (_engine, router) = local_app(".flac", 256);
    let (status, headers, body) = get(router, "/stream/local/song", &[]).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, fixture_bytes(256));
    assert_eq!(
        header(&headers, "content-type"),
        Some("audio/flac".to_owned())
    );
    assert_eq!(header(&headers, "content-length"), Some("256".to_owned()));
    assert_eq!(header(&headers, "accept-ranges"), Some("bytes".to_owned()));
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
    );
    assert_eq!(header(&headers, "content-range"), None);
}

// ---------------------------------------------------------------------------
// Range matrix: suffix/open/closed land 206; multi-range answers 416
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ranges_return_exact_slices() {
    for (range, from, to) in [
        ("bytes=0-9", 0, 10),
        ("bytes=0-0", 0, 1),
        ("bytes=200-", 200, 256),
        ("bytes=-16", 240, 256),
        ("bytes=-9999", 0, 256),
        ("bytes=250-9999", 250, 256),
    ] {
        let (_engine, router) = local_app(".mp3", 256);
        let (status, headers, body) = get(router, "/stream/local/song", &[("range", range)]).await;
        assert_eq!(status, StatusCode::PARTIAL_CONTENT, "{range}");
        assert_eq!(body, fixture_bytes(256)[from..to], "{range}");
        assert_eq!(
            header(&headers, "content-range"),
            Some(format!("bytes {from}-{}/256", to - 1)),
            "{range}"
        );
        assert_eq!(
            header(&headers, "content-length"),
            Some((to - from).to_string())
        );
    }
}

#[tokio::test]
async fn multi_range_answers_416_like_every_stream_route() {
    let (_engine, router) = local_app(".mp3", 256);
    let (status, headers, _) =
        get(router, "/stream/local/song", &[("range", "bytes=0-1,5-6")]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes */256".to_owned())
    );
}

// ---------------------------------------------------------------------------
// Range matrix: the 416 set
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unsatisfiable_ranges_answer_416_with_sized_content_range() {
    for range in [
        "bytes=256-",
        "bytes=999-",
        "bytes=50-40",
        "bytes=-0",
        "bytes=-",
        "bytes=",
        "bytes=abc",
        "items=0-9",
    ] {
        let (_engine, router) = local_app(".flac", 256);
        let (status, headers, body) = get(router, "/stream/local/song", &[("range", range)]).await;

        assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(
            header(&headers, "content-range"),
            Some("bytes */256".to_owned()),
            "{range}"
        );
        assert_eq!(
            envelope(&body)["error"]["code"],
            "RANGE_NOT_SATISFIABLE",
            "{range}"
        );
    }
}

// ---------------------------------------------------------------------------
// HEAD mirrors GET headers with no body
// ---------------------------------------------------------------------------

#[tokio::test]
async fn head_full_returns_get_headers_without_body() {
    let (_engine, router) = local_app(".aac", 256);
    let (status, headers, body) = call(router, Method::HEAD, "/stream/local/song", &[]).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert_eq!(
        header(&headers, "content-type"),
        Some("audio/aac".to_owned())
    );
    assert_eq!(header(&headers, "content-length"), Some("256".to_owned()));
    assert_eq!(header(&headers, "accept-ranges"), Some("bytes".to_owned()));
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
    );
}

#[tokio::test]
async fn head_range_returns_206_headers_without_body() {
    let (_engine, router) = local_app(".flac", 256);
    let (status, headers, body) = call(
        router,
        Method::HEAD,
        "/stream/local/song",
        &[("range", "bytes=-16")],
    )
    .await;

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert!(body.is_empty());
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes 240-255/256".to_owned())
    );
    assert_eq!(header(&headers, "content-length"), Some("16".to_owned()));
}

// ---------------------------------------------------------------------------
// Content types and the WMA cut
// ---------------------------------------------------------------------------

#[tokio::test]
async fn content_type_follows_extension_table() {
    for (ext, media_type) in [
        (".flac", "audio/flac"),
        (".mp3", "audio/mpeg"),
        (".ogg", "audio/ogg"),
        (".m4a", "audio/mp4"),
        (".aac", "audio/aac"),
        (".wav", "audio/wav"),
        (".opus", "audio/opus"),
    ] {
        let (_engine, router) = local_app(ext, 64);
        let (status, headers, _body) = get(router, "/stream/local/song", &[]).await;

        assert_eq!(status, StatusCode::OK, "{ext}");
        assert_eq!(
            header(&headers, "content-type"),
            Some(media_type.to_owned()),
            "{ext}"
        );
    }
}

#[tokio::test]
async fn scripted_remote_body_keeps_upstream_content_type() {
    let bytes = fixture_bytes(128);
    let engine = FakeEngine::with(vec![(
        "plex/part-7",
        Script::Media(Canned::direct("audio/x-flac", bytes.clone())),
    )]);
    let (status, headers, body) = get(app(engine, true), "/stream/plex/part-7", &[]).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(
        header(&headers, "content-type"),
        Some("audio/x-flac".to_owned())
    );
}

// ---------------------------------------------------------------------------
// Query parsing and engine hints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malformed_query_renders_envelope_not_plaintext() {
    let (_engine, router) = local_app(".mp3", 64);
    let (status, headers, body) = get(router, "/stream/local/song?max_bitrate=abc", &[]).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/json".to_owned())
    );
    assert_eq!(envelope(&body)["error"]["code"], "INVALID_INPUT");
}

#[tokio::test]
async fn query_hints_and_principal_reach_engine() {
    let (engine, router) = local_app(".mp3", 64);
    let (status, _, _) = get(
        router,
        "/stream/local/song?format=opus&max_bitrate=128&estimate_content_length=true",
        &[],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let seen = engine.observed();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].user_id, "user-1");
    assert_eq!(seen[0].source, AudioSource::Local);
    assert_eq!(seen[0].key, "song");
    assert_eq!(seen[0].format, Some("opus".to_owned()));
    assert_eq!(seen[0].max_bitrate_kbps, Some(128));
    assert!(seen[0].estimate_content_length);
}

// ---------------------------------------------------------------------------
// Fault mapping: 404/403/429/502
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_ids_404_with_source_detail() {
    let engine = FakeEngine::with(vec![]);
    let (status, _, body) = get(app(Arc::clone(&engine), true), "/stream/local/gone", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let json = envelope(&body);
    assert_eq!(json["error"]["code"], "STREAM_NOT_FOUND");
    assert_eq!(json["error"]["message"], "Track file not found");

    let (status, _, body) = get(app(engine, true), "/stream/jellyfin/gone", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let json = envelope(&body);
    assert_eq!(json["error"]["code"], "STREAM_NOT_FOUND");
    assert_eq!(json["error"]["message"], "Audio item not found");
}

#[tokio::test]
async fn forbidden_passes_safe_message_through() {
    let engine = FakeEngine::with(vec![
        (
            "local/nope",
            Script::Fault(StreamFault::Forbidden {
                message: "Playback not allowed".to_owned(),
            }),
        ),
        (
            "local/escape",
            Script::Fault(StreamFault::Forbidden {
                message: "Access denied: path is outside the music directory".to_owned(),
            }),
        ),
    ]);
    for (key, message) in [
        ("nope", "Playback not allowed"),
        (
            "escape",
            "Access denied: path is outside the music directory",
        ),
    ] {
        let (status, _, body) = get(
            app(Arc::clone(&engine), true),
            &format!("/stream/local/{key}"),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{key}");
        let json = envelope(&body);
        assert_eq!(json["error"]["code"], "STREAM_FORBIDDEN", "{key}");
        assert_eq!(json["error"]["message"], message, "{key}");
    }
}

#[tokio::test]
async fn lease_exhaustion_answers_429_with_retry_after() {
    let engine = FakeEngine::with(vec![(
        "navidrome/busy",
        Script::Fault(StreamFault::Capacity),
    )]);
    let (status, headers, body) = get(app(engine, true), "/stream/navidrome/busy", &[]).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&headers, "retry-after"), Some("1".to_owned()));
    let json = envelope(&body);
    assert_eq!(json["error"]["code"], "STREAM_CAPACITY_EXHAUSTED");
    assert_eq!(json["error"]["details"]["retry_after"], 1);
}

/// A failed read is a 502: the closed-list `REMOTE_UNAVAILABLE` for a media
/// server, a plain fault for a local file or a plugin. The request boundary
/// writes the bodies (see `media_journey`).
#[tokio::test]
async fn upstream_failure_answers_502() {
    let upstream = |source| {
        Script::Fault(StreamFault::Upstream {
            source,
            cause: "scripted".to_owned(),
        })
    };
    let engine = FakeEngine::with(vec![
        ("plex/part-9", upstream(AudioSource::Plex)),
        ("local/song", upstream(AudioSource::Local)),
        ("plugin/mbid", upstream(AudioSource::Plugin)),
    ]);
    for (uri, code) in [
        ("/stream/plex/part-9", "REMOTE_UNAVAILABLE"),
        ("/stream/local/song", "INTERNAL_ERROR"),
        ("/stream/plugin/mbid", "INTERNAL_ERROR"),
    ] {
        let (status, _, body) = get(app(Arc::clone(&engine), true), uri, &[]).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{uri}");
        assert_eq!(envelope(&body)["error"]["code"], code, "{uri}");
    }
}

// ---------------------------------------------------------------------------
// Transcode landing: no ranges, no cache, identity, optional estimate
// ---------------------------------------------------------------------------

fn transcode_engine() -> Arc<FakeEngine> {
    let bytes = b"fake-transcoded-mp3-bytes".to_vec();
    FakeEngine::with(vec![(
        "local/song",
        Script::Media(Canned {
            content_type: "audio/mpeg".to_owned(),
            transcoded: true,
            estimated_len: Some(48_000),
            bytes,
        }),
    )])
}

#[tokio::test]
async fn transcode_ignores_range_with_v2_headers() {
    let (status, headers, body) = get(
        app(transcode_engine(), true),
        "/stream/local/song",
        &[("range", "bytes=0-4")],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"fake-transcoded-mp3-bytes");
    assert_eq!(header(&headers, "accept-ranges"), Some("none".to_owned()));
    assert_eq!(
        header(&headers, "cache-control"),
        Some("no-store".to_owned())
    );
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
    );
    assert_eq!(header(&headers, "content-range"), None);
    // No estimate was asked, so routes set no length; the "25" below is the
    // router's automatic stamp for the buffered fake body (production
    // transcode streams have unknown size and stay lengthless).
    assert_eq!(header(&headers, "content-length"), Some("25".to_owned()));
}

#[tokio::test]
async fn transcode_estimate_header_only_when_asked() {
    let (status, headers, _) = get(
        app(transcode_engine(), true),
        "/stream/local/song?estimate_content_length=true",
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-length"), Some("48000".to_owned()));

    let (status, headers, _) = get(app(transcode_engine(), true), "/stream/local/song", &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-length"), Some("25".to_owned()));
}

#[tokio::test]
async fn head_transcode_without_estimate_reports_actual_length() {
    let (status, headers, body) = call(
        app(transcode_engine(), true),
        Method::HEAD,
        "/stream/local/song",
        &[],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    assert_eq!(header(&headers, "content-length"), Some("25".to_owned()));
    assert_eq!(header(&headers, "accept-ranges"), Some("none".to_owned()));
}

// ---------------------------------------------------------------------------
// Auth and routing shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn anonymous_stream_is_401_with_challenge() {
    let engine = FakeEngine::with(vec![(
        "local/song",
        Script::LocalFile {
            ext: ".mp3".to_owned(),
            len: 64,
        },
    )]);
    let (status, headers, body) = get(app(engine, false), "/stream/local/song", &[]).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        header(&headers, "www-authenticate"),
        Some("Bearer".to_owned())
    );
    assert_eq!(envelope(&body)["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn plex_part_key_with_slashes_routes_whole_key() {
    let bytes = fixture_bytes(48);
    let engine = FakeEngine::with(vec![(
        "plex/library/parts/7/file.mp3",
        Script::Media(Canned::direct("audio/mpeg", bytes.clone())),
    )]);
    let (status, _, body) = get(
        app(Arc::clone(&engine), true),
        "/stream/plex/library/parts/7/file.mp3",
        &[],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    let seen = engine.observed();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].source, AudioSource::Plex);
    assert_eq!(seen[0].key, "library/parts/7/file.mp3");
}
