//! Compat client journeys through the whole app, over a library its own
//! scanner filled from the committed fixture tracks.
//!
//! Symfonium (Subsonic) and Finamp (Jellyfin) each run browse, stream,
//! favorite, playlist and scrobble; Jellify fetches `Latest` and plays the
//! stream URL with no auth headers, the token in its `api_key` query.
//! Every step lands in a `library_*` golden under `tests/fixtures/compat/`.
//! `COMPAT_BLESS=1` rewrites them, never in CI: review the diff before
//! committing.

use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

use crate::common::compat::{
    CompatApp, PLAYER, Trace, assert_golden, bless_requested, library_goldens, named, rest,
};

#[test]
fn bless_is_refused_in_ci() {
    assert_eq!(bless_requested(None, None), Ok(false));
    assert_eq!(bless_requested(None, Some("true")), Ok(false));
    assert_eq!(bless_requested(Some("1"), None), Ok(true));
    assert_eq!(bless_requested(Some("1"), Some("")), Ok(true));
    let refusal = bless_requested(Some("1"), Some("true")).expect_err("refused");
    assert!(
        refusal.contains("COMPAT_BLESS") && refusal.contains("CI"),
        "{refusal}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symfonium_browse_stream_favorite_playlist_scrobble() {
    let rig = CompatApp::start("compat-symfonium").await;
    let mut trace = Trace::new(&rig);
    let auth = rig.subsonic_auth("symfonium");
    let one = rig.file("one");

    trace.get(&rest("ping", &auth), &[]).await;
    let newest = trace
        .get(&rest("getAlbumList2?type=newest", &auth), &[])
        .await;
    let album_id = named(
        &newest["subsonic-response"]["albumList2"]["album"],
        "name",
        "Achtung Baby",
    )["id"]
        .as_str()
        .expect("album id")
        .to_owned();
    let album = trace
        .get(&rest(&format!("getAlbum?id={album_id}"), &auth), &[])
        .await;
    let album = &album["subsonic-response"]["album"];
    assert_eq!(album["songCount"], json!(1));
    let song = named(&album["song"], "title", "One")["id"]
        .as_str()
        .expect("song id")
        .to_owned();
    let stream = rest(&format!("stream?id={song}"), &auth);
    let full = trace.get(&stream, &[]).await;
    assert_eq!(full["byte_len"], json!(one.len()));
    let slice = trace.get(&stream, &[("Range", "bytes=0-49")]).await;
    assert_eq!(slice["byte_len"], json!(50));
    trace
        .get(&rest(&format!("star?id={song}"), &auth), &[])
        .await;
    let starred = trace.get(&rest("getStarred2", &auth), &[]).await;
    assert_eq!(
        starred["subsonic-response"]["starred2"]["song"][0]["id"],
        json!(song)
    );
    let created = trace
        .get(
            &rest(
                &format!("createPlaylist?name=Journey+Mix&songId={song}"),
                &auth,
            ),
            &[],
        )
        .await;
    let playlist_id = created["subsonic-response"]["playlist"]["id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    trace
        .get(&rest(&format!("getPlaylist?id={playlist_id}"), &auth), &[])
        .await;
    trace
        .get(
            &rest(&format!("scrobble?id={song}&submission=false"), &auth),
            &[],
        )
        .await;
    let playing = trace.get(&rest("getNowPlaying", &auth), &[]).await;
    let entries = playing["subsonic-response"]["nowPlaying"]["entry"]
        .as_array()
        .expect("now playing entries");
    assert_eq!(entries.len(), 1, "{playing}");
    assert_eq!(entries[0]["id"], json!(song));
    assert_eq!(entries[0]["username"], json!("alice"));
    assert_eq!(entries[0]["playerName"], json!("symfonium"));
    trace
        .get(
            &rest(&format!("scrobble?id={song}&submission=true"), &auth),
            &[],
        )
        .await;

    assert_golden("library_symfonium.trace.json", &trace.body());
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finamp_browse_stream_favorite_playlist_progress() {
    let rig = CompatApp::start("compat-finamp").await;
    let mut trace = Trace::new(&rig);
    let auth_value = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", auth_value.as_str())];
    let user = rig.user_id.clone();
    let one = rig.file("one");

    trace.get("/jellyfin/System/Info/Public", &[]).await;
    trace
        .get(&format!("/jellyfin/Users/{user}/Views"), &auth)
        .await;
    let items = trace
        .get(
            &format!("/jellyfin/Users/{user}/Items?includeItemTypes=Audio"),
            &auth,
        )
        .await;
    let track_id = named(&items["Items"], "Name", "One")["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    let info = trace
        .get(&format!("/jellyfin/Items/{track_id}/PlaybackInfo"), &auth)
        .await;
    let direct = info["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .expect("direct stream url");
    let stream_url = direct
        .find("/jellyfin/")
        .map(|at| direct[at..].to_owned())
        .expect("a stream url under /jellyfin");
    let full = trace.get(&stream_url, &auth).await;
    assert_eq!(full["byte_len"], json!(one.len()));
    let slice = trace
        .get(&stream_url, &[auth[0], ("Range", "bytes=0-99")])
        .await;
    assert_eq!(slice["byte_len"], json!(100));

    let favorite = format!("/jellyfin/Users/{user}/FavoriteItems/{track_id}");
    trace.call("POST", &favorite, &auth, None).await;
    let favorites = trace
        .get(
            &format!("/jellyfin/Users/{user}/Items?includeItemTypes=Audio&IsFavorite=true"),
            &auth,
        )
        .await;
    assert_eq!(favorites["Items"][0]["Id"], json!(track_id));

    let created = trace
        .call(
            "POST",
            "/jellyfin/Playlists",
            &auth,
            Some(json!({ "Name": "Journey Mix", "Ids": [track_id] })),
        )
        .await;
    let playlist_id = created["Id"].as_str().expect("playlist id").to_owned();
    let entries = trace
        .get(&format!("/jellyfin/Playlists/{playlist_id}/Items"), &auth)
        .await;
    assert_eq!(entries["Items"][0]["Id"], json!(track_id));

    for endpoint in ["Playing", "Playing/Progress"] {
        let uri = format!("/jellyfin/Sessions/{endpoint}");
        let body = json!({ "ItemId": track_id });
        trace.call("POST", &uri, &auth, Some(body)).await;
    }
    let stopped = json!({
        "ItemId": track_id,
        "PositionTicks": 1_700_000_000_i64,
        "RunTimeTicks": 1_800_000_000_i64,
    });
    trace
        .call(
            "POST",
            "/jellyfin/Sessions/Playing/Stopped",
            &auth,
            Some(stopped),
        )
        .await;
    rig.app
        .until("the stop past the threshold counts as a play", || async {
            let plays: i64 = rig
                .app
                .db()
                .query_row(
                    "SELECT COUNT(*) FROM library_play_history WHERE track_name = 'One'",
                    [],
                    |row| row.get(0),
                )
                .expect("history counts");
            (plays == 1).then_some(())
        })
        .await;

    assert_golden("library_finamp.trace.json", &trace.body());
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellify_latest_then_headerless_play() {
    let rig = CompatApp::start("compat-jellify").await;
    let mut trace = Trace::new(&rig);
    let auth_value = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", auth_value.as_str())];
    let teardrop = rig.file("teardrop");

    let latest = trace.get("/jellyfin/UserItems/Latest", &auth).await;
    assert_eq!(latest[0]["Type"], json!("MusicAlbum"));
    let album_id = named(&latest, "Name", "Mezzanine")["Id"]
        .as_str()
        .expect("album id");
    let tracks = trace
        .get(
            &format!(
                "/jellyfin/Users/{}/Items?parentId={album_id}&includeItemTypes=Audio",
                rig.user_id
            ),
            &auth,
        )
        .await;
    let track_id = tracks["Items"][0]["Id"].as_str().expect("track id");
    let stream_url = format!(
        "/jellyfin/Audio/{track_id}/stream.mp3?static=true&api_key={}",
        rig.secret
    );

    // No auth headers from here on; the token rides in the query.
    let full = trace.get(&stream_url, &[]).await;
    assert_eq!(full["byte_len"], json!(teardrop.len()));
    let slice = trace.get(&stream_url, &[("Range", "bytes=100-199")]).await;
    assert_eq!(slice["byte_len"], json!(100));
    let head = trace.call("HEAD", &stream_url, &[], None).await;
    assert!(head.is_null(), "HEAD has no body");
    let universal = stream_url.replace("/stream.mp3", "/universal");
    trace.get(&universal, &[]).await;

    assert_golden("library_jellify.trace.json", &trace.body());
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subsonic_pinned_reference_shapes() {
    let rig = CompatApp::start("compat-pinned-subsonic").await;
    let mut trace = Trace::new(&rig);
    let auth = rig.subsonic_auth("pin-check");

    trace.get(&rest("ping", &auth), &[]).await;
    assert_golden("library_pinned_subsonic_ping.json", &trace.recorded());
    let newest = trace
        .get(&rest("getAlbumList2?type=newest", &auth), &[])
        .await;
    assert_golden(
        "library_pinned_subsonic_album_list2.json",
        &trace.recorded(),
    );
    let album_id = named(
        &newest["subsonic-response"]["albumList2"]["album"],
        "name",
        "Achtung Baby",
    )["id"]
        .as_str()
        .expect("album id")
        .to_owned();
    let album = trace
        .get(&rest(&format!("getAlbum?id={album_id}"), &auth), &[])
        .await;
    let song = album["subsonic-response"]["album"]["song"][0]["id"]
        .as_str()
        .expect("song id")
        .to_owned();
    trace
        .get(&rest(&format!("getSong?id={song}"), &auth), &[])
        .await;
    assert_golden("library_pinned_subsonic_song.json", &trace.recorded());
    trace
        .get(&rest(&format!("star?id={song}"), &auth), &[])
        .await;
    trace.get(&rest("getStarred2", &auth), &[]).await;
    assert_golden("library_pinned_subsonic_starred2.json", &trace.recorded());
    let created = trace
        .get(
            &rest(
                &format!("createPlaylist?name=Journey+Mix&songId={song}"),
                &auth,
            ),
            &[],
        )
        .await;
    let playlist_id = created["subsonic-response"]["playlist"]["id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    trace
        .get(&rest(&format!("getPlaylist?id={playlist_id}"), &auth), &[])
        .await;
    assert_golden("library_pinned_subsonic_playlist.json", &trace.recorded());
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_pinned_reference_shapes() {
    let rig = CompatApp::start("compat-pinned-jellyfin").await;
    let mut trace = Trace::new(&rig);
    let auth_value = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", auth_value.as_str())];
    let user = rig.user_id.clone();

    trace.get("/jellyfin/System/Info/Public", &[]).await;
    assert_golden(
        "library_pinned_jellyfin_public_info.json",
        &trace.recorded(),
    );
    for (uri, pinned) in [
        (
            format!("/jellyfin/Users/{user}/Views"),
            "library_pinned_jellyfin_views.json",
        ),
        (
            format!("/jellyfin/Users/{user}/Items"),
            "library_pinned_jellyfin_items.json",
        ),
    ] {
        trace.get(&uri, &auth).await;
        assert_golden(pinned, &trace.recorded());
    }
    let latest = trace.get("/jellyfin/UserItems/Latest", &auth).await;
    assert_golden("library_pinned_jellyfin_latest.json", &trace.recorded());

    // The rig identifies no album, so no album sends a release group key.
    let mezzanine = named(&latest, "Name", "Mezzanine");
    assert_eq!(mezzanine["ProviderIds"], json!({}));

    let album_id = named(&latest, "Name", "Mezzanine")["Id"]
        .as_str()
        .expect("album id");
    let tracks = trace
        .get(
            &format!("/jellyfin/Users/{user}/Items?parentId={album_id}&includeItemTypes=Audio"),
            &auth,
        )
        .await;
    let track_id = tracks["Items"][0]["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    trace
        .get(&format!("/jellyfin/Items/{track_id}/PlaybackInfo"), &auth)
        .await;
    assert_golden(
        "library_pinned_jellyfin_playback_info.json",
        &trace.recorded(),
    );

    let created = trace
        .call(
            "POST",
            "/jellyfin/Playlists",
            &auth,
            Some(json!({ "Name": "Journey Mix", "Ids": [track_id] })),
        )
        .await;
    let playlist_id = created["Id"].as_str().expect("playlist id");
    trace
        .get(&format!("/jellyfin/Playlists/{playlist_id}/Items"), &auth)
        .await;
    assert_golden(
        "library_pinned_jellyfin_playlist_items.json",
        &trace.recorded(),
    );

    // Nor does the rig identify an artist, so no artist sends an artist key.
    let artists = trace.get("/jellyfin/Artists", &auth).await;
    let radiohead = named(&artists["Items"], "Name", "Radiohead");
    assert_eq!(radiohead["ProviderIds"], json!({}));
    // As Jellyfin counts and times an artist: every album and song that
    // credits it, also one it only appears on (Artist One on Song A).
    assert_eq!(radiohead["ChildCount"], json!(3), "{radiohead}");
    let guest = named(&artists["Items"], "Name", "Artist One");
    assert_eq!(guest["ChildCount"], json!(2), "{guest}");
    assert!(guest["RunTimeTicks"].as_i64() > Some(0), "{guest}");
    rig.stop().await;
}

fn header_str(map: &HeaderMap, name: &str) -> String {
    map.get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

/// A song's path, on Subsonic and on a Jellyfin track fetched by id, is
/// where its file sits under its own library folder, and says nothing of
/// where on the host that folder is. A second folder is added so a path
/// that carried its folder would show.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn song_paths_never_carry_the_host_path() {
    let rig = CompatApp::start("compat-song-path").await;
    let auth = rig.subsonic_auth("path-check");
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let extra = "Second Folder/Extra/01 Extra.flac";
    let second = rig.app.config.root_app_dir.join("second");
    let planted = second.join(extra);
    std::fs::create_dir_all(planted.parent().expect("album folder")).expect("mkdir");
    std::fs::copy(
        manifest.join("tests/fixtures/library/management_full.flac"),
        &planted,
    )
    .expect("fixture copies");
    rig.app.add_library_folder("second", &second).await;

    let layout: Value = serde_json::from_slice(
        &std::fs::read(manifest.join("tests/reference/compat/library.json")).expect("layout"),
    )
    .expect("layout parses");
    let mut expected: Vec<String> = layout["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .map(|track| track["path"].as_str().expect("planted path").to_owned())
        .collect();
    expected.push(extra.to_owned());
    expected.sort();

    let (status, all) = rig
        .app
        .call(
            "GET",
            &rest("search3?query=&songCount=500", &auth),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{all}");
    let mut paths = Vec::new();
    for found in all["subsonic-response"]["searchResult3"]["song"]
        .as_array()
        .expect("songs")
    {
        let id = found["id"].as_str().expect("song id");
        let (status, song) = rig
            .app
            .call("GET", &rest(&format!("getSong?id={id}"), &auth), &[], None)
            .await;
        assert_eq!(status, StatusCode::OK, "{song}");
        let path = song["subsonic-response"]["song"]["path"]
            .as_str()
            .unwrap_or_else(|| panic!("no path on {song}"))
            .to_owned();
        assert_eq!(found["path"], json!(path), "search and getSong agree");
        paths.push(path);
    }
    paths.sort();
    assert_eq!(paths, expected);
    let host = rig.app.config.root_app_dir.to_string_lossy().into_owned();
    for path in &paths {
        assert!(!path.starts_with('/'), "{path} is absolute");
        assert!(!path.contains(&host), "{path} carries the host path");
    }

    // Jellyfin sends the same path on a track fetched by id, and an empty
    // one on the library view, an album and an artist.
    let jellyfin = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", jellyfin.as_str())];
    let (status, views) = rig
        .app
        .call("GET", "/jellyfin/UserViews", &auth, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{views}");
    assert_eq!(views["Items"][0]["Path"], json!(""), "{views}");
    let (status, tracks) = rig
        .app
        .call(
            "GET",
            "/jellyfin/Items?includeItemTypes=Audio&Recursive=true",
            &auth,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{tracks}");
    let first = &tracks["Items"][0];
    let mut jellyfin_paths = Vec::new();
    for track in tracks["Items"].as_array().expect("tracks") {
        let id = track["Id"].as_str().expect("track id");
        let (status, item) = rig
            .app
            .call("GET", &format!("/jellyfin/Items/{id}"), &auth, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{item}");
        jellyfin_paths.push(item["Path"].as_str().expect("track path").to_owned());
    }
    jellyfin_paths.sort();
    assert_eq!(jellyfin_paths, expected);
    for id in [&first["AlbumId"], &first["AlbumArtists"][0]["Id"]] {
        let id = id.as_str().expect("album or artist id");
        let (status, item) = rig
            .app
            .call("GET", &format!("/jellyfin/Items/{id}"), &auth, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{item}");
        assert_eq!(item["Path"], json!(""), "{item}");
    }
    rig.stop().await;
}

/// Identity, Range and HEAD semantics on the scanned files, identical on
/// both protocols: HEAD answers exactly what GET would, minus the body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_ranges_and_head_on_both_protocols() {
    let rig = CompatApp::start("compat-ranges").await;
    let app = &rig.app;
    let auth = rig.subsonic_auth("matrix");
    let key = format!("api_key={}", rig.secret);

    let (_, found) = app
        .call(
            "GET",
            &rest("search3?query=One&songCount=5", &auth),
            &[],
            None,
        )
        .await;
    let mp3 = named(
        &found["subsonic-response"]["searchResult3"]["song"],
        "title",
        "One",
    )["id"]
        .as_str()
        .expect("song id")
        .to_owned();
    let (_, items) = app
        .call(
            "GET",
            &format!("/jellyfin/Items?includeItemTypes=Audio&Recursive=true&{key}"),
            &[],
            None,
        )
        .await;
    let flac = named(&items["Items"], "Name", "Airbag")["Id"]
        .as_str()
        .expect("track id")
        .to_owned();

    let cases = [
        (
            format!("/subsonic/rest/stream?id={mp3}&{auth}"),
            rig.file("one"),
            "audio/mpeg",
        ),
        (
            format!("/jellyfin/Audio/{flac}/stream?{key}"),
            rig.file("airbag"),
            "audio/flac",
        ),
    ];
    for (url, file, content_type) in &cases {
        let total = file.len() as u64;
        let (status, headers, bytes) = app.raw("GET", url, &[], None).await;
        assert_eq!(status, StatusCode::OK, "{url}");
        assert_eq!(header_str(&headers, "content-type"), *content_type);
        assert_eq!(header_str(&headers, "accept-ranges"), "bytes");
        assert_eq!(header_str(&headers, "content-length"), total.to_string());
        assert_eq!(&bytes, file);
        let (status, headers, bytes) = app.raw("HEAD", url, &[], None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header_str(&headers, "content-length"), total.to_string());
        assert!(bytes.is_empty());

        let last = total - 1;
        let unsatisfiable = format!("bytes */{total}");
        let past_end = format!("bytes={total}-");
        let ranges: [(&str, u16, String, std::ops::Range<u64>); 9] = [
            ("bytes=10-19", 206, format!("bytes 10-19/{total}"), 10..20),
            ("bytes=0-1 ", 206, format!("bytes 0-1/{total}"), 0..2),
            (
                "bytes=-4",
                206,
                format!("bytes {}-{last}/{total}", total - 4),
                total - 4..total,
            ),
            (
                "bytes=90-",
                206,
                format!("bytes 90-{last}/{total}"),
                90..total,
            ),
            (&past_end, 416, unsatisfiable.clone(), 0..0),
            ("bytes=0-0,2-3", 416, unsatisfiable.clone(), 0..0),
            ("bytes=-0", 416, unsatisfiable.clone(), 0..0),
            ("bytes=abc", 416, unsatisfiable.clone(), 0..0),
            ("items=0-1", 416, unsatisfiable.clone(), 0..0),
        ];
        for method in ["GET", "HEAD"] {
            for (range, want, content_range, span) in &ranges {
                let (status, headers, bytes) =
                    app.raw(method, url, &[("Range", range)], None).await;
                let case = format!("{method} {content_type} {range:?}");
                assert_eq!(status.as_u16(), *want, "{case}");
                assert_eq!(
                    header_str(&headers, "content-range"),
                    *content_range,
                    "{case}"
                );
                let len = span.end - span.start;
                if *want == 206 {
                    assert_eq!(
                        header_str(&headers, "content-length"),
                        len.to_string(),
                        "{case}"
                    );
                }
                if method == "GET" {
                    assert_eq!(
                        bytes,
                        file[span.start as usize..span.end as usize],
                        "{case}"
                    );
                } else {
                    assert!(bytes.is_empty(), "{case}");
                }
            }
        }
    }

    // An unknown Subsonic id is the envelope with code 70 over 200;
    // Jellyfin's unknown ids are plain 404s.
    let missing = format!("/subsonic/rest/stream?id=tr-nope&{auth}");
    let (status, headers, bytes) = app.raw("GET", &missing, &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-type"), "application/json");
    assert!(String::from_utf8_lossy(&bytes).contains("\"code\":70"));
    let (status, _, _) = app
        .raw(
            "GET",
            &format!("/jellyfin/Audio/nope/stream?{key}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // No credentials, no audio.
    let (status, _, bytes) = app
        .raw("GET", &format!("/jellyfin/Audio/{flac}/stream"), &[], None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(bytes.is_empty());

    // The container suffix is cosmetic and `universal` serves the same bytes.
    for tail in ["stream.mp3", "universal"] {
        let uri = format!("/jellyfin/Audio/{flac}/{tail}?{key}");
        let (status, _, bytes) = app.raw("GET", &uri, &[], None).await;
        assert_eq!(status, StatusCode::OK, "{tail}");
        assert_eq!(bytes, cases[1].1, "{tail}");
    }
    rig.stop().await;
}

/// Media paths skip the token buckets but not the auth-failure lockout, so
/// guessing passwords over `stream` still trips the backoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_path_auth_lockout_blocks_brute_force() {
    let rig = CompatApp::start("compat-lockout").await;
    let app = &rig.app;
    let auth = rig.subsonic_auth("probe");
    let (_, found) = app
        .call(
            "GET",
            &rest("search3?query=One&songCount=5", &auth),
            &[],
            None,
        )
        .await;
    let song = found["subsonic-response"]["searchResult3"]["song"][0]["id"]
        .as_str()
        .expect("song id")
        .to_owned();
    let guess =
        format!("/subsonic/rest/stream?id={song}&u={PLAYER}&p=wrong&v=1.16.1&c=probe&f=json");
    for _ in 0..5 {
        app.raw("GET", &guess, &[], None).await;
    }
    let url = rest(&format!("stream?id={song}"), &auth);
    let (status, headers, bytes) = app.raw("GET", &url, &[], None).await;
    assert_eq!(status, StatusCode::OK, "Subsonic rejects stay HTTP 200");
    assert!(!header_str(&headers, "retry-after").is_empty());
    let reject: Value = serde_json::from_slice(&bytes).expect("reject json");
    assert_eq!(reject["subsonic-response"]["error"]["code"], json!(0));
    rig.stop().await;
}

/// A player trusts `IsAdministrator`: only the admin role is reported as
/// one, at sign-in and on "who am I". A trusted user is not one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_reports_an_administrator_only_for_an_admin() {
    let rig = CompatApp::start("compat-jellyfin-admin").await;
    let app = &rig.app;
    let owner = rig.admin_secret().await;
    let trusted = rig.trusted_secret("tris").await;

    for (name, secret, is_admin) in [
        (PLAYER, rig.secret.as_str(), false),
        ("tris", &trusted, false),
        ("admin", &owner, true),
    ] {
        let (status, echo) = app
            .call(
                "POST",
                "/jellyfin/Users/AuthenticateByName",
                &[],
                Some(json!({"Username": name, "Pw": secret})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{name}");
        assert_eq!(echo["User"]["Name"], json!(name));
        assert_eq!(
            echo["User"]["Policy"]["IsAdministrator"],
            json!(is_admin),
            "{name} at sign-in"
        );
        let token = format!("MediaBrowser Client=\"Journey\", Token=\"{secret}\"");
        let (status, me) = app
            .call(
                "GET",
                "/jellyfin/Users/Me",
                &[("X-Emby-Authorization", token.as_str())],
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{name}");
        assert_eq!(
            me["Policy"]["IsAdministrator"],
            json!(is_admin),
            "{name} on who am I"
        );
    }
    rig.stop().await;
}

/// A playlist with no uploaded cover gets a picture made from its tracks'
/// albums, for those who may read it only: another user asking for a
/// private playlist's cover gets the placeholder, as for any miss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subsonic_made_playlist_cover_is_private_to_its_readers() {
    let rig = CompatApp::start("compat-playlist-cover").await;
    let app = &rig.app;
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let second = app.config.root_app_dir.join("second");
    let album = second.join("Covered/Sleeve");
    std::fs::create_dir_all(&album).expect("mkdir");
    std::fs::copy(
        manifest.join("tests/fixtures/library/management_full.flac"),
        album.join("01 Sleeve.flac"),
    )
    .expect("fixture copies");
    image::RgbImage::from_pixel(8, 8, image::Rgb([200, 10, 10]))
        .save(album.join("cover.png"))
        .expect("cover writes");
    app.add_library_folder("second", &second).await;

    let admin = app.login_admin().await;
    let (status, created) = app
        .call(
            "POST",
            "/api/v3/me/app-passwords",
            &[("authorization", admin.as_str())],
            Some(json!({"name": "Other"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let other = format!(
        "u=admin&p={}&v=1.16.1&c=cover-check&f=json",
        created["secret"].as_str().expect("secret")
    );
    let owner = rig.subsonic_auth("cover-check");

    let (_, all) = app
        .call(
            "GET",
            &rest("search3?query=&songCount=500", &owner),
            &[],
            None,
        )
        .await;
    let song = all["subsonic-response"]["searchResult3"]["song"]
        .as_array()
        .expect("songs")
        .iter()
        .find(|song| song["path"] == "Covered/Sleeve/01 Sleeve.flac")
        .expect("the covered song")["id"]
        .as_str()
        .expect("song id")
        .to_owned();
    let (_, made) = app
        .call(
            "GET",
            &rest(&format!("createPlaylist?name=Mine&songId={song}"), &owner),
            &[],
            None,
        )
        .await;
    let playlist = &made["subsonic-response"]["playlist"];
    let id = playlist["id"].as_str().expect("playlist id").to_owned();
    assert_eq!(playlist["coverArt"], json!(id));

    let cover = |auth: String| {
        let id = id.clone();
        async move {
            let (status, headers, body) = app
                .raw(
                    "GET",
                    &rest(&format!("getCoverArt?id={id}"), &auth),
                    &[],
                    None,
                )
                .await;
            assert_eq!(status, StatusCode::OK);
            (
                header_str(&headers, "content-type"),
                header_str(&headers, "cache-control"),
                body,
            )
        }
    };
    let (kind, cache, picture) = cover(owner).await;
    assert_eq!(
        (kind.as_str(), cache.as_str()),
        ("image/png", "private, max-age=3600")
    );
    let (kind, cache, placeholder) = cover(other.clone()).await;
    assert_eq!(
        (kind.as_str(), cache.as_str()),
        ("image/webp", "public, max-age=86400")
    );
    assert_ne!(placeholder, picture);

    let (_, lists) = app
        .call("GET", &rest("getPlaylists", &other), &[], None)
        .await;
    assert!(
        !lists["subsonic-response"]["playlists"]
            .to_string()
            .contains(&id),
        "{lists}"
    );
    rig.stop().await;
}

/// The admin's setting for who may download library files holds in player
/// apps. A user it excludes is told so and is refused at the download
/// addresses; a user it admits is told so and is served. Playing is not
/// downloading: everyone still streams, and still gets the original file
/// from the address a Jellyfin app plays it from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_setting_holds_in_player_apps() {
    let rig = CompatApp::start("compat-download-setting").await;
    let app = &rig.app;
    let users = [
        (PLAYER, rig.secret.clone()),
        ("tris", rig.trusted_secret("tris").await),
        ("admin", rig.admin_secret().await),
    ];
    let admin = app.login_admin().await;
    let admin = [("authorization", admin.as_str())];

    let (_, items) = app
        .call(
            "GET",
            &format!(
                "/jellyfin/Items?includeItemTypes=Audio&Recursive=true&api_key={}",
                rig.secret
            ),
            &[],
            None,
        )
        .await;
    let flac = named(&items["Items"], "Name", "Airbag")["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    let (_, found) = app
        .call(
            "GET",
            &rest(
                "search3?query=One&songCount=5",
                &rig.subsonic_auth("journey"),
            ),
            &[],
            None,
        )
        .await;
    let mp3 = named(
        &found["subsonic-response"]["searchResult3"]["song"],
        "title",
        "One",
    )["id"]
        .as_str()
        .expect("song id")
        .to_owned();
    let (airbag, one) = (rig.file("airbag"), rig.file("one"));

    let mut wrong: Vec<String> = Vec::new();
    for (setting, admitted) in [
        ("everyone", [true, true, true]),
        ("trusted", [false, true, true]),
        ("admin", [false, false, true]),
    ] {
        let (status, mut security) = app
            .call("GET", "/api/v3/settings/security", &admin, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{security}");
        security["library_download_access"] = json!(setting);
        let (status, saved) = app
            .call("PUT", "/api/v3/settings/security", &admin, Some(security))
            .await;
        assert_eq!(status, StatusCode::OK, "{saved}");

        for ((name, secret), may) in users.iter().zip(admitted) {
            let case = format!("{name}, downloads for {setting}");
            let key = format!("api_key={secret}");

            let (status, echo) = app
                .call(
                    "POST",
                    "/jellyfin/Users/AuthenticateByName",
                    &[],
                    Some(json!({"Username": name, "Pw": secret})),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{case}");
            let (status, me) = app
                .call("GET", &format!("/jellyfin/Users/Me?{key}"), &[], None)
                .await;
            assert_eq!(status, StatusCode::OK, "{case}");
            for (policy, at) in [
                (&echo["User"]["Policy"], "sign-in"),
                (&me["Policy"], "who am I"),
            ] {
                let told = &policy["EnableContentDownloading"];
                if *told != json!(may) {
                    wrong.push(format!("{case}: EnableContentDownloading {told} at {at}"));
                }
            }
            for method in ["GET", "HEAD"] {
                let uri = format!("/jellyfin/Items/{flac}/Download?{key}");
                let (status, headers, bytes) = app.raw(method, &uri, &[], None).await;
                let served = status == StatusCode::OK
                    && header_str(&headers, "content-disposition")
                        == "attachment; filename=\"Airbag.flac\""
                    && (method == "HEAD" || bytes == airbag);
                let refused = status == StatusCode::FORBIDDEN && bytes.is_empty();
                if !(if may { served } else { refused }) {
                    wrong.push(format!(
                        "{case}: {method} /Items/Download is {status} with {} bytes",
                        bytes.len()
                    ));
                }
            }
            // Playing is not downloading: the address an app plays the
            // original from, and the stream, serve everyone.
            for tail in [format!("Items/{flac}/File"), format!("Audio/{flac}/stream")] {
                let uri = format!("/jellyfin/{tail}?{key}");
                let (status, headers, bytes) = app.raw("GET", &uri, &[], None).await;
                let attached = !header_str(&headers, "content-disposition").is_empty();
                if status != StatusCode::OK || bytes != airbag || attached {
                    wrong.push(format!("{case}: GET {tail} is {status}"));
                }
            }

            let auth = format!("u={name}&p={secret}&v=1.16.1&c=journey&f=json");
            let (_, user) = app
                .call(
                    "GET",
                    &rest(&format!("getUser?username={name}"), &auth),
                    &[],
                    None,
                )
                .await;
            let told = &user["subsonic-response"]["user"]["downloadRole"];
            if *told != json!(may) {
                wrong.push(format!("{case}: downloadRole {told}"));
            }
            for method in ["GET", "HEAD"] {
                let uri = rest(&format!("download?id={mp3}"), &auth);
                let (status, headers, bytes) = app.raw(method, &uri, &[], None).await;
                let kind = header_str(&headers, "content-type");
                let served = kind == "audio/mpeg" && (method == "HEAD" || bytes == one);
                // A Subsonic refusal is an answer: code 50, not authorized.
                let code = serde_json::from_slice::<Value>(&bytes)
                    .map(|body| body["subsonic-response"]["error"]["code"].clone());
                let refused = kind == "application/json"
                    && (method == "HEAD" || code.is_ok_and(|code| code == json!(50)));
                if !(status == StatusCode::OK && if may { served } else { refused }) {
                    wrong.push(format!(
                        "{case}: {method} Subsonic download is {status} with {} bytes",
                        bytes.len()
                    ));
                }
            }
            let uri = rest(&format!("stream?id={mp3}"), &auth);
            let (status, _, bytes) = app.raw("GET", &uri, &[], None).await;
            if status != StatusCode::OK || bytes != one {
                wrong.push(format!("{case}: Subsonic stream is {status}"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    rig.stop().await;
}

/// Served under a base path, the app hands a Jellyfin app addresses that
/// carry it, and the address it gives for a track plays the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_addresses_carry_the_base_path() {
    let mut rig = CompatApp::start("compat-base-path").await;
    rig.app.config.base_path = "/music".to_owned();
    rig.app = rig.app.restart().await;
    let app = &rig.app;
    let key = format!("api_key={}", rig.secret);

    let (status, info) = app
        .call("GET", "/music/jellyfin/System/Info/Public", &[], None)
        .await;
    assert_eq!(status, StatusCode::OK, "{info}");
    assert_eq!(info["LocalAddress"], "http://test.local/music/jellyfin");

    let (_, items) = app
        .call(
            "GET",
            &format!("/music/jellyfin/Items?includeItemTypes=Audio&Recursive=true&{key}"),
            &[],
            None,
        )
        .await;
    let track = named(&items["Items"], "Name", "Airbag")["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    let (status, playback) = app
        .call(
            "GET",
            &format!("/music/jellyfin/Items/{track}/PlaybackInfo?{key}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{playback}");
    let stream = playback["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .expect("direct stream address");
    let path = stream
        .strip_prefix("http://test.local")
        .unwrap_or_else(|| panic!("{stream} is not at the address the app used"));
    assert!(path.starts_with("/music/jellyfin/Audio/"), "{stream}");
    let (status, _, bytes) = app.raw("GET", path, &[], None).await;
    assert_eq!(status, StatusCode::OK, "{stream}");
    assert!(bytes == rig.file("airbag"), "{stream} serves another file");
    rig.stop().await;
}

/// Tripwire: no golden recorded from the library carries a credential.
#[test]
fn library_goldens_carry_no_credentials() {
    let goldens = library_goldens();
    assert!(!goldens.is_empty(), "the library goldens are committed");
    let secret =
        regex::Regex::new(r"(?i)\b(p|t|s|api_key|apikey|token)=([^&\x22\s]+)").expect("pattern");
    for path in goldens {
        let text = std::fs::read_to_string(&path).expect("golden reads");
        for found in secret.captures_iter(&text) {
            assert_eq!(&found[2], "***", "{} carries {}", path.display(), &found[0]);
        }
        for word in ["AccessToken", "Bearer ", "-password-", "correct horse"] {
            assert!(!text.contains(word), "{} carries {word:?}", path.display());
        }
    }
}
