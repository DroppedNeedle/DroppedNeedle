//! Unified search over a migrated in-memory database behind the real
//! session gate: ranked buckets, accent and case folding, drill-down
//! paging, suggest, enrichment that degrades inside a 200, the auth row
//! and the fixed 500 on a database fault.

use droppedneedle_catalog::reads::search;

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
use search::SearchDeps;
use search::models::{EnrichmentBatchRequest, EnrichmentResponse};
use search::ports::{EnrichmentPort, EnrichmentPortError};
use search::service::SearchService;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt as _;

/// Bearer token the rig signs the test user in with.
const TEST_TOKEN: &str = "dn-search-test-token-0000000000001";

/// Provider fakes: a dead provider that must degrade, never 500.
#[derive(Debug, Clone)]
struct FailingEnrichment;

impl EnrichmentPort for FailingEnrichment {
    fn enrich_batch(
        &self,
        _request: EnrichmentBatchRequest,
    ) -> search::ports::BoxFuture<'_, Result<EnrichmentResponse, EnrichmentPortError>> {
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
async fn seed_catalog(pool: &SqlitePool) {
    for (id, name, folded) in [
        ("artist-beyonce", "Beyoncé", "beyonce"),
        ("artist-beirut", "Beirut", "beirut"),
    ] {
        sqlx::query(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
             VALUES (?, ?, ?, 'group', 0, 0)",
        )
        .bind(id)
        .bind(name)
        .bind(folded)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO local_artist_external_identities \
         (local_artist_id, provider, provider_artist_id, decision_source, selected_at) \
         VALUES ('artist-beyonce', 'musicbrainz', '859d1e45-caf9-4e94-9e90-f17e67db2a6a', 'manual', 0)",
    )
    .execute(pool)
    .await
    .unwrap();
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
        sqlx::query(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, year, \
             grouping_source, created_at, updated_at) \
             VALUES (?, 'root-1', ?, ?, ?, ?, ?, ?, ?, 'manual', 0, 0)",
        )
        .bind(id)
        .bind(format!("group-{id}"))
        .bind(title)
        .bind(folded)
        .bind(artist_name)
        .bind(artist_folded)
        .bind(artist_id)
        .bind(year)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
             VALUES (?, 0, ?, 'main')",
        )
        .bind(id)
        .bind(artist_id)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         artist_name, artist_name_folded, album_title, album_title_folded, \
         file_format, ingest_source, imported_at, membership_source) \
         VALUES ('track-alien', 'album-renaissance', 'root-1', '/music/alien.flac', \
         'alien.flac', 'hash-1', 1000, 0, 'stat-1', 'Alien Superstar', 'alien superstar', \
         'Beyoncé', 'beyonce', 'Renaissance', 'renaissance', \
         'flac', 'scan', 0, 'manual')",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn search_deps(pool: SqlitePool, enrichment: Arc<dyn EnrichmentPort>) -> SearchDeps {
    SearchDeps::new(SearchService::new(pool), enrichment)
}

/// What a test's router needs alive: the rig its session lives in and the
/// app whose request wrapper it runs under.
type Held = (AuthRig, TestApp);

/// The search router behind the real middleware stack: session gate
/// inside, request scope outside, shared 404/405 fallbacks. Mirrors the
/// production posture in `create_app` for these paths. A test user is
/// signed in with [`TEST_TOKEN`].
async fn app(pool: SqlitePool, enrichment: Arc<dyn EnrichmentPort>) -> (axum::Router, Held) {
    let rig = AuthRig::new("reads-search").await;
    let user = rig.seed_user("ada", Role::User).await;
    rig.sign_in(&user.id, TEST_TOKEN).await;
    let scope = TestApp::start("reads-search").await;
    let app = scope
        .request_scope(
            search::routes()
                .with_state(search_deps(pool, enrichment))
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

async fn authed_app() -> (axum::Router, SqlitePool, Held) {
    let (pool, _dir) = crate::common::migrated_pool("reads-search").await;
    seed_catalog(&pool).await;
    let (app, held) = app(
        pool.clone(),
        Arc::new(search::ports::UnconfiguredEnrichment),
    )
    .await;
    (app, pool, held)
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

#[tokio::test]
async fn unified_search_returns_ranked_buckets_with_tops() {
    let (app, _, _held) = authed_app().await;
    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search?q=beyonce",
        Some(TEST_TOKEN),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body["artists"][0]["title"], "Beyoncé");
    assert_eq!(body["artists"][0]["kind"], "artist");
    assert_eq!(body["artists"][0]["score"], 100);
    assert_eq!(
        body["artists"][0]["musicbrainz_id"],
        "859d1e45-caf9-4e94-9e90-f17e67db2a6a"
    );
    assert_eq!(body["artists"][0]["in_library"], true);
    assert_eq!(body["artists"][0]["requested"], false);
    assert_eq!(body["top_artist"]["title"], "Beyoncé");
    assert_eq!(body["albums"][0]["title"], "Renaissance");
    assert_eq!(body["albums"][0]["artist"], "Beyoncé");
    assert_eq!(body["albums"][0]["year"], 2022);
    assert_eq!(body["tracks"][0]["title"], "Alien Superstar");
    assert_eq!(body["tracks"][0]["track"]["album_id"], "album-renaissance");
    assert_eq!(body["tracks"][0]["track"]["album_title"], "Renaissance");
    assert_eq!(body["tracks"][0]["track"]["format"], "flac");
    assert!(body["artists"][0]["track"].is_null());
    assert!(body["top_track"].is_null());
}

/// Insert one track; `availability` other than `indexed` must never match.
async fn seed_track(
    pool: &SqlitePool,
    id: &str,
    album_id: &str,
    title: &str,
    album_title: &str,
    availability: &str,
) {
    sqlx::query(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         artist_name, artist_name_folded, album_title, album_title_folded, duration_seconds, \
         file_format, availability, ingest_source, imported_at, membership_source) \
         VALUES (?, ?, 'root-1', ?, ?, ?, 1000, 0, 'stat-1', ?, lower(?), \
         'Someone', 'someone', ?, lower(?), 61.5, 'mp3', ?, 'scan', 0, 'manual')",
    )
    .bind(id)
    .bind(album_id)
    .bind(format!("/music/{id}.mp3"))
    .bind(format!("{id}.mp3"))
    .bind(format!("hash-{id}"))
    .bind(title)
    .bind(title)
    .bind(album_title)
    .bind(album_title)
    .bind(availability)
    .execute(pool)
    .await
    .unwrap();
}

/// A song title finds the song, ranked exact title, title prefix, title
/// substring, then songs matched only through their album; each carries
/// what playing it and linking its album need. Tied titles page stably,
/// a missing file never matches, and a query with no word of three
/// characters matches title prefixes.
#[tokio::test]
async fn a_song_title_finds_the_song() {
    let (app, pool, _held) = authed_app().await;
    seed_track(
        &pool,
        "t-opening",
        "album-gulag",
        "Opening",
        "Intro Sessions",
        "indexed",
    )
    .await;
    seed_track(
        &pool,
        "t-song",
        "album-gulag",
        "The Intro Song",
        "Gulag Orkestar",
        "indexed",
    )
    .await;
    seed_track(
        &pool,
        "t-reprise",
        "album-gulag",
        "Intro (Reprise)",
        "Gulag Orkestar",
        "indexed",
    )
    .await;
    seed_track(
        &pool,
        "t-intro-r",
        "album-renaissance",
        "Intro",
        "Renaissance",
        "indexed",
    )
    .await;
    seed_track(
        &pool,
        "t-intro-g",
        "album-gulag",
        "Intro",
        "Gulag Orkestar",
        "indexed",
    )
    .await;
    seed_track(
        &pool,
        "t-gone",
        "album-gulag",
        "Intro",
        "Gulag Orkestar",
        "missing",
    )
    .await;

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

#[tokio::test]
async fn search_ignores_accents_and_case() {
    let (app, _, _held) = authed_app().await;
    for query in ["beyonce", "BEYONCE", "Beyoncé", "BEYONCÉ"] {
        let uri = format!("/api/v3/search?{}", urlencode_query(query));
        let (status, _, bytes) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK, "{query}");
        let body = json(&bytes);
        assert_eq!(body["artists"][0]["title"], "Beyoncé", "{query}");
    }
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
async fn search_blank_query_is_a_400_envelope() {
    let (app, _, _held) = authed_app().await;
    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search?q=%20%20",
        Some(TEST_TOKEN),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = json(&bytes);
    assert_eq!(
        body,
        serde_json::json!({"error": {"code": "INVALID_INPUT", "message": "Query must not be blank", "details": null}})
    );
    assert_screaming_snake(body["error"]["code"].as_str().unwrap());
}

#[tokio::test]
async fn suggest_merges_buckets_best_first_and_caps() {
    let (app, _, _held) = authed_app().await;
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search/suggest?q=bey",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    let kinds: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"artist"));
    assert!(kinds.contains(&"album"));
    let scores: Vec<i64> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["score"].as_i64().unwrap())
        .collect();
    assert!(scores.windows(2).all(|pair| pair[0] >= pair[1]));

    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/suggest?q=bey&limit=1",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["results"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn bucket_drilldown_pages_with_top_only_on_first_page() {
    let (app, _, _held) = authed_app().await;
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search/artists?q=be&limit=1&offset=0",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body["bucket"], "artists");
    assert_eq!(body["limit"], 1);
    assert_eq!(body["offset"], 0);
    assert_eq!(body["results"].as_array().unwrap().len(), 1);
    assert_eq!(body["results"][0]["title"], "Beirut");
    assert_eq!(body["top_result"]["title"], "Beirut");

    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/artists?q=be&limit=1&offset=1",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body["results"][0]["title"], "Beyoncé");
    assert!(body["top_result"].is_null());
}

#[tokio::test]
async fn enrich_failure_degrades_inside_a_200() {
    let (pool, _dir) = crate::common::migrated_pool("reads-search").await;
    let (app, _held) = app(pool, Arc::new(FailingEnrichment)).await;
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
async fn auth_matrix_row_401_without_bearer() {
    let (app, _, _held) = authed_app().await;
    for uri in [
        "/api/v3/search?q=beyonce",
        "/api/v3/search/suggest?q=bey",
        "/api/v3/search/artists?q=bey",
    ] {
        let (status, headers, bytes) = call(app.clone(), Method::GET, uri, None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(headers.get("www-authenticate").unwrap(), "Bearer", "{uri}");
        assert_eq!(json(&bytes)["error"]["code"], "UNAUTHORIZED", "{uri}");
    }
    let (status, _, _) = call(
        app.clone(),
        Method::POST,
        "/api/v3/search/enrich/batch",
        None,
        Some(serde_json::json!({"artists": [], "albums": []})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search?q=beyonce",
        Some("bogus-token"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = call(
        app,
        Method::GET,
        "/api/v3/search?q=beyonce",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn leak_db_failure_renders_fixed_500_with_error_id() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let (app, _held) = app(pool, Arc::new(search::ports::UnconfiguredEnrichment)).await;
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
async fn rebuild_tracks(pool: &SqlitePool) {
    let table: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'local_tracks'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let dependents: Vec<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE tbl_name = 'local_tracks' \
         AND type IN ('index', 'trigger') AND sql IS NOT NULL",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let columns: String = sqlx::query_scalar(
        "SELECT group_concat(name, ', ') FROM pragma_table_info('local_tracks')",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let create = table.replacen(
        "CREATE TABLE local_tracks",
        "CREATE TABLE local_tracks_rebuilt",
        1,
    );
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql("PRAGMA foreign_keys = OFF; PRAGMA legacy_alter_table = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::raw_sql(&format!(
        "BEGIN; {create}; \
         INSERT INTO local_tracks_rebuilt ({columns}) SELECT {columns} FROM local_tracks; \
         DROP TABLE local_tracks; \
         ALTER TABLE local_tracks_rebuilt RENAME TO local_tracks; {}; COMMIT",
        dependents.join("; ")
    ))
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::raw_sql("PRAGMA legacy_alter_table = OFF; PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
}

/// Track search answers the same after the database is vacuumed and after
/// the track table is rebuilt over the gaps deleted tracks left: the
/// search index never names a track by a number a copy reassigns.
#[tokio::test]
async fn track_search_survives_vacuum_and_a_table_rebuild() {
    let (pool, _dir) = crate::common::migrated_pool("reads-search-rebuild").await;
    seed_catalog(&pool).await;
    let words = ["amber", "cobalt", "dune"];
    for step in 0..30 {
        let id = format!("t-needle-{step:02}");
        let title = format!("Needle {step:02} {}", words[step % 3]);
        seed_track(
            &pool,
            &id,
            "album-gulag",
            &title,
            "Gulag Orkestar",
            "indexed",
        )
        .await;
    }
    sqlx::query(
        "DELETE FROM local_tracks WHERE id IN ('t-needle-04', 't-needle-05', \
         't-needle-11', 't-needle-12', 't-needle-13', 't-needle-20')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (app, _held) = app(
        pool.clone(),
        Arc::new(search::ports::UnconfiguredEnrichment),
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

    sqlx::query("VACUUM").execute(&pool).await.unwrap();
    for (query, expected) in queries.iter().zip(&before) {
        assert_eq!(
            &track_hits(&app, query).await,
            expected,
            "{query} after VACUUM"
        );
    }

    rebuild_tracks(&pool).await;
    for (query, expected) in queries.iter().zip(&before) {
        assert_eq!(
            &track_hits(&app, query).await,
            expected,
            "{query} after a table rebuild"
        );
    }
}
