//! Stage-6 gateway briefs: range matrix, HEAD, content types, identity
//! encoding, and lease exhaustion over a scripted fake engine.
//!
//! No live servers: local fixtures are deterministic byte vectors and remote
//! bodies are scripted per key. The fake resolves local content types through
//! the real [`gateway::content_type_for_extension`] helper, so the table is
//! exercised on the request path, not copied.

#[path = "../src/stream/routes.rs"]
mod gateway;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::roles::SessionKind;
use gateway::{
    AudioSource, OpenMedia, StreamEngine, StreamFault, StreamOpen, StreamState,
    content_type_for_extension, stream_routes,
};
use serde_json::Value;
use tower::ServiceExt as _;

/// Fixed error id asserted in 5xx envelopes. A valid UUID.
const FIXED_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

/// Id generator returning one fixed value.
#[derive(Debug, Clone)]
struct FixedIds;

impl droppedneedle::ids::IdGenerator for FixedIds {
    fn new_id(&self) -> String {
        FIXED_ID.to_owned()
    }
}

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

/// Scripted per-key behavior. `LocalFile` resolves its content type through
/// the real gateway helper, returning InvalidInput for unstreamable formats.
#[derive(Debug, Clone)]
enum Script {
    LocalFile { ext: String, len: usize },
    Media(OpenMedia),
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
    async fn open(&self, request: StreamOpen) -> Result<OpenMedia, StreamFault> {
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
            Some(Script::Media(media)) => Ok(media.clone()),
            Some(Script::Fault(fault)) => Err(fault.clone()),
            Some(Script::LocalFile { ext, len }) => match content_type_for_extension(ext) {
                Some(content_type) => {
                    let bytes = fixture_bytes(*len);
                    Ok(OpenMedia {
                        content_type: content_type.to_owned(),
                        total_len: bytes.len() as u64,
                        transcoded: false,
                        estimated_len: None,
                        bytes,
                    })
                }
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

/// Router with a stashed session for `user-1`, or anonymous when false.
fn app(engine: Arc<FakeEngine>, authed: bool) -> Router {
    let state = StreamState {
        engine,
        ids: Arc::new(FixedIds),
    };
    let router = stream_routes(state);
    if !authed {
        return router;
    }
    router.layer(axum::middleware::from_fn(
        |mut req: Request<Body>, next: axum::middleware::Next| async move {
            req.extensions_mut().insert(CurrentSession {
                user_id: "user-1".to_owned(),
                session_id: "sess-1".to_owned(),
                kind: SessionKind::Standard,
                transport: Transport::Bearer,
            });
            next.run(req).await
        },
    ))
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
// Range matrix: suffix/open/closed land 206
// ---------------------------------------------------------------------------

#[tokio::test]
async fn range_closed_returns_exact_slice() {
    let (_engine, router) = local_app(".mp3", 256);
    let (status, headers, body) =
        get(router, "/stream/local/song", &[("range", "bytes=0-9")]).await;

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, fixture_bytes(256)[0..10]);
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes 0-9/256".to_owned())
    );
    assert_eq!(header(&headers, "content-length"), Some("10".to_owned()));
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
    );
}

#[tokio::test]
async fn range_single_byte_returns_one_byte() {
    let (_engine, router) = local_app(".mp3", 256);
    let (status, headers, body) =
        get(router, "/stream/local/song", &[("range", "bytes=0-0")]).await;

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, fixture_bytes(256)[0..1]);
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes 0-0/256".to_owned())
    );
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
    );
}

#[tokio::test]
async fn range_open_runs_to_end_of_object() {
    let (_engine, router) = local_app(".ogg", 256);
    let (status, headers, body) =
        get(router, "/stream/local/song", &[("range", "bytes=200-")]).await;

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, fixture_bytes(256)[200..256]);
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes 200-255/256".to_owned())
    );
    assert_eq!(header(&headers, "content-length"), Some("56".to_owned()));
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
    );
}

#[tokio::test]
async fn range_suffix_returns_tail_bytes() {
    let (_engine, router) = local_app(".opus", 256);
    let (status, headers, body) =
        get(router, "/stream/local/song", &[("range", "bytes=-16")]).await;

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, fixture_bytes(256)[240..256]);
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes 240-255/256".to_owned())
    );
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
    );
}

#[tokio::test]
async fn range_suffix_longer_than_file_serves_whole_object() {
    let (_engine, router) = local_app(".m4a", 256);
    let (status, headers, body) =
        get(router, "/stream/local/song", &[("range", "bytes=-9999")]).await;

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, fixture_bytes(256));
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes 0-255/256".to_owned())
    );
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
    );
}

#[tokio::test]
async fn range_end_past_file_clamps_to_last_byte() {
    let (_engine, router) = local_app(".wav", 256);
    let (status, headers, body) =
        get(router, "/stream/local/song", &[("range", "bytes=250-9999")]).await;

    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, fixture_bytes(256)[250..256]);
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes 250-255/256".to_owned())
    );
    assert_eq!(
        header(&headers, "content-encoding"),
        Some("identity".to_owned())
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
        "bytes=0-1,2-3",
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

#[tokio::test]
async fn head_unsatisfiable_answers_416() {
    let (_engine, router) = local_app(".flac", 256);
    let (status, headers, body) = call(
        router,
        Method::HEAD,
        "/stream/local/song",
        &[("range", "bytes=999-")],
    )
    .await;

    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert!(body.is_empty());
    assert_eq!(
        header(&headers, "content-range"),
        Some("bytes */256".to_owned())
    );
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
async fn wma_never_streams() {
    let (_engine, router) = local_app(".wma", 64);
    let (status, _headers, body) = get(router, "/stream/local/song", &[]).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(envelope(&body)["error"]["code"], "INVALID_INPUT");
}

#[tokio::test]
async fn scripted_remote_body_keeps_upstream_content_type() {
    let bytes = fixture_bytes(128);
    let engine = FakeEngine::with(vec![(
        "plex/part-7",
        Script::Media(OpenMedia {
            content_type: "audio/x-flac".to_owned(),
            total_len: bytes.len() as u64,
            transcoded: false,
            estimated_len: None,
            bytes: bytes.clone(),
        }),
    )]);
    let (status, headers, body) = get(app(engine, true), "/stream/plex/part-7", &[]).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(
        header(&headers, "content-type"),
        Some("audio/x-flac".to_owned())
    );
}

#[tokio::test]
async fn unparseable_direct_content_type_falls_back_to_mp3() {
    let bytes = fixture_bytes(64);
    let engine = FakeEngine::with(vec![(
        "plex/part-7",
        Script::Media(OpenMedia {
            content_type: "audio/mpeg\ninjected: yes".to_owned(),
            total_len: bytes.len() as u64,
            transcoded: false,
            estimated_len: None,
            bytes: bytes.clone(),
        }),
    )]);
    let (status, headers, body) = get(app(engine, true), "/stream/plex/part-7", &[]).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(
        header(&headers, "content-type"),
        Some("audio/mpeg".to_owned())
    );
}

#[tokio::test]
async fn unparseable_transcode_content_type_falls_back_to_mp3() {
    let bytes = b"fake-transcoded-bytes".to_vec();
    let engine = FakeEngine::with(vec![(
        "local/song",
        Script::Media(OpenMedia {
            content_type: "not a\ttype\nat all".to_owned(),
            total_len: bytes.len() as u64,
            transcoded: true,
            estimated_len: None,
            bytes: bytes.clone(),
        }),
    )]);
    let (status, headers, body) = get(app(engine, true), "/stream/local/song", &[]).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, bytes);
    assert_eq!(
        header(&headers, "content-type"),
        Some("audio/mpeg".to_owned())
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

#[tokio::test]
async fn unknown_source_is_invalid_input() {
    let (_engine, router) = local_app(".mp3", 64);
    let (status, _headers, body) = get(router, "/stream/ftp/song", &[]).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let json = envelope(&body);
    assert_eq!(json["error"]["code"], "INVALID_INPUT");
    assert_eq!(json["error"]["message"], "Unknown stream source");
}

// ---------------------------------------------------------------------------
// Fault mapping: 404/403/429/502/500
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

#[tokio::test]
async fn upstream_failure_answers_502_naming_source() {
    let engine = FakeEngine::with(vec![(
        "plex/part-9",
        Script::Fault(StreamFault::Upstream {
            source: AudioSource::Plex,
        }),
    )]);
    let (status, _, body) = get(app(engine, true), "/stream/plex/part-9", &[]).await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let json = envelope(&body);
    assert_eq!(json["error"]["code"], "UPSTREAM_UNAVAILABLE");
    assert_eq!(json["error"]["message"], "Failed to stream from Plex");
}

#[tokio::test]
async fn internal_fault_is_fixed_envelope_with_error_id() {
    let engine = FakeEngine::with(vec![(
        "local/broken",
        Script::Fault(StreamFault::Internal {
            cause: "disk exploded at /secret/path".to_owned(),
        }),
    )]);
    let (status, _, body) = get(app(engine, true), "/stream/local/broken", &[]).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let text = String::from_utf8(body).expect("utf8 envelope");
    assert!(!text.contains("exploded"), "{text}");
    assert!(!text.contains("/secret/path"), "{text}");
    let json: Value = serde_json::from_str(&text).expect("json envelope");
    assert_eq!(json["error"]["code"], "INTERNAL_ERROR");
    assert_eq!(json["error"]["message"], "Internal server error");
    assert_eq!(json["error"]["details"]["error_id"], FIXED_ID);
}

// ---------------------------------------------------------------------------
// Transcode landing: no ranges, no cache, identity, optional estimate
// ---------------------------------------------------------------------------

fn transcode_engine() -> Arc<FakeEngine> {
    let bytes = b"fake-transcoded-mp3-bytes".to_vec();
    FakeEngine::with(vec![(
        "local/song",
        Script::Media(OpenMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len: bytes.len() as u64,
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
async fn anonymous_head_is_401_with_challenge() {
    let engine = FakeEngine::with(vec![(
        "local/song",
        Script::LocalFile {
            ext: ".mp3".to_owned(),
            len: 64,
        },
    )]);
    let (status, headers, body) =
        call(app(engine, false), Method::HEAD, "/stream/local/song", &[]).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.is_empty(), "HEAD answers carry no body");
    assert_eq!(
        header(&headers, "www-authenticate"),
        Some("Bearer".to_owned())
    );
}

#[tokio::test]
async fn plex_part_key_with_slashes_routes_whole_key() {
    let bytes = fixture_bytes(48);
    let engine = FakeEngine::with(vec![(
        "plex/library/parts/7/file.mp3",
        Script::Media(OpenMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len: bytes.len() as u64,
            transcoded: false,
            estimated_len: None,
            bytes: bytes.clone(),
        }),
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

#[tokio::test]
async fn error_codes_are_screaming_snake() {
    let engine = FakeEngine::with(vec![
        ("local/busy", Script::Fault(StreamFault::Capacity)),
        (
            "local/broken",
            Script::Fault(StreamFault::Internal {
                cause: "boom".to_owned(),
            }),
        ),
    ]);
    let mut codes = Vec::new();
    for uri in [
        "/stream/local/song?max_bitrate=zz",
        "/stream/ftp/song",
        "/stream/local/missing",
        "/stream/local/busy",
        "/stream/local/broken",
    ] {
        let (_, _, body) = get(app(Arc::clone(&engine), true), uri, &[]).await;
        codes.push(envelope(&body)["error"]["code"].clone());
    }
    let (_, _, body) = get(app(Arc::clone(&engine), false), "/stream/local/song", &[]).await;
    codes.push(envelope(&body)["error"]["code"].clone());

    assert_eq!(codes.len(), 6);
    for code in &codes {
        let code = code.as_str().expect("string code");
        assert!(!code.is_empty());
        assert_eq!(code, code.to_uppercase());
        assert!(
            code.chars()
                .all(|char| char.is_ascii_uppercase() || char == '_'),
            "{code}"
        );
    }
}
