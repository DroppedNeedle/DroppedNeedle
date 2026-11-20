//! Unified search over a migrated database behind the real
//! session gate: ranked buckets, accent and case folding, drill-down
//! paging, suggest, enrichment that degrades inside a 200 and the fixed
//! 500 on a database fault.

use crate::common::Probe;
use droppedneedle_catalog::search;

use crate::common::TestApp;
use crate::common::auth::AuthRig;

use std::sync::Arc;
use std::time::Duration;

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
use droppedneedle_kernel::runtime_config::Secret;
use droppedneedle_kernel::runtime_config::secret_sections::{AdvancedSettings, SpotifySettings};
use droppedneedle_kernel::runtime_config::sections::{
    MbSourceMode, MusicBrainzSettings, UserPreferences,
};
use droppedneedle_providers::Providers;
use droppedneedle_providers::Wire;
use droppedneedle_providers::executor::{Body as WireBody, Prepared, WireAnswer, WireError};
use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
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
/// with one, the official MusicBrainz answers on it, and ListenBrainz
/// too when `settings` turn it on.
fn search_deps(
    app_db: PathBuf,
    enrichment: Arc<dyn EnrichmentPort>,
    users: droppedneedle_auth::users::UsersDeps,
    wire: Option<Arc<dyn Wire>>,
    settings: OfficialMusicBrainz,
) -> SearchDeps {
    let db = droppedneedle_kernel::db::Db::open_for_tests(&app_db).expect("search database opens");
    let remote = match wire {
        Some(wire) => scripted_catalog(&db, users, wire, settings),
        None => crate::common::bundles::offline_catalog(&db, users),
    };
    SearchDeps::new(SearchService::new(db, remote), enrichment)
}

/// A metadata catalog over `db` reading the official MusicBrainz, which
/// the closed provider endpoints put at [`MUSICBRAINZ`] on `wire`, and
/// ListenBrainz at [`LISTENBRAINZ`] when `settings` turn it on.
fn scripted_catalog(
    db: &droppedneedle_kernel::db::Db,
    users: droppedneedle_auth::users::UsersDeps,
    wire: Arc<dyn Wire>,
    settings: OfficialMusicBrainz,
) -> Catalog {
    scripted_catalog_over(db, users, Arc::new(Providers::unpaced(wire)), settings)
}

/// [`scripted_catalog`] over `providers`, which the caller may share.
fn scripted_catalog_over(
    db: &droppedneedle_kernel::db::Db,
    users: droppedneedle_auth::users::UsersDeps,
    providers: Arc<Providers>,
    settings: OfficialMusicBrainz,
) -> Catalog {
    Catalog::new(
        Upstream::new(
            providers,
            Arc::new(settings),
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

/// Where ListenBrainz answers in [`scripted_catalog`].
const LISTENBRAINZ: &str = "http://127.0.0.1:9/listenbrainz/1";

/// Catalog settings on the official MusicBrainz, everything else at its
/// default, with ListenBrainz reads on or off, and a Spotify app and
/// YouTube on or off (`links`).
struct OfficialMusicBrainz {
    listenbrainz: bool,
    links: bool,
}

impl CatalogSettings for OfficialMusicBrainz {
    fn musicbrainz(&self) -> Option<MusicBrainzSettings> {
        // A fixed source id, as stored settings have: the default mints a
        // new one each time, which would give every search its own cache.
        Some(MusicBrainzSettings {
            source_mode: MbSourceMode::Official,
            source_id: "official-test".to_owned(),
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
        self.listenbrainz
    }

    fn store_region(&self) -> Option<String> {
        None
    }

    fn spotify_app(&self) -> Option<SpotifySettings> {
        self.links.then(|| SpotifySettings {
            client_id: "test-client".to_owned(),
            client_secret: Secret::new("test-secret"),
            enabled: true,
            spotify_redirect_origin: String::new(),
        })
    }

    fn youtube_on(&self) -> bool {
        self.links
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
    let wire = wire.map(|wire| wire as Arc<dyn Wire>);
    app_listening(app_db, enrichment, wire, false).await
}

/// [`app_on`] with ListenBrainz reads on or off.
async fn app_listening(
    app_db: PathBuf,
    enrichment: Arc<dyn EnrichmentPort>,
    wire: Option<Arc<dyn Wire>>,
    listenbrainz: bool,
) -> (axum::Router, Held) {
    let settings = OfficialMusicBrainz {
        listenbrainz,
        links: true,
    };
    app_with(app_db, enrichment, wire, settings).await
}

/// [`app_on`] with these catalog settings.
async fn app_with(
    app_db: PathBuf,
    enrichment: Arc<dyn EnrichmentPort>,
    wire: Option<Arc<dyn Wire>>,
    settings: OfficialMusicBrainz,
) -> (axum::Router, Held) {
    let rig = AuthRig::new("reads-search").await;
    let user = rig.seed_user("ada", Role::User).await;
    rig.sign_in(&user.id, TEST_TOKEN).await;
    let scope = TestApp::start("reads-search").await;
    let app = scope
        .request_scope(
            search::routes()
                .with_state(search_deps(
                    app_db,
                    enrichment,
                    rig.deps.clone(),
                    wire,
                    settings,
                ))
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

/// The search router with ListenBrainz reads on and the real search
/// counts the web UI's enrichment asks for, both over one set of providers
/// on `wire`, so what enrichment keeps is what search ranks by.
async fn app_counting(app_db: PathBuf, wire: Arc<dyn Wire>) -> (axum::Router, Held) {
    let rig = AuthRig::new("reads-search").await;
    let user = rig.seed_user("ada", Role::User).await;
    rig.sign_in(&user.id, TEST_TOKEN).await;
    let db = droppedneedle_kernel::db::Db::open_for_tests(&app_db).expect("search database opens");
    let providers = Arc::new(Providers::unpaced(wire));
    let counts = droppedneedle_catalog::search::enrichment::SearchCounts::new(
        &providers,
        rig.config.live(),
        rig.config.live(),
        droppedneedle_media::playback::listening::ListeningStore::new(
            db.clone(),
            rig.deps.crypto.clone(),
            rig.deps.clock.clone(),
        ),
        rig.deps.clone(),
    );
    let settings = OfficialMusicBrainz {
        listenbrainz: true,
        links: true,
    };
    let catalog = scripted_catalog_over(&db, rig.deps.clone(), providers, settings);
    let scope = TestApp::start("reads-search").await;
    let app = scope
        .request_scope(
            search::routes()
                .with_state(SearchDeps::new(
                    SearchService::new(db, catalog),
                    Arc::new(counts),
                ))
                .into_router()
                .layer(middleware::from_fn_with_state(
                    rig.session_gate(),
                    require_session,
                )),
        )
        .fallback(droppedneedle::handlers::fallback_404);
    (app, (rig, scope))
}

/// The enrichment the web UI asks for an album bucket's counts.
async fn enrich_albums(app: axum::Router, albums: &[&str]) {
    let albums: Vec<serde_json::Value> = albums
        .iter()
        .map(|mbid| serde_json::json!({"musicbrainz_id": mbid}))
        .collect();
    let (status, _, _) = call(
        app,
        Method::POST,
        "/api/v3/search/enrich/batch",
        Some(TEST_TOKEN),
        Some(serde_json::json!({ "albums": albums })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
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

/// A library album hit names its release group, except a guess its files'
/// tags contradict: that one owns no group, so it opens as itself.
#[tokio::test]
async fn album_hits_give_no_group_for_a_guess_the_files_contradict() {
    let (app, app_db, _held) = authed_app().await;
    seed_track(
        &app_db,
        "t-gulag",
        "album-gulag",
        "Brandenburg",
        "Gulag Orkestar",
        "indexed",
    );
    let (matched, guessed) = (
        "0a0b0c0d-0001-4000-8000-000000000001",
        "0a0b0c0d-0001-4000-8000-000000000002",
    );
    app_db.exec(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at) VALUES \
         ('album-renaissance', 'musicbrainz', ?1, 'automatic', 0), \
         ('album-gulag', 'musicbrainz', ?2, 'automatic', 0)",
        rusqlite::params![matched, guessed],
    );
    app_db.exec(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         candidates_json, updated_at) VALUES \
         ('album-gulag', 'unconfirmed', 'NAMED_GROUP_DIFFERS', '[]', 0)",
        [],
    );

    let mut groups = Vec::new();
    for query in ["renaissance", "gulag"] {
        let uri = format!("/api/v3/search?q={query}&scope=library");
        let (status, _, bytes) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK);
        let hit = json(&bytes)["albums"][0].clone();
        groups.push((
            hit["id"].as_str().unwrap().to_owned(),
            hit["musicbrainz_id"].clone(),
        ));
    }
    assert_eq!(
        groups,
        vec![
            ("album-renaissance".to_owned(), serde_json::json!(matched)),
            ("album-gulag".to_owned(), serde_json::Value::Null),
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
    // Suggest reads the full search's first page of "the beatles".
    let expected = ["the beatles", "queen", "ok computer"];
    assert_eq!(
        asked.len(),
        expected.len(),
        "one album search per query: {asked:?}"
    );
    for (sent, query) in asked.iter().zip(expected) {
        assert!(
            sent.contains(&format!("\"{query}\"")),
            "{sent} answers {query}"
        );
    }
}

/// One recording search hit: id, title, credited artist, score, and the
/// releases carrying it as (release id, title, status, group id).
fn recording_hit(
    id: &str,
    title: &str,
    artist: &str,
    score: u32,
    releases: &[(&str, &str, &str, &str)],
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "score": score,
        "title": title,
        "length": 248_000,
        "artist-credit": [{
            "name": artist,
            "joinphrase": "",
            "artist": {"id": format!("artist-{artist}"), "name": artist},
        }],
        "releases": releases.iter().map(|(release, title, status, group)| serde_json::json!({
            "id": release,
            "title": title,
            "status": status,
            "date": "2018-10-18",
            "release-group": {"id": group, "title": title, "primary-type": "Single"},
        })).collect::<Vec<_>>(),
    })
}

/// Songs the library lacks come from MusicBrainz: each row carries what
/// requesting it needs, under its first official release, and an open
/// request shows as requested. A recording the library holds attaches
/// the library's copy and plays it; one on no release, or a video, is left
/// out. The songs tab pages the same way and says how MusicBrainz did.
#[tokio::test]
async fn songs_the_library_lacks_come_from_musicbrainz() {
    let wire = Arc::new(ScriptedWire::new());
    let hits = vec![
        recording_hit(
            "rec-sunflower",
            "Sunflower",
            "Post Malone",
            100,
            &[
                ("rel-promo", "Sunflower (promo)", "Promotion", "rg-promo"),
                ("rel-single", "Sunflower", "Official", "rg-single"),
            ],
        ),
        recording_hit(
            "REC-OWNED",
            "Sunflower",
            "Someone",
            99,
            &[("rel-owned", "Renaissance", "Official", "rg-owned")],
        ),
        recording_hit(
            "rec-requested",
            "Sunflower (remix)",
            "Post Malone",
            90,
            &[("rel-remix", "Sunflower (remix)", "Official", "rg-remix")],
        ),
        recording_hit("rec-loose", "Sunflower", "Nobody", 80, &[]),
        serde_json::json!({
            "id": "rec-video", "score": 80, "title": "Sunflower", "video": true,
            "releases": [{"id": "rel-video", "title": "Sunflower", "status": "Official"}],
        }),
    ];
    for _ in 0..2 {
        wire.answer(
            &format!("{MUSICBRAINZ}/recording"),
            200,
            &[],
            search_answer("recordings", hits.clone()),
        );
    }
    let (app_db, _dir) = crate::common::migrated_file("reads-search-songs");
    seed_catalog(&app_db);
    seed_track(
        &app_db,
        "t-owned",
        "album-renaissance",
        "Sunflower",
        "Renaissance",
        "indexed",
    );
    app_db.exec(
        "INSERT INTO local_track_external_identities (local_track_id, provider, recording_mbid, \
         decision_source, selected_at) VALUES ('t-owned', 'musicbrainz', 'rec-owned', 'manual', 0)",
        [],
    );
    app_db.exec(
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status, request_kind) \
         VALUES ('track:rec-requested', 'rec-requested', 'Post Malone', 'Sunflower (remix)', \
         1700000000000, 'pending', 'track')",
        [],
    );
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire),
    )
    .await;

    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search?q=sunflower&buckets=tracks",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body["track_status"], "ok");
    let rows: Vec<(&str, Option<&str>, bool, bool)> = body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| {
            (
                hit["musicbrainz_id"].as_str().unwrap(),
                hit["id"].as_str(),
                hit["in_library"].as_bool().unwrap(),
                hit["requested"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("REC-OWNED", Some("t-owned"), true, false),
            ("rec-sunflower", None, false, false),
            ("rec-requested", None, false, true),
        ],
        "{body}"
    );
    let owned = &body["tracks"][0];
    assert_eq!(owned["track"]["album_id"], "album-renaissance");
    assert!(owned["remote_track"].is_null());
    let remote = &body["tracks"][1];
    assert_eq!(remote["kind"], "track");
    assert_eq!(remote["artist"], "Post Malone");
    assert_eq!(remote["year"], 2018);
    assert!(remote["track"].is_null());
    assert_eq!(remote["remote_track"]["release_mbid"], "rel-single");
    assert_eq!(remote["remote_track"]["release_group_mbid"], "rg-single");
    assert_eq!(remote["remote_track"]["album_title"], "Sunflower");
    assert_eq!(remote["remote_track"]["duration_seconds"], 248);
    assert_eq!(remote["remote_track"]["artist_mbid"], "artist-Post Malone");

    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/tracks?q=sunflower&limit=2",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(body["status"], "ok");
    let ids: Vec<&str> = body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["musicbrainz_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["REC-OWNED", "rec-sunflower"], "{body}");
}

/// A song the library holds never shows as one to request: its MusicBrainz
/// hit takes the library's copy by the recording id pages read (the tags'
/// when no match row names one), even when the copy's own title is not
/// what was searched. Two files of one recording stay two rows, the hit
/// folding into one of them, and the library scope keeps both too.
#[tokio::test]
async fn owned_songs_show_their_library_copy_and_copies_stay_apart() {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        search_answer(
            "recordings",
            vec![
                recording_hit(
                    "rec-twice",
                    "Sunflower",
                    "Post Malone",
                    100,
                    &[("rel-a", "Hollywood's Bleeding", "Official", "rg-a")],
                ),
                recording_hit(
                    "rec-tagged",
                    "Sunflower",
                    "Post Malone",
                    95,
                    &[("rel-b", "Spider-Verse", "Official", "rg-b")],
                ),
            ],
        ),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-owned-songs");
    seed_catalog(&app_db);
    for (id, title) in [
        ("t-one", "Sunflower"),
        ("t-two", "Sunflower"),
        ("t-tagged", "Girassol"),
    ] {
        seed_track(
            &app_db,
            id,
            "album-renaissance",
            title,
            "Renaissance",
            "indexed",
        );
    }
    for id in ["t-one", "t-two"] {
        app_db.exec(
            "INSERT INTO local_track_external_identities (local_track_id, provider, \
             recording_mbid, decision_source, selected_at) \
             VALUES (?, 'musicbrainz', 'rec-twice', 'manual', 0)",
            [id],
        );
    }
    app_db.exec(
        "UPDATE local_tracks SET embedded_recording_mbid = 'rec-tagged' WHERE id = 't-tagged'",
        [],
    );
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire),
    )
    .await;

    let rows = |body: &serde_json::Value| -> Vec<(String, String, bool)> {
        let mut rows: Vec<(String, String, bool)> = body["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| {
                (
                    hit["id"].as_str().unwrap_or("none").to_owned(),
                    hit["musicbrainz_id"].as_str().unwrap_or("").to_owned(),
                    hit["remote_track"].is_null(),
                )
            })
            .collect();
        rows.sort();
        rows
    };
    let (status, _, bytes) = call(
        app.clone(),
        Method::GET,
        "/api/v3/search?q=sunflower&buckets=tracks",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(
        rows(&body),
        vec![
            ("t-one".to_owned(), "rec-twice".to_owned(), true),
            ("t-tagged".to_owned(), "rec-tagged".to_owned(), true),
            ("t-two".to_owned(), "rec-twice".to_owned(), true),
        ],
        "{body}"
    );

    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search?q=sunflower&buckets=tracks&scope=library",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = json(&bytes);
    assert_eq!(
        rows(&body),
        vec![
            ("t-one".to_owned(), "rec-twice".to_owned(), true),
            ("t-two".to_owned(), "rec-twice".to_owned(), true),
        ],
        "{body}"
    );
}

/// MusicBrainz's songs never hold a full search: once the library and the
/// artist and album legs are in, a songs leg still out leaves the page
/// answering with the songs bucket `partial`, long before the deadline.
#[tokio::test]
async fn a_slow_songs_leg_does_not_hold_the_page() {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{MUSICBRAINZ}/artist"),
        200,
        &[],
        search_answer("artists", vec![artist_hit("Sunflower Bean", 100)]),
    );
    wire.answer(
        &format!("{MUSICBRAINZ}/release-group"),
        200,
        &[],
        search_answer(
            "release-groups",
            vec![group_hit((
                "rg-sun",
                "Sunflower",
                "Someone",
                "Album",
                100,
                1,
            ))],
        ),
    );
    wire.on(
        droppedneedle_providers::executor::Method::Get,
        &format!("{MUSICBRAINZ}/recording"),
        droppedneedle_providers::test_support::Scripted::Hang,
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-slow-songs");
    seed_catalog(&app_db);
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire),
    )
    .await;

    let started = std::time::Instant::now();
    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search?q=sunflower",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        started.elapsed() < search::service::FULL_SEARCH_DEADLINE / 2,
        "took {:?}",
        started.elapsed()
    );
    let body = json(&bytes);
    assert_eq!(body["track_status"], "partial", "{body}");
    assert_eq!(body["album_status"], "ok", "{body}");
    assert_eq!(body["albums"][0]["musicbrainz_id"], "rg-sun", "{body}");
}

/// Titles tagged with MusicBrainz's hyphen (U+2010) match a query typed
/// with the keyboard hyphen: an album whose title is the query scores and
/// headlines as exact, and a song is found through the song index. A word
/// with a dash keeps every piece: "t-pain" never finds a song that only
/// says "pain".
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
    seed_track(
        &app_db,
        "t-feat",
        "album-verse",
        "Sunday (feat. T\u{2010}Pain)",
        "Across the Spider\u{2010}Verse",
        "indexed",
    );
    seed_track(
        &app_db,
        "t-pain",
        "album-verse",
        "Growing Pains",
        "Across the Spider\u{2010}Verse",
        "indexed",
    );
    let get = |query: &str| {
        let (app, uri) = (
            app.clone(),
            format!("/api/v3/search?{}&scope=library", urlencode_query(query)),
        );
        async move {
            let (status, _, bytes) = call(app, Method::GET, &uri, Some(TEST_TOKEN), None).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            json(&bytes)
        }
    };
    let body = get("across the spider-verse").await;
    assert_eq!(body["albums"][0]["id"], "album-verse", "{body}");
    assert_eq!(body["albums"][0]["score"], 100, "{body}");
    assert_eq!(body["top_album"]["id"], "album-verse", "{body}");
    let body = get("spider-verse theme").await;
    assert_eq!(body["tracks"][0]["id"], "t-verse", "{body}");
    let body = get("t-pain").await;
    let songs: Vec<&str> = body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(songs, ["t-feat"], "{body}");
}

/// An album query matches word by word across the title and the album
/// artist, so "artist title" finds a local-only album; a lone dash between
/// words is not a word.
#[tokio::test]
async fn artist_and_title_words_find_a_local_only_album() {
    let (app, app_db, _held) = authed_app().await;
    seed_track(
        &app_db,
        "t-gulag",
        "album-gulag",
        "Brandenburg",
        "Gulag Orkestar",
        "indexed",
    );
    for (query, path, scope, bucket) in [
        ("beirut gulag", "/api/v3/search/albums", "", "results"),
        (
            "Beirut - Gulag Orkestar",
            "/api/v3/search",
            "&scope=library",
            "albums",
        ),
    ] {
        let uri = format!("{path}?{}{scope}", urlencode_query(query));
        let (status, _, bytes) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        let body = json(&bytes);
        assert_eq!(body[bucket][0]["id"], "album-gulag", "{uri}: {body}");
    }
}

/// The forgiving retry asks MusicBrainz again only when it matched nothing
/// at all: a later "see all" page that comes back empty is the end of the
/// list, typeahead never asks twice, and a full search reuses typeahead's
/// empty answer and asks once more, with the forgiving term.
#[tokio::test]
async fn the_forgiving_retry_runs_only_on_no_match_and_never_from_typeahead() {
    let wire = Arc::new(ScriptedWire::new());
    let url = format!("{MUSICBRAINZ}/release-group");
    let body = |count: u32| {
        serde_json::to_vec(&serde_json::json!({"count": count, "offset": 0, "release-groups": []}))
            .expect("answer encodes")
    };
    wire.answer(&url, 200, &[], body(30));
    wire.answer(&url, 200, &[], body(0));
    let (app_db, _dir) = crate::common::migrated_file("reads-search-retry-bounds");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;
    let asked = || -> Vec<String> {
        wire.sent()
            .iter()
            .filter(|sent| sent.url.path().ends_with("/release-group"))
            .filter_map(|sent| sent.query("query"))
            .collect()
    };
    for uri in [
        "/api/v3/search/albums?q=qwertyuiop&limit=24&offset=24",
        "/api/v3/search/suggest?q=qwertyuiop",
    ] {
        let (status, _, _) = call(app.clone(), Method::GET, uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
    assert_eq!(asked().len(), 2, "one call each, no retry");

    let uri = "/api/v3/search?q=qwertyuiop&buckets=albums";
    let (status, _, _) = call(app, Method::GET, uri, Some(TEST_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    let asked = asked();
    assert_eq!(
        asked.len(),
        3,
        "the empty first page is shared, then asked forgivingly: {asked:?}"
    );
    assert!(asked[2].contains("qwertyuiop~"), "{asked:?}");
}

/// One ListenBrainz popularity answer: each id under `key` with its
/// listens, `None` for an id ListenBrainz does not know.
fn popularity_answer(key: &str, counts: &[(&str, Option<i64>)]) -> Scripted {
    let rows: Vec<serde_json::Value> = counts
        .iter()
        .map(|(mbid, count)| serde_json::json!({key: mbid, "total_listen_count": count}))
        .collect();
    Scripted::Answer {
        status: 200,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: serde_json::to_vec(&rows).expect("answer encodes"),
    }
}

/// The ids each ListenBrainz popularity request on `wire` asked about, in
/// the order sent: (path, ids).
fn listens_asked(wire: &ScriptedWire) -> Vec<(String, Vec<String>)> {
    wire.sent()
        .iter()
        .filter(|sent| sent.url.path().starts_with("/listenbrainz/1/popularity/"))
        .map(|sent| {
            let body = match &sent.body {
                WireBody::Json(json) => json.clone(),
                other => panic!("popularity sent as {other:?}"),
            };
            let body: serde_json::Value = serde_json::from_slice(&body).expect("json body");
            let ids = body
                .as_object()
                .and_then(|body| body.values().next())
                .and_then(|ids| ids.as_array())
                .expect("one list of ids")
                .iter()
                .map(|id| id.as_str().expect("an id").to_owned())
                .collect();
            (sent.url.path().to_owned(), ids)
        })
        .collect()
}

/// For a ListenBrainz follower, albums and songs rank by the listens the
/// cache holds: the first search ranks by text and fetches the counts in
/// the background, and once they are kept the soundtrack everyone plays
/// goes above the namesakes MusicBrainz scores higher, an exact title
/// still ranks first, a row with no count sits below the counted ones, and
/// a song ranks by its own listens before its album's. The scores and the
/// album counts are BrainzMash's and ListenBrainz's on 2026-11-14.
#[tokio::test]
async fn popular_albums_and_songs_rank_above_namesakes() {
    use droppedneedle_providers::executor::Method as WireMethod;

    let groups: Vec<Group<'_>> = vec![
        (
            "rg-pemberton",
            "Spider-Man: Across the Spider-Verse (Original Score)",
            "Daniel Pemberton",
            "Album",
            100,
            3,
        ),
        (
            "rg-tribute",
            "Across the Spider-Verse",
            "Tribute Players",
            "Album",
            100,
            1,
        ),
        (
            "rg-metro",
            "Metro Boomin Presents Spider-Man: Across the Spider-Verse",
            "Metro Boomin",
            "Album",
            55,
            5,
        ),
        (
            "rg-unheard",
            "Across the Spider-Verse: Piano Covers",
            "Nobody",
            "Album",
            80,
            1,
        ),
    ];
    let recordings = vec![
        recording_hit(
            "rec-calling",
            "Calling (Spider-Man: Across the Spider-Verse)",
            "Metro Boomin",
            60,
            &[("rel-metro", "Metro Boomin Presents", "Official", "rg-metro")],
        ),
        recording_hit(
            "rec-score",
            "Across the Spider-Verse (Intro)",
            "Daniel Pemberton",
            100,
            &[(
                "rel-pemberton",
                "Original Score",
                "Official",
                "rg-pemberton",
            )],
        ),
        recording_hit(
            "rec-cover",
            "Across the Spider-Verse Theme (Piano Cover)",
            "Nobody",
            100,
            &[("rel-unheard", "Piano Covers", "Official", "rg-unheard")],
        ),
    ];
    let wire = Arc::new(ScriptedWire::new());
    let answer = |path: &str, key: &str, hits| {
        wire.answer(
            &format!("{MUSICBRAINZ}/{path}"),
            200,
            &[],
            search_answer(key, hits),
        );
    };
    answer("artist", "artists", Vec::new());
    answer(
        "release-group",
        "release-groups",
        groups.iter().copied().map(group_hit).collect(),
    );
    answer("recording", "recordings", recordings);
    wire.on(
        WireMethod::Post,
        &format!("{LISTENBRAINZ}/popularity/release-group"),
        popularity_answer(
            "release_group_mbid",
            &[
                ("rg-pemberton", Some(41_316)),
                ("rg-tribute", Some(2)),
                ("rg-metro", Some(442_170)),
                ("rg-unheard", None),
            ],
        ),
    );
    wire.on(
        WireMethod::Post,
        &format!("{LISTENBRAINZ}/popularity/recording"),
        popularity_answer(
            "recording_mbid",
            &[
                ("rec-calling", Some(118_000)),
                ("rec-score", Some(200_000)),
                ("rec-cover", None),
            ],
        ),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-listens");
    let (app, _held) = app_listening(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
        true,
    )
    .await;
    let get = |path: &str, extra: &str| {
        let query = urlencode_query("across the spider-verse");
        let (app, uri) = (app.clone(), format!("{path}?{query}{extra}"));
        async move {
            let (status, _, bytes) = call(app, Method::GET, &uri, Some(TEST_TOKEN), None).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            json(&bytes)
        }
    };
    let ids = |rows: &serde_json::Value| -> Vec<String> {
        rows.as_array()
            .expect("rows")
            .iter()
            .map(|row| row["musicbrainz_id"].as_str().unwrap().to_owned())
            .collect()
    };

    let ranked = ["rg-tribute", "rg-metro", "rg-pemberton", "rg-unheard"];
    // The intro's own 200k beat Calling's own 118k, though Calling's album
    // has the most listens; the cover has no count anywhere.
    let songs = ["rec-score", "rec-calling", "rec-cover"];
    let body = get("/api/v3/search", "").await;
    assert_ne!(ids(&body["albums"]), ranked, "text order first: {body}");
    let started = std::time::Instant::now();
    let body = loop {
        let body = get("/api/v3/search", "").await;
        let landed = ids(&body["albums"]) == ranked && ids(&body["tracks"]) == songs;
        if landed || started.elapsed() > Duration::from_secs(10) {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(ids(&body["albums"]), ranked, "{body}");
    assert_eq!(ids(&body["tracks"]), songs, "{body}");
}

/// When ListenBrainz cannot answer, or is off, search ranks by text as it
/// did and the answer says nothing of it: the counts only ever reorder.
#[tokio::test]
async fn search_ranks_by_text_when_listenbrainz_is_down() {
    use droppedneedle_providers::TransportKind;
    use droppedneedle_providers::executor::Method as WireMethod;

    let groups: Vec<Group<'_>> = vec![
        ("rg-lemaitre", "Ok Computer", "Lemaitre", "Single", 100, 1),
        ("rg-radiohead", "OK Computer", "Radiohead", "Album", 95, 39),
    ];
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{MUSICBRAINZ}/artist"),
        200,
        &[],
        search_answer("artists", Vec::new()),
    );
    wire.answer(
        &format!("{MUSICBRAINZ}/release-group"),
        200,
        &[],
        search_answer(
            "release-groups",
            groups.iter().copied().map(group_hit).collect(),
        ),
    );
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        search_answer("recordings", Vec::new()),
    );
    wire.on(
        WireMethod::Post,
        &format!("{LISTENBRAINZ}/popularity/release-group"),
        Scripted::Transport(TransportKind::Connect),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-listens-down");
    let (app, _held) = app_listening(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
        true,
    )
    .await;
    let uri = format!("/api/v3/search?{}", urlencode_query("ok computer"));
    let started = std::time::Instant::now();
    while listens_asked(&wire).is_empty() && started.elapsed() < Duration::from_secs(10) {
        let (status, _, bytes) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK);
        let body = json(&bytes);
        assert_eq!(body["album_status"], "ok", "{body}");
        let ids: Vec<&str> = body["albums"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["musicbrainz_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["rg-radiohead", "rg-lemaitre"], "{body}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let asked = listens_asked(&wire);
    assert!(
        !asked.is_empty()
            && asked
                .iter()
                .all(|(path, _)| path.ends_with("/release-group")),
        "ListenBrainz was asked, retries included, and for the groups only: {asked:?}"
    );
}

/// A wire that holds every ListenBrainz request until [`HeldListens::open`],
/// and sends the rest to its script.
struct HeldListens {
    script: Arc<ScriptedWire>,
    gate: tokio::sync::Semaphore,
}

impl HeldListens {
    /// Let ListenBrainz answer.
    fn open(&self) {
        self.gate.add_permits(1_000);
    }
}

impl Wire for HeldListens {
    fn send<'a>(
        &'a self,
        prepared: Prepared<'a>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<WireAnswer, WireError>> + Send + 'a>> {
        Box::pin(async move {
            if prepared.url.path().starts_with("/listenbrainz/") {
                let _held = self.gate.acquire().await;
            }
            self.script.send(prepared).await
        })
    }
}

/// A listen count the cache does not hold never holds a search: the answer
/// comes in text order while ListenBrainz has not answered, and once it
/// has, the count is kept and the next search ranks by it. Someone whose
/// primary source is Last.fm keeps the text order and asks ListenBrainz
/// nothing.
#[tokio::test]
async fn a_missing_listen_count_never_delays_a_search_and_is_kept_for_the_next() {
    use droppedneedle_providers::executor::Method as WireMethod;

    let script = Arc::new(ScriptedWire::new());
    let answer = |path: &str, key: &str, hits| {
        script.answer(
            &format!("{MUSICBRAINZ}/{path}"),
            200,
            &[],
            search_answer(key, hits),
        );
    };
    answer("artist", "artists", Vec::new());
    answer("recording", "recordings", Vec::new());
    answer(
        "release-group",
        "release-groups",
        vec![
            group_hit(("rg-quiet", "Night Drive Sessions", "Quiet", "Album", 90, 1)),
            group_hit(("rg-loud", "Night Drive Tapes", "Loud", "Album", 60, 1)),
        ],
    );
    script.on(
        WireMethod::Post,
        &format!("{LISTENBRAINZ}/popularity/release-group"),
        popularity_answer(
            "release_group_mbid",
            &[("rg-quiet", Some(10)), ("rg-loud", Some(50_000))],
        ),
    );
    let wire = Arc::new(HeldListens {
        script: script.clone(),
        gate: tokio::sync::Semaphore::new(0),
    });
    let (app_db, _dir) = crate::common::migrated_file("reads-search-listens-later");
    app_db.exec(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-ada', 'ada', 'user', 0)",
        [],
    );
    let (app, _held) = app_listening(
        app_db.clone(),
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
        true,
    )
    .await;
    let uri = format!("/api/v3/search?{}", urlencode_query("night drive"));
    let albums = |app: axum::Router| {
        let uri = uri.clone();
        async move {
            let (status, _, bytes) = call(app, Method::GET, &uri, Some(TEST_TOKEN), None).await;
            assert_eq!(status, StatusCode::OK);
            let body = json(&bytes);
            body["albums"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["musicbrainz_id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        }
    };

    let started = std::time::Instant::now();
    assert_eq!(albums(app.clone()).await, ["rg-quiet", "rg-loud"]);
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "the search waited on ListenBrainz: {:?}",
        started.elapsed()
    );

    wire.open();
    let started = std::time::Instant::now();
    let mut order = albums(app.clone()).await;
    while order != ["rg-loud", "rg-quiet"] && started.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(20)).await;
        order = albums(app.clone()).await;
    }
    assert_eq!(order, ["rg-loud", "rg-quiet"], "the kept count ranks");
    assert_eq!(listens_asked(&script).len(), 1, "asked once, then kept");

    app_db.exec(
        "INSERT INTO user_listening_prefs (user_id, primary_music_source, updated_at) \
         VALUES ('user-ada', 'lastfm', 0)",
        [],
    );
    assert_eq!(albums(app).await, ["rg-quiet", "rg-loud"], "text order");
    assert_eq!(
        listens_asked(&script).len(),
        1,
        "ListenBrainz asked nothing"
    );
}

/// A new search by a ListenBrainz follower asks ListenBrainz once for its
/// albums' counts and once for its songs', in the background. The counts
/// a hover's enrichment then asks for are the ones kept, so it sends
/// nothing, and neither does the same search again.
#[tokio::test]
async fn a_new_search_asks_listenbrainz_once_per_kind() {
    use droppedneedle_providers::executor::Method as WireMethod;

    let script = Arc::new(ScriptedWire::new());
    let answer = |path: &str, key: &str, hits| {
        script.answer(
            &format!("{MUSICBRAINZ}/{path}"),
            200,
            &[],
            search_answer(key, hits),
        );
    };
    answer("artist", "artists", Vec::new());
    answer(
        "release-group",
        "release-groups",
        vec![
            group_hit(("rg-quiet", "Night Drive Sessions", "Quiet", "Album", 90, 1)),
            group_hit(("rg-loud", "Night Drive Tapes", "Loud", "Album", 60, 1)),
        ],
    );
    answer(
        "recording",
        "recordings",
        vec![recording_hit(
            "rec-drive",
            "Night Drive",
            "Loud",
            100,
            &[("rel-single", "Night Drive", "Official", "rg-single")],
        )],
    );
    script.on(
        WireMethod::Post,
        &format!("{LISTENBRAINZ}/popularity/release-group"),
        popularity_answer(
            "release_group_mbid",
            &[("rg-quiet", Some(10)), ("rg-loud", Some(50_000))],
        ),
    );
    script.on(
        WireMethod::Post,
        &format!("{LISTENBRAINZ}/popularity/recording"),
        popularity_answer("recording_mbid", &[("rec-drive", Some(7))]),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-listens-once");
    let (app, _held) = app_counting(app_db, script.clone()).await;
    let uri = format!("/api/v3/search?{}", urlencode_query("night drive"));
    let posts = |path: &str| {
        listens_asked(&script)
            .iter()
            .filter(|(sent, _)| sent.ends_with(path))
            .count()
    };

    let (status, _, _) = call(app.clone(), Method::GET, &uri, Some(TEST_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    let started = std::time::Instant::now();
    while posts("/recording") == 0 && started.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let asked = listens_asked(&script);
    assert_eq!(posts("/release-group"), 1, "{asked:?}");
    assert_eq!(posts("/recording"), 1, "{asked:?}");

    enrich_albums(app.clone(), &["rg-quiet", "rg-loud"]).await;
    let (status, _, _) = call(app, Method::GET, &uri, Some(TEST_TOKEN), None).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        listens_asked(&script).len(),
        asked.len(),
        "the hover and the repeat ask nothing"
    );
}

/// One query's first pages share one MusicBrainz answer per endpoint:
/// typing it, then the All tab, then the first page of Albums, Songs and
/// Artists ask MusicBrainz three times in all.
#[tokio::test]
async fn a_query_asks_musicbrainz_once_per_endpoint() {
    let wire = Arc::new(ScriptedWire::new());
    let answer = |path: &str, key: &str, hits| {
        wire.answer(
            &format!("{MUSICBRAINZ}/{path}"),
            200,
            &[],
            search_answer(key, hits),
        );
    };
    answer("artist", "artists", vec![artist_hit("Night Drive", 100)]);
    answer(
        "release-group",
        "release-groups",
        vec![group_hit((
            "rg-night",
            "Night Drive",
            "Night Drive",
            "Album",
            100,
            1,
        ))],
    );
    answer(
        "recording",
        "recordings",
        vec![recording_hit(
            "rec-night",
            "Night Drive",
            "Night Drive",
            100,
            &[("rel-night", "Night Drive", "Official", "rg-night")],
        )],
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-one-answer");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;
    for uri in [
        "/api/v3/search/suggest?q=night%20drive",
        "/api/v3/search?q=night%20drive",
        "/api/v3/search/albums?q=night%20drive&limit=24",
        "/api/v3/search/tracks?q=night%20drive&limit=24",
        "/api/v3/search/artists?q=night%20drive&limit=24",
    ] {
        let (status, _, _) = call(app.clone(), Method::GET, uri, Some(TEST_TOKEN), None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
    let mut paths: Vec<String> = wire
        .sent()
        .iter()
        .map(|sent| sent.url.path().to_owned())
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            "/musicbrainz/ws/2/artist",
            "/musicbrainz/ws/2/recording",
            "/musicbrainz/ws/2/release-group"
        ]
    );
}

/// Where Deezer, iTunes and Spotify answer in [`scripted_catalog`].
const DEEZER: &str = "http://127.0.0.1:9/deezer";
const ITUNES_LOOKUP: &str = "http://127.0.0.1:9/itunes/lookup";
const SPOTIFY_TOKEN: &str = "http://127.0.0.1:9/spotify-accounts/api/token";
const SPOTIFY_API: &str = "http://127.0.0.1:9/spotify";
const DEEZER_LINKS: &str = "http://127.0.0.1:9/deezer-links";
const YOUTUBE_OEMBED: &str = "http://127.0.0.1:9/youtube-oembed";

/// A full search for `query` on `app`, with the answer's body.
async fn search_for(app: axum::Router, query: &str) -> serde_json::Value {
    let query: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    let (status, _, bytes) = call(
        app,
        Method::GET,
        &format!("/api/v3/search?q={query}"),
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    json(&bytes)
}

/// The MusicBrainz queries `wire` was sent, in order.
fn musicbrainz_queries(wire: &ScriptedWire) -> Vec<String> {
    wire.sent()
        .iter()
        .filter(|sent| sent.url.as_str().starts_with(MUSICBRAINZ))
        .filter_map(|sent| sent.query("query"))
        .collect()
}

/// A Deezer song link is read for its ISRC, and the search shows the
/// songs MusicBrainz files under it: the library's copy where it holds
/// one, the others to request. Nothing else is searched, and the
/// dropdown stays quiet.
#[tokio::test]
async fn a_deezer_link_shows_the_songs_its_isrc_names() {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{DEEZER}/track/13791930"),
        200,
        &[],
        include_str!("../fixtures/deezer/track.json"),
    );
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        search_answer(
            "recordings",
            vec![
                recording_hit(
                    "rec-single",
                    "Opener",
                    "Aurora",
                    100,
                    &[("rel-single", "Opener", "Official", "rg-single")],
                ),
                recording_hit(
                    "rec-owned",
                    "Opener",
                    "Aurora",
                    100,
                    &[("rel-owned", "Renaissance", "Official", "rg-owned")],
                ),
            ],
        ),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-deezer-link");
    seed_catalog(&app_db);
    seed_track(
        &app_db,
        "t-owned",
        "album-renaissance",
        "Opener",
        "Renaissance",
        "indexed",
    );
    app_db.exec(
        "INSERT INTO local_track_external_identities (local_track_id, provider, recording_mbid, \
         decision_source, selected_at) VALUES ('t-owned', 'musicbrainz', 'rec-owned', 'manual', 0)",
        [],
    );
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;

    let body = search_for(
        app.clone(),
        "https://www.deezer.com/en/track/13791930?utm_source=deezer",
    )
    .await;
    assert_eq!(body["link"], "read", "{body}");
    let rows: Vec<(&str, Option<&str>)> = body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| (hit["musicbrainz_id"].as_str().unwrap(), hit["id"].as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![("rec-owned", Some("t-owned")), ("rec-single", None)],
        "{body}"
    );
    assert_eq!(body["top_track"]["id"], "t-owned");
    assert!(body["artists"].as_array().unwrap().is_empty());
    assert!(body["albums"].as_array().unwrap().is_empty());
    assert_eq!(musicbrainz_queries(&wire), vec!["isrc:USGF19942501"]);

    let (status, _, bytes) = call(
        app,
        Method::GET,
        "/api/v3/search/suggest?q=https%3A%2F%2Fwww.deezer.com%2Ftrack%2F13791930",
        Some(TEST_TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json(&bytes)["results"].as_array().unwrap().is_empty());
    assert_eq!(wire.sent().len(), 2, "the dropdown sends nothing");
}

/// An Apple Music link carries no ISRC, so its song is found by title and
/// artist, and only a close match shows: a high score and a length within
/// three seconds of the store's.
#[tokio::test]
async fn an_apple_music_link_shows_only_close_matches() {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        ITUNES_LOOKUP,
        200,
        &[],
        include_str!("../fixtures/itunes/lookup_song.json"),
    );
    let hit = |id: &str, score: u32, length: u32| {
        let mut hit = recording_hit(
            id,
            "Smells Like Teen Spirit",
            "Nirvana",
            score,
            &[("rel-1", "Nevermind", "Official", "rg-1")],
        );
        hit["length"] = serde_json::json!(length);
        hit
    };
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        search_answer(
            "recordings",
            vec![
                hit("rec-close", 100, 302_000),
                hit("rec-live", 100, 312_000),
                hit("rec-weak", 80, 301_000),
            ],
        ),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-apple-link");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;

    let body = search_for(
        app,
        "https://music.apple.com/us/album/nevermind/1440783617?i=1440783625",
    )
    .await;
    assert_eq!(body["link"], "read", "{body}");
    let ids: Vec<&str> = body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["musicbrainz_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["rec-close"], "{body}");
    assert_eq!(
        musicbrainz_queries(&wire),
        vec![r#"recording:"Smells Like Teen Spirit" AND artist:"Nirvana""#]
    );
    let lookup = wire
        .sent()
        .into_iter()
        .find(|sent| sent.url.as_str().starts_with(ITUNES_LOOKUP))
        .expect("the store was asked");
    assert_eq!(lookup.query("id").as_deref(), Some("1440783625"));
    assert_eq!(lookup.query("country").as_deref(), Some("us"));
}

/// A Spotify link is read with the server's Spotify app, no one's sign-in:
/// an app token, then the track's ISRC.
#[tokio::test]
async fn a_spotify_link_is_read_with_the_servers_app() {
    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        droppedneedle_providers::executor::Method::Post,
        SPOTIFY_TOKEN,
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: br#"{"access_token":"app-token","token_type":"Bearer","expires_in":3600}"#
                .to_vec(),
        },
    );
    wire.answer(
        &format!("{SPOTIFY_API}/tracks/1a2B3c4D5e6F7g8H9i0JkL"),
        200,
        &[],
        serde_json::to_vec(&serde_json::json!({
            "id": "1a2B3c4D5e6F7g8H9i0JkL",
            "name": "Opener",
            "artists": [{"name": "Aurora"}],
            "duration_ms": 248_000,
            "external_ids": {"isrc": "QZAAA2600001"},
        }))
        .unwrap(),
    );
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        search_answer(
            "recordings",
            vec![recording_hit(
                "rec-opener",
                "Opener",
                "Aurora",
                100,
                &[("rel-1", "First Light", "Official", "rg-1")],
            )],
        ),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-spotify-link");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;

    let body = search_for(
        app,
        "https://open.spotify.com/track/1a2B3c4D5e6F7g8H9i0JkL?si=x",
    )
    .await;
    assert_eq!(body["link"], "read", "{body}");
    assert_eq!(body["tracks"][0]["musicbrainz_id"], "rec-opener");
    assert_eq!(musicbrainz_queries(&wire), vec!["isrc:QZAAA2600001"]);
    let sent = wire.sent();
    let token = sent
        .iter()
        .find(|sent| sent.url.as_str() == SPOTIFY_TOKEN)
        .expect("an app token was asked for");
    assert!(
        token
            .headers
            .iter()
            .any(|(name, value)| name == "authorization" && value.starts_with("Basic "))
    );
    let track = sent
        .iter()
        .find(|sent| sent.url.path().ends_with("/tracks/1a2B3c4D5e6F7g8H9i0JkL"))
        .expect("the track was read");
    assert!(
        track
            .headers
            .iter()
            .any(|(name, value)| name == "authorization" && value == "Bearer app-token")
    );
}

/// A link whose service does not hold the song is unreadable, and one
/// MusicBrainz has nothing for is no match. Neither falls back to a text
/// search.
#[tokio::test]
async fn a_link_that_finds_nothing_says_why() {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{DEEZER}/track/99999999999"),
        200,
        &[],
        include_str!("../fixtures/deezer/track_not_found.json"),
    );
    wire.answer(
        &format!("{DEEZER}/track/13791930"),
        200,
        &[],
        include_str!("../fixtures/deezer/track.json"),
    );
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        search_answer("recordings", Vec::new()),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-link-nothing");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;

    let body = search_for(app.clone(), "https://www.deezer.com/track/99999999999").await;
    assert_eq!(body["link"], "unreadable", "{body}");
    assert!(body["tracks"].as_array().unwrap().is_empty());
    assert!(musicbrainz_queries(&wire).is_empty());

    let body = search_for(app, "https://www.deezer.com/track/13791930").await;
    assert_eq!(body["link"], "no_match", "{body}");
    assert!(body["tracks"].as_array().unwrap().is_empty());
    assert_eq!(musicbrainz_queries(&wire), vec!["isrc:USGF19942501"]);
}

/// What a link's service said is kept a day, so the same link again asks
/// it nothing, and the Spotify app's token serves every link until it
/// expires.
#[tokio::test]
async fn a_link_is_read_once_and_the_spotify_token_reused() {
    let wire = Arc::new(ScriptedWire::new());
    wire.on(
        droppedneedle_providers::executor::Method::Post,
        SPOTIFY_TOKEN,
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: br#"{"access_token":"app-token","token_type":"Bearer","expires_in":3600}"#
                .to_vec(),
        },
    );
    for (id, isrc) in [
        ("1a2B3c4D5e6F7g8H9i0JkL", "QZAAA2600001"),
        ("2a2B3c4D5e6F7g8H9i0JkL", "QZAAA2600002"),
    ] {
        wire.answer(
            &format!("{SPOTIFY_API}/tracks/{id}"),
            200,
            &[],
            serde_json::to_vec(&serde_json::json!({
                "id": id,
                "name": "Opener",
                "artists": [{"name": "Aurora"}],
                "duration_ms": 248_000,
                "external_ids": {"isrc": isrc},
            }))
            .unwrap(),
        );
    }
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        search_answer(
            "recordings",
            vec![recording_hit(
                "rec-opener",
                "Opener",
                "Aurora",
                100,
                &[("rel-1", "First Light", "Official", "rg-1")],
            )],
        ),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-link-kept");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;

    for link in [
        "https://open.spotify.com/track/1a2B3c4D5e6F7g8H9i0JkL",
        "https://open.spotify.com/track/1a2B3c4D5e6F7g8H9i0JkL?si=again",
        "https://open.spotify.com/track/2a2B3c4D5e6F7g8H9i0JkL",
    ] {
        let body = search_for(app.clone(), link).await;
        assert_eq!(body["link"], "read", "{body}");
    }
    let sent = wire.sent();
    let count = |wanted: &str| sent.iter().filter(|s| s.url.path() == wanted).count();
    assert_eq!(count("/spotify-accounts/api/token"), 1, "one token");
    assert_eq!(count("/spotify/tracks/1a2B3c4D5e6F7g8H9i0JkL"), 1);
    assert_eq!(count("/spotify/tracks/2a2B3c4D5e6F7g8H9i0JkL"), 1);
}

/// A search that asks for no songs reads no link: nothing is sent.
#[tokio::test]
async fn a_link_search_without_songs_sends_nothing() {
    let wire = Arc::new(ScriptedWire::new());
    let (app_db, _dir) = crate::common::migrated_file("reads-search-link-no-songs");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;

    for filter in ["buckets=artists,albums", "limit_tracks=0"] {
        let (status, _, bytes) = call(
            app.clone(),
            Method::GET,
            &format!("/api/v3/search?q=https%3A%2F%2Fwww.deezer.com%2Ftrack%2F13791930&{filter}"),
            Some(TEST_TOKEN),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(json(&bytes)["tracks"].as_array().unwrap().is_empty());
    }
    assert!(wire.sent().is_empty(), "{:?}", wire.sent().len());
}

/// One MusicBrainz answer naming "Smells Like Teen Spirit" by Nirvana, at
/// the length the stores give it.
fn teen_spirit_answer() -> Vec<u8> {
    let mut hit = recording_hit(
        "rec-teen-spirit",
        "Smells Like Teen Spirit",
        "Nirvana",
        100,
        &[("rel-1", "Nevermind", "Official", "rg-1")],
    );
    hit["length"] = serde_json::json!(301_000);
    search_answer("recordings", vec![hit])
}

/// A YouTube link is read through oEmbed: "Artist - Title (Official
/// Video)" becomes a search by name, with the video's tags dropped.
#[tokio::test]
async fn a_youtube_link_is_searched_by_its_video_title() {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        YOUTUBE_OEMBED,
        200,
        &[],
        include_str!("../fixtures/youtube/oembed_video.json"),
    );
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        teen_spirit_answer(),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-youtube-link");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;

    let body = search_for(app, "https://youtu.be/hTWKbfoikeg?si=x").await;
    assert_eq!(body["link"], "read", "{body}");
    assert_eq!(body["tracks"][0]["musicbrainz_id"], "rec-teen-spirit");
    assert_eq!(
        musicbrainz_queries(&wire),
        vec![r#"recording:"Smells Like Teen Spirit" AND artist:"Nirvana""#]
    );
    let oembed = wire
        .sent()
        .into_iter()
        .find(|sent| sent.url.as_str().starts_with(YOUTUBE_OEMBED))
        .expect("oEmbed was asked");
    assert_eq!(
        oembed.query("url").as_deref(),
        Some("https://www.youtube.com/watch?v=hTWKbfoikeg")
    );
}

/// A Deezer short link is followed one hop, read from its redirect, never
/// fetched further; it counts only when it lands on a Deezer track.
#[tokio::test]
async fn a_deezer_short_link_is_read_from_its_redirect() {
    let wire = Arc::new(ScriptedWire::new());
    let dest = "https://link.deezer.com/?dest=https%3A%2F%2Fwww.deezer.com%2Ftrack%2F13791930%3Fhost%3D1&gwf=x";
    wire.answer(
        &format!("{DEEZER_LINKS}/s/TrackCode1"),
        301,
        &[("location", dest)],
        Vec::new(),
    );
    let artist = "https://link.deezer.com/?dest=https%3A%2F%2Fwww.deezer.com%2Fartist%2F415";
    wire.answer(
        &format!("{DEEZER_LINKS}/s/ArtistCode1"),
        301,
        &[("location", artist)],
        Vec::new(),
    );
    wire.answer(
        &format!("{DEEZER}/track/13791930"),
        200,
        &[],
        include_str!("../fixtures/deezer/track.json"),
    );
    wire.answer(
        &format!("{MUSICBRAINZ}/recording"),
        200,
        &[],
        teen_spirit_answer(),
    );
    let (app_db, _dir) = crate::common::migrated_file("reads-search-deezer-short-link");
    let (app, _held) = app_on(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone()),
    )
    .await;

    let body = search_for(app.clone(), "https://link.deezer.com/s/TrackCode1").await;
    assert_eq!(body["link"], "read", "{body}");
    assert_eq!(musicbrainz_queries(&wire), vec!["isrc:USGF19942501"]);

    let body = search_for(app, "https://link.deezer.com/s/ArtistCode1").await;
    assert_eq!(body["link"], "unreadable", "{body}");
    assert!(
        wire.sent()
            .iter()
            .all(|sent| !sent.url.as_str().contains("link.deezer.com")
                && !sent.url.path().contains("/artist/")),
        "nothing past the redirect is fetched"
    );
}

/// With Spotify and YouTube switched off, their links are not read: a
/// Spotify link says Spotify needs setting up, a YouTube link can't be
/// read, and nothing is sent.
#[tokio::test]
async fn links_to_services_switched_off_send_nothing() {
    let wire = Arc::new(ScriptedWire::new());
    let (app_db, _dir) = crate::common::migrated_file("reads-search-links-off");
    let settings = OfficialMusicBrainz {
        listenbrainz: false,
        links: false,
    };
    let (app, _held) = app_with(
        app_db,
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
        Some(wire.clone() as Arc<dyn Wire>),
        settings,
    )
    .await;

    let body = search_for(
        app.clone(),
        "https://open.spotify.com/track/1a2B3c4D5e6F7g8H9i0JkL",
    )
    .await;
    assert_eq!(body["link"], "needs_spotify", "{body}");
    let body = search_for(app, "https://youtu.be/hTWKbfoikeg").await;
    assert_eq!(body["link"], "unreadable", "{body}");
    assert!(wire.sent().is_empty(), "{:?}", wire.sent().len());
}
