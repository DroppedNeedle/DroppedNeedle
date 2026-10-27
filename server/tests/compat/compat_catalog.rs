//! Compat over the real catalog: Subsonic and Jellyfin read the scanned
//! library from SQLite, and playlists and favorites are one set of rows
//! shared with the native routes and the Navidrome m3u export.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_catalog::reads::ReadsSetup;
use droppedneedle_catalog::reads::collections::CollectionsState;
use droppedneedle_catalog::reads::collections::db::CollectionsDb;
use droppedneedle_catalog::reads::collections::models::{
    AddTracksBody, CreatePlaylistBody, TrackInput,
};
use droppedneedle_catalog::reads::collections::service::CollectionsService;
use droppedneedle_compat::adapters::audio::GatewayAudio;
use droppedneedle_compat::adapters::jellyfin_ids::JellyfinIds;
use droppedneedle_compat::adapters::jellyfin_library::{CatalogIds, JellyfinLibrary};
use droppedneedle_compat::adapters::library::CompatLibrary;
use droppedneedle_compat::adapters::playback::CompatPlayback;
use droppedneedle_compat::jellyfin::builders::LIBRARY_INTERNAL_ID;
use droppedneedle_compat::jellyfin::{JellyfinState, routes};
use droppedneedle_compat::settings::LiveSettings;
use droppedneedle_compat::shared::address::ListenerConfig;
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::db::{DbRuntime, Lane};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::Crypto;
use droppedneedle_kernel::runtime_config::sections::ConnectApps;
use droppedneedle_library::wiring::LibrarySetup;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::ScratchDir;
use crate::common::auth::AuthRig;
use crate::common::compat::{CompatApp, rest};

/// One album by Portishead with one track, as a scan writes them.
pub(crate) const LIBRARY: &str = "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
    VALUES ('art-1', 'Portishead', 'portishead', 'group', 1, 1); \
    INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_name, \
    album_artist_name_folded, album_artist_id, year, grouping_source, created_at, updated_at) \
    VALUES ('alb-1', 'root', 'k1', 'Dummy', 'dummy', 'Portishead', 'portishead', 'art-1', 1994, \
    'automatic', 1, 1); \
    INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
    file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, artist_name, \
    artist_name_folded, album_title, album_title_folded, album_artist_name, \
    album_artist_name_folded, track_number, genre, genre_folded, duration_seconds, \
    file_format, ingest_source, imported_at, membership_source) \
    VALUES ('trk-1', 'alb-1', 'root', '/music/dummy/01.flac', 'dummy/01.flac', 'h1', 1000, 1, \
    'r1', 'Mysterons', 'mysterons', 'Portishead', 'portishead', 'Dummy', 'dummy', 'Portishead', \
    'portishead', 1, 'Trip Hop', 'trip hop', 305.0, 'flac', 'scan', 1, 'automatic'); \
    INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
    VALUES ('trk-1', 0, 'art-1', 'main');";

/// The whole app with both protocols on, over the seeded catalog.
async fn app(tag: &str) -> CompatApp {
    let rig = CompatApp::unscanned(tag).await;
    seed(&rig, LIBRARY);
    rig
}

fn seed(rig: &CompatApp, sql: &str) {
    rig.app.db().execute_batch(sql).expect("catalog seeds");
}

/// The JSON answer of a Subsonic app signed in as the player.
async fn subsonic(rig: &CompatApp, endpoint: &str, pairs: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://test.local/").expect("base parses");
    url.query_pairs_mut().extend_pairs(pairs);
    let call = format!("{endpoint}?{}", url.query().unwrap_or(""));
    let uri = rest(&call, &rig.subsonic_auth("catalog"));
    let (status, _, body) =
        crate::common::through_busy(|| rig.app.raw("GET", &uri, &[], None)).await;
    assert_eq!(status, StatusCode::OK, "{endpoint}");
    String::from_utf8(body).expect("utf-8 answer")
}

/// A Jellyfin `Items` page for the player: the item names and the total.
async fn jellyfin_items(rig: &CompatApp, query: &str) -> (Vec<Value>, u64) {
    let token = rig.jellyfin_auth();
    let (status, body) = rig
        .app
        .call_through_busy(
            "GET",
            &format!("/jellyfin/Items?Recursive=true&{query}"),
            &[("X-Emby-Authorization", token.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["Items"].as_array().expect("items").clone(),
        body["TotalRecordCount"].as_u64().expect("total"),
    )
}

/// The `authorization` header value of a new account called `name`.
async fn native(rig: &CompatApp, name: &str) -> String {
    rig.app.login_user(name).await.authorization
}

/// The player's `authorization` header value.
async fn player(rig: &CompatApp) -> String {
    let (status, body) = rig
        .app
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(json!({
                "username": crate::common::compat::PLAYER,
                "password": format!("{}-password-1234", crate::common::compat::PLAYER),
                "transport": "bearer",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    format!("Bearer {}", body["token"].as_str().expect("session token"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subsonic_and_jellyfin_read_the_scanned_library() {
    let rig = app("compat-catalog-read").await;

    let album = subsonic(&rig, "getAlbum", &[("id", "al-alb-1")]).await;
    assert!(album.contains("\"title\":\"Mysterons\""), "{album}");
    assert!(album.contains("\"id\":\"tr-trk-1\""), "{album}");
    let artists = subsonic(&rig, "getArtists", &[]).await;
    assert!(artists.contains("Portishead"), "{artists}");
    let found = subsonic(&rig, "search3", &[("query", "myster")]).await;
    assert!(found.contains("tr-trk-1"), "{found}");
    let genre = subsonic(&rig, "getSongsByGenre", &[("genre", "Trip Hop")]).await;
    assert!(genre.contains("tr-trk-1"), "{genre}");

    let (tracks, total) =
        jellyfin_items(&rig, "IncludeItemTypes=Audio&SearchTerm=myster&Limit=10").await;
    assert_eq!((total, tracks[0]["Name"].as_str()), (1, Some("Mysterons")));
    let (albums, total) = jellyfin_items(
        &rig,
        "IncludeItemTypes=MusicAlbum&SortBy=DateCreated&SortOrder=Descending&Limit=1",
    )
    .await;
    assert_eq!((total, albums[0]["Name"].as_str()), (1, Some("Dummy")));
    let (played, total) = jellyfin_items(
        &rig,
        "IncludeItemTypes=Audio&SortBy=PlayCount&SortOrder=Descending&Limit=10",
    )
    .await;
    assert_eq!((played.len(), total), (1, 1), "history sorts keep unplayed");
    rig.stop().await;
}

/// The seeded album's folder picture: a square PNG of one colour.
fn paint_cover(path: &std::path::Path, side: u32, colour: [u8; 3]) {
    image::RgbImage::from_pixel(side, side, image::Rgb(colour))
        .save(path)
        .expect("cover writes");
}

/// The colour of the top left pixel of a served picture.
fn colour_of(body: &[u8]) -> [u8; 3] {
    image::load_from_memory(body)
        .expect("a picture")
        .to_rgb8()
        .get_pixel(0, 0)
        .0
}

/// A Subsonic JSON answer's payload under `key`.
async fn subsonic_json(rig: &CompatApp, endpoint: &str, id: &str, key: &str) -> Value {
    let body = subsonic(rig, endpoint, &[("id", id)]).await;
    let parsed: Value = serde_json::from_str(&body).expect("json answer");
    parsed["subsonic-response"][key].clone()
}

/// A GET with no credentials: the cache rule and the picture's colour.
async fn open(rig: &CompatApp, uri: &str) -> (String, [u8; 3]) {
    let (status, headers, body) =
        crate::common::through_busy(|| rig.app.raw("GET", uri, &[], None)).await;
    assert_eq!(status, StatusCode::OK, "{uri}");
    let cache = headers
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    (cache, colour_of(&body))
}

/// Run `sql` as one catalog write through the app's writer, bumping the
/// catalog revision as a scan does, so the loops that follow the catalog
/// see it.
async fn write(rig: &CompatApp, sql: String) {
    rig.app
        .state()
        .library
        .db
        .write(Lane::Foreground, "seed", move |tx| {
            tx.execute_batch(&sql)?;
            tx.execute_batch(
                "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 1) \
                 ON CONFLICT (singleton) DO UPDATE SET value = value + 1;",
            )?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("catalog writes");
}

/// The seeded album's Jellyfin album item and its track item.
async fn jellyfin_album_and_track(rig: &CompatApp) -> (Value, Value) {
    let (albums, _) = jellyfin_items(rig, "IncludeItemTypes=MusicAlbum&Limit=10").await;
    let (tracks, _) = jellyfin_items(rig, "IncludeItemTypes=Audio&Limit=10").await;
    (albums[0].clone(), tracks[0].clone())
}

/// When a scan finds new art for an album, both protocols name its cover
/// anew, so apps fetch the new picture instead of keeping the old one for
/// a year. The album's own ids never change, and every cover id and link
/// an app stored before still opens the album's current picture.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_cover_gets_a_new_cover_id_and_old_ids_still_open() {
    let rig = CompatApp::unscanned("compat-catalog-cover-version").await;
    let music = rig.app.config.root_app_dir.join("music");
    let folder = music.join("dummy");
    std::fs::create_dir_all(&folder).expect("album folder");
    let cover = folder.join("cover.png");
    paint_cover(&cover, 2, [200, 10, 10]);
    let cover_path = cover.to_str().expect("utf-8 path").to_owned();
    let track_path = folder.join("01.flac");
    write(
        &rig,
        format!(
            "{LIBRARY} UPDATE local_tracks SET file_path = '{}' WHERE id = 'trk-1'; \
             INSERT INTO local_album_artwork (local_album_id, source, source_locator, \
             content_hash, version, updated_at) VALUES ('alb-1', 'folder', '{cover_path}', \
             'red', 1, 1); \
             INSERT INTO local_album_aliases (alias, local_album_id, kind, created_at) \
             VALUES ('my_album', 'alb-1', 'compat_migration', 1);",
            track_path.to_str().expect("utf-8 path")
        ),
    )
    .await;
    rig.jellyfin_ids_caught_up().await;

    let album = subsonic_json(&rig, "getAlbum", "al-alb-1", "album").await;
    let old_cover = album["coverArt"].as_str().expect("cover id").to_owned();
    let info = subsonic_json(&rig, "getAlbumInfo2", "al-alb-1", "albumInfo").await;
    let old_link = info["mediumImageUrl"].as_str().expect("link").to_owned();
    let (jf_album, jf_track) = jellyfin_album_and_track(&rig).await;
    let old_tag = jf_album["ImageTags"]["Primary"].clone();
    let old_track_tag = jf_track["AlbumPrimaryImageTag"].clone();
    assert!(
        old_tag.is_string() && old_track_tag.is_string(),
        "{jf_album}"
    );

    // A rescan finds a different picture in the folder and bumps the row.
    paint_cover(&cover, 3, [10, 10, 200]);
    write(
        &rig,
        "UPDATE local_album_artwork SET version = 2, content_hash = 'blue' \
         WHERE local_album_id = 'alb-1';"
            .to_owned(),
    )
    .await;
    let blue = [10, 10, 200];

    let album = subsonic_json(&rig, "getAlbum", "al-alb-1", "album").await;
    let new_cover = album["coverArt"].as_str().expect("cover id").to_owned();
    assert_ne!(new_cover, old_cover, "a new cover id");
    assert!(new_cover.starts_with("al-alb-1_"), "{new_cover}");
    assert_eq!(album["id"], "al-alb-1", "the album id stays");
    let song = &album["song"][0];
    assert_eq!(song["coverArt"], new_cover.as_str(), "{song}");
    assert_eq!(
        (&song["parent"], &song["albumId"]),
        (&json!("al-alb-1"), &json!("al-alb-1"))
    );
    let directory = subsonic_json(&rig, "getMusicDirectory", "al-alb-1", "directory").await;
    assert_eq!(directory["coverArt"], new_cover.as_str(), "{directory}");
    // An id v2 handed out for the album, free text with an underscore,
    // still names it.
    let legacy = subsonic_json(&rig, "getAlbum", "al-my_album", "album").await;
    assert_eq!(legacy["id"], "al-alb-1", "{legacy}");
    let auth = rig.subsonic_auth("catalog");
    for id in [
        &new_cover,
        &old_cover,
        &"al-alb-1".to_owned(),
        &"al-my_album".to_owned(),
    ] {
        let uri = rest(&format!("getCoverArt?id={id}"), &auth);
        let (cache, colour) = open(&rig, &uri).await;
        assert_eq!(colour, blue, "{id} opens the new picture");
        assert_eq!(cache, "public, max-age=31536000, immutable", "{id}");
    }

    let info = subsonic_json(&rig, "getAlbumInfo2", "al-alb-1", "albumInfo").await;
    let new_link = info["mediumImageUrl"].as_str().expect("link").to_owned();
    assert_ne!(new_link, old_link, "the link changes with the cover");
    let version = new_cover.rsplit_once('_').expect("a version").1;
    assert!(new_link.ends_with(&format!("&cv={version}")), "{new_link}");
    let path = |link: &str| link[link.find("/subsonic/img").expect("picture link")..].to_owned();
    let unversioned = path(&new_link[..new_link.find("&cv=").expect("version")]);
    for link in [path(&new_link), path(&old_link), unversioned] {
        let (cache, colour) = open(&rig, &link).await;
        assert_eq!(colour, blue, "{link} opens the new picture");
        assert_eq!(cache, "public, max-age=86400", "{link}");
    }

    let (jf_album, jf_track) = jellyfin_album_and_track(&rig).await;
    let new_tag = jf_album["ImageTags"]["Primary"]
        .as_str()
        .expect("album tag")
        .to_owned();
    assert_ne!(json!(new_tag), old_tag, "the album's tag changes");
    assert_ne!(jf_track["AlbumPrimaryImageTag"], old_track_tag);
    assert_eq!(jf_track["AlbumPrimaryImageTag"], new_tag.as_str());
    let item = jf_album["Id"].as_str().expect("album item id");
    let untagged = format!("/jellyfin/Items/{item}/Images/Primary?format=jpg&quality=90");
    let (cache, colour) = open(&rig, &untagged).await;
    assert_eq!((colour, cache.as_str()), (blue, "public, max-age=86400"));
    let tagged = format!("/jellyfin/Items/{item}/Images/Primary?tag={new_tag}");
    let (cache, colour) = open(&rig, &tagged).await;
    assert_eq!(
        (colour, cache.as_str()),
        (blue, "public, max-age=31536000, immutable")
    );
    rig.stop().await;
}

/// One more streamable track on the seeded album, by Portishead.
fn track_row(id: &str, title: &str, number: i64) -> String {
    track_by(id, title, "Portishead", number)
}

/// One more streamable track on the seeded album, by `artist`.
fn track_by(id: &str, title: &str, artist: &str, number: i64) -> String {
    let folded = title.to_lowercase();
    let artist_folded = artist.to_lowercase();
    format!(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         artist_name, artist_name_folded, album_title, album_title_folded, album_artist_name, \
         album_artist_name_folded, track_number, duration_seconds, file_format, ingest_source, \
         imported_at, membership_source) \
         VALUES ('{id}', 'alb-1', 'root', '/music/dummy/{id}.flac', 'dummy/{id}.flac', 'h-{id}', \
         1000, 1, 'r-{id}', '{title}', '{folded}', '{artist}', '{artist_folded}', 'Dummy', \
         'dummy', 'Portishead', 'portishead', {number}, 200.0, 'flac', 'scan', 1, 'automatic');"
    )
}

/// Words under 3 characters still narrow a search that has a longer word:
/// "the xx" finds The xx, not every track with "the" in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn short_words_narrow_a_search() {
    let rig = app("compat-catalog-short").await;
    seed(
        &rig,
        &[
            track_by("trk-a", "The Rip", "Portishead", 2),
            track_by("trk-b", "Over The Hill", "Portishead", 3),
            track_by("trk-c", "Intro", "The xx", 4),
        ]
        .concat(),
    );
    let found = subsonic(
        &rig,
        "search3",
        &[
            ("query", "the xx"),
            ("songCount", "1"),
            ("artistCount", "0"),
            ("albumCount", "0"),
        ],
    )
    .await;
    assert!(found.contains("tr-trk-c"), "{found}");
    // A query with no word of 3+ characters keeps the substring match.
    let short = subsonic(
        &rig,
        "search3",
        &[("query", "xx"), ("artistCount", "0"), ("albumCount", "0")],
    )
    .await;
    assert!(short.contains("tr-trk-c"), "{short}");
    assert!(!short.contains("tr-trk-a"), "{short}");
    rig.stop().await;
}

/// The search3 empty-query sync (Symfonium, Tempo) pages in insertion
/// order, so pages already read stay put while a scan adds tracks; words
/// match across artist and title; random pages share one shuffle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search3_sync_pages_are_stable_and_words_match_across_columns() {
    let rig = app("compat-catalog-pages").await;
    seed(
        &rig,
        &[
            track_row("trk-2", "Roads", 2),
            track_row("trk-3", "Glory Box", 3),
        ]
        .concat(),
    );
    async fn page(rig: &CompatApp, offset: &str) -> String {
        let body = subsonic(
            rig,
            "search3",
            &[
                ("query", "\"\""),
                ("songCount", "1"),
                ("songOffset", offset),
                ("artistCount", "0"),
                ("albumCount", "0"),
            ],
        )
        .await;
        let value: Value = serde_json::from_str(&body).expect("json answer");
        value["subsonic-response"]["searchResult3"]["song"][0]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    let before = [
        page(&rig, "0").await,
        page(&rig, "1").await,
        page(&rig, "2").await,
    ];
    assert_eq!(before, ["tr-trk-1", "tr-trk-2", "tr-trk-3"]);
    seed(&rig, &track_row("trk-0", "Sour Times", 4));
    let after = [
        page(&rig, "0").await,
        page(&rig, "1").await,
        page(&rig, "2").await,
    ];
    assert_eq!(after, before, "a new track lands after the pages read");
    assert_eq!(page(&rig, "3").await, "tr-trk-0");

    let found = subsonic(&rig, "search3", &[("query", "portishead roads")]).await;
    assert!(found.contains("tr-trk-2"), "{found}");
    assert!(!found.contains("tr-trk-3"), "{found}");
    let dashed = subsonic(&rig, "search3", &[("query", "Portishead - Roads")]).await;
    assert!(dashed.contains("tr-trk-2"), "{dashed}");

    let (all, _) = jellyfin_items(&rig, "IncludeItemTypes=Audio&Limit=10").await;
    let mut every: Vec<&str> = all.iter().filter_map(|item| item["Id"].as_str()).collect();
    every.sort();
    let mut shuffled = Vec::new();
    for start in 0..4 {
        let (tracks, total) = jellyfin_items(
            &rig,
            &format!("IncludeItemTypes=Audio&SortBy=Random&StartIndex={start}&Limit=1"),
        )
        .await;
        assert_eq!(total, 4);
        shuffled.extend(
            tracks
                .iter()
                .filter_map(|track| track["Id"].as_str().map(str::to_owned)),
        );
    }
    shuffled.sort();
    assert_eq!(shuffled, every, "no repeats");
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playlists_and_stars_are_shared_with_the_native_routes() {
    let rig = app("compat-catalog-shared").await;
    let web = player(&rig).await;
    let web = [("authorization", web.as_str())];

    // A playlist made in a Subsonic client shows in the web UI.
    let created = subsonic(
        &rig,
        "createPlaylist",
        &[("name", "From Symfonium"), ("songId", "tr-trk-1")],
    )
    .await;
    assert!(created.contains("tr-trk-1"), "{created}");
    let (status, listed) = rig.app.call("GET", "/api/v3/playlists", &web, None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["playlists"][0]["name"], "From Symfonium");
    assert_eq!(listed["playlists"][0]["track_count"], 1);

    // A playlist made in the web UI with a local track streams in Subsonic.
    let (status, made) = rig
        .app
        .call(
            "POST",
            "/api/v3/playlists",
            &web,
            Some(json!({"name": "From the web"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let made = made["id"].as_str().expect("playlist id").to_owned();
    let (status, added) = rig
        .app
        .call(
            "POST",
            &format!("/api/v3/playlists/{made}/tracks"),
            &web,
            Some(json!({"tracks": [{
                "track_name": "Mysterons",
                "artist_name": "Portishead",
                "album_name": "Dummy",
                "track_source_id": "trk-1",
                "source_type": "local",
            }]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    let listed = subsonic(&rig, "getPlaylist", &[("id", &format!("pl-{made}"))]).await;
    assert!(listed.contains("\"songCount\":1"), "{listed}");

    // A star from a client is a heart in the web UI.
    subsonic(&rig, "star", &[("id", "al-alb-1")]).await;
    let (status, hearts) = rig
        .app
        .call("GET", "/api/v3/favorites?kind=album", &web, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{hearts}");
    assert_eq!(hearts["items"][0]["item_id"], "alb-1");
    let starred = subsonic(&rig, "getStarred2", &[]).await;
    assert!(starred.contains("al-alb-1"), "{starred}");

    // Queues persist in the database.
    subsonic(
        &rig,
        "savePlayQueue",
        &[("id", "tr-trk-1"), ("current", "tr-trk-1")],
    )
    .await;
    let saved: i64 = rig
        .app
        .db()
        .query_row("SELECT COUNT(*) FROM compat_play_queue_items", [], |row| {
            row.get(0)
        })
        .expect("queue rows count");
    assert_eq!(saved, 1);
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playlist_refusals_keep_their_codes_and_repeats_stay() {
    let rig = app("compat-catalog-refusals").await;
    let other = native(&rig, "other").await;
    let other = [("authorization", other.as_str())];
    let (status, theirs) = rig
        .app
        .call(
            "POST",
            "/api/v3/playlists",
            &other,
            Some(json!({"name": "Theirs"})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{theirs}");
    let theirs = theirs["id"].as_str().expect("playlist id").to_owned();
    let id = format!("pl-{theirs}");

    // Private to someone else: not found (70), never a server fault.
    let hidden = subsonic(&rig, "getPlaylist", &[("id", &id)]).await;
    assert!(hidden.contains("\"code\":70"), "{hidden}");
    // Public but not ours to change: not authorized (50).
    let (status, shown) = rig
        .app
        .call(
            "PATCH",
            &format!("/api/v3/playlists/{theirs}/visibility"),
            &other,
            Some(json!({"is_public": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    let refused = subsonic(
        &rig,
        "updatePlaylist",
        &[("playlistId", &id), ("name", "Mine")],
    )
    .await;
    assert!(refused.contains("\"code\":50"), "{refused}");

    // The same song asked twice is added twice (v2 appends each).
    let created = subsonic(&rig, "createPlaylist", &[("name", "Twice")]).await;
    let ours: Value = serde_json::from_str(&created).expect("json answer");
    let ours = ours["subsonic-response"]["playlist"]["id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    subsonic(
        &rig,
        "updatePlaylist",
        &[
            ("playlistId", &ours),
            ("songIdToAdd", "tr-trk-1"),
            ("songIdToAdd", "tr-trk-1"),
        ],
    )
    .await;
    let listed = subsonic(&rig, "getPlaylist", &[("id", &ours)]).await;
    assert!(listed.contains("\"songCount\":2"), "{listed}");
    rig.stop().await;
}

/// The user the reads rig seeds.
const USER_ID: &str = "u1";

/// The seeded catalog read through the native reads bundle alone, for the
/// export and the Jellyfin id map.
struct Rig {
    _dir: ScratchDir,
    runtime: DbRuntime,
    platform: Platform,
    library: LibrarySetup,
    reads: ReadsSetup,
    auth: AuthRig,
}

impl Rig {
    async fn open() -> Self {
        let dir = ScratchDir::new("compat-catalog");
        let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
        runtime
            .db()
            .write(Lane::Foreground, "seed", |tx| {
                tx.execute_batch(
                    "INSERT INTO auth_users (id, display_name, role, created_at) \
                     VALUES ('u1', 'User', 'user', '2024-01-01T00:00:00Z');",
                )?;
                tx.execute_batch(LIBRARY)?;
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            })
            .await
            .unwrap();
        let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
            .build()
            .await;
        let auth = AuthRig::on(&platform);
        let (library, _) = crate::common::library::library_at(&platform);
        let reads = crate::common::bundles::reads_at(
            &platform,
            auth.deps.clone(),
            crate::common::reads_inputs(&dir),
            &library,
            CollectionsState::new(
                CollectionsDb::new(runtime.db().clone(), platform.clock.clone()),
                droppedneedle_catalog::reads::library::store::LocalCatalog::new(
                    runtime.db().clone(),
                ),
            ),
            None,
        );
        Self {
            _dir: dir,
            runtime,
            platform,
            library,
            reads,
            auth,
        }
    }
}

/// The Navidrome m3u export reads the playlists the web UI saves: a local
/// entry resolves to its file, and a fractional duration (the web player
/// sends float seconds) does not make the playlist read fail.
#[tokio::test]
async fn native_playlists_export_to_m3u_with_their_files() {
    use droppedneedle_kernel::jobs::playlist_export::M3uPlaylistExporter;
    use droppedneedle_kernel::jobs::playlist_sync::{PlaylistExporter as _, PlaylistSyncConfig};

    let rig = Rig::open().await;
    let native = CollectionsService::new(&rig.reads.collections);
    let made = native
        .create_playlist(
            USER_ID,
            &CreatePlaylistBody {
                name: "Export me".to_owned(),
                source_ref: None,
            },
        )
        .await
        .unwrap();
    native
        .add_tracks(
            USER_ID,
            &made.id,
            &AddTracksBody {
                tracks: vec![TrackInput {
                    track_name: "Mysterons".to_owned(),
                    artist_name: "Portishead".to_owned(),
                    album_name: "Dummy".to_owned(),
                    album_id: None,
                    artist_id: None,
                    track_source_id: Some("trk-1".to_owned()),
                    cover_url: None,
                    source_type: "local".to_owned(),
                    available_sources: None,
                    format: None,
                    track_number: None,
                    disc_number: None,
                    duration: Some(305.6),
                    plex_rating_key: None,
                }],
                position: None,
            },
        )
        .await
        .unwrap();

    let out = ScratchDir::new("m3u-export");
    let folder = out.join("playlists");
    let report = M3uPlaylistExporter::new(Some(rig.runtime.db().sqlx().clone()))
        .sync(PlaylistSyncConfig {
            target_dir: folder.display().to_string(),
            scope: "all".to_owned(),
            remove_deleted: true,
        })
        .await;
    assert!(report.success, "{}", report.message);
    assert_eq!(report.written, 1, "{}", report.message);
    let file = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "m3u8"))
        .unwrap();
    let text = std::fs::read_to_string(file).unwrap();
    assert!(
        text.contains("#EXTINF:305,Portishead - Mysterons"),
        "{text}"
    );
    assert!(text.contains("music/dummy/01.flac"), "{text}");
}

/// Move the catalog revision, as every catalog write does.
const BUMP_REVISION: &str = "INSERT INTO library_catalog_revision (singleton, value) \
    VALUES (1, 1) ON CONFLICT (singleton) DO UPDATE SET value = value + 1;";

/// A second album by Portishead with one track.
const SECOND_ALBUM: &str = "INSERT INTO local_albums (id, root_id, grouping_key, title, \
    title_folded, album_artist_name, album_artist_name_folded, album_artist_id, year, \
    grouping_source, created_at, updated_at) VALUES ('alb-2', 'root', 'k2', 'Portishead', \
    'portishead', 'Portishead', 'portishead', 'art-1', 1997, 'automatic', 1, 1); \
    INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
    path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
    album_title, album_title_folded, file_format, ingest_source, imported_at, \
    membership_source) VALUES ('trk-2', 'alb-2', 'root', '/music/portishead/01.flac', \
    'portishead/01.flac', 'h2', 1000, 1, 'r2', 'Cowboys', 'cowboys', 'Portishead', \
    'portishead', 'flac', 'scan', 1, 'automatic');";

/// Write `sql` and move the catalog revision in one transaction through
/// the rig's writer, as a scan writes the catalog.
async fn write_catalog(rig: &Rig, sql: &'static str) {
    rig.runtime
        .db()
        .write(Lane::Foreground, "seed", move |tx| {
            tx.execute_batch(sql)?;
            tx.execute_batch(BUMP_REVISION)?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .unwrap();
}

/// The stored Jellyfin ids over the rig's database under the data key
/// `[key; 32]`, as one process holds them. A signed-in miss waits `wait`.
fn jellyfin_ids(rig: &Rig, key: u8, wait: Duration) -> JellyfinIds {
    let crypto = Arc::new(Crypto::from_key_bytes(&[key; 32]).unwrap());
    JellyfinIds::new(rig.runtime.db().clone(), crypto, wait)
}

/// A process started under `key`: its id map once its first fill pass
/// has committed.
async fn booted(rig: &Rig, key: u8) -> (CatalogIds, JellyfinIds) {
    let ids = jellyfin_ids(rig, key, Duration::from_secs(1));
    ids.fill().pass().await.unwrap();
    (
        CatalogIds::new(ids.clone(), rig.reads.library.catalog.clone()),
        ids,
    )
}

fn named(kind: &str, internal: &str) -> Option<(String, String)> {
    Some((kind.to_owned(), internal.to_owned()))
}

/// Ids handed out before a restart resolve after it straight from the
/// stored table, before the new process has run a fill pass and with the
/// catalog impossible to list (its genre table is gone, so any read of
/// every item fails): an album, a track, a playlist and an album's old
/// unkeyed id for a signed-in caller, the album for a caller without
/// sign-in. An id never handed out answers at once, with a commit the
/// fill has not seen yet, rather than waiting for a pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_ids_resolve_after_a_restart_without_reading_the_catalog() {
    let rig = Rig::open().await;
    write_catalog(
        &rig,
        "INSERT INTO playlists (id, name, created_at, updated_at, user_id) \
         VALUES ('pl-1', 'Mix', '2024-01-01', '2024-01-01', 'u1');",
    )
    .await;
    let (before, _) = booted(&rig, 7).await;
    let handed = [
        before.to_jf("album", "alb-1").await,
        before.to_jf("track", "trk-1").await,
        before.to_jf("playlist", "pl-1").await,
        CatalogIds::derive("album", "alb-1"),
    ];
    drop(before);
    write_catalog(&rig, "DROP TABLE local_track_genres;").await;

    let wait = Duration::from_secs(2);
    let ids = CatalogIds::new(
        jellyfin_ids(&rig, 7, wait),
        rig.reads.library.catalog.clone(),
    );
    assert_eq!(
        ids.from_jf_anonymous(&handed[0]).await,
        Ok(named("album", "alb-1"))
    );
    let mut found = Vec::new();
    for id in &handed {
        found.push(ids.from_jf(id).await);
    }
    assert_eq!(
        found,
        [
            Ok(named("album", "alb-1")),
            Ok(named("track", "trk-1")),
            Ok(named("playlist", "pl-1")),
            Ok(named("album", "alb-1")),
        ]
    );
    let started = std::time::Instant::now();
    assert_eq!(
        ids.from_jf_anonymous("0123456789abcdef0123456789abcdef")
            .await,
        Ok(None)
    );
    assert!(started.elapsed() < wait, "an anonymous miss never waits");
}

/// A stored album or artist id survives a new data key: the server keeps
/// handing it out and both lookups answer it. An album added afterwards
/// gets its id under the new key. Track, playlist, genre and library ids
/// never depend on the key. The derivation is pinned here, since the
/// golden traces mask ids.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stored_jellyfin_ids_survive_a_new_data_key() {
    let rig = Rig::open().await;
    let (seven, _) = booted(&rig, 7).await;
    let album = seven.to_jf("album", "alb-1").await;
    let artist = seven.to_jf("artist", "art-1").await;
    assert_eq!(album, "99b1c37e25c6afcbaa3b4632f29459bd");
    assert_eq!(
        CatalogIds::derive("album", "alb-1"),
        "e14c88410632f4de1c3da5b07ae1f34d"
    );

    let (eight, ids) = booted(&rig, 8).await;
    assert_ne!(eight.id_for("album", "alb-1"), album);
    assert_eq!(eight.to_jf("album", "alb-1").await, album);
    assert_eq!(eight.to_jf("artist", "art-1").await, artist);
    assert_eq!(eight.from_jf(&album).await, Ok(named("album", "alb-1")));
    assert_eq!(
        eight.from_jf_anonymous(&artist).await,
        Ok(named("artist", "art-1"))
    );

    write_catalog(&rig, SECOND_ALBUM).await;
    ids.fill().pass().await.unwrap();
    let added = eight.to_jf("album", "alb-2").await;
    assert_eq!(added, eight.id_for("album", "alb-2"));
    assert_ne!(added, seven.id_for("album", "alb-2"));
    assert_eq!(
        eight.from_jf_anonymous(&added).await,
        Ok(named("album", "alb-2"))
    );

    for (kind, internal) in [
        ("track", "trk-1"),
        ("playlist", "pl-1"),
        ("genre", "trip-hop"),
        ("library", LIBRARY_INTERNAL_ID),
    ] {
        assert_eq!(seven.id_for(kind, internal), eight.id_for(kind, internal));
        assert_eq!(
            eight.to_jf(kind, internal).await,
            CatalogIds::derive(kind, internal)
        );
    }
}

/// The Jellyfin router over the production adapters and `ids`, which
/// fill only when a test runs their passes.
fn jellyfin_router(rig: &Rig, ids: &JellyfinIds) -> axum::Router {
    let collections = crate::common::bundles::collections_at(&rig.platform);
    let acquire = crate::common::bundles::acquire_at(
        &rig.platform,
        rig.auth.deps.clone(),
        &rig.library,
        &collections.stores,
    );
    let media = crate::common::bundles::media_at(
        &rig.platform,
        rig.auth.deps.clone(),
        &rig.library,
        &rig.reads,
        &acquire,
    );
    let library = CompatLibrary::from_reads(&rig.reads);
    rig.platform
        .settings
        .save_blocking(ConnectApps {
            jellyfin_enabled: true,
            ..ConnectApps::default()
        })
        .expect("connect apps save");
    let engine = media.stream.engine.clone();
    let settings = LiveSettings::from_config(&rig.platform.settings, engine.can_transcode());
    routes(JellyfinState::new(
        rig.auth.compat_passwords(),
        JellyfinLibrary::new(library.clone()),
        GatewayAudio::new(engine),
        CompatPlayback::new(media.playback.clone()),
        CatalogIds::new(ids.clone(), library.catalog.clone()),
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

async fn jellyfin_get(
    router: axum::Router,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, Value, bool) {
    let mut request = axum::http::Request::builder().uri(uri);
    if let Some(token) = token {
        request = request.header(
            "authorization",
            format!("MediaBrowser Client=\"Test\", Token=\"{token}\""),
        );
    }
    let response = router
        .oneshot(request.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let retry = response.headers().contains_key("retry-after");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        retry,
    )
}

/// Until this process's first fill pass has committed, every Jellyfin
/// route answers 503 server busy with `Retry-After`, the picture address
/// with no sign-in included, so no app is told an item it holds is gone.
/// After it, an `Ids=` list of ids the store has not seen since the last
/// commit waits once for the fill, not once per id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_answers_busy_until_its_ids_load_and_waits_once_per_request() {
    const SECRET: &str = "alice-app-secret";
    let rig = Rig::open().await;
    let user = rig.auth.seed_user("alice", Role::User).await;
    rig.auth.seed_app_password(&user.id, SECRET).await;
    let wait = Duration::from_secs(3);
    let ids = jellyfin_ids(&rig, 7, wait);
    let album = ids.computed("album", "alb-1");
    let picture = format!("/jellyfin/Items/{album}/Images/Primary");
    let listed = format!("/jellyfin/Items?Ids={album}");
    let router = jellyfin_router(&rig, &ids);
    for (uri, token) in [
        (picture.as_str(), None),
        (listed.as_str(), Some(SECRET)),
        ("/jellyfin/System/Info/Public", None),
    ] {
        let (status, _, retry) = jellyfin_get(router.clone(), uri, token).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert!(retry, "{uri} says when to try again");
    }

    ids.fill().pass().await.unwrap();
    let (status, _, _) = jellyfin_get(router.clone(), &picture, None).await;
    assert_ne!(status, StatusCode::SERVICE_UNAVAILABLE);

    write_catalog(&rig, "").await;
    let unseen = (1..=3)
        .map(|n| format!("{n:032x}"))
        .collect::<Vec<_>>()
        .join(",");
    let started = std::time::Instant::now();
    let (status, body, _) = jellyfin_get(
        router,
        &format!("/jellyfin/Items?Ids={unseen}"),
        Some(SECRET),
    )
    .await;
    let took = started.elapsed();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["TotalRecordCount"], 0, "{body}");
    assert!(took >= wait, "the misses waited for the fill: {took:?}");
    // One wait for all three misses; a wait per miss would take at least
    // three times as long.
    assert!(took < wait * 3, "three misses waited once: {took:?}");
}
