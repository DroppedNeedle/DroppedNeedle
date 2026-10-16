//! Compat client journeys through the whole app, over a library its own
//! scanner filled from the committed fixture tracks.
//!
//! Symfonium (Subsonic) and Finamp (Jellyfin) each run browse, stream,
//! favorite, playlist and scrobble; Jellify fetches `Latest` and plays the
//! stream URL with no auth headers, the token in its `api_key` query.
//! Arpeggi syncs its artist list and opens one artist.
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

    let latest = trace.get("/jellyfin/Items/Latest", &auth).await;
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

/// Arpeggi's artist browsing, as recorded from Navidrome 0.62.0 in
/// `navidrome-0.62.0-arpeggi`. Its library sync is a search3 with no query
/// (Arpeggi-App issue #6), and it may ask getIndexes with its own clock time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arpeggi_syncs_artists_then_opens_one() {
    let rig = CompatApp::start("compat-arpeggi").await;
    let mut trace = Trace::new(&rig);
    let auth = rig.subsonic_auth("Arpeggi");

    trace.get(&rest("ping", &auth), &[]).await;
    trace.get(&rest("getMusicFolders", &auth), &[]).await;
    let sync = "search3?artistCount=500&artistOffset=0&albumCount=0&albumOffset=0\
                &songCount=0&songOffset=0&musicFolderId=1";
    let synced = trace.get(&rest(sync, &auth), &[]).await;
    let synced = &synced["subsonic-response"]["searchResult3"]["artist"];
    let artists = trace
        .get(&rest("getArtists?musicFolderId=1", &auth), &[])
        .await;
    let indexed = artists["subsonic-response"]["artists"]["index"]
        .as_array()
        .expect("artist index");
    let listed: Vec<&Value> = synced
        .as_array()
        .expect("synced artists")
        .iter()
        .chain(indexed.iter().flat_map(|index| {
            index["artist"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
        }))
        .collect();
    assert!(!listed.is_empty(), "{artists}");
    for artist in listed {
        assert!(artist["musicBrainzId"].is_string(), "{artist}");
    }

    let indexes = trace
        .get(
            &rest(
                "getIndexes?musicFolderId=1&ifModifiedSince=1760000000000",
                &auth,
            ),
            &[],
        )
        .await;
    let index = &indexes["subsonic-response"]["indexes"]["index"];
    assert!(
        index.as_array().is_some_and(|index| !index.is_empty()),
        "a clock time gets the full index: {indexes}"
    );

    let yuki = named(synced, "name", "ユキ")["id"]
        .as_str()
        .expect("artist id")
        .to_owned();
    let artist = trace
        .get(&rest(&format!("getArtist?id={yuki}"), &auth), &[])
        .await;
    let albums = artist["subsonic-response"]["artist"]["album"]
        .as_array()
        .expect("albums");
    assert!(!albums.is_empty(), "{artist}");
    for album in albums {
        for key in ["created", "displayArtist", "artists"] {
            assert!(album.get(key).is_some(), "{key} missing: {album}");
        }
        assert_eq!(album["musicBrainzId"], json!(""), "{album}");
    }

    assert_golden("library_arpeggi.trace.json", &trace.body());
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
    let latest = trace.get("/jellyfin/Items/Latest", &auth).await;
    assert_golden("library_pinned_jellyfin_latest.json", &trace.recorded());

    // The rig identifies no album and Mezzanine's tags hold no well-formed
    // MusicBrainz id, so it sends none.
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

/// The MusicBrainz ids player apps get for an album come from the files'
/// tags until the app has matched it, then from the accepted match, even
/// where the tags say otherwise: Subsonic sends the release (the release
/// group when the match knows no release), Jellyfin both. Reading them
/// writes no identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_album_sends_the_accepted_ids_else_its_tags() {
    const TAG_RELEASE: &str = "0da3b3e3-1111-4444-8888-aaaaaaaaaaaa";
    const TAG_GROUP: &str = "b1392450-e666-3926-a536-22c65f834433";
    const RELEASE: &str = "44444444-4444-4444-8444-444444444444";
    const GROUP: &str = "55555555-5555-4555-8555-555555555555";
    let rig = CompatApp::start("compat-player-ids").await;
    let (airbag, album, _) = catalog_ids(&rig, "Radiohead/OK Computer/01 Airbag.flac");

    let before = identity_rows(&rig);
    let (subsonic, jellyfin) = album_ids_sent(&rig, &album, &airbag).await;
    assert_eq!(subsonic, json!(TAG_RELEASE));
    assert_eq!(
        jellyfin,
        json!({"MusicBrainzAlbum": TAG_RELEASE, "MusicBrainzReleaseGroup": TAG_GROUP})
    );
    assert_eq!(identity_rows(&rig), before, "reading wrote an identity");

    rig.app
        .db()
        .execute_batch(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', '{GROUP}', '{RELEASE}', 'manual', 1) \
             ON CONFLICT (local_album_id, provider) DO UPDATE SET \
             release_group_mbid = excluded.release_group_mbid, \
             release_mbid = excluded.release_mbid, decision_source = 'manual'; \
             DELETE FROM library_album_match_state WHERE local_album_id = '{album}';"
        ))
        .expect("match accepted");
    let before = identity_rows(&rig);
    let (subsonic, jellyfin) = album_ids_sent(&rig, &album, &airbag).await;
    assert_eq!(subsonic, json!(RELEASE));
    assert_eq!(
        jellyfin,
        json!({"MusicBrainzAlbum": RELEASE, "MusicBrainzReleaseGroup": GROUP})
    );
    assert_eq!(identity_rows(&rig), before, "reading wrote an identity");

    rig.app
        .db()
        .execute(
            "UPDATE local_album_external_identities SET release_mbid = NULL \
             WHERE local_album_id = ?1",
            [album.as_str()],
        )
        .expect("release unknown");
    let (subsonic, jellyfin) = album_ids_sent(&rig, &album, &airbag).await;
    assert_eq!(subsonic, json!(GROUP));
    assert_eq!(jellyfin, json!({"MusicBrainzReleaseGroup": GROUP}));
    rig.stop().await;
}

/// An artist row whose files name two MusicBrainz artists for it, with no
/// list pairing names and ids, is no single artist: player apps get no
/// MusicBrainz id for it, on Subsonic or on a Jellyfin track.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_composite_artist_sends_no_musicbrainz_id() {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    const FIRST: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const SECOND: &str = "aaaaaaaa-0000-4000-8000-000000000002";
    let rig = CompatApp::start("compat-composite-artist").await;
    let music = rig.app.config.root_app_dir.join("music");
    let duet = music.join("Duo/Together/01.flac");
    std::fs::create_dir_all(duet.parent().expect("folder")).expect("folder made");
    std::fs::copy(music.join("Radiohead/OK Computer/01 Airbag.flac"), &duet).expect("copied");
    let one = |value: &str| vec![value.to_owned()];
    let both = vec![FIRST.to_owned(), SECOND.to_owned()];
    save_tags(
        &duet,
        &[
            TagEdit::verbatim(TagField::Album, one("Together")),
            TagEdit::verbatim(TagField::Artist, one("Jay-Z & Kanye West")),
            TagEdit::verbatim(TagField::AlbumArtist, one("Jay-Z & Kanye West")),
            TagEdit::verbatim(TagField::Artists, Vec::new()),
            TagEdit::verbatim(TagField::AlbumArtists, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzArtistId, both.clone()),
            TagEdit::verbatim(TagField::MusicBrainzAlbumArtistId, both),
            TagEdit::verbatim(TagField::MusicBrainzReleaseId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzReleaseGroupId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
        ],
    )
    .expect("copy tagged");
    rig.app.scan_library().await;
    let (track, _, artist) = catalog_ids(&rig, "Duo/Together/01.flac");

    let answer = subsonic_ok(&rig, &format!("getArtist?id=ar-{artist}")).await;
    assert_eq!(answer["artist"]["musicBrainzId"], json!(""), "{answer}");
    let token = rig.jellyfin_auth();
    let (status, item) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Items/{}", unkeyed_id("track", &track)),
            &[("X-Emby-Authorization", token.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{item}");
    assert_eq!(item["ProviderIds"].get("MusicBrainzArtist"), None, "{item}");
    rig.stop().await;
}

/// What Subsonic `getAlbum` sends as the album's `musicBrainzId`, and the
/// `ProviderIds` of the album a Jellyfin app reaches from its `track`.
async fn album_ids_sent(rig: &CompatApp, album: &str, track: &str) -> (Value, Value) {
    let subsonic = subsonic_ok(rig, &format!("getAlbum?id=al-{album}")).await;
    let token = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", token.as_str())];
    let (status, item) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Items/{}", unkeyed_id("track", track)),
            &auth,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{item}");
    let album_id = item["AlbumId"].as_str().expect("AlbumId");
    let (status, album) = rig
        .app
        .call("GET", &format!("/jellyfin/Items/{album_id}"), &auth, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{album}");
    (
        subsonic["album"]["musicBrainzId"].clone(),
        album["ProviderIds"].clone(),
    )
}

/// Every row of the external identity tables, in a fixed order.
fn identity_rows(rig: &CompatApp) -> Vec<String> {
    let db = rig.app.db();
    let mut rows = Vec::new();
    for sql in [
        "SELECT local_album_id || ' ' || release_group_mbid || ' ' || \
         COALESCE(release_mbid, '-') || ' ' || row_revision \
         FROM local_album_external_identities ORDER BY 1",
        "SELECT local_artist_id || ' ' || provider_artist_id || ' ' || row_revision \
         FROM local_artist_external_identities ORDER BY 1",
        "SELECT local_track_id || ' ' || COALESCE(recording_mbid, '-') || ' ' || \
         COALESCE(release_mbid, '-') FROM local_track_external_identities ORDER BY 1",
    ] {
        let mut statement = db.prepare(sql).expect("identity read");
        let read = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("identity rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("identity row");
        rows.extend(read);
    }
    rows
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

/// The address every request in these journeys is sent to.
const SENT_TO: &str = "http://test.local";

/// The library with an album whose folder holds a `cover.png`, and the
/// Subsonic ids of that album, its artist and its one song.
async fn covered_album(tag: &str) -> (CompatApp, [String; 3]) {
    let rig = CompatApp::start(tag).await;
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let second = rig.app.config.root_app_dir.join("second");
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
    rig.app.add_library_folder("second", &second).await;
    let auth = rig.subsonic_auth("pictures");
    let (_, all) = rig
        .app
        .call(
            "GET",
            &rest("search3?query=&songCount=500", &auth),
            &[],
            None,
        )
        .await;
    let song = all["subsonic-response"]["searchResult3"]["song"]
        .as_array()
        .expect("songs")
        .iter()
        .find(|song| song["path"] == "Covered/Sleeve/01 Sleeve.flac")
        .expect("the covered song")
        .clone();
    let album_id = song["albumId"].as_str().expect("album id").to_owned();
    let (_, album) = rig
        .app
        .call(
            "GET",
            &rest(&format!("getAlbum?id={album_id}"), &auth),
            &[],
            None,
        )
        .await;
    let artist_id = album["subsonic-response"]["album"]["artistId"]
        .as_str()
        .expect("album artist id")
        .to_owned();
    let song_id = song["id"].as_str().expect("song id").to_owned();
    (rig, [album_id, artist_id, song_id])
}

/// The path and query of an open picture link, after checking it is
/// absolute at the address the request was sent to and carries nothing
/// but the id, the signature and the size.
fn picture_path(link: &Value) -> String {
    let link = link.as_str().unwrap_or_else(|| panic!("no link: {link}"));
    let path = link
        .strip_prefix(SENT_TO)
        .unwrap_or_else(|| panic!("{link} is not under {SENT_TO}"));
    let query = path
        .strip_prefix("/subsonic/img?")
        .unwrap_or_else(|| panic!("{link} is not a picture link"));
    let keys: Vec<&str> = query
        .split('&')
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .collect();
    assert_eq!(keys, ["id", "sig", "size"], "{link}");
    path.to_owned()
}

/// The three links of a `getAlbumInfo2` or `getArtistInfo2` answer.
fn info_links(info: &Value) -> Vec<String> {
    ["smallImageUrl", "mediumImageUrl", "largeImageUrl"]
        .iter()
        .map(|key| picture_path(&info[key]))
        .collect()
}

/// Album and artist picture links open with no credentials: the links of
/// `getAlbumInfo2`, `getArtistInfo2` and an artist's `artistImageUrl` are
/// absolute at the address the app used, carry no credential, and answer
/// a picture with `nosniff`, a sandbox policy and a one-day cache to a
/// request that signs in with nothing. The album's picture is the one
/// `getCoverArt` gives a signed-in user, and the same request gets the
/// same link every time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subsonic_picture_links_open_without_sign_in() {
    let (rig, [album_id, artist_id, _]) = covered_album("compat-picture-links").await;
    let app = &rig.app;
    let auth = rig.subsonic_auth("pictures");
    let get = |call: String| {
        let auth = auth.clone();
        async move { app.call("GET", &rest(&call, &auth), &[], None).await.1 }
    };

    let album_info = get(format!("getAlbumInfo2?id={album_id}")).await;
    let album_links = info_links(&album_info["subsonic-response"]["albumInfo"]);
    let again = get(format!("getAlbumInfo2?id={album_id}")).await;
    assert_eq!(
        info_links(&again["subsonic-response"]["albumInfo"]),
        album_links
    );
    let artist_info = get(format!("getArtistInfo2?id={artist_id}")).await;
    let mut links = info_links(&artist_info["subsonic-response"]["artistInfo2"]);
    let artists = get("getArtists".to_owned()).await;
    let artist = artists["subsonic-response"]["artists"]["index"]
        .as_array()
        .expect("index")
        .iter()
        .flat_map(|bucket| bucket["artist"].as_array().expect("artists").iter())
        .find(|artist| artist["id"] == artist_id.as_str())
        .expect("the album artist in getArtists")
        .clone();
    links.push(picture_path(&artist["artistImageUrl"]));
    links.extend(album_links.iter().cloned());

    for link in &links {
        let (status, headers, body) = app.raw("GET", link, &[], None).await;
        assert_eq!(status, StatusCode::OK, "{link}");
        assert!(
            header_str(&headers, "content-type").starts_with("image/"),
            "{link}"
        );
        assert_eq!(header_str(&headers, "x-content-type-options"), "nosniff");
        assert_eq!(
            header_str(&headers, "content-security-policy"),
            "sandbox; default-src 'none'"
        );
        assert_eq!(
            header_str(&headers, "cache-control"),
            "public, max-age=86400",
            "{link}"
        );
        assert!(!body.is_empty(), "{link}");
    }

    let (_, _, opened) = app.raw("GET", &album_links[2], &[], None).await;
    let (status, headers, served) = app
        .raw(
            "GET",
            &rest(&format!("getCoverArt?id={album_id}&size=1200"), &auth),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-type"), "image/png");
    assert_eq!(opened, served);
    rig.stop().await;
}

/// The picture address opens nothing else, each refusal the same empty
/// 404: a signature with one character changed, an album's signature on
/// its artist's id, on a readable playlist's id or on a song's id, no
/// signature, and a good link while the Subsonic API is off. None of them
/// counts as a failed sign-in: the client that sent them still signs in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subsonic_picture_address_opens_nothing_else() {
    let (rig, [album_id, artist_id, song_id]) = covered_album("compat-picture-refusals").await;
    let app = &rig.app;
    let auth = rig.subsonic_auth("pictures");
    let (_, info) = app
        .call(
            "GET",
            &rest(&format!("getAlbumInfo2?id={album_id}"), &auth),
            &[],
            None,
        )
        .await;
    let link = picture_path(&info["subsonic-response"]["albumInfo"]["mediumImageUrl"]);
    let (status, _, _) = app.raw("GET", &link, &[], None).await;
    assert_eq!(status, StatusCode::OK, "the link opens");
    let sig = link
        .split('&')
        .find_map(|pair| pair.strip_prefix("sig="))
        .expect("signature")
        .to_owned();
    let (_, made) = app
        .call(
            "GET",
            &rest(&format!("createPlaylist?name=Mine&songId={song_id}"), &auth),
            &[],
            None,
        )
        .await;
    let playlist_id = made["subsonic-response"]["playlist"]["id"]
        .as_str()
        .expect("playlist id")
        .to_owned();

    let flipped = format!(
        "{}{}",
        if sig.starts_with('0') { '1' } else { '0' },
        &sig[1..]
    );
    let refused = [
        link.replace(&sig, &flipped),
        format!("/subsonic/img?id={artist_id}&sig={sig}&size=500"),
        format!("/subsonic/img?id={playlist_id}&sig={sig}&size=500"),
        format!("/subsonic/img?id={song_id}&sig={sig}&size=500"),
        format!("/subsonic/img?id={album_id}&size=500"),
    ];
    for path in &refused {
        let (status, _, body) = app.raw("GET", path, &[], None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(body.is_empty(), "{path}");
    }

    let admin = app.login_admin().await;
    let admin = [("authorization", admin.as_str())];
    let switch = |on: bool| async move {
        let (_, mut apps) = app
            .call("GET", "/api/v3/settings/connect-apps", &admin, None)
            .await;
        apps["subsonic_enabled"] = json!(on);
        let (status, saved) = app
            .call("PUT", "/api/v3/settings/connect-apps", &admin, Some(apps))
            .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
    };
    switch(false).await;
    let (status, _, body) = app.raw("GET", &link, &[], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "while Subsonic is off");
    assert!(body.is_empty());
    switch(true).await;

    let (_, ping) = app.call("GET", &rest("ping", &auth), &[], None).await;
    assert_eq!(ping["subsonic-response"]["status"], "ok", "{ping}");
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

/// `sha256("kind:internal")[:32]`: every Jellyfin id before album and
/// artist ids were keyed, and still the id of a track.
fn unkeyed_id(kind: &str, internal: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(format!("{kind}:{internal}").as_bytes())
    )[..32]
        .to_owned()
}

/// Jellyfin ids of the covered album: what a signed-in app is handed for
/// the album, its artist and its song, and the album's and artist's old
/// unkeyed ids.
struct CoveredIds {
    album: String,
    artist: String,
    track: String,
    old_album: String,
    old_artist: String,
}

/// The covered album's ids, read as a signed-in Jellyfin app reads them.
/// Its artist is given a MusicBrainz id and a cached picture so the
/// artist's picture has something to serve.
async fn covered_jellyfin(tag: &str) -> (CompatApp, CoveredIds) {
    let (rig, _) = covered_album(tag).await;
    let (track, album, artist): (String, String, String) = rig
        .app
        .db()
        .query_row(
            "SELECT t.id, t.local_album_id, a.album_artist_id FROM local_tracks t \
             JOIN local_albums a ON a.id = t.local_album_id \
             WHERE t.relative_path = 'Covered/Sleeve/01 Sleeve.flac'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the covered song's catalog ids");
    let mbid = "5b11f4ce-a62d-471e-81fc-a69a8278c7da";
    rig.app
        .db()
        .execute(
            "INSERT OR REPLACE INTO local_artist_external_identities \
             (local_artist_id, provider, provider_artist_id, decision_source, selected_at) \
             VALUES (?1, 'musicbrainz', ?2, 'manual', 1000)",
            [artist.as_str(), mbid],
        )
        .expect("artist identity seeds");
    plant_artist_picture(&rig, mbid).await;

    let token = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", token.as_str())];
    let track_id = unkeyed_id("track", &track);
    let (status, item) = rig
        .app
        .call("GET", &format!("/jellyfin/Items/{track_id}"), &auth, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{item}");
    assert_eq!(item["Id"], json!(track_id), "a track keeps its unkeyed id");
    let album_id = item["AlbumId"].as_str().expect("AlbumId").to_owned();
    let artist_id = item["AlbumArtists"][0]["Id"]
        .as_str()
        .expect("album artist id")
        .to_owned();
    let (_, albums) = rig
        .app
        .call(
            "GET",
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&Recursive=true",
            &auth,
            None,
        )
        .await;
    assert!(
        albums["Items"]
            .as_array()
            .expect("albums")
            .iter()
            .any(|listed| listed["Id"] == json!(album_id)),
        "{albums}"
    );
    let ids = CoveredIds {
        album: album_id,
        artist: artist_id,
        track: track_id,
        old_album: unkeyed_id("album", &album),
        old_artist: unkeyed_id("artist", &artist),
    };
    (rig, ids)
}

/// Put a picture for the artist `mbid` in the cover cache, as a fetch from
/// a provider stores one.
async fn plant_artist_picture(rig: &CompatApp, mbid: &str) {
    let mut png = Vec::new();
    image::RgbImage::from_pixel(4, 4, image::Rgb([10, 200, 10]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("picture encodes");
    droppedneedle_catalog::reads::platform::artwork::cache::ArtworkCache::new(
        rig.app.config.cache_dir.join("covers"),
        64 * 1024 * 1024,
    )
    .put(Some(&format!("artist:{mbid}")), png, "image/png")
    .await;
}

/// One picture request with no credentials, or with `api_key` when given.
async fn picture(
    rig: &CompatApp,
    id: &str,
    api_key: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let query = api_key.map_or(String::new(), |key| format!("?api_key={key}"));
    rig.app
        .raw(
            "GET",
            &format!("/jellyfin/Items/{id}/Images/Primary{query}"),
            &[],
            None,
        )
        .await
}

/// The Jellyfin picture address opens only for ids the server handed out.
/// The album's and the artist's ids from a signed-in answer open with no
/// credentials, as an image with `nosniff` and a sandbox policy. Their old
/// unkeyed ids, which anyone can work out from public data, are an empty
/// 404 with or without a valid token, and so is a track's id; the track
/// points at its album's picture instead. None of these refusals counts
/// as a failed sign-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_pictures_open_only_for_ids_the_server_handed_out() {
    let (rig, ids) = covered_jellyfin("compat-jellyfin-pictures").await;
    for id in [&ids.album, &ids.artist] {
        let (status, headers, body) = picture(&rig, id, None).await;
        assert_eq!(status, StatusCode::OK, "{id}");
        assert!(
            header_str(&headers, "content-type").starts_with("image/"),
            "{id}"
        );
        assert_eq!(header_str(&headers, "x-content-type-options"), "nosniff");
        assert_eq!(
            header_str(&headers, "content-security-policy"),
            "sandbox; default-src 'none'"
        );
        assert_eq!(
            header_str(&headers, "cache-control"),
            "public, max-age=31536000, immutable"
        );
        assert!(!body.is_empty(), "{id}");
    }

    let token = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", token.as_str())];
    let (_, track) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Items/{}", ids.track),
            &auth,
            None,
        )
        .await;
    assert_eq!(track["AlbumId"], json!(ids.album));
    assert!(track["AlbumPrimaryImageTag"].is_string(), "{track}");
    assert_eq!(track["ImageTags"], json!({}), "{track}");

    let unknown = "0123456789abcdef0123456789abcdef".to_owned();
    let refused = [
        (&ids.old_album, None),
        (&ids.old_album, Some(rig.secret.as_str())),
        (&ids.old_artist, None),
        (&ids.old_artist, Some(rig.secret.as_str())),
        (&ids.track, None),
        (&unknown, None),
    ];
    for (id, api_key) in refused {
        let (status, _, body) = picture(&rig, id, api_key).await;
        assert_eq!(
            (status, body.len()),
            (StatusCode::NOT_FOUND, 0),
            "{id} with {api_key:?}"
        );
    }
    let (status, me) = rig.app.call("GET", "/jellyfin/Users/Me", &auth, None).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    rig.stop().await;
}

/// A signed-in app that stored an album's old unkeyed id still reaches the
/// album through it: the item answers with its current id, browsing it
/// lists its tracks and favouriting it sticks. With no token the same
/// address is a 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_old_album_ids_still_work_signed_in() {
    let (rig, ids) = covered_jellyfin("compat-jellyfin-old-ids").await;
    let token = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", token.as_str())];
    let old = &ids.old_album;

    let (status, item) = rig
        .app
        .call("GET", &format!("/jellyfin/Items/{old}"), &auth, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{item}");
    assert_eq!(item["Id"], json!(ids.album));
    let (_, children) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Items?ParentId={old}"),
            &auth,
            None,
        )
        .await;
    let listed: Vec<&Value> = children["Items"]
        .as_array()
        .expect("children")
        .iter()
        .map(|child| &child["Id"])
        .collect();
    assert_eq!(listed, [&json!(ids.track)], "{children}");
    let (status, marked) = rig
        .app
        .call(
            "POST",
            &format!("/jellyfin/UserFavoriteItems/{old}"),
            &auth,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{marked}");
    assert_eq!(marked["ItemId"], json!(ids.album), "{marked}");
    let (_, favourites) = rig
        .app
        .call(
            "GET",
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&Recursive=true&IsFavorite=true",
            &auth,
            None,
        )
        .await;
    assert_eq!(
        favourites["Items"][0]["Id"],
        json!(ids.album),
        "{favourites}"
    );

    let (status, _) = rig
        .app
        .call("GET", &format!("/jellyfin/Items/{old}"), &[], None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    rig.stop().await;
}

/// An album keeps its id across a restart, and its picture opens when that
/// is the first request the new process gets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_album_ids_survive_a_restart() {
    let (mut rig, ids) = covered_jellyfin("compat-jellyfin-restart").await;
    rig.app = rig.app.restart().await;
    let (status, _, body) = picture(&rig, &ids.album, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_empty());

    let token = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", token.as_str())];
    let (_, track) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Items/{}", ids.track),
            &auth,
            None,
        )
        .await;
    assert_eq!(track["AlbumId"], json!(ids.album));
    rig.stop().await;
}

/// The local track, album and album artist ids of the song at `path`.
fn catalog_ids(rig: &CompatApp, path: &str) -> (String, String, String) {
    rig.app
        .db()
        .query_row(
            "SELECT t.id, t.local_album_id, a.album_artist_id FROM local_tracks t \
             JOIN local_albums a ON a.id = t.local_album_id WHERE t.relative_path = ?1",
            [path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// Fold the album holding `track` into `into`, as an admin does from the
/// album tools.
async fn merge_albums(rig: &CompatApp, track: &str, into: &str) {
    let admin = rig.app.login_admin().await;
    let admin = [("authorization", admin.as_str())];
    let mut selection = json!({"track_ids": [track], "target_album_id": into});
    let (status, preview) = rig
        .app
        .call(
            "POST",
            "/api/v3/library/albums/merge-preview",
            &admin,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    selection["preview_token"] = preview["preview_token"].clone();
    let (status, body) = rig
        .app
        .call(
            "POST",
            "/api/v3/library/albums/merge",
            &admin,
            Some(selection),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Merge the artist `from` into `into`, as an admin does.
async fn merge_artists(rig: &CompatApp, from: &str, into: &str) {
    let admin = rig.app.login_admin().await;
    let admin = [("authorization", admin.as_str())];
    let mut selection = json!({"source_artist_ids": [from], "surviving_artist_id": into});
    let (status, preview) = rig
        .app
        .call(
            "POST",
            "/api/v3/library/artists/merge-preview",
            &admin,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    selection["preview_token"] = preview["preview_token"].clone();
    let (status, body) = rig
        .app
        .call(
            "POST",
            "/api/v3/library/artists/merge",
            &admin,
            Some(selection),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The Jellyfin id a signed-in app is handed for the album called `name`.
async fn jellyfin_album_id(rig: &CompatApp, name: &str) -> String {
    let token = rig.jellyfin_auth();
    let (_, albums) = rig
        .app
        .call(
            "GET",
            "/jellyfin/Items?IncludeItemTypes=MusicAlbum&Recursive=true",
            &[("X-Emby-Authorization", token.as_str())],
            None,
        )
        .await;
    named(&albums["Items"], "Name", name)["Id"]
        .as_str()
        .expect("album id")
        .to_owned()
}

/// The Jellyfin id a signed-in app is handed for the album artist of the
/// track `track`.
async fn jellyfin_artist_id(rig: &CompatApp, track: &str) -> String {
    let token = rig.jellyfin_auth();
    let (status, item) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Items/{}", unkeyed_id("track", track)),
            &[("X-Emby-Authorization", token.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{item}");
    item["AlbumArtists"][0]["Id"]
        .as_str()
        .expect("album artist id")
        .to_owned()
}

/// The `Id` a signed-in Jellyfin app is answered for the item `id`, and
/// the ids of the tracks listed under it as a parent.
async fn jellyfin_item(rig: &CompatApp, id: &str) -> (Value, Vec<String>) {
    let token = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", token.as_str())];
    let (status, item) = rig
        .app
        .call("GET", &format!("/jellyfin/Items/{id}"), &auth, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{id}: {item}");
    let (_, children) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Items?ParentId={id}"),
            &auth,
            None,
        )
        .await;
    let mut listed: Vec<String> = children["Items"]
        .as_array()
        .unwrap_or_else(|| panic!("{id}: {children}"))
        .iter()
        .map(|child| child["Id"].as_str().expect("child id").to_owned())
        .collect();
    listed.sort();
    (item["Id"].clone(), listed)
}

/// The Subsonic answer to `call`, which must succeed.
async fn subsonic_ok(rig: &CompatApp, call: &str) -> Value {
    let auth = rig.subsonic_auth("merged");
    let (_, body) = rig.app.call("GET", &rest(call, &auth), &[], None).await;
    let answer = &body["subsonic-response"];
    assert_eq!(answer["status"], json!("ok"), "{call}: {body}");
    answer.clone()
}

/// A player app that saved an album before it was merged into another
/// still opens it, at the album that holds it now, also after a second
/// merge and a restart. Subsonic: `getAlbum` answers the surviving album
/// with its id, `getMusicDirectory` and `getCoverArt` answer as for that
/// album's id, and starring the old id stars it. Jellyfin, signed in, for
/// both ids the album was handed out under: the item answers with the
/// current id and lists the album's tracks, also as the first request of
/// a new process; with no token it is a 401. The picture address never
/// opens an old id: an empty 404 in the process that listed it and as the
/// first request after a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_albums_keep_their_ids_in_player_apps() {
    let mut rig = CompatApp::start("compat-merged-albums").await;
    let (airbag, ok_computer, _) = catalog_ids(&rig, "Radiohead/OK Computer/01 Airbag.flac");
    let (paranoid, _, _) = catalog_ids(&rig, "Radiohead/OK Computer/02 Paranoid Android.flac");
    let (teardrop, mezzanine, _) = catalog_ids(&rig, "Massive Attack/Mezzanine/04 Teardrop.m4a");
    let (one, achtung, _) = catalog_ids(&rig, "U2/Achtung Baby/03 One.mp3");
    let keyed = jellyfin_album_id(&rig, "Mezzanine").await;
    let unkeyed = unkeyed_id("album", &mezzanine);
    let ok_computer_id = jellyfin_album_id(&rig, "OK Computer").await;
    let achtung_id = jellyfin_album_id(&rig, "Achtung Baby").await;

    merge_albums(&rig, &teardrop, &ok_computer).await;
    let old = format!("al-{mezzanine}");
    let album = subsonic_ok(&rig, &format!("getAlbum?id={old}")).await;
    assert_eq!(album["album"]["id"], json!(format!("al-{ok_computer}")));
    let survivor = subsonic_ok(&rig, &format!("getMusicDirectory?id=al-{ok_computer}")).await;
    assert_eq!(
        subsonic_ok(&rig, &format!("getMusicDirectory?id={old}")).await,
        survivor
    );
    let auth = rig.subsonic_auth("merged");
    let cover = rig
        .app
        .raw(
            "GET",
            &rest(&format!("getCoverArt?id=al-{ok_computer}"), &auth),
            &[],
            None,
        )
        .await;
    let (status, _, bytes) = rig
        .app
        .raw(
            "GET",
            &rest(&format!("getCoverArt?id={old}"), &auth),
            &[],
            None,
        )
        .await;
    assert_eq!((status, bytes), (cover.0, cover.2));
    subsonic_ok(&rig, &format!("star?id={old}")).await;
    let starred = subsonic_ok(&rig, "getStarred2").await;
    assert_eq!(
        starred["starred2"]["album"][0]["id"],
        json!(format!("al-{ok_computer}")),
        "{starred}"
    );

    let mut tracks: Vec<String> = [&airbag, &paranoid, &teardrop]
        .iter()
        .map(|track| unkeyed_id("track", track))
        .collect();
    tracks.sort();
    for id in [&keyed, &unkeyed] {
        assert_eq!(
            jellyfin_item(&rig, id).await,
            (json!(ok_computer_id), tracks.clone())
        );
        let (status, _) = rig
            .app
            .call("GET", &format!("/jellyfin/Items/{id}"), &[], None)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{id}");
    }
    let (status, _, body) = picture(&rig, &keyed, None).await;
    assert_eq!((status, body.len()), (StatusCode::NOT_FOUND, 0));
    // The survivor has no picture in this library, and its miss names it.
    let (status, _, body) = picture(&rig, &ok_computer_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("a JSON answer"),
        json!("OK Computer does not have an image of type Primary")
    );

    merge_albums(&rig, &airbag, &achtung).await;
    let album = subsonic_ok(&rig, &format!("getAlbum?id={old}")).await;
    assert_eq!(album["album"]["id"], json!(format!("al-{achtung}")));
    tracks.push(unkeyed_id("track", &one));
    tracks.sort();
    for id in [&keyed, &unkeyed] {
        rig.app = rig.app.restart().await;
        assert_eq!(
            jellyfin_item(&rig, id).await,
            (json!(achtung_id), tracks.clone()),
            "{id} after a restart"
        );
    }
    rig.app = rig.app.restart().await;
    let (status, _, body) = picture(&rig, &keyed, None).await;
    assert_eq!((status, body.len()), (StatusCode::NOT_FOUND, 0));
    rig.stop().await;
}

/// A player app that saved an artist before two merges in a row opens the
/// last artist through the first one's Subsonic id and Jellyfin id. An
/// album known by an alias of an older kind opens on Subsonic, while
/// Jellyfin, which never handed that alias out, has no id for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_artists_and_album_aliases_open_in_player_apps() {
    let rig = CompatApp::start("compat-merged-artists").await;
    let (teardrop, _, massive_attack) =
        catalog_ids(&rig, "Massive Attack/Mezzanine/04 Teardrop.m4a");
    let (airbag, _, radiohead) = catalog_ids(&rig, "Radiohead/OK Computer/01 Airbag.flac");
    let (one, _, u2) = catalog_ids(&rig, "U2/Achtung Baby/03 One.mp3");
    let (_, bokou, _) = catalog_ids(&rig, "ユキ/望厚/01 桃源へ.flac");
    let keyed = jellyfin_artist_id(&rig, &teardrop).await;
    let radiohead_id = jellyfin_artist_id(&rig, &airbag).await;
    let u2_id = jellyfin_artist_id(&rig, &one).await;
    let old = format!("ar-{massive_attack}");

    merge_artists(&rig, &massive_attack, &radiohead).await;
    let artist = subsonic_ok(&rig, &format!("getArtist?id={old}")).await;
    assert_eq!(artist["artist"]["id"], json!(format!("ar-{radiohead}")));
    assert_eq!(jellyfin_item(&rig, &keyed).await.0, json!(radiohead_id));

    merge_artists(&rig, &radiohead, &u2).await;
    let artist = subsonic_ok(&rig, &format!("getArtist?id={old}")).await;
    assert_eq!(artist["artist"]["id"], json!(format!("ar-{u2}")));
    for id in [&keyed, &unkeyed_id("artist", &massive_attack)] {
        assert_eq!(jellyfin_item(&rig, id).await.0, json!(u2_id), "{id}");
    }

    let legacy = "8c2a6c8e-61a3-4a31-9a0f-3a4c5e6b7d80";
    rig.app
        .db()
        .execute(
            "INSERT INTO local_album_aliases (alias, local_album_id, kind, created_at) \
             VALUES (?1, ?2, 'legacy_release_group', 1)",
            [legacy, bokou.as_str()],
        )
        .expect("legacy alias seeds");
    let album = subsonic_ok(&rig, &format!("getAlbum?id=al-{legacy}")).await;
    assert_eq!(album["album"]["id"], json!(format!("al-{bokou}")));
    let token = rig.jellyfin_auth();
    let (status, _) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Items/{}", unkeyed_id("album", legacy)),
            &[("X-Emby-Authorization", token.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    rig.stop().await;
}

/// An artist row proven by its files to be MusicBrainz artist x merges at
/// the next scan into the row a person links to x, and a player app that
/// saved the merged artist opens the survivor: Subsonic `getArtist`
/// answers with the survivor's id, Jellyfin signed in with the survivor's
/// `Id`, and with no token it is a 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_artist_merged_by_its_musicbrainz_id_opens_in_player_apps() {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    const ARTIST: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    let rig = CompatApp::start("compat-converged-artist").await;
    let (teardrop, _, linked) = catalog_ids(&rig, "Massive Attack/Mezzanine/04 Teardrop.m4a");
    let music = rig.app.config.root_app_dir.join("music");
    let live = music.join("MA/Live/01.flac");
    std::fs::create_dir_all(live.parent().expect("folder")).expect("folder made");
    std::fs::copy(music.join("Radiohead/OK Computer/01 Airbag.flac"), &live).expect("copied");
    let one = |value: &str| vec![value.to_owned()];
    save_tags(
        &live,
        &[
            TagEdit::verbatim(TagField::Album, one("Live")),
            TagEdit::verbatim(TagField::Artist, one("MA")),
            TagEdit::verbatim(TagField::Artists, one("MA")),
            TagEdit::verbatim(TagField::AlbumArtist, one("MA")),
            TagEdit::verbatim(TagField::AlbumArtists, one("MA")),
            TagEdit::verbatim(TagField::MusicBrainzArtistId, one(ARTIST)),
            TagEdit::verbatim(TagField::MusicBrainzAlbumArtistId, one(ARTIST)),
            TagEdit::verbatim(TagField::MusicBrainzReleaseId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzReleaseGroupId, Vec::new()),
            TagEdit::verbatim(TagField::MusicBrainzRecordingId, Vec::new()),
        ],
    )
    .expect("copy tagged");
    rig.app.scan_library().await;
    let (live_track, _, merged) = catalog_ids(&rig, "MA/Live/01.flac");
    assert_ne!(merged, linked);
    // Jellyfin reads its ids once, so only after the new file is in.
    let saved = jellyfin_artist_id(&rig, &live_track).await;
    let survivor_id = jellyfin_artist_id(&rig, &teardrop).await;

    rig.app
        .db()
        .execute(
            "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
             provider_artist_id, decision_source, selected_at) \
             VALUES (?1, 'musicbrainz', ?2, 'manual', 1)",
            [linked.as_str(), ARTIST],
        )
        .expect("link seeded");
    rig.app.scan_library().await;

    let artist = subsonic_ok(&rig, &format!("getArtist?id=ar-{merged}")).await;
    assert_eq!(artist["artist"]["id"], json!(format!("ar-{linked}")));
    assert_eq!(jellyfin_item(&rig, &saved).await.0, json!(survivor_id));
    let (status, _) = rig
        .app
        .call("GET", &format!("/jellyfin/Items/{saved}"), &[], None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    rig.stop().await;
}

/// A track whose file was deleted while a copy of it stays, both mapped
/// to the same release track of the album's edition, continues into the
/// copy, and the id a player app saved for it opens the copy. Subsonic:
/// `getSong` answers the copy with its id, `stream` serves the copy's
/// bytes (a range too), starring the old id stars the copy and a scrobble files under
/// it. Jellyfin, signed in: the old id answers with the copy's id, also as
/// the first request of a new process, and is a 401 with no token. The
/// web reads of the track and its stream, GET and HEAD, follow it too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_continued_track_opens_by_its_old_id() {
    let mut rig = CompatApp::start("compat-continued-track").await;
    let (old, album, _) = catalog_ids(&rig, "U2/Achtung Baby/03 One.mp3");
    let music = rig.app.config.root_app_dir.join("music");
    std::fs::copy(
        music.join("U2/Achtung Baby/03 One.mp3"),
        music.join("U2/Achtung Baby/03 One (copy).mp3"),
    )
    .expect("copied");
    rig.app.scan_library().await;
    let (copy, copy_album, _) = catalog_ids(&rig, "U2/Achtung Baby/03 One (copy).mp3");
    assert_eq!(copy_album, album);
    let release = "33333333-3333-4333-8333-333333333333";
    rig.app
        .db()
        .execute_batch(&format!(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_at) \
             VALUES ('{album}', 'musicbrainz', 'rg-3', '{release}', 'manual', 1) \
             ON CONFLICT (local_album_id, provider) DO UPDATE SET \
             release_mbid = excluded.release_mbid, decision_source = 'manual'; \
             DELETE FROM library_album_match_state WHERE local_album_id = '{album}'; \
             INSERT INTO local_track_external_identities (local_track_id, provider, \
             recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at) \
             VALUES ('{old}', 'musicbrainz', 'rec-one', '{release}', 'rt-one', 'manual', 1), \
             ('{copy}', 'musicbrainz', 'rec-one', '{release}', 'rt-one', 'manual', 1) \
             ON CONFLICT (local_track_id, provider) DO UPDATE SET \
             release_mbid = excluded.release_mbid, \
             release_track_mbid = excluded.release_track_mbid;"
        ))
        .expect("both copies mapped");
    let bytes = rig.file("one");
    std::fs::remove_file(music.join("U2/Achtung Baby/03 One.mp3")).expect("original deleted");
    rig.app.scan_library().await;

    let song = subsonic_ok(&rig, &format!("getSong?id=tr-{old}")).await;
    assert_eq!(song["song"]["id"], json!(format!("tr-{copy}")), "{song}");
    let auth = rig.subsonic_auth("merged");
    let (status, headers, body) = rig
        .app
        .raw(
            "GET",
            &rest(&format!("stream?id=tr-{old}"), &auth),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, "content-length"),
        bytes.len().to_string()
    );
    assert_eq!(body, bytes);
    let (status, _, body) = rig
        .app
        .raw(
            "GET",
            &rest(&format!("stream?id=tr-{old}"), &auth),
            &[("Range", "bytes=0-49")],
            None,
        )
        .await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &bytes[..50])
    );
    subsonic_ok(&rig, &format!("star?id=tr-{old}")).await;
    let starred = subsonic_ok(&rig, "getStarred2").await;
    assert_eq!(
        starred["starred2"]["song"][0]["id"],
        json!(format!("tr-{copy}")),
        "{starred}"
    );
    subsonic_ok(&rig, &format!("scrobble?id=tr-{old}&submission=true")).await;
    let filed: (i64, i64) = rig
        .app
        .db()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM library_play_history WHERE local_track_id = ?1), \
             (SELECT COUNT(*) FROM library_play_history WHERE local_track_id = ?2)",
            [&copy, &old],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("plays read");
    assert_eq!(filed, (1, 0));

    let admin = rig.app.login_admin().await;
    let admin = [("authorization", admin.as_str())];
    let (status, track) = rig
        .app
        .call(
            "GET",
            &format!("/api/v3/library/tracks/{old}"),
            &admin,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{track}");
    assert_eq!(track["id"], json!(copy), "{track}");
    let (status, _, body) = rig
        .app
        .raw("GET", &format!("/api/v3/stream/local/{old}"), &admin, None)
        .await;
    assert_eq!((status, body), (StatusCode::OK, bytes.clone()));
    let (status, headers, body) = rig
        .app
        .raw("HEAD", &format!("/api/v3/stream/local/{old}"), &admin, None)
        .await;
    assert_eq!((status, body.len()), (StatusCode::OK, 0));
    assert_eq!(
        header_str(&headers, "content-length"),
        bytes.len().to_string()
    );

    let (old_id, copy_id) = (unkeyed_id("track", &old), unkeyed_id("track", &copy));
    assert_eq!(jellyfin_item(&rig, &old_id).await.0, json!(copy_id));
    rig.app = rig.app.restart().await;
    assert_eq!(
        jellyfin_item(&rig, &old_id).await.0,
        json!(copy_id),
        "after a restart"
    );
    let (status, _) = rig
        .app
        .call("GET", &format!("/jellyfin/Items/{old_id}"), &[], None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
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

/// One Subsonic call as `auth`, answered 200: its `subsonic-response`.
async fn subsonic(rig: &CompatApp, call: &str, auth: &str) -> Value {
    let (status, body) = rig.app.call("GET", &rest(call, auth), &[], None).await;
    assert_eq!(status, StatusCode::OK, "{call}: {body}");
    body["subsonic-response"].clone()
}

/// `(userRating, averageRating)` of a song or album, null where absent.
async fn ratings(rig: &CompatApp, call: &str, key: &str, auth: &str) -> (Value, Value) {
    let item = &subsonic(rig, call, auth).await[key];
    (item["userRating"].clone(), item["averageRating"].clone())
}

/// A rating set in a Subsonic app is the user's own: another user sees
/// their own, and an average that counts both. It survives a restart, 0
/// clears it, and a merged album's ratings move to the album it went
/// into, where a user who rated both keeps the survivor's. A rating on an
/// unknown id is 70, and nothing is stored without sign-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subsonic_ratings_are_per_user_and_follow_a_merge() {
    let mut rig = CompatApp::start("compat-ratings").await;
    let mine = rig.subsonic_auth("ratings");
    let login = rig.app.login_user("listener").await;
    let theirs = format!(
        "u=listener&p={}&v=1.16.1&c=ratings&f=json",
        rig.app_password(&login.authorization).await
    );
    let all = subsonic(&rig, "search3?query=&songCount=500", &mine).await;
    let song = |title: &str, key: &str| -> String {
        all["searchResult3"]["song"]
            .as_array()
            .expect("songs")
            .iter()
            .find(|song| song["title"] == title)
            .expect("the song")[key]
            .as_str()
            .expect("an id")
            .to_owned()
    };
    let (track, kept, merged) = (
        song("Airbag", "id"),
        song("Airbag", "albumId"),
        song("Teardrop", "albumId"),
    );
    let teardrop = song("Teardrop", "id");
    let rate = |id: &str, rating: u8| format!("setRating?id={id}&rating={rating}");
    let ok = |answer: &Value| assert_eq!(answer["status"], "ok", "{answer}");
    let get_song = format!("getSong?id={track}");

    ok(&subsonic(&rig, &rate(&track, 4), &mine).await);
    ok(&subsonic(&rig, &rate(&track, 2), &theirs).await);
    assert_eq!(
        ratings(&rig, &get_song, "song", &mine).await,
        (json!(4), json!(3))
    );
    assert_eq!(
        ratings(&rig, &get_song, "song", &theirs).await,
        (json!(2), json!(3))
    );
    ok(&subsonic(&rig, &rate(&track, 0), &theirs).await);
    assert_eq!(
        ratings(&rig, &get_song, "song", &mine).await,
        (json!(4), json!(4))
    );
    assert_eq!(
        ratings(&rig, &get_song, "song", &theirs).await,
        (Value::Null, json!(4))
    );

    rig.app = rig.app.restart().await;
    assert_eq!(
        ratings(&rig, &get_song, "song", &mine).await,
        (json!(4), json!(4)),
        "after a restart"
    );

    let refused = subsonic(&rig, &rate("tr-no-such-track", 3), &mine).await;
    assert_eq!(refused["error"]["code"], 70, "{refused}");
    let (status, anonymous) = rig
        .app
        .call(
            "GET",
            &format!(
                "/subsonic/rest/{}&v=1.16.1&c=ratings&f=json",
                rate(&kept, 1)
            ),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{anonymous}");
    assert_eq!(
        anonymous["subsonic-response"]["error"]["code"], 10,
        "{anonymous}"
    );

    ok(&subsonic(&rig, &rate(&merged, 2), &mine).await);
    ok(&subsonic(&rig, &rate(&merged, 3), &theirs).await);
    ok(&subsonic(&rig, &rate(&kept, 5), &theirs).await);
    let local = |id: &str| id[3..].to_owned();
    let admin = rig.app.login_admin().await;
    let headers = [("authorization", admin.as_str())];
    let selection = json!({"track_ids": [local(&teardrop)], "target_album_id": local(&kept)});
    let (status, preview) = rig
        .app
        .call(
            "POST",
            "/api/v3/library/albums/merge-preview",
            &headers,
            Some(selection.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let mut apply = selection;
    apply["preview_token"] = preview["preview_token"].clone();
    let (status, body) = rig
        .app
        .call(
            "POST",
            "/api/v3/library/albums/merge",
            &headers,
            Some(apply),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let get_album = format!("getAlbum?id={kept}");
    assert_eq!(
        ratings(&rig, &get_album, "album", &mine).await,
        (json!(2), json!(3.5))
    );
    assert_eq!(
        ratings(&rig, &get_album, "album", &theirs).await,
        (json!(5), json!(3.5))
    );
    let highest = subsonic(&rig, "getAlbumList2?type=highest", &mine).await;
    assert_eq!(highest["albumList2"]["album"][0]["id"], json!(kept));
    let left: i64 = rig
        .app
        .db()
        .query_row(
            "SELECT COUNT(*) FROM library_user_ratings WHERE item_id = ?1",
            [local(&merged)],
            |row| row.get(0),
        )
        .expect("ratings read");
    assert_eq!(left, 0);
    rig.stop().await;
}

/// LRCLIB as the lyrics lookup sees it, counting every lookup: synced
/// lyrics for "Paranoid Android", nothing for anything else.
async fn counted_lyrics_server() -> (
    String,
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use axum::extract::Query;
    use axum::response::IntoResponse;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = asked.clone();
    let get = move |Query(query): Query<HashMap<String, String>>| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            if query.get("track_name").map(String::as_str) != Some("Paranoid Android") {
                return StatusCode::NOT_FOUND.into_response();
            }
            axum::Json(json!({
                "id": 7,
                "trackName": "Paranoid Android",
                "artistName": "Radiohead",
                "albumName": "OK Computer",
                "duration": 200,
                "instrumental": false,
                "syncedLyrics": "[00:01.00]Copper rain on a tin roof\n[00:02.50]Static in the hall",
            }))
            .into_response()
        }
    };
    let router = axum::Router::new().route("/lrclib/api/get", axum::routing::get(get));
    let (base, server) = droppedneedle_testkit::usenet::serve_loopback(router)
        .await
        .expect("lyrics server starts");
    (base, server, asked)
}

/// Lyrics a file carries come first, and only those tracks are marked.
/// With a `.lrc` beside Airbag, Jellyfin lists Airbag with lyrics and a
/// `Lyric` stream and Paranoid Android with neither; Airbag's lyrics come
/// from its file on both protocols, with no online lookup, and listing
/// never looks anything up. Paranoid Android's come from LRCLIB while the
/// lookup is on, after exactly one lookup. With it off, Airbag still reads
/// from its file and Paranoid Android gets Jellyfin's 404, with no lookup.
/// The address needs sign-in like the stream.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lyrics_come_from_the_file_first() {
    use std::sync::atomic::Ordering;

    let (lrclib, server, asked) = counted_lyrics_server().await;
    let rig = CompatApp::start_as_recorded(
        "compat-lyrics",
        crate::common::outbound_at(&lrclib),
        "jellyfin-10.10.6-lyrics",
    )
    .await;
    // The lookup skips a track shorter than a second, as every fixture is.
    rig.app
        .db()
        .execute(
            "UPDATE local_tracks SET duration_seconds = 200 \
             WHERE relative_path = 'Radiohead/OK Computer/02 Paranoid Android.flac'",
            [],
        )
        .expect("duration seeds");
    rig.set_lyrics_lookup(true).await;
    rig.wait_for_lyrics_mark("Airbag").await;

    let token = rig.jellyfin_auth();
    let auth = [("X-Emby-Authorization", token.as_str())];
    let app = &rig.app;
    let get = move |uri: String| async move { app.call("GET", &uri, &auth, None).await };
    let (_, tracks) = get("/jellyfin/Items?Recursive=true&IncludeItemTypes=Audio".to_owned()).await;
    let airbag = named(&tracks["Items"], "Name", "Airbag");
    let album = airbag["AlbumId"].as_str().expect("album id").to_owned();
    let airbag = airbag["Id"].as_str().expect("track id").to_owned();
    let paranoid = named(&tracks["Items"], "Name", "Paranoid Android")["Id"]
        .as_str()
        .expect("track id")
        .to_owned();

    let (status, listed) = get(format!("/jellyfin/Items?ParentId={album}")).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(
        named(&listed["Items"], "Name", "Airbag")["HasLyrics"],
        json!(true)
    );
    assert_eq!(
        named(&listed["Items"], "Name", "Paranoid Android")["HasLyrics"],
        json!(false)
    );
    for (id, streams) in [
        (&airbag, json!(["Audio", "Lyric"])),
        (&paranoid, json!(["Audio"])),
    ] {
        let (status, info) = rig
            .app
            .call(
                "POST",
                &format!("/jellyfin/Items/{id}/PlaybackInfo"),
                &auth,
                Some(json!({"UserId": rig.user_id, "DeviceProfile": {}})),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{info}");
        let types: Vec<Value> = info["MediaSources"][0]["MediaStreams"]
            .as_array()
            .expect("streams")
            .iter()
            .map(|stream| stream["Type"].clone())
            .collect();
        assert_eq!(json!(types), streams, "{id}");
    }

    let from_file = json!({
        "Metadata": {"IsSynced": true},
        "Lyrics": [
            {"Text": "Paper lanterns over the quiet field", "Start": 0},
            {"Text": "Velvet engines hum a borrowed tune", "Start": 4_000_000},
            {"Text": "Every window counts the passing moon", "Start": 12_000_000},
        ],
    });
    let (status, lyrics) = get(format!("/jellyfin/Audio/{airbag}/Lyrics")).await;
    assert_eq!((status, &lyrics), (StatusCode::OK, &from_file));
    let player = rig.subsonic_auth("lyrics");
    let found = subsonic(&rig, "search3?query=Airbag&songCount=1", &player).await;
    let song = found["searchResult3"]["song"][0]["id"]
        .as_str()
        .expect("song id")
        .to_owned();
    let answer = subsonic(&rig, &format!("getLyricsBySongId?id={song}"), &player).await;
    let structured = &answer["lyricsList"]["structuredLyrics"];
    assert_eq!(structured.as_array().map(Vec::len), Some(1), "{structured}");
    assert_eq!(structured[0]["synced"], json!(true));
    assert_eq!(
        structured[0]["line"][1],
        json!({"value": "Velvet engines hum a borrowed tune", "start": 400})
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "file lyrics and lists never look up"
    );

    let (status, lyrics) = get(format!("/jellyfin/Audio/{paranoid}/Lyrics")).await;
    assert_eq!(status, StatusCode::OK, "{lyrics}");
    assert_eq!(
        lyrics["Lyrics"],
        json!([
            {"Text": "Copper rain on a tin roof", "Start": 10_000_000},
            {"Text": "Static in the hall", "Start": 25_000_000},
        ])
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    rig.set_lyrics_lookup(false).await;
    let (status, lyrics) = get(format!("/jellyfin/Audio/{airbag}/Lyrics")).await;
    assert_eq!((status, &lyrics), (StatusCode::OK, &from_file));
    let (status, missing) = get(format!("/jellyfin/Audio/{paranoid}/Lyrics")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_eq!(
        (&missing["title"], &missing["status"]),
        (&json!("Not Found"), &json!(404))
    );
    assert_eq!(asked.load(Ordering::SeqCst), 1, "no lookup while it is off");

    let (status, _) = rig
        .app
        .call(
            "GET",
            &format!("/jellyfin/Audio/{airbag}/Lyrics"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    rig.stop().await;
    server.abort();
}
