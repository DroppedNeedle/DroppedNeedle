//! Subsonic-compat golden briefs: one per matrix row, pinning status,
//! content type, headers, and body.
//!
//! Runs `dispatch` against the `fake` fixture with its fixed clock. The
//! goldens are these inline asserts (exact bytes for key rows, field
//! asserts elsewhere) — there is no on-disk byte corpus and no runner
//! sibling; see the harness doc in `server/src/compat/subsonic/mod.rs`.

use std::collections::HashMap;

use droppedneedle::compat::subsonic::fake::{FakeAudio, FakeStore, FakeVerifier, NOW_UNIX};
use droppedneedle::compat::subsonic::params::SubsonicParameters;
use droppedneedle::compat::subsonic::value::Rendered;
use droppedneedle::compat::subsonic::{Request, Settings, dispatch};

fn settings() -> Settings {
    Settings {
        enabled: true,
        server_version: "3.0.0-test".to_owned(),
        ..Settings::default()
    }
}

fn transcoding_settings() -> Settings {
    Settings {
        enabled: true,
        server_version: "3.0.0-test".to_owned(),
        transcoding_enabled: true,
        ffmpeg_available: true,
        ..Settings::default()
    }
}

fn params(pairs: &[(&str, &str)]) -> SubsonicParameters {
    SubsonicParameters::new(
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
    )
}

fn authed(pairs: &[(&str, &str)]) -> SubsonicParameters {
    let mut all = vec![
        ("u", "user"),
        ("p", "secret"),
        ("v", "1.16.1"),
        ("c", "golden"),
    ];
    all.extend_from_slice(pairs);
    params(&all)
}

fn request(endpoint: &str, params: SubsonicParameters) -> Request {
    Request {
        method: "GET".to_owned(),
        endpoint: endpoint.to_owned(),
        params,
        headers: HashMap::new(),
        body: Vec::new(),
        content_type: None,
        now_unix: Some(NOW_UNIX),
    }
}

async fn get(endpoint: &str, pairs: &[(&str, &str)]) -> Rendered {
    dispatch(
        &FakeVerifier,
        &FakeStore::loaded(),
        &FakeAudio,
        &settings(),
        &request(endpoint, authed(pairs)),
    )
    .await
}

async fn get_with(
    settings: &Settings,
    store: &FakeStore,
    _endpoint: &str,
    req: Request,
) -> Rendered {
    dispatch(&FakeVerifier, store, &FakeAudio, settings, &req).await
}

fn brief_ok(body: &Rendered, brief: &str) {
    assert_eq!(body.status, 200, "{brief}: status");
    assert_eq!(
        body.content_type, "application/xml",
        "{brief}: content-type"
    );
    assert!(
        body.body_text().contains("status=\"ok\"")
            || body.body_text().contains("\"status\":\"ok\""),
        "{brief}: ok envelope\n{}",
        body.body_text()
    );
}

// --- transport: normalization, formats, errors, auth ---

#[tokio::test]
async fn ping_xml_exact_bytes() {
    let body = get("ping", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert_eq!(
        body.body_text(),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"http://subsonic.org/restapi\" status=\"ok\" version=\"1.16.1\" type=\"DroppedNeedle\" serverVersion=\"3.0.0-test\" openSubsonic=\"true\"/>"
    );
}

#[tokio::test]
async fn ping_json_exact_bytes() {
    let body = get("ping", &[("f", "json")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/json");
    assert_eq!(
        body.body_text(),
        "{\"subsonic-response\":{\"status\":\"ok\",\"version\":\"1.16.1\",\"type\":\"DroppedNeedle\",\"serverVersion\":\"3.0.0-test\",\"openSubsonic\":true}}"
    );
}

#[tokio::test]
async fn ping_jsonp_wraps_safe_callback() {
    let body = get("ping", &[("f", "jsonp"), ("callback", "app.cb$1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/javascript");
    assert!(
        body.body_text()
            .starts_with("app.cb$1({\"subsonic-response\":")
    );
    assert!(body.body_text().ends_with("});"));
}

#[tokio::test]
async fn ping_jsonp_unsafe_callback_falls_back_to_json() {
    let body = get("ping", &[("f", "jsonp"), ("callback", "alert(1)")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/json");
    assert!(body.body_text().starts_with("{\"subsonic-response\":"));
}

#[tokio::test]
async fn endpoint_name_casefolded_and_view_stripped() {
    let body = get("Ping.VIEW", &[]).await;
    brief_ok(&body, "ping.view");
    let body = get("GETLICENSE", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert_eq!(
        body.body_text(),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"http://subsonic.org/restapi\" status=\"ok\" version=\"1.16.1\" type=\"DroppedNeedle\" serverVersion=\"3.0.0-test\" openSubsonic=\"true\"><license valid=\"true\"/></subsonic-response>"
    );
}

#[tokio::test]
async fn unknown_method_is_code_0() {
    let body = get("nope", &[]).await;
    assert_eq!(body.content_type, "application/xml");
    assert_eq!(body.status, 200);
    assert!(
        body.body_text().contains("status=\"failed\""),
        "{}",
        body.body_text()
    );
    assert!(
        body.body_text().contains("code=\"0\""),
        "{}",
        body.body_text()
    );
    assert!(
        body.body_text().contains("Unknown method nope"),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn invalid_format_is_code_10_in_sniffed_default() {
    let body = get("ping", &[("f", "yaml")]).await;
    assert_eq!(body.status, 200);
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
    assert_eq!(body.content_type, "application/xml");
}

#[tokio::test]
async fn error_envelope_keeps_requested_json() {
    let body = get("nope", &[("f", "json")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/json");
    assert!(
        body.body_text().contains("\"code\":0"),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn disabled_api_is_code_0_before_lookup() {
    let settings = Settings {
        enabled: false,
        ..settings()
    };
    let body = get_with(
        &settings,
        &FakeStore::loaded(),
        "ping",
        request("ping", authed(&[])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"0\""),
        "{}",
        body.body_text()
    );
    assert!(
        body.body_text().contains("disabled"),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings,
        &FakeStore::loaded(),
        "nope",
        request("nope", authed(&[])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("disabled"),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn extensions_are_public_no_auth() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "getOpenSubsonicExtensions",
        request("getOpenSubsonicExtensions", params(&[])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    // Matrix wins: exactly 3 advertised (`transcoding` is served but
    // deliberately unadvertised, like songLyrics) — pinned byte-exact.
    assert_eq!(
        body.body_text(),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"http://subsonic.org/restapi\" status=\"ok\" version=\"1.16.1\" type=\"DroppedNeedle\" serverVersion=\"3.0.0-test\" openSubsonic=\"true\"><openSubsonicExtensions name=\"apiKeyAuthentication\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"formPost\"><versions>1</versions></openSubsonicExtensions><openSubsonicExtensions name=\"transcodeOffset\"><versions>1</versions></openSubsonicExtensions></subsonic-response>"
    );
}

#[tokio::test]
async fn missing_credentials_are_code_10() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "ping",
        request("ping", params(&[])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert_eq!(
        body.body_text(),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"http://subsonic.org/restapi\" status=\"failed\" version=\"1.16.1\" type=\"DroppedNeedle\" serverVersion=\"3.0.0-test\" openSubsonic=\"true\"><error code=\"10\" message=\"Required parameter is missing.\"/></subsonic-response>"
    );
}

#[tokio::test]
async fn bad_password_is_code_40() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "ping",
        request(
            "ping",
            params(&[("u", "user"), ("p", "wrong"), ("c", "golden")]),
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"40\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn bad_apikey_is_code_44() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "ping",
        request("ping", params(&[("apiKey", "nope"), ("c", "golden")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"44\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn apikey_plus_user_is_code_43() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "ping",
        request("ping", params(&[("apiKey", "key-1"), ("u", "user")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"43\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn good_apikey_authenticates_without_u() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "ping",
        request("ping", params(&[("apiKey", "key-1"), ("c", "golden")])),
    )
    .await;
    brief_ok(&body, "apikey ping");
}

#[tokio::test]
async fn token_without_salt_is_code_10() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "ping",
        request("ping", params(&[("u", "user"), ("t", "abc")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn password_plus_token_is_code_10() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "ping",
        request(
            "ping",
            params(&[("u", "user"), ("p", "secret"), ("t", "a"), ("s", "b")]),
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn duplicated_auth_key_is_code_10() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "ping",
        request(
            "ping",
            params(&[("u", "user"), ("u", "user"), ("p", "secret")]),
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[test]
fn xml_strips_c0_controls_and_escapes() {
    let val = droppedneedle::compat::subsonic::value::obj(vec![(
        "a",
        droppedneedle::compat::subsonic::value::Val::Str("x\x07\x1b&<>\"".to_owned()),
    )]);
    let xml = droppedneedle::compat::subsonic::value::Val::Obj(vec![("root".to_owned(), val)])
        .to_xml("t");
    assert!(xml.contains("x&amp;&lt;&gt;&quot;"), "{xml}");
    assert!(!xml.contains('\x07'), "{xml}");
}

#[test]
fn none_strips_but_empty_lists_stay() {
    let val = droppedneedle::compat::subsonic::value::obj(vec![
        ("gone", droppedneedle::compat::subsonic::value::Val::Null),
        (
            "empty",
            droppedneedle::compat::subsonic::value::Val::List(vec![]),
        ),
    ])
    .stripped();
    assert_eq!(val.to_json(), "{\"empty\":[]}");
}

// --- browse: folders, artists, albums, songs ---

#[tokio::test]
async fn music_folders_single_folder() {
    let body = get("getMusicFolders", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("musicFolder id=\"1\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn unknown_music_folder_is_70() {
    let body = get("getArtists", &[("musicFolderId", "2")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn repeated_same_music_folder_accepted() {
    let req = request(
        "getArtists",
        params(&[
            ("u", "user"),
            ("p", "secret"),
            ("musicFolderId", "1"),
            ("musicFolderId", "1"),
        ]),
    );
    let body = get_with(&settings(), &FakeStore::loaded(), "getArtists", req).await;
    brief_ok(&body, "repeated folder");
}

#[tokio::test]
async fn artists_bucket_strips_articles() {
    let body = get("getArtists", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(
        text.contains("ignoredArticles=\"The El La Los Las Le Les\""),
        "{text}"
    );
    assert!(text.contains("index name=\"A\""), "{text}");
    assert!(text.contains("index name=\"T\""), "{text}");
    assert!(
        !text.contains("index name=\"H\""),
        "The Testers must not bucket under H: {text}"
    );
}

#[test]
fn index_letter_cases() {
    assert_eq!(
        droppedneedle::compat::subsonic::browse::index_letter("The Testers"),
        'T'
    );
    assert_eq!(
        droppedneedle::compat::subsonic::browse::index_letter("  la  Luna "),
        'L'
    );
    assert_eq!(
        droppedneedle::compat::subsonic::browse::index_letter("1984"),
        '#'
    );
    assert_eq!(
        droppedneedle::compat::subsonic::browse::index_letter(""),
        '#'
    );
}

#[tokio::test]
async fn indexes_fresh_since_returns_empty() {
    let body = get("getIndexes", &[("ifModifiedSince", "42")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("lastModified=\"42\""), "{text}");
    assert!(!text.contains("<index "), "{text}");
}

#[tokio::test]
async fn indexes_stale_since_returns_buckets() {
    let body = get("getIndexes", &[("ifModifiedSince", "41")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(body.body_text().contains("<index"), "{}", body.body_text());
}

#[tokio::test]
async fn artist_embeds_albums() {
    let body = get("getArtist", &[("id", "ar-artist-1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("First Album"), "{text}");
    assert!(text.contains("Second Sounds"), "{text}");
}

#[tokio::test]
async fn artist_wrong_kind_is_70() {
    let body = get("getArtist", &[("id", "tr-f1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn artist_unknown_prefix_is_70() {
    let body = get("getArtist", &[("id", "xx-1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn album_embeds_songs() {
    let body = get("getAlbum", &[("id", "al-rg-1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("Song One"), "{text}");
    assert!(text.contains("Song Two"), "{text}");
    assert!(text.contains("mediaType=\"song\""), "{text}");
    assert!(text.contains("channelCount=\"2\""), "{text}");
}

#[tokio::test]
async fn song_child_shape() {
    let body = get("getSong", &[("id", "tr-f2")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("contentType=\"audio/flac\""), "{text}");
    assert!(text.contains("suffix=\"flac\""), "{text}");
    assert!(text.contains("<genres name=\"Rock\"/>"), "{text}");
    assert!(text.contains("<genres name=\"Indie\"/>"), "{text}");
    assert!(
        !text.contains("transcodedContentType"),
        "no hint without ffmpeg: {text}"
    );
}

#[tokio::test]
async fn song_child_carries_transcode_hint_when_enabled() {
    let body = get_with(
        &transcoding_settings(),
        &FakeStore::loaded(),
        "getSong",
        request("getSong", authed(&[("id", "tr-f1")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(
        text.contains("transcodedContentType=\"audio/mpeg\""),
        "{text}"
    );
    assert!(text.contains("transcodedSuffix=\"mp3\""), "{text}");
}

#[tokio::test]
async fn song_missing_is_70() {
    let body = get("getSong", &[("id", "tr-nope")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

// --- album lists ---

#[tokio::test]
async fn album_list_requires_type() {
    let body = get("getAlbumList2", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn album_list_highest_is_code_0() {
    let body = get("getAlbumList2", &[("type", "highest")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("code=\"0\""), "{text}");
    assert!(text.contains("highest"), "{text}");
}

#[tokio::test]
async fn album_list_newest_orders_recent_first() {
    let body = get("getAlbumList2", &[("type", "newest")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    let second = text.find("Second Sounds").unwrap();
    let first = text.find("First Album").unwrap();
    assert!(second < first, "{text}");
}

#[tokio::test]
async fn album_list_by_year_direction_follows_param_order() {
    let asc = get(
        "getAlbumList2",
        &[("type", "byYear"), ("fromYear", "2019"), ("toYear", "2021")],
    )
    .await;
    assert_eq!(asc.status, 200);
    assert_eq!(asc.content_type, "application/xml");
    let text = asc.body_text();
    assert!(
        text.find("First Album").unwrap() < text.find("Second Sounds").unwrap(),
        "{text}"
    );
    let desc = get(
        "getAlbumList2",
        &[("type", "byYear"), ("fromYear", "2021"), ("toYear", "2019")],
    )
    .await;
    assert_eq!(desc.status, 200);
    assert_eq!(desc.content_type, "application/xml");
    let text = desc.body_text();
    assert!(
        text.find("Second Sounds").unwrap() < text.find("First Album").unwrap(),
        "{text}"
    );
}

#[tokio::test]
async fn album_list_by_year_zero_is_unbounded() {
    let body = get(
        "getAlbumList2",
        &[("type", "byYear"), ("fromYear", "0"), ("toYear", "2021")],
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(
        text.contains("First Album") && text.contains("Second Sounds"),
        "{text}"
    );
}

#[tokio::test]
async fn album_list_by_year_needs_both_bounds() {
    let body = get("getAlbumList2", &[("type", "byYear"), ("fromYear", "2020")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn album_list_rejects_stray_year_params() {
    let body = get("getAlbumList2", &[("type", "newest"), ("fromYear", "2020")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn album_list_by_genre_needs_genre() {
    let body = get("getAlbumList2", &[("type", "byGenre")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
    let body = get("getAlbumList2", &[("type", "byGenre"), ("genre", "Rock")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("First Album"),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn album_list_file_shape_uses_child() {
    let body = get("getAlbumList", &[("type", "newest")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("<albumList"), "{text}");
    assert!(text.contains("isDir=\"true\""), "{text}");
}

#[tokio::test]
async fn random_songs_filter() {
    let body = get("getRandomSongs", &[("size", "5")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("<randomSongs"),
        "{}",
        body.body_text()
    );
}

// --- directory + search ---

#[tokio::test]
async fn directory_root_lists_artists() {
    let body = get("getMusicDirectory", &[("id", "1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("The Testers"), "{text}");
    assert!(text.contains("isDir=\"true\""), "{text}");
}

#[tokio::test]
async fn directory_artist_lists_albums() {
    let body = get("getMusicDirectory", &[("id", "ar-artist-1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("First Album"),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn directory_album_lists_songs_with_parent() {
    let body = get("getMusicDirectory", &[("id", "al-rg-1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("Song One"), "{text}");
    assert!(text.contains("parent=\"ar-artist-1\""), "{text}");
}

#[tokio::test]
async fn directory_track_is_70() {
    let body = get("getMusicDirectory", &[("id", "tr-f1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn search_missing_query_matches_all() {
    let body = get("search3", &[("songCount", "10")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(
        text.contains("Song One") && text.contains("Song Two"),
        "{text}"
    );
}

#[tokio::test]
async fn search_empty_and_quoted_queries_match_all() {
    for query in ["", "\"\"", "''", "  "] {
        let body = get("search3", &[("query", query), ("songCount", "10")]).await;
        assert_eq!(body.status, 200);
        assert_eq!(body.content_type, "application/xml");
        assert!(
            body.body_text().contains("Song One"),
            "query {query:?}: {}",
            body.body_text()
        );
    }
    let body = get("search3", &[("query", "\"Song One\""), ("songCount", "10")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("Song One"),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn search_counts_and_offsets_apply_per_type() {
    let body = get(
        "search3",
        &[
            ("songCount", "1"),
            ("songOffset", "1"),
            ("artistCount", "0"),
            ("albumCount", "0"),
        ],
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(!text.contains("Song One"), "{text}");
    assert!(text.contains("Song Two"), "{text}");
}

#[tokio::test]
async fn search2_uses_file_shapes() {
    let body = get("search2", &[("query", "Testers")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("<searchResult2"), "{text}");
    assert!(text.contains("The Testers"), "{text}");
}

// --- cover art ---

#[tokio::test]
async fn cover_art_album_hit_has_immutable_cache() {
    let body = get("getCoverArt", &[("id", "al-rg-1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "image/jpeg");
    assert_eq!(
        body.header("Cache-Control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(body.body, b"COVER-500");
}

#[tokio::test]
async fn cover_art_size_buckets() {
    let body = get("getCoverArt", &[("id", "al-rg-1"), ("size", "100")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "image/jpeg");
    assert_eq!(body.body, b"COVER-250");
    let body = get("getCoverArt", &[("id", "al-rg-1"), ("size", "2000")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "image/jpeg");
    assert_eq!(body.body, b"COVER-1200");
}

#[tokio::test]
async fn cover_art_track_uses_release_group() {
    let body = get("getCoverArt", &[("id", "tr-f1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "image/jpeg");
    assert_eq!(body.body, b"COVER-500");
}

#[tokio::test]
async fn cover_art_artist_hit() {
    let body = get("getCoverArt", &[("id", "ar-artist-1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "image/jpeg");
    assert_eq!(body.body, b"ARTIST-500");
}

#[tokio::test]
async fn cover_art_miss_serves_placeholder() {
    let body = get("getCoverArt", &[("id", "al-rg-2")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "image/svg+xml");
    assert_eq!(body.header("Cache-Control"), Some("public, max-age=86400"));
    assert!(body.body_text().contains("<svg"), "{}", body.body_text());
}

#[tokio::test]
async fn cover_art_playlist_hit() {
    let body = get("getCoverArt", &[("id", "pl-p1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "image/jpeg");
    assert_eq!(body.header("Cache-Control"), Some("private, max-age=3600"));
    assert_eq!(body.body, b"PLAYLIST-COVER");
}

#[tokio::test]
async fn cover_art_unknown_album_is_404_text() {
    let body = get("getCoverArt", &[("id", "al-nope")]).await;
    assert_eq!(body.status, 404);
    assert_eq!(body.content_type, "text/plain");
}

#[tokio::test]
async fn cover_art_unknown_prefix_is_404_text() {
    let body = get("getCoverArt", &[("id", "xx-1")]).await;
    assert_eq!(body.status, 404);
    assert_eq!(body.content_type, "text/plain");
}

// --- stream / download ---

#[tokio::test]
async fn stream_full_body() {
    let body = get("stream", &[("id", "tr-f1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.header("Content-Length"), Some("100"));
    assert_eq!(body.header("Accept-Ranges"), Some("bytes"));
    assert_eq!(body.header("Content-Encoding"), Some("identity"));
    assert_eq!(body.body, (0..100u8).collect::<Vec<_>>());
}

#[tokio::test]
async fn stream_range_variants_return_206() {
    let mut req = request("stream", authed(&[("id", "tr-f1")]));
    req.headers
        .insert("Range".to_owned(), "bytes=10-19".to_owned());
    let body = get_with(&settings(), &FakeStore::loaded(), "stream", req).await;
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.status, 206);
    assert_eq!(body.header("Content-Range"), Some("bytes 10-19/100"));
    assert_eq!(body.body, (10..20u8).collect::<Vec<_>>());

    let mut req = request("stream", authed(&[("id", "tr-f1")]));
    req.headers
        .insert("Range".to_owned(), "bytes=90-".to_owned());
    let body = get_with(&settings(), &FakeStore::loaded(), "stream", req).await;
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.status, 206);
    assert_eq!(body.header("Content-Range"), Some("bytes 90-99/100"));

    let mut req = request("stream", authed(&[("id", "tr-f1")]));
    req.headers
        .insert("Range".to_owned(), "bytes=-10".to_owned());
    let body = get_with(&settings(), &FakeStore::loaded(), "stream", req).await;
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.status, 206);
    assert_eq!(body.header("Content-Range"), Some("bytes 90-99/100"));
    assert_eq!(body.body, (90..100u8).collect::<Vec<_>>());
}

#[tokio::test]
async fn stream_bad_ranges_return_416_without_body() {
    for range in [
        "bytes=100-200",
        "bytes=-0",
        "bytes=0-1,2-3",
        "bytes=abc",
        "items=0-1",
        "bytes=",
    ] {
        let mut req = request("stream", authed(&[("id", "tr-f1")]));
        req.headers.insert("Range".to_owned(), range.to_owned());
        let body = get_with(&settings(), &FakeStore::loaded(), "stream", req).await;
        assert_eq!(body.content_type, "application/octet-stream");
        assert_eq!(body.status, 416, "range {range}");
        assert_eq!(
            body.header("Content-Range"),
            Some("bytes */100"),
            "range {range}"
        );
        assert!(body.body.is_empty(), "range {range}");
    }
}

#[tokio::test]
async fn stream_head_returns_headers_only() {
    let mut req = request("stream", authed(&[("id", "tr-f1")]));
    req.method = "HEAD".to_owned();
    let body = get_with(&settings(), &FakeStore::loaded(), "stream", req).await;
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.status, 200);
    assert_eq!(body.header("Content-Length"), Some("100"));
    assert!(body.body.is_empty());
}

#[tokio::test]
async fn stream_head_honors_range_like_get() {
    // Brief 4: HEAD answers the same status + headers GET would (206 +
    // Content-Range on a slice, 416 past the end), never a body.
    for (range, status, content_range, len) in [
        ("bytes=10-19", 206, "bytes 10-19/100", "10"),
        ("bytes=100-", 416, "bytes */100", ""),
    ] {
        let mut req = request("stream", authed(&[("id", "tr-f1")]));
        req.method = "HEAD".to_owned();
        req.headers.insert("Range".to_owned(), range.to_owned());
        let body = get_with(&settings(), &FakeStore::loaded(), "stream", req).await;
        assert_eq!(body.status, status, "range {range}");
        assert_eq!(
            body.header("Content-Range"),
            Some(content_range),
            "range {range}"
        );
        if status == 206 {
            assert_eq!(body.header("Content-Length"), Some(len), "range {range}");
            assert_eq!(body.content_type, "audio/mpeg", "range {range}");
        } else {
            assert_eq!(
                body.content_type, "application/octet-stream",
                "range {range}"
            );
        }
        assert!(body.body.is_empty(), "range {range}");
    }
}

#[tokio::test]
async fn stream_transcode_head_skips_the_pipe() {
    // m10: HEAD on a transcode answers headers only without running the
    // backend pipe (a panicking backend still passes).
    use droppedneedle::compat::subsonic::stream::{
        AudioBackend, AudioFacts, BackendError, StreamPlan,
    };

    #[derive(Clone)]
    struct NoPipe;
    impl AudioBackend for NoPipe {
        async fn audio_facts(&self, file_id: &str) -> Result<Option<AudioFacts>, BackendError> {
            FakeAudio.audio_facts(file_id).await
        }
        async fn read_range(
            &self,
            file_id: &str,
            start: u64,
            end: u64,
        ) -> Result<Vec<u8>, BackendError> {
            FakeAudio.read_range(file_id, start, end).await
        }
        async fn transcode(
            &self,
            _file_id: &str,
            _plan: &StreamPlan,
        ) -> Result<(Vec<u8>, String), BackendError> {
            panic!("HEAD must not run the transcode pipe");
        }
    }

    let mut req = request("stream", authed(&[("id", "tr-f1"), ("format", "opus")]));
    req.method = "HEAD".to_owned();
    let body = dispatch(
        &FakeVerifier,
        &FakeStore::loaded(),
        &NoPipe,
        &transcoding_settings(),
        &req,
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "audio/ogg");
    assert_eq!(body.header("Accept-Ranges"), Some("none"));
    assert_eq!(body.header("Cache-Control"), Some("no-store"));
    assert!(body.body.is_empty());
}

#[tokio::test]
async fn stream_missing_track_is_404_text() {
    let body = get("stream", &[("id", "tr-nope")]).await;
    assert_eq!(body.status, 404);
    assert_eq!(body.content_type, "text/plain");
}

#[tokio::test]
async fn stream_transcodes_on_codec_trigger() {
    let body = get_with(
        &transcoding_settings(),
        &FakeStore::loaded(),
        "stream",
        request("stream", authed(&[("id", "tr-f1"), ("format", "opus")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "audio/ogg");
    assert_eq!(body.header("Accept-Ranges"), Some("none"));
    assert_eq!(body.header("Cache-Control"), Some("no-store"));
    assert_eq!(body.body, b"TRANSCODED");
}

#[tokio::test]
async fn stream_estimate_adds_length_on_transcode_only() {
    let body = get_with(
        &transcoding_settings(),
        &FakeStore::loaded(),
        "stream",
        request(
            "stream",
            authed(&[
                ("id", "tr-f2"),
                ("maxBitRate", "128"),
                ("estimateContentLength", "true"),
            ]),
        ),
    )
    .await;
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.status, 200);
    assert_eq!(body.body, b"TRANSCODED");
    assert_eq!(body.header("Content-Length"), Some("3200000"));
}

#[tokio::test]
async fn download_sets_attachment_filename() {
    let body = get("download", &[("id", "tr-f1")]).await;
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.status, 200);
    assert_eq!(
        body.header("Content-Disposition"),
        Some("attachment; filename=\"Song One.mp3\"")
    );
    assert_eq!(body.body.len(), 100);
}

#[test]
fn download_filename_sanitizes() {
    assert_eq!(
        droppedneedle::compat::subsonic::stream::download_filename("a/b:c*d", "MP3"),
        "a_b_c_d.mp3"
    );
    assert_eq!(
        droppedneedle::compat::subsonic::stream::download_filename("...", "flac"),
        "track.flac"
    );
}

#[test]
fn decide_cases() {
    use droppedneedle::compat::subsonic::stream::decide;
    // No trigger, no transcode even with a server ceiling present.
    let plan = decide(
        "flac",
        Some(900),
        None,
        None,
        false,
        0.0,
        true,
        true,
        "mp3",
        320,
    );
    assert!(!plan.transcode);
    // Feishin bitrate-0 (#464): zero means unset, not a trigger.
    let plan = decide(
        "flac",
        Some(900),
        None,
        Some(0),
        false,
        0.0,
        true,
        true,
        "mp3",
        320,
    );
    assert!(!plan.transcode);
    // Client ceiling under source bitrate triggers.
    let plan = decide(
        "flac",
        Some(900),
        None,
        Some(128),
        false,
        0.0,
        true,
        true,
        "mp3",
        320,
    );
    assert!(plan.transcode);
    assert_eq!(plan.out_format.as_deref(), Some("mp3"));
    assert_eq!(plan.out_bitrate_kbps, Some(128));
    // Codec mismatch triggers; unknown requests fall back to the default.
    let plan = decide(
        "mp3",
        Some(320),
        Some("opus"),
        None,
        false,
        0.0,
        true,
        true,
        "mp3",
        320,
    );
    assert!(plan.transcode);
    assert_eq!(plan.out_format.as_deref(), Some("opus"));
    // Same-format request is not a mismatch.
    let plan = decide(
        "mp3",
        Some(320),
        Some("mp3"),
        None,
        false,
        0.0,
        true,
        true,
        "mp3",
        320,
    );
    assert!(!plan.transcode);
    // force-original / disabled / no ffmpeg always direct.
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

#[test]
fn cover_buckets() {
    assert_eq!(
        droppedneedle::compat::subsonic::stream::cover_bucket(None),
        "500"
    );
    assert_eq!(
        droppedneedle::compat::subsonic::stream::cover_bucket(Some(300)),
        "250"
    );
    assert_eq!(
        droppedneedle::compat::subsonic::stream::cover_bucket(Some(301)),
        "500"
    );
    assert_eq!(
        droppedneedle::compat::subsonic::stream::cover_bucket(Some(751)),
        "1200"
    );
}

#[test]
fn id_round_trips() {
    use droppedneedle::compat::subsonic::ids::{IdKind, decode, decode_expect, encode};
    assert_eq!(encode(IdKind::Track, "f1"), "tr-f1");
    assert_eq!(decode("ar-x").unwrap(), (IdKind::Artist, "x".to_owned()));
    assert!(decode("xx-1").is_err());
    assert_eq!(decode("xx-1").unwrap_err().code, 70);
    assert!(decode_expect("tr-f1", IdKind::Album).is_err());
}

// --- transcode decision / stream ---

fn decision_request(pairs: &[(&str, &str)], body: &[u8]) -> Request {
    let mut req = request("getTranscodeDecision", authed(pairs));
    req.method = "POST".to_owned();
    req.content_type = Some("application/json".to_owned());
    req.body = body.to_vec();
    req
}

#[tokio::test]
async fn transcode_decision_ok() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "getTranscodeDecision",
        decision_request(
            &[("mediaId", "tr-f2"), ("mediaType", "song")],
            br#"{"name":"Feishin","platform":"web","maxAudioBitrate":0}"#,
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("<transcodeDecision"), "{text}");
    assert!(text.contains("transcodeParams=\"signed-params\""), "{text}");
    assert!(text.contains("canDirectPlay=\"true\""), "{text}");
}

#[tokio::test]
async fn transcode_decision_rejects_get_and_forms_and_unknown_fields() {
    let body = get(
        "getTranscodeDecision",
        &[("mediaId", "tr-f2"), ("mediaType", "song")],
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("requires POST"),
        "{}",
        body.body_text()
    );
    let mut req = decision_request(&[("mediaId", "tr-f2"), ("mediaType", "song")], b"{}");
    req.content_type = Some("application/x-www-form-urlencoded".to_owned());
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "getTranscodeDecision",
        req,
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("requires JSON"),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "getTranscodeDecision",
        decision_request(
            &[("mediaId", "tr-f2"), ("mediaType", "song")],
            br#"{"name":"x","platform":"y","bogus":1}"#,
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn transcode_decision_song_only_and_track_must_exist() {
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "getTranscodeDecision",
        decision_request(
            &[("mediaId", "tr-f2"), ("mediaType", "podcast")],
            br#"{"name":"x","platform":"y"}"#,
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("Only song"),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings(),
        &FakeStore::loaded(),
        "getTranscodeDecision",
        decision_request(
            &[("mediaId", "tr-nope"), ("mediaType", "song")],
            br#"{"name":"x","platform":"y"}"#,
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn transcode_stream_direct_params_serve_original() {
    let body = get(
        "getTranscodeStream",
        &[
            ("mediaId", "tr-f1"),
            ("mediaType", "song"),
            ("transcodeParams", "direct-params"),
        ],
    )
    .await;
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.status, 200);
    assert_eq!(body.body.len(), 100);
}

#[tokio::test]
async fn transcode_stream_signed_params_transcode() {
    let body = get_with(
        &transcoding_settings(),
        &FakeStore::loaded(),
        "getTranscodeStream",
        request(
            "getTranscodeStream",
            authed(&[
                ("mediaId", "tr-f1"),
                ("mediaType", "song"),
                ("transcodeParams", "signed-params"),
            ]),
        ),
    )
    .await;
    assert_eq!(body.content_type, "audio/mpeg");
    assert_eq!(body.status, 200);
    assert_eq!(body.body, b"TRANSCODED");
}

#[tokio::test]
async fn transcode_stream_missing_params_is_enveloped_10() {
    let body = get(
        "getTranscodeStream",
        &[("mediaId", "tr-f1"), ("mediaType", "song")],
    )
    .await;
    assert_eq!(body.content_type, "application/xml");
    assert_eq!(body.status, 200);
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn transcode_stream_disabled_is_404_text() {
    // Code 0 is outside AUTH_CODES, so the binary path renders text (v2 parity).
    let body = get(
        "getTranscodeStream",
        &[
            ("mediaId", "tr-f1"),
            ("mediaType", "song"),
            ("transcodeParams", "signed-params"),
        ],
    )
    .await;
    assert_eq!(body.status, 404);
    assert_eq!(body.content_type, "text/plain");
    assert!(
        body.body_text().contains("Transcoding is disabled"),
        "{}",
        body.body_text()
    );
}

// --- avatar ---

#[tokio::test]
async fn avatar_self_serves_bytes() {
    let body = get("getAvatar", &[("username", "user")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "image/png");
    assert_eq!(body.body, b"AVATAR-BYTES");
}

#[tokio::test]
async fn avatar_non_self_is_403_as_text() {
    let body = get("getAvatar", &[("username", "someone-else")]).await;
    assert_eq!(body.status, 403);
    assert_eq!(body.content_type, "text/plain");
    assert_eq!(
        body.body_text(),
        "Avatar access is limited to the authenticated user"
    );
}

#[tokio::test]
async fn avatar_auth_codes_stay_enveloped_on_binary() {
    assert!(droppedneedle::compat::subsonic::dispatch_uses_envelope(
        50, "stream"
    ));
    assert!(droppedneedle::compat::subsonic::dispatch_uses_envelope(
        40,
        "getavatar"
    ));
    assert!(!droppedneedle::compat::subsonic::dispatch_uses_envelope(
        70, "stream"
    ));
    assert!(droppedneedle::compat::subsonic::dispatch_uses_envelope(
        70, "getsong"
    ));
}

// --- playlists ---

#[tokio::test]
async fn playlists_streamable_counts_skip_legacy() {
    let body = get("getPlaylists", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("songCount=\"2\""), "{text}");
    assert!(text.contains("duration=\"380\""), "{text}");
    assert!(text.contains("coverArt=\"pl-p1\""), "{text}");
}

#[tokio::test]
async fn playlist_detail_serves_streamable_only() {
    let body = get("getPlaylist", &[("id", "pl-p1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("songCount=\"2\""), "{text}");
    assert!(text.contains("Song One"), "{text}");
    assert!(text.contains("Ballad"), "{text}");
    assert!(text.contains("owner=\"user\""), "{text}");
}

#[tokio::test]
async fn playlist_missing_is_70() {
    let body = get("getPlaylist", &[("id", "pl-nope")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn create_playlist_then_replace_with_id() {
    let store = FakeStore::loaded();
    let body = get_with(
        &settings(),
        &store,
        "createPlaylist",
        request(
            "createPlaylist",
            authed(&[("name", "New"), ("songId", "tr-f1")]),
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("name=\"New\""),
        "{}",
        body.body_text()
    );
    assert!(
        body.body_text().contains("songCount=\"1\""),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings(),
        &store,
        "createPlaylist",
        request(
            "createPlaylist",
            authed(&[("playlistId", "pl-p1"), ("songId", "tr-f3")]),
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("songCount=\"1\""), "{text}");
    assert!(text.contains("Ballad"), "{text}");
    assert!(!text.contains("Song One"), "{text}");
}

#[tokio::test]
async fn create_playlist_needs_name_without_id() {
    let body = get("createPlaylist", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn update_playlist_rename_and_remove_by_index() {
    let store = FakeStore::loaded();
    let body = get_with(
        &settings(),
        &store,
        "updatePlaylist",
        request(
            "updatePlaylist",
            authed(&[
                ("playlistId", "pl-p1"),
                ("name", "Renamed"),
                ("songIndexToRemove", "0"),
            ]),
        ),
    )
    .await;
    brief_ok(&body, "updatePlaylist");
    let body = get_with(
        &settings(),
        &store,
        "getPlaylist",
        request("getPlaylist", authed(&[("id", "pl-p1")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("name=\"Renamed\""), "{text}");
    assert!(text.contains("songCount=\"1\""), "{text}");
}

#[tokio::test]
async fn delete_playlist_removes_it() {
    let store = FakeStore::loaded();
    let body = get_with(
        &settings(),
        &store,
        "deletePlaylist",
        request("deletePlaylist", authed(&[("id", "pl-p1")])),
    )
    .await;
    brief_ok(&body, "deletePlaylist");
    let body = get_with(
        &settings(),
        &store,
        "getPlaylist",
        request("getPlaylist", authed(&[("id", "pl-p1")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

// --- favorites + rating ---

#[tokio::test]
async fn star_and_unstar_round_trip() {
    let store = FakeStore::loaded();
    let body = get_with(
        &settings(),
        &store,
        "star",
        request("star", authed(&[("id", "tr-f2"), ("albumId", "al-rg-1")])),
    )
    .await;
    brief_ok(&body, "star");
    let body = get_with(
        &settings(),
        &store,
        "getStarred2",
        request("getStarred2", authed(&[])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("Song Two"), "{text}");
    assert!(text.contains("First Album"), "{text}");
    let body = get_with(
        &settings(),
        &store,
        "unstar",
        request("unstar", authed(&[("id", "tr-f2")])),
    )
    .await;
    brief_ok(&body, "unstar");
}

#[tokio::test]
async fn star_empty_is_10_and_unknown_is_70() {
    let body = get("star", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
    let body = get("star", &[("id", "tr-nope")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn starred_file_shape() {
    let body = get("getStarred", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("<starred"), "{text}");
    assert!(text.contains("Song One"), "{text}");
}

#[tokio::test]
async fn set_rating_validates_then_noops() {
    let body = get("setRating", &[("id", "tr-f1"), ("rating", "4")]).await;
    brief_ok(&body, "setRating");
    let body = get("setRating", &[("id", "tr-f1"), ("rating", "6")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
    let body = get("setRating", &[("id", "tr-nope"), ("rating", "3")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
    let body = get("setRating", &[("id", "tr-f1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

// --- scrobble + now playing + playback reports ---

#[tokio::test]
async fn scrobble_ok_and_timestamp_mismatch_is_0() {
    let body = get("scrobble", &[("id", "tr-f1"), ("time", "1700000000000")]).await;
    brief_ok(&body, "scrobble");
    let body = get(
        "scrobble",
        &[("id", "tr-f1"), ("id", "tr-f2"), ("time", "1")],
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("code=\"0\""), "{text}");
    assert!(text.contains("Wrong number of timestamps"), "{text}");
}

#[tokio::test]
async fn scrobble_without_submission_is_now_playing_only() {
    let body = get("scrobble", &[("id", "tr-f1"), ("submission", "false")]).await;
    brief_ok(&body, "scrobble np");
}

#[tokio::test]
async fn scrobble_needs_id() {
    let body = get("scrobble", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn now_playing_pins_attribution() {
    let body = get("getNowPlaying", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("username=\"user Display\""), "{text}");
    assert!(text.contains("minutesAgo=\"2\""), "{text}");
    assert!(text.contains("playerId=\"0\""), "{text}");
    assert!(text.contains("playerName=\"Symfonium\""), "{text}");
    assert!(text.contains("Song Two"), "{text}");
}

#[tokio::test]
async fn report_playback_ok_and_validated() {
    let body = get(
        "reportPlayback",
        &[
            ("mediaId", "tr-f1"),
            ("mediaType", "song"),
            ("positionMs", "1000"),
            ("state", "playing"),
        ],
    )
    .await;
    brief_ok(&body, "reportPlayback");
    let body = get(
        "reportPlayback",
        &[
            ("mediaId", "tr-f1"),
            ("mediaType", "song"),
            ("positionMs", "1"),
        ],
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

// --- queues ---

#[tokio::test]
async fn play_queue_round_trip() {
    let body = get("getPlayQueue", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("current=\"tr-f1\""), "{text}");
    assert!(text.contains("position=\"1000\""), "{text}");
    assert!(text.contains("changedBy=\"golden\""), "{text}");
}

#[tokio::test]
async fn save_play_queue_enforces_current_rules() {
    let store = FakeStore::loaded();
    let body = get_with(
        &settings(),
        &store,
        "savePlayQueue",
        request("savePlayQueue", authed(&[("id", "tr-f1"), ("id", "tr-f2")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("current is required"),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings(),
        &store,
        "savePlayQueue",
        request(
            "savePlayQueue",
            authed(&[("id", "tr-f1"), ("current", "tr-f3")]),
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("must reference a queued song"),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings(),
        &store,
        "savePlayQueue",
        request("savePlayQueue", authed(&[("current", "tr-f1")])),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("invalid for an empty play queue"),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings(),
        &store,
        "savePlayQueue",
        request(
            "savePlayQueue",
            authed(&[("id", "tr-f2"), ("current", "tr-f2"), ("position", "5")]),
        ),
    )
    .await;
    brief_ok(&body, "savePlayQueue");
}

#[tokio::test]
async fn play_queue_by_index_round_trip() {
    let body = get("getPlayQueueByIndex", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("currentIndex=\"0\""),
        "{}",
        body.body_text()
    );
    let store = FakeStore::loaded();
    let body = get_with(
        &settings(),
        &store,
        "savePlayQueueByIndex",
        request(
            "savePlayQueueByIndex",
            authed(&[("id", "tr-f1"), ("currentIndex", "4")]),
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("outside the play queue"),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings(),
        &store,
        "savePlayQueueByIndex",
        request(
            "savePlayQueueByIndex",
            authed(&[("id", "tr-f1"), ("currentIndex", "0")]),
        ),
    )
    .await;
    brief_ok(&body, "savePlayQueueByIndex");
}

// --- bookmarks ---

#[tokio::test]
async fn bookmarks_crud() {
    let body = get("getBookmarks", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("position=\"5000\""), "{text}");
    assert!(text.contains("comment=\"chorus\""), "{text}");
    assert!(text.contains("username=\"user\""), "{text}");
    let store = FakeStore::loaded();
    let body = get_with(
        &settings(),
        &store,
        "createBookmark",
        request(
            "createBookmark",
            authed(&[("id", "tr-f2"), ("position", "10")]),
        ),
    )
    .await;
    brief_ok(&body, "createBookmark");
    let body = get_with(
        &settings(),
        &store,
        "createBookmark",
        request(
            "createBookmark",
            authed(&[("id", "tr-nope"), ("position", "10")]),
        ),
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
    let body = get_with(
        &settings(),
        &store,
        "deleteBookmark",
        request("deleteBookmark", authed(&[("id", "tr-f2")])),
    )
    .await;
    brief_ok(&body, "deleteBookmark");
}

// --- info + lyrics + genres ---

#[tokio::test]
async fn artist_info_keys_and_cover_urls() {
    for (endpoint, key) in [
        ("getArtistInfo2", "artistInfo2"),
        ("getArtistInfo", "artistInfo"),
    ] {
        let body = get(endpoint, &[("id", "ar-artist-1")]).await;
        assert_eq!(body.status, 200);
        assert_eq!(body.content_type, "application/xml");
        let text = body.body_text();
        assert!(text.contains(&format!("<{key}")), "{text}");
        assert!(text.contains("musicBrainzId=\"artist-1\""), "{text}");
        assert!(text.contains("smallImageUrl=\"http://localhost/subsonic/rest/getCoverArt?id=ar-artist-1&amp;size=250\""), "{text}");
        assert!(!text.contains("apiKey"), "credential-free urls: {text}");
    }
}

#[tokio::test]
async fn album_info_keys() {
    for (endpoint, key) in [
        ("getAlbumInfo2", "albumInfo2"),
        ("getAlbumInfo", "albumInfo"),
    ] {
        let body = get(endpoint, &[("id", "al-rg-1")]).await;
        assert_eq!(body.status, 200);
        assert_eq!(body.content_type, "application/xml");
        assert!(
            body.body_text().contains(&format!("<{key}")),
            "{}",
            body.body_text()
        );
    }
    let body = get("getAlbumInfo2", &[("id", "al-nope")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn structured_lyrics_pins_required_fields() {
    let body = get("getLyricsBySongId", &[("id", "tr-f1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("lang=\"eng\""), "{text}");
    assert!(text.contains("synced=\"true\""), "{text}");
    assert!(text.contains("First line"), "{text}");
    assert!(text.contains("displayTitle=\"Song One\""), "{text}");
    let body = get("getLyricsBySongId", &[("id", "tr-f2")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        !body.body_text().contains("<structuredLyrics"),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn legacy_lyrics_exact_match_and_empty_miss() {
    let body = get(
        "getLyrics",
        &[("artist", "The Testers"), ("title", "Song One")],
    )
    .await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("First line"), "{text}");
    assert!(text.contains("Second line"), "{text}");
    let body = get("getLyrics", &[("title", "No Such Song")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("<lyrics"), "{text}");
    assert!(!text.contains("First line"), "{text}");
    let body = get("getLyrics", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn genres_counts_are_ints() {
    let body = get("getGenres", &[("f", "json")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/json");
    let text = body.body_text();
    assert!(text.contains("\"songCount\":3"), "{text}");
    assert!(text.contains("\"albumCount\":2"), "{text}");
    let body = get("getSongsByGenre", &[("genre", "Rock"), ("count", "2")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("Song One"),
        "{}",
        body.body_text()
    );
    let body = get("getSongsByGenre", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

// --- user + scan + discovery ---

#[tokio::test]
async fn user_returns_caller_roles() {
    let body = get("getUser", &[("username", "ignored-anyone")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("username=\"user\""), "{text}");
    assert!(text.contains("adminRole=\"false\""), "{text}");
    assert!(text.contains("streamRole=\"true\""), "{text}");
    assert!(text.contains("maxBitRate=\"320\""), "{text}");
    let req = request(
        "getUser",
        params(&[("u", "admin"), ("p", "secret"), ("username", "x")]),
    );
    let body = get_with(&settings(), &FakeStore::loaded(), "getUser", req).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("adminRole=\"true\""),
        "{}",
        body.body_text()
    );
    let body = get("getUser", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"10\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn scan_status_and_admin_start() {
    let body = get("getScanStatus", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("scanning=\"false\""),
        "{}",
        body.body_text()
    );
    let body = get("startScan", &[]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    let text = body.body_text();
    assert!(text.contains("code=\"50\""), "{text}");
    assert!(text.contains("Administrator"), "{text}");
    let store = FakeStore::loaded();
    let req = request("startScan", params(&[("u", "admin"), ("p", "secret")]));
    let body = get_with(&settings(), &store, "startScan", req).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("scanning=\"true\""),
        "{}",
        body.body_text()
    );
}

#[tokio::test]
async fn top_songs_needs_exact_artist() {
    let body = get("getTopSongs", &[("artist", "The Testers")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("Song One"),
        "{}",
        body.body_text()
    );
    let body = get("getTopSongs", &[("artist", "Testers")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}

#[test]
fn param_decoding_bounds() {
    use droppedneedle::compat::subsonic::params::{check_limits, decode_pairs};
    assert_eq!(
        droppedneedle::compat::subsonic::params::MAX_REQUEST_PARAMETER_BYTES,
        64 * 1024
    );
    assert_eq!(decode_pairs(b"a=%").unwrap_err().code, 10);
    assert_eq!(
        decode_pairs(b"a=1&b=x+y&q=%22%22").unwrap(),
        vec![
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "x y".to_owned()),
            ("q".to_owned(), "\"\"".to_owned()),
        ]
    );
    assert_eq!(decode_pairs(b"a=%zz").unwrap_err().code, 10);
    assert_eq!(decode_pairs(b"a=%2").unwrap_err().code, 10);
    assert_eq!(decode_pairs(&[0xff]).unwrap_err().code, 10);
    assert!(check_limits(&[("a".to_owned(), "b".to_owned())]).is_ok());
    assert_eq!(
        check_limits(&[("".to_owned(), "b".to_owned())])
            .unwrap_err()
            .code,
        10
    );
    assert_eq!(
        check_limits(&[("a".to_owned(), "x".repeat(9000))])
            .unwrap_err()
            .code,
        10
    );
    let many = vec![("a".to_owned(), "b".to_owned()); 2000];
    assert_eq!(check_limits(&many).unwrap_err().code, 10);
}

#[test]
fn seam_helpers_and_caps() {
    assert_eq!(
        droppedneedle::compat::subsonic::views::genre_slug("Rock & Roll!"),
        "rock-roll"
    );
    assert!(droppedneedle::compat::subsonic::stream::is_audio_suffix(
        "FLAC"
    ));
    assert!(!droppedneedle::compat::subsonic::stream::is_audio_suffix(
        "exe"
    ));
    assert_eq!(
        droppedneedle::compat::subsonic::value::DEFAULT_SERVER_NAME,
        "DroppedNeedle"
    );
    use droppedneedle::compat::subsonic::auth::caps;
    assert_eq!(caps::MAX_USERNAME_LENGTH, 256);
    assert_eq!(caps::MAX_AUTH_VALUE_LENGTH, 1024);
    assert_eq!(caps::MAX_ENCODED_PASSWORD_LENGTH, 2052);
    assert_eq!(caps::MAX_SALT_LENGTH, 128);
    assert_eq!(caps::MAX_TOKEN_LENGTH, 128);
    assert_eq!(caps::MAX_CLIENT_NAME_LENGTH, 256);
    let full = droppedneedle::compat::subsonic::stream::BackendError::full();
    assert!(full.is_full());
    assert!(!droppedneedle::compat::subsonic::stream::BackendError::failed("x").is_full());
}

#[tokio::test]
async fn audio_facts_carry_bitrate_and_duration() {
    use droppedneedle::compat::subsonic::fake::FakeAudio;
    use droppedneedle::compat::subsonic::stream::AudioBackend;
    let facts = FakeAudio.audio_facts("f2").await.unwrap().unwrap();
    assert_eq!(facts.bitrate_kbps, Some(900));
    assert_eq!(facts.duration_seconds, Some(200.0));
    assert!(FakeAudio.audio_facts("nope").await.unwrap().is_none());
}

#[test]
fn client_info_profiles_parse_strictly() {
    let parsed = droppedneedle::compat::subsonic::media::parse_client_info(
        "POST",
        Some("application/json"),
        br#"{"name":"n","platform":"p","directPlayProfiles":[{"containers":["mp3"],"audioCodecs":["mp3"],"protocols":["http"],"maxAudioChannels":2}],"transcodingProfiles":[{"container":"mp3","audioCodec":"mp3","protocol":"http"}],"codecProfiles":[{"type":"Audio","name":"mp3","limitations":[{"name":"b","comparison":"<=", "values":["128"],"required":true}]}]}"#,
    )
    .unwrap();
    assert_eq!(parsed.direct_play_profiles[0].containers, vec!["mp3"]);
    assert_eq!(parsed.direct_play_profiles[0].audio_codecs, vec!["mp3"]);
    assert_eq!(parsed.direct_play_profiles[0].protocols, vec!["http"]);
    assert_eq!(parsed.direct_play_profiles[0].max_audio_channels, Some(2));
    assert_eq!(parsed.name, "n");
    assert_eq!(parsed.platform, "p");
    assert_eq!(parsed.max_audio_bitrate, None);
    assert_eq!(parsed.max_transcoding_audio_bitrate, None);
    assert_eq!(parsed.transcoding_profiles[0].container, "mp3");
    assert_eq!(parsed.transcoding_profiles[0].audio_codec, "mp3");
    assert_eq!(parsed.transcoding_profiles[0].protocol, "http");
    assert_eq!(parsed.transcoding_profiles[0].max_audio_channels, None);
    assert_eq!(parsed.codec_profiles[0].profile_type, "Audio");
    assert_eq!(parsed.codec_profiles[0].name, "mp3");
    assert_eq!(parsed.codec_profiles[0].limitations[0].name, "b");
    assert_eq!(parsed.codec_profiles[0].limitations[0].comparison, "<=");
    assert_eq!(parsed.codec_profiles[0].limitations[0].values, vec!["128"]);
    assert!(parsed.codec_profiles[0].limitations[0].required);
}

#[tokio::test]
async fn similar_songs_both_spellings() {
    for endpoint in ["getSimilarSongs2", "getSimilarSongs"] {
        let body = get(endpoint, &[("id", "ar-artist-1")]).await;
        assert_eq!(body.status, 200);
        assert_eq!(body.content_type, "application/xml");
        assert!(
            body.body_text().contains("Song One"),
            "{endpoint}: {}",
            body.body_text()
        );
    }
    let body = get("getSimilarSongs", &[("id", "ar-nope")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
    let body = get("getSimilarSongs", &[("id", "tr-f1")]).await;
    assert_eq!(body.status, 200);
    assert_eq!(body.content_type, "application/xml");
    assert!(
        body.body_text().contains("code=\"70\""),
        "{}",
        body.body_text()
    );
}
