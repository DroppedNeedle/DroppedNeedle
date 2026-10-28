//! Library reads over the real stores on a migrated template database:
//! the auth matrix, input validation and fault hiding over the routes, and
//! the catalog, favorites and lookups reads, including a missing and an
//! excluded track proving streamable-only counts.

use crate::common::auth::{test_principal, with_test_principal};
use crate::common::{ScratchDir, TestApp};
use droppedneedle_catalog::reads::library;
use droppedneedle_catalog::reads::lyrics::LyricsService;

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_kernel::db::Db;
use droppedneedle_library::scan::coordinator::SharedResolver;
use droppedneedle_media::stream::local_files::LibraryFiles;
use library::LibraryDeps;
use library::store::LocalCatalog;
use library::stores::{
    After, AlbumFilter, AlbumSort, ArtistScope, ArtistSort, BoxFuture, LyricDoc, LyricsPort,
    StoreError, TrackFilter, TrackSort,
};
use library::tags::FileTagReader;
use serde_json::Value;
use sqlx::SqlitePool;
use tower::ServiceExt as _;

/// Lyrics answered from a fixed set of docs.
struct ScriptedLyrics(HashMap<String, LyricDoc>);

impl LyricsPort for ScriptedLyrics {
    fn get<'a>(&'a self, track_id: &'a str) -> BoxFuture<'a, Result<Option<LyricDoc>, StoreError>> {
        Box::pin(async move { Ok(self.0.get(track_id).cloned()) })
    }
}

fn scripted_lyrics() -> ScriptedLyrics {
    ScriptedLyrics(HashMap::from([(
        "t1".to_owned(),
        LyricDoc {
            lines: vec![
                ("line one".to_owned(), Some(1000)),
                ("line two".to_owned(), Some(4500)),
            ],
            synced: true,
        },
    )]))
}

/// The production library deps over one database, with `lyrics` for the
/// lyrics route, or the production lyrics without an online lookup.
fn deps(db: &Db, lyrics: Option<Arc<dyn LyricsPort>>) -> LibraryDeps {
    let catalog = LocalCatalog::new(db.clone());
    let files = LibraryFiles::new(db.sqlx().clone(), SharedResolver::default());
    LibraryDeps {
        lyrics: lyrics
            .unwrap_or_else(|| Arc::new(LyricsService::new(catalog.clone(), files.clone(), None))),
        catalog,
        upgrade_policy: None,
        tags: FileTagReader::new(files),
    }
}

/// The signed-in user the routes run as.
const ADA: (&str, Role) = ("user-ada", Role::User);

/// Router signed in as `caller`, or anonymous when None.
fn app(deps: LibraryDeps, caller: Option<(&str, Role)>) -> Router {
    let router = library::routes().with_state(deps).into_router();
    match caller {
        Some((user_id, role)) => with_test_principal(
            router,
            test_principal(user_id, role, "sess-1", SessionKind::Standard),
        ),
        None => router,
    }
}

async fn get(app: Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::get(path)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

/// Every library route with ids the seeded catalog holds. The auth and
/// fault loops walk this.
const ROUTES: &[&str] = &[
    "/api/v3/library/albums",
    "/api/v3/library/albums/al1",
    "/api/v3/library/albums/al1/tracks",
    "/api/v3/library/albums/al1/copies",
    "/api/v3/library/artists",
    "/api/v3/library/artists/a1",
    "/api/v3/library/artists/a1/albums",
    "/api/v3/library/artists/a1/appearances",
    "/api/v3/library/tracks",
    "/api/v3/library/tracks/t1",
    "/api/v3/library/tracks/t1/lyrics",
    "/api/v3/library/stats",
    "/api/v3/library/recently-added",
    "/api/v3/library/genres",
    "/api/v3/library/genres/rock/tracks",
    "/api/v3/library/albums/match/rg1",
    "/api/v3/library/decades",
    "/api/v3/library/suggestions",
];

#[tokio::test]
async fn bad_input_stays_in_the_envelope() {
    let fixture = sqlite_fixture().await;
    let app = app(
        deps(fixture.runtime.db(), Some(Arc::new(scripted_lyrics()))),
        Some(ADA),
    );
    for path in [
        "/api/v3/library/albums?sort=nope",
        "/api/v3/library/albums?order=sideways",
        "/api/v3/library/albums?limit=0",
        "/api/v3/library/albums?limit=abc",
        "/api/v3/library/albums?after=not-a-cursor",
        "/api/v3/library/tracks?after=not-a-cursor",
        "/api/v3/library/albums?decade=1995",
        "/api/v3/library/artists?scope=nope",
        "/api/v3/library/artists?sort=nope",
        "/api/v3/library/tracks?sort=nope",
        "/api/v3/library/suggestions?limit=99",
    ] {
        let (status, json) = get(app.clone(), path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "for {path}");
        assert_eq!(json["error"]["code"], "INVALID_INPUT", "for {path}");
        assert!(json["error"]["message"].is_string(), "for {path}");
    }
}

#[tokio::test]
async fn auth_matrix_every_route_401s_anonymous_and_admits_users() {
    let fixture = sqlite_fixture().await;
    let deps = deps(fixture.runtime.db(), Some(Arc::new(scripted_lyrics())));
    let authenticated = app(deps.clone(), Some(ADA));
    let anonymous = app(deps.clone(), None);
    for path in ROUTES {
        let (status, _) = get(authenticated.clone(), path).await;
        assert_eq!(status, StatusCode::OK, "user should pass {path}");
        let response = anonymous
            .clone()
            .oneshot(
                Request::get(*path)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router answers");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "anon {path}");
        let challenge = response
            .headers()
            .get(axum::http::header::WWW_AUTHENTICATE)
            .expect("challenge header");
        assert_eq!(challenge, "Bearer", "anon {path}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let json: Value = serde_json::from_slice(&bytes).expect("body is json");
        assert_eq!(json["error"]["code"], "UNAUTHORIZED", "anon {path}");
    }
    let as_admin = app(deps, Some(("user-root", Role::Admin)));
    let (status, _) = get(as_admin, "/api/v3/library/stats").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn every_route_hides_store_faults() {
    let scope = TestApp::start("reads-library-faults").await;
    // A database without the catalog's tables: every store read fails.
    let dir = ScratchDir::new("reads-library-faults");
    let secret = dir.join("secret-library.db");
    std::fs::write(&secret, b"").expect("empty database file writes");
    let db = Db::open_for_tests(&secret).expect("empty database opens");
    let app = scope.request_scope(app(deps(&db, None), Some(ADA)));
    for path in ROUTES {
        let response = app
            .clone()
            .oneshot(
                Request::get(*path)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router answers");
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "for {path}"
        );
        let request_id = response.headers()["x-request-id"]
            .to_str()
            .expect("request id is text")
            .to_owned();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let text = String::from_utf8(bytes.to_vec()).expect("body is text");
        assert!(!text.contains("no such table"), "cause leaked on {path}");
        let json: Value = serde_json::from_str(&text).expect("body is json");
        assert_eq!(
            json,
            serde_json::json!({"error": {
                "code": "INTERNAL_ERROR",
                "message": "Internal server error",
                "details": {"error_id": request_id},
            }}),
            "for {path}"
        );
    }
}

struct SqliteFixture {
    runtime: droppedneedle_kernel::db::DbRuntime,
    catalog: LocalCatalog,
    /// Declared last so the database closes before the directory goes.
    _scratch: ScratchDir,
}

async fn sqlite_fixture() -> SqliteFixture {
    let dir = ScratchDir::new("reads-library");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let seed = [
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) VALUES \
         ('a1', 'Aurora', 'aurora', 'group', 1000, 1000), \
         ('a2', 'Boreal', 'boreal', 'person', 1500, 1500), \
         ('a3', 'Guest', 'guest', 'person', 1600, 1600), \
         ('ax', 'Retired', 'retired', 'person', 900, 900)",
        "UPDATE local_artists SET retired_into_artist_id = 'a1' WHERE id = 'ax'",
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, \
         grouping_source, created_at, updated_at) VALUES \
         ('al1', 'r1', 'g1', 'First Light', 'first light', 'Aurora', 'aurora', 'a1', 1994, 'automatic', 1000, 1000), \
         ('al2', 'r1', 'g2', 'Second Dawn', 'second dawn', 'Aurora', 'aurora', 'a1', 2001, 'automatic', 2000, 2000), \
         ('al3', 'r1', 'g3', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', 'a2', 1994, 'automatic', 1500, 1500), \
         ('al4', 'r1', 'g4', 'First Light (Deluxe)', 'first light (deluxe)', 'Aurora', 'aurora', 'a1', 1994, 'automatic', 1200, 1200)",
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
         file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, artist_name, artist_name_folded, \
         album_title, album_title_folded, album_artist_name, album_artist_name_folded, \
         disc_number, track_number, year, duration_seconds, file_format, availability, \
         ingest_source, imported_at, membership_source) VALUES \
         ('t1', 'al1', 'r1', '/m/t1.flac', 't1.flac', 'h1', 4000000, 1, 's1', 'Opener', 'opener', \
          'Aurora', 'aurora', 'First Light', 'first light', 'Aurora', 'aurora', 1, 1, 1994, 200.0, 'flac', 'indexed', 'scan', 1000.0, 'automatic'), \
         ('t2', 'al1', 'r1', '/m/t2.flac', 't2.flac', 'h2', 4000000, 1, 's2', 'Closer', 'closer', \
          'Aurora', 'aurora', 'First Light', 'first light', 'Aurora', 'aurora', 1, 2, 1994, 200.0, 'flac', 'indexed', 'scan', 1100.0, 'automatic'), \
         ('t3', 'al1', 'r1', '/m/t3.flac', 't3.flac', 'h3', 4000000, 1, 's3', 'Gone', 'gone', \
          'Aurora', 'aurora', 'First Light', 'first light', 'Aurora', 'aurora', 1, 3, 1994, 200.0, 'flac', 'missing', 'scan', 1150.0, 'automatic'), \
         ('t4', 'al2', 'r1', '/m/t4.mp3', 't4.mp3', 'h4', 3000000, 1, 's4', 'Single', 'single', \
          'Aurora', 'aurora', 'Second Dawn', 'second dawn', 'Aurora', 'aurora', 1, 1, 2001, 180.0, 'mp3', 'indexed', 'scan', 2000.0, 'automatic'), \
         ('t5', 'al2', 'r1', '/m/t5.mp3', 't5.mp3', 'h5', 3000000, 1, 's5', 'Skipped', 'skipped', \
          'Aurora', 'aurora', 'Second Dawn', 'second dawn', 'Aurora', 'aurora', 1, 2, 2001, 180.0, 'mp3', 'excluded', 'scan', 2050.0, 'automatic'), \
         ('t6', 'al3', 'r1', '/m/t6.flac', 't6.flac', 'h6', 5000000, 1, 's6', 'Peak', 'peak', \
          'Boreal', 'boreal', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', 1, 1, 1994, 220.0, 'flac', 'indexed', 'scan', 1500.0, 'automatic')",
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) VALUES \
         ('t1', 0, 'a1', 'main'), ('t2', 0, 'a1', 'main'), ('t3', 0, 'a1', 'main'), \
         ('t4', 0, 'a1', 'main'), ('t5', 0, 'a1', 'main'), \
         ('t6', 0, 'a2', 'main'), ('t6', 1, 'a3', 'guest')",
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) VALUES \
         ('al1', 0, 'a1', 'main'), ('al2', 0, 'a1', 'main'), \
         ('al3', 0, 'a2', 'main'), ('al4', 0, 'a1', 'main')",
        "INSERT INTO local_album_external_identities \
         (local_album_id, provider, release_group_mbid, release_mbid, decision_source, selected_at) VALUES \
         ('al1', 'musicbrainz', 'rg1', 'r1', 'manual', 1000), \
         ('al4', 'musicbrainz', 'rg1', NULL, 'manual', 1200)",
        "INSERT INTO local_artist_external_identities \
         (local_artist_id, provider, provider_artist_id, decision_source, selected_at) VALUES \
         ('a1', 'musicbrainz', 'm1', 'manual', 1000), \
         ('a3', 'musicbrainz', 'm3', 'manual', 1600)",
        "INSERT INTO local_album_artwork (local_album_id, source, updated_at) VALUES \
         ('al1', 'embedded', 1000)",
        "INSERT INTO local_track_genres (local_track_id, position, name, folded_name, source) VALUES \
         ('t1', 0, 'Rock', 'rock', 'local'), ('t2', 0, 'Rock', 'rock', 'local'), \
         ('t4', 0, 'Jazz', 'jazz', 'local')",
        "INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) VALUES \
         ('u1', 'album', 'al1', 1000), ('u1', 'artist', 'a2', 1000), ('u1', 'track', 't4', 1000)",
    ]
    .join(";\n");
    runtime
        .db()
        .write(
            droppedneedle_kernel::db::Lane::Foreground,
            "library seed",
            move |tx| {
                tx.execute_batch(&seed)
                    .map_err(droppedneedle_kernel::db::DbError::from)
            },
        )
        .await
        .expect("seed batch runs");
    SqliteFixture {
        catalog: LocalCatalog::new(runtime.db().clone()),
        runtime,
        _scratch: dir,
    }
}

#[tokio::test]
async fn sqlite_albums_streamable_counts_identity_and_filters() {
    let fixture = sqlite_fixture().await;
    let filter = AlbumFilter::default();
    let (albums, total) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Name, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("albums list");
    assert_eq!(total, 4);
    let first = albums
        .iter()
        .find(|album| album.id == "al1")
        .expect("al1 listed");
    assert_eq!(first.track_count, 2);
    assert_eq!(first.total_duration_seconds, 400.0);
    assert_eq!(first.total_size_bytes, 8_000_000);
    assert_eq!(first.format.as_deref(), Some("flac"));
    assert_eq!(first.release_group_mbid.as_deref(), Some("rg1"));
    assert_eq!(first.artist_mbid.as_deref(), Some("m1"));
    assert!(first.linked);
    assert!(first.cover_available);
    let second = albums
        .iter()
        .find(|album| album.id == "al2")
        .expect("al2 listed");
    assert_eq!(second.track_count, 1);
    assert!(!second.linked);

    assert!(
        fixture
            .catalog
            .get_album("nope")
            .await
            .expect("lookup runs")
            .is_none()
    );

    let filter = AlbumFilter {
        q: Some("peak".to_owned()),
        ..Default::default()
    };
    let (_, total) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Name, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("q filter");
    assert_eq!(total, 1);

    let filter = AlbumFilter {
        decade: Some(1990),
        ..Default::default()
    };
    let (_, total) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Name, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("decade filter");
    assert_eq!(total, 3);

    let filter = AlbumFilter {
        artist_id: Some("a1".to_owned()),
        ..Default::default()
    };
    let (_, total) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Name, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("artist filter");
    assert_eq!(total, 3);
}

#[tokio::test]
async fn sqlite_format_filter_artist_sort_and_release_group_lookup() {
    let fixture = sqlite_fixture().await;
    for (format, total) in [("flac", 2), ("mp3", 1), ("FLAC", 2), ("ogg", 0)] {
        let filter = AlbumFilter {
            format: Some(format.to_lowercase()),
            ..Default::default()
        };
        let (albums, counted) = fixture
            .catalog
            .list_albums(&filter, AlbumSort::Name, false, 50, &After::default())
            .await
            .map(|page| (page.items, page.total))
            .expect("format filter");
        assert_eq!(counted, total, "for {format}");
        assert_eq!(albums.len() as u64, total, "for {format}");
    }

    let filter = AlbumFilter::default();
    let (albums, _) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Artist, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("artist sort");
    assert_eq!(
        albums
            .iter()
            .map(|album| album.id.as_str())
            .collect::<Vec<_>>(),
        ["al1", "al4", "al2", "al3"]
    );
    let (albums, _) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Artist, true, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("artist sort desc");
    assert_eq!(
        albums
            .iter()
            .map(|album| album.id.as_str())
            .collect::<Vec<_>>(),
        ["al3", "al2", "al4", "al1"]
    );

    let found = fixture
        .catalog
        .get_album_by_release_group("rg1")
        .await
        .expect("lookup runs")
        .expect("rg1 resolves");
    assert_eq!(found.id, "al1");
    assert!(
        fixture
            .catalog
            .get_album_by_release_group("nope")
            .await
            .expect("lookup runs")
            .is_none()
    );
}

#[tokio::test]
async fn sqlite_album_tracks_and_copies() {
    let fixture = sqlite_fixture().await;
    let (tracks, total) = fixture
        .catalog
        .album_tracks("al1", 50, 0)
        .await
        .expect("tracks list");
    assert_eq!(total, 2);
    assert_eq!(
        tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        ["t1", "t2"]
    );

    let copies = fixture
        .catalog
        .album_copies("al1")
        .await
        .expect("copies list");
    assert_eq!(
        copies
            .iter()
            .map(|album| album.id.as_str())
            .collect::<Vec<_>>(),
        ["al4"]
    );
    let copies = fixture
        .catalog
        .album_copies("al2")
        .await
        .expect("copies list");
    assert!(copies.is_empty());
}

#[tokio::test]
async fn sqlite_artists_scopes_and_credits() {
    let fixture = sqlite_fixture().await;
    let (artists, total, led, contributors) = fixture
        .catalog
        .list_artists(ArtistScope::All, None, ArtistSort::Name, false, 50, 0)
        .await
        .expect("artists list");
    // Every credited artist: the retired row and the migration's uncredited
    // Various/Unknown sentinels stay out.
    assert_eq!((total, led, contributors), (3, 2, 1));
    assert!(!artists.iter().any(|artist| artist.id == "ax"));

    let (scoped, total, _, _) = fixture
        .catalog
        .list_artists(
            ArtistScope::Contributors,
            None,
            ArtistSort::Name,
            false,
            50,
            0,
        )
        .await
        .expect("contributors list");
    assert_eq!(total, 1);
    assert_eq!(scoped[0].id, "a3");

    let a1 = fixture
        .catalog
        .get_artist("a1")
        .await
        .expect("lookup runs")
        .expect("a1 found");
    // al4 has no files, so it is not counted as one of a1's albums.
    assert_eq!(
        (a1.album_count, a1.track_count, a1.appearance_album_count),
        (2, 3, 0)
    );
    assert_eq!(a1.artist_mbid.as_deref(), Some("m1"));

    let a3 = fixture
        .catalog
        .get_artist("a3")
        .await
        .expect("lookup runs")
        .expect("a3 found");
    assert_eq!(
        (
            a3.album_count,
            a3.track_count,
            a3.appearance_album_count,
            a3.appearance_track_count
        ),
        (0, 1, 1, 1)
    );

    let (led_albums, total) = fixture
        .catalog
        .led_albums("a1", 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("led list");
    assert_eq!(total, 3);
    assert_eq!(led_albums[0].id, "al1");

    let (appearances, total) = fixture
        .catalog
        .artist_appearances("a3", 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("appearances list");
    assert_eq!(total, 1);
    assert_eq!(appearances[0].id, "al3");
}

#[tokio::test]
async fn appearances_answer_by_musicbrainz_id_as_by_library_id() {
    let fixture = sqlite_fixture().await;
    let deps = deps(fixture.runtime.db(), None);
    let (status, by_id) = get(
        app(deps.clone(), Some(ADA)),
        "/api/v3/library/artists/a3/appearances",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{by_id}");
    assert_eq!(by_id["total"], 1, "{by_id}");
    let (status, by_mbid) = get(
        app(deps.clone(), Some(ADA)),
        "/api/v3/library/artists/M3/appearances",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{by_mbid}");
    assert_eq!(by_mbid, by_id);

    let (status, body) = get(
        app(deps, Some(ADA)),
        "/api/v3/library/artists/no-such-artist/appearances",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn sqlite_tracks_filters_and_stats() {
    let fixture = sqlite_fixture().await;
    let filter = TrackFilter {
        q: Some("single".to_owned()),
        ..Default::default()
    };
    let (tracks, total) = fixture
        .catalog
        .list_tracks(&filter, TrackSort::Title, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("q filter");
    assert_eq!(total, 1);
    assert_eq!(tracks[0].id, "t4");
    assert_eq!(tracks[0].genre.as_deref(), Some("Jazz"));

    let filter = TrackFilter {
        genre: Some("Rock".to_owned()),
        ..Default::default()
    };
    let (_, total) = fixture
        .catalog
        .list_tracks(&filter, TrackSort::Title, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("genre filter");
    assert_eq!(total, 2);

    let filter = TrackFilter {
        artist_id: Some("a3".to_owned()),
        ..Default::default()
    };
    let (tracks, total) = fixture
        .catalog
        .list_tracks(&filter, TrackSort::Title, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("artist filter");
    assert_eq!(total, 1);
    assert_eq!(tracks[0].id, "t6");

    assert!(
        fixture
            .catalog
            .get_track("t3")
            .await
            .expect("lookup runs")
            .is_none()
    );

    let stats = fixture.catalog.stats().await.expect("stats read");
    assert_eq!(
        (stats.total_albums, stats.total_artists, stats.total_tracks),
        (3, 3, 4),
        "the Artists card counts what the Artists page lists, not uncredited rows"
    );
    assert_eq!(stats.total_size_bytes, 16_000_000);
    assert_eq!(stats.format_breakdown.get("flac"), Some(&3));
    assert_eq!(stats.format_breakdown.get("mp3"), Some(&1));
}

#[tokio::test]
async fn sqlite_genres_decades_and_suggestion_pools() {
    let fixture = sqlite_fixture().await;
    let genres = fixture.catalog.genres().await.expect("genres list");
    assert_eq!(genres.len(), 2);
    assert_eq!(genres[0].folded_name, "rock");
    assert_eq!((genres[0].track_count, genres[0].album_count), (2, 1));

    let (_, total) = fixture
        .catalog
        .genre_tracks("ROCK", 50, 0)
        .await
        .expect("genre tracks");
    assert_eq!(total, 2);
    let (_, total) = fixture
        .catalog
        .genre_tracks("polka", 50, 0)
        .await
        .expect("genre tracks");
    assert_eq!(total, 0);

    let decades = fixture.catalog.decades().await.expect("decades list");
    assert_eq!(decades.len(), 2);
    assert_eq!((decades[0].decade, decades[0].album_count), (1990, 2));
    assert_eq!((decades[1].decade, decades[1].album_count), (2000, 1));

    let newest = fixture.catalog.newest_tracks(2).await.expect("newest");
    assert_eq!(
        newest
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        ["t4", "t6"]
    );
    let oldest = fixture.catalog.oldest_tracks(1).await.expect("oldest");
    assert_eq!(oldest[0].id, "t1");
    let era = fixture
        .catalog
        .random_tracks(10, Some(1990))
        .await
        .expect("era random");
    assert!(!era.is_empty());
    assert!(
        era.iter()
            .all(|track| track.year.is_some_and(|year| (1990..2000).contains(&year)))
    );
    assert!(era.iter().all(|track| track.id != "t3" && track.id != "t5"));
}

#[tokio::test]
async fn sqlite_favorites_flags_and_counts() {
    let fixture = sqlite_fixture().await;
    let ids = ["al1".to_owned(), "al2".to_owned()];
    let marked = fixture
        .catalog
        .filter_favorites("u1", "album", &ids)
        .await
        .expect("filter runs");
    assert_eq!(marked, std::collections::HashSet::from(["al1".to_owned()]));
    let counts = fixture
        .catalog
        .favorite_counts("u1")
        .await
        .expect("counts read");
    assert_eq!(counts, (1, 1, 1));
    let counts = fixture
        .catalog
        .favorite_counts("nobody")
        .await
        .expect("counts read");
    assert_eq!(counts, (0, 0, 0));
}

/// One artist's albums, one of each kind the album rules tell apart: a live
/// album with files, one merged into it (with its own identity and an
/// indexed track), one whose only track is missing, one whose only track
/// is excluded, one with no tracks, and two copies of one release group,
/// the older without files. One track carries only the id in its tags, one
/// a match that differs from its tags.
const ALBUM_KINDS: &str = "INSERT INTO local_artists (id, display_name, folded_name, kind, \
    created_at, updated_at) VALUES ('ar-aurora', 'Aurora', 'aurora', 'group', 1, 1); \
    INSERT INTO local_artist_external_identities (local_artist_id, provider, provider_artist_id, \
    decision_source, selected_at) VALUES ('ar-aurora', 'musicbrainz', 'mb-aurora', 'manual', 1); \
    INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_name, \
    album_artist_name_folded, album_artist_id, year, grouping_source, created_at, updated_at) VALUES \
    ('al-live', 'root', 'k-live', 'Set Live', 'set live', 'Aurora', 'aurora', 'ar-aurora', 1994, 'automatic', 10, 10), \
    ('al-retired', 'root', 'k-retired', 'Set Retired', 'set retired', 'Aurora', 'aurora', 'ar-aurora', 1995, 'automatic', 20, 20), \
    ('al-missing', 'root', 'k-missing', 'Set Missing', 'set missing', 'Aurora', 'aurora', 'ar-aurora', 1996, 'automatic', 30, 30), \
    ('al-excluded', 'root', 'k-excluded', 'Set Excluded', 'set excluded', 'Aurora', 'aurora', 'ar-aurora', 1997, 'automatic', 40, 40), \
    ('al-empty', 'root', 'k-empty', 'Set Empty', 'set empty', 'Aurora', 'aurora', 'ar-aurora', 1998, 'automatic', 50, 50), \
    ('al-pair-a', 'root', 'k-pair-a', 'Set Pair', 'set pair', 'Aurora', 'aurora', 'ar-aurora', 1999, 'automatic', 60, 60), \
    ('al-pair-b', 'root', 'k-pair-b', 'Set Pair', 'set pair', 'Aurora', 'aurora', 'ar-aurora', 1999, 'automatic', 70, 70); \
    UPDATE local_albums SET retired_into_album_id = 'al-live' WHERE id = 'al-retired'; \
    INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
    SELECT id, 0, 'ar-aurora', 'main' FROM local_albums WHERE id LIKE 'al-%'; \
    INSERT INTO local_album_external_identities (local_album_id, provider, release_group_mbid, \
    decision_source, selected_at) VALUES ('al-live', 'musicbrainz', 'rg-live', 'manual', 1), \
    ('al-retired', 'musicbrainz', 'rg-retired', 'manual', 1), \
    ('al-missing', 'musicbrainz', 'rg-missing', 'manual', 1), \
    ('al-excluded', 'musicbrainz', 'rg-excluded', 'manual', 1), \
    ('al-empty', 'musicbrainz', 'rg-empty', 'manual', 1), \
    ('al-pair-a', 'musicbrainz', 'rg-pair', 'manual', 1), \
    ('al-pair-b', 'musicbrainz', 'rg-pair', 'manual', 1); \
    INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
    file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, artist_name, \
    artist_name_folded, album_title, album_title_folded, album_artist_name, \
    album_artist_name_folded, track_number, duration_seconds, file_format, availability, \
    embedded_recording_mbid, ingest_source, imported_at, membership_source) VALUES \
    ('tk-matched', 'al-live', 'root', '/m/1.flac', '1.flac', 'h1', 1000, 1, 's1', 'Matched', 'matched', \
     'Aurora', 'aurora', 'Set Live', 'set live', 'Aurora', 'aurora', 1, 200.0, 'flac', 'indexed', \
     'rec-tagged-other', 'scan', 1, 'automatic'), \
    ('tk-tagged', 'al-live', 'root', '/m/2.flac', '2.flac', 'h2', 1000, 1, 's2', 'Tagged', 'tagged', \
     'Aurora', 'aurora', 'Set Live', 'set live', 'Aurora', 'aurora', 2, 200.0, 'flac', 'indexed', \
     'rec-tagged', 'scan', 1, 'automatic'), \
    ('tk-retired', 'al-retired', 'root', '/m/3.flac', '3.flac', 'h3', 1000, 1, 's3', 'Old', 'old', \
     'Aurora', 'aurora', 'Set Retired', 'set retired', 'Aurora', 'aurora', 1, 200.0, 'flac', 'indexed', \
     NULL, 'scan', 1, 'automatic'), \
    ('tk-missing', 'al-missing', 'root', '/m/4.flac', '4.flac', 'h4', 1000, 1, 's4', 'Gone', 'gone', \
     'Aurora', 'aurora', 'Set Missing', 'set missing', 'Aurora', 'aurora', 1, 200.0, 'flac', 'missing', \
     NULL, 'scan', 1, 'automatic'), \
    ('tk-excluded', 'al-excluded', 'root', '/m/5.flac', '5.flac', 'h5', 1000, 1, 's5', 'Out', 'out', \
     'Aurora', 'aurora', 'Set Excluded', 'set excluded', 'Aurora', 'aurora', 1, 200.0, 'flac', 'excluded', \
     NULL, 'scan', 1, 'automatic'), \
    ('tk-pair', 'al-pair-b', 'root', '/m/6.flac', '6.flac', 'h6', 1000, 1, 's6', 'Kept', 'kept', \
     'Aurora', 'aurora', 'Set Pair', 'set pair', 'Aurora', 'aurora', 1, 200.0, 'flac', 'indexed', \
     NULL, 'scan', 1, 'automatic'); \
    INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
    SELECT id, 0, 'ar-aurora', 'main' FROM local_tracks WHERE id LIKE 'tk-%'; \
    INSERT INTO local_track_external_identities (local_track_id, provider, recording_mbid, \
    decision_source, selected_at) VALUES ('tk-matched', 'musicbrainz', 'rec-matched', 'manual', 1); \
    INSERT INTO new_release_feed (release_group_mbid_lower, release_group_mbid, artist_mbid_lower, \
    artist_name, title, first_release_date, discovered_at) \
    SELECT release_group_mbid, release_group_mbid, 'mb-aurora', 'Aurora', release_group_mbid, \
    '2020-01-01', 1 FROM local_album_external_identities GROUP BY release_group_mbid;";

/// Ids of a JSON list of objects, under `key`, sorted.
fn sorted_ids(items: &Value, key: &str) -> Vec<String> {
    let mut ids: Vec<String> = items
        .as_array()
        .unwrap_or_else(|| panic!("a list: {items}"))
        .iter()
        .map(|item| item[key].as_str().expect("an id").to_owned())
        .collect();
    ids.sort();
    ids
}

/// A merged album appears nowhere and its id answers for the survivor; an
/// album without files shows on the web with no tracks but counts as owned
/// nowhere and never reaches a player app; a track's recording id is its
/// match's, else its tags'.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_album_set_everywhere() {
    use crate::common::compat::{CompatApp, PLAYER, named, rest};
    use droppedneedle_catalog::reads::discover::page::library::LibraryReads;

    let rig = CompatApp::unscanned("reads-library-album-set").await;
    rig.app
        .db()
        .execute_batch(ALBUM_KINDS)
        .expect("album kinds seed");
    let player_id: String = rig
        .app
        .db()
        .query_row(
            "SELECT id FROM auth_users WHERE username = ?1",
            [PLAYER],
            |row| row.get(0),
        )
        .expect("player row");
    rig.app
        .db()
        .execute(
            "INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
             artist_name, followed_at, updated_at) VALUES (?1, 'mb-aurora', 'mb-aurora', \
             'Aurora', 1, 1)",
            [&player_id],
        )
        .expect("follow seeds");
    let (status, login) = rig
        .app
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(serde_json::json!({
                "username": PLAYER,
                "password": format!("{PLAYER}-password-1234"),
                "transport": "bearer",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{login}");
    let bearer = format!("Bearer {}", login["token"].as_str().expect("token"));
    let native = [("authorization", bearer.as_str())];
    let auth = rig.subsonic_auth("album-set");
    let subsonic = |call: &str| rest(call, &auth);

    // The web list: every live album, the ones without files with no tracks.
    let (status, page) = rig
        .app
        .call("GET", "/api/v3/library/albums?limit=200", &native, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        sorted_ids(&page["items"], "id"),
        [
            "al-empty",
            "al-excluded",
            "al-live",
            "al-missing",
            "al-pair-a",
            "al-pair-b"
        ]
    );
    assert_eq!(page["total"], 6, "{page}");
    for item in page["items"].as_array().expect("items") {
        let has_files = matches!(item["id"].as_str(), Some("al-live" | "al-pair-b"));
        assert_eq!(item["track_count"] != 0, has_files, "{item}");
    }

    // A merged album's id answers for the album it merged into.
    let (status, album) = rig
        .app
        .call("GET", "/api/v3/library/albums/al-retired", &native, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{album}");
    assert_eq!(album["id"], "al-live", "{album}");
    let (status, tracks) = rig
        .app
        .call(
            "GET",
            "/api/v3/library/albums/match/al-retired",
            &native,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{tracks}");
    assert_eq!(
        sorted_ids(&tracks["items"], "id"),
        ["tk-matched", "tk-tagged"]
    );

    // The status of a group held only by a copy without files names that
    // copy, not in library, so its album page still reaches it.
    let (status, held) = rig
        .app
        .call(
            "GET",
            "/api/v3/library/albums/rg-missing/status",
            &native,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(
        (&held["album_id"], &held["in_library"]),
        (&Value::from("al-missing"), &Value::from(false)),
        "{held}"
    );

    // A release group answers with the copy that has files.
    let (status, tracks) = rig
        .app
        .call("GET", "/api/v3/library/albums/match/rg-pair", &native, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{tracks}");
    assert_eq!(sorted_ids(&tracks["items"], "id"), ["tk-pair"]);

    // A track's recording id: its match's, else its tags'.
    let (status, tracks) = rig
        .app
        .call(
            "GET",
            "/api/v3/library/albums/al-live/tracks",
            &native,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{tracks}");
    assert_eq!(
        named(&tracks["items"], "id", "tk-matched")["recording_mbid"],
        "rec-matched"
    );
    assert_eq!(
        named(&tracks["items"], "id", "tk-tagged")["recording_mbid"],
        "rec-tagged"
    );

    // Owned: the library half of search, membership, follows and Discover.
    let (status, found) = rig
        .app
        .call("GET", "/api/v3/search?q=set&buckets=albums", &native, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let library_hits: Vec<Value> = found["albums"]
        .as_array()
        .expect("album hits")
        .iter()
        .filter(|hit| hit["id"].is_string())
        .cloned()
        .collect();
    assert_eq!(
        sorted_ids(&Value::Array(library_hits), "id"),
        ["al-live", "al-pair-b"]
    );
    let (status, membership) = rig
        .app
        .call(
            "POST",
            "/api/v3/library/membership",
            &native,
            Some(serde_json::json!({"album_ids": [
                "rg-live", "rg-retired", "rg-missing", "rg-excluded", "rg-empty", "rg-pair"
            ]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{membership}");
    assert_eq!(
        membership["owned_ids"],
        serde_json::json!(["rg-live", "rg-pair"])
    );
    let not_owned = ["rg-empty", "rg-excluded", "rg-missing", "rg-retired"];
    let (status, releases) = rig
        .app
        .call("GET", "/api/v3/following/new-releases", &native, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{releases}");
    assert_eq!(
        sorted_ids(&releases["items"], "release_group_mbid"),
        not_owned
    );
    let pool = SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&rig.app.config.library_db_path)
            .read_only(true),
    )
    .await
    .expect("reader opens");
    let mut discover: Vec<String> = LibraryReads::new(pool.clone())
        .followed_releases(&player_id, 50)
        .await
        .expect("followed releases read")
        .into_iter()
        .map(|release| release.release_group_mbid)
        .collect();
    discover.sort();
    assert_eq!(discover, not_owned);
    pool.close().await;

    // Player apps: owned albums only, listed, by id and in artist counts.
    let subsonic_json = |body: Vec<u8>| -> Value {
        let json: Value = serde_json::from_slice(&body).expect("json answer");
        json["subsonic-response"].clone()
    };
    let (status, _, body) = rig
        .app
        .raw(
            "GET",
            &subsonic("getAlbumList2?type=alphabeticalByName&size=500"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sorted_ids(&subsonic_json(body)["albumList2"]["album"], "id"),
        ["al-al-live", "al-al-pair-b"]
    );
    for id in ["al-al-empty", "al-al-pair-a"] {
        let (_, _, body) = rig
            .app
            .raw("GET", &subsonic(&format!("getAlbum?id={id}")), &[], None)
            .await;
        let answer = subsonic_json(body);
        assert_eq!(answer["error"]["code"], 70, "{id}: {answer}");
    }
    let (_, _, body) = rig
        .app
        .raw("GET", &subsonic("getAlbum?id=al-al-live"), &[], None)
        .await;
    let album = subsonic_json(body);
    let songs = &album["album"]["song"];
    assert_eq!(
        named(songs, "id", "tr-tk-matched")["musicBrainzId"],
        "rec-matched"
    );
    assert_eq!(
        named(songs, "id", "tr-tk-tagged")["musicBrainzId"],
        "rec-tagged"
    );
    let (_, _, body) = rig.app.raw("GET", &subsonic("getArtists"), &[], None).await;
    let artists = subsonic_json(body);
    let aurora = artists["artists"]["index"]
        .as_array()
        .expect("artist index")
        .iter()
        .flat_map(|index| index["artist"].as_array().cloned().unwrap_or_default())
        .find(|artist| artist["name"] == "Aurora")
        .unwrap_or_else(|| panic!("Aurora listed: {artists}"));
    assert_eq!(aurora["albumCount"], 2, "{aurora}");

    rig.stop().await;
}
