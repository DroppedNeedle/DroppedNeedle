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
    rig.stop().await;
}

fn header_str(map: &HeaderMap, name: &str) -> String {
    map.get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
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

    // Unknown ids are plain 404s; Subsonic's binary path answers as text.
    let missing = format!("/subsonic/rest/stream?id=tr-nope&{auth}");
    let (status, headers, _) = app.raw("GET", &missing, &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&headers, "content-type"), "text/plain");
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
