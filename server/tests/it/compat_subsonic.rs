//! Subsonic wire contract beyond the pinned references in
//! `compat_journeys`: envelopes, error codes, response shapes per
//! endpoint, cover art, binary endpoints, transcoding and mutations.
//! Runs `dispatch` over the `fake` fixture with its fixed clock.

use std::collections::HashMap;

use droppedneedle::compat::subsonic::fake::{FakeAudio, FakeStore, FakeVerifier, NOW_UNIX};
use droppedneedle::compat::subsonic::params::SubsonicParameters;
use droppedneedle::compat::subsonic::stream::{
    AudioBackend, AudioBody, AudioFacts, BackendError, OpenedAudio, StreamPlan, decide,
};
use droppedneedle::compat::subsonic::value::{Rendered, Val, obj};
use droppedneedle::compat::subsonic::{Request, Settings, dispatch, dispatch_uses_envelope};

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
        now_unix: Some(NOW_UNIX),
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

fn assert_ok(body: &Rendered, case: &str) {
    let text = body.body_text();
    assert_eq!(body.status, 200, "{case}");
    assert!(
        text.contains("status=\"ok\"") || text.contains("\"status\":\"ok\""),
        "{case}: {text}"
    );
}

fn assert_code(body: &Rendered, code: u32, case: &str) {
    let text = body.body_text();
    assert_eq!(body.status, 200, "{case}");
    assert_eq!(body.content_type, "application/xml", "{case}");
    assert!(text.contains("status=\"failed\""), "{case}: {text}");
    assert!(text.contains(&format!("code=\"{code}\"")), "{case}: {text}");
}

const XML_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"http://subsonic.org/restapi\"";
const ATTRS: &str =
    "version=\"1.16.1\" type=\"DroppedNeedle\" serverVersion=\"3.0.0-test\" openSubsonic=\"true\"";

#[tokio::test]
async fn envelopes_are_byte_exact() {
    let store = FakeStore::loaded();
    let ok = format!("{XML_HEAD} status=\"ok\" {ATTRS}");
    let cases = [
        (request("ping", &[], true), "application/xml", format!("{ok}/>")),
        (
            request("ping", &[("f", "json")], true),
            "application/json",
            "{\"subsonic-response\":{\"status\":\"ok\",\"version\":\"1.16.1\",\"type\":\"DroppedNeedle\",\"serverVersion\":\"3.0.0-test\",\"openSubsonic\":true}}".to_owned(),
        ),
        // Endpoint names casefold and drop `.view`.
        (
            request("GETLICENSE", &[], true),
            "application/xml",
            format!("{ok}><license valid=\"true\"/></subsonic-response>"),
        ),
        // Extensions are public; `transcoding` is served but not advertised.
        (
            request("getOpenSubsonicExtensions", &[], false),
            "application/xml",
            format!(
                "{ok}><openSubsonicExtensions name=\"apiKeyAuthentication\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"formPost\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"transcodeOffset\"><versions>1</versions></openSubsonicExtensions></subsonic-response>"
            ),
        ),
        (
            request("ping", &[], false),
            "application/xml",
            format!(
                "{XML_HEAD} status=\"failed\" {ATTRS}><error code=\"10\" message=\"Required parameter is missing.\"/></subsonic-response>"
            ),
        ),
    ];
    for (req, content_type, expected) in cases {
        let body = run(&settings(), &store, &req).await;
        assert_eq!(body.status, 200, "{}", req.endpoint);
        assert_eq!(body.content_type, content_type, "{}", req.endpoint);
        assert_eq!(body.body_text(), expected, "{}", req.endpoint);
    }
    assert_ok(&get("Ping.VIEW", &[]).await, "ping.view");

    // Errors keep the requested format.
    let body = get("nope", &[("f", "json")]).await;
    assert_eq!(body.content_type, "application/json");
    assert!(body.body_text().contains("\"code\":0"));

    // JSONP wraps only a safe callback name, otherwise falls back to JSON.
    let body = get("ping", &[("f", "jsonp"), ("callback", "app.cb$1")]).await;
    assert_eq!(body.content_type, "application/javascript");
    let text = body.body_text();
    assert!(text.starts_with("app.cb$1({\"subsonic-response\":") && text.ends_with("});"));
    let body = get("ping", &[("f", "jsonp"), ("callback", "alert(1)")]).await;
    assert_eq!(body.content_type, "application/json");
    assert!(body.body_text().starts_with("{\"subsonic-response\":"));
}

#[tokio::test]
async fn error_codes() {
    /// Endpoint, query pairs and the expected Subsonic error code.
    type Case<'a> = (&'a str, &'a [(&'a str, &'a str)], u32);
    let store = FakeStore::loaded();
    let unauthed: [Case; 6] = [
        (
            "ping",
            &[("u", "user"), ("p", "wrong"), ("c", "golden")],
            40,
        ),
        ("ping", &[("apiKey", "nope"), ("c", "golden")], 44),
        ("ping", &[("apiKey", "key-1"), ("u", "user")], 43),
        ("ping", &[("u", "user"), ("t", "abc")], 10),
        (
            "ping",
            &[("u", "user"), ("p", "secret"), ("t", "a"), ("s", "b")],
            10,
        ),
        ("ping", &[("u", "user"), ("u", "user"), ("p", "secret")], 10),
    ];
    for (endpoint, pairs, code) in unauthed {
        let body = run(&settings(), &store, &request(endpoint, pairs, false)).await;
        assert_code(&body, code, &format!("{endpoint} {pairs:?}"));
    }
    let good_key = request("ping", &[("apiKey", "key-1"), ("c", "golden")], false);
    assert_ok(&run(&settings(), &store, &good_key).await, "apiKey alone");

    let authed: [Case; 36] = [
        ("nope", &[], 0),
        ("ping", &[("f", "yaml")], 10),
        ("getArtists", &[("musicFolderId", "2")], 70),
        ("getArtist", &[("id", "tr-f1")], 70),
        ("getArtist", &[("id", "xx-1")], 70),
        ("getSong", &[("id", "tr-nope")], 70),
        ("getAlbumList2", &[], 10),
        ("getAlbumList2", &[("type", "highest")], 0),
        (
            "getAlbumList2",
            &[("type", "byYear"), ("fromYear", "2020")],
            10,
        ),
        (
            "getAlbumList2",
            &[("type", "newest"), ("fromYear", "2020")],
            10,
        ),
        ("getAlbumList2", &[("type", "byGenre")], 10),
        ("getMusicDirectory", &[("id", "tr-f1")], 70),
        ("getPlaylist", &[("id", "pl-nope")], 70),
        ("createPlaylist", &[], 10),
        ("star", &[], 10),
        ("star", &[("id", "tr-nope")], 70),
        ("setRating", &[("id", "tr-f1"), ("rating", "6")], 10),
        ("setRating", &[("id", "tr-nope"), ("rating", "3")], 70),
        ("setRating", &[("id", "tr-f1")], 10),
        ("scrobble", &[], 10),
        (
            "scrobble",
            &[("id", "tr-f1"), ("id", "tr-f2"), ("time", "1")],
            0,
        ),
        (
            "reportPlayback",
            &[
                ("mediaId", "tr-f1"),
                ("mediaType", "song"),
                ("positionMs", "1"),
            ],
            10,
        ),
        (
            "createBookmark",
            &[("id", "tr-nope"), ("position", "10")],
            70,
        ),
        ("getAlbumInfo2", &[("id", "al-nope")], 70),
        ("getLyrics", &[], 10),
        ("getSongsByGenre", &[], 10),
        ("getUser", &[], 10),
        ("startScan", &[], 50),
        ("getTopSongs", &[("artist", "Testers")], 70),
        ("getSimilarSongs", &[("id", "ar-nope")], 70),
        ("getSimilarSongs", &[("id", "tr-f1")], 70),
        (
            "getTranscodeStream",
            &[("mediaId", "tr-f1"), ("mediaType", "song")],
            10,
        ),
        (
            "getTranscodeDecision",
            &[("mediaId", "tr-f2"), ("mediaType", "song")],
            10,
        ),
        ("savePlayQueue", &[("id", "tr-f1"), ("id", "tr-f2")], 10),
        ("savePlayQueue", &[("current", "tr-f1")], 10),
        (
            "savePlayQueueByIndex",
            &[("id", "tr-f1"), ("currentIndex", "4")],
            10,
        ),
    ];
    for (endpoint, pairs, code) in authed {
        assert_code(
            &get(endpoint, pairs).await,
            code,
            &format!("{endpoint} {pairs:?}"),
        );
    }

    // A disabled API answers code 0 before any lookup or auth.
    let off = Settings {
        enabled: false,
        ..settings()
    };
    for endpoint in ["ping", "nope"] {
        let body = run(&off, &store, &request(endpoint, &[], true)).await;
        assert_code(&body, 0, endpoint);
        assert!(body.body_text().contains("disabled"));
    }
}

#[tokio::test]
async fn response_shapes() {
    type Case<'a> = (
        &'a str,
        &'a [(&'a str, &'a str)],
        &'a [&'a str],
        &'a [&'a str],
    );
    let cases: &[Case] = &[
        ("getMusicFolders", &[], &["musicFolder id=\"1\""], &[]),
        (
            "getArtists",
            &[("musicFolderId", "1"), ("musicFolderId", "1")],
            &[],
            &[],
        ),
        // Articles are ignored when bucketing: "The Testers" files under T.
        (
            "getArtists",
            &[],
            &[
                "ignoredArticles=\"The El La Los Las Le Les\"",
                "index name=\"A\"",
                "index name=\"T\"",
            ],
            &["index name=\"H\""],
        ),
        (
            "getIndexes",
            &[("ifModifiedSince", "42")],
            &["lastModified=\"42\""],
            &["<index "],
        ),
        ("getIndexes", &[("ifModifiedSince", "41")], &["<index"], &[]),
        (
            "getArtist",
            &[("id", "ar-artist-1")],
            &["First Album", "Second Sounds"],
            &[],
        ),
        (
            "getAlbum",
            &[("id", "al-rg-1")],
            &[
                "Song One",
                "Song Two",
                "mediaType=\"song\"",
                "channelCount=\"2\"",
            ],
            &[],
        ),
        (
            "getSong",
            &[("id", "tr-f2")],
            &[
                "contentType=\"audio/flac\"",
                "suffix=\"flac\"",
                "<genres name=\"Rock\"/>",
                "<genres name=\"Indie\"/>",
            ],
            &["transcodedContentType"],
        ),
        (
            "getAlbumList2",
            &[("type", "byYear"), ("fromYear", "0"), ("toYear", "2021")],
            &["First Album", "Second Sounds"],
            &[],
        ),
        (
            "getAlbumList2",
            &[("type", "byGenre"), ("genre", "Rock")],
            &["First Album"],
            &[],
        ),
        (
            "getAlbumList",
            &[("type", "newest")],
            &["<albumList", "isDir=\"true\""],
            &[],
        ),
        ("getRandomSongs", &[("size", "5")], &["<randomSongs"], &[]),
        (
            "getMusicDirectory",
            &[("id", "1")],
            &["The Testers", "isDir=\"true\""],
            &[],
        ),
        (
            "getMusicDirectory",
            &[("id", "ar-artist-1")],
            &["First Album"],
            &[],
        ),
        (
            "getMusicDirectory",
            &[("id", "al-rg-1")],
            &["Song One", "parent=\"ar-artist-1\""],
            &[],
        ),
        // Missing, empty and quoted-empty queries match everything.
        (
            "search3",
            &[("songCount", "10")],
            &["Song One", "Song Two"],
            &[],
        ),
        (
            "search3",
            &[("query", ""), ("songCount", "10")],
            &["Song One"],
            &[],
        ),
        (
            "search3",
            &[("query", "\"\""), ("songCount", "10")],
            &["Song One"],
            &[],
        ),
        (
            "search3",
            &[("query", "''"), ("songCount", "10")],
            &["Song One"],
            &[],
        ),
        (
            "search3",
            &[("query", "  "), ("songCount", "10")],
            &["Song One"],
            &[],
        ),
        (
            "search3",
            &[("query", "\"Song One\""), ("songCount", "10")],
            &["Song One"],
            &[],
        ),
        (
            "search3",
            &[
                ("songCount", "1"),
                ("songOffset", "1"),
                ("artistCount", "0"),
                ("albumCount", "0"),
            ],
            &["Song Two"],
            &["Song One"],
        ),
        (
            "search2",
            &[("query", "Testers")],
            &["<searchResult2", "The Testers"],
            &[],
        ),
        // Playlist counts and entries cover streamable tracks only.
        (
            "getPlaylists",
            &[],
            &["songCount=\"2\"", "duration=\"380\"", "coverArt=\"pl-p1\""],
            &[],
        ),
        (
            "getPlaylist",
            &[("id", "pl-p1")],
            &["songCount=\"2\"", "Song One", "Ballad", "owner=\"user\""],
            &[],
        ),
        ("getStarred", &[], &["<starred", "Song One"], &[]),
        (
            "getNowPlaying",
            &[],
            &[
                "username=\"user Display\"",
                "minutesAgo=\"2\"",
                "playerId=\"0\"",
                "playerName=\"Symfonium\"",
                "Song Two",
            ],
            &[],
        ),
        (
            "getPlayQueue",
            &[],
            &[
                "current=\"tr-f1\"",
                "position=\"1000\"",
                "changedBy=\"golden\"",
            ],
            &[],
        ),
        ("getPlayQueueByIndex", &[], &["currentIndex=\"0\""], &[]),
        (
            "getBookmarks",
            &[],
            &[
                "position=\"5000\"",
                "comment=\"chorus\"",
                "username=\"user\"",
            ],
            &[],
        ),
        // Info image URLs never embed credentials.
        (
            "getArtistInfo2",
            &[("id", "ar-artist-1")],
            &[
                "<artistInfo2",
                "musicBrainzId=\"artist-1\"",
                "smallImageUrl=\"http://localhost/subsonic/rest/getCoverArt?id=ar-artist-1&amp;size=250\"",
            ],
            &["apiKey"],
        ),
        (
            "getArtistInfo",
            &[("id", "ar-artist-1")],
            &["<artistInfo"],
            &["apiKey"],
        ),
        ("getAlbumInfo2", &[("id", "al-rg-1")], &["<albumInfo2"], &[]),
        ("getAlbumInfo", &[("id", "al-rg-1")], &["<albumInfo"], &[]),
        (
            "getLyricsBySongId",
            &[("id", "tr-f1")],
            &[
                "lang=\"eng\"",
                "synced=\"true\"",
                "First line",
                "displayTitle=\"Song One\"",
            ],
            &[],
        ),
        (
            "getLyricsBySongId",
            &[("id", "tr-f2")],
            &[],
            &["<structuredLyrics"],
        ),
        (
            "getLyrics",
            &[("artist", "The Testers"), ("title", "Song One")],
            &["First line", "Second line"],
            &[],
        ),
        (
            "getLyrics",
            &[("title", "No Such Song")],
            &["<lyrics"],
            &["First line"],
        ),
        (
            "getGenres",
            &[("f", "json")],
            &["\"songCount\":3", "\"albumCount\":2"],
            &[],
        ),
        (
            "getSongsByGenre",
            &[("genre", "Rock"), ("count", "2")],
            &["Song One"],
            &[],
        ),
        // getUser always describes the caller.
        (
            "getUser",
            &[("username", "ignored-anyone")],
            &[
                "username=\"user\"",
                "adminRole=\"false\"",
                "streamRole=\"true\"",
                "maxBitRate=\"320\"",
            ],
            &[],
        ),
        ("getScanStatus", &[], &["scanning=\"false\""], &[]),
        (
            "getTopSongs",
            &[("artist", "The Testers")],
            &["Song One"],
            &[],
        ),
        (
            "getSimilarSongs2",
            &[("id", "ar-artist-1")],
            &["Song One"],
            &[],
        ),
        (
            "getSimilarSongs",
            &[("id", "ar-artist-1")],
            &["Song One"],
            &[],
        ),
        ("setRating", &[("id", "tr-f1"), ("rating", "4")], &[], &[]),
        (
            "scrobble",
            &[("id", "tr-f1"), ("time", "1700000000000")],
            &[],
            &[],
        ),
        (
            "scrobble",
            &[("id", "tr-f1"), ("submission", "false")],
            &[],
            &[],
        ),
        (
            "reportPlayback",
            &[
                ("mediaId", "tr-f1"),
                ("mediaType", "song"),
                ("positionMs", "1000"),
                ("state", "playing"),
            ],
            &[],
            &[],
        ),
    ];
    for (endpoint, pairs, present, absent) in cases {
        let body = get(endpoint, pairs).await;
        let case = format!("{endpoint} {pairs:?}");
        assert_ok(&body, &case);
        let text = body.body_text();
        for needle in *present {
            assert!(text.contains(needle), "{case}: missing {needle}\n{text}");
        }
        for needle in *absent {
            assert!(
                !text.contains(needle),
                "{case}: unexpected {needle}\n{text}"
            );
        }
    }

    // Album lists: newest first; byYear direction follows the bounds' order.
    let order = |text: &str, first: &str, second: &str| {
        let at = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
        at(first) < at(second)
    };
    let newest = get("getAlbumList2", &[("type", "newest")])
        .await
        .body_text();
    assert!(order(&newest, "Second Sounds", "First Album"), "{newest}");
    let up = [("type", "byYear"), ("fromYear", "2019"), ("toYear", "2021")];
    let text = get("getAlbumList2", &up).await.body_text();
    assert!(order(&text, "First Album", "Second Sounds"), "{text}");
    let down = [("type", "byYear"), ("fromYear", "2021"), ("toYear", "2019")];
    let text = get("getAlbumList2", &down).await.body_text();
    assert!(order(&text, "Second Sounds", "First Album"), "{text}");
}

#[tokio::test]
async fn cover_art_buckets_caching_and_misses() {
    let immutable = Some("public, max-age=31536000, immutable");
    for (id, size, bytes) in [
        ("al-rg-1", None, &b"COVER-500"[..]),
        ("al-rg-1", Some("100"), b"COVER-250"),
        ("al-rg-1", Some("2000"), b"COVER-1200"),
        ("tr-f1", None, b"COVER-500"),
        ("ar-artist-1", None, b"ARTIST-500"),
    ] {
        let mut pairs = vec![("id", id)];
        pairs.extend(size.map(|size| ("size", size)));
        let body = get("getCoverArt", &pairs).await;
        assert_eq!(
            (body.status, body.content_type.as_str()),
            (200, "image/jpeg"),
            "{id}"
        );
        assert_eq!(body.header("Cache-Control"), immutable, "{id} {size:?}");
        assert_eq!(body.body, bytes, "{id} {size:?}");
    }
    let playlist = get("getCoverArt", &[("id", "pl-p1")]).await;
    assert_eq!(
        playlist.header("Cache-Control"),
        Some("private, max-age=3600")
    );
    assert_eq!(playlist.body, b"PLAYLIST-COVER");
    // A known album without art gets the placeholder; unknown ids 404 text.
    let placeholder = get("getCoverArt", &[("id", "al-rg-2")]).await;
    assert_eq!(placeholder.content_type, "image/svg+xml");
    assert_eq!(
        placeholder.header("Cache-Control"),
        Some("public, max-age=86400")
    );
    for id in ["al-nope", "xx-1"] {
        let body = get("getCoverArt", &[("id", id)]).await;
        assert_eq!(
            (body.status, body.content_type.as_str()),
            (404, "text/plain"),
            "{id}"
        );
    }
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
    use droppedneedle::compat::subsonic::stream::download_filename;
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

    // Auth failures stay enveloped on binary endpoints; not-found goes text.
    assert!(dispatch_uses_envelope(50, "stream"));
    assert!(dispatch_uses_envelope(40, "getavatar"));
    assert!(!dispatch_uses_envelope(70, "stream"));
    assert!(dispatch_uses_envelope(70, "getsong"));
}

/// Backend whose transcode pipe must never run.
#[derive(Clone)]
struct NoPipe;

impl AudioBackend for NoPipe {
    async fn audio_facts(&self, file_id: &str) -> Result<Option<AudioFacts>, BackendError> {
        FakeAudio.audio_facts(file_id).await
    }
    async fn open_original(&self, file_id: &str) -> Result<Option<OpenedAudio>, BackendError> {
        FakeAudio.open_original(file_id).await
    }
    async fn transcode(
        &self,
        _: &str,
        _: &StreamPlan,
    ) -> Result<(AudioBody, String), BackendError> {
        panic!("HEAD must not run the transcode pipe");
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

#[tokio::test]
async fn transcode_decision_needs_a_json_post_for_an_existing_song() {
    let store = FakeStore::loaded();
    let post = |pairs: &[(&str, &str)], content_type: &str, body: &[u8]| {
        let mut req = request("getTranscodeDecision", pairs, true);
        req.method = "POST".to_owned();
        req.content_type = Some(content_type.to_owned());
        req.body = body.to_vec();
        req
    };
    let song = [("mediaId", "tr-f2"), ("mediaType", "song")];
    let feishin = br#"{"name":"Feishin","platform":"web","maxAudioBitrate":0}"#;
    let body = run(
        &settings(),
        &store,
        &post(&song, "application/json", feishin),
    )
    .await;
    let text = body.body_text();
    for needle in [
        "<transcodeDecision",
        "transcodeParams=\"signed-params\"",
        "canDirectPlay=\"true\"",
    ] {
        assert!(text.contains(needle), "{text}");
    }

    let minimal = br#"{"name":"x","platform":"y"}"#;
    let cases = [
        (
            post(&song, "application/x-www-form-urlencoded", b"{}"),
            "requires JSON",
        ),
        (
            post(
                &song,
                "application/json",
                br#"{"name":"x","platform":"y","bogus":1}"#,
            ),
            "code=\"10\"",
        ),
        (
            post(
                &[("mediaId", "tr-f2"), ("mediaType", "podcast")],
                "application/json",
                minimal,
            ),
            "Only song",
        ),
        (
            post(
                &[("mediaId", "tr-nope"), ("mediaType", "song")],
                "application/json",
                minimal,
            ),
            "code=\"70\"",
        ),
    ];
    for (req, needle) in cases {
        let text = run(&settings(), &store, &req).await.body_text();
        assert!(text.contains(needle), "{needle}: {text}");
    }
    assert!(
        get("getTranscodeDecision", &song)
            .await
            .body_text()
            .contains("requires POST")
    );
}

#[tokio::test]
async fn mutations_persist_in_the_store() {
    let store = FakeStore::loaded();
    let call = |endpoint: &'static str, pairs: Vec<(&'static str, &'static str)>| {
        let store = &store;
        async move { run(&settings(), store, &request(endpoint, &pairs, true)).await }
    };

    // Playlists: create, replace contents by id, rename and remove by index,
    // delete.
    let created = call("createPlaylist", vec![("name", "New"), ("songId", "tr-f1")]).await;
    let text = created.body_text();
    assert!(
        text.contains("name=\"New\"") && text.contains("songCount=\"1\""),
        "{text}"
    );
    let replaced = call(
        "createPlaylist",
        vec![("playlistId", "pl-p1"), ("songId", "tr-f3")],
    )
    .await;
    let text = replaced.body_text();
    assert!(
        text.contains("songCount=\"1\"") && text.contains("Ballad") && !text.contains("Song One"),
        "{text}"
    );
    call(
        "createPlaylist",
        vec![
            ("playlistId", "pl-p1"),
            ("songId", "tr-f1"),
            ("songId", "tr-f2"),
        ],
    )
    .await;
    let updated = call(
        "updatePlaylist",
        vec![
            ("playlistId", "pl-p1"),
            ("name", "Renamed"),
            ("songIndexToRemove", "0"),
        ],
    )
    .await;
    assert_ok(&updated, "updatePlaylist");
    let text = call("getPlaylist", vec![("id", "pl-p1")]).await.body_text();
    assert!(
        text.contains("name=\"Renamed\"") && text.contains("songCount=\"1\""),
        "{text}"
    );
    assert_ok(
        &call("deletePlaylist", vec![("id", "pl-p1")]).await,
        "deletePlaylist",
    );
    assert_code(
        &call("getPlaylist", vec![("id", "pl-p1")]).await,
        70,
        "deleted",
    );

    // Stars.
    assert_ok(
        &call("star", vec![("id", "tr-f2"), ("albumId", "al-rg-1")]).await,
        "star",
    );
    let text = call("getStarred2", vec![]).await.body_text();
    assert!(
        text.contains("Song Two") && text.contains("First Album"),
        "{text}"
    );
    assert_ok(&call("unstar", vec![("id", "tr-f2")]).await, "unstar");

    // Play queues: `current` must reference a queued song.
    let text = call("savePlayQueue", vec![("id", "tr-f1"), ("current", "tr-f3")])
        .await
        .body_text();
    assert!(text.contains("must reference a queued song"), "{text}");
    let saved = call(
        "savePlayQueue",
        vec![("id", "tr-f2"), ("current", "tr-f2"), ("position", "5")],
    )
    .await;
    assert_ok(&saved, "savePlayQueue");
    assert!(
        call("getPlayQueue", vec![])
            .await
            .body_text()
            .contains("current=\"tr-f2\"")
    );
    let saved = call(
        "savePlayQueueByIndex",
        vec![("id", "tr-f1"), ("currentIndex", "0")],
    )
    .await;
    assert_ok(&saved, "savePlayQueueByIndex");

    // Bookmarks.
    assert_ok(
        &call("createBookmark", vec![("id", "tr-f2"), ("position", "10")]).await,
        "create",
    );
    assert_ok(
        &call("deleteBookmark", vec![("id", "tr-f2")]).await,
        "delete",
    );

    // Admin-only calls succeed for an admin.
    let admin = |endpoint: &str, extra: &[(&str, &str)]| {
        let mut pairs = vec![("u", "admin"), ("p", "secret")];
        pairs.extend_from_slice(extra);
        request(endpoint, &pairs, false)
    };
    let user = run(&settings(), &store, &admin("getUser", &[("username", "x")])).await;
    assert!(user.body_text().contains("adminRole=\"true\""));
    let scan = run(&settings(), &store, &admin("startScan", &[])).await;
    assert!(scan.body_text().contains("scanning=\"true\""));
}

/// When `stream` transcodes: only on a real trigger, never when forced
/// original, disabled or without ffmpeg.
#[test]
fn stream_transcode_decision() {
    let plan = |source: (&str, i64), requested: Option<&str>, ceiling: Option<i64>| {
        decide(
            source.0,
            Some(source.1),
            requested,
            ceiling,
            false,
            0.0,
            true,
            true,
            "mp3",
            320,
        )
    };
    // A server ceiling alone, or a zero client bitrate (Feishin), is no trigger.
    assert!(!plan(("flac", 900), None, None).transcode);
    assert!(!plan(("flac", 900), None, Some(0)).transcode);
    assert!(!plan(("mp3", 320), Some("mp3"), None).transcode);
    let ceiling = plan(("flac", 900), None, Some(128));
    assert!(ceiling.transcode);
    assert_eq!(
        (ceiling.out_format.as_deref(), ceiling.out_bitrate_kbps),
        (Some("mp3"), Some(128))
    );
    assert_eq!(
        plan(("mp3", 320), Some("opus"), None).out_format.as_deref(),
        Some("opus")
    );
    for (force, enabled, ffmpeg) in [
        (true, true, true),
        (false, false, true),
        (false, true, false),
    ] {
        let plan = decide(
            "flac",
            Some(900),
            Some("opus"),
            Some(64),
            force,
            5.0,
            enabled,
            ffmpeg,
            "mp3",
            320,
        );
        assert!(!plan.transcode);
        assert_eq!(plan.start_seconds, 5.0);
    }
}

/// Request parsing bounds and XML escaping: both guard against hostile input.
#[test]
fn request_limits_and_xml_escaping() {
    use droppedneedle::compat::subsonic::params::{
        MAX_REQUEST_PARAMETER_BYTES, check_limits, decode_pairs,
    };
    assert_eq!(MAX_REQUEST_PARAMETER_BYTES, 64 * 1024);
    assert_eq!(
        decode_pairs(b"a=1&b=x+y&q=%22%22").expect("decodes"),
        vec![
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "x y".to_owned()),
            ("q".to_owned(), "\"\"".to_owned()),
        ]
    );
    for bad in [&b"a=%"[..], b"a=%zz", b"a=%2", &[0xff]] {
        assert_eq!(decode_pairs(bad).expect_err("rejects").code, 10, "{bad:?}");
    }
    let pair = |key: &str, value: String| vec![(key.to_owned(), value)];
    assert!(check_limits(&pair("a", "b".to_owned())).is_ok());
    assert_eq!(
        check_limits(&pair("", "b".to_owned()))
            .expect_err("empty key")
            .code,
        10
    );
    assert_eq!(
        check_limits(&pair("a", "x".repeat(9000)))
            .expect_err("long value")
            .code,
        10
    );
    let many = vec![("a".to_owned(), "b".to_owned()); 2000];
    assert_eq!(check_limits(&many).expect_err("too many").code, 10);

    let value = obj(vec![("a", Val::Str("x\x07\x1b&<>\"".to_owned()))]);
    let xml = Val::Obj(vec![("root".to_owned(), value)]).to_xml("t");
    assert!(
        xml.contains("x&amp;&lt;&gt;&quot;") && !xml.contains('\x07'),
        "{xml}"
    );
    let stripped = obj(vec![("gone", Val::Null), ("empty", Val::List(vec![]))]).stripped();
    assert_eq!(stripped.to_json(), "{\"empty\":[]}");
}
