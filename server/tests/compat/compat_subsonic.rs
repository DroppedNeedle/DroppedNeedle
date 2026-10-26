//! Subsonic answers with transcoding on, and the binary endpoints'
//! transcode refusal, over the in-memory `fake` store and audio backend
//! with its fixed clock. The rest of the protocol runs on the real library
//! in `compat_subsonic_library`, whose app turns transcoding off: a server
//! transcodes only with ffmpeg on its PATH. These stay until the test app
//! can stand in for ffmpeg.

use std::collections::HashMap;

use droppedneedle_compat::subsonic::fake::{FakeAudio, FakeStore, FakeVerifier, NOW_UNIX};
use droppedneedle_compat::subsonic::params::SubsonicParameters;
use droppedneedle_compat::subsonic::stream::{
    AudioBackend, BackendError, PlayedAudio, StreamParams,
};
use droppedneedle_compat::subsonic::value::Rendered;
use droppedneedle_compat::subsonic::{Request, Settings, dispatch, dispatch_uses_envelope};

fn settings() -> Settings {
    Settings {
        enabled: true,
        server_version: "3.0.0-test".to_owned(),
        ..Settings::default()
    }
}

fn transcoding() -> Settings {
    Settings {
        transcoding_enabled: true,
        ffmpeg_available: true,
        ..settings()
    }
}

const AUTH: [(&str, &str); 4] = [
    ("u", "user"),
    ("p", "secret"),
    ("v", "1.16.1"),
    ("c", "golden"),
];

/// A request for `endpoint`; `authed` prepends the fixture credentials.
fn request(endpoint: &str, pairs: &[(&str, &str)], authed: bool) -> Request {
    let auth: &[(&str, &str)] = if authed { &AUTH } else { &[] };
    let params = auth
        .iter()
        .chain(pairs)
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    Request {
        method: "GET".to_owned(),
        endpoint: endpoint.to_owned(),
        params: SubsonicParameters::new(params),
        headers: HashMap::new(),
        body: Vec::new(),
        content_type: None,
        now_unix: NOW_UNIX,
        picture_base: None,
    }
}

async fn run(settings: &Settings, store: &FakeStore, req: &Request) -> Rendered {
    dispatch(&FakeVerifier, store, &FakeAudio, settings, req).await
}

async fn get(endpoint: &str, pairs: &[(&str, &str)]) -> Rendered {
    run(
        &settings(),
        &FakeStore::loaded(),
        &request(endpoint, pairs, true),
    )
    .await
}

#[tokio::test]
async fn binary_endpoints_render_errors_as_text() {
    let body = get("stream", &[("id", "tr-f1")]).await;
    assert_eq!(body.header("Content-Encoding"), Some("identity"));
    assert_eq!(body.body, (0..100u8).collect::<Vec<_>>());

    let body = get("download", &[("id", "tr-f1")]).await;
    assert_eq!(
        body.header("Content-Disposition"),
        Some("attachment; filename=\"Song One.mp3\"")
    );
    use droppedneedle_compat::subsonic::stream::download_filename;
    assert_eq!(download_filename("a/b:c*d", "MP3"), "a_b_c_d.mp3");
    assert_eq!(download_filename("...", "flac"), "track.flac");

    let body = get("getAvatar", &[("username", "user")]).await;
    assert_eq!(
        (body.content_type.as_str(), body.body.as_slice()),
        ("image/png", &b"AVATAR-BYTES"[..])
    );
    let body = get("getAvatar", &[("username", "someone-else")]).await;
    assert_eq!(
        (body.status, body.content_type.as_str()),
        (403, "text/plain")
    );
    assert_eq!(
        body.body_text(),
        "Avatar access is limited to the authenticated user"
    );

    let disabled = [
        ("mediaId", "tr-f1"),
        ("mediaType", "song"),
        ("transcodeParams", "signed-params"),
    ];
    let body = get("getTranscodeStream", &disabled).await;
    assert_eq!(
        (body.status, body.content_type.as_str()),
        (404, "text/plain")
    );
    assert!(body.body_text().contains("Transcoding is disabled"));

    // Auth failures stay enveloped on binary endpoints. A stream miss is
    // enveloped too; other not-found answers stay text.
    assert!(dispatch_uses_envelope(50, "stream"));
    assert!(dispatch_uses_envelope(40, "getavatar"));
    assert!(dispatch_uses_envelope(70, "stream"));
    assert!(!dispatch_uses_envelope(70, "download"));
    assert!(dispatch_uses_envelope(70, "getsong"));
}

/// Backend whose transcode pipe must never run.
#[derive(Clone)]
struct NoPipe;

impl AudioBackend for NoPipe {
    async fn play(
        &self,
        file_id: &str,
        params: &StreamParams,
        head_only: bool,
    ) -> Result<Option<PlayedAudio>, BackendError> {
        assert!(head_only, "HEAD must not run the transcode pipe");
        FakeAudio.play(file_id, params, head_only).await
    }
}

#[tokio::test]
async fn transcoding_streams() {
    let store = FakeStore::loaded();
    let on = transcoding();
    let song = run(&on, &store, &request("getSong", &[("id", "tr-f1")], true)).await;
    let text = song.body_text();
    assert!(
        text.contains("transcodedContentType=\"audio/mpeg\"")
            && text.contains("transcodedSuffix=\"mp3\""),
        "{text}"
    );

    let opus = run(
        &on,
        &store,
        &request("stream", &[("id", "tr-f1"), ("format", "opus")], true),
    )
    .await;
    assert_eq!(
        (opus.status, opus.content_type.as_str()),
        (200, "audio/ogg")
    );
    assert_eq!(opus.header("Accept-Ranges"), Some("none"));
    assert_eq!(opus.header("Cache-Control"), Some("no-store"));
    assert_eq!(opus.body, b"TRANSCODED");

    // HEAD answers the transcode headers without running the pipe.
    let mut head = request("stream", &[("id", "tr-f1"), ("format", "opus")], true);
    head.method = "HEAD".to_owned();
    let body = dispatch(&FakeVerifier, &store, &NoPipe, &on, &head).await;
    assert_eq!(
        (body.status, body.content_type.as_str()),
        (200, "audio/ogg")
    );
    assert!(body.body.is_empty());

    // Only transcodes carry the estimated length.
    let estimate = [
        ("id", "tr-f2"),
        ("maxBitRate", "128"),
        ("estimateContentLength", "true"),
    ];
    let body = run(&on, &store, &request("stream", &estimate, true)).await;
    assert_eq!(body.body, b"TRANSCODED");
    assert_eq!(body.header("Content-Length"), Some("3200000"));

    let direct = [
        ("mediaId", "tr-f1"),
        ("mediaType", "song"),
        ("transcodeParams", "direct-params"),
    ];
    assert_eq!(get("getTranscodeStream", &direct).await.body.len(), 100);
    let signed = [
        ("mediaId", "tr-f1"),
        ("mediaType", "song"),
        ("transcodeParams", "signed-params"),
    ];
    let body = run(&on, &store, &request("getTranscodeStream", &signed, true)).await;
    assert_eq!(body.body, b"TRANSCODED");
}
