//! The Jellyfin wire contract over the whole app and a library its own
//! scanner filled: gates and system routes, login, the query dialects of
//! Finamp, Jellify and Manet, pictures, both favourite dialects, played
//! markers, audio routes, PlaybackInfo, session reports, playlists and
//! token transports.
//!
//! The rig is given no ffmpeg, so nothing transcodes and every answer is
//! the same on a host with ffmpeg and one without. The transcoding rig is
//! given the committed stand-in, which answers one line naming the muxer
//! and bitrate it was asked for.
//!
//! The app locks out an address after five failed sign-ins. Requests that
//! are meant to fail come through a proxy from an address of their own,
//! so one refusal never turns the next answer into a 429.

use std::sync::atomic::{AtomicU8, Ordering};

use axum::http::{HeaderMap, StatusCode};
use droppedneedle_auth::compat_auth::jellyfin::server_id;
use droppedneedle_media::stream::transcode::{
    OutFormat, StreamPlan, TrackInfo, TranscodeSettings, decide,
};
use serde_json::{Value, json};

use crate::common::compat::{CompatApp, PLAYER, named};
use droppedneedle_compat::jellyfin::{Access, ROUTES, Verb};

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("json body")
    }

    /// Status and body length, for the empty answers.
    fn bare(&self) -> (u16, usize) {
        (self.status.as_u16(), self.body.len())
    }
}

/// The authorization header Finamp sends.
fn finamp(token: &str) -> String {
    format!("MediaBrowser Client=\"Finamp\", Device=\"Test\", DeviceId=\"dev1\", Token=\"{token}\"")
}

/// One request, asked again while the app answers busy.
async fn raw(
    rig: &CompatApp,
    method: &str,
    uri: &str,
    mut headers: Vec<(&str, &str)>,
    auth: Option<&str>,
    body: Option<Value>,
) -> Reply {
    if let Some(auth) = auth {
        headers.push(("authorization", auth));
    }
    let (status, headers, body) =
        crate::common::through_busy(|| rig.app.raw(method, uri, &headers, body.clone())).await;
    Reply {
        status,
        headers,
        body,
    }
}

async fn send(
    rig: &CompatApp,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    body: Option<Value>,
) -> Reply {
    raw(rig, method, uri, Vec::new(), auth, body).await
}

async fn get(rig: &CompatApp, uri: &str, auth: Option<&str>) -> Reply {
    send(rig, "GET", uri, auth, None).await
}

async fn json(rig: &CompatApp, uri: &str, auth: &str) -> Value {
    let reply = get(rig, uri, Some(auth)).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri}");
    reply.json()
}

/// A request that is meant to be refused, sent through the local proxy
/// from an address no other refusal used.
async fn refused(
    rig: &CompatApp,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    body: Option<Value>,
) -> Reply {
    static NEXT: AtomicU8 = AtomicU8::new(1);
    let from = format!("198.51.100.{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let headers = vec![("x-forwarded-for", from.as_str())];
    raw(rig, method, uri, headers, auth, body).await
}

/// The id of the item called `name` in the listing at `uri`.
async fn id_of(rig: &CompatApp, auth: &str, uri: &str, name: &str) -> String {
    let page = json(rig, uri, auth).await;
    named(&page["Items"], "Name", name)["Id"]
        .as_str()
        .expect("item id")
        .to_owned()
}

async fn track_id(rig: &CompatApp, auth: &str, name: &str) -> String {
    id_of(rig, auth, "/jellyfin/Items?IncludeItemTypes=Audio", name).await
}

async fn album_id(rig: &CompatApp, auth: &str, name: &str) -> String {
    id_of(
        rig,
        auth,
        "/jellyfin/Items?IncludeItemTypes=MusicAlbum",
        name,
    )
    .await
}

async fn artist_id(rig: &CompatApp, auth: &str, name: &str) -> String {
    id_of(rig, auth, "/jellyfin/Artists", name).await
}

/// The play count the caller sees on the track `id`.
async fn play_count(rig: &CompatApp, auth: &str, id: &str) -> i64 {
    json(rig, &format!("/jellyfin/Items/{id}"), auth).await["UserData"]["PlayCount"]
        .as_i64()
        .expect("PlayCount")
}

/// Play the track `id` to the end, as a player reports it, and wait until
/// the caller sees the play.
async fn play(rig: &CompatApp, auth: &str, id: &str) {
    let before = play_count(rig, auth, id).await;
    let stopped = send(
        rig,
        "POST",
        "/jellyfin/Sessions/Playing/Stopped",
        Some(auth),
        Some(json!({"ItemId": id})),
    )
    .await;
    assert_eq!(stopped.status, StatusCode::NO_CONTENT);
    rig.app
        .until("the play is counted", || async {
            (play_count(rig, auth, id).await > before).then_some(())
        })
        .await;
}

/// Turn the Jellyfin API on or off in Settings, as an admin does.
async fn set_jellyfin(rig: &CompatApp, enabled: bool) {
    let admin = rig.app.login_admin().await;
    let admin = [("authorization", admin.as_str())];
    let (status, mut apps) = rig
        .app
        .call("GET", "/api/v3/settings/connect-apps", &admin, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{apps}");
    apps["jellyfin_enabled"] = json!(enabled);
    let (status, saved) = rig
        .app
        .call("PUT", "/api/v3/settings/connect-apps", &admin, Some(apps))
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
}

/// Exact-key-set JSON comparison; `__UUID__` and `__ISO_NET__` match by
/// shape.
fn assert_shape(actual: &Value, expected: &Value, path: &str) {
    match (actual, expected) {
        (Value::String(got), Value::String(exp)) if exp == "__ISO_NET__" => {
            assert!(is_net_date(got), "{path}: {got} is not a Jellyfin date");
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

/// `2026-09-28T12:00:00.1234560Z`, as Jellyfin writes a date: seven
/// fractional digits even on whole seconds (Manet rejects fewer).
fn is_net_date(text: &str) -> bool {
    regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{7}Z$")
        .expect("regex")
        .is_match(text)
}

/// The user DTO every Jellyfin client parses at login; Finamp rejects the
/// session when any of these keys is missing.
fn expected_user(name: &str, id: &str) -> Value {
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
        "IsAdministrator": false,
        "IsHidden": true,
        "IsDisabled": false,
        "EnableAllFolders": true,
        "EnabledFolders": [],
        "EnableAllChannels": false,
        "EnabledChannels": [],
        "EnableAllDevices": true,
        "EnabledDevices": [],
        "EnableMediaPlayback": true,
        // The rig has no ffmpeg, and transcoding is offered only while it
        // can be served.
        "EnableAudioPlaybackTranscoding": false,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gates_and_system_routes() {
    let rig = CompatApp::start("jf-gates").await;
    let auth = finamp(&rig.secret);

    let denied = refused(&rig, "GET", "/jellyfin/System/Info", None, None).await;
    assert_eq!(denied.bare(), (401, 0));
    let info = get(&rig, "/jellyfin/System/Info", Some(&auth)).await;
    assert_eq!(info.header("content-type"), "application/json");
    let mut body = info.json();
    // Both depend on the build host; only that they are named is pinned.
    for key in ["OperatingSystemDisplayName", "SystemArchitecture"] {
        let value = body.as_object_mut().expect("object").remove(key);
        assert!(
            value.is_some_and(|value| value.as_str().is_some_and(|text| !text.is_empty())),
            "{key} is named"
        );
    }
    assert_shape(
        &body,
        &json!({
            "LocalAddress": "http://test.local/jellyfin",
            "ServerName": "DroppedNeedle",
            "Version": "10.10.6",
            "ProductName": "Jellyfin Server",
            "OperatingSystem": "",
            "Id": server_id(),
            "StartupWizardCompleted": true,
            "HasPendingRestart": false,
            "IsShuttingDown": false,
            "SupportsLibraryMonitor": true,
            "WebSocketPortNumber": rig.app.config.port,
            "CompletedInstallations": [],
            "CanSelfRestart": false,
            "CanLaunchWebBrowser": false,
            "ProgramDataPath": "",
            "WebPath": "",
            "ItemsByNamePath": "",
            "CachePath": "",
            "LogPath": "",
            "InternalMetadataPath": "",
            "CastReceiverApplications": [],
            "HasUpdateAvailable": false,
            // The app was given no ffmpeg.
            "EncoderLocation": "NotFound",
            "TranscodingTempPath": "",
        }),
        "$",
    );
    // No host path reaches the answer, so no value starts with a slash.
    assert!(
        info.json()
            .as_object()
            .expect("object")
            .values()
            .all(|value| !value.as_str().unwrap_or("").starts_with('/')),
        "a System/Info value is a path"
    );
    // A trusted proxy (the app's peer here) that says https is believed.
    let forwarded = vec![
        ("x-forwarded-proto", "https"),
        ("x-forwarded-host", "music.example"),
    ];
    let proxied = raw(
        &rig,
        "GET",
        "/jellyfin/System/Info",
        forwarded,
        Some(&auth),
        None,
    )
    .await;
    assert_eq!(
        proxied.json()["LocalAddress"],
        "https://music.example/jellyfin"
    );

    let quick = get(&rig, "/jellyfin/QuickConnect/Enabled", None).await;
    assert_eq!(
        (quick.status, quick.body.as_slice()),
        (StatusCode::OK, &b"false"[..])
    );
    // Signing out needs sign-in, as on a real server, and ends nothing: the
    // token is the app password, which keeps working.
    let anonymous = refused(&rig, "POST", "/jellyfin/Sessions/Logout", None, None).await;
    assert_eq!(anonymous.bare(), (401, 0));
    let logout = send(&rig, "POST", "/jellyfin/Sessions/Logout", Some(&auth), None).await;
    assert_eq!(logout.bare(), (204, 0));
    assert_eq!(
        get(&rig, "/jellyfin/Users/Me", Some(&auth)).await.status,
        StatusCode::OK
    );

    // Turned off in Settings: every route 404s with an empty body, signed
    // in or not.
    set_jellyfin(&rig, false).await;
    for (uri, header) in [
        ("/jellyfin/System/Info/Public", None),
        ("/jellyfin/Items", Some(auth.as_str())),
    ] {
        assert_eq!(get(&rig, uri, header).await.bare(), (404, 0), "{uri}");
    }
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_echoes_the_app_password_and_users_me_matches() {
    let rig = CompatApp::start("jf-login").await;
    let login = "/jellyfin/Users/AuthenticateByName";
    // Finamp sends an undocumented `UserId`; the lenient decode accepts it.
    let body = json!({"Username": PLAYER, "Pw": rig.secret, "UserId": "zzz"});
    let echo = send(&rig, "POST", login, None, Some(body)).await;
    assert_eq!(echo.status, StatusCode::OK);
    let echo = echo.json();
    assert_shape(
        &echo,
        &json!({
            "User": expected_user(PLAYER, &rig.user_id),
            "AccessToken": rig.secret,
            "SessionInfo": {
                "Id": "__UUID__",
                "UserId": rig.user_id,
                "UserName": PLAYER,
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
                // The address the app sees the request come from.
                "RemoteEndPoint": "127.0.0.1",
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
    let body = json!({"Username": PLAYER, "Pw": rig.secret});
    let jellify = send(&rig, "POST", login, Some(header), Some(body))
        .await
        .json();
    assert_eq!(jellify["SessionInfo"]["Client"], "Jellify");
    assert_eq!(jellify["SessionInfo"]["DeviceName"], "Pixel");
    assert_eq!(jellify["SessionInfo"]["DeviceId"], "abc");
    assert_eq!(jellify["SessionInfo"]["ApplicationVersion"], "1.2.3");
    assert_eq!(
        jellify["SessionInfo"]["Capabilities"]["SupportsPersistentIdentifier"],
        true
    );

    // A wrong app password and the account password both fail; no body is
    // a 400.
    for pw in ["nope".to_owned(), format!("{PLAYER}-password-1234")] {
        let body = json!({"Username": PLAYER, "Pw": pw});
        let reply = refused(&rig, "POST", login, None, Some(body)).await;
        assert_eq!(reply.bare(), (401, 0), "{pw}");
    }
    assert_eq!(
        send(&rig, "POST", login, None, None).await.status,
        StatusCode::BAD_REQUEST
    );

    // `/Users/Me` and `/Users/{any id}` both answer the caller, equal to
    // the login echo's User but for the two dates, which move.
    let auth = finamp(&rig.secret);
    let mut signed_in = echo["User"].clone();
    for date in ["LastLoginDate", "LastActivityDate"] {
        signed_in[date] = json!("__ISO_NET__");
    }
    for uri in ["/jellyfin/Users/Me", "/jellyfin/Users/someone-else"] {
        assert_shape(&json(&rig, uri, &auth).await, &signed_in, uri);
    }
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browse_query_dialects() {
    let rig = CompatApp::start("jf-browse").await;
    let auth = finamp(&rig.secret);
    let names = |page: &Value| -> Vec<String> {
        let items = page.get("Items").unwrap_or(page);
        items
            .as_array()
            .expect("item list")
            .iter()
            .map(|item| item["Name"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    let airbag = track_id(&rig, &auth, "Airbag").await;
    let ok_computer = album_id(&rig, &auth, "OK Computer").await;
    let guest = artist_id(&rig, &auth, "Artist One").await;
    let radiohead = artist_id(&rig, &auth, "Radiohead").await;
    let views = json(&rig, "/jellyfin/UserViews", &auth).await;
    let library = views["Items"][0]["Id"]
        .as_str()
        .expect("view id")
        .to_owned();
    play(&rig, &auth, &airbag).await;

    let none = "f".repeat(32);
    let newest_year_first = vec![
        "望厚",
        "Now Thats Music",
        "Mezzanine",
        "OK Computer",
        "Achtung Baby",
    ];
    let by_name = vec![
        "Achtung Baby",
        "Mezzanine",
        "Now Thats Music",
        "OK Computer",
        "望厚",
    ];
    // Every artist with music is listed; the catalog's empty placeholders
    // ("Unknown Artist" and a second "Various Artists") are not.
    let every_artist = vec![
        "Artist One",
        "Massive Attack",
        "Radiohead",
        "U2",
        "Various Artists",
        "ユキ",
    ];
    let cases: Vec<(String, Vec<&str>)> =
        vec![
        // Lowercase query keys bind, as ASP.NET binds them; children come in
        // track order.
        (
            format!("/jellyfin/Items?parentid={ok_computer}"),
            vec!["Airbag", "Paranoid Android"],
        ),
        // MusicArtist outranks MusicAlbum in IncludeItemTypes.
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum,MusicArtist".to_owned(),
            every_artist.clone(),
        ),
        // Jellify's PremiereDate sort is year descending unless told otherwise.
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=PremiereDate".to_owned(),
            newest_year_first.clone(),
        ),
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=PremiereDate&SortOrder=Ascending"
                .to_owned(),
            newest_year_first.iter().rev().copied().collect(),
        ),
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&SortBy=Bogus".to_owned(),
            by_name.clone(),
        ),
        // StartIndex is the offset with or without SortBy, also when it is
        // not a multiple of Limit.
        (
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&StartIndex=3&Limit=2".to_owned(),
            by_name[3..].to_vec(),
        ),
        // An unknown contributor yields nothing, never the whole catalog;
        // a guest artist resolves to the albums they appear on.
        (
            format!("/jellyfin/Items?IncludeItemTypes=MusicAlbum&ContributingArtistIds={none}"),
            vec![],
        ),
        (
            format!("/jellyfin/Items?IncludeItemTypes=MusicAlbum&ContributingArtistIds={guest}"),
            vec!["Now Thats Music"],
        ),
        // Ids skips unknown ids; the played filter keeps what was played.
        (format!("/jellyfin/Items?Ids={airbag},{none}"), vec!["Airbag"]),
        (
            "/jellyfin/Items?IncludeItemTypes=Audio&Filters=IsPlayed".to_owned(),
            vec!["Airbag"],
        ),
        // A non-library ParentId on Latest is empty.
        (format!("/jellyfin/Items/Latest?ParentId={ok_computer}"), vec![]),
        (
            "/jellyfin/Artists".to_owned(),
            every_artist.clone(),
        ),
        (
            "/jellyfin/Artists/AlbumArtists".to_owned(),
            vec!["Massive Attack", "Radiohead", "U2", "Various Artists", "ユキ"],
        ),
        ("/jellyfin/Genres".to_owned(), vec!["Alternative Rock", "Rock"]),
        ("/jellyfin/MusicGenres".to_owned(), vec!["Alternative Rock", "Rock"]),
        // Similar and InstantMix resolve through the artist; unknown is empty.
        (
            format!("/jellyfin/Items/{airbag}/Similar"),
            vec!["Airbag", "Paranoid Android"],
        ),
        (
            format!("/jellyfin/Artists/{radiohead}/InstantMix"),
            vec!["Airbag", "Paranoid Android"],
        ),
        (format!("/jellyfin/Items/{none}/Similar"), vec![]),
    ];
    for (uri, expected) in cases {
        let page = json(&rig, &uri, &auth).await;
        if uri.contains("Latest") {
            assert!(page.is_array(), "{uri}: Latest is a bare array");
        }
        let items = page["Items"].as_array().or(page.as_array());
        for artist in items
            .into_iter()
            .flatten()
            .filter(|item| item["Type"] == "MusicArtist")
        {
            assert_ne!(artist["ChildCount"], 0, "{uri}: an empty artist is listed");
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
    // Asked for no total, the page's length is sent in its place, as
    // Jellyfin 10.10.6 does; asked for one, it is every match.
    for (types, every) in [("MusicAlbum", 5), ("Audio", 6)] {
        let uri = format!("/jellyfin/Items?IncludeItemTypes={types}&StartIndex=1&Limit=2");
        let counted = json(&rig, &uri, &auth).await;
        assert_eq!(counted["TotalRecordCount"], every, "{uri}");
        let uri = format!("{uri}&EnableTotalRecordCount=false");
        let uncounted = json(&rig, &uri, &auth).await;
        assert_eq!(names(&uncounted), names(&counted), "{uri}");
        assert_eq!(uncounted["TotalRecordCount"], 2, "{uri}");
        assert_eq!(uncounted["StartIndex"], 1, "{uri}");
    }
    // Latest is a bare array of albums, newest first; under the library it
    // is the same list, and Limit bounds it.
    let latest = json(&rig, "/jellyfin/Items/Latest", &auth).await;
    let mut all = names(&latest);
    assert!(latest.is_array(), "Latest is a bare array");
    assert_eq!(
        names(
            &json(
                &rig,
                &format!("/jellyfin/Items/Latest?ParentId={library}"),
                &auth
            )
            .await
        ),
        all
    );
    let one = json(&rig, "/jellyfin/Users/x/Items/Latest?Limit=1", &auth).await;
    assert!(one.is_array(), "Latest is a bare array");
    assert_eq!(names(&one), all[..1]);
    all.sort();
    assert_eq!(all, by_name);

    // Manet boots on these: the music view and the filter lists.
    assert_eq!(views["Items"][0]["CollectionType"], "music");
    let filters = json(&rig, "/jellyfin/Items/Filters", &auth).await;
    assert_eq!(
        filters,
        json!({
            "Genres": ["Alternative Rock", "Rock"],
            "Tags": [],
            "OfficialRatings": [],
            "Years": [1991, 1997, 1998, 2001, 2015],
        })
    );
    // .NET "O" dates keep seven fractional digits even on whole seconds.
    let album = json(&rig, &format!("/jellyfin/Items/{ok_computer}"), &auth).await;
    let created = album["DateCreated"].as_str().expect("DateCreated");
    assert!(is_net_date(created), "{created}");
    // Unknown ids and genre ids (a genre is no item) 404 with the
    // problem object.
    let genres = json(&rig, "/jellyfin/Genres", &auth).await;
    let genre = genres["Items"][0]["Id"]
        .as_str()
        .expect("genre id")
        .to_owned();
    for id in [none, genre] {
        let reply = get(&rig, &format!("/jellyfin/Items/{id}"), Some(&auth)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(reply.json()["title"], "Not Found", "{id}");
    }
    rig.stop().await;
}

/// An artist matched to MusicBrainz sends that id as `MusicBrainzArtist`,
/// on the artist and on its tracks. (An album's ids are pinned by
/// `an_album_sends_the_accepted_ids_else_its_tags`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identified_artist_sends_its_musicbrainz_id() {
    const MBID: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
    let rig = CompatApp::start("jf-artist-id").await;
    let auth = finamp(&rig.secret);
    let local: String = rig
        .app
        .db()
        .query_row(
            "SELECT a.album_artist_id FROM local_tracks t \
             JOIN local_albums a ON a.id = t.local_album_id \
             WHERE t.relative_path = 'Radiohead/OK Computer/01 Airbag.flac'",
            [],
            |row| row.get(0),
        )
        .expect("the album artist");
    rig.app
        .db()
        .execute(
            "INSERT OR REPLACE INTO local_artist_external_identities \
             (local_artist_id, provider, provider_artist_id, decision_source, selected_at) \
             VALUES (?1, 'musicbrainz', ?2, 'manual', 1000)",
            [local.as_str(), MBID],
        )
        .expect("artist identified");

    let artists = json(&rig, "/jellyfin/Artists", &auth).await;
    let radiohead = named(&artists["Items"], "Name", "Radiohead");
    assert_eq!(
        radiohead["ProviderIds"],
        json!({"MusicBrainzArtist": MBID}),
        "{radiohead}"
    );
    let airbag = track_id(&rig, &auth, "Airbag").await;
    let track = json(&rig, &format!("/jellyfin/Items/{airbag}"), &auth).await;
    assert_eq!(track["ProviderIds"]["MusicBrainzArtist"], MBID, "{track}");
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn images_serve_anonymously_with_immutable_cache() {
    let rig = CompatApp::start("jf-images").await;
    let auth = finamp(&rig.secret);
    let views = json(&rig, "/jellyfin/UserViews", &auth).await;
    let library_id = views["Items"][0]["Id"].as_str().expect("view id");
    let library = get(
        &rig,
        &format!("/jellyfin/Items/{library_id}/Images/Primary"),
        None,
    )
    .await;
    assert_eq!(
        (library.status, library.header("content-type")),
        (StatusCode::OK, "image/png")
    );
    // Asked for without a tag, as Finamp asks, a picture is kept a day.
    assert_eq!(library.header("cache-control"), "public, max-age=86400");
    assert!(library.body.starts_with(b"\x89PNG\r\n\x1a\n"));

    // A miss is a 404 with no placeholder (unlike Subsonic), and an album
    // or artist miss names the item. Only Primary serves.
    let mezzanine = album_id(&rig, &auth, "Mezzanine").await;
    let massive = artist_id(&rig, &auth, "Massive Attack").await;
    for (id, name) in [(&mezzanine, "Mezzanine"), (&massive, "Massive Attack")] {
        let reply = get(&rig, &format!("/jellyfin/Items/{id}/Images/Primary"), None).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{name}");
        assert_eq!(
            reply.json(),
            json!(format!("{name} does not have an image of type Primary")),
            "{name}"
        );
    }
    let backdrop = get(
        &rig,
        &format!("/jellyfin/Items/{mezzanine}/Images/Backdrop"),
        None,
    )
    .await;
    assert_eq!(backdrop.bare(), (404, 0));
    rig.stop().await;
}

/// A library or id read that fails is a server error with an empty body,
/// never an empty list or a missing item: an app that reads an empty 200
/// or a 404 drops what it holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_read_is_a_server_error_not_an_empty_answer() {
    let rig = CompatApp::start("jf-failed-read").await;
    let auth = finamp(&rig.secret);
    let mezzanine = album_id(&rig, &auth, "Mezzanine").await;
    let airbag = track_id(&rig, &auth, "Airbag").await;
    let picture = format!("/jellyfin/Items/{mezzanine}/Images/Primary");
    // The id fill has nothing left to write, so no pass races the renames.
    rig.jellyfin_ids_caught_up().await;
    assert_eq!(
        get(&rig, &picture, None).await.status,
        StatusCode::NOT_FOUND
    );

    rig.app
        .db()
        .execute_batch("ALTER TABLE local_albums RENAME TO local_albums_gone;")
        .expect("albums table renames");
    let items = get(&rig, "/jellyfin/Items", Some(&auth)).await;
    assert_eq!(items.bare(), (500, 0));

    rig.app
        .db()
        .execute_batch("ALTER TABLE compat_jellyfin_ids RENAME TO compat_jellyfin_ids_gone;")
        .expect("ids table renames");
    assert_eq!(get(&rig, &picture, None).await.bare(), (500, 0));

    // A playlist write reads every id first, so a failed read writes
    // nothing and asking again makes no second playlist.
    let create = format!("/jellyfin/Playlists?name=Retry&ids={airbag}");
    let created = send(&rig, "POST", &create, Some(&auth), None).await;
    assert_eq!(created.bare(), (500, 0));
    let playlists: i64 = rig
        .app
        .db()
        .query_row("SELECT COUNT(*) FROM playlists", [], |row| row.get(0))
        .expect("playlists count");
    assert_eq!(playlists, 0);
    rig.stop().await;
}

/// An artist with no MusicBrainz link advertises a picture when their
/// albums have art, and it is the mosaic the web UI shows, at the bucket
/// the size asks for. One with no art advertises none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unlinked_artist_shows_their_album_mosaic() {
    use crate::compat_subsonic_library::{GREEN, RED, assert_quadrants, two_covers_for_radiohead};
    let rig = CompatApp::start("jf-artist-mosaic").await;
    let auth = finamp(&rig.secret);
    two_covers_for_radiohead(&rig);
    let artists = json(&rig, "/jellyfin/Artists", &auth).await;
    let radiohead = named(&artists["Items"], "Name", "Radiohead");
    assert!(radiohead["ImageTags"]["Primary"].is_string(), "{radiohead}");
    let massive = named(&artists["Items"], "Name", "Massive Attack");
    assert_eq!(massive["ImageTags"], json!({}), "{massive}");
    let id = radiohead["Id"].as_str().expect("artist id");
    let reply = get(
        &rig,
        &format!("/jellyfin/Items/{id}/Images/Primary?fillWidth=200"),
        None,
    )
    .await;
    assert_eq!(
        (reply.status, reply.header("content-type")),
        (StatusCode::OK, "image/jpeg")
    );
    assert_quadrants(&reply.body, 250, [RED, GREEN, GREEN, RED]);
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn favorite_dialects_and_played_markers() {
    let rig = CompatApp::start("jf-favorites").await;
    let auth = finamp(&rig.secret);
    let airbag = track_id(&rig, &auth, "Airbag").await;
    let one = track_id(&rig, &auth, "One").await;
    play(&rig, &auth, &airbag).await;
    let favorites = "/jellyfin/Items?IncludeItemTypes=Audio&IsFavorite=true";
    // Finamp adds through the legacy path.
    let added = send(
        &rig,
        "POST",
        &format!("/jellyfin/Users/x/FavoriteItems/{airbag}"),
        Some(&auth),
        None,
    )
    .await;
    assert_eq!(added.status, StatusCode::OK);
    // The answer is the track's state as its own read gives it.
    let state = added.json();
    assert_eq!(
        (
            &state["ItemId"],
            &state["IsFavorite"],
            &state["Played"],
            &state["PlayCount"]
        ),
        (&json!(airbag), &json!(true), &json!(true), &json!(1)),
        "{state}"
    );
    assert!(state["LastPlayedDate"].is_string(), "{state}");
    for uri in [
        favorites,
        "/jellyfin/Items?IncludeItemTypes=Audio&Filters=IsFavorite",
    ] {
        let page = json(&rig, uri, &auth).await;
        assert_eq!(page["TotalRecordCount"], 1, "{uri}");
        assert_eq!(page["Items"][0]["UserData"]["IsFavorite"], true, "{uri}");
    }
    // The played filter narrows favourites too: the one favourite is played.
    for (filters, total) in [("IsFavorite,IsPlayed", 1), ("IsFavorite,IsUnplayed", 0)] {
        let uri = format!("/jellyfin/Items?IncludeItemTypes=Audio&Filters={filters}");
        assert_eq!(
            json(&rig, &uri, &auth).await["TotalRecordCount"],
            total,
            "{uri}"
        );
    }
    // Jellify removes through the modern path.
    let removed = send(
        &rig,
        "DELETE",
        &format!("/jellyfin/UserFavoriteItems/{airbag}"),
        Some(&auth),
        None,
    )
    .await;
    assert_eq!(removed.json()["IsFavorite"], false);
    assert_eq!(json(&rig, favorites, &auth).await["TotalRecordCount"], 0);

    // A played mark records no play and shows nothing playing.
    let plays = || -> i64 {
        rig.app
            .db()
            .query_row("SELECT COUNT(*) FROM library_play_history", [], |row| {
                row.get(0)
            })
            .expect("plays count")
    };
    let before = plays();
    for uri in [
        format!("/jellyfin/Users/x/PlayedItems/{one}"),
        format!("/jellyfin/UserPlayedItems/{one}"),
    ] {
        let reply = send(&rig, "POST", &uri, Some(&auth), None).await;
        assert_eq!(reply.status, StatusCode::OK, "{uri}");
    }
    assert_eq!(plays(), before);
    assert_eq!(now_playing(&rig).await, json!([]));

    let unknown = "d".repeat(32);
    for uri in [
        format!("/jellyfin/UserFavoriteItems/{unknown}"),
        format!("/jellyfin/UserPlayedItems/{unknown}"),
    ] {
        let reply = send(&rig, "POST", &uri, Some(&auth), None).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{uri}");
    }
    rig.stop().await;
}

/// Audio routes without ffmpeg: every accepted request streams the
/// file as it is, whatever container, codec or ceiling the app asks for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_routes_stream_direct_while_transcoding_is_off() {
    let rig = CompatApp::start("jf-audio").await;
    let auth = finamp(&rig.secret);
    let one = track_id(&rig, &auth, "One").await;
    let ok_computer = album_id(&rig, &auth, "OK Computer").await;
    let file = rig.file("one");
    let audio = |tail: &str| format!("/jellyfin/Audio/{one}/{tail}");
    // No credential at all: 401, so nobody streams or transcodes anonymously.
    let anonymous = refused(&rig, "GET", &audio("stream.mp3?static=true"), None, None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    // Headerless players carry the token as `api_key`, the way PlaybackInfo
    // hands out its URLs.
    let direct = get(
        &rig,
        &audio(&format!("stream.mp3?static=true&api_key={}", rig.secret)),
        None,
    )
    .await;
    assert_eq!(direct.status, StatusCode::OK);
    assert_eq!(direct.header("content-encoding"), "identity");
    assert!(direct.body == file, "stream serves another file");

    // Unknown tails and non-track ids 404 on GET and HEAD alike.
    for (method, uri) in [
        ("GET", audio("bogus")),
        ("HEAD", audio("bogus")),
        ("GET", format!("/jellyfin/Audio/{ok_computer}/stream")),
    ] {
        let reply = send(&rig, method, &uri, Some(&auth), None).await;
        assert_eq!(reply.bare(), (404, 0), "{method} {uri}");
    }

    for tail in [
        "universal?Container=mp3",
        "universal?Container=mp3%7Cmp4",
        "universal?Container=ogg&AudioCodec=opus",
        "stream?audioBitRate=64000",
    ] {
        let reply = get(&rig, &audio(tail), Some(&auth)).await;
        assert_eq!(reply.status, StatusCode::OK, "{tail}");
        assert!(reply.body == file, "{tail} serves another file");
    }
    rig.stop().await;
}

/// Finamp 1.0.1: direct play and downloads via `/Items/{id}/File`,
/// transcoded downloads via `/Audio/{id}/Universal` (capital U), and
/// transcoded playback via the HLS playlist, which is not offered while
/// the app has no ffmpeg.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finamp_file_universal_and_hls_routes() {
    let rig = CompatApp::start("jf-finamp-routes").await;
    let auth = finamp(&rig.secret);
    let one = track_id(&rig, &auth, "One").await;
    let size = rig.file("one").len();
    let file = format!("/jellyfin/Items/{one}/File?ApiKey={}", rig.secret);
    let direct = get(&rig, &file, None).await;
    assert_eq!(direct.bare(), (200, size));
    assert_eq!(direct.header("accept-ranges"), "bytes");
    let head = send(&rig, "HEAD", &file, None, None).await;
    assert_eq!(head.bare(), (200, 0));
    assert_eq!(head.header("content-length"), size.to_string());
    let anonymous = refused(
        &rig,
        "GET",
        &format!("/jellyfin/Items/{one}/File"),
        None,
        None,
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    let audio = |tail: &str| format!("/jellyfin/Audio/{one}/{tail}");
    let universal = get(
        &rig,
        &audio(&format!("Universal?Container=mp3&ApiKey={}", rig.secret)),
        None,
    )
    .await;
    assert_eq!(universal.bare(), (200, size));

    let query = format!(
        "audioCodec=aac&segmentContainer=ts&audioBitRate=128000&ApiKey={}",
        rig.secret
    );
    let playlist = get(&rig, &audio(&format!("main.m3u8?{query}")), None).await;
    assert_eq!(playlist.status, StatusCode::NOT_FOUND);
    rig.stop().await;
}

/// The transcode policy shared by `/Audio` and PlaybackInfo, which both
/// read the stream gateway's one decision.
#[test]
fn transcode_decision_rules() {
    let track = TrackInfo {
        file_format: "mp3".to_owned(),
        bitrate_kbps: Some(320),
        duration_seconds: 0.0,
    };
    let on = TranscodeSettings::default();
    let plan = |requested: Option<&str>,
                ceiling: Option<i64>,
                force: bool,
                start: f64,
                settings: &TranscodeSettings| {
        decide(&track, requested, ceiling, force, start, settings, true)
    };
    // Nothing requested, a zero ceiling, force-original or transcoding off
    // all stream direct.
    assert!(!plan(None, None, false, 0.0, &on).is_transcode());
    assert!(!plan(None, Some(0), false, 0.0, &on).is_transcode());
    assert!(!plan(Some("opus"), None, true, 0.0, &on).is_transcode());
    let off = TranscodeSettings {
        transcoding_enabled: false,
        ..TranscodeSettings::default()
    };
    assert!(!plan(Some("opus"), None, false, 0.0, &off).is_transcode());
    // A codec or a ceiling triggers; the server max caps but never triggers.
    assert!(matches!(
        plan(None, Some(64), false, 0.0, &on),
        StreamPlan::Transcode {
            out_bitrate_kbps: 64,
            ..
        }
    ));
    assert!(matches!(
        plan(Some("opus"), Some(999), false, 0.0, &on),
        StreamPlan::Transcode {
            out_format: OutFormat::Opus,
            out_bitrate_kbps: 320,
            ..
        }
    ));
    // Unusable formats fall back to the default; negative starts clamp.
    assert!(matches!(
        plan(Some("wav"), None, false, -5.0, &on),
        StreamPlan::Transcode { out_format: OutFormat::Mp3, start_seconds, .. }
            if start_seconds == 0.0
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playback_info_urls_play_and_transcode_fields_follow_policy() {
    let rig = CompatApp::start("jf-playback-info").await;
    let auth = finamp(&rig.secret);
    let one = track_id(&rig, &auth, "One").await;
    let info_uri = format!("/jellyfin/Items/{one}/PlaybackInfo");
    // The advertised direct URL plays with no auth header (Jellify, Manet).
    let info = json(&rig, &info_uri, &auth).await;
    let source = &info["MediaSources"][0];
    assert!(source.get("TranscodingUrl").is_none(), "{source}");
    let url = source["DirectStreamUrl"].as_str().expect("direct url");
    let path = url.strip_prefix("http://test.local").expect("origin");
    let played = get(&rig, path, None).await;
    assert_eq!(played.bare(), (200, rig.file("one").len()));

    // Without ffmpeg a low ceiling, in the query or in a POST body
    // (where Finamp's `UserId` is tolerated), still adds no transcoding
    // fields.
    let low = json(
        &rig,
        &format!("{info_uri}?maxStreamingBitrate=64000"),
        &auth,
    )
    .await;
    assert!(
        low["MediaSources"][0].get("TranscodingUrl").is_none(),
        "{low}"
    );
    let body = json!({"MaxStreamingBitrate": 64000, "UserId": "zzz"});
    let posted = send(&rig, "POST", &info_uri, Some(&auth), Some(body)).await;
    assert_eq!(posted.status, StatusCode::OK);
    assert!(
        posted.json()["MediaSources"][0]
            .get("TranscodingUrl")
            .is_none(),
        "transcoding offered without ffmpeg"
    );

    let unknown = format!("/jellyfin/Items/{}/PlaybackInfo", "c".repeat(32));
    assert_eq!(
        get(&rig, &unknown, Some(&auth)).await.status,
        StatusCode::NOT_FOUND
    );
    rig.stop().await;
}

/// With an ffmpeg (the stand-in) and transcoding on: the user policy and
/// System/Info offer it, a codec or a ceiling the source misses
/// transcodes, HLS plays, and PlaybackInfo adds the transcoding fields.
/// Turned off in Settings, transcoding is no longer offered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcoding_answers_with_ffmpeg() {
    let rig = CompatApp::transcoding("jf-transcoding").await;
    let auth = finamp(&rig.secret);
    let one = track_id(&rig, &auth, "One").await;
    let transcoding = "EnableAudioPlaybackTranscoding";
    let login = "/jellyfin/Users/AuthenticateByName";
    let sign_in = || json!({"Username": PLAYER, "Pw": rig.secret});

    let echo = send(&rig, "POST", login, None, Some(sign_in()))
        .await
        .json();
    assert_eq!(echo["User"]["Policy"][transcoding], true);
    let me = json(&rig, "/jellyfin/Users/Me", &auth).await;
    assert_eq!(me["Policy"][transcoding], true);
    let info = json(&rig, "/jellyfin/System/Info", &auth).await;
    assert_eq!(info["EncoderLocation"], "System");

    // Accepted containers (first pipe variant wins) stream direct; a codec
    // mismatch or a client ceiling under the source bitrate transcodes, and
    // unknown codecs fall back to opus.
    let audio = |tail: &str| format!("/jellyfin/Audio/{one}/{tail}");
    for (tail, transcoded) in [
        ("universal?Container=mp3", None),
        ("universal?Container=mp3%7Cmp4", None),
        (
            "universal?Container=ogg&AudioCodec=opus",
            Some("ffmpeg stand-in: ogg "),
        ),
        (
            "universal?Container=ogg&AudioCodec=aac",
            Some("ffmpeg stand-in: ogg "),
        ),
        (
            "stream?audioBitRate=64000",
            Some("ffmpeg stand-in: mp3 64k\n"),
        ),
    ] {
        let reply = get(&rig, &audio(tail), Some(&auth)).await;
        assert_eq!(reply.status, StatusCode::OK, "{tail}");
        match transcoded {
            None => assert!(reply.body == rig.file("one"), "{tail} serves another file"),
            Some(marker) => {
                let body = String::from_utf8_lossy(&reply.body);
                assert!(body.starts_with(marker), "{tail}: {body}");
                assert_eq!(reply.header("accept-ranges"), "none", "{tail}");
            }
        }
    }

    // Finamp's transcoded playback: the HLS playlist and its one segment.
    let query = format!(
        "audioCodec=aac&segmentContainer=ts&audioBitRate=128000&ApiKey={}",
        rig.secret
    );
    let playlist = get(&rig, &audio(&format!("main.m3u8?{query}")), None).await;
    assert_eq!(playlist.status, StatusCode::OK);
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
    let head = send(
        &rig,
        "HEAD",
        &audio(&format!("main.m3u8?{query}")),
        None,
        None,
    )
    .await;
    assert_eq!(head.bare(), (200, 0));
    assert_eq!(head.header("content-length"), text.len().to_string());
    let ts = get(&rig, &audio(segment), None).await;
    assert_eq!(ts.status, StatusCode::OK);
    assert_eq!(
        String::from_utf8_lossy(&ts.body),
        "ffmpeg stand-in: mpegts 128k\n"
    );

    // PlaybackInfo offers transcoding; a low ceiling adds its fields.
    let info_uri = format!("/jellyfin/Items/{one}/PlaybackInfo");
    let info = json(&rig, &info_uri, &auth).await;
    let source = &info["MediaSources"][0];
    assert!(source.get("TranscodingUrl").is_none(), "{source}");
    assert_eq!(source["SupportsTranscoding"], true);
    let low = json(
        &rig,
        &format!("{info_uri}?maxStreamingBitrate=64000"),
        &auth,
    )
    .await;
    let source = &low["MediaSources"][0];
    let transcode_url = source["TranscodingUrl"].as_str().expect("transcoding url");
    assert!(
        transcode_url.contains("/jellyfin/Audio/") && transcode_url.contains("AudioCodec=mp3"),
        "{transcode_url}"
    );
    assert_eq!(source["TranscodingSubProtocol"], "http");
    assert_eq!(source["TranscodingContainer"], "mp3");
    // Same through a POST body; an opus default rides the ogg container.
    rig.set_connect_apps(json!({"transcode_default_format": "opus"}))
        .await;
    let body = json!({"MaxStreamingBitrate": 64000, "UserId": "zzz"});
    let posted = send(&rig, "POST", &info_uri, Some(&auth), Some(body)).await;
    assert_eq!(posted.status, StatusCode::OK);
    assert_eq!(
        posted.json()["MediaSources"][0]["TranscodingContainer"],
        "ogg"
    );

    // Turned off, transcoding is offered nowhere.
    rig.set_connect_apps(json!({"transcoding_enabled": false}))
        .await;
    let echo = send(&rig, "POST", login, None, Some(sign_in()))
        .await
        .json();
    assert_eq!(echo["User"]["Policy"][transcoding], false);
    let me = json(&rig, "/jellyfin/Users/Me", &auth).await;
    assert_eq!(me["Policy"][transcoding], false);
    rig.stop().await;
}

/// The admin's view of who is listening: (track, progress, paused) per session.
async fn now_playing(rig: &CompatApp) -> Value {
    let admin = rig.app.login_admin().await;
    let (status, body) = rig
        .app
        .call(
            "GET",
            "/api/v3/now-playing",
            &[("authorization", admin.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    Value::Array(
        body["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .map(|entry| {
                json!([
                    entry["track_name"],
                    entry["progress_ms"],
                    entry["is_paused"]
                ])
            })
            .collect(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_reports_drive_presence_and_scrobbles() {
    let rig = CompatApp::start("jf-sessions").await;
    let auth = finamp(&rig.secret);
    let post = |uri: &'static str, body: Option<Value>| {
        let rig = &rig;
        let auth = auth.clone();
        async move { send(rig, "POST", uri, Some(&auth), body).await.status }
    };
    let airbag = track_id(&rig, &auth, "Airbag").await;
    // Empty bodies, pings and capabilities accept silently.
    assert_eq!(
        post("/jellyfin/Sessions/Playing", None).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        post("/jellyfin/Sessions/Playing/Ping", None).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        post("/jellyfin/Sessions/Capabilities/Full", Some(json!({}))).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(now_playing(&rig).await, json!([]));
    // Start shows the track as playing; progress moves it to the reported
    // position and paused state, and never counts a play.
    let start = json!({"ItemId": airbag, "PlaySessionId": "ps-1"});
    assert_eq!(
        post("/jellyfin/Sessions/Playing", Some(start)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(now_playing(&rig).await, json!([["Airbag", null, false]]));
    let progress = json!({"ItemId": airbag, "PositionTicks": 20_000_000_i64, "IsPaused": true});
    assert_eq!(
        post("/jellyfin/Sessions/Playing/Progress", Some(progress)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(now_playing(&rig).await, json!([["Airbag", 2000, true]]));
    assert_eq!(play_count(&rig, &auth, &airbag).await, 0);

    // Stopped always clears presence; it counts a play past 90%, or when
    // the position is omitted, but never below the threshold or on
    // failure. The ones that count go last: once they show, the others
    // would have.
    let runtime = 1_800_000_000_i64;
    let mut stops = Vec::new();
    for (name, body, counts) in [
        (
            "Paranoid Android",
            json!({"PositionTicks": 100_000_000_i64, "RunTimeTicks": runtime}),
            false,
        ),
        ("Teardrop", json!({"Failed": true}), false),
        (
            "One",
            json!({"PositionTicks": 1_700_000_000_i64, "RunTimeTicks": runtime}),
            true,
        ),
        ("Song A", json!({}), true),
    ] {
        let id = track_id(&rig, &auth, name).await;
        let start = json!({"ItemId": id});
        assert_eq!(
            post("/jellyfin/Sessions/Playing", Some(start)).await,
            StatusCode::NO_CONTENT
        );
        let mut body = body;
        body["ItemId"] = json!(id);
        assert_eq!(
            post("/jellyfin/Sessions/Playing/Stopped", Some(body.clone())).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(now_playing(&rig).await, json!([]), "{body}");
        stops.push((id, body, counts));
    }
    for (id, body, counts) in &stops {
        if *counts {
            rig.app
                .until("the play is counted", || async {
                    (play_count(&rig, &auth, id).await == 1).then_some(())
                })
                .await;
        } else {
            assert_eq!(play_count(&rig, &auth, id).await, 0, "{body}");
        }
    }
    for (id, body, counts) in &stops {
        assert_eq!(
            play_count(&rig, &auth, id).await,
            i64::from(*counts),
            "{body}"
        );
    }
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playlists_create_add_move_remove() {
    let rig = CompatApp::start("jf-playlists").await;
    let auth = finamp(&rig.secret);
    let airbag = track_id(&rig, &auth, "Airbag").await;
    let paranoid = track_id(&rig, &auth, "Paranoid Android").await;
    let one = track_id(&rig, &auth, "One").await;
    let entries = |pid: &str| format!("/jellyfin/Playlists/{pid}/Items");
    // Unknown ids in the create body are skipped.
    let body = json!({"Name": "Road", "Ids": [airbag, "b".repeat(32)]});
    let created = send(&rig, "POST", "/jellyfin/Playlists", Some(&auth), Some(body)).await;
    let pid = created.json()["Id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    let detail = json(&rig, &format!("/jellyfin/Playlists/{pid}"), &auth).await;
    assert_eq!(detail["Name"], "Road");
    assert_eq!(detail["ItemIds"], json!([airbag]));
    let page = json(&rig, &entries(&pid), &auth).await;
    let entry = page["Items"][0]["PlaylistItemId"]
        .as_str()
        .expect("entry id")
        .to_owned();

    // Add a second track, move the first entry to index 1, then remove it.
    let add = format!("{}?ids={paranoid}", entries(&pid));
    assert_eq!(
        send(&rig, "POST", &add, Some(&auth), None).await.status,
        StatusCode::NO_CONTENT
    );
    let moved = format!("{}/{entry}/Move/1", entries(&pid));
    assert_eq!(
        send(&rig, "POST", &moved, Some(&auth), None).await.status,
        StatusCode::NO_CONTENT
    );
    let page = json(&rig, &entries(&pid), &auth).await;
    assert_eq!(page["Items"][0]["Name"], "Paranoid Android");
    assert_eq!(page["Items"][1]["Name"], "Airbag");
    let remove = format!("{}?entryIds={entry}", entries(&pid));
    assert_eq!(
        send(&rig, "DELETE", &remove, Some(&auth), None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    let browse = json(&rig, "/jellyfin/Items?IncludeItemTypes=Playlist", &auth).await;
    assert_eq!(named(&browse["Items"], "Name", "Road")["ChildCount"], 1);

    // Query-parameter creation defaults the name.
    let created = send(
        &rig,
        "POST",
        &format!("/jellyfin/Playlists?ids={one}"),
        Some(&auth),
        None,
    )
    .await;
    let pid2 = created.json()["Id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    let detail = json(&rig, &format!("/jellyfin/Playlists/{pid2}"), &auth).await;
    assert_eq!(detail["Name"], "Playlist");

    // Another user's public playlist is listed beside the caller's own;
    // their private one is not.
    let bob = rig.app.login_user("bob").await;
    let bob = [("authorization", bob.authorization.as_str())];
    for (name, public) in [("Bob Public", true), ("Bob Private", false)] {
        let (status, made) = rig
            .app
            .call(
                "POST",
                "/api/v3/playlists",
                &bob,
                Some(json!({"name": name})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{made}");
        let id = made["id"].as_str().expect("playlist id");
        let (status, body) = rig
            .app
            .call(
                "PATCH",
                &format!("/api/v3/playlists/{id}/visibility"),
                &bob,
                Some(json!({"is_public": public})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let browse = json(&rig, "/jellyfin/Items?IncludeItemTypes=Playlist", &auth).await;
    let mut listed: Vec<&str> = browse["Items"]
        .as_array()
        .expect("playlists")
        .iter()
        .filter_map(|item| item["Name"].as_str())
        .collect();
    listed.sort_unstable();
    assert_eq!(listed, ["Bob Public", "Playlist", "Road"]);

    // A playlist id on a track route is a bare 404, never a native envelope.
    let reply = get(&rig, &format!("/jellyfin/Audio/{pid}/stream"), Some(&auth)).await;
    assert_eq!(reply.bare(), (404, 0));
    rig.stop().await;
}

/// A bad token is an empty 401; `api_key` in the query signs a request.
/// One item at the older address, the one with a user id in the path,
/// is the same item: the id is not read, the token decides whose library
/// answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn token_transports_and_the_older_item_address() {
    let rig = CompatApp::start("jf-tokens").await;
    let auth = finamp(&rig.secret);
    let bad = refused(
        &rig,
        "GET",
        "/jellyfin/Items",
        Some(&finamp("wrong-secret")),
        None,
    )
    .await;
    assert_eq!(bad.bare(), (401, 0));
    let by_query = get(
        &rig,
        &format!("/jellyfin/Items?api_key={}&Limit=0", rig.secret),
        None,
    )
    .await;
    assert_eq!(by_query.status, StatusCode::OK);

    let airbag = track_id(&rig, &auth, "Airbag").await;
    let newer = json(&rig, &format!("/jellyfin/Items/{airbag}"), &auth).await;
    for user in [rig.user_id.as_str(), "someone-else"] {
        let uri = format!("/jellyfin/Users/{user}/Items/{airbag}");
        assert_eq!(json(&rig, &uri, &auth).await, newer, "{user}");
        let anonymous = refused(&rig, "GET", &uri, None, None).await;
        assert_eq!(anonymous.bare(), (401, 0), "{user}");
    }
    rig.stop().await;
}

/// Point the app password `secret` at the token `token` instead, as one
/// made elsewhere could be: the Jellyfin API finds a token by its hash.
fn retoken(rig: &CompatApp, secret: &str, token: &str) {
    use sha2::{Digest, Sha256};
    let hash = |raw: &str| format!("{:x}", Sha256::digest(raw.as_bytes()));
    let changed = rig
        .app
        .db()
        .execute(
            "UPDATE connect_app_passwords SET secret_sha256 = ?1 WHERE secret_sha256 = ?2",
            [hash(token), hash(secret)],
        )
        .expect("token changes");
    assert_eq!(changed, 1);
}

/// An app password ends in `=`. A player may send it as written or as
/// `%3D`, and both are the same token. The value is decoded once, and one
/// that does not decode is refused. A `+` stays a plus and is never a
/// space.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_tokens_are_percent_decoded_exactly_once() {
    let rig = CompatApp::start("jf-query-tokens").await;
    let auth = finamp(&rig.secret);
    let one = track_id(&rig, &auth, "One").await;
    let secret = rig.secret.clone();
    let bare = secret.strip_suffix('=').expect("an app password ends in =");
    let first = secret.chars().next().expect("first character");
    let flipped = if first.is_ascii_uppercase() {
        first.to_ascii_lowercase()
    } else {
        first.to_ascii_uppercase()
    };
    let play =
        |key: &str, token: &str| format!("/jellyfin/Audio/{one}/stream?static=true&{key}={token}");
    for key in ["api_key", "ApiKey"] {
        for played in [
            secret.clone(),
            format!("{bare}%3D"),
            format!("{bare}%3d"),
            format!("%{:02X}{}%3D", u32::from(first), &bare[1..]),
        ] {
            let reply = get(&rig, &play(key, &played), None).await;
            assert_eq!(reply.status, StatusCode::OK, "{key}={played}");
        }
        let mut refusals = vec![
            "wrong-secret%3D".to_owned(),
            // Encoded twice: decoded once it is the text `%3D`.
            format!("{bare}%253D"),
            // No trimming, no case folding, no padding added.
            bare.to_owned(),
            format!("{bare}%3D%20"),
            // Not percent-encoding, or not UTF-8 once decoded.
            format!("{bare}%3"),
            format!("{bare}%zz"),
            format!("{bare}%3D%ff"),
        ];
        if flipped != first {
            refusals.push(format!("{flipped}{}%3D", &bare[1..]));
        }
        for refused_token in refusals {
            let reply = refused(&rig, "GET", &play(key, &refused_token), None, None).await;
            assert_eq!(
                reply.status,
                StatusCode::UNAUTHORIZED,
                "{key}={refused_token}"
            );
        }
    }
    // A token in a header is taken as written.
    let header = refused(
        &rig,
        "GET",
        "/jellyfin/Items?Limit=0",
        Some(&finamp(&format!("{bare}%3D"))),
        None,
    )
    .await;
    assert_eq!(header.status, StatusCode::UNAUTHORIZED);
    // A query token that does not decode refuses the request, even beside
    // a header token that is good.
    let beside = get(&rig, "/jellyfin/Items?Limit=0", Some(&auth)).await;
    assert_eq!(beside.status, StatusCode::OK);
    let beside = refused(
        &rig,
        "GET",
        "/jellyfin/Items?Limit=0&api_key=%zz",
        Some(&auth),
        None,
    )
    .await;
    assert_eq!(beside.status, StatusCode::UNAUTHORIZED);

    // Plus and space: the player's token becomes `d+s=`, the admin's
    // `d s=`.
    retoken(&rig, &secret, "d+s=");
    let admin = rig.admin_secret().await;
    retoken(&rig, &admin, "d s=");
    for (token, name) in [
        ("d+s=", PLAYER),
        ("d%2Bs%3D", PLAYER),
        ("d%20s%3D", "admin"),
    ] {
        let me = json_anonymous(&rig, &format!("/jellyfin/Users/Me?api_key={token}")).await;
        assert_eq!(me["Name"], json!(name), "{token}");
    }
    rig.stop().await;
}

async fn json_anonymous(rig: &CompatApp, uri: &str) -> Value {
    let reply = get(rig, uri, None).await;
    assert_eq!(reply.status, StatusCode::OK, "{uri}");
    reply.json()
}

/// The routes anyone may call while the API is on. Written out here, never
/// read from the table, so a row that turns public fails below.
const PUBLIC: [(&str, &str); 6] = [
    ("GET", "/jellyfin/System/Info/Public"),
    ("GET", "/jellyfin/QuickConnect/Enabled"),
    ("POST", "/jellyfin/Users/AuthenticateByName"),
    ("GET", "/jellyfin/Users/Public"),
    ("GET", "/jellyfin/Items/{item_id}/Images/{image_type}"),
    (
        "GET",
        "/jellyfin/Items/{item_id}/Images/{image_type}/{index}",
    ),
];

/// Every request of the walk comes from an address of its own, so no
/// refusal turns a later answer into a 429.
fn walker_address() -> String {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("10.77.{}.{}", n / 250, n % 250 + 1)
}

/// Each method of every row of the route table, each path parameter filled
/// with a dummy segment. A `GET` row also answers `HEAD` where the table
/// lists none.
fn walk() -> Vec<(&'static str, String, Access, &'static str)> {
    let mut requests = Vec::new();
    for row in ROUTES {
        let path = row
            .template
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    "dummy"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let mut verbs = vec![row.verb.as_str()];
        let has_head = ROUTES
            .iter()
            .any(|other| other.template == row.template && other.verb == Verb::Head);
        if row.verb == Verb::Get && !has_head {
            verbs.push("HEAD");
        }
        for verb in verbs {
            requests.push((verb, path.clone(), row.access, row.template));
        }
    }
    requests
}

/// Who may call a route is the table's access column and nothing else:
/// with no token, every signed-in route answers an empty 401 and no public
/// one does. With the API off, every route is the empty 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_route_is_public_or_signed_in_by_the_table() {
    let mut public: Vec<(&str, &str)> = ROUTES
        .iter()
        .filter(|row| row.access == Access::Public)
        .map(|row| (row.verb.as_str(), row.template))
        .collect();
    public.sort_unstable();
    let mut expected = PUBLIC.to_vec();
    expected.sort_unstable();
    assert_eq!(public, expected, "the public rows of the route table");

    let rig = CompatApp::start("jf-route-walk").await;
    for (verb, path, access, template) in walk() {
        let from = walker_address();
        let headers = vec![("x-forwarded-for", from.as_str())];
        let reply = raw(&rig, verb, &path, headers, None, None).await;
        let what = format!("{verb} {path}");
        match access {
            Access::SignedIn => assert_eq!(reply.bare(), (401, 0), "{what}"),
            Access::Public => {
                let expected = match (verb, template) {
                    ("POST", "/jellyfin/Users/AuthenticateByName") => StatusCode::BAD_REQUEST,
                    (_, template) if template.contains("/Images/") => StatusCode::NOT_FOUND,
                    _ => StatusCode::OK,
                };
                assert_eq!(reply.status, expected, "{what}");
            }
        }
    }
    for path in ["/jellyfin/Users/Public", "/jellyfin/users/public"] {
        let reply = get(&rig, path, None).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.json(), json!([]), "{path}");
    }

    set_jellyfin(&rig, false).await;
    for (verb, path, _, _) in walk() {
        let from = walker_address();
        let headers = vec![("x-forwarded-for", from.as_str())];
        let reply = raw(&rig, verb, &path, headers, None, None).await;
        assert_eq!(reply.bare(), (404, 0), "{verb} {path} with the API off");
    }
    rig.stop().await;
}

/// Wrong app passwords at sign-in still lock the address out; asking who
/// may sign in never does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_sign_ins_lock_out_and_public_routes_do_not() {
    let rig = CompatApp::start("jf-sign-in-lockout").await;
    let login = "/jellyfin/Users/AuthenticateByName";
    let guesser = vec![("x-forwarded-for", "10.78.0.1")];
    for _ in 0..5 {
        let body = json!({"Username": PLAYER, "Pw": "nope"});
        let reply = raw(&rig, "POST", login, guesser.clone(), None, Some(body)).await;
        assert_eq!(reply.bare(), (401, 0));
    }
    let locked = raw(&rig, "GET", "/jellyfin/Users/Public", guesser, None, None).await;
    assert_eq!(locked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(!locked.header("retry-after").is_empty(), "Retry-After");

    let asker = vec![("x-forwarded-for", "10.78.0.2")];
    for _ in 0..6 {
        let reply = raw(
            &rig,
            "GET",
            "/jellyfin/Users/Public",
            asker.clone(),
            None,
            None,
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
    }
    rig.stop().await;
}

/// The sign-in layer hands each handler its own caller: a private
/// playlist answers its owner, is not found for another user, and needs a
/// token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_private_playlist_answers_only_its_owner() {
    let rig = CompatApp::start("jf-private-playlist").await;
    let auth = finamp(&rig.secret);
    let created = send(
        &rig,
        "POST",
        "/jellyfin/Playlists",
        Some(&auth),
        Some(json!({"Name": "Mine"})),
    )
    .await;
    let pid = created.json()["Id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    let uri = format!("/jellyfin/Playlists/{pid}");
    let carol = rig.app.login_user("carol").await;
    let carol = finamp(&rig.app_password(&carol.authorization).await);

    assert_eq!(
        refused(&rig, "GET", &uri, None, None).await.bare(),
        (401, 0)
    );
    assert_eq!(get(&rig, &uri, Some(&carol)).await.bare(), (404, 0));
    assert_eq!(json(&rig, &uri, &auth).await["Name"], "Mine");
    rig.stop().await;
}
