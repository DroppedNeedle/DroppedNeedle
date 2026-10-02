//! Jellyfin compat goldens: one brief per matrixed-client row (Finamp,
//! Jellify, Manet) plus the shared wire contract.
//!
//! GOLDEN FORMAT (brief-first; this inline `Golden` harness IS the golden
//! shape — there is no on-disk corpus): each brief pins the status, a
//! header sidecar, and the body. Headers assert byte-exact per entry.
//! Bodies assert either as exact wire bytes (`BodyExp::Exact`) or as
//! parsed JSON with exact key sets (`BodyExp::Json`, NOT wire bytes: key
//! order and spacing are not pinned), where the few nondeterministic
//! values (session ids, activity timestamps, `.NET "O"` dates) assert by
//! shape via `__UUID__`/`__ISO_PY__`/`__ISO_O__`/`__ANY__` placeholders.
//! Time travels only through seeded library dates; ids are the
//! deterministic `sha256("kind:internal")[:32]` derivation.
//!
use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use droppedneedle::auth::compat_auth::fakes::FakeCompatPasswords;
use droppedneedle::auth::compat_auth::jellyfin::server_id;
use droppedneedle::compat::jellyfin::seams::{
    AlbumView, ArtistView, GenreView, IdMap, JellyfinSettings, MemoryEngine, MemoryIds,
    MemoryLibrary, MemorySessions, SessionCall, StreamEngine, TrackView,
};
use droppedneedle::compat::jellyfin::{JellyfinState, router};
use tower::ServiceExt as _;

// ===== Golden harness =====

/// Body assertion: exact wire bytes, or parsed JSON (NOT wire bytes:
/// key order and spacing are not pinned) with `__UUID__`,
/// `__ISO_PY__`, `__ISO_O__`, `__ANY__` placeholders and exact key sets.
enum BodyExp {
    Exact(&'static [u8]),
    Json(serde_json::Value),
}

/// One golden brief: status + header sidecar (byte-exact per entry) + body.
struct Golden {
    status: u16,
    headers: &'static [(&'static str, &'static str)],
    body: BodyExp,
}

fn is_uuid32(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_iso_py(s: &str) -> bool {
    // `2026-09-28T12:00:00.123456+00:00`
    let b = s.as_bytes();
    b.len() == 32
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'.'
        && s.ends_with("+00:00")
        && b[20..26].iter().all(|c| c.is_ascii_digit())
}

fn is_iso_o(s: &str) -> bool {
    // .NET "O": 7 fractional digits + Z; whole-second ISO is rejected.
    let b = s.as_bytes();
    b.len() == 28
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'.'
        && b[27] == b'Z'
        && b[20..27].iter().all(|c| c.is_ascii_digit())
}

/// Placeholder-aware JSON comparison with exact key sets.
fn assert_json(got: &[u8], expected: &serde_json::Value) {
    let actual: serde_json::Value = serde_json::from_slice(got).expect("response is valid JSON");
    compare(&actual, expected, "$");
}

fn compare(actual: &serde_json::Value, expected: &serde_json::Value, path: &str) {
    use serde_json::Value as V;
    if let V::String(exp) = expected {
        if let V::String(got) = actual {
            match exp.as_str() {
                "__UUID__" => assert!(is_uuid32(got), "{path}: {got} is not uuid32"),
                "__ISO_PY__" => assert!(is_iso_py(got), "{path}: {got} is not iso-py"),
                "__ISO_O__" => assert!(is_iso_o(got), "{path}: {got} is not .NET O"),
                "__ANY__" => {}
                _ => assert_eq!(actual, expected, "{path}"),
            }
            return;
        }
        if exp == "__ANY__" {
            return;
        }
    }
    match (actual, expected) {
        (V::Object(a), V::Object(e)) => {
            let mut ak: Vec<&str> = a.keys().map(String::as_str).collect();
            let mut ek: Vec<&str> = e.keys().map(String::as_str).collect();
            ak.sort();
            ek.sort();
            assert_eq!(ak, ek, "{path}: key sets differ");
            for key in ek {
                compare(&a[key], &e[key], &format!("{path}.{key}"));
            }
        }
        (V::Array(a), V::Array(e)) => {
            assert_eq!(a.len(), e.len(), "{path}: array lengths differ");
            for (i, (x, y)) in a.iter().zip(e.iter()).enumerate() {
                compare(x, y, &format!("{path}[{i}]"));
            }
        }
        _ => assert_eq!(actual, expected, "{path}"),
    }
}

async fn check(response: Response, golden: &Golden) -> Vec<u8> {
    assert_eq!(response.status().as_u16(), golden.status, "status");
    for (name, expected) in golden.headers {
        let got = response
            .headers()
            .get(*name)
            .unwrap_or_else(|| panic!("missing header {name}"))
            .to_str()
            .expect("header is ascii");
        assert_eq!(got, *expected, "header {name}");
    }
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    match &golden.body {
        BodyExp::Exact(exp) => assert_eq!(&bytes[..], *exp, "body bytes"),
        BodyExp::Json(exp) => assert_json(&bytes, exp),
    }
    bytes.to_vec()
}

// ===== Fixtures =====

const ALICE: &str = "alice-app-secret";
const BOB: &str = "bob-app-secret";

fn finamp(secret: &str) -> String {
    format!(
        "MediaBrowser Client=\"Finamp\", Device=\"Test\", DeviceId=\"dev1\", Token=\"{secret}\""
    )
}

fn jellify(secret: &str) -> String {
    format!(
        "MediaBrowser Client=\"Jellify\", Device=\"Phone\", DeviceId=\"dev2\", Token=\"{secret}\""
    )
}

struct Fx {
    passwords: FakeCompatPasswords,
    library: MemoryLibrary,
    engine: MemoryEngine,
    sessions: MemorySessions,
    ids: MemoryIds,
    track1: String,
    track2: String,
    track3: String,
    album1: String,
    album2: String,
    artist1: String,
    artist2: String,
    genre_rock: String,
    library_id: String,
}

fn track(id: &str, title: &str, rg: &str, artist: &str, album_artist: &str) -> TrackView {
    TrackView {
        file_id: id.to_owned(),
        title: title.to_owned(),
        rg_mbid: Some(rg.to_owned()),
        artist_mbid: Some(artist.to_owned()),
        album_artist_mbid: Some(album_artist.to_owned()),
        ..TrackView::default()
    }
}

async fn fixture() -> Fx {
    let passwords = FakeCompatPasswords::new();
    passwords.add_user(
        "user-alice",
        "alice",
        "Alice",
        "user",
        "alice-account-pw",
        &[ALICE],
    );
    passwords.add_user("user-bob", "bob", "Bob", "admin", "bob-account-pw", &[BOB]);
    let library = MemoryLibrary::new();
    library.add_artist(
        ArtistView {
            artist_mbid: "mb-a1".to_owned(),
            name: "Blue Giant".to_owned(),
            album_count: 2,
            date_added: Some(900.0),
            starred: false,
        },
        true,
    );
    library.add_artist(
        ArtistView {
            artist_mbid: "mb-a2".to_owned(),
            name: "Guest Star".to_owned(),
            album_count: 0,
            date_added: Some(950.0),
            starred: false,
        },
        false,
    );
    library.add_album(AlbumView {
        rg_mbid: "rg-1".to_owned(),
        title: "First Light".to_owned(),
        artist_name: Some("Blue Giant".to_owned()),
        artist_mbid: Some("mb-a1".to_owned()),
        year: Some(2021),
        genre: Some("Rock".to_owned()),
        track_count: 2,
        total_duration_seconds: Some(380.0),
        date_added: Some(1000.0),
        starred: false,
        play_count: 5,
        last_played: Some(5000.0),
    });
    library.add_album(AlbumView {
        rg_mbid: "rg-2".to_owned(),
        title: "Second Dawn".to_owned(),
        artist_name: Some("Blue Giant".to_owned()),
        artist_mbid: Some("mb-a1".to_owned()),
        year: Some(2023),
        genre: Some("Rock".to_owned()),
        track_count: 1,
        total_duration_seconds: Some(240.0),
        date_added: Some(2000.0),
        starred: false,
        play_count: 0,
        last_played: None,
    });
    let mut t1 = track("f1", "Opener", "rg-1", "mb-a1", "mb-a1");
    t1.album_title = Some("First Light".to_owned());
    t1.artist_name = Some("Blue Giant".to_owned());
    t1.album_artist_name = Some("Blue Giant".to_owned());
    t1.duration_seconds = Some(180.0);
    t1.year = Some(2021);
    t1.track_number = Some(1);
    t1.disc_number = Some(1);
    t1.genre = Some("Rock".to_owned());
    t1.file_format = Some("mp3".to_owned());
    t1.bitrate = Some(320);
    t1.channels = Some(2);
    t1.sample_rate = Some(44100);
    t1.file_size_bytes = Some(1024);
    t1.created_at = Some(1000.0);
    t1.recording_mbid = Some("rec-1".to_owned());
    t1.play_count = 5;
    t1.last_played = Some(5000.0);
    library.add_track(t1);
    let mut t2 = track("f2", "Deep Cut", "rg-1", "mb-a1", "mb-a1");
    t2.album_title = Some("First Light".to_owned());
    t2.artist_name = Some("Blue Giant".to_owned());
    t2.album_artist_name = Some("Blue Giant".to_owned());
    t2.duration_seconds = Some(200.0);
    t2.year = Some(2021);
    t2.track_number = Some(2);
    t2.disc_number = Some(1);
    t2.genre = Some("Rock".to_owned());
    t2.file_format = Some("flac".to_owned());
    t2.bitrate = Some(900);
    t2.file_size_bytes = Some(512);
    t2.created_at = Some(1100.0);
    library.add_track(t2);
    let mut t3 = track("f3", "Late Bloomer", "rg-2", "mb-a2", "mb-a1");
    t3.album_title = Some("Second Dawn".to_owned());
    t3.artist_name = Some("Guest Star".to_owned());
    t3.album_artist_name = Some("Blue Giant".to_owned());
    t3.duration_seconds = Some(240.0);
    t3.year = Some(2023);
    t3.track_number = Some(1);
    t3.disc_number = Some(1);
    t3.genre = Some("Rock".to_owned());
    t3.file_format = Some("mp3".to_owned());
    t3.bitrate = Some(128);
    t3.file_size_bytes = Some(256);
    t3.created_at = Some(2000.0);
    library.add_track(t3);
    library.add_genre(GenreView {
        name: "Rock".to_owned(),
        song_count: 3,
    });
    library.add_cover("rg-1", "500", b"cover-500-bytes".to_vec(), "image/jpeg");
    library.add_cover("rg-1", "250", b"cover-250-bytes".to_vec(), "image/jpeg");
    library.add_artist_image("mb-a1", b"artist-bytes".to_vec(), "image/jpeg");

    let engine = MemoryEngine::new();
    let bytes1: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
    engine.add_file("f1", bytes1, Some("mp3"));
    engine.add_file("f2", vec![7u8; 512], Some("flac"));
    engine.add_file("f3", vec![9u8; 256], Some("mp3"));

    let sessions = MemorySessions::new();
    let ids = MemoryIds::new();
    // Register the reverse map (the derivation is deterministic, so these
    // are also the ids the goldens expect).
    let track1 = ids.to_jf("track", "f1").await;
    let track2 = ids.to_jf("track", "f2").await;
    let track3 = ids.to_jf("track", "f3").await;
    let album1 = ids.to_jf("album", "rg-1").await;
    let album2 = ids.to_jf("album", "rg-2").await;
    let artist1 = ids.to_jf("artist", "mb-a1").await;
    let artist2 = ids.to_jf("artist", "mb-a2").await;
    let genre_rock = ids.to_jf("genre", "rock").await;
    let library_id = ids.to_jf("library", "music").await;
    Fx {
        passwords,
        library,
        engine,
        sessions,
        ids,
        track1,
        track2,
        track3,
        album1,
        album2,
        artist1,
        artist2,
        genre_rock,
        library_id,
    }
}

fn settings() -> JellyfinSettings {
    JellyfinSettings {
        enabled: true,
        ..JellyfinSettings::default()
    }
}

fn app(fx: &Fx, settings: JellyfinSettings) -> Router {
    Router::new().nest(
        "/jellyfin",
        router(JellyfinState::new(
            fx.passwords.clone(),
            fx.library.clone(),
            fx.engine.clone(),
            fx.sessions.clone(),
            fx.ids.clone(),
            settings,
        )),
    )
}

async fn get(app: Router, uri: &str, auth: Option<&str>) -> Response {
    let mut builder = Request::builder().uri(uri);
    if let Some(header) = auth {
        builder = builder.header("authorization", header);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn post(app: Router, uri: &str, auth: Option<&str>, body: &[u8]) -> Response {
    let mut builder = Request::builder().method("POST").uri(uri);
    if let Some(header) = auth {
        builder = builder.header("authorization", header);
    }
    if !body.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    app.oneshot(builder.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap()
}

async fn delete(app: Router, uri: &str, auth: Option<&str>) -> Response {
    let mut builder = Request::builder().method("DELETE").uri(uri);
    if let Some(header) = auth {
        builder = builder.header("authorization", header);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn head(app: Router, uri: &str, auth: Option<&str>, range: Option<&str>) -> Response {
    let mut builder = Request::builder().method("HEAD").uri(uri);
    if let Some(header) = auth {
        builder = builder.header("authorization", header);
    }
    if let Some(range) = range {
        builder = builder.header("range", range);
    }
    app.oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

// ===== System / gates =====

#[tokio::test]
async fn disabled_protocol_is_404_before_handler_lookup() {
    let fx = fixture().await;
    let off = JellyfinSettings::default();
    assert!(!off.enabled);
    let response = get(app(&fx, off.clone()), "/jellyfin/System/Info/Public", None).await;
    check(
        response,
        &Golden {
            status: 404,
            headers: &[],
            body: BodyExp::Exact(b""),
        },
    )
    .await;
    // Authed routes gate identically (no method enumeration when disabled).
    let auth = finamp(ALICE);
    let response = get(app(&fx, off), "/jellyfin/Items", Some(&auth)).await;
    assert_eq!(response.status().as_u16(), 404);
}

#[tokio::test]
async fn public_info_golden() {
    let fx = fixture().await;
    let response = get(app(&fx, settings()), "/jellyfin/System/Info/Public", None).await;
    check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "LocalAddress": "http://localhost/jellyfin",
                "ServerName": "DroppedNeedle",
                "Version": "10.10.6",
                "ProductName": "Jellyfin Server",
                "OperatingSystem": "",
                "Id": server_id(),
                "StartupWizardCompleted": true,
            })),
        },
    )
    .await;
}

#[tokio::test]
async fn system_info_needs_auth_then_golden() {
    let fx = fixture().await;
    let denied = get(app(&fx, settings()), "/jellyfin/System/Info", None).await;
    check(
        denied,
        &Golden {
            status: 401,
            headers: &[],
            body: BodyExp::Exact(b""),
        },
    )
    .await;
    let auth = finamp(ALICE);
    let response = get(app(&fx, settings()), "/jellyfin/System/Info", Some(&auth)).await;
    check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "LocalAddress": "http://localhost/jellyfin",
                "ServerName": "DroppedNeedle",
                "Version": "10.10.6",
                "ProductName": "Jellyfin Server",
                "OperatingSystem": "",
                "Id": server_id(),
                "StartupWizardCompleted": true,
                "HasPendingRestart": false,
                "IsShuttingDown": false,
                "SupportsLibraryMonitor": true,
            })),
        },
    )
    .await;
}

#[tokio::test]
async fn quick_connect_is_literal_false() {
    let fx = fixture().await;
    let response = get(app(&fx, settings()), "/jellyfin/QuickConnect/Enabled", None).await;
    check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Exact(b"false"),
        },
    )
    .await;
}

#[tokio::test]
async fn logout_is_204() {
    let fx = fixture().await;
    let response = post(app(&fx, settings()), "/jellyfin/Sessions/Logout", None, b"").await;
    check(
        response,
        &Golden {
            status: 204,
            headers: &[],
            body: BodyExp::Exact(b""),
        },
    )
    .await;
}

// ===== Auth (Finamp/Manet login row) =====

fn expected_user(name: &str, id: &str, is_admin: bool) -> serde_json::Value {
    serde_json::json!({
        "Id": id,
        "Name": name,
        "ServerId": server_id(),
        "HasPassword": true,
        "HasConfiguredPassword": true,
        "HasConfiguredEasyPassword": false,
        "Configuration": {
            "PlayDefaultAudioTrack": true,
            "DisplayMissingEpisodes": false,
            "GroupedFolders": [],
            "SubtitleMode": "Default",
            "DisplayCollectionsView": false,
            "EnableLocalPassword": false,
            "OrderedViews": [],
            "LatestItemsExcludes": [],
            "MyMediaExcludes": [],
            "HidePlayedInLatest": true,
            "RememberAudioSelections": true,
            "RememberSubtitleSelections": true,
            "EnableNextEpisodeAutoPlay": true,
        },
        "Policy": {
            "IsAdministrator": is_admin,
            "IsHidden": false,
            "IsDisabled": false,
            "EnableAllFolders": true,
            "EnabledFolders": [],
            "EnableAllChannels": true,
            "EnabledChannels": [],
            "EnableAllDevices": true,
            "EnabledDevices": [],
            "EnableMediaPlayback": true,
            "EnableAudioPlaybackTranscoding": true,
            "EnableVideoPlaybackTranscoding": true,
            "EnablePlaybackRemuxing": true,
            "EnableContentDownloading": true,
            "EnableRemoteAccess": true,
            "EnableSyncTranscoding": true,
            "EnableUserPreferenceAccess": true,
            "EnableLiveTvAccess": false,
            "EnableLiveTvManagement": false,
            "EnableContentDeletion": false,
            "EnableMediaConversion": false,
            "EnablePublicSharing": false,
            "EnableRemoteControlOfOtherUsers": false,
            "EnableSharedDeviceControl": false,
            "InvalidLoginAttemptCount": 0,
            "RemoteClientBitrateLimit": 0,
            "SyncPlayAccess": "CreateAndJoinGroups",
            "BlockedTags": [],
            "AllowedTags": [],
            "AccessSchedules": [],
            "BlockUnratedItems": [],
        },
    })
}

#[tokio::test]
async fn finamp_login_echo_is_fully_populated_and_verbatim() {
    let fx = fixture().await;
    // Finamp sends an undocumented `UserId`: lenient decode must not 400.
    let body = serde_json::json!({"Username": "alice", "Pw": ALICE, "UserId": "zzz"}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Users/AuthenticateByName",
        None,
        body.as_bytes(),
    )
    .await;
    let bytes = check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "User": expected_user("alice", "user-alice", false),
                "AccessToken": ALICE,
                "SessionInfo": {
                    "Id": "__UUID__",
                    "UserId": "user-alice",
                    "UserName": "alice",
                    "LastActivityDate": "__ISO_PY__",
                    "DeviceName": "",
                    "IsActive": true,
                    "SupportsRemoteControl": false,
                    "SupportsMediaControl": false,
                    "HasCustomDeviceName": false,
                    "ServerId": server_id(),
                },
                "ServerId": server_id(),
            })),
        },
    )
    .await;
    // The echoed token authenticates: it is the app password verbatim.
    let echo: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(echo["AccessToken"], ALICE);
}

#[tokio::test]
async fn login_carries_client_and_device_facts() {
    let fx = fixture().await;
    // Case-insensitive check happens through the header extractor.
    let auth =
        "MediaBrowser Client=\"Jellify\", Device=\"Pixel\", DeviceId=\"abc\", Token=\"unused\"";
    let body = serde_json::json!({"Username": "bob", "Pw": BOB}).to_string();
    let mut builder = Request::builder()
        .method("POST")
        .uri("/jellyfin/Users/AuthenticateByName")
        .header("authorization", auth)
        .header("content-type", "application/json");
    let _ = &mut builder;
    let response = app(&fx, settings())
        .oneshot(builder.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let bytes = check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "User": expected_user("bob", "user-bob", true),
                "AccessToken": BOB,
                "SessionInfo": {
                    "Id": "__UUID__",
                    "UserId": "user-bob",
                    "UserName": "bob",
                    "LastActivityDate": "__ISO_PY__",
                    "Client": "Jellify",
                    "DeviceName": "Pixel",
                    "DeviceId": "abc",
                    "IsActive": true,
                    "SupportsRemoteControl": false,
                    "SupportsMediaControl": false,
                    "HasCustomDeviceName": false,
                    "ServerId": server_id(),
                },
                "ServerId": server_id(),
            })),
        },
    )
    .await;
    let _ = bytes;
}

#[tokio::test]
async fn login_rejects_bad_credentials_and_empty_body() {
    let fx = fixture().await;
    let bad = serde_json::json!({"Username": "alice", "Pw": "nope"}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Users/AuthenticateByName",
        None,
        bad.as_bytes(),
    )
    .await;
    check(
        response,
        &Golden {
            status: 401,
            headers: &[],
            body: BodyExp::Exact(b""),
        },
    )
    .await;
    // Account passwords never verify on compat routes.
    let account = serde_json::json!({"Username": "alice", "Pw": "alice-account-pw"}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Users/AuthenticateByName",
        None,
        account.as_bytes(),
    )
    .await;
    assert_eq!(response.status().as_u16(), 401);
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Users/AuthenticateByName",
        None,
        b"",
    )
    .await;
    assert_eq!(response.status().as_u16(), 400);
}

#[tokio::test]
async fn users_me_matches_login_user_and_ignores_path_id() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let me = get(app(&fx, settings()), "/jellyfin/Users/Me", Some(&auth)).await;
    let me_bytes = check(
        me,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(expected_user("alice", "user-alice", false)),
        },
    )
    .await;
    // The `{id}` is ignored: any id returns the caller.
    let other = get(
        app(&fx, settings()),
        "/jellyfin/Users/someone-else",
        Some(&auth),
    )
    .await;
    let other_bytes = check(
        other,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(expected_user("alice", "user-alice", false)),
        },
    )
    .await;
    assert_eq!(me_bytes, other_bytes);
    // And it byte-matches the login echo's embedded User object.
    let body = serde_json::json!({"Username": "alice", "Pw": ALICE}).to_string();
    let login = post(
        app(&fx, settings()),
        "/jellyfin/Users/AuthenticateByName",
        None,
        body.as_bytes(),
    )
    .await;
    let login_bytes = axum::body::to_bytes(login.into_body(), 64 * 1024)
        .await
        .unwrap();
    let login_json: serde_json::Value = serde_json::from_slice(&login_bytes).unwrap();
    let me_json: serde_json::Value = serde_json::from_slice(&me_bytes).unwrap();
    assert_eq!(login_json["User"], me_json);
}

// ===== Views / filters (Manet rows) =====

#[tokio::test]
async fn manet_music_view_carries_required_fields() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    for uri in ["/jellyfin/UserViews", "/jellyfin/Users/x/Views"] {
        let response = get(app(&fx, settings()), uri, Some(&auth)).await;
        check(
            response,
            &Golden {
                status: 200,
                headers: &[("content-type", ("application/json"))],
                body: BodyExp::Json(serde_json::json!({
                    "Items": [{
                        "Id": fx.library_id,
                        "Name": "Music",
                        "Type": "CollectionFolder",
                        "ServerId": server_id(),
                        "IsFolder": true,
                        "MediaType": "Unknown",
                        "ImageTags": {"Primary": fx.library_id},
                        "CollectionType": "music",
                        "SortName": "Music",
                        "UserData": {
                            "ItemId": fx.library_id,
                            "Key": fx.library_id,
                            "PlaybackPositionTicks": 0,
                            "PlayCount": 0,
                            "IsFavorite": false,
                            "Played": false,
                        },
                        "LocationType": "FileSystem",
                        "BackdropImageTags": [],
                        "ImageBlurHashes": {},
                    }],
                    "TotalRecordCount": 1,
                    "StartIndex": 0,
                })),
            },
        )
        .await;
    }
}

#[tokio::test]
async fn manet_filters_boot_call_never_404s() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    for uri in ["/jellyfin/Items/Filters", "/jellyfin/Users/x/Items/Filters"] {
        let response = get(app(&fx, settings()), uri, Some(&auth)).await;
        check(
            response,
            &Golden {
                status: 200,
                headers: &[("content-type", ("application/json"))],
                body: BodyExp::Json(serde_json::json!({
                    "Genres": ["Rock"],
                    "Tags": [],
                    "OfficialRatings": [],
                    "Years": [],
                })),
            },
        )
        .await;
    }
}

// ===== Browse =====

#[tokio::test]
async fn browse_defaults_to_albums_with_manet_arrays() {
    let fx = fixture().await;
    let auth = jellify(ALICE);
    let response = get(app(&fx, settings()), "/jellyfin/Items", Some(&auth)).await;
    check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "Items": [
                    {
                        "Id": fx.album1,
                        "Name": "First Light",
                        "Type": "MusicAlbum",
                        "ServerId": server_id(),
                        "IsFolder": true,
                        "MediaType": "Unknown",
                        "RunTimeTicks": 3_800_000_000_i64,
                        "ProductionYear": 2021,
                        "AlbumArtist": "Blue Giant",
                        "AlbumArtists": [{"Name": "Blue Giant", "Id": fx.artist1}],
                        "ArtistItems": [{"Name": "Blue Giant", "Id": fx.artist1}],
                        "Artists": ["Blue Giant"],
                        "ImageTags": {"Primary": "tag-rg-1"},
                        "Genres": ["Rock"],
                        "ChildCount": 2,
                        "SortName": "First Light",
                        "DateCreated": "__ISO_O__",
                        "ProviderIds": {"MusicBrainzReleaseGroup": "rg-1"},
                        "UserData": {
                            "ItemId": fx.album1,
                            "Key": fx.album1,
                            "PlaybackPositionTicks": 0,
                            "PlayCount": 5,
                            "IsFavorite": false,
                            "Played": true,
                        },
                        "LocationType": "FileSystem",
                        "BackdropImageTags": [],
                        "ImageBlurHashes": {},
                    },
                    {
                        "Id": fx.album2,
                        "Name": "Second Dawn",
                        "Type": "MusicAlbum",
                        "ServerId": server_id(),
                        "IsFolder": true,
                        "MediaType": "Unknown",
                        "RunTimeTicks": 2_400_000_000_i64,
                        "ProductionYear": 2023,
                        "AlbumArtist": "Blue Giant",
                        "AlbumArtists": [{"Name": "Blue Giant", "Id": fx.artist1}],
                        "ArtistItems": [{"Name": "Blue Giant", "Id": fx.artist1}],
                        "Artists": ["Blue Giant"],
                        "ImageTags": {},
                        "Genres": ["Rock"],
                        "ChildCount": 1,
                        "SortName": "Second Dawn",
                        "DateCreated": "__ISO_O__",
                        "ProviderIds": {"MusicBrainzReleaseGroup": "rg-2"},
                        "UserData": {
                            "ItemId": fx.album2,
                            "Key": fx.album2,
                            "PlaybackPositionTicks": 0,
                            "PlayCount": 0,
                            "IsFavorite": false,
                            "Played": false,
                        },
                        "LocationType": "FileSystem",
                        "BackdropImageTags": [],
                        "ImageBlurHashes": {},
                    },
                ],
                "TotalRecordCount": 2,
                "StartIndex": 0,
            })),
        },
    )
    .await;
}

#[tokio::test]
async fn manet_date_created_is_dotnet_o_even_for_whole_seconds() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    // rg-1 was added at exactly 1000.0 (whole seconds): the fraction stays.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}", fx.album1),
        Some(&auth),
    )
    .await;
    let bytes = check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "Id": fx.album1,
                "Name": "First Light",
                "Type": "MusicAlbum",
                "ServerId": server_id(),
                "IsFolder": true,
                "MediaType": "Unknown",
                "RunTimeTicks": 3_800_000_000_i64,
                "ProductionYear": 2021,
                "AlbumArtist": "Blue Giant",
                "AlbumArtists": [{"Name": "Blue Giant", "Id": fx.artist1}],
                "ArtistItems": [{"Name": "Blue Giant", "Id": fx.artist1}],
                "Artists": ["Blue Giant"],
                "ImageTags": {"Primary": "tag-rg-1"},
                "Genres": ["Rock"],
                "ChildCount": 2,
                "SortName": "First Light",
                "DateCreated": "__ISO_O__",
                "ProviderIds": {"MusicBrainzReleaseGroup": "rg-1"},
                "UserData": {
                    "ItemId": fx.album1,
                    "Key": fx.album1,
                    "PlaybackPositionTicks": 0,
                    "PlayCount": 5,
                    "IsFavorite": false,
                    "Played": true,
                },
                "LocationType": "FileSystem",
                "BackdropImageTags": [],
                "ImageBlurHashes": {},
            })),
        },
    )
    .await;
    let item: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(item["DateCreated"], "1970-01-01T00:16:40.0000000Z");
}

#[tokio::test]
async fn album_children_are_track_ordered_and_query_is_case_insensitive() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    // Lowercase `parentid` binds (ASP.NET parity).
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items?parentid={}", fx.album1),
        Some(&auth),
    )
    .await;
    let bytes = check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "Items": [
                    {
                        "Id": fx.track1,
                        "Name": "Opener",
                        "Type": "Audio",
                        "ServerId": server_id(),
                        "IsFolder": false,
                        "MediaType": "Audio",
                        "RunTimeTicks": 1_800_000_000_i64,
                        "ProductionYear": 2021,
                        "IndexNumber": 1,
                        "ParentIndexNumber": 1,
                        "Album": "First Light",
                        "AlbumId": fx.album1,
                        "AlbumArtist": "Blue Giant",
                        "AlbumArtists": [{"Name": "Blue Giant", "Id": fx.artist1}],
                        "ArtistItems": [{"Name": "Blue Giant", "Id": fx.artist1}],
                        "Artists": ["Blue Giant"],
                        "AlbumPrimaryImageTag": "tag-rg-1",
                        "ImageTags": {"Primary": "tag-rg-1"},
                        "ParentId": fx.album1,
                        "Genres": ["Rock"],
                        "Container": "mp3",
                        "SortName": "Opener",
                        "DateCreated": "__ISO_O__",
                        "ProviderIds": {"MusicBrainzTrack": "rec-1"},
                        "UserData": {
                            "ItemId": fx.track1,
                            "Key": fx.track1,
                            "PlaybackPositionTicks": 0,
                            "PlayCount": 5,
                            "IsFavorite": false,
                            "Played": true,
                        },
                        "LocationType": "FileSystem",
                        "BackdropImageTags": [],
                        "ImageBlurHashes": {},
                    },
                    {
                        "Id": fx.track2,
                        "Name": "Deep Cut",
                        "Type": "Audio",
                        "ServerId": server_id(),
                        "IsFolder": false,
                        "MediaType": "Audio",
                        "RunTimeTicks": 2_000_000_000_i64,
                        "ProductionYear": 2021,
                        "IndexNumber": 2,
                        "ParentIndexNumber": 1,
                        "Album": "First Light",
                        "AlbumId": fx.album1,
                        "AlbumArtist": "Blue Giant",
                        "AlbumArtists": [{"Name": "Blue Giant", "Id": fx.artist1}],
                        "ArtistItems": [{"Name": "Blue Giant", "Id": fx.artist1}],
                        "Artists": ["Blue Giant"],
                        "AlbumPrimaryImageTag": "tag-rg-1",
                        "ImageTags": {"Primary": "tag-rg-1"},
                        "ParentId": fx.album1,
                        "Genres": ["Rock"],
                        "Container": "flac",
                        "SortName": "Deep Cut",
                        "DateCreated": "__ISO_O__",
                        "UserData": {
                            "ItemId": fx.track2,
                            "Key": fx.track2,
                            "PlaybackPositionTicks": 0,
                            "PlayCount": 0,
                            "IsFavorite": false,
                            "Played": false,
                        },
                        "LocationType": "FileSystem",
                        "BackdropImageTags": [],
                        "ImageBlurHashes": {},
                    },
                ],
                "TotalRecordCount": 2,
                "StartIndex": 0,
            })),
        },
    )
    .await;
    let _ = bytes;
}

#[tokio::test]
async fn include_item_types_priority_and_jellify_premiere_date_sort() {
    let fx = fixture().await;
    let auth = jellify(ALICE);
    // MusicArtist outranks MusicAlbum.
    let response = get(
        app(&fx, settings()),
        "/jellyfin/Items?IncludeItemTypes=MusicAlbum,MusicArtist",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["TotalRecordCount"], 2);
    assert_eq!(parsed["Items"][0]["Type"], "MusicArtist");
    // Jellify sends PremiereDate first: year desc by default.
    let response = get(
        app(&fx, settings()),
        "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=PremiereDate",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["Items"][0]["Name"], "Second Dawn");
    assert_eq!(parsed["Items"][1]["Name"], "First Light");
    // Explicit asc flips; unknown SortBy keeps legacy order.
    let response = get(
        app(&fx, settings()),
        "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=PremiereDate&SortOrder=Ascending",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["Items"][0]["Name"], "First Light");
    let response = get(
        app(&fx, settings()),
        "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=Bogus",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["Items"][0]["Name"], "First Light");
}

#[tokio::test]
async fn contributor_miss_is_empty_never_catalog() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let unknown = "0".repeat(32);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items?IncludeItemTypes=MusicAlbum&ContributingArtistIds={unknown}"),
        Some(&auth),
    )
    .await;
    check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "Items": [],
                "TotalRecordCount": 0,
                "StartIndex": 0,
            })),
        },
    )
    .await;
    // A real contributor resolves appears-on (Guest Star appears on rg-2,
    // owned by Blue Giant).
    let response = get(
        app(&fx, settings()),
        &format!(
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&ContributingArtistIds={}",
            fx.artist2
        ),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["TotalRecordCount"], 1);
    assert_eq!(parsed["Items"][0]["Name"], "Second Dawn");
}

#[tokio::test]
async fn ids_lookup_skips_unknown_and_history_sorts_page() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let unknown = "f".repeat(32);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items?Ids={},{unknown}", fx.track1),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["TotalRecordCount"], 1);
    assert_eq!(parsed["Items"][0]["Name"], "Opener");
    // PlayCount history sort excludes the unplayed tracks.
    let response = get(
        app(&fx, settings()),
        "/jellyfin/Items?IncludeItemTypes=Audio&SortBy=PlayCount",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["TotalRecordCount"], 1);
    assert_eq!(parsed["Items"][0]["Name"], "Opener");
}

#[tokio::test]
async fn jellify_latest_is_a_bare_newest_first_array() {
    let fx = fixture().await;
    let auth = jellify(ALICE);
    for uri in [
        "/jellyfin/UserItems/Latest",
        "/jellyfin/Users/x/Items/Latest",
    ] {
        let response = get(app(&fx, settings()), uri, Some(&auth)).await;
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(parsed.is_array(), "Latest must be a bare array");
        assert_eq!(parsed[0]["Name"], "Second Dawn");
        assert_eq!(parsed[1]["Name"], "First Light");
    }
    let response = get(
        app(&fx, settings()),
        "/jellyfin/UserItems/Latest?Limit=1",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        parsed.is_array(),
        "Latest must be a bare array, not an object"
    );
    assert_eq!(parsed.as_array().unwrap().len(), 1);
    assert_eq!(parsed[0]["Name"], "Second Dawn");
    assert_eq!(parsed[0]["Type"], "MusicAlbum");
    // A non-library ParentId yields [].
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/UserItems/Latest?ParentId={}", fx.album1),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"[]");
    // The library ParentId is accepted.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/UserItems/Latest?ParentId={}", fx.library_id),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn artists_genres_and_single_item_shapes() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let response = get(app(&fx, settings()), "/jellyfin/Artists", Some(&auth)).await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["TotalRecordCount"], 2);
    assert_eq!(parsed["Items"][0]["Type"], "MusicArtist");
    assert!(parsed["Items"][0]["Genres"].is_array());
    assert!(is_iso_o(
        parsed["Items"][0]["DateCreated"].as_str().unwrap()
    ));
    // AlbumArtists scopes to album artists only.
    let response = get(
        app(&fx, settings()),
        "/jellyfin/Artists/AlbumArtists",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["TotalRecordCount"], 1);
    assert_eq!(parsed["Items"][0]["Name"], "Blue Giant");
    // Both genre dialects share the handler.
    for uri in ["/jellyfin/Genres", "/jellyfin/MusicGenres"] {
        let response = get(app(&fx, settings()), uri, Some(&auth)).await;
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["TotalRecordCount"], 1);
        assert_eq!(parsed["Items"][0]["Name"], "Rock");
    }
    // Unknown ids 404 with an empty body; genre ids 404 too (v2 `_single_item`
    // has no genre branch — ported faithfully).
    let unknown = "e".repeat(32);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{unknown}"),
        Some(&auth),
    )
    .await;
    check(
        response,
        &Golden {
            status: 404,
            headers: &[],
            body: BodyExp::Exact(b""),
        },
    )
    .await;
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}", fx.genre_rock),
        Some(&auth),
    )
    .await;
    assert_eq!(response.status().as_u16(), 404);
}

// ===== Images (anonymous) =====

#[tokio::test]
async fn images_serve_primary_only_with_immutable_cache() {
    let fx = fixture().await;
    // Library view: the 1x1 PNG, no auth.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}/Images/Primary", fx.library_id),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers().get("content-type").unwrap(), "image/png");
    assert_eq!(
        response.headers().get("cache-control").unwrap(),
        "public, max-age=31536000, immutable"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    // Album art honors the size bucket (width=200 → 250).
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}/Images/Primary?width=200", fx.album1),
        None,
    )
    .await;
    check(
        response,
        &Golden {
            status: 200,
            headers: &[
                ("content-type", ("image/jpeg")),
                ("cache-control", ("public, max-age=31536000, immutable")),
            ],
            body: BodyExp::Exact(b"cover-250-bytes"),
        },
    )
    .await;
    // Track art resolves through the release; artist images serve too.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}/Images/Primary", fx.track1),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}/Images/Primary/0", fx.artist1),
        None,
    )
    .await;
    check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("image/jpeg"))],
            body: BodyExp::Exact(b"artist-bytes"),
        },
    )
    .await;
    // Misses 404 with NO placeholder (unlike Subsonic); non-Primary 404s.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}/Images/Primary", fx.album2),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 404);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}/Images/Backdrop", fx.album1),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 404);
}

// ===== Favorites + played (both dialects) =====

#[tokio::test]
async fn finamp_and_jellify_favorite_dialects_roundtrip() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    // Legacy dialect (Finamp): POST adds.
    let response = post(
        app(&fx, settings()),
        &format!("/jellyfin/Users/x/FavoriteItems/{}", fx.track1),
        Some(&auth),
        b"",
    )
    .await;
    check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "ItemId": fx.track1,
                "Key": fx.track1,
                "PlaybackPositionTicks": 0,
                "PlayCount": 0,
                "IsFavorite": true,
                "Played": false,
            })),
        },
    )
    .await;
    // It shows up via ?IsFavorite=true and via Filters=IsFavorite.
    for uri in [
        "/jellyfin/Items?IncludeItemTypes=Audio&IsFavorite=true".to_owned(),
        "/jellyfin/Items?IncludeItemTypes=Audio&Filters=IsFavorite".to_owned(),
    ] {
        let response = get(app(&fx, settings()), &uri, Some(&auth)).await;
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["TotalRecordCount"], 1);
        assert_eq!(parsed["Items"][0]["Name"], "Opener");
        assert!(
            parsed["Items"][0]["UserData"]["IsFavorite"]
                .as_bool()
                .unwrap()
        );
    }
    // Modern dialect (Jellify): DELETE removes.
    let response = delete(
        app(&fx, settings()),
        &format!("/jellyfin/UserFavoriteItems/{}", fx.track1),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["IsFavorite"], false);
    let response = get(
        app(&fx, settings()),
        "/jellyfin/Items?IncludeItemTypes=Audio&IsFavorite=true",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["TotalRecordCount"], 0);
    // Unknown ids 404; non-favoritable kinds (playlist) 400.
    let unknown = "d".repeat(32);
    let response = post(
        app(&fx, settings()),
        &format!("/jellyfin/UserFavoriteItems/{unknown}"),
        Some(&auth),
        b"",
    )
    .await;
    assert_eq!(response.status().as_u16(), 404);
}

#[tokio::test]
async fn played_items_are_markers_only() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    for uri in [
        format!("/jellyfin/Users/x/PlayedItems/{}", fx.track1),
        format!("/jellyfin/UserPlayedItems/{}", fx.track1),
    ] {
        let response = post(app(&fx, settings()), &uri, Some(&auth), b"").await;
        check(
            response,
            &Golden {
                status: 200,
                headers: &[("content-type", ("application/json"))],
                body: BodyExp::Json(serde_json::json!({
                    "ItemId": fx.track1,
                    "Key": fx.track1,
                    "PlaybackPositionTicks": 0,
                    "PlayCount": 0,
                    "IsFavorite": false,
                    "Played": true,
                })),
            },
        )
        .await;
    }
    // No scrobble was forwarded: the marker writes nothing.
    assert!(fx.sessions.calls().is_empty());
    let unknown = "d".repeat(32);
    let response = post(
        app(&fx, settings()),
        &format!("/jellyfin/UserPlayedItems/{unknown}"),
        Some(&auth),
        b"",
    )
    .await;
    assert_eq!(response.status().as_u16(), 404);
}

// ===== Streaming (Jellify headerless row) =====

#[tokio::test]
async fn jellify_headerless_audio_serves_anonymously() {
    let fx = fixture().await;
    // No auth header at all: direct bytes with the seek-safe header set.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Audio/{}/stream.mp3?static=true", fx.track1),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "audio/mpeg"
    );
    assert_eq!(response.headers().get("content-length").unwrap(), "1024");
    assert_eq!(response.headers().get("accept-ranges").unwrap(), "bytes");
    assert_eq!(
        response.headers().get("content-encoding").unwrap(),
        "identity"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(bytes.len(), 1024);
    assert_eq!(bytes[0], 0);
    // The `.ext` suffix is cosmetic; unknown tails 404.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Audio/{}/stream", fx.track1),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Audio/{}/bogus", fx.track1),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 404);
    // Non-track and unknown ids 404.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Audio/{}/stream", fx.album1),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 404);
}

#[tokio::test]
async fn range_contract_single_suffix_open_206_rest_416() {
    let fx = fixture().await;
    let uri = format!("/jellyfin/Audio/{}/stream.mp3?static=true", fx.track1);
    for (range, status, content_range, len, first) in [
        ("bytes=0-99", 206, "bytes 0-99/1024", 100, 0u8),
        ("bytes=100-", 206, "bytes 100-1023/1024", 924, 100u8),
        ("bytes=-10", 206, "bytes 1014-1023/1024", 10, 10u8),
        ("bytes=0-99999", 206, "bytes 0-1023/1024", 1024, 0u8),
        ("bytes=0-0,2-3", 416, "bytes */1024", 0, 0u8),
        ("bytes=2000-", 416, "bytes */1024", 0, 0u8),
        ("bytes=-0", 416, "bytes */1024", 0, 0u8),
        ("bytes=abc", 416, "bytes */1024", 0, 0u8),
        ("items=0-1", 416, "bytes */1024", 0, 0u8),
    ] {
        let response = app(&fx, settings())
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("range", range)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status, "range {range}");
        assert_eq!(
            response.headers().get("content-range").unwrap(),
            content_range,
            "range {range}"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(bytes.len(), len, "range {range}");
        if status == 206 {
            assert_eq!(bytes[0], first, "range {range}");
        }
    }
    // HEAD: headers only, identity encoding, empty body.
    let response = head(app(&fx, settings()), &uri, None, None).await;
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers().get("content-length").unwrap(), "1024");
    assert_eq!(
        response.headers().get("content-encoding").unwrap(),
        "identity"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.is_empty());

    // HEAD honors Range exactly like GET: 206 + Content-Range on a
    // slice, 416 past the end, never a body.
    let response = head(app(&fx, settings()), &uri, None, Some("bytes=0-99")).await;
    assert_eq!(response.status().as_u16(), 206);
    assert_eq!(
        response.headers().get("content-range").unwrap(),
        "bytes 0-99/1024"
    );
    assert_eq!(response.headers().get("content-length").unwrap(), "100");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.is_empty());
    let response = head(app(&fx, settings()), &uri, None, Some("bytes=2000-")).await;
    assert_eq!(response.status().as_u16(), 416);
    assert_eq!(
        response.headers().get("content-range").unwrap(),
        "bytes */1024"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn audio_unknown_tails_404_on_get_and_head() {
    // m5: HEAD /Audio/{id}/bogus 404s like GET does — no tail probes a
    // 200 out of a route GET would refuse.
    let fx = fixture().await;
    let uri = format!("/jellyfin/Audio/{}/bogus", fx.track1);
    let response = get(app(&fx, settings()), &uri, None).await;
    assert_eq!(response.status().as_u16(), 404);
    let response = head(app(&fx, settings()), &uri, None, None).await;
    assert_eq!(response.status().as_u16(), 404);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn universal_negotiates_containers_and_transcodes_on_request() {
    let fx = fixture().await;
    // Source format accepted → direct.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Audio/{}/universal?Container=mp3", fx.track1),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(bytes.len(), 1024);
    // Pipe-variants split, first token wins (`mp3|mp4` accepts mp3).
    let response = get(
        app(&fx, settings()),
        &format!(
            "/jellyfin/Audio/{}/universal?Container=mp3%7Cmp4",
            fx.track1
        ),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    // Codec mismatch → transcode marker. (The "no Content-Length" half of
    // the contract pins at the seam below: axum auto-adds the header to the
    // fake's sized marker bytes, while the real ffmpeg pipe streams unsized.)
    let response = get(
        app(&fx, settings()),
        &format!(
            "/jellyfin/Audio/{}/universal?Container=ogg&AudioCodec=opus",
            fx.track1
        ),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers().get("accept-ranges").unwrap(), "none");
    assert_eq!(response.headers().get("content-type").unwrap(), "audio/ogg");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"transcoded:opus:"));
    // Unknown codecs coerce to opus, "nearest we can produce".
    let response = get(
        app(&fx, settings()),
        &format!(
            "/jellyfin/Audio/{}/universal?Container=ogg&AudioCodec=aac",
            fx.track1
        ),
        None,
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"transcoded:opus:"));
    // A client ceiling below the source bitrate transcodes (the server max
    // alone never triggers: 320kbps source at default settings → direct).
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Audio/{}/stream?audioBitRate=64000", fx.track1),
        None,
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert!(bytes.starts_with(b"transcoded:mp3:64:"));
}

#[tokio::test]
async fn engine_seam_pins_length_headers_and_decide_rules() {
    // Seams-level contract for the stage-6 binding: direct outcomes carry an
    // exact Content-Length, transcode outcomes carry none (estimate off),
    // and the policy ports the §3.1 rules verbatim.
    use droppedneedle::compat::jellyfin::seams::{DecideInput, StreamPlan, decide};
    let fx = fixture().await;
    let direct = fx.engine.direct("f1", None).await;
    assert_eq!(direct.status, 200);
    assert!(
        direct
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Length" && v == "1024")
    );
    let partial = fx.engine.direct("f1", Some("bytes=0-9")).await;
    assert_eq!(partial.status, 206);
    assert!(
        partial
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Length" && v == "10")
    );
    let missing = fx.engine.direct("f1", Some("bytes=9999-")).await;
    assert_eq!(missing.status, 416);
    assert!(missing.headers.iter().any(|(k, _)| k == "Content-Range"));
    assert!(missing.body.is_empty());
    let transcoded = fx.engine.transcode("f1", "opus", 128, 0.0).await;
    assert_eq!(transcoded.status, 200);
    assert!(
        !transcoded
            .headers
            .iter()
            .any(|(k, _)| k == "Content-Length")
    );
    assert!(
        transcoded
            .headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v == "audio/ogg"),
        "transcoded opus rides -f ogg"
    );

    let input = |requested: Option<&'static str>, ceiling: Option<u32>, force: bool| DecideInput {
        src_format: Some("mp3"),
        src_bitrate_kbps: 320,
        requested,
        ceiling_kbps: ceiling,
        force_original: force,
        start_seconds: 0.0,
        transcoding_enabled: true,
        server_max_kbps: 320,
        default_format: "mp3",
        ffmpeg: true,
    };
    // No request → direct, even though the server max equals the source.
    assert_eq!(decide(&input(None, None, false)), StreamPlan::Direct);
    // Explicit triggers transcode.
    assert!(matches!(
        decide(&input(Some("opus"), None, false)),
        StreamPlan::Transcode { format, .. } if format == "opus"
    ));
    assert!(matches!(
        decide(&input(None, Some(64), false)),
        StreamPlan::Transcode {
            bitrate_kbps: 64,
            ..
        }
    ));
    // Ceiling 0/None means unset, never a trigger.
    assert_eq!(decide(&input(None, Some(0), false)), StreamPlan::Direct);
    // The server max caps quality but never triggers.
    assert!(matches!(
        decide(&input(Some("opus"), Some(999), false)),
        StreamPlan::Transcode {
            bitrate_kbps: 320,
            ..
        }
    ));
    // Silent direct fallbacks.
    assert_eq!(decide(&input(Some("opus"), None, true)), StreamPlan::Direct);
    let mut off = input(Some("opus"), None, false);
    off.transcoding_enabled = false;
    assert_eq!(decide(&off), StreamPlan::Direct);
    // Unusable requests fall back to the default format; start clamps at 0.
    let mut start = input(Some("wav"), None, false);
    start.start_seconds = -5.0;
    assert!(matches!(
        decide(&start),
        StreamPlan::Transcode { format, start_seconds, .. }
        if format == "mp3" && start_seconds == 0.0
    ));
}

// ===== PlaybackInfo (Finamp row) =====

#[tokio::test]
async fn finamp_playback_info_direct_has_15_non_null_fields() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{}/PlaybackInfo", fx.track1),
        Some(&auth),
    )
    .await;
    let bytes = check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "MediaSources": [{
                    "Id": fx.track1,
                    "Protocol": "File",
                    "Container": "mp3",
                    "Size": 1024,
                    "Bitrate": 320_000,
                    "RunTimeTicks": 1_800_000_000_i64,
                    "SupportsDirectPlay": true,
                    "SupportsDirectStream": true,
                    "SupportsTranscoding": true,
                    "DefaultAudioStreamIndex": 0,
                    "MediaStreams": [{
                        "Type": "Audio",
                        "Codec": "mp3",
                        "Index": 0,
                        "BitRate": 320_000,
                        "Channels": 2,
                        "ChannelLayout": "stereo",
                        "SampleRate": 44100,
                        "IsDefault": true,
                        "IsInterlaced": false,
                        "IsForced": false,
                        "IsExternal": false,
                        "IsTextSubtitleStream": false,
                        "SupportsExternalStream": false,
                    }],
                    "IsRemote": false,
                    "DirectStreamUrl": format!(
                        "http://localhost/jellyfin/Audio/{}/stream.mp3?static=true&mediaSourceId={}&api_key={}",
                        fx.track1, fx.track1, ALICE,
                    ),
                    "Type": "Default",
                    "IsInfiniteStream": false,
                    "RequiresOpening": false,
                    "RequiresClosing": false,
                    "RequiresLooping": false,
                    "SupportsProbing": false,
                    "ReadAtNativeFramerate": false,
                    "IgnoreDts": false,
                    "IgnoreIndex": false,
                    "GenPtsInput": false,
                }],
                "PlaySessionId": "__UUID__",
            })),
        },
    )
    .await;
    // The advertised URL fetches headerless: strip the origin, GET with no
    // auth, expect bytes. This is the Jellify/Manet playback path.
    let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let url = info["MediaSources"][0]["DirectStreamUrl"].as_str().unwrap();
    let path = url.strip_prefix("http://localhost").unwrap();
    let response = get(app(&fx, settings()), path, None).await;
    assert_eq!(response.status().as_u16(), 200);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(bytes.len(), 1024);
}

#[tokio::test]
async fn playback_info_transcode_fields_only_when_policy_transcodes() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    // Low ceiling via query → transcoding fields appear (default mp3).
    let response = get(
        app(&fx, settings()),
        &format!(
            "/jellyfin/Items/{}/PlaybackInfo?maxStreamingBitrate=64000",
            fx.track1
        ),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let src = &parsed["MediaSources"][0];
    assert!(
        src["TranscodingUrl"]
            .as_str()
            .unwrap()
            .contains("/jellyfin/Audio/")
    );
    assert!(
        src["TranscodingUrl"]
            .as_str()
            .unwrap()
            .contains("AudioCodec=mp3")
    );
    assert_eq!(src["TranscodingSubProtocol"], "http");
    assert_eq!(src["TranscodingContainer"], "mp3");
    // Same via POST body; opus default → ogg container.
    let mut opus = settings();
    opus.transcode_default_format = "opus".to_owned();
    let body = serde_json::json!({"MaxStreamingBitrate": 64000, "UserId": "zzz"}).to_string();
    let response = post(
        app(&fx, opus),
        &format!("/jellyfin/Items/{}/PlaybackInfo", fx.track1),
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["MediaSources"][0]["TranscodingContainer"], "ogg");
    // Unknown track 404s.
    let unknown = "c".repeat(32);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{unknown}/PlaybackInfo"),
        Some(&auth),
    )
    .await;
    assert_eq!(response.status().as_u16(), 404);
}

// ===== Sessions =====

#[tokio::test]
async fn playing_and_progress_report_presence_without_scrobbling() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    // Empty and ItemId-less bodies still 204 silently.
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Playing",
        Some(&auth),
        b"",
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    assert!(fx.sessions.calls().is_empty());
    // Start records the session key and now-playing.
    let body = serde_json::json!({"ItemId": fx.track1, "PlaySessionId": "ps-1"}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Playing",
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    // Progress converts ticks → ms and never scrobbles.
    let body =
        serde_json::json!({"ItemId": fx.track1, "PositionTicks": 20_000_000_i64}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Playing/Progress",
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    assert_eq!(
        fx.sessions.calls(),
        vec![
            SessionCall::MarkStarted {
                user_id: "user-alice".to_owned(),
                key: "ps-1".to_owned()
            },
            SessionCall::NowPlaying {
                user_id: "user-alice".to_owned(),
                file_id: "f1".to_owned()
            },
            SessionCall::Progress {
                user_id: "user-alice".to_owned(),
                file_id: "f1".to_owned(),
                position_ms: Some(2000),
                paused: false,
            },
        ]
    );
    // Ping + capabilities accept and 204.
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Playing/Ping",
        Some(&auth),
        b"",
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Capabilities/Full",
        Some(&auth),
        b"{}",
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
}

#[tokio::test]
async fn stopped_scrobbles_past_threshold_and_always_clears_presence() {
    // Past 90% → scrobble.
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let body = serde_json::json!({
        "ItemId": fx.track1,
        "PlaySessionId": "ps-1",
        "PositionTicks": 1_700_000_000_i64,
        "RunTimeTicks": 1_800_000_000_i64,
    })
    .to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Playing/Stopped",
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    assert_eq!(
        fx.sessions.calls(),
        vec![
            SessionCall::ClearPresence {
                user_id: "user-alice".to_owned()
            },
            SessionCall::Scrobble {
                user_id: "user-alice".to_owned(),
                file_id: "f1".to_owned()
            },
        ]
    );
    // Below threshold → presence cleared, no scrobble.
    let fx = fixture().await;
    let body = serde_json::json!({
        "ItemId": fx.track1,
        "PositionTicks": 100_000_000_i64,
        "RunTimeTicks": 1_800_000_000_i64,
    })
    .to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Playing/Stopped",
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    assert_eq!(
        fx.sessions.calls(),
        vec![SessionCall::ClearPresence {
            user_id: "user-alice".to_owned()
        }]
    );
    // Omitted position counts; Failed skips.
    let fx = fixture().await;
    let body = serde_json::json!({"ItemId": fx.track1}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Playing/Stopped",
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    assert!(
        fx.sessions
            .calls()
            .iter()
            .any(|c| matches!(c, SessionCall::Scrobble { .. }))
    );
    let fx = fixture().await;
    let body = serde_json::json!({"ItemId": fx.track1, "Failed": true}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Sessions/Playing/Stopped",
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    assert!(
        !fx.sessions
            .calls()
            .iter()
            .any(|c| matches!(c, SessionCall::Scrobble { .. }))
    );
}

// ===== Playlists =====

#[tokio::test]
async fn playlists_create_detail_items_add_remove_move() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    // Create from a JSON body; unknown ids skip silently.
    let unknown = "b".repeat(32);
    let body = serde_json::json!({"Name": "Road", "Ids": [fx.track1, unknown]}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Playlists",
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let pid = parsed["Id"].as_str().unwrap().to_owned();
    // Detail carries the served ItemIds only.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Playlists/{pid}"),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["Name"], "Road");
    assert_eq!(
        parsed["ItemIds"].as_array().unwrap(),
        &vec![fx.track1.clone()]
    );
    // Items carry the per-entry PlaylistItemId handle.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Playlists/{pid}/Items"),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["TotalRecordCount"], 1);
    assert_eq!(parsed["Items"][0]["Name"], "Opener");
    let entry = parsed["Items"][0]["PlaylistItemId"]
        .as_str()
        .unwrap()
        .to_owned();
    // Add a second entry, then move it first.
    let response = post(
        app(&fx, settings()),
        &format!("/jellyfin/Playlists/{pid}/Items?ids={}", fx.track2),
        Some(&auth),
        b"",
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    let response = post(
        app(&fx, settings()),
        &format!("/jellyfin/Playlists/{pid}/Items/{entry}/Move/1"),
        Some(&auth),
        b"",
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Playlists/{pid}/Items"),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["Items"][0]["Name"], "Deep Cut");
    assert_eq!(parsed["Items"][1]["Name"], "Opener");
    // Remove by entry id; browse counts stay streamable-only.
    let response = delete(
        app(&fx, settings()),
        &format!("/jellyfin/Playlists/{pid}/Items?entryIds={entry}"),
        Some(&auth),
    )
    .await;
    assert_eq!(response.status().as_u16(), 204);
    let response = get(
        app(&fx, settings()),
        "/jellyfin/Items?IncludeItemTypes=Playlist",
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["Items"][0]["ChildCount"], 1);
    // Query-param creation defaults the name.
    let response = post(
        app(&fx, settings()),
        &format!("/jellyfin/Playlists?ids={}", fx.track3),
        Some(&auth),
        b"",
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let pid2 = parsed["Id"].as_str().unwrap().to_owned();
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Playlists/{pid2}"),
        Some(&auth),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["Name"], "Playlist");
}

// ===== Discovery + error shape =====

#[tokio::test]
async fn similar_resolves_artist_and_empty_means_empty() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    for uri in [
        format!("/jellyfin/Items/{}/Similar", fx.track1),
        format!("/jellyfin/Items/{}/InstantMix", fx.album1),
        format!("/jellyfin/Artists/{}/InstantMix", fx.artist1),
    ] {
        let response = get(app(&fx, settings()), &uri, Some(&auth)).await;
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["TotalRecordCount"], 2, "{uri}");
    }
    // Unknown ids yield EMPTY, not 404.
    let unknown = "a".repeat(32);
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items/{unknown}/Similar"),
        Some(&auth),
    )
    .await;
    let bytes = check(
        response,
        &Golden {
            status: 200,
            headers: &[("content-type", ("application/json"))],
            body: BodyExp::Json(serde_json::json!({
                "Items": [],
                "TotalRecordCount": 0,
                "StartIndex": 0,
            })),
        },
    )
    .await;
    let _ = bytes;
}

#[tokio::test]
async fn errors_are_real_statuses_with_empty_bodies() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    // Bad token → 401, empty, never the native envelope.
    let bad = finamp("wrong-secret");
    let response = get(app(&fx, settings()), "/jellyfin/Items", Some(&bad)).await;
    check(
        response,
        &Golden {
            status: 401,
            headers: &[],
            body: BodyExp::Exact(b""),
        },
    )
    .await;
    // Query-key auth works as an alternative transport.
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Items?api_key={ALICE}&Limit=0"),
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 200);
    // No route is behind the native envelope: a playlist id on a track
    // route is a bare 404.
    let body = serde_json::json!({"Name": "X"}).to_string();
    let response = post(
        app(&fx, settings()),
        "/jellyfin/Playlists",
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let pid = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["Id"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = get(
        app(&fx, settings()),
        &format!("/jellyfin/Audio/{pid}/stream"),
        None,
    )
    .await;
    check(
        response,
        &Golden {
            status: 404,
            headers: &[],
            body: BodyExp::Exact(b""),
        },
    )
    .await;
}
