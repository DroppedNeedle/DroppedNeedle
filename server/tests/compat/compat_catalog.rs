//! Compat over the real catalog: Subsonic and Jellyfin read the scanned
//! library from SQLite, and playlists and favorites are one set of rows
//! shared with the native routes and the Navidrome m3u export.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use droppedneedle_catalog::reads::ReadsSetup;
use droppedneedle_catalog::reads::collections::CollectionsState;
use droppedneedle_catalog::reads::collections::db::CollectionsDb;
use droppedneedle_catalog::reads::collections::models::{
    AddTracksBody, CreatePlaylistBody, TrackInput,
};
use droppedneedle_catalog::reads::collections::service::CollectionsService;
use droppedneedle_compat::adapters::jellyfin_library::CatalogIds;
use droppedneedle_compat::adapters::library::CompatLibrary;
use droppedneedle_compat::jellyfin::builders::LIBRARY_INTERNAL_ID;
use droppedneedle_compat::jellyfin::seams::IdMap as _;
use droppedneedle_kernel::db::{DbRuntime, Lane};
use droppedneedle_kernel::runtime_config::Crypto;
use serde_json::{Value, json};

use crate::common::ScratchDir;
use crate::common::auth::AuthRig;
use crate::common::compat::{CompatApp, rest};

/// One album by Portishead with one track, as a scan writes them.
const LIBRARY: &str = "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
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
    let (status, _, body) = rig
        .app
        .raw(
            "GET",
            &rest(&call, &rig.subsonic_auth("catalog")),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{endpoint}");
    String::from_utf8(body).expect("utf-8 answer")
}

/// A Jellyfin `Items` page for the player: the item names and the total.
async fn jellyfin_items(rig: &CompatApp, query: &str) -> (Vec<Value>, u64) {
    let token = rig.jellyfin_auth();
    let (status, body) = rig
        .app
        .call(
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
    reads: ReadsSetup,
    _auth: AuthRig,
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
            CollectionsState::new(CollectionsDb::new(runtime.db().clone())),
            None,
        );
        Self {
            _dir: dir,
            runtime,
            reads,
            _auth: auth,
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

/// Jellyfin ids over the seeded catalog, keyed with `key`, reading the
/// catalog again with no wait between reads.
fn catalog_ids(rig: &Rig, key: u8) -> CatalogIds {
    let crypto = Arc::new(Crypto::from_key_bytes(&[key; 32]).unwrap());
    CatalogIds::new(
        CompatLibrary::from_reads(&rig.reads),
        crypto,
        Duration::ZERO,
    )
}

/// A caller who has not signed in causes one catalog read, the first, and
/// never another: an album added after it is found only once a signed-in
/// lookup has read the catalog again. An album's old unkeyed id answers
/// only the signed-in lookup.
#[tokio::test]
async fn jellyfin_ids_unknown_to_a_caller_without_sign_in_read_the_catalog_once() {
    let rig = Rig::open().await;
    let ids = catalog_ids(&rig, 7);
    let unknown = "0123456789abcdef0123456789abcdef";
    assert_eq!(ids.from_jf_anonymous(unknown).await, None);
    let first = ids.id_for("album", "alb-1");
    let found = Some(("album".to_owned(), "alb-1".to_owned()));
    assert_eq!(ids.from_jf_anonymous(&first).await, found);
    let old = CatalogIds::derive("album", "alb-1");
    assert_eq!(ids.from_jf_anonymous(&old).await, None);
    assert_eq!(ids.from_jf(&old).await, found);

    rig.runtime
        .db()
        .write(Lane::Foreground, "seed", |tx| {
            tx.execute_batch(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                 album_artist_name, album_artist_name_folded, album_artist_id, year, \
                 grouping_source, created_at, updated_at) VALUES ('alb-2', 'root', 'k2', \
                 'Portishead', 'portishead', 'Portishead', 'portishead', 'art-1', 1997, \
                 'automatic', 1, 1); \
                 INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                 relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                 title, title_folded, album_title, album_title_folded, file_format, \
                 ingest_source, imported_at, membership_source) VALUES ('trk-2', 'alb-2', \
                 'root', '/music/portishead/01.flac', 'portishead/01.flac', 'h2', 1000, 1, \
                 'r2', 'Cowboys', 'cowboys', 'Portishead', 'portishead', 'flac', 'scan', 1, \
                 'automatic')",
            )?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .unwrap();
    let added = ids.id_for("album", "alb-2");
    let found = Some(("album".to_owned(), "alb-2".to_owned()));
    assert_eq!(ids.from_jf_anonymous(&added).await, None);
    assert_eq!(ids.from_jf(&added).await, found);
    assert_eq!(ids.from_jf_anonymous(&added).await, found);
}

/// Album and artist ids depend on the data key; track, playlist, genre and
/// library ids do not, so music and playlists an app downloaded stay
/// matched and its stored library still browses.
#[tokio::test]
async fn only_album_and_artist_ids_depend_on_the_data_key() {
    let rig = Rig::open().await;
    let (one, two) = (catalog_ids(&rig, 7), catalog_ids(&rig, 8));
    for (kind, internal) in [("album", "alb-1"), ("artist", "art-1")] {
        assert_ne!(one.id_for(kind, internal), two.id_for(kind, internal));
        assert_ne!(
            one.id_for(kind, internal),
            CatalogIds::derive(kind, internal)
        );
    }
    for (kind, internal) in [
        ("track", "trk-1"),
        ("playlist", "pl-1"),
        ("genre", "trip-hop"),
        ("library", LIBRARY_INTERNAL_ID),
    ] {
        assert_eq!(one.id_for(kind, internal), two.id_for(kind, internal));
        assert_eq!(
            one.id_for(kind, internal),
            CatalogIds::derive(kind, internal)
        );
    }
}
