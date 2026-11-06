//! Library reads over the real stores on a migrated template database:
//! input validation and fault hiding over the routes, and
//! the catalog, favorites and lookups reads, including a missing and an
//! excluded track proving streamable-only counts.

use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use crate::common::{ScratchDir, TestApp};
use droppedneedle_catalog::local;
use droppedneedle_catalog::lyrics::LyricsService;

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_kernel::db::Db;
use droppedneedle_library::scan::coordinator::SharedResolver;
use droppedneedle_media::stream::local_files::LibraryFiles;
use local::LibraryDeps;
use local::store::LocalCatalog;
use local::stores::{
    After, AlbumFilter, AlbumSort, ArtistScope, ArtistSort, BoxFuture, LyricDoc, LyricsPort,
    StoreError, TrackFilter, TrackSort,
};
use serde_json::Value;
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
/// lyrics route, or the production lyrics without an online lookup. The
/// cover cache, the download policy and the users come from `beside`;
/// the metadata catalog holds no releases and calls no provider.
fn deps(beside: &AuthRig, db: &Db, lyrics: Option<Arc<dyn LyricsPort>>) -> LibraryDeps {
    let catalog = LocalCatalog::new(db.clone());
    let files = LibraryFiles::new(db.clone(), SharedResolver::default());
    let releases = crate::common::bundles::offline_catalog(db, beside.deps.clone());
    let covers = droppedneedle_catalog::wiring::artwork(
        &beside.platform,
        &crate::common::catalog_inputs(&beside.platform),
        releases.clone(),
    );
    LibraryDeps {
        lyrics: lyrics.unwrap_or_else(|| {
            Arc::new(LyricsService::new(
                catalog.clone(),
                files.clone(),
                Arc::new(crate::common::bundles::NoOnlineLyrics),
            ))
        }),
        catalog,
        upgrade_policy: beside.config.live(),
        covers,
        releases,
    }
}

/// The signed-in user the routes run as.
const ADA: (&str, Role) = ("user-ada", Role::User);

/// Router signed in as `caller`, or anonymous when None.
fn app(deps: LibraryDeps, caller: Option<(&str, Role)>) -> Router {
    let router = local::routes().with_state(deps).into_router();
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

/// Every library route with ids the seeded catalog holds. The fault loop
/// walks this.
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
    "/api/v3/library/tracks/t1/lyrics",
    "/api/v3/library/stats",
    "/api/v3/library/recently-added",
    "/api/v3/library/albums/match/rg1",
    "/api/v3/library/decades",
    "/api/v3/library/suggestions",
];

#[tokio::test]
async fn bad_input_stays_in_the_envelope() {
    let fixture = sqlite_fixture().await;
    let beside = AuthRig::new("reads-library").await;
    let app = app(
        deps(
            &beside,
            fixture.runtime.db(),
            Some(Arc::new(scripted_lyrics())),
        ),
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
async fn every_route_hides_store_faults() {
    let scope = TestApp::start("reads-library-faults").await;
    // A database without the catalog's tables: every store read fails.
    let dir = ScratchDir::new("reads-library-faults");
    let secret = dir.join("secret-library.db");
    std::fs::write(&secret, b"").expect("empty database file writes");
    let db = Db::open_for_tests(&secret).expect("empty database opens");
    let beside = AuthRig::new("reads-library-faults").await;
    let app = scope.request_scope(app(deps(&beside, &db, None), Some(ADA)));
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
          'Boreal', 'boreal', 'Lone Peak', 'lone peak', 'Boreal', 'boreal', 1, 1, 1994, 220.0, 'flac', 'indexed', 'scan', 1500.0, 'automatic'), \
         ('tg4', 'al4', 'r1', '/m/tg4.flac', 'tg4.flac', 'hg4', 4000000, 1, 'sg4', 'Lost', 'lost', \
          'Aurora', 'aurora', 'First Light (Deluxe)', 'first light (deluxe)', 'Aurora', 'aurora', 1, 1, 1994, 200.0, 'flac', 'missing', 'scan', 1200.0, 'automatic')",
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
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'U', 'user', 1)",
        "INSERT INTO catalog_user_favorites (user_id, item_kind, item_id, created_at) VALUES \
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

/// The store reads the seeded catalog back: album lists with their counts,
/// identity and filters, sorts and the release group lookup, an album's
/// tracks and copies, artist scopes and credits, track filters and stats,
/// genres, decades and suggestion pools, favourites, and the ALAC name.
#[tokio::test]
async fn the_seeded_catalog_reads_back() {
    let fixture = sqlite_fixture().await;

    // Albums: streamable counts, identity and filters.
    let filter = AlbumFilter::default();
    let (albums, total) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Name, false, 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("albums list");
    assert_eq!(total, 4, "albums");
    let first = albums
        .iter()
        .find(|album| album.id == "al1")
        .expect("al1 listed");
    assert_eq!(first.track_count, 2, "al1");
    assert_eq!(first.total_duration_seconds, 400.0, "al1");
    assert_eq!(first.total_size_bytes, 8_000_000, "al1");
    assert_eq!(first.format.as_deref(), Some("flac"), "al1");
    assert_eq!(first.release_group_mbid.as_deref(), Some("rg1"), "al1");
    assert_eq!(first.artist_mbid.as_deref(), Some("m1"), "al1");
    assert!(first.linked, "al1");
    assert!(first.cover_available, "al1");
    let second = albums
        .iter()
        .find(|album| album.id == "al2")
        .expect("al2 listed");
    assert_eq!(second.track_count, 1, "al2");
    assert!(!second.linked, "al2");

    assert!(
        fixture
            .catalog
            .get_album("nope")
            .await
            .expect("lookup runs")
            .is_none(),
        "unknown album"
    );

    for (filter, want, case) in [
        (
            AlbumFilter {
                q: Some("peak".to_owned()),
                ..Default::default()
            },
            1,
            "q filter",
        ),
        (
            AlbumFilter {
                decade: Some(1990),
                ..Default::default()
            },
            3,
            "decade filter",
        ),
        (
            AlbumFilter {
                artist_id: Some("a1".to_owned()),
                ..Default::default()
            },
            3,
            "artist filter",
        ),
    ] {
        let (_, total) = fixture
            .catalog
            .list_albums(&filter, AlbumSort::Name, false, 50, &After::default())
            .await
            .map(|page| (page.items, page.total))
            .expect(case);
        assert_eq!(total, want, "{case}");
    }

    // The artist sort, both ways, and the release group lookup.
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
        ["al1", "al4", "al2", "al3"],
        "artist sort"
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
        ["al3", "al2", "al4", "al1"],
        "artist sort desc"
    );

    let found = fixture
        .catalog
        .get_album_by_release_group("rg1")
        .await
        .expect("lookup runs")
        .expect("rg1 resolves");
    assert_eq!(found.id, "al1", "release group lookup");
    assert!(
        fixture
            .catalog
            .get_album_by_release_group("nope")
            .await
            .expect("lookup runs")
            .is_none(),
        "unknown release group"
    );

    // An album's tracks and its copies.
    let (tracks, total) = fixture
        .catalog
        .album_tracks("al1", 50, 0)
        .await
        .expect("tracks list");
    assert_eq!(total, 2, "album tracks");
    assert_eq!(
        tracks
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        ["t1", "t2"],
        "album tracks"
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
        ["al4"],
        "al1 copies"
    );
    let copies = fixture
        .catalog
        .album_copies("al2")
        .await
        .expect("copies list");
    assert!(copies.is_empty(), "al2 copies");

    // Artists: scopes and credits. Every credited artist is listed; the
    // retired row and the migration's uncredited Various/Unknown sentinels
    // stay out.
    let (artists, total, led, contributors) = fixture
        .catalog
        .list_artists(ArtistScope::All, None, ArtistSort::Name, false, 50, 0)
        .await
        .expect("artists list");
    assert_eq!((total, led, contributors), (3, 2, 1), "artists");
    assert!(!artists.iter().any(|artist| artist.id == "ax"), "artists");

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
    assert_eq!(total, 1, "contributors");
    assert_eq!(scoped[0].id, "a3", "contributors");

    let a1 = fixture
        .catalog
        .get_artist("a1")
        .await
        .expect("lookup runs")
        .expect("a1 found");
    // al4 has no files, so it is not counted as one of a1's albums.
    assert_eq!(
        (
            a1.album_count,
            a1.track_count,
            a1.led_track_count,
            a1.appearance_album_count
        ),
        (2, 3, 3, 0),
        "a1"
    );
    assert_eq!(a1.artist_mbid.as_deref(), Some("m1"), "a1");

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
            a3.led_track_count,
            a3.appearance_album_count,
            a3.appearance_track_count
        ),
        (0, 1, 0, 1, 1),
        "a3"
    );

    let (led_albums, total) = fixture
        .catalog
        .led_albums("a1", 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("led list");
    assert_eq!(total, 3, "led albums");
    assert_eq!(led_albums[0].id, "al1", "led albums");

    let (appearances, total) = fixture
        .catalog
        .artist_appearances("a3", 50, &After::default())
        .await
        .map(|page| (page.items, page.total))
        .expect("appearances list");
    assert_eq!(total, 1, "appearances");
    assert_eq!(appearances[0].id, "al3", "appearances");

    // Tracks: filters and stats.
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
    assert_eq!(total, 1, "track q filter");
    assert_eq!(tracks[0].id, "t4", "track q filter");
    assert_eq!(tracks[0].genre.as_deref(), Some("Jazz"), "track q filter");

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
    assert_eq!(total, 2, "track genre filter");

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
    assert_eq!(total, 1, "track artist filter");
    assert_eq!(tracks[0].id, "t6", "track artist filter");

    assert!(
        fixture
            .catalog
            .get_track("t3")
            .await
            .expect("lookup runs")
            .is_none(),
        "a missing track"
    );

    let stats = fixture.catalog.stats().await.expect("stats read");
    assert_eq!(
        (stats.total_albums, stats.total_artists, stats.total_tracks),
        (3, 3, 4),
        "the Artists card counts what the Artists page lists, not uncredited rows"
    );
    assert_eq!(stats.total_size_bytes, 16_000_000, "stats");
    assert_eq!(stats.format_breakdown.get("flac"), Some(&3), "stats");
    assert_eq!(stats.format_breakdown.get("mp3"), Some(&1), "stats");

    // Genres, decades and suggestion pools.
    let genres = fixture.catalog.genres().await.expect("genres list");
    assert_eq!(genres.len(), 2, "genres");
    assert_eq!(genres[0].folded_name, "rock", "genres");
    assert_eq!(
        (genres[0].track_count, genres[0].album_count),
        (2, 1),
        "genres"
    );

    let decades = fixture.catalog.decades().await.expect("decades list");
    assert_eq!(decades.len(), 2, "decades");
    assert_eq!(
        (decades[0].decade, decades[0].album_count),
        (1990, 2),
        "decades"
    );
    assert_eq!(
        (decades[1].decade, decades[1].album_count),
        (2000, 1),
        "decades"
    );

    let newest = fixture.catalog.newest_tracks(2).await.expect("newest");
    assert_eq!(
        newest
            .iter()
            .map(|track| track.id.as_str())
            .collect::<Vec<_>>(),
        ["t4", "t6"],
        "newest"
    );
    let oldest = fixture.catalog.oldest_tracks(1).await.expect("oldest");
    assert_eq!(oldest[0].id, "t1", "oldest");
    let era = fixture
        .catalog
        .random_tracks(10, Some(1990))
        .await
        .expect("era random");
    assert!(!era.is_empty(), "era");
    assert!(
        era.iter()
            .all(|track| track.year.is_some_and(|year| (1990..2000).contains(&year))),
        "era"
    );
    assert!(
        era.iter().all(|track| track.id != "t3" && track.id != "t5"),
        "era"
    );

    // Favourites.
    let ids = ["al1".to_owned(), "al2".to_owned()];
    let marked = fixture
        .catalog
        .filter_favorites("u1", "album", &ids)
        .await
        .expect("filter runs");
    assert_eq!(
        marked,
        std::collections::HashSet::from(["al1".to_owned()]),
        "favourites"
    );
    let counts = fixture
        .catalog
        .favorite_counts("u1")
        .await
        .expect("counts read");
    assert_eq!(counts, (1, 1, 1), "favourites");
    let counts = fixture
        .catalog
        .favorite_counts("nobody")
        .await
        .expect("counts read");
    assert_eq!(counts, (0, 0, 0), "no favourites");

    // A web player picks its transcode by this name: an MP4 file with a bit
    // depth is ALAC, and one without stays what it is. Last: it rewrites
    // t1 and t2.
    seed(
        &fixture,
        "UPDATE local_tracks SET file_format = 'm4a', bit_depth = 16 WHERE id = 't1'; \
         UPDATE local_tracks SET file_format = 'm4a' WHERE id = 't2'",
    )
    .await;
    let (tracks, _) = fixture
        .catalog
        .album_tracks("al1", 50, 0)
        .await
        .expect("tracks list");
    assert_eq!(
        tracks
            .iter()
            .map(|track| track.format.as_str())
            .collect::<Vec<_>>(),
        ["alac", "m4a"],
        "alac"
    );
}

/// The album and artist pages over the seeded catalog: an album page
/// carries its row revision (what a stale Apply is refused by) and its
/// pending review, appearances answer by MusicBrainz id as by library id,
/// and albums tell how many tracks their edition has.
#[tokio::test]
async fn album_and_artist_pages_answer_the_seeded_catalog() {
    let fixture = sqlite_fixture().await;
    let beside = AuthRig::new("reads-library").await;
    let deps = deps(&beside, fixture.runtime.db(), None);

    // Appearances by MusicBrainz id as by library id.
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
    assert_eq!(by_mbid, by_id, "appearances by MusicBrainz id");
    let (status, body) = get(
        app(deps.clone(), Some(ADA)),
        "/api/v3/library/artists/no-such-artist/appearances",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    // A suggested track names its credited artist, so the player links it.
    let (status, body) = get(app(deps.clone(), Some(ADA)), "/api/v3/library/suggestions").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let peak = body["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["track_id"] == "t6"))
        .expect("t6 suggested");
    assert_eq!(peak["artist_id"], "a2", "{peak}");

    // An album page's revision and pending review.
    let uri = "/api/v3/library/albums/al2";
    let (status, body) = get(app(deps.clone(), Some(ADA)), uri).await;
    assert_eq!(status, 200, "album page");
    assert_eq!(body["row_revision"], 1, "album page");
    // Its seeded track joining moved it once.
    assert_eq!(body["listing_revision"], 1, "album page");
    assert!(body["pending_review_id"].is_null(), "album page");
    seed(
        &fixture,
        "UPDATE local_albums SET row_revision = 7 WHERE id = 'al2'; \
         INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
         candidates_json, state, created_at, updated_at) VALUES \
         ('done', 'al2', 'r', '[]', 'approved', 3, 3), \
         ('open', 'al2', 'r', '[]', 'pending', 2, 2)",
    )
    .await;
    let (_, body) = get(app(deps.clone(), Some(ADA)), uri).await;
    assert_eq!(body["row_revision"], 7, "album page after a write");
    assert_eq!(
        body["pending_review_id"], "open",
        "album page after a write"
    );

    // How many tracks an album's edition has: with no MusicBrainz release
    // held, from its files' tags; an album no tag tells about has none. The
    // list and the album agree.
    seed(
        &fixture,
        "INSERT INTO local_track_edition_tags (local_track_id, total_discs, total_tracks) \
         VALUES ('t1', 1, 12), ('t2', 1, 12)",
    )
    .await;
    let (status, page) = get(
        app(deps.clone(), Some(ADA)),
        "/api/v3/library/albums?limit=50",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let counts: Vec<(String, u64, Value)> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|album| {
            (
                album["id"].as_str().unwrap().to_owned(),
                album["track_count"].as_u64().unwrap(),
                album["expected_track_count"].clone(),
            )
        })
        .filter(|(id, _, _)| id == "al1" || id == "al3")
        .collect();
    assert_eq!(
        counts,
        [
            ("al1".to_owned(), 2, serde_json::json!(12)),
            ("al3".to_owned(), 1, Value::Null)
        ],
        "edition track counts"
    );
    let (status, album) = get(app(deps, Some(ADA)), "/api/v3/library/albums/al1").await;
    assert_eq!(status, StatusCode::OK, "{album}");
    assert_eq!(
        album["expected_track_count"],
        serde_json::json!(12),
        "edition track count"
    );
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
     '9b2c4f10-0000-4000-8000-0000000000a3', 'scan', 1, 'automatic'), \
    ('tk-tagged', 'al-live', 'root', '/m/2.flac', '2.flac', 'h2', 1000, 1, 's2', 'Tagged', 'tagged', \
     'Aurora', 'aurora', 'Set Live', 'set live', 'Aurora', 'aurora', 2, 200.0, 'flac', 'indexed', \
     '9b2c4f10-0000-4000-8000-0000000000a2', 'scan', 1, 'automatic'), \
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
    decision_source, selected_at) VALUES ('tk-matched', 'musicbrainz', '9b2c4f10-0000-4000-8000-0000000000a1', 'manual', 1); \
    INSERT INTO acquire_new_release_feed (release_group_mbid_lower, release_group_mbid, artist_mbid_lower, \
    artist_name, title, first_release_date, discovered_at) \
    SELECT release_group_mbid, release_group_mbid, 'mb-aurora', 'Aurora', release_group_mbid, \
    date('now'), 1 FROM local_album_external_identities GROUP BY release_group_mbid;";

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
    use droppedneedle_catalog::discover::page::library::LibraryReads;

    let rig = CompatApp::unscanned("reads-library-album-set").await;
    rig.app.seed(ALBUM_KINDS).await;
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
        .seed_with(
            "INSERT INTO acquire_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
             artist_name, followed_at, updated_at) VALUES (?1, 'mb-aurora', 'mb-aurora', \
             'Aurora', 1, 1)",
            [&player_id],
        )
        .await;
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

    // The web list: every live album with a track row, the ones without
    // files with no tracks.
    let (status, page) = rig
        .app
        .call("GET", "/api/v3/library/albums?limit=200", &native, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        sorted_ids(&page["items"], "id"),
        ["al-excluded", "al-live", "al-missing", "al-pair-b"]
    );
    assert_eq!(page["total"], 4, "{page}");
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
    // A track left on a merged album is not searched, listed or returned.
    let (status, found) = rig
        .app
        .call("GET", "/api/v3/library/tracks?q=old", &native, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["total"], 0, "{found}");
    let (status, listed) = rig
        .app
        .call("GET", "/api/v3/library/tracks?limit=200", &native, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(
        sorted_ids(&listed["items"], "id"),
        ["tk-matched", "tk-pair", "tk-tagged"]
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
        "9b2c4f10-0000-4000-8000-0000000000a1"
    );
    assert_eq!(
        named(&tracks["items"], "id", "tk-tagged")["recording_mbid"],
        "9b2c4f10-0000-4000-8000-0000000000a2"
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
        .call(
            "GET",
            "/api/v3/following/new-releases/recent?days=365&limit=50&include_owned=false",
            &native,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{releases}");
    assert_eq!(
        sorted_ids(&releases["items"], "release_group_mbid"),
        not_owned
    );
    let mut discover: Vec<String> = LibraryReads::new(rig.app.state().library.db.clone())
        .followed_releases(&player_id, 50)
        .await
        .expect("followed releases read")
        .into_iter()
        .map(|release| release.release_group_mbid)
        .collect();
    discover.sort();
    assert_eq!(discover, not_owned);

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
        "9b2c4f10-0000-4000-8000-0000000000a1"
    );
    assert_eq!(
        named(songs, "id", "tr-tk-tagged")["musicBrainzId"],
        "9b2c4f10-0000-4000-8000-0000000000a2"
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

/// An artist leading an album of its own who also appears on someone
/// else's is listed under both tabs, each filtered by its own count: the
/// led tracks for album artists, the appearances for contributors.
#[tokio::test]
async fn an_album_artist_who_appears_elsewhere_is_also_a_contributor() {
    let fixture = sqlite_fixture().await;
    seed(
        &fixture,
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) VALUES \
         ('a4', 'Atb', 'atb', 'person', 1700, 1700);
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, \
         grouping_source, created_at, updated_at) VALUES \
         ('al5', 'r1', 'g5', 'Own One', 'own one', 'Atb', 'atb', 'a4', 2000, 'automatic', 1700, 1700), \
         ('al6', 'r1', 'g6', 'Guest Spots', 'guest spots', 'Boreal', 'boreal', 'a2', 2002, 'automatic', 1700, 1700);
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, \
         file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, artist_name, artist_name_folded, \
         album_title, album_title_folded, album_artist_name, album_artist_name_folded, \
         disc_number, track_number, year, duration_seconds, file_format, availability, \
         ingest_source, imported_at, membership_source) VALUES \
         ('t7', 'al5', 'r1', '/m/t7.flac', 't7.flac', 'h7', 4000000, 1, 's7', 'Own', 'own', \
          'Atb', 'atb', 'Own One', 'own one', 'Atb', 'atb', 1, 1, 2000, 200.0, 'flac', 'indexed', 'scan', 1700.0, 'automatic'), \
         ('t8', 'al6', 'r1', '/m/t8.flac', 't8.flac', 'h8', 4000000, 1, 's8', 'One', 'one', \
          'Boreal', 'boreal', 'Guest Spots', 'guest spots', 'Boreal', 'boreal', 1, 1, 2002, 200.0, 'flac', 'indexed', 'scan', 1700.0, 'automatic'), \
         ('t9', 'al6', 'r1', '/m/t9.flac', 't9.flac', 'h9', 4000000, 1, 's9', 'Two', 'two', \
          'Boreal', 'boreal', 'Guest Spots', 'guest spots', 'Boreal', 'boreal', 1, 2, 2002, 200.0, 'flac', 'indexed', 'scan', 1700.0, 'automatic'), \
         ('t10', 'al6', 'r1', '/m/t10.flac', 't10.flac', 'h10', 4000000, 1, 's10', 'Three', 'three', \
          'Boreal', 'boreal', 'Guest Spots', 'guest spots', 'Boreal', 'boreal', 1, 3, 2002, 200.0, 'flac', 'indexed', 'scan', 1700.0, 'automatic');
         INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) VALUES \
         ('t7', 0, 'a4', 'main'), \
         ('t8', 0, 'a2', 'main'), ('t8', 1, 'a4', 'guest'), \
         ('t9', 0, 'a2', 'main'), ('t9', 1, 'a4', 'guest'), \
         ('t10', 0, 'a2', 'main'), ('t10', 1, 'a4', 'guest');
         INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) VALUES \
         ('al5', 0, 'a4', 'main'), ('al6', 0, 'a2', 'main');",
    )
    .await;
    let beside = AuthRig::new("reads-library-contributors").await;
    let app = || app(deps(&beside, fixture.runtime.db(), None), Some(ADA));

    for (query, want, totals) in [
        ("scope=album_artists", vec!["a1", "a2", "a4"], (3, 2)),
        ("scope=contributors", vec!["a3", "a4"], (3, 2)),
        ("scope=album_artists&min_tracks=2", vec!["a1", "a2"], (2, 1)),
        ("scope=contributors&min_tracks=2", vec!["a4"], (2, 1)),
        // Boreal leads four tracks; Atb leads one and appears on three.
        ("scope=album_artists&min_tracks=4", vec!["a2"], (1, 0)),
        ("scope=contributors&min_tracks=4", vec![], (1, 0)),
    ] {
        let (status, page) = get(app(), &format!("/api/v3/library/artists?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(sorted_ids(&page["items"], "id"), want, "{query}");
        assert_eq!(page["total"], serde_json::json!(want.len()), "{query}");
        assert_eq!(
            (&page["album_artist_total"], &page["contributor_total"]),
            (&serde_json::json!(totals.0), &serde_json::json!(totals.1)),
            "{query}"
        );
    }
}

/// Run `sql` on the fixture's database as one write.
/// The album list keeps albums in some formats, or hides them, and drops
/// albums with fewer tracks than asked; the format filter's counts cover
/// the whole list. The artist list drops artists with fewer tracks, in
/// every scope and in the scope totals alike.
#[tokio::test]
async fn lists_filter_by_formats_and_minimum_tracks() {
    let fixture = sqlite_fixture().await;
    let beside = AuthRig::new("reads-library").await;
    let app = || app(deps(&beside, fixture.runtime.db(), None), Some(ADA));
    let ids = |page: &Value| sorted_ids(&page["items"], "id");

    for (query, want) in [
        ("min_tracks=2", vec!["al1"]),
        ("formats=MP3", vec!["al2"]),
        ("formats=flac,mp3", vec!["al1", "al2", "al3"]),
        ("formats=flac&format_mode=without", vec!["al2", "al4"]),
        ("formats=flac&min_tracks=2", vec!["al1"]),
        ("format=mp3", vec!["al2"]),
    ] {
        let (status, page) = get(app(), &format!("/api/v3/library/albums?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(ids(&page), want, "{query}");
        assert_eq!(page["total"], serde_json::json!(want.len()), "{query}");
    }
    let (status, _) = get(app(), "/api/v3/library/albums?format_mode=except").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, formats) = get(app(), "/api/v3/library/albums/formats").await;
    assert_eq!(status, StatusCode::OK, "{formats}");
    assert_eq!(
        formats["items"],
        serde_json::json!([
            {"format": "flac", "album_count": 2},
            {"format": "mp3", "album_count": 1}
        ])
    );

    // An artist passes the minimum when the count its card shows reaches
    // it: an album artist's tracks on the albums it leads, a contributor's
    // appearances. Each tab's total counts the same artists, in every
    // order.
    let reaching = |scope: &str, field: &str, min: u64| {
        let app = app();
        let scope = scope.to_owned();
        let field = field.to_owned();
        async move {
            let (status, page) = get(app, &format!("/api/v3/library/artists?scope={scope}")).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            let mut ids: Vec<String> = page["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|artist| artist[field.as_str()].as_u64().unwrap() >= min)
                .map(|artist| artist["id"].as_str().unwrap().to_owned())
                .collect();
            ids.sort();
            ids
        }
    };
    let mut seen_filtered_out = false;
    for min in 2..=4 {
        let led = reaching("album_artists", "led_track_count", min).await;
        let appearing = reaching("contributors", "appearance_track_count", min).await;
        for (query, want) in [
            (format!("scope=album_artists&min_tracks={min}"), &led),
            (
                format!("scope=album_artists&sort=album_count&order=desc&min_tracks={min}"),
                &led,
            ),
            (format!("scope=contributors&min_tracks={min}"), &appearing),
        ] {
            let (status, page) = get(app(), &format!("/api/v3/library/artists?{query}")).await;
            assert_eq!(status, StatusCode::OK, "{query}: {page}");
            assert_eq!(ids(&page), *want, "{query}");
            assert_eq!(page["total"], serde_json::json!(want.len()), "{query}");
            // The filtered page keeps the unfiltered page's order.
            let in_order = |page: &Value| -> Vec<String> {
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|artist| artist["id"].as_str().unwrap().to_owned())
                    .filter(|id| want.contains(id))
                    .collect()
            };
            let unfiltered = query.replace(&format!("&min_tracks={min}"), "");
            let (status, all) = get(app(), &format!("/api/v3/library/artists?{unfiltered}")).await;
            assert_eq!(status, StatusCode::OK, "{unfiltered}: {all}");
            assert_eq!(in_order(&page), in_order(&all), "{query}");
            assert_eq!(
                (
                    page["album_artist_total"].as_u64().unwrap(),
                    page["contributor_total"].as_u64().unwrap()
                ),
                (led.len() as u64, appearing.len() as u64),
                "{query}"
            );
        }
        let all = reaching("album_artists", "led_track_count", 1).await;
        seen_filtered_out |= led.len() < all.len();
    }
    assert!(
        seen_filtered_out,
        "the fixture has an artist the minimum drops"
    );
}

/// The lists keep what the caller rated within a span of stars, at least
/// or at most so many, and leave out what they did not rate or only
/// someone else did; the track bound reads either way. Each list's total
/// and each artist tab's total count the same items as the page.
#[tokio::test]
async fn lists_filter_by_your_rating_and_by_fewer_tracks() {
    let fixture = sqlite_fixture().await;
    seed(
        &fixture,
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('user-ada', 'Ada', 'user', 1); \
         INSERT INTO catalog_user_ratings (user_id, item_kind, item_id, rating, rated_at) VALUES \
         ('user-ada', 'album', 'al1', 5, 1), ('user-ada', 'album', 'al2', 2, 1), \
         ('user-ada', 'album', 'al3', 4, 1), ('u1', 'album', 'al4', 5, 1), \
         ('user-ada', 'artist', 'a1', 4, 1), ('user-ada', 'artist', 'a2', 1, 1), \
         ('u1', 'artist', 'a3', 5, 1), \
         ('user-ada', 'track', 't1', 5, 1), ('user-ada', 'track', 't2', 3, 1), \
         ('user-ada', 'track', 't6', 1, 1), ('u1', 'track', 't4', 5, 1)",
    )
    .await;
    let beside = AuthRig::new("reads-library-rating").await;
    let app = || app(deps(&beside, fixture.runtime.db(), None), Some(ADA));
    let ids = |page: &Value| sorted_ids(&page["items"], "id");

    for (query, want) in [
        ("min_tracks=2&tracks_mode=below", vec!["al2", "al3", "al4"]),
        ("min_tracks=2&tracks_mode=at_least", vec!["al1"]),
        ("rating=4", vec!["al1", "al3"]),
        ("rating=4&rating_mode=at_least", vec!["al1", "al3"]),
        ("rating=2&rating_mode=at_most", vec!["al2"]),
        ("rating=5", vec!["al1"]),
        ("rating=1", vec!["al1", "al2", "al3"]),
        ("rating=4&min_tracks=2&tracks_mode=below", vec!["al3"]),
        ("rating=1&formats=flac&format_mode=without", vec!["al2"]),
        ("rating=1&sort=name&order=desc", vec!["al1", "al2", "al3"]),
    ] {
        let (status, page) = get(app(), &format!("/api/v3/library/albums?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(ids(&page), want, "{query}");
        assert_eq!(page["total"], serde_json::json!(want.len()), "{query}");
    }

    for (query, want, (led, contributing)) in [
        (
            "scope=album_artists&min_tracks=2&tracks_mode=below",
            vec!["a2"],
            (1, 1),
        ),
        (
            "scope=album_artists&sort=album_count&order=desc&min_tracks=2&tracks_mode=below",
            vec!["a2"],
            (1, 1),
        ),
        (
            "scope=contributors&min_tracks=2&tracks_mode=below",
            vec!["a3"],
            (1, 1),
        ),
        ("scope=album_artists&rating=4", vec!["a1"], (1, 0)),
        (
            "scope=album_artists&sort=album_count&order=desc&rating=4",
            vec!["a1"],
            (1, 0),
        ),
        (
            "scope=album_artists&rating=1&rating_mode=at_most",
            vec!["a2"],
            (1, 0),
        ),
        ("scope=contributors&rating=1", Vec::new(), (2, 0)),
        (
            "scope=album_artists&rating=1&min_tracks=2",
            vec!["a1"],
            (1, 0),
        ),
    ] {
        let (status, page) = get(app(), &format!("/api/v3/library/artists?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(ids(&page), want, "{query}");
        assert_eq!(page["total"], serde_json::json!(want.len()), "{query}");
        assert_eq!(
            (
                page["album_artist_total"].as_u64().unwrap(),
                page["contributor_total"].as_u64().unwrap()
            ),
            (led, contributing),
            "{query}"
        );
    }

    for (query, want) in [
        ("formats=flac", vec!["t1", "t2", "t6"]),
        ("formats=FLAC&format_mode=without", vec!["t4"]),
        ("rating=3", vec!["t1", "t2"]),
        ("rating=3&rating_mode=at_most", vec!["t2", "t6"]),
        ("rating=1&formats=mp3", Vec::new()),
        ("rating=1&q=closer", vec!["t2"]),
    ] {
        let (status, page) = get(app(), &format!("/api/v3/library/tracks?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        assert_eq!(ids(&page), want, "{query}");
        assert_eq!(page["total"], serde_json::json!(want.len()), "{query}");
    }

    for path in [
        "/api/v3/library/albums?rating=6",
        "/api/v3/library/albums?rating=0",
        "/api/v3/library/albums?rating=3&rating_mode=exactly",
        "/api/v3/library/albums?min_tracks=2&tracks_mode=above",
        "/api/v3/library/artists?rating=9",
        "/api/v3/library/artists?min_tracks=2&tracks_mode=fewer",
        "/api/v3/library/tracks?rating=6",
        "/api/v3/library/tracks?formats=flac&format_mode=except",
    ] {
        let (status, body) = get(app(), path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {body}");
    }
}

async fn seed(fixture: &SqliteFixture, sql: &'static str) {
    fixture
        .runtime
        .db()
        .write(
            droppedneedle_kernel::db::Lane::Foreground,
            "library seed",
            move |tx| {
                tx.execute_batch(sql)
                    .map_err(droppedneedle_kernel::db::DbError::from)
            },
        )
        .await
        .expect("seed runs");
}

/// The library holds a release group only while a live copy of it has an
/// indexed track: the landing and the wanted watcher read this to decide
/// whether an album still needs fetching.
#[tokio::test]
async fn library_holds_a_group_only_through_an_indexed_track_on_a_live_copy() {
    use droppedneedle_acquire::edition::library_holds;

    let fixture = sqlite_fixture().await;
    let db = fixture.runtime.db();
    assert!(library_holds(db, "RG1").await.unwrap(), "any case holds");

    seed(
        &fixture,
        "INSERT INTO local_album_external_identities \
         (local_album_id, provider, release_group_mbid, decision_source, selected_at) \
         VALUES ('al3', 'musicbrainz', 'rg3', 'manual', 1500); \
         UPDATE local_tracks SET availability = 'missing' WHERE id = 't6'",
    )
    .await;
    assert!(
        !library_holds(db, "rg3").await.unwrap(),
        "a copy whose files are gone holds nothing"
    );

    seed(
        &fixture,
        "UPDATE local_tracks SET availability = 'indexed' WHERE id = 't6'",
    )
    .await;
    assert!(library_holds(db, "rg3").await.unwrap());

    seed(
        &fixture,
        "UPDATE local_albums SET retired_into_album_id = 'al1' WHERE id = 'al3'",
    )
    .await;
    assert!(
        !library_holds(db, "rg3").await.unwrap(),
        "a merged-away copy holds nothing"
    );
}
