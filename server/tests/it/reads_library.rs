//! Library reads: the auth matrix, input validation and fault hiding over
//! the routes, the same-era suggestion pool, and the SQLite catalog and
//! favorites adapters over a scratch migrated database.

use crate::common::ScratchDir;
use droppedneedle::reads::library;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::UsersDeps;
use droppedneedle::auth::users::memory::TestRig;
use droppedneedle::auth::users::roles::{Role, SessionKind};
use library::memory::{
    FailingCatalog, FailingFavorites, FailingLyrics, MemoryCatalog, MemoryFavorites, MemoryLyrics,
};
use library::models::SuggestionsResponse;
use library::sqlite::{LibraryDb, SqliteCatalog, SqliteFavorites};
use library::stores::{
    AlbumFilter, AlbumRecord, AlbumSort, ArtistRecord, ArtistScope, ArtistSort, FavoriteReads,
    LibraryCatalog, LyricDoc, LyricsPort, TrackFilter, TrackRecord, TrackSort,
};
use library::{LibraryDeps, library_router};
use serde_json::Value;
use tower::ServiceExt as _;

/// Fixed error id asserted in leak envelopes. A valid UUID.
const FIXED_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

/// Id generator returning one fixed value.
#[derive(Debug, Clone)]
struct FixedIdGenerator;

impl droppedneedle::ids::IdGenerator for FixedIdGenerator {
    fn new_id(&self) -> String {
        FIXED_ID.to_owned()
    }
}

fn album(
    id: &str,
    title: &str,
    artist_id: &str,
    artist_name: &str,
    year: Option<i64>,
    added: f64,
) -> AlbumRecord {
    AlbumRecord {
        id: id.to_owned(),
        title: title.to_owned(),
        artist_name: artist_name.to_owned(),
        artist_id: artist_id.to_owned(),
        release_group_mbid: None,
        release_mbid: None,
        artist_mbid: None,
        linked: false,
        track_count: 2,
        total_duration_seconds: 400.0,
        total_size_bytes: 8_000_000,
        format: Some("flac".to_owned()),
        year,
        is_compilation: false,
        cover_available: true,
        date_added: Some(added),
    }
}

fn artist(id: &str, name: &str, albums: u64, tracks: u64, added: f64) -> ArtistRecord {
    ArtistRecord {
        id: id.to_owned(),
        name: name.to_owned(),
        artist_mbid: None,
        linked: false,
        album_count: albums,
        track_count: tracks,
        appearance_album_count: 0,
        date_added: Some(added),
    }
}

#[allow(clippy::too_many_arguments)]
fn track(
    id: &str,
    title: &str,
    album_id: &str,
    album_title: &str,
    artist_name: &str,
    added: f64,
    year: Option<i64>,
    format: &str,
) -> TrackRecord {
    TrackRecord {
        id: id.to_owned(),
        title: title.to_owned(),
        album_id: album_id.to_owned(),
        album_title: album_title.to_owned(),
        artist_name: artist_name.to_owned(),
        artist_id: None,
        album_artist_name: artist_name.to_owned(),
        disc_number: 1,
        track_number: 1,
        year,
        genre: None,
        duration_seconds: Some(200.0),
        format: format.to_owned(),
        bit_rate: None,
        sample_rate: None,
        file_size_bytes: 4_000_000,
        date_added: Some(added),
        cover_available: true,
    }
}

/// Seeded catalog matching the briefs below.
fn seeded_catalog() -> MemoryCatalog {
    let mut al1 = album("al1", "First Light", "a1", "Aurora", Some(1994), 1000.0);
    al1.release_group_mbid = Some("rg-first-light".to_owned());
    let al2 = album("al2", "Second Dawn", "a1", "Aurora", Some(2001), 2000.0);
    let al3 = album("al3", "Lone Peak", "a2", "Boreal", Some(1994), 1500.0);
    let al4 = album(
        "al4",
        "First Light (Deluxe)",
        "a1",
        "Aurora",
        Some(1994),
        1200.0,
    );
    let t1 = track(
        "t1",
        "Opener",
        "al1",
        "First Light",
        "Aurora",
        1000.0,
        Some(1994),
        "flac",
    );
    let t2 = track(
        "t2",
        "Closer",
        "al1",
        "First Light",
        "Aurora",
        1100.0,
        Some(1994),
        "flac",
    );
    let t3 = track(
        "t3",
        "Single",
        "al2",
        "Second Dawn",
        "Aurora",
        2000.0,
        Some(2001),
        "mp3",
    );
    let t4 = track(
        "t4",
        "Peak",
        "al3",
        "Lone Peak",
        "Boreal",
        1500.0,
        Some(1994),
        "flac",
    );
    MemoryCatalog::new()
        .with_albums(vec![al1.clone(), al2.clone(), al3.clone()])
        .with_artists(vec![
            artist("a1", "Aurora", 2, 3, 1000.0),
            artist("a2", "Boreal", 1, 1, 1500.0),
            artist("a3", "Guest", 0, 1, 1600.0),
        ])
        .with_tracks(vec![t1.clone(), t2.clone(), t3.clone(), t4.clone()])
        .with_copies("al1", vec![al4])
        .with_artist_albums("a1", vec![al1.clone(), al2.clone()], vec![])
        .with_artist_albums("a2", vec![al3], vec![])
        .with_artist_albums("a3", vec![], vec![al1])
        .with_genre("rock", "Rock", vec![t1, t2])
        .with_genre("jazz", "Jazz", vec![t3])
}

fn seeded_favorites() -> MemoryFavorites {
    MemoryFavorites::new()
        .with_favorites("user-ada", "album", &["al1"])
        .with_favorites("user-ada", "artist", &["a2"])
        .with_favorites("user-ada", "track", &["t3"])
}

fn seeded_lyrics() -> MemoryLyrics {
    MemoryLyrics::new()
        .with_doc(
            "t1",
            LyricDoc {
                lines: vec![
                    ("line one".to_owned(), Some(1000)),
                    ("line two".to_owned(), Some(4500)),
                ],
                synced: true,
            },
        )
        .with_doc(
            "t2",
            LyricDoc {
                lines: vec![("plain words".to_owned(), None)],
                synced: false,
            },
        )
}

fn deps(
    catalog: Arc<dyn LibraryCatalog>,
    favorites: Arc<dyn FavoriteReads>,
    lyrics: Arc<dyn LyricsPort>,
    auth: &UsersDeps,
) -> LibraryDeps {
    LibraryDeps {
        catalog,
        favorites,
        lyrics,
        auth: auth.clone(),
        ids: Arc::new(FixedIdGenerator),
    }
}

/// Router with a stashed session for `user_id`, or anonymous when None.
fn app(deps: LibraryDeps, user_id: Option<&str>) -> Router {
    let router = library_router(deps);
    match user_id {
        Some(user_id) => {
            let user_id = user_id.to_owned();
            router.layer(axum::middleware::from_fn(
                move |mut req: Request<Body>, next: axum::middleware::Next| {
                    let user_id = user_id.clone();
                    async move {
                        req.extensions_mut().insert(CurrentSession {
                            user_id,
                            session_id: "sess-1".to_owned(),
                            kind: SessionKind::Standard,
                            transport: Transport::Bearer,
                        });
                        next.run(req).await
                    }
                },
            ))
        }
        None => router,
    }
}

async fn auth_bundle() -> (TestRig, String) {
    let rig = TestRig::new().expect("rig builds");
    let user = rig.seed_user("ada", Role::User).await;
    (rig, user.id)
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

/// Every slice route with concrete ids. The auth and leak loops walk this.
const ROUTES: &[&str] = &[
    "/library/albums",
    "/library/albums/al1",
    "/library/albums/al1/tracks",
    "/library/albums/al1/copies",
    "/library/artists",
    "/library/artists/a1",
    "/library/artists/a1/albums",
    "/library/artists/a1/appearances",
    "/library/tracks",
    "/library/tracks/t1",
    "/library/tracks/t1/lyrics",
    "/library/stats",
    "/library/recently-added",
    "/library/genres",
    "/library/genres/rock/tracks",
    "/local-library/albums",
    "/local-library/albums/match/rg-first-light",
    "/local-library/search?q=light",
    "/local-library/recent",
    "/local-library/decades",
    "/local-library/suggestions",
];

#[tokio::test]
async fn suggestions_same_era_pool_tags_era_picks() {
    let (rig, user_id) = auth_bundle().await;
    let catalog = MemoryCatalog::new().with_tracks(vec![
        track(
            "u1",
            "New One",
            "al9",
            "New Set",
            "Aurora",
            1.0,
            Some(2001),
            "flac",
        ),
        track(
            "u2",
            "New Two",
            "al9",
            "New Set",
            "Aurora",
            2.0,
            Some(2001),
            "flac",
        ),
        track(
            "u3",
            "Old One",
            "al8",
            "Old Set",
            "Aurora",
            3.0,
            Some(1990),
            "flac",
        ),
        track(
            "u4",
            "Old Two",
            "al8",
            "Old Set",
            "Aurora",
            4.0,
            Some(1990),
            "flac",
        ),
    ]);
    let app = app(
        deps(
            Arc::new(catalog),
            Arc::new(MemoryFavorites::new()),
            Arc::new(MemoryLyrics::new()),
            &rig.deps,
        ),
        Some(&user_id),
    );
    let (status, json) = get(app, "/local-library/suggestions?limit=4&decade=1990").await;
    assert_eq!(status, StatusCode::OK);
    let suggestions: SuggestionsResponse =
        serde_json::from_value(json).expect("suggestions decode");
    let era: Vec<&str> = suggestions
        .items
        .iter()
        .filter(|item| item.reason == "same_era")
        .map(|item| item.track_id.as_str())
        .collect();
    assert_eq!(era, ["u3"]);
}

#[tokio::test]
async fn bad_input_stays_in_the_envelope() {
    let (rig, user_id) = auth_bundle().await;
    let app = app(
        deps(
            Arc::new(seeded_catalog()),
            Arc::new(seeded_favorites()),
            Arc::new(seeded_lyrics()),
            &rig.deps,
        ),
        Some(&user_id),
    );
    for path in [
        "/library/albums?sort=nope",
        "/library/albums?order=sideways",
        "/library/albums?limit=0",
        "/library/albums?limit=abc",
        "/library/albums?offset=-1",
        "/library/albums?decade=1995",
        "/library/artists?scope=nope",
        "/library/artists?sort=nope",
        "/library/tracks?sort=nope",
        "/local-library/suggestions?limit=99",
    ] {
        let (status, json) = get(app.clone(), path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "for {path}");
        assert_eq!(json["error"]["code"], "INVALID_INPUT", "for {path}");
        assert!(json["error"]["message"].is_string(), "for {path}");
    }
}

#[tokio::test]
async fn auth_matrix_every_route_401s_anonymous_and_admits_users() {
    let (rig, user_id) = auth_bundle().await;
    let admin = rig.seed_user("root", Role::Admin).await;
    let authenticated = app(
        deps(
            Arc::new(seeded_catalog()),
            Arc::new(seeded_favorites()),
            Arc::new(seeded_lyrics()),
            &rig.deps,
        ),
        Some(&user_id),
    );
    let anonymous = app(
        deps(
            Arc::new(seeded_catalog()),
            Arc::new(seeded_favorites()),
            Arc::new(seeded_lyrics()),
            &rig.deps,
        ),
        None,
    );
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
    let as_admin = app(
        deps(
            Arc::new(seeded_catalog()),
            Arc::new(seeded_favorites()),
            Arc::new(seeded_lyrics()),
            &rig.deps,
        ),
        Some(&admin.id),
    );
    let (status, _) = get(as_admin, "/library/stats").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn leak_brief_every_route_hides_store_faults() {
    let (rig, user_id) = auth_bundle().await;
    let app = app(
        deps(
            Arc::new(FailingCatalog::new()),
            Arc::new(FailingFavorites::new()),
            Arc::new(FailingLyrics::new()),
            &rig.deps,
        ),
        Some(&user_id),
    );
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
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body reads");
        let text = String::from_utf8(bytes.to_vec()).expect("body is text");
        assert!(!text.contains("/tmp/secret.db"), "path leaked on {path}");
        assert!(!text.contains("db.internal"), "host leaked on {path}");
        assert!(!text.contains("injected fault"), "cause leaked on {path}");
        let json: Value = serde_json::from_str(&text).expect("body is json");
        assert_eq!(json["error"]["code"], "INTERNAL_ERROR", "for {path}");
        assert_eq!(
            json["error"]["message"], "Internal server error",
            "for {path}"
        );
        assert_eq!(json["error"]["details"]["error_id"], FIXED_ID, "for {path}");
    }
}

// SQLite briefs: the real adapters over a scratch migrated database,
// including a missing and an excluded track proving streamable-only counts.

struct SqliteFixture {
    #[allow(dead_code)]
    runtime: droppedneedle::db::DbRuntime,
    catalog: SqliteCatalog,
    favorites: SqliteFavorites,
    /// Declared last so the database closes before the directory goes.
    _scratch: ScratchDir,
}

async fn sqlite_fixture() -> SqliteFixture {
    use droppedneedle::db::{DbConfig, open_runtime};

    let dir = ScratchDir::new("reads-library");
    let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
        .await
        .expect("scratch runtime opens");
    let pool = runtime.pool();
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
         ('a1', 'musicbrainz', 'm1', 'manual', 1000)",
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
        .lane()
        .write(
            droppedneedle::db::Lane::Foreground,
            "library seed",
            move |tx| {
                tx.execute_batch(&seed)
                    .map_err(droppedneedle::db::OpError::from)?;
                Ok(())
            },
        )
        .await
        .expect("seed batch runs");
    let db = LibraryDb::new(pool);
    SqliteFixture {
        runtime,
        catalog: SqliteCatalog::new(&db),
        favorites: SqliteFavorites::new(&db),
        _scratch: dir,
    }
}

#[tokio::test]
async fn sqlite_albums_streamable_counts_identity_and_filters() {
    let fixture = sqlite_fixture().await;
    let filter = AlbumFilter::default();
    let (albums, total) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Name, false, 50, 0)
        .await
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
        .list_albums(&filter, AlbumSort::Name, false, 50, 0)
        .await
        .expect("q filter");
    assert_eq!(total, 1);

    let filter = AlbumFilter {
        decade: Some(1990),
        ..Default::default()
    };
    let (_, total) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Name, false, 50, 0)
        .await
        .expect("decade filter");
    assert_eq!(total, 3);

    let filter = AlbumFilter {
        artist_id: Some("a1".to_owned()),
        ..Default::default()
    };
    let (_, total) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Name, false, 50, 0)
        .await
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
            .list_albums(&filter, AlbumSort::Name, false, 50, 0)
            .await
            .expect("format filter");
        assert_eq!(counted, total, "for {format}");
        assert_eq!(albums.len() as u64, total, "for {format}");
    }

    let filter = AlbumFilter::default();
    let (albums, _) = fixture
        .catalog
        .list_albums(&filter, AlbumSort::Artist, false, 50, 0)
        .await
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
        .list_albums(&filter, AlbumSort::Artist, true, 50, 0)
        .await
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
    // Three seeded plus the migration's Various/Unknown sentinels; the
    // retired row stays out, and the uncredited sentinels join neither scope.
    assert_eq!((total, led, contributors), (5, 2, 1));
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
    assert_eq!(
        (a1.album_count, a1.track_count, a1.appearance_album_count),
        (3, 3, 0)
    );
    assert_eq!(a1.artist_mbid.as_deref(), Some("m1"));

    let a3 = fixture
        .catalog
        .get_artist("a3")
        .await
        .expect("lookup runs")
        .expect("a3 found");
    assert_eq!(
        (a3.album_count, a3.track_count, a3.appearance_album_count),
        (0, 1, 1)
    );

    let (led_albums, total) = fixture
        .catalog
        .artist_albums("a1", 50, 0)
        .await
        .expect("led list");
    assert_eq!(total, 3);
    assert_eq!(led_albums[0].id, "al1");

    let (appearances, total) = fixture
        .catalog
        .artist_appearances("a3", 50, 0)
        .await
        .expect("appearances list");
    assert_eq!(total, 1);
    assert_eq!(appearances[0].id, "al3");
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
        .list_tracks(&filter, TrackSort::Title, false, 50, 0)
        .await
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
        .list_tracks(&filter, TrackSort::Title, false, 50, 0)
        .await
        .expect("genre filter");
    assert_eq!(total, 2);

    let filter = TrackFilter {
        artist_id: Some("a3".to_owned()),
        ..Default::default()
    };
    let (tracks, total) = fixture
        .catalog
        .list_tracks(&filter, TrackSort::Title, false, 50, 0)
        .await
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
        (4, 5, 4)
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
    assert_eq!((decades[0].decade, decades[0].album_count), (1990, 3));
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
async fn sqlite_favorites_and_unwired_fails_closed() {
    let fixture = sqlite_fixture().await;
    let ids = ["al1".to_owned(), "al2".to_owned()];
    let marked = fixture
        .favorites
        .filter_favorites("u1", "album", &ids)
        .await
        .expect("filter runs");
    assert_eq!(marked, std::collections::HashSet::from(["al1".to_owned()]));
    let counts = fixture
        .favorites
        .favorite_counts("u1")
        .await
        .expect("counts read");
    assert_eq!(counts, (1, 1, 1));
    let counts = fixture
        .favorites
        .favorite_counts("nobody")
        .await
        .expect("counts read");
    assert_eq!(counts, (0, 0, 0));

    let unwired = LibraryDb::unwired();
    let catalog = SqliteCatalog::new(&unwired);
    assert!(catalog.stats().await.is_err());
    let favorites = SqliteFavorites::new(&unwired);
    assert!(favorites.favorite_counts("u1").await.is_err());
}
