//! Unified search over a migrated database behind the real
//! session gate: ranked buckets, accent and case folding, drill-down
//! paging, suggest, enrichment that degrades inside a 200 and the fixed
//! 500 on a database fault.

use crate::common::Probe;
use droppedneedle_catalog::search;

use crate::common::TestApp;
use crate::common::auth::AuthRig;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    middleware,
};
use droppedneedle_auth::session::middleware::require_session;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_catalog::metadata::Catalog;
use droppedneedle_catalog::metadata::library::LocalCatalog;
use droppedneedle_catalog::metadata::upstream::{CatalogSettings, Upstream};
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_kernel::runtime_config::sections::{
    MbSourceMode, MusicBrainzSettings, UserPreferences,
};
use droppedneedle_providers::Providers;
use droppedneedle_providers::test_support::ScriptedWire;
use search::SearchDeps;
use search::models::{EnrichmentBatchRequest, EnrichmentResponse};
use search::ports::{EnrichmentPort, EnrichmentPortError};
use search::service::SearchService;
use std::path::{Path, PathBuf};
use tower::ServiceExt as _;

/// Bearer token the rig signs the test user in with.
const TEST_TOKEN: &str = "dn-search-test-token-0000000000001";

/// Provider fakes: a dead provider that must degrade, never 500.
#[derive(Debug, Clone)]
struct FailingEnrichment;

impl EnrichmentPort for FailingEnrichment {
    fn enrich_batch<'a>(
        &'a self,
        _user_id: &'a str,
        _request: EnrichmentBatchRequest,
    ) -> search::ports::BoxFuture<'a, Result<EnrichmentResponse, EnrichmentPortError>> {
        Box::pin(async move {
            Err(EnrichmentPortError::new(
                "listenbrainz",
                "connection refused at lb.internal: critique-brainz".to_owned(),
            ))
        })
    }
}

/// Fixture catalog: two artists plus an album each plus one track.
/// Queries in these tests avoid the `various artists` / `unknown artist`
/// sentinel rows, so every hit below is a fixture row.
fn seed_catalog(app_db: &Path) {
    for (id, name, folded) in [
        ("artist-beyonce", "Beyoncé", "beyonce"),
        ("artist-beirut", "Beirut", "beirut"),
    ] {
        app_db.exec("INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
             VALUES (?, ?, ?, 'group', 0, 0)", rusqlite::params![id, name, folded]);
    }
    app_db.exec("INSERT INTO local_artist_external_identities \
         (local_artist_id, provider, provider_artist_id, decision_source, selected_at) \
         VALUES ('artist-beyonce', 'musicbrainz', '859d1e45-caf9-4e94-9e90-f17e67db2a6a', 'manual', 0)", []);
    for (id, artist_id, title, folded, artist_name, artist_folded, year) in [
        (
            "album-renaissance",
            "artist-beyonce",
            "Renaissance",
            "renaissance",
            "Beyoncé",
            "beyonce",
            2022,
        ),
        (
            "album-gulag",
            "artist-beirut",
            "Gulag Orkestar",
            "gulag orkestar",
            "Beirut",
            "beirut",
            2006,
        ),
    ] {
        app_db.exec(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, year, \
             grouping_source, created_at, updated_at) \
             VALUES (?, 'root-1', ?, ?, ?, ?, ?, ?, ?, 'manual', 0, 0)",
            rusqlite::params![
                id,
                format!("group-{id}"),
                title,
                folded,
                artist_name,
                artist_folded,
                artist_id,
                year
            ],
        );
        app_db.exec(
            "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
             VALUES (?, 0, ?, 'main')",
            rusqlite::params![id, artist_id],
        );
    }
    app_db.exec(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         artist_name, artist_name_folded, album_title, album_title_folded, \
         file_format, ingest_source, imported_at, membership_source) \
         VALUES ('track-alien', 'album-renaissance', 'root-1', '/music/alien.flac', \
         'alien.flac', 'hash-1', 1000, 0, 'stat-1', 'Alien Superstar', 'alien superstar', \
         'Beyoncé', 'beyonce', 'Renaissance', 'renaissance', \
         'flac', 'scan', 0, 'manual')",
        [],
    );
}

/// The search deps over `app_db`. With no wire, MusicBrainz search fails
/// at once without a request and the tests read the library's own hits;
/// with one, the official MusicBrainz answers on it.
fn search_deps(
    app_db: PathBuf,
    enrichment: Arc<dyn EnrichmentPort>,
    users: droppedneedle_auth::users::UsersDeps,
    wire: Option<Arc<ScriptedWire>>,
) -> SearchDeps {
    let db = droppedneedle_kernel::db::Db::open_for_tests(&app_db).expect("search database opens");
    let remote = match wire {
        Some(wire) => scripted_catalog(&db, users, wire),
        None => crate::common::bundles::offline_catalog(&db, users),
    };
    SearchDeps::new(SearchService::new(db, remote), enrichment)
}

/// A metadata catalog over `db` reading the official MusicBrainz, which
/// the closed provider endpoints put at [`MUSICBRAINZ`] on `wire`.
fn scripted_catalog(
    db: &droppedneedle_kernel::db::Db,
    users: droppedneedle_auth::users::UsersDeps,
    wire: Arc<ScriptedWire>,
) -> Catalog {
    Catalog::new(
        Upstream::new(
            Arc::new(Providers::unpaced(wire)),
            Arc::new(OfficialMusicBrainz),
            users.clone(),
            droppedneedle_media::playback::listening::ListeningStore::new(
                db.clone(),
                users.crypto.clone(),
                users.clock.clone(),
            ),
        ),
        LocalCatalog::new(db.clone()),
        crate::common::bundles::quiet_page_sources(crate::common::bundles::cover_warm_at(
            db,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        )),
    )
}

/// Where the official MusicBrainz answers in [`scripted_catalog`].
const MUSICBRAINZ: &str = "http://127.0.0.1:9/musicbrainz/ws/2";

/// Catalog settings on the official MusicBrainz, everything else at its
/// default and ListenBrainz off.
struct OfficialMusicBrainz;

impl CatalogSettings for OfficialMusicBrainz {
    fn musicbrainz(&self) -> Option<MusicBrainzSettings> {
        Some(MusicBrainzSettings {
            source_mode: MbSourceMode::Official,
            ..MusicBrainzSettings::default()
        })
    }

    fn preferences(&self) -> UserPreferences {
        UserPreferences::default()
    }

    fn advanced(&self) -> AdvancedSettings {
        AdvancedSettings::default()
    }

    fn listenbrainz_enabled(&self) -> bool {
        false
    }

    fn store_region(&self) -> Option<String> {
        None
    }
}

/// What a test's router needs alive: the rig its session lives in and the
/// app whose request wrapper it runs under.
type Held = (AuthRig, TestApp);

/// The search router behind the real middleware stack: session gate
/// inside, request scope outside, shared 404/405 fallbacks. Mirrors the
/// production posture in `create_app` for these paths. A test user is
/// signed in with [`TEST_TOKEN`].
async fn app(app_db: PathBuf, enrichment: Arc<dyn EnrichmentPort>) -> (axum::Router, Held) {
    app_on(app_db, enrichment, None).await
}

/// [`app`] with MusicBrainz answering on `wire`, when there is one.
async fn app_on(
    app_db: PathBuf,
    enrichment: Arc<dyn EnrichmentPort>,
    wire: Option<Arc<ScriptedWire>>,
) -> (axum::Router, Held) {
    let rig = AuthRig::new("reads-search").await;
    let user = rig.seed_user("ada", Role::User).await;
    rig.sign_in(&user.id, TEST_TOKEN).await;
    let scope = TestApp::start("reads-search").await;
    let app = scope
        .request_scope(
            search::routes()
                .with_state(search_deps(app_db, enrichment, rig.deps.clone(), wire))
                .into_router()
                .layer(middleware::from_fn_with_state(
                    rig.session_gate(),
                    require_session,
                )),
        )
        .fallback(droppedneedle::handlers::fallback_404)
        .method_not_allowed_fallback(droppedneedle::handlers::fallback_405);
    (app, (rig, scope))
}

async fn authed_app() -> (axum::Router, PathBuf, (Held, crate::common::ScratchDir)) {
    let (app_db, dir) = crate::common::migrated_file("reads-search");
    seed_catalog(&app_db);
    let (app, held) = app(
        app_db.clone(),
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
    )
    .await;
    (app, app_db, (held, dir))
}

async fn call(
    app: axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let request = if let Some(body) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

fn assert_screaming_snake(code: &str) {
    assert!(!code.is_empty());
    assert_eq!(code, code.to_uppercase());
    assert!(
        code.chars()
            .all(|char| char.is_ascii_uppercase() || char == '_')
    );
}

/// The search routes over the seeded catalog: ranked buckets with their
/// tops, accents and case folded, a blank query refused in the envelope,
/// suggest merging buckets best first under its cap, and a bucket's
/// drill-down paging with its top only on the first page.
#[tokio::test]
async fn search_routes_answer_the_seeded_catalog() {
    let (app, _, _held) = authed_app().await;

    // Ranked buckets with tops.
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search?q=beyonce",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "buckets");
    let body = json(&bytes);
    assert_eq!(body["artists"][0]["title"], "Beyoncé", "buckets");
    assert_eq!(body["artists"][0]["kind"], "artist", "buckets");
    assert_eq!(body["artists"][0]["score"], 100, "buckets");
    assert_eq!(
        body["artists"][0]["musicbrainz_id"], "859d1e45-caf9-4e94-9e90-f17e67db2a6a",
        "buckets"
    );
    assert_eq!(body["artists"][0]["in_library"], true, "buckets");
    assert_eq!(body["artists"][0]["requested"], false, "buckets");
    assert_eq!(body["top_artist"]["title"], "Beyoncé", "buckets");
    assert_eq!(body["albums"][0]["title"], "Renaissance", "buckets");
    assert_eq!(body["albums"][0]["artist"], "Beyoncé", "buckets");
    assert_eq!(body["albums"][0]["year"], 2022, "buckets");
    assert_eq!(body["tracks"][0]["title"], "Alien Superstar", "buckets");
    assert_eq!(
        body["tracks"][0]["track"]["album_id"], "album-renaissance",
        "buckets"
    );
    assert_eq!(
        body["tracks"][0]["track"]["album_title"], "Renaissance",
        "buckets"
    );
    assert_eq!(body["tracks"][0]["track"]["format"], "flac", "buckets");
    assert!(body["artists"][0]["track"].is_null(), "buckets");
    assert!(body["top_track"].is_null(), "buckets");

    // Accents and case.
    for query in ["beyonce", "BEYONCE", "Beyoncé", "BEYONCÉ"] {
        let uri = format!("/api/v3/search?{}", urlencode_query(query));
        let (status, _, bytes) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        let body = json(&bytes);
        assert_eq!(body["artists"][0]["title"], "Beyoncé", "{query}");
    }

    // A blank query.
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search?q=%20%20",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "blank query");
    let body = json(&bytes);
    assert_eq!(
        body,
        serde_json::json!({"error": {"code": "INVALID_INPUT", "message": "Query must not be blank", "details": null}}),
        "blank query"
    );
    assert_screaming_snake(body["error"]["code"].as_str().unwrap());

    // Suggest.
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search/suggest?q=bey",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suggest");
    let body = json(&bytes);
    let kinds: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"artist"), "suggest: {kinds:?}");
    assert!(kinds.contains(&"album"), "suggest: {kinds:?}");
    let scores: Vec<i64> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["score"].as_i64().unwrap())
        .collect();
    assert!(
        scores.windows(2).all(|pair| pair[0] >= pair[1]),
        "suggest: {scores:?}"
    );
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search/suggest?q=bey&limit=1",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "suggest cap");
    assert_eq!(
        json(&bytes)["results"].as_array().unwrap().len(),
        1,
        "suggest cap"
    );

    // A bucket's drill-down.
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search/artists?q=be&limit=1&offset=0",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "first page");
    let body = json(&bytes);
    assert_eq!(body["bucket"], "artists", "first page");
    assert_eq!(body["limit"], 1, "first page");
    assert_eq!(body["offset"], 0, "first page");
    assert_eq!(body["results"].as_array().unwrap().len(), 1, "first page");
    assert_eq!(body["results"][0]["title"], "Beirut", "first page");
    assert_eq!(body["top_result"]["title"], "Beirut", "first page");
    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/artists?q=be&limit=1&offset=1",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "second page");
    let body = json(&bytes);
    assert_eq!(body["results"][0]["title"], "Beyoncé", "second page");
    assert!(body["top_result"].is_null(), "second page");
}

/// A library album with no art of its own and no release group is marked
/// `art_missing`, so the web UI draws its placeholder without asking for a
/// cover; one with a folder image is not. A song inherits its album's mark.
#[tokio::test]
async fn album_hits_say_when_art_is_known_missing() {
    let (app, app_db, _held) = authed_app().await;
    seed_track(
        &app_db,
        "t-gulag",
        "album-gulag",
        "Brandenburg",
        "Gulag Orkestar",
        "indexed",
    );
    app_db.exec(
        "INSERT INTO local_album_artwork (local_album_id, source, source_locator, \
         version, updated_at) VALUES ('album-renaissance', 'folder', '/music/cover.jpg', 1, 1)",
        [],
    );

    let mut marks = Vec::new();
    for query in ["renaissance", "gulag", "brandenburg"] {
        let uri = format!("/api/v3/search?q={query}");
        let (status, _, bytes) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK);
        let body = json(&bytes);
        let hit = if query == "brandenburg" {
            body["tracks"][0].clone()
        } else {
            body["albums"][0].clone()
        };
        marks.push((
            hit["id"].as_str().unwrap().to_owned(),
            hit["art_missing"].clone(),
        ));
    }
    assert_eq!(
        marks,
        vec![
            ("album-renaissance".to_owned(), serde_json::json!(false)),
            ("album-gulag".to_owned(), serde_json::json!(true)),
            ("t-gulag".to_owned(), serde_json::json!(true)),
        ]
    );
}

/// Insert one track; `availability` other than `indexed` must never match.
fn seed_track(
    app_db: &Path,
    id: &str,
    album_id: &str,
    title: &str,
    album_title: &str,
    availability: &str,
) {
    app_db.exec(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         artist_name, artist_name_folded, album_title, album_title_folded, duration_seconds, \
         file_format, availability, ingest_source, imported_at, membership_source) \
         VALUES (?, ?, 'root-1', ?, ?, ?, 1000, 0, 'stat-1', ?, lower(?), \
         'Someone', 'someone', ?, lower(?), 61.5, 'mp3', ?, 'scan', 0, 'manual')",
        rusqlite::params![
            id,
            album_id,
            format!("/music/{id}.mp3"),
            format!("{id}.mp3"),
            format!("hash-{id}"),
            title,
            title,
            album_title,
            album_title,
            availability
        ],
    );
}

/// A song title finds the song, ranked exact title, title prefix, title
/// substring, then songs matched only through their album; each carries
/// what playing it and linking its album need. Tied titles page stably,
/// a missing file never matches, and a query with no word of three
/// characters matches title prefixes.
#[tokio::test]
async fn a_song_title_finds_the_song() {
    let (app, app_db, _held) = authed_app().await;
    seed_track(
        &app_db,
        "t-opening",
        "album-gulag",
        "Opening",
        "Intro Sessions",
        "indexed",
    );
    seed_track(
        &app_db,
        "t-song",
        "album-gulag",
        "The Intro Song",
        "Gulag Orkestar",
        "indexed",
    );
    seed_track(
        &app_db,
        "t-reprise",
        "album-gulag",
        "Intro (Reprise)",
        "Gulag Orkestar",
        "indexed",
    );
    seed_track(
        &app_db,
        "t-intro-r",
        "album-renaissance",
        "Intro",
        "Renaissance",
        "indexed",
    );
    seed_track(
        &app_db,
        "t-intro-g",
        "album-gulag",
        "Intro",
        "Gulag Orkestar",
        "indexed",
    );
    seed_track(
        &app_db,
        "t-gone",
        "album-gulag",
        "Intro",
        "Gulag Orkestar",
        "missing",
    );

    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search?q=intro&buckets=tracks",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    let hits: Vec<(&str, i64)> = body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| (hit["id"].as_str().unwrap(), hit["score"].as_i64().unwrap()))
        .collect();
    assert_eq!(
        hits,
        vec![
            ("t-intro-g", 100),
            ("t-intro-r", 100),
            ("t-reprise", 90),
            ("t-song", 70),
            ("t-opening", 60),
        ]
    );
    let first = &body["tracks"][0];
    assert_eq!(first["kind"], "track");
    assert_eq!(first["artist"], "Someone");
    assert_eq!(first["track"]["album_id"], "album-gulag");
    assert_eq!(first["track"]["album_title"], "Gulag Orkestar");
    assert_eq!(first["track"]["duration_seconds"], 61.5);
    assert_eq!(first["track"]["cover_available"], false);

    let mut paged = Vec::new();
    for offset in 0..2 {
        let uri = format!("/api/v3/search/tracks?q=intro&limit=1&offset={offset}");
        let (status, _, bytes) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK);
        paged.push(json(&bytes)["results"][0]["id"].clone());
    }
    assert_eq!(paged, vec!["t-intro-g", "t-intro-r"]);

    let (_, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search?q=in&buckets=tracks",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    let short: Vec<String> = json(&bytes)["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(short, vec!["t-intro-g", "t-intro-r", "t-reprise"]);
}

fn urlencode_query(query: &str) -> String {
    let mut encoded = String::from("q=");
    for byte in query.as_bytes() {
        if byte.is_ascii_alphanumeric() {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[tokio::test]
async fn enrich_failure_degrades_inside_a_200() {
    let (app_db, _dir) = crate::common::migrated_file("reads-search");
    let (app, _held) = app(app_db, Arc::new(FailingEnrichment)).await;
    let (status, _, bytes) = call(
        app,
        Method::POST,
        "/api/v3/search/enrich/batch",
        Some(TEST_TOKEN),
        Some(serde_json::json!({"artists": [{"musicbrainz_id": "artist-1"}], "albums": []})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let response = json(&bytes);
    assert!(response["source"].is_null());
    assert_eq!(response["artists"][0]["musicbrainz_id"], "artist-1");
    assert!(response["artists"][0]["listen_count"].is_null());
    assert_eq!(
        response["degradations"],
        serde_json::json!([{
            "source": "listenbrainz",
            "code": "PROVIDER_UNAVAILABLE",
            "message": "Enrichment temporarily unavailable",
        }])
    );
    let raw = String::from_utf8(bytes).unwrap();
    assert!(!raw.contains("lb.internal"));
    assert!(!raw.contains("critique-brainz"));
}

#[tokio::test]
async fn leak_db_failure_renders_fixed_500_with_error_id() {
    let dir = crate::common::ScratchDir::new("reads-search-no-schema");
    let app_db = dir.join("app.db");
    drop(droppedneedle_kernel::db::open_connection(&app_db).expect("empty database opens"));
    let (app, _held) = app(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
    )
    .await;
    let (status, headers, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search?q=beyonce",
        Some(TEST_TOKEN),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let body = json(&bytes);
    assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
    assert_eq!(body["error"]["message"], "Internal server error");
    assert_eq!(
        body["error"]["details"]["error_id"],
        headers["x-request-id"].to_str().unwrap()
    );
    let raw = String::from_utf8(bytes).unwrap();
    assert!(!raw.contains("local_artists"));
    assert!(!raw.contains("no such table"));
}

/// The ids a track search answers, in order.
async fn track_hits(app: &axum::Router, query: &str) -> Vec<String> {
    let uri = format!("/api/v3/search/tracks?{}&limit=50", urlencode_query(query));
    let (status, _, bytes) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK, "{query}");
    json(&bytes)["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["id"].as_str().unwrap().to_owned())
        .collect()
}

/// Copies the track table into a new one and swaps it in, the way a table
/// is reshaped. The copy numbers its rows afresh, so the gaps deleted
/// tracks left close up.
async fn rebuild_tracks(app_db: &Path) {
    let table: String = app_db.one(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'local_tracks'",
        [],
    );
    let dependents: Vec<String> = app_db.column(
        "SELECT sql FROM sqlite_master WHERE tbl_name = 'local_tracks' \
         AND type IN ('index', 'trigger') AND sql IS NOT NULL",
        [],
    );
    let columns: String = app_db.one(
        "SELECT group_concat(name, ', ') FROM pragma_table_info('local_tracks')",
        [],
    );
    let create = table.replacen(
        "CREATE TABLE \"local_tracks\"",
        "CREATE TABLE local_tracks_rebuilt",
        1,
    );
    app_db
        .connect()
        .execute_batch(&format!(
            "PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON; \
             BEGIN; {create}; \
             INSERT INTO local_tracks_rebuilt ({columns}) SELECT {columns} FROM local_tracks; \
             DROP TABLE local_tracks; \
             ALTER TABLE local_tracks_rebuilt RENAME TO local_tracks; {}; COMMIT; \
             PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON",
            dependents.join("; ")
        ))
        .unwrap();
}

/// Track search answers the same after the database is vacuumed and after
/// the track table is rebuilt over the gaps deleted tracks left: the
/// search index never names a track by a number a copy reassigns.
#[tokio::test]
async fn track_search_survives_vacuum_and_a_table_rebuild() {
    let (app_db, _dir) = crate::common::migrated_file("reads-search-rebuild");
    seed_catalog(&app_db);
    let words = ["amber", "cobalt", "dune"];
    for step in 0..30 {
        let id = format!("t-needle-{step:02}");
        let title = format!("Needle {step:02} {}", words[step % 3]);
        seed_track(
            &app_db,
            &id,
            "album-gulag",
            &title,
            "Gulag Orkestar",
            "indexed",
        );
    }
    app_db.exec(
        "DELETE FROM local_tracks WHERE id IN ('t-needle-04', 't-needle-05', \
         't-needle-11', 't-needle-12', 't-needle-13', 't-needle-20')",
        [],
    );
    let (app, _held) = app(
        app_db.clone(),
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
    )
    .await;
    let queries = ["amber", "cobalt", "dune", "needle 1", "needle"];
    let mut before = Vec::new();
    for query in queries {
        before.push(track_hits(&app, query).await);
    }
    assert_eq!(
        before[0],
        [
            "t-needle-00",
            "t-needle-03",
            "t-needle-06",
            "t-needle-09",
            "t-needle-15",
            "t-needle-18",
            "t-needle-21",
            "t-needle-24",
            "t-needle-27",
        ]
    );
    assert_eq!(before[4].len(), 24);

    app_db.exec("VACUUM", []);
    for (query, expected) in queries.iter().zip(&before) {
        assert_eq!(
            &track_hits(&app, query).await,
            expected,
            "{query} after VACUUM"
        );
    }

    rebuild_tracks(&app_db).await;
    for (query, expected) in queries.iter().zip(&before) {
        assert_eq!(
            &track_hits(&app, query).await,
            expected,
            "{query} after a table rebuild"
        );
    }
}

/// One MusicBrainz search answer listing `hits` under `key`.
fn search_answer(key: &str, hits: Vec<serde_json::Value>) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"count": hits.len(), "offset": 0, key: hits}))
        .expect("answer encodes")
}

/// One artist search hit.
fn artist_hit(name: &str, score: u32) -> serde_json::Value {
    serde_json::json!({"id": format!("artist-{name}"), "score": score, "name": name})
}

/// One release-group search hit: id, title, credited artist, primary type,
/// score and how many releases the group holds.
type Group<'a> = (&'a str, &'a str, &'a str, &'a str, u32, u32);

fn group_hit((id, title, artist, kind, score, count): Group<'_>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "score": score,
        "count": count,
        "title": title,
        "primary-type": kind,
        "artist-credit": [{
            "name": artist,
            "joinphrase": "",
            "artist": {"id": format!("artist-{artist}"), "name": artist},
        }],
    })
}

/// MusicBrainz scores an album's tributes and namesakes as high as the
/// album, so a hit whose title is the query ranks as exact and the group
/// with more releases goes first, then the library, then the title. The
/// score shown stays MusicBrainz's, the ranking runs before the cut to the
/// limit, and the release count never lifts an album above an artist. The
/// hits keep the order, scores and release counts of BrainzMash's answers
/// on 2026-11-13, with one namesake more for "the beatles".
#[tokio::test]
async fn exact_title_albums_rank_by_their_releases() {
    let beatles: Vec<Group<'_>> = vec![
        (
            "rg-dreamsound",
            "The Beatles",
            "The Dreamsound Orchestra",
            "Album",
            100,
            1,
        ),
        (
            "rg-mersey",
            "The Beatles",
            "The Mersey Sound Orchestra",
            "Album",
            100,
            1,
        ),
        (
            "rg-ukulele",
            "The Beatles",
            "Various Artists",
            "Album",
            100,
            1,
        ),
        ("rg-1994", "The Beatles", "The Beatles", "Album", 100, 1),
        ("rg-porter", "The Beatles", "Cheryl Porter", "Album", 100, 1),
        (
            "rg-slumber",
            "The Beatles",
            "Slumberlandband",
            "Single",
            100,
            1,
        ),
        (
            "rg-child",
            "The Beatles",
            "Child Pornography",
            "Album",
            100,
            1,
        ),
        (
            "rg-twilley",
            "The Beatles",
            "Dwight Twilley",
            "Album",
            100,
            2,
        ),
        ("rg-famous", "The Beatles", "Famous", "Single", 100, 1),
        ("rg-revival", "The Beatles", "Revival Band", "Album", 100, 1),
        (
            "rg-white-album",
            "The Beatles",
            "The Beatles",
            "Album",
            95,
            58,
        ),
        (
            "rg-introducing",
            "Introducing… The Beatles",
            "The Beatles",
            "Album",
            89,
            27,
        ),
    ];
    let queen: Vec<Group<'_>> = vec![
        ("rg-queen", "Queen", "Queen", "Album", 100, 60),
        ("rg-jessie", "Queen", "Jessie J", "Single", 100, 2),
        ("rg-nicki", "Queen", "Nicki Minaj", "Album", 100, 19),
        ("rg-monae", "Q.U.E.E.N.", "Janelle Monáe", "Single", 99, 1),
        ("rg-perfume", "Queen", "Perfume Genius", "Single", 99, 1),
        ("rg-kaya", "QUEEN", "Kaya", "Album", 99, 3),
    ];
    let computer: Vec<Group<'_>> = vec![
        ("rg-lemaitre", "Ok Computer", "Lemaitre", "Single", 100, 1),
        ("rg-radiohead", "OK Computer", "Radiohead", "Album", 95, 39),
        (
            "rg-turns-10",
            "OK Computer Turns 10",
            "Various Artists",
            "Album",
            77,
            1,
        ),
    ];
    // Answers queue per address, in the order the calls below ask.
    let wire = Arc::new(ScriptedWire::new());
    for (artist, groups) in [
        ("The Beatles", &beatles),
        ("Queen", &queen),
        ("OK Go", &computer),
        ("The Beatles", &beatles),
    ] {
        let artists = vec![artist_hit(artist, 100)];
        let groups = groups.iter().copied().map(group_hit).collect();
        let answer = |path: &str, key: &str, hits| {
            wire.answer(
                &format!("{MUSICBRAINZ}/{path}"),
                200,
                &[],
                search_answer(key, hits),
            );
        };
        answer("artist", "artists", artists);
        answer("release-group", "release-groups", groups);
    }
    let (app_db, _dir) = crate::common::migrated_file("reads-search-exact");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;
    let get = |path: &str, query: &str| {
        let (app, uri) = (app.clone(), format!("{path}?{}", urlencode_query(query)));
        async move {
            let (status, _, bytes) = call(app, Method::GET, &uri, Some(TEST_TOKEN), None).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            json(&bytes)
        }
    };
    let albums = |body: &serde_json::Value| -> Vec<String> {
        let albums = body["albums"].as_array().expect("albums");
        albums
            .iter()
            .map(|album| album["musicbrainz_id"].as_str().unwrap().to_owned())
            .collect()
    };

    // Eleventh after the type filter, past the default cut of ten.
    let body = get("/api/v3/search", "the beatles").await;
    let found = albums(&body);
    assert_eq!(found.len(), 10, "{found:?}");
    assert_eq!(
        found[..3],
        ["rg-white-album", "rg-twilley", "rg-dreamsound"],
        "{found:?}"
    );
    assert_eq!(
        body["albums"][0]["score"], 95,
        "the score shown is MusicBrainz's"
    );
    assert_eq!(body["top_album"]["musicbrainz_id"], "rg-white-album");

    let body = get("/api/v3/search", "queen").await;
    assert_eq!(
        albums(&body),
        [
            "rg-queen",
            "rg-nicki",
            "rg-kaya",
            "rg-jessie",
            "rg-perfume",
            "rg-monae"
        ]
    );

    let body = get("/api/v3/search", "ok computer").await;
    assert_eq!(
        albums(&body),
        ["rg-radiohead", "rg-lemaitre", "rg-turns-10"]
    );

    let body = get("/api/v3/search/suggest", "the beatles").await;
    assert_eq!(body["results"][0]["kind"], "artist", "suggest: {body}");
    assert_eq!(
        body["results"][0]["title"], "The Beatles",
        "suggest: {body}"
    );
    assert_eq!(body["results"][1]["kind"], "album", "suggest: {body}");
    let asked: Vec<String> = wire
        .sent()
        .iter()
        .filter(|sent| sent.url.path().ends_with("/release-group"))
        .filter_map(|sent| sent.query("query"))
        .collect();
    let expected = ["the beatles", "queen", "ok computer", "the beatles"];
    assert_eq!(
        asked.len(),
        expected.len(),
        "one album search per call: {asked:?}"
    );
    for (sent, query) in asked.iter().zip(expected) {
        assert!(
            sent.contains(&format!("\"{query}\"")),
            "{sent} answers {query}"
        );
    }
}

/// Titles tagged with MusicBrainz's hyphen (U+2010) match a query typed
/// with the keyboard hyphen, as an album's exact tier, as a substring and
/// through the song index; the pattern's dash matches any dash.
#[tokio::test]
async fn a_typed_dash_matches_any_dash_in_a_title() {
    let (app, app_db, _held) = authed_app().await;
    app_db.exec(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
         created_at, updated_at) VALUES ('album-verse', 'root-1', 'g-verse', \
         'Across the Spider\u{2010}Verse', 'across the spider\u{2010}verse', 'Beirut', 'beirut', \
         'artist-beirut', 'manual', 0, 0)",
        [],
    );
    seed_track(
        &app_db,
        "t-verse",
        "album-verse",
        "Spider\u{2010}Verse Theme",
        "Across the Spider\u{2010}Verse",
        "indexed",
    );
    let body = {
        let (status, _, bytes) = call(
            app,
            Method::GET,
            "/api/v3/search?q=across%20the%20spider-verse&scope=library",
            Some(TEST_TOKEN),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        json(&bytes)
    };
    assert_eq!(body["albums"][0]["id"], "album-verse", "{body}");
    assert_eq!(
        body["albums"][0]["score"], 70,
        "a dash is any character, not exact"
    );
    assert_eq!(body["tracks"][0]["id"], "t-verse", "{body}");
}

/// A text search MusicBrainz answers empty is asked once more with its
/// one word split into two, or its words fuzzy; a search that found
/// something costs no second call.
#[tokio::test]
async fn an_empty_musicbrainz_answer_is_asked_again_forgivingly() {
    let wire = Arc::new(ScriptedWire::new());
    let groups: Vec<Group<'_>> = vec![(
        "rg-verse",
        "Spider‐Man: Across the Spider‐Verse",
        "Daniel Pemberton",
        "Album",
        90,
        3,
    )];
    let answer = |path: &str, key: &str, hits: Vec<serde_json::Value>| {
        wire.answer(
            &format!("{MUSICBRAINZ}/{path}"),
            200,
            &[],
            search_answer(key, hits),
        );
    };
    // "spiderverse": artists empty twice, albums empty then found.
    answer("artist", "artists", vec![]);
    answer("artist", "artists", vec![]);
    answer("release-group", "release-groups", vec![]);
    answer(
        "release-group",
        "release-groups",
        groups.iter().copied().map(group_hit).collect(),
    );
    // "beatels": artists found first time, albums empty twice.
    answer("artist", "artists", vec![artist_hit("The Beatles", 100)]);
    answer("release-group", "release-groups", vec![]);
    answer("release-group", "release-groups", vec![]);
    let (app_db, _dir) = crate::common::migrated_file("reads-search-forgiving");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;
    let get = |query: &str| {
        let (app, uri) = (
            app.clone(),
            format!("/api/v3/search?{}", urlencode_query(query)),
        );
        async move {
            let (status, _, bytes) = call(app, Method::GET, &uri, Some(TEST_TOKEN), None).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            json(&bytes)
        }
    };

    let body = get("spiderverse").await;
    assert_eq!(body["albums"][0]["musicbrainz_id"], "rg-verse", "{body}");
    assert_eq!(body["album_status"], "ok");
    let body = get("beatels").await;
    assert_eq!(body["artists"][0]["title"], "The Beatles", "{body}");
    assert!(body["albums"].as_array().unwrap().is_empty(), "{body}");

    // The buckets run at once, so each path keeps its own order.
    let asked = |path: &str| -> Vec<String> {
        wire.sent()
            .iter()
            .filter(|sent| sent.url.path().ends_with(path))
            .filter_map(|sent| sent.query("query"))
            .collect()
    };
    const SPLIT: &str = r#"(spiderverse~ OR "sp iderverse" OR "spi derverse" OR "spid erverse" OR "spide rverse" OR "spider verse" OR "spiderv erse" OR "spiderve rse" OR "spiderver se")"#;
    assert_eq!(
        asked("/artist"),
        vec![
            r#"artist:"spiderverse"^3 OR artistaccent:"spiderverse"^3 OR alias:"spiderverse"^2 OR spiderverse"#.to_owned(),
            format!("artist:{SPLIT}^3 OR artistaccent:{SPLIT}^3 OR alias:{SPLIT}^2 OR {SPLIT}"),
            r#"artist:"beatels"^3 OR artistaccent:"beatels"^3 OR alias:"beatels"^2 OR beatels"#.to_owned(),
        ]
    );
    const FUZZY: &str = r#"(beatels~ OR "be atels" OR "bea tels" OR "beat els" OR "beate ls")"#;
    assert_eq!(
        asked("/release-group"),
        vec![
            r#"releasegroup:"spiderverse"^3 OR release:"spiderverse"^2 OR spiderverse"#.to_owned(),
            format!("releasegroup:{SPLIT}^3 OR release:{SPLIT}^2 OR {SPLIT}"),
            r#"releasegroup:"beatels"^3 OR release:"beatels"^2 OR beatels"#.to_owned(),
            format!("releasegroup:{FUZZY}^3 OR release:{FUZZY}^2 OR {FUZZY}"),
        ]
    );
}
