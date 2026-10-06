//! Jellyfin wire contract beyond the pinned references in
//! `compat_journeys`: login echo, query dialects of Finamp, Jellify and
//! Manet, images, favorites, transcode negotiation, session reports,
//! playlists and error shapes. Runs the real router over the fixture
//! seams; ids are the deterministic `sha256("kind:internal")[:32]`.

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use droppedneedle::auth::compat_auth::fakes::FakeCompatPasswords;
use droppedneedle::auth::compat_auth::jellyfin::server_id;
use droppedneedle::compat::jellyfin::fake::{
    FakeLibrary, MemoryEngine, MemoryIds, MemorySessions, SessionCall,
};
use droppedneedle::compat::jellyfin::seams::{
    DecideInput, IdMap, JellyfinSettings, StreamPlan, decide,
};
use droppedneedle::compat::jellyfin::{JellyfinState, router};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ALICE: &str = "alice-app-secret";
const BOB: &str = "bob-app-secret";

struct Fx {
    passwords: FakeCompatPasswords,
    library: FakeLibrary,
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
    library_id: String,
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
    let library = FakeLibrary::new();
    let engine = MemoryEngine::new();
    crate::compat_journeys::seed_jellyfin(&library, &engine);
    let ids = MemoryIds::new();
    Fx {
        track1: ids.to_jf("track", "f1").await,
        track2: ids.to_jf("track", "f2").await,
        track3: ids.to_jf("track", "f3").await,
        album1: ids.to_jf("album", "rg-1").await,
        album2: ids.to_jf("album", "rg-2").await,
        artist1: ids.to_jf("artist", "mb-a1").await,
        artist2: ids.to_jf("artist", "mb-a2").await,
        library_id: ids.to_jf("library", "music").await,
        passwords,
        library,
        engine,
        sessions: MemorySessions::new(),
        ids,
    }
}

fn settings() -> JellyfinSettings {
    JellyfinSettings {
        enabled: true,
        ..JellyfinSettings::default()
    }
}

impl Fx {
    fn app(&self, settings: JellyfinSettings) -> Router {
        Router::new().nest(
            "/jellyfin",
            router(JellyfinState::new(
                self.passwords.clone(),
                self.library.clone(),
                self.engine.clone(),
                self.sessions.clone(),
                self.ids.clone(),
                settings,
            )),
        )
    }

    async fn send(&self, method: &str, uri: &str, auth: Option<&str>, body: &[u8]) -> Reply {
        send(self.app(settings()), method, uri, auth, body).await
    }

    async fn get(&self, uri: &str, auth: Option<&str>) -> Reply {
        self.send("GET", uri, auth, b"").await
    }

    async fn json(&self, uri: &str, auth: Option<&str>) -> Value {
        self.get(uri, auth).await.json()
    }
}

struct Reply {
    status: u16,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("json body")
    }
}

async fn send(app: Router, method: &str, uri: &str, auth: Option<&str>, body: &[u8]) -> Reply {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(header) = auth {
        builder = builder.header("authorization", header);
    }
    if !body.is_empty() {
        builder = builder.header("content-type", "application/json");
    }
    let response = app
        .oneshot(builder.body(Body::from(body.to_vec())).expect("request"))
        .await
        .expect("router responds");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("body")
        .to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

fn finamp(secret: &str) -> String {
    format!(
        "MediaBrowser Client=\"Finamp\", Device=\"Test\", DeviceId=\"dev1\", Token=\"{secret}\""
    )
}

/// Exact-key-set JSON comparison; `__UUID__` and `__ISO_PY__` match by shape.
fn assert_shape(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::String(got), Value::String(exp)) if exp == "__UUID__" => {
            assert!(
                got.len() == 32 && got.bytes().all(|b| b.is_ascii_hexdigit()),
                "{path}: {got} is not a 32-hex id"
            );
        }
        (Value::String(got), Value::String(exp)) if exp == "__ISO_PY__" => {
            // `2026-09-28T12:00:00.123456+00:00`
            let shape = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6}\+00:00$")
                .expect("regex");
            assert!(
                shape.is_match(got),
                "{path}: {got} is not a microsecond UTC timestamp"
            );
        }
        (Value::Object(got), Value::Object(exp)) => {
            let mut got_keys: Vec<_> = got.keys().collect();
            let mut exp_keys: Vec<_> = exp.keys().collect();
            got_keys.sort();
            exp_keys.sort();
            assert_eq!(got_keys, exp_keys, "{path}: key sets differ");
            for key in exp.keys() {
                assert_shape(&got[key], &exp[key], &format!("{path}.{key}"));
            }
        }
        _ => assert_eq!(actual, expected, "{path}"),
    }
}

/// The user DTO every Jellyfin client parses at login; Finamp rejects the
/// session when any of these keys is missing.
fn expected_user(name: &str, id: &str, is_admin: bool) -> Value {
    json!({
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
async fn gates_and_system_endpoints() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    // Disabled: every route 404s with an empty body, authed or not.
    let off = JellyfinSettings::default();
    assert!(!off.enabled);
    for (uri, header) in [
        ("/jellyfin/System/Info/Public", None),
        ("/jellyfin/Items", Some(auth.as_str())),
    ] {
        let reply = send(fx.app(off.clone()), "GET", uri, header, b"").await;
        assert_eq!((reply.status, reply.body.len()), (404, 0), "{uri}");
    }

    let denied = fx.get("/jellyfin/System/Info", None).await;
    assert_eq!((denied.status, denied.body.len()), (401, 0));
    let info = fx.get("/jellyfin/System/Info", Some(&auth)).await;
    assert_eq!(info.header("content-type"), "application/json");
    assert_shape(
        &info.json(),
        &json!({
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
        }),
        "$",
    );
    let quick = fx.get("/jellyfin/QuickConnect/Enabled", None).await;
    assert_eq!((quick.status, quick.body.as_slice()), (200, &b"false"[..]));
    let logout = fx
        .send("POST", "/jellyfin/Sessions/Logout", None, b"")
        .await;
    assert_eq!((logout.status, logout.body.len()), (204, 0));
}

#[tokio::test]
async fn login_echoes_the_app_password_and_users_me_matches() {
    let fx = fixture().await;
    let login = "/jellyfin/Users/AuthenticateByName";
    // Finamp sends an undocumented `UserId`; the lenient decode accepts it.
    let body = json!({"Username": "alice", "Pw": ALICE, "UserId": "zzz"}).to_string();
    let echo = fx.send("POST", login, None, body.as_bytes()).await;
    assert_eq!(echo.status, 200);
    let echo = echo.json();
    assert_shape(
        &echo,
        &json!({
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
        }),
        "$",
    );

    // Client and device facts from the header land in SessionInfo.
    let header =
        "MediaBrowser Client=\"Jellify\", Device=\"Pixel\", DeviceId=\"abc\", Token=\"unused\"";
    let body = json!({"Username": "bob", "Pw": BOB}).to_string();
    let bob = fx
        .send("POST", login, Some(header), body.as_bytes())
        .await
        .json();
    assert_eq!(bob["User"], expected_user("bob", "user-bob", true));
    assert_eq!(bob["SessionInfo"]["Client"], "Jellify");
    assert_eq!(bob["SessionInfo"]["DeviceName"], "Pixel");
    assert_eq!(bob["SessionInfo"]["DeviceId"], "abc");

    // Wrong app password and the account password both fail; no body is 400.
    for pw in ["nope", "alice-account-pw"] {
        let body = json!({"Username": "alice", "Pw": pw}).to_string();
        let reply = fx.send("POST", login, None, body.as_bytes()).await;
        assert_eq!((reply.status, reply.body.len()), (401, 0), "{pw}");
    }
    assert_eq!(fx.send("POST", login, None, b"").await.status, 400);

    // `/Users/Me` and `/Users/{any id}` both return the caller, equal to
    // the login echo's User.
    let auth = finamp(ALICE);
    for uri in ["/jellyfin/Users/Me", "/jellyfin/Users/someone-else"] {
        assert_eq!(fx.json(uri, Some(&auth)).await, echo["User"], "{uri}");
    }
}

#[tokio::test]
async fn browse_query_dialects() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let names = |page: &Value| -> Vec<String> {
        let items = page.get("Items").unwrap_or(page);
        items
            .as_array()
            .expect("item list")
            .iter()
            .map(|item| item["Name"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    let none = "f".repeat(32);
    let cases: Vec<(String, Vec<&str>)> =
        vec![
        // Lowercase query keys bind (ASP.NET parity); children in track order.
        (format!("/jellyfin/Items?parentid={}", fx.album1), vec!["Opener", "Deep Cut"]),
        // MusicArtist outranks MusicAlbum in IncludeItemTypes.
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum,MusicArtist".to_owned(),
            vec!["Blue Giant", "Guest Star"],
        ),
        // Jellify's PremiereDate sort is year descending unless told otherwise.
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=PremiereDate".to_owned(),
            vec!["Second Dawn", "First Light"],
        ),
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=PremiereDate&SortOrder=Ascending"
                .to_owned(),
            vec!["First Light", "Second Dawn"],
        ),
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=Bogus".to_owned(),
            vec!["First Light", "Second Dawn"],
        ),
        // An unknown contributor yields nothing, never the whole catalog;
        // a guest artist resolves to the albums they appear on.
        (
            format!("/jellyfin/Items?IncludeItemTypes=MusicAlbum&ContributingArtistIds={none}"),
            vec![],
        ),
        (
            format!(
                "/jellyfin/Items?IncludeItemTypes=MusicAlbum&ContributingArtistIds={}",
                fx.artist2
            ),
            vec!["Second Dawn"],
        ),
        // Ids skips unknown ids; the PlayCount history sort drops unplayed.
        (format!("/jellyfin/Items?Ids={},{none}", fx.track1), vec!["Opener"]),
        (
            "/jellyfin/Items?IncludeItemTypes=Audio&SortBy=PlayCount".to_owned(),
            vec!["Opener"],
        ),
        // Latest is a bare newest-first array; a non-library ParentId is empty.
        ("/jellyfin/UserItems/Latest".to_owned(), vec!["Second Dawn", "First Light"]),
        ("/jellyfin/Users/x/Items/Latest?Limit=1".to_owned(), vec!["Second Dawn"]),
        (
            format!("/jellyfin/UserItems/Latest?ParentId={}", fx.library_id),
            vec!["Second Dawn", "First Light"],
        ),
        (format!("/jellyfin/UserItems/Latest?ParentId={}", fx.album1), vec![]),
        ("/jellyfin/Artists".to_owned(), vec!["Blue Giant", "Guest Star"]),
        ("/jellyfin/Artists/AlbumArtists".to_owned(), vec!["Blue Giant"]),
        ("/jellyfin/Genres".to_owned(), vec!["Rock"]),
        ("/jellyfin/MusicGenres".to_owned(), vec!["Rock"]),
        // Similar and InstantMix resolve through the artist; unknown is empty.
        (format!("/jellyfin/Items/{}/Similar", fx.track1), vec!["Opener", "Deep Cut"]),
        (format!("/jellyfin/Artists/{}/InstantMix", fx.artist1), vec!["Opener", "Deep Cut"]),
        (format!("/jellyfin/Items/{none}/Similar"), vec![]),
    ];
    for (uri, expected) in cases {
        let page = fx.json(&uri, Some(&auth)).await;
        if uri.contains("Latest") {
            assert!(page.is_array(), "{uri}: Latest is a bare array");
        }
        let mut got = names(&page);
        if uri.contains("Similar") || uri.contains("InstantMix") {
            got.sort();
            let mut want: Vec<String> = expected.iter().map(|s| (*s).to_owned()).collect();
            want.sort();
            assert_eq!(got, want, "{uri}");
        } else {
            assert_eq!(got, expected, "{uri}");
        }
    }

    // Manet boots on these: the music view and the filter lists.
    let views = fx.json("/jellyfin/UserViews", Some(&auth)).await;
    assert_eq!(views["Items"][0]["CollectionType"], "music");
    let filters = fx.json("/jellyfin/Items/Filters", Some(&auth)).await;
    assert_eq!(
        filters,
        json!({"Genres": ["Rock"], "Tags": [], "OfficialRatings": [], "Years": []})
    );
    // .NET "O" dates keep seven fractional digits even on whole seconds.
    let album = fx
        .json(&format!("/jellyfin/Items/{}", fx.album1), Some(&auth))
        .await;
    assert_eq!(album["DateCreated"], "1970-01-01T00:16:40.0000000Z");
    // Unknown ids and genre ids (v2 has no genre branch) 404 empty.
    let genre = fx.ids.to_jf("genre", "rock").await;
    for id in [none, genre] {
        let reply = fx.get(&format!("/jellyfin/Items/{id}"), Some(&auth)).await;
        assert_eq!((reply.status, reply.body.len()), (404, 0));
    }
}

#[tokio::test]
async fn images_serve_anonymously_with_immutable_cache() {
    let fx = fixture().await;
    let immutable = "public, max-age=31536000, immutable";
    let library = fx
        .get(
            &format!("/jellyfin/Items/{}/Images/Primary", fx.library_id),
            None,
        )
        .await;
    assert_eq!(
        (library.status, library.header("content-type")),
        (200, "image/png")
    );
    assert_eq!(library.header("cache-control"), immutable);
    assert!(library.body.starts_with(b"\x89PNG\r\n\x1a\n"));
    for (uri, bytes) in [
        // width=200 picks the 250 bucket; tracks resolve through their album.
        (
            format!("/jellyfin/Items/{}/Images/Primary?width=200", fx.album1),
            &b"cover-250-bytes"[..],
        ),
        (
            format!("/jellyfin/Items/{}/Images/Primary", fx.track1),
            b"cover-500-bytes",
        ),
        (
            format!("/jellyfin/Items/{}/Images/Primary/0", fx.artist1),
            b"artist-bytes",
        ),
    ] {
        let reply = fx.get(&uri, None).await;
        assert_eq!(
            (reply.status, reply.header("content-type")),
            (200, "image/jpeg"),
            "{uri}"
        );
        assert_eq!(reply.header("cache-control"), immutable, "{uri}");
        assert_eq!(reply.body, bytes, "{uri}");
    }
    // Misses 404 with no placeholder (unlike Subsonic); only Primary serves.
    for uri in [
        format!("/jellyfin/Items/{}/Images/Primary", fx.album2),
        format!("/jellyfin/Items/{}/Images/Backdrop", fx.album1),
    ] {
        assert_eq!(fx.get(&uri, None).await.status, 404, "{uri}");
    }
}

#[tokio::test]
async fn favorite_dialects_and_played_markers() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let favorites = "/jellyfin/Items?IncludeItemTypes=Audio&IsFavorite=true";
    // Finamp adds through the legacy path.
    let added = fx
        .send(
            "POST",
            &format!("/jellyfin/Users/x/FavoriteItems/{}", fx.track1),
            Some(&auth),
            b"",
        )
        .await;
    assert_eq!(added.status, 200);
    assert_eq!(
        added.json(),
        json!({
            "ItemId": fx.track1,
            "Key": fx.track1,
            "PlaybackPositionTicks": 0,
            "PlayCount": 0,
            "IsFavorite": true,
            "Played": false,
        })
    );
    for uri in [
        favorites,
        "/jellyfin/Items?IncludeItemTypes=Audio&Filters=IsFavorite",
    ] {
        let page = fx.json(uri, Some(&auth)).await;
        assert_eq!(page["TotalRecordCount"], 1, "{uri}");
        assert_eq!(page["Items"][0]["UserData"]["IsFavorite"], true, "{uri}");
    }
    // Jellify removes through the modern path.
    let removed = fx
        .send(
            "DELETE",
            &format!("/jellyfin/UserFavoriteItems/{}", fx.track1),
            Some(&auth),
            b"",
        )
        .await;
    assert_eq!(removed.json()["IsFavorite"], false);
    assert_eq!(fx.json(favorites, Some(&auth)).await["TotalRecordCount"], 0);

    // Played markers answer Played=true and forward nothing.
    for uri in [
        format!("/jellyfin/Users/x/PlayedItems/{}", fx.track1),
        format!("/jellyfin/UserPlayedItems/{}", fx.track1),
    ] {
        let reply = fx.send("POST", &uri, Some(&auth), b"").await;
        assert_eq!(reply.json()["Played"], true, "{uri}");
    }
    assert!(fx.sessions.calls().is_empty());

    let unknown = "d".repeat(32);
    for uri in [
        format!("/jellyfin/UserFavoriteItems/{unknown}"),
        format!("/jellyfin/UserPlayedItems/{unknown}"),
    ] {
        assert_eq!(
            fx.send("POST", &uri, Some(&auth), b"").await.status,
            404,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn audio_routes_negotiate_direct_or_transcode() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let audio = |tail: &str| format!("/jellyfin/Audio/{}/{tail}", fx.track1);
    // No credential at all: 401, so nobody streams or transcodes anonymously.
    let anonymous = fx.get(&audio("stream.mp3?static=true"), None).await;
    assert_eq!(anonymous.status, 401);
    // Headerless players carry the token as `api_key`, the way PlaybackInfo
    // hands out its URLs.
    let direct = fx
        .get(
            &audio(&format!("stream.mp3?static=true&api_key={ALICE}")),
            None,
        )
        .await;
    assert_eq!(direct.status, 200);
    assert_eq!(direct.header("content-encoding"), "identity");
    assert_eq!(direct.body.len(), 1024);

    // Unknown tails and non-track ids 404 on GET and HEAD alike.
    for (method, uri) in [
        ("GET", audio("bogus")),
        ("HEAD", audio("bogus")),
        ("GET", format!("/jellyfin/Audio/{}/stream", fx.album1)),
    ] {
        let reply = fx.send(method, &uri, Some(&auth), b"").await;
        assert_eq!((reply.status, reply.body.len()), (404, 0), "{method} {uri}");
    }

    // Accepted containers (first pipe variant wins) stream direct; a codec
    // mismatch or a client ceiling under the source bitrate transcodes, and
    // unknown codecs fall back to opus.
    for (tail, transcoded) in [
        ("universal?Container=mp3", None),
        ("universal?Container=mp3%7Cmp4", None),
        (
            "universal?Container=ogg&AudioCodec=opus",
            Some("transcoded:opus:"),
        ),
        (
            "universal?Container=ogg&AudioCodec=aac",
            Some("transcoded:opus:"),
        ),
        ("stream?audioBitRate=64000", Some("transcoded:mp3:64:")),
    ] {
        let reply = fx.get(&audio(tail), Some(&auth)).await;
        assert_eq!(reply.status, 200, "{tail}");
        match transcoded {
            None => assert_eq!(reply.body.len(), 1024, "{tail}"),
            Some(marker) => {
                assert!(reply.body.starts_with(marker.as_bytes()), "{tail}");
                assert_eq!(reply.header("accept-ranges"), "none", "{tail}");
            }
        }
    }
}

/// Finamp 1.0.1: direct play and downloads via `/Items/{id}/File`,
/// transcoded downloads via `/Audio/{id}/Universal` (capital U), and
/// transcoded playback via the HLS playlist and its one segment.
#[tokio::test]
async fn finamp_file_universal_and_hls_routes() {
    let fx = fixture().await;
    let file = format!("/jellyfin/Items/{}/File?ApiKey={ALICE}", fx.track1);
    let direct = fx.get(&file, None).await;
    assert_eq!((direct.status, direct.body.len()), (200, 1024));
    assert_eq!(direct.header("accept-ranges"), "bytes");
    let head = fx.send("HEAD", &file, None, b"").await;
    assert_eq!((head.status, head.body.len()), (200, 0));
    assert_eq!(head.header("content-length"), "1024");
    let anonymous = fx
        .get(&format!("/jellyfin/Items/{}/File", fx.track1), None)
        .await;
    assert_eq!(anonymous.status, 401);

    let audio = |tail: &str| format!("/jellyfin/Audio/{}/{tail}", fx.track1);
    let universal = fx
        .get(
            &audio(&format!("Universal?Container=mp3&ApiKey={ALICE}")),
            None,
        )
        .await;
    assert_eq!((universal.status, universal.body.len()), (200, 1024));

    let query = format!("audioCodec=aac&segmentContainer=ts&audioBitRate=128000&ApiKey={ALICE}");
    let playlist = fx.get(&audio(&format!("main.m3u8?{query}")), None).await;
    assert_eq!(playlist.status, 200);
    assert_eq!(
        playlist.header("content-type"),
        "application/vnd.apple.mpegurl"
    );
    let text = String::from_utf8(playlist.body).expect("utf-8 playlist");
    assert!(text.starts_with("#EXTM3U\n"), "{text}");
    assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD"), "{text}");
    assert!(text.trim_end().ends_with("#EXT-X-ENDLIST"), "{text}");
    let segment = text
        .lines()
        .find(|line| !line.starts_with('#'))
        .expect("one segment line");
    assert_eq!(segment, format!("main.ts?{query}"));
    let head = fx
        .send("HEAD", &audio(&format!("main.m3u8?{query}")), None, b"")
        .await;
    assert_eq!((head.status, head.body.len()), (200, 0));
    assert_eq!(head.header("content-length"), text.len().to_string());
    let ts = fx.get(&audio(segment), None).await;
    assert_eq!(ts.status, 200);
    assert!(
        ts.body.starts_with(b"transcoded:aac-ts:128:"),
        "{:?}",
        String::from_utf8_lossy(&ts.body)
    );

    // No ffmpeg: there is no HLS to offer.
    let off = JellyfinSettings {
        ffmpeg_available: false,
        ..settings()
    };
    let reply = send(
        fx.app(off),
        "GET",
        &audio(&format!("main.m3u8?{query}")),
        None,
        b"",
    )
    .await;
    assert_eq!(reply.status, 404);
}

/// The transcode policy shared by `/Audio` and PlaybackInfo.
#[test]
fn transcode_decision_rules() {
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
    // Nothing requested, a zero ceiling, force-original or transcoding off
    // all stream direct.
    assert_eq!(decide(&input(None, None, false)), StreamPlan::Direct);
    assert_eq!(decide(&input(None, Some(0), false)), StreamPlan::Direct);
    assert_eq!(decide(&input(Some("opus"), None, true)), StreamPlan::Direct);
    let mut off = input(Some("opus"), None, false);
    off.transcoding_enabled = false;
    assert_eq!(decide(&off), StreamPlan::Direct);
    // A codec or a ceiling triggers; the server max caps but never triggers.
    assert!(matches!(
        decide(&input(None, Some(64), false)),
        StreamPlan::Transcode {
            bitrate_kbps: 64,
            ..
        }
    ));
    assert!(matches!(
        decide(&input(Some("opus"), Some(999), false)),
        StreamPlan::Transcode { ref format, bitrate_kbps: 320, .. } if format == "opus"
    ));
    // Unusable formats fall back to the default; negative starts clamp.
    let mut start = input(Some("wav"), None, false);
    start.start_seconds = -5.0;
    assert!(matches!(
        decide(&start),
        StreamPlan::Transcode { ref format, start_seconds, .. }
            if format == "mp3" && start_seconds == 0.0
    ));
}

#[tokio::test]
async fn playback_info_urls_play_and_transcode_fields_follow_policy() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let info_uri = format!("/jellyfin/Items/{}/PlaybackInfo", fx.track1);
    // The advertised direct URL plays with no auth header (Jellify, Manet).
    let info = fx.json(&info_uri, Some(&auth)).await;
    let source = &info["MediaSources"][0];
    assert!(source.get("TranscodingUrl").is_none());
    let url = source["DirectStreamUrl"].as_str().expect("direct url");
    let path = url.strip_prefix("http://localhost").expect("origin");
    let played = fx.get(path, None).await;
    assert_eq!((played.status, played.body.len()), (200, 1024));

    // A low ceiling in the query adds the transcoding fields.
    let low = fx
        .json(
            &format!("{info_uri}?maxStreamingBitrate=64000"),
            Some(&auth),
        )
        .await;
    let source = &low["MediaSources"][0];
    let transcode_url = source["TranscodingUrl"].as_str().expect("transcoding url");
    assert!(transcode_url.contains("/jellyfin/Audio/") && transcode_url.contains("AudioCodec=mp3"));
    assert_eq!(source["TranscodingSubProtocol"], "http");
    assert_eq!(source["TranscodingContainer"], "mp3");
    // Same through a POST body; an opus default rides the ogg container.
    let mut opus = settings();
    opus.transcode_default_format = "opus".to_owned();
    let body = json!({"MaxStreamingBitrate": 64000, "UserId": "zzz"}).to_string();
    let posted = send(
        fx.app(opus),
        "POST",
        &info_uri,
        Some(&auth),
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        posted.json()["MediaSources"][0]["TranscodingContainer"],
        "ogg"
    );

    let unknown = format!("/jellyfin/Items/{}/PlaybackInfo", "c".repeat(32));
    assert_eq!(fx.get(&unknown, Some(&auth)).await.status, 404);
}

#[tokio::test]
async fn session_reports_drive_presence_and_scrobbles() {
    let auth = finamp(ALICE);
    let alice = || "user-alice".to_owned();
    let fx = fixture().await;
    let post = |uri: &'static str, body: Value| {
        let fx = &fx;
        let auth = auth.clone();
        async move {
            let bytes = if body.is_null() {
                Vec::new()
            } else {
                body.to_string().into_bytes()
            };
            fx.send("POST", uri, Some(&auth), &bytes).await.status
        }
    };
    // Empty bodies, pings and capabilities accept silently.
    assert_eq!(post("/jellyfin/Sessions/Playing", Value::Null).await, 204);
    assert_eq!(
        post("/jellyfin/Sessions/Playing/Ping", Value::Null).await,
        204
    );
    assert_eq!(
        post("/jellyfin/Sessions/Capabilities/Full", json!({})).await,
        204
    );
    assert!(fx.sessions.calls().is_empty());
    // Start records the session and now-playing; progress converts ticks
    // to milliseconds and never scrobbles.
    let start = json!({"ItemId": fx.track1, "PlaySessionId": "ps-1"});
    assert_eq!(post("/jellyfin/Sessions/Playing", start).await, 204);
    let progress = json!({"ItemId": fx.track1, "PositionTicks": 20_000_000_i64});
    assert_eq!(
        post("/jellyfin/Sessions/Playing/Progress", progress).await,
        204
    );
    assert_eq!(
        fx.sessions.calls(),
        vec![
            SessionCall::MarkStarted {
                user_id: alice(),
                key: "ps-1".to_owned()
            },
            SessionCall::NowPlaying {
                user_id: alice(),
                file_id: "f1".to_owned()
            },
            SessionCall::Progress {
                user_id: alice(),
                file_id: "f1".to_owned(),
                position_ms: Some(2000),
                paused: false,
            },
        ]
    );

    // Stopped always clears presence; it scrobbles past 90%, or when the
    // position is omitted, but never below the threshold or on failure.
    let runtime = 1_800_000_000_i64;
    for (body, scrobbles) in [
        (
            json!({"PositionTicks": 1_700_000_000_i64, "RunTimeTicks": runtime}),
            true,
        ),
        (
            json!({"PositionTicks": 100_000_000_i64, "RunTimeTicks": runtime}),
            false,
        ),
        (json!({}), true),
        (json!({"Failed": true}), false),
    ] {
        let fx = fixture().await;
        let mut body = body;
        body["ItemId"] = json!(fx.track1);
        let reply = fx
            .send(
                "POST",
                "/jellyfin/Sessions/Playing/Stopped",
                Some(&auth),
                body.to_string().as_bytes(),
            )
            .await;
        assert_eq!(reply.status, 204);
        let calls = fx.sessions.calls();
        assert_eq!(
            calls[0],
            SessionCall::ClearPresence { user_id: alice() },
            "{body}"
        );
        let scrobbled = calls.contains(&SessionCall::Scrobble {
            user_id: alice(),
            file_id: "f1".to_owned(),
        });
        assert_eq!(scrobbled, scrobbles, "{body}");
    }
}

#[tokio::test]
async fn playlists_create_add_move_remove() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let entries = |pid: &str| format!("/jellyfin/Playlists/{pid}/Items");
    // Unknown ids in the create body are skipped.
    let body = json!({"Name": "Road", "Ids": [fx.track1, "b".repeat(32)]}).to_string();
    let created = fx
        .send("POST", "/jellyfin/Playlists", Some(&auth), body.as_bytes())
        .await;
    let pid = created.json()["Id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    let detail = fx
        .json(&format!("/jellyfin/Playlists/{pid}"), Some(&auth))
        .await;
    assert_eq!(detail["Name"], "Road");
    assert_eq!(detail["ItemIds"], json!([fx.track1]));
    let page = fx.json(&entries(&pid), Some(&auth)).await;
    let entry = page["Items"][0]["PlaylistItemId"]
        .as_str()
        .expect("entry id")
        .to_owned();

    // Add a second track, move the first entry to index 1, then remove it.
    let add = format!("{}?ids={}", entries(&pid), fx.track2);
    assert_eq!(fx.send("POST", &add, Some(&auth), b"").await.status, 204);
    let moved = format!("{}/{entry}/Move/1", entries(&pid));
    assert_eq!(fx.send("POST", &moved, Some(&auth), b"").await.status, 204);
    let page = fx.json(&entries(&pid), Some(&auth)).await;
    assert_eq!(page["Items"][0]["Name"], "Deep Cut");
    assert_eq!(page["Items"][1]["Name"], "Opener");
    let remove = format!("{}?entryIds={entry}", entries(&pid));
    assert_eq!(
        fx.send("DELETE", &remove, Some(&auth), b"").await.status,
        204
    );
    let browse = fx
        .json("/jellyfin/Items?IncludeItemTypes=Playlist", Some(&auth))
        .await;
    assert_eq!(browse["Items"][0]["ChildCount"], 1);

    // Query-parameter creation defaults the name.
    let created = fx
        .send(
            "POST",
            &format!("/jellyfin/Playlists?ids={}", fx.track3),
            Some(&auth),
            b"",
        )
        .await;
    let pid2 = created.json()["Id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    let detail = fx
        .json(&format!("/jellyfin/Playlists/{pid2}"), Some(&auth))
        .await;
    assert_eq!(detail["Name"], "Playlist");

    // A playlist id on a track route is a bare 404, never a native envelope.
    let reply = fx
        .get(&format!("/jellyfin/Audio/{pid}/stream"), Some(&auth))
        .await;
    assert_eq!((reply.status, reply.body.len()), (404, 0));
}

#[tokio::test]
async fn token_transports_and_bad_tokens() {
    let fx = fixture().await;
    let bad = fx
        .get("/jellyfin/Items", Some(&finamp("wrong-secret")))
        .await;
    assert_eq!((bad.status, bad.body.len()), (401, 0));
    let by_query = fx
        .get(&format!("/jellyfin/Items?api_key={ALICE}&Limit=0"), None)
        .await;
    assert_eq!(by_query.status, 200);
}
