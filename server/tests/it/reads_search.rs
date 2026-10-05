//! Unified search briefs: shape, fold matching, suggest leniency, bucket
//! drill-down, the single enrich-batch method, the auth matrix row, and
//! the 5xx-leak boundary.
//!
//! This target includes the module by path and mounts its router directly
//! behind the real session gate and request-scope middleware. App wiring
//! landed separately (`ReadsSetup::search_router` merges the same router
//! into the gated tree).
//! Fakes exist only for the enrichment provider port, never for storage:
//! every search brief runs against a migrated in-memory 0001 database.
//!
//! Stage-5 boundary: the enrichment fakes swap to real providers; canned
//! values pin handler mapping, not provider data.

use droppedneedle::reads::search;

use crate::common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    middleware,
};
use droppedneedle::auth::session::middleware::{SessionAuth, require_session};
use droppedneedle::auth::session::store::{MemorySessionStore, SessionRecord, SessionStore};
use droppedneedle::auth::session::tokens::hash_token;
use droppedneedle::middleware::request_scope;
use droppedneedle::schema::apply_migrations;
use search::SearchDeps;
use search::models::{
    AlbumEnrichment, ArtistEnrichment, EnrichmentBatchRequest, EnrichmentResponse, EnrichmentSource,
};
use search::ports::{EnrichmentPort, EnrichmentPortError};
use search::service::SearchService;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt as _;

/// Bearer token the rig mints a session for.
const TEST_TOKEN: &str = "dn-search-brief-token-000000000001";

/// Provider fakes: canned counts from a healthy provider.
#[derive(Debug, Clone)]
struct CannedEnrichment;

impl EnrichmentPort for CannedEnrichment {
    fn enrich_batch(
        &self,
        request: EnrichmentBatchRequest,
    ) -> search::ports::BoxFuture<'_, Result<EnrichmentResponse, EnrichmentPortError>> {
        Box::pin(async move {
            Ok(EnrichmentResponse {
                artists: request
                    .artists
                    .into_iter()
                    .map(|item| ArtistEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        release_group_count: Some(12),
                        listen_count: Some(4_000_000),
                    })
                    .collect(),
                albums: request
                    .albums
                    .into_iter()
                    .map(|item| AlbumEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        track_count: Some(16),
                        listen_count: Some(90_000),
                    })
                    .collect(),
                source: EnrichmentSource::Listenbrainz,
                degradations: Vec::new(),
            })
        })
    }
}

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

/// One-connection in-memory pool, migrated to the 0001 baseline.
async fn migrated_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    apply_migrations(&pool).await.unwrap();
    pool
}

/// Fixture catalog: two artists plus an album each plus one track.
/// Queries in the briefs avoid the `various artists` / `unknown artist`
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

/// Session store holding one valid standard session for the test token.
async fn session_auth() -> SessionAuth<MemorySessionStore> {
    let store = MemorySessionStore::new();
    let now = droppedneedle::auth::session::store::now_unix();
    store
        .insert(SessionRecord {
            id: "session-1".to_owned(),
            user_id: "user-1".to_owned(),
            token_hash: hash_token(TEST_TOKEN),
            kind: droppedneedle::auth::session::store::SessionKind::Standard,
            label: None,
            issued_at: now,
            expires_at: now + 3600,
            last_seen_at: now,
            revoked: false,
            user_agent: None,
        })
        .await
        .unwrap();
    SessionAuth::new(store, "")
}

fn search_deps(pool: SqlitePool, enrichment: Arc<dyn EnrichmentPort>) -> SearchDeps {
    SearchDeps::new(
        SearchService::new(pool),
        enrichment,
        Arc::new(common::FixedIdGenerator::new(common::FIXED_ID)),
    )
}

/// The search router behind the real middleware stack: session gate
/// inside, request scope outside, shared 404/405 fallbacks. Mirrors the
/// production posture in `create_app` for these paths.
async fn app(pool: SqlitePool, enrichment: Arc<dyn EnrichmentPort>) -> axum::Router {
    search::router(search_deps(pool, enrichment))
        .layer(middleware::from_fn_with_state(
            session_auth().await,
            require_session,
        ))
        .layer(middleware::from_fn_with_state(
            common::hooked_state(),
            request_scope,
        ))
        .fallback(droppedneedle::handlers::fallback_404)
        .method_not_allowed_fallback(droppedneedle::handlers::fallback_405)
}

async fn authed_app() -> (axum::Router, SqlitePool) {
    let pool = migrated_pool().await;
    seed_catalog(&pool).await;
    let app = app(
        pool.clone(),
        Arc::new(search::ports::UnconfiguredEnrichment),
    )
    .await;
    (app, pool)
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
    let (app, _) = authed_app().await;
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
    assert!(body["top_track"].is_null());
}

#[tokio::test]
async fn search_responses_carry_ok_statuses_while_local_only() {
    let (app, _) = authed_app().await;
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search?q=beyonce",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body["artist_status"], "ok");
    assert_eq!(body["album_status"], "ok");
    assert_eq!(body["track_status"], "ok");

    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search/artists?q=beyonce",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["status"], "ok");

    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search/suggest?q=beyonce",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json(&bytes)["status"], "ok");

    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/suggest?q=x",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body["status"], "ok");
    assert!(body["results"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn search_ignores_accents_and_case() {
    let (app, _) = authed_app().await;
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
    let (app, _) = authed_app().await;
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
async fn buckets_filter_selects_buckets_and_rejects_unknown() {
    let (app, _) = authed_app().await;
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search?q=beyonce&buckets=tracks",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert!(body["artists"].as_array().unwrap().is_empty());
    assert!(body["albums"].as_array().unwrap().is_empty());
    assert_eq!(body["tracks"][0]["title"], "Alien Superstar");

    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search?q=beyonce&buckets=nope",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json(&bytes)["error"]["code"], "INVALID_INPUT");
}

#[tokio::test]
async fn suggest_short_query_returns_empty_200() {
    let (app, _) = authed_app().await;
    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/suggest?q=x",
        Some(TEST_TOKEN),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json(&bytes),
        serde_json::json!({"results": [], "status": "ok"})
    );
}

#[tokio::test]
async fn suggest_merges_buckets_best_first_and_caps() {
    let (app, _) = authed_app().await;
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
    let (app, _) = authed_app().await;
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
async fn bucket_unknown_is_a_404_envelope() {
    let (app, _) = authed_app().await;
    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/playlists?q=bey",
        Some(TEST_TOKEN),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        json(&bytes),
        serde_json::json!({"error": {"code": "NOT_FOUND", "message": "Not found", "details": null}})
    );
}

#[tokio::test]
async fn enrich_batch_single_method_serves_both_buckets() {
    let pool = migrated_pool().await;
    let app = app(pool, Arc::new(CannedEnrichment)).await;
    let body = serde_json::json!({
        "artists": [{"musicbrainz_id": "artist-mbid-1", "name": "Beyoncé"}],
        "albums": [{"musicbrainz_id": "album-mbid-1", "artist_name": "Beyoncé", "album_name": "Renaissance"}],
    });
    let (status, _, bytes) = call(
        app.clone(),
        Method::POST,
        "/api/v3/search/enrich/batch",
        Some(TEST_TOKEN),
        Some(body),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let response = json(&bytes);
    assert_eq!(response["source"], "listenbrainz");
    assert_eq!(response["artists"][0]["musicbrainz_id"], "artist-mbid-1");
    assert_eq!(response["artists"][0]["listen_count"], 4_000_000);
    assert_eq!(response["albums"][0]["musicbrainz_id"], "album-mbid-1");
    assert_eq!(response["albums"][0]["track_count"], 16);
    assert!(response["degradations"].as_array().unwrap().is_empty());

    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/enrich/batch",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        json(&bytes),
        serde_json::json!({"error": {"code": "METHOD_NOT_ALLOWED", "message": "Method not allowed", "details": null}})
    );
}

#[tokio::test]
async fn enrich_batch_caps_per_bucket_and_drops_blank_ids() {
    let pool = migrated_pool().await;
    let app = app(pool, Arc::new(search::ports::UnconfiguredEnrichment)).await;
    let artists: Vec<serde_json::Value> = (0..12)
        .map(|index| serde_json::json!({"musicbrainz_id": format!("artist-{index}")}))
        .chain([serde_json::json!({"musicbrainz_id": "   "})])
        .collect();
    let (status, _, bytes) = call(
        app,
        Method::POST,
        "/api/v3/search/enrich/batch",
        Some(TEST_TOKEN),
        Some(serde_json::json!({"artists": artists, "albums": []})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let response = json(&bytes);
    assert_eq!(response["source"], "none");
    let returned = response["artists"].as_array().unwrap();
    assert_eq!(returned.len(), search::ports::MAX_ENRICHMENT_PER_BUCKET);
    assert!(returned.iter().all(|item| item["listen_count"].is_null()));
}

#[tokio::test]
async fn enrich_failure_degrades_inside_a_200() {
    let pool = migrated_pool().await;
    let app = app(pool, Arc::new(FailingEnrichment)).await;
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
    assert_eq!(response["source"], "none");
    assert_eq!(response["artists"][0]["musicbrainz_id"], "artist-1");
    assert!(response["artists"][0]["listen_count"].is_null());
    assert_eq!(
        response["degradations"],
        serde_json::json!([{
            "source": "listenbrainz",
            "code": "ENRICHMENT_UNAVAILABLE",
            "message": "Enrichment temporarily unavailable",
        }])
    );
    let raw = String::from_utf8(bytes).unwrap();
    assert!(!raw.contains("lb.internal"));
    assert!(!raw.contains("critique-brainz"));
}

#[tokio::test]
async fn auth_matrix_row_401_without_bearer() {
    let (app, _) = authed_app().await;
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
    let app = app(pool, Arc::new(search::ports::UnconfiguredEnrichment)).await;
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
    assert_eq!(body["error"]["details"]["error_id"], common::FIXED_ID);
    assert_eq!(headers.get("x-request-id").unwrap(), common::FIXED_ID);
    let raw = String::from_utf8(bytes).unwrap();
    assert!(!raw.contains("local_artists"));
    assert!(!raw.contains("no such table"));
}
