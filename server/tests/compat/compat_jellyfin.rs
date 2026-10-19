//! Jellyfin answers with transcoding on: the user policy that offers it,
//! transcoded audio, HLS and the PlaybackInfo transcoding fields. Runs the
//! real router over the in-memory `fake` seams; ids are the deterministic
//! `sha256("kind:internal")[:32]`. The rest of the protocol runs on the
//! real library in `compat_jellyfin_library`, whose app turns transcoding
//! off: a server transcodes only with ffmpeg on its PATH. These stay until
//! the test app can stand in for ffmpeg.

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use droppedneedle_auth::compat_auth::jellyfin::server_id;
use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_compat::jellyfin::fake::{FakeLibrary, MemoryEngine, MemoryIds, MemorySessions};
use droppedneedle_compat::jellyfin::seams::{
    AlbumView, ArtistView, GenreView, IdMap, JellyfinSettings, TrackView,
};
use droppedneedle_compat::jellyfin::{JellyfinState, routes};
use droppedneedle_compat::shared::address::ListenerConfig;
use droppedneedle_kernel::config::TrustedProxies;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::{AuthRig, PASSWORD};

const ALICE: &str = "alice-app-secret";
const BOB: &str = "bob-app-secret";

struct Fx {
    _rig: AuthRig,
    passwords: ProdCompatPasswords,
    library: FakeLibrary,
    engine: MemoryEngine,
    sessions: MemorySessions,
    ids: MemoryIds,
    track1: String,
    album1: String,
}

async fn fixture() -> Fx {
    let rig = AuthRig::new("compat-jellyfin").await;
    for (name, role, secret) in [("alice", Role::User, ALICE), ("bob", Role::Admin, BOB)] {
        let user = rig.seed_user(name, role).await;
        rig.seed_app_password(&user.id, secret).await;
    }
    let passwords = rig.compat_passwords();
    let library = FakeLibrary::new();
    let engine = MemoryEngine::new();
    seed_jellyfin(&library, &engine);
    let ids = MemoryIds::new();
    Fx {
        track1: ids.to_jf("track", "f1").await,
        album1: ids.to_jf("album", "rg-1").await,
        _rig: rig,
        passwords,
        library,
        engine,
        sessions: MemorySessions::new(),
        ids,
    }
}

/// Seeds the Jellyfin fixture library: two artists, two albums, three
/// tracks (mp3 1024 bytes, flac 512 bytes, mp3 256 bytes), one genre, art.
fn seed_jellyfin(library: &FakeLibrary, engine: &MemoryEngine) {
    for (mbid, name, albums, added, album_artist) in [
        ("mb-a1", "Blue Giant", 2, 900.0, true),
        ("mb-a2", "Guest Star", 0, 950.0, false),
    ] {
        library.add_artist(
            ArtistView {
                id: mbid.to_owned(),
                artist_mbid: Some(mbid.to_owned()),
                name: name.to_owned(),
                album_count: albums,
                appearance_album_count: 0,
                track_count: 0,
                total_duration_seconds: 0.0,
                date_added: Some(added),
                starred: false,
                image_tag: None,
            },
            album_artist,
        );
    }
    for (rg, title, year, tracks, seconds, added, plays, last_played) in [
        (
            "rg-1",
            "First Light",
            2021,
            2,
            380.0,
            1000.0,
            5,
            Some(5000.0),
        ),
        ("rg-2", "Second Dawn", 2023, 1, 240.0, 2000.0, 0, None),
    ] {
        library.add_album(AlbumView {
            id: rg.to_owned(),
            release_group_mbid: Some(rg.to_owned()),
            release_mbid: None,
            artist_musicbrainz_id: None,
            title: title.to_owned(),
            artist_name: Some("Blue Giant".to_owned()),
            artist_id: Some("mb-a1".to_owned()),
            year: Some(year),
            release_date: None,
            genre: Some("Rock".to_owned()),
            track_count: tracks,
            total_duration_seconds: Some(seconds),
            date_added: Some(added),
            starred: false,
            played: plays > 0,
            play_count: plays,
            last_played,
            image_tag: None,
        });
    }
    let track = |id: &str, title: &str, rg: &str, album: &str, artist: (&str, &str)| TrackView {
        file_id: id.to_owned(),
        title: title.to_owned(),
        album_id: Some(rg.to_owned()),
        album_title: Some(album.to_owned()),
        artist_id: Some(artist.0.to_owned()),
        artist_name: Some(artist.1.to_owned()),
        album_artist_id: Some("mb-a1".to_owned()),
        album_artist_name: Some("Blue Giant".to_owned()),
        disc_number: Some(1),
        genre: Some("Rock".to_owned()),
        ..TrackView::default()
    };
    let blue = ("mb-a1", "Blue Giant");
    library.add_track(TrackView {
        duration_seconds: Some(180.0),
        year: Some(2021),
        track_number: Some(1),
        file_format: Some("mp3".to_owned()),
        bitrate: Some(320),
        channels: Some(2),
        sample_rate: Some(44100),
        file_size_bytes: Some(1024),
        created_at: Some(1000.0),
        recording_mbid: Some("rec-1".to_owned()),
        play_count: 5,
        last_played: Some(5000.0),
        ..track("f1", "Opener", "rg-1", "First Light", blue)
    });
    library.add_track(TrackView {
        duration_seconds: Some(200.0),
        year: Some(2021),
        track_number: Some(2),
        file_format: Some("flac".to_owned()),
        bitrate: Some(900),
        file_size_bytes: Some(512),
        created_at: Some(1100.0),
        ..track("f2", "Deep Cut", "rg-1", "First Light", blue)
    });
    library.add_track(TrackView {
        duration_seconds: Some(240.0),
        year: Some(2023),
        track_number: Some(1),
        file_format: Some("mp3".to_owned()),
        bitrate: Some(128),
        file_size_bytes: Some(256),
        created_at: Some(2000.0),
        ..track(
            "f3",
            "Late Bloomer",
            "rg-2",
            "Second Dawn",
            ("mb-a2", "Guest Star"),
        )
    });
    library.add_genre(GenreView {
        name: "Rock".to_owned(),
        song_count: 3,
    });
    library.add_cover("rg-1", "500", b"cover-500-bytes".to_vec(), "image/jpeg");
    library.add_cover("rg-1", "250", b"cover-250-bytes".to_vec(), "image/jpeg");
    library.add_artist_image("mb-a1", b"artist-bytes".to_vec(), "image/jpeg");
    engine.add_file(
        "f1",
        (0..1024).map(|i| (i % 251) as u8).collect(),
        Some("mp3"),
    );
    engine.add_file("f2", vec![7u8; 512], Some("flac"));
    engine.add_file("f3", vec![9u8; 256], Some("mp3"));
}

fn settings() -> JellyfinSettings {
    JellyfinSettings {
        enabled: true,
        ..JellyfinSettings::default()
    }
}

impl Fx {
    fn app(&self, settings: JellyfinSettings) -> Router {
        routes(JellyfinState::new(
            self.passwords.clone(),
            self.library.clone(),
            self.engine.clone(),
            self.sessions.clone(),
            self.ids.clone(),
            settings,
            ListenerConfig {
                port: 8096,
                trusted_proxies: TrustedProxies::default(),
                base_path: String::new(),
            },
        ))
        .into_parts()
        .0
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

/// Exact-key-set JSON comparison; `__UUID__` and `__ISO_NET__` match by
/// shape.
fn assert_shape(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::String(got), Value::String(exp)) if exp == "__ISO_NET__" => {
            // `2026-09-28T12:00:00.1234560Z`, as Jellyfin writes a date
            let shape =
                regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{7}Z$").expect("regex");
            assert!(shape.is_match(got), "{path}: {got} is not a Jellyfin date");
        }
        (Value::String(got), Value::String(exp)) if exp == "__UUID__" => {
            assert!(
                got.len() == 32 && got.bytes().all(|b| b.is_ascii_hexdigit()),
                "{path}: {got} is not a 32-hex id"
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
    let mut user = json!({
        "Id": id,
        "Name": name,
        "ServerId": server_id(),
        "HasPassword": true,
        "HasConfiguredPassword": true,
        "HasConfiguredEasyPassword": false,
        "EnableAutoLogin": false,
        "LastLoginDate": "__ISO_NET__",
        "LastActivityDate": "__ISO_NET__",
        "Configuration": {
            "PlayDefaultAudioTrack": true,
            "SubtitleLanguagePreference": "",
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
            "CastReceiverId": "",
        },
    });
    user["Policy"] = json!({
        "IsAdministrator": is_admin,
        "IsHidden": true,
        "IsDisabled": false,
        "EnableAllFolders": true,
        "EnabledFolders": [],
        "EnableAllChannels": false,
        "EnabledChannels": [],
        "EnableAllDevices": true,
        "EnabledDevices": [],
        "EnableMediaPlayback": true,
        "EnableAudioPlaybackTranscoding": true,
        "EnableVideoPlaybackTranscoding": false,
        "EnablePlaybackRemuxing": false,
        "EnableContentDownloading": true,
        "EnableRemoteAccess": true,
        "EnableSyncTranscoding": false,
        "EnableUserPreferenceAccess": false,
        "EnableLiveTvAccess": false,
        "EnableLiveTvManagement": false,
        "EnableContentDeletion": false,
        "EnableMediaConversion": false,
        "EnablePublicSharing": false,
        "EnableRemoteControlOfOtherUsers": false,
        "EnableSharedDeviceControl": false,
        "InvalidLoginAttemptCount": 0,
        "RemoteClientBitrateLimit": 0,
        "SyncPlayAccess": "None",
        "BlockedTags": [],
        "AllowedTags": [],
        "AccessSchedules": [],
        "BlockUnratedItems": [],
    });
    // In parts: one `json!` this long passes the macro's recursion limit.
    let more = json!({
        "EnableCollectionManagement": false,
        "EnableSubtitleManagement": false,
        "EnableLyricManagement": false,
        "ForceRemoteSourceTranscoding": false,
        "EnableContentDeletionFromFolders": [],
        "LoginAttemptsBeforeLockout": -1,
        "MaxActiveSessions": 0,
        "BlockedMediaFolders": [],
        "BlockedChannels": [],
        "AuthenticationProviderId": "",
        "PasswordResetProviderId": "",
    });
    for (key, value) in more.as_object().expect("object") {
        user["Policy"][key] = value.clone();
    }
    user
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
                "LastActivityDate": "__ISO_NET__",
                "DeviceName": "",
                "IsActive": true,
                "SupportsRemoteControl": false,
                "SupportsMediaControl": false,
                "HasCustomDeviceName": false,
                "ServerId": server_id(),
                "PlayState": {
                    "CanSeek": false,
                    "IsPaused": false,
                    "IsMuted": false,
                    "RepeatMode": "RepeatNone",
                    "PlaybackOrder": "Default",
                },
                "AdditionalUsers": [],
                "Capabilities": {
                    "PlayableMediaTypes": [],
                    "SupportedCommands": [],
                    "SupportsMediaControl": false,
                    "SupportsPersistentIdentifier": false,
                },
                "RemoteEndPoint": "",
                "PlayableMediaTypes": [],
                "LastPlaybackCheckIn": "0001-01-01T00:00:00.0000000Z",
                "ApplicationVersion": "",
                "NowPlayingQueue": [],
                "NowPlayingQueueFullItems": [],
                "SupportedCommands": [],
            },
            "ServerId": server_id(),
        }),
        "$",
    );

    // Client, device and version facts from the header land in SessionInfo.
    let header = "MediaBrowser Client=\"Jellify\", Device=\"Pixel\", DeviceId=\"abc\", \
                  Version=\"1.2.3\", Token=\"unused\"";
    let body = json!({"Username": "bob", "Pw": BOB}).to_string();
    let bob = fx
        .send("POST", login, Some(header), body.as_bytes())
        .await
        .json();
    assert_shape(
        &bob["User"],
        &expected_user("bob", "user-bob", true),
        "$.User",
    );
    assert_eq!(bob["SessionInfo"]["Client"], "Jellify");
    assert_eq!(bob["SessionInfo"]["DeviceName"], "Pixel");
    assert_eq!(bob["SessionInfo"]["DeviceId"], "abc");
    assert_eq!(bob["SessionInfo"]["ApplicationVersion"], "1.2.3");
    assert_eq!(
        bob["SessionInfo"]["Capabilities"]["SupportsPersistentIdentifier"],
        true
    );

    // Wrong app password and the account password both fail; no body is 400.
    for pw in ["nope", PASSWORD] {
        let body = json!({"Username": "alice", "Pw": pw}).to_string();
        let reply = fx.send("POST", login, None, body.as_bytes()).await;
        assert_eq!((reply.status, reply.body.len()), (401, 0), "{pw}");
    }
    assert_eq!(fx.send("POST", login, None, b"").await.status, 400);

    // `/Users/Me` and `/Users/{any id}` both return the caller, equal to
    // the login echo's User but for the two dates, which move.
    let auth = finamp(ALICE);
    let mut signed_in = echo["User"].clone();
    for date in ["LastLoginDate", "LastActivityDate"] {
        signed_in[date] = json!("__ISO_NET__");
    }
    for uri in ["/jellyfin/Users/Me", "/jellyfin/Users/someone-else"] {
        assert_shape(&fx.json(uri, Some(&auth)).await, &signed_in, uri);
    }

    // Transcoded audio is offered only while it can be served.
    let transcoding = "EnableAudioPlaybackTranscoding";
    for cannot in [
        JellyfinSettings {
            ffmpeg_available: false,
            ..settings()
        },
        JellyfinSettings {
            transcoding_enabled: false,
            ..settings()
        },
    ] {
        let echo = send(fx.app(cannot.clone()), "POST", login, None, body.as_bytes()).await;
        assert_eq!(echo.json()["User"]["Policy"][transcoding], false);
        let me = send(
            fx.app(cannot),
            "GET",
            "/jellyfin/Users/Me",
            Some(&auth),
            b"",
        )
        .await;
        assert_eq!(me.json()["Policy"][transcoding], false);
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

#[tokio::test]
async fn playback_info_urls_play_and_transcode_fields_follow_policy() {
    let fx = fixture().await;
    let auth = finamp(ALICE);
    let info_uri = format!("/jellyfin/Items/{}/PlaybackInfo", fx.track1);
    // The advertised direct URL plays with no auth header (Jellify, Manet).
    let info = fx.json(&info_uri, Some(&auth)).await;
    let source = &info["MediaSources"][0];
    assert!(source.get("TranscodingUrl").is_none());
    assert_eq!(source["SupportsTranscoding"], true);
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
