//! Artist and album pages and MusicBrainz-backed search, behind the real
//! session gate, against recorded provider payloads served from a
//! loopback fixture server (tests/fixtures/catalog). One test per page,
//! the MusicBrainz outage fallback, and the auth matrix rows.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, Uri},
    middleware,
};
use droppedneedle::auth::session::middleware::{SessionAuth, require_session};
use droppedneedle::auth::session::store::{MemorySessionStore, SessionRecord, SessionStore};
use droppedneedle::auth::session::tokens::hash_token;
use droppedneedle::auth::users::memory::TestRig;
use droppedneedle::auth::users::models::LastFmConnection;
use droppedneedle::middleware::request_scope;
use droppedneedle::providers::Providers;
use droppedneedle::reads::catalog::{
    self, Catalog, CatalogDeps,
    library::LocalCatalog,
    upstream::{CatalogSettings, Endpoints, Upstream},
};
use droppedneedle::reads::search::{self, SearchDeps, service::SearchService};
use droppedneedle::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle::runtime_config::sections::{
    MbSourceMode, MusicBrainzSettings, MusicSource, UserPreferences,
};
use droppedneedle::schema::apply_migrations;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tower::ServiceExt as _;

use crate::common;

const TOKEN: &str = "dn-catalog-test-token-000000000001";
const USER: &str = "user-1";

const ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
const KID_A: &str = "1c6d2e3b-0a5f-4d8e-9f3a-5d6e8b2c4a10";
const RELEASE_CD: &str = "6b1c7d8a-5f0e-4cd3-8e8f-0c1d3a7b9f65";
const UNKNOWN_ALBUM: &str = "00000000-1111-4222-8333-444444444444";

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/catalog/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("fixture {path}: {error}"))
}

fn upstream_fixture(key: &str) -> String {
    let all: Value = serde_json::from_str(&fixture("upstreams.json")).unwrap();
    all[key].to_string()
}

/// One canned upstream answer: a path, optionally one query parameter that
/// must be present (with a value containing the given text), and the
/// recorded body.
struct Route {
    path: String,
    param: Option<(&'static str, &'static str)>,
    status: u16,
    body: String,
}

/// Loopback server answering the recorded payloads and counting hits per
/// path, so tests can see what reached "the network".
#[derive(Clone)]
struct Fixtures {
    base: String,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Fixtures {
    async fn start() -> Self {
        let routes = Arc::new(routes());
        let hits: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let counter = hits.clone();
        let app = axum::Router::new().fallback(move |uri: Uri| {
            let routes = routes.clone();
            let counter = counter.clone();
            async move {
                let query = axum::extract::Query::<HashMap<String, String>>::try_from_uri(&uri)
                    .map(|query| query.0)
                    .unwrap_or_default();
                *counter
                    .lock()
                    .unwrap()
                    .entry(uri.path().to_owned())
                    .or_default() += 1;
                let found = routes.iter().find(|route| {
                    route.path == uri.path()
                        && route.param.is_none_or(|(key, wanted)| {
                            query.get(key).is_some_and(|value| value.contains(wanted))
                        })
                });
                match found {
                    Some(route) => (
                        StatusCode::from_u16(route.status).unwrap(),
                        [("content-type", "application/json")],
                        route.body.clone(),
                    ),
                    None => (
                        StatusCode::NOT_FOUND,
                        [("content-type", "application/json")],
                        "{\"error\":\"Not Found\"}".to_owned(),
                    ),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { base, hits }
    }

    fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    fn endpoints(&self) -> Endpoints {
        Endpoints {
            listenbrainz: format!("{}/lb", self.base),
            lastfm: format!("{}/lastfm/2.0/", self.base),
            audiodb: format!("{}/audiodb", self.base),
            wikidata: format!("{}/wikidata", self.base),
            wikipedia: format!("{}/wikipedia-{{lang}}", self.base),
            commons: format!("{}/commons", self.base),
            itunes: format!("{}/itunes/search", self.base),
        }
    }
}

fn routes() -> Vec<Route> {
    let route = |path: String, param, body: String| Route {
        path,
        param,
        status: 200,
        body,
    };
    vec![
        route(
            format!("/mb/ws/2/artist/{ARTIST}"),
            None,
            fixture("mb_artist.json"),
        ),
        route(
            "/mb/ws/2/release-group".to_owned(),
            Some(("artist", ARTIST)),
            fixture("mb_artist_release_groups.json"),
        ),
        route(
            "/mb/ws/2/release-group".to_owned(),
            Some(("query", "")),
            fixture("mb_search_release_groups.json"),
        ),
        route(
            "/mb/ws/2/artist".to_owned(),
            Some(("query", "")),
            fixture("mb_search_artists.json"),
        ),
        route(
            format!("/mb/ws/2/release-group/{OK_COMPUTER}"),
            None,
            fixture("mb_release_group.json"),
        ),
        route(
            format!("/mb/ws/2/release/{RELEASE_CD}"),
            None,
            fixture("mb_release_cd.json"),
        ),
        route(
            "/audiodb/123/artist-mb.php".to_owned(),
            Some(("i", ARTIST)),
            upstream_fixture("audiodb_artist"),
        ),
        route(
            "/wikidata/wiki/Special:EntityData/Q44190.json".to_owned(),
            None,
            upstream_fixture("wikidata_entity"),
        ),
        route(
            "/wikidata/w/api.php".to_owned(),
            Some(("action", "wbgetclaims")),
            upstream_fixture("wikidata_claims"),
        ),
        route(
            "/wikipedia-en/w/api.php".to_owned(),
            Some(("prop", "extracts")),
            upstream_fixture("wikipedia_extract"),
        ),
        route(
            "/commons/w/api.php".to_owned(),
            Some(("prop", "imageinfo")),
            upstream_fixture("commons_imageinfo"),
        ),
        route(
            format!("/lb/1/lb-radio/artist/{ARTIST}"),
            None,
            upstream_fixture("lb_radio_artist"),
        ),
        route(
            format!("/lb/1/popularity/top-release-groups-for-artist/{ARTIST}"),
            None,
            upstream_fixture("lb_top_release_groups"),
        ),
        // ListenBrainz popularity often answers empty; Last.fm fills in.
        route(
            format!("/lb/1/popularity/top-recordings-for-artist/{ARTIST}"),
            None,
            "[]".to_owned(),
        ),
        route(
            "/lastfm/2.0/".to_owned(),
            Some(("method", "artist.getTopTracks")),
            upstream_fixture("lastfm_artist_top_tracks"),
        ),
        route(
            "/lastfm/2.0/".to_owned(),
            Some(("method", "artist.getInfo")),
            upstream_fixture("lastfm_artist_info"),
        ),
        route(
            "/itunes/search".to_owned(),
            Some(("term", "OK Computer")),
            upstream_fixture("itunes_search"),
        ),
    ]
}

/// Live settings for the tests: MusicBrainz as a mirror at `mb_url`,
/// ListenBrainz on, everything else at its defaults.
struct TestSettings {
    mb_url: String,
}

impl CatalogSettings for TestSettings {
    fn musicbrainz(&self) -> MusicBrainzSettings {
        MusicBrainzSettings {
            source_mode: MbSourceMode::Mirror,
            api_url: self.mb_url.clone(),
            source_id: "test-source".to_owned(),
            generation: 1,
            ..MusicBrainzSettings::default()
        }
    }

    fn preferences(&self) -> UserPreferences {
        UserPreferences::default()
    }

    fn advanced(&self) -> AdvancedSettings {
        AdvancedSettings::default()
    }

    fn listenbrainz_enabled(&self) -> bool {
        true
    }

    fn primary_source(&self) -> MusicSource {
        MusicSource::Listenbrainz
    }

    fn store_region(&self) -> String {
        "US".to_owned()
    }
}

async fn migrated_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    apply_migrations(&pool).await.unwrap();
    pool
}

/// The library: Radiohead with OK Computer (identified as the CD edition,
/// one indexed track), an unidentified bootleg, and an open request for
/// Kid A.
async fn seed_library(pool: &SqlitePool) {
    let statements = [
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('artist-radiohead', 'Radiohead', 'radiohead', 'group', 0, 0)"
            .to_owned(),
        format!(
            "INSERT INTO local_artist_external_identities \
             (local_artist_id, provider, provider_artist_id, decision_source, selected_at) \
             VALUES ('artist-radiohead', 'musicbrainz', '{ARTIST}', 'manual', 0)"
        ),
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, \
         grouping_source, created_at, updated_at) VALUES \
         ('album-okc', 'root-1', 'g-okc', 'OK Computer', 'ok computer', 'Radiohead', \
         'radiohead', 'artist-radiohead', 1997, 'manual', 0, 0), \
         ('album-bootleg', 'root-1', 'g-boot', 'OK Computer Live Bootleg', \
         'ok computer live bootleg', 'Radiohead', 'radiohead', 'artist-radiohead', 1998, \
         'manual', 1, 1)"
            .to_owned(),
        format!(
            "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, release_mbid, decision_source, \
             selected_at) VALUES ('album-okc', 'musicbrainz', '{OK_COMPUTER}', '{RELEASE_CD}', \
             'manual', 0)"
        ),
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         artist_name, artist_name_folded, album_title, album_title_folded, disc_number, \
         track_number, duration_seconds, file_format, ingest_source, imported_at, \
         membership_source) VALUES ('track-airbag', 'album-okc', 'root-1', \
         '/music/airbag.flac', 'airbag.flac', 'hash-1', 1000, 0, 'stat-1', 'Airbag', 'airbag', \
         'Radiohead', 'radiohead', 'OK Computer', 'ok computer', 1, 1, 284.0, 'flac', 'scan', \
         0, 'manual')"
            .to_owned(),
        format!(
            "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
             album_title, requested_at, status, request_kind) VALUES \
             ('{KID_A}', '{KID_A}', 'Radiohead', 'Kid A', '2026-09-01T00:00:00Z', 'pending', \
             'album')"
        ),
    ];
    for statement in statements {
        sqlx::query(&statement).execute(pool).await.unwrap();
    }
}

async fn session_auth() -> SessionAuth<MemorySessionStore> {
    let store = MemorySessionStore::new();
    let now = droppedneedle::auth::session::store::now_unix();
    store
        .insert(SessionRecord {
            id: "session-1".to_owned(),
            user_id: USER.to_owned(),
            token_hash: hash_token(TOKEN),
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

/// The catalog and search routes behind the session gate and request
/// scope, with MusicBrainz at `mb_url`. The test user has a Last.fm key.
async fn app_with(fixtures: &Fixtures, mb_url: String) -> axum::Router {
    let pool = migrated_pool().await;
    seed_library(&pool).await;
    let rig = TestRig::new().unwrap();
    let sealed = rig.deps.crypto.encrypt("test-lastfm-key").unwrap();
    rig.deps
        .lastfm
        .upsert(
            USER,
            LastFmConnection {
                configured: true,
                api_key_encrypted: Some(sealed),
                ..LastFmConnection::default()
            },
        )
        .await
        .unwrap();
    let http = droppedneedle::http_client::HttpClientFactory::new().unwrap();
    let upstream = Upstream::new(
        &http,
        Arc::new(Providers::unpaced()),
        Arc::new(TestSettings { mb_url }),
        rig.deps.clone(),
    )
    .with_endpoints(fixtures.endpoints());
    let catalog = Catalog::new(upstream, LocalCatalog::new(pool.clone()));
    let ids: Arc<dyn droppedneedle::ids::IdGenerator> =
        Arc::new(common::FixedIdGenerator::new(common::FIXED_ID));
    let search = SearchDeps::new(
        SearchService::new(pool).with_remote(catalog.clone()),
        Arc::new(search::ports::UnconfiguredEnrichment),
        ids.clone(),
    );
    axum::Router::new()
        .nest("/api/v3", catalog::router(CatalogDeps { catalog, ids }))
        .merge(search::router(search))
        .layer(middleware::from_fn_with_state(
            session_auth().await,
            require_session,
        ))
        .layer(middleware::from_fn_with_state(
            common::hooked_state(),
            request_scope,
        ))
        .fallback(droppedneedle::handlers::fallback_404)
}

async fn app(fixtures: &Fixtures) -> axum::Router {
    app_with(fixtures, format!("{}/mb/ws/2", fixtures.base)).await
}

async fn call(
    app: &axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let (status, body) = call(app, Method::GET, uri, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

fn titles(items: &Value) -> Vec<&str> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn artist_header_reads_musicbrainz_once_and_flags_the_library() {
    let fixtures = Fixtures::start().await;
    let app = app(&fixtures).await;

    let uri = format!("/api/v3/artists/{}", ARTIST.to_uppercase());
    let body = get(&app, &uri).await;
    assert_eq!(body["name"], "Radiohead");
    assert_eq!(body["musicbrainz_id"], ARTIST);
    assert_eq!(body["type"], "Group");
    assert_eq!(body["country"], "GB");
    assert_eq!(body["life_span"]["begin"], "1991");
    assert_eq!(
        body["tags"],
        json!(["rock", "alternative rock", "art rock"])
    );
    assert_eq!(body["aliases"], json!(["On a Friday"]));
    let labels: Vec<&str> = body["external_links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| link["label"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Official Website", "Instagram", "Bandcamp"]);
    assert_eq!(body["in_library"], true);
    assert_eq!(body["source"], "musicbrainz");
    assert!(body["service_status"].is_null());

    get(&app, &uri).await;
    assert_eq!(
        fixtures.hits(&format!("/mb/ws/2/artist/{ARTIST}")),
        1,
        "the second load is served from the cache"
    );

    let stores = get(&app, &format!("/api/v3/artists/{ARTIST}/purchase-options")).await;
    assert_eq!(
        stores,
        json!({
            "links": [
                {"store": "bandcamp", "label": "Bandcamp", "url": "https://radiohead.bandcamp.com/", "kind": "digital"},
                {"store": "other", "label": "store.radiohead.com", "url": "https://store.radiohead.com/", "kind": "physical"}
            ],
            "bandcamp_search_url": "https://bandcamp.com/search?q=Radiohead&item_type=b"
        })
    );
}

#[tokio::test]
async fn artist_extended_fetches_biography_and_images_for_the_header() {
    let fixtures = Fixtures::start().await;
    let app = app(&fixtures).await;

    let before = get(&app, &format!("/api/v3/artists/{ARTIST}")).await;
    assert!(
        before["images"]["thumb_url"].is_null(),
        "the header never dials out to AudioDB"
    );

    let extended = get(&app, &format!("/api/v3/artists/{ARTIST}/extended")).await;
    assert_eq!(
        extended["description"],
        "Radiohead are an English rock band formed in Abingdon, Oxfordshire, in 1985."
    );
    assert_eq!(
        extended["image"],
        "https://upload.wikimedia.org/wikipedia/commons/a/a1/Radiohead_2016.jpg"
    );
    assert_eq!(
        extended["images"]["thumb_url"],
        "https://r2.theaudiodb.com/images/media/artist/thumb/radiohead.jpg"
    );

    let after = get(&app, &format!("/api/v3/artists/{ARTIST}")).await;
    assert_eq!(after["images"], extended["images"]);
}

#[tokio::test]
async fn discography_filters_flags_and_pages() {
    let fixtures = Fixtures::start().await;
    let app = app(&fixtures).await;

    let first = get(&app, &format!("/api/v3/artists/{ARTIST}/releases?limit=3")).await;
    assert_eq!(titles(&first["albums"]), ["Kid A", "OK Computer"]);
    assert_eq!(titles(&first["eps"]), ["My Iron Lung"]);
    assert!(first["singles"].as_array().unwrap().is_empty());
    assert_eq!(first["albums"][0]["requested"], true);
    assert_eq!(first["albums"][1]["in_library"], true);
    assert_eq!(first["albums"][1]["requested"], false);
    assert_eq!(first["returned_count"], 3);
    assert_eq!(first["next_offset"], 3);
    assert_eq!(first["has_more"], true);
    assert_eq!(
        first["source_total_count"], 4,
        "live and compilation groups are filtered"
    );
    assert_eq!(first["warming"], false);

    let rest = get(
        &app,
        &format!("/api/v3/artists/{ARTIST}/releases?offset=3&limit=3"),
    )
    .await;
    assert_eq!(titles(&rest["singles"]), ["Creep"]);
    assert_eq!(rest["has_more"], false);

    let more = get(
        &app,
        &format!("/api/v3/albums/{OK_COMPUTER}/more-by-artist?artist_id={ARTIST}&count=2"),
    )
    .await;
    assert_eq!(more["artist_name"], "Radiohead");
    assert_eq!(
        titles(&more["albums"]),
        ["Kid A", "I Might Be Wrong: Live Recordings"]
    );
    assert_eq!(more["albums"][0]["requested"], true);
    assert_eq!(
        fixtures.hits("/mb/ws/2/release-group"),
        1,
        "one browse serves all three"
    );
}

#[tokio::test]
async fn artist_sections_use_listenbrainz_then_lastfm() {
    let fixtures = Fixtures::start().await;
    let app = app(&fixtures).await;

    let similar = get(&app, &format!("/api/v3/artists/{ARTIST}/similar?count=5")).await;
    assert_eq!(similar["source"], "listenbrainz");
    assert_eq!(similar["configured"], true);
    let names: Vec<&str> = similar["similar_artists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artist| artist["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["Lana Del Rey", "Portishead"],
        "seed artist dropped, most listened first"
    );
    assert_eq!(similar["similar_artists"][1]["listen_count"], 7500);

    let albums = get(&app, &format!("/api/v3/artists/{ARTIST}/top-albums")).await;
    assert_eq!(titles(&albums["albums"]), ["OK Computer", "Kid A"]);
    assert_eq!(albums["albums"][0]["in_library"], true);
    assert_eq!(albums["albums"][1]["requested"], true);

    let songs = get(&app, &format!("/api/v3/artists/{ARTIST}/top-songs")).await;
    assert_eq!(
        songs["source"], "lastfm",
        "empty ListenBrainz falls back to Last.fm"
    );
    assert_eq!(titles(&songs["songs"]), ["Creep", "No Surprises"]);
    assert_eq!(songs["songs"][0]["listen_count"], 9_182_736);

    let lastfm = get(
        &app,
        &format!("/api/v3/artists/{ARTIST}/lastfm?artist_name=Radiohead"),
    )
    .await;
    assert_eq!(
        lastfm["bio"],
        "Radiohead are an English rock band from Abingdon."
    );
    assert_eq!(lastfm["summary"], "Radiohead are an English rock band.");
    assert_eq!(lastfm["listeners"], 6_000_000);
    assert_eq!(lastfm["similar_artists"][0]["name"], "Thom Yorke");
}

#[tokio::test]
async fn album_page_shows_the_owned_edition_and_where_to_buy() {
    let fixtures = Fixtures::start().await;
    let app = app(&fixtures).await;

    let album = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}")).await;
    assert_eq!(album["title"], "OK Computer");
    assert_eq!(album["artist_name"], "Radiohead");
    assert_eq!(album["artist_id"], ARTIST);
    assert_eq!(album["year"], 1997);
    assert_eq!(album["in_library"], true);
    assert_eq!(album["selected_release_mbid"], RELEASE_CD);
    assert_eq!(album["pick_basis"], "owned");
    assert_eq!(titles(&album["tracks"]), ["Airbag", "Paranoid Android"]);
    assert_eq!(album["tracks"][0]["length"], 284_000);
    assert_eq!(album["total_length"], 667_000);
    assert_eq!(album["label"], "Parlophone");

    let editions = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}/editions")).await;
    assert_eq!(editions["items"].as_array().unwrap().len(), 3);
    assert_eq!(editions["owned_release_mbid"], RELEASE_CD);
    assert_eq!(editions["selected_basis"], "owned");
    let owned: Vec<&Value> = editions["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["is_owned"] == true)
        .collect();
    assert_eq!(owned.len(), 1);
    assert_eq!(owned[0]["track_count"], 12);

    let stores = get(
        &app,
        &format!("/api/v3/albums/{OK_COMPUTER}/purchase-options"),
    )
    .await;
    assert_eq!(stores["physical"][0]["store"], "amazon");
    assert_eq!(
        stores["digital"],
        json!([{
            "store": "itunes",
            "label": "iTunes / Apple Music",
            "url": "https://music.apple.com/us/album/ok-computer/1097861387?uo=4",
            "kind": "digital"
        }]),
        "no download store on MusicBrainz, so iTunes fills in"
    );
    assert_eq!(
        stores["bandcamp_search_url"],
        "https://bandcamp.com/search?q=Radiohead+OK+Computer&item_type=a"
    );
}

#[tokio::test]
async fn musicbrainz_outage_falls_back_to_the_library() {
    let fixtures = Fixtures::start().await;
    // Nothing listens on the discard port: every MusicBrainz call fails.
    let app = app_with(&fixtures, "http://127.0.0.1:9/ws/2".to_owned()).await;

    let artist = get(&app, &format!("/api/v3/artists/{ARTIST}")).await;
    assert_eq!(artist["source"], "library");
    assert_eq!(artist["name"], "Radiohead");
    assert_eq!(artist["service_status"], json!({"musicbrainz": "error"}));

    let releases = get(&app, &format!("/api/v3/artists/{ARTIST}/releases")).await;
    assert_eq!(releases["source"], "library");
    assert_eq!(titles(&releases["albums"]), ["OK Computer"]);

    let album = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}")).await;
    assert_eq!(album["source"], "library");
    assert_eq!(titles(&album["tracks"]), ["Airbag"]);

    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/api/v3/albums/{UNKNOWN_ALBUM}/basic"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["message"], "Internal server error");
    assert_eq!(body["error"]["details"]["error_id"], common::FIXED_ID);

    let search = get(&app, "/api/v3/search?q=ok%20computer").await;
    assert_eq!(search["album_status"], "error");
    assert_eq!(
        titles(&search["albums"]),
        ["OK Computer", "OK Computer Live Bootleg"],
        "library hits still show"
    );
}

#[tokio::test]
async fn unified_search_joins_musicbrainz_with_the_library() {
    let fixtures = Fixtures::start().await;
    let app = app(&fixtures).await;

    let body = get(&app, "/api/v3/search?q=ok%20computer").await;
    assert_eq!(body["album_status"], "ok");
    assert_eq!(
        titles(&body["albums"]),
        ["OK Computer", "OK Computer Live Bootleg", "Kid A"],
        "compilations filtered, library-only rows kept, ranked by score"
    );
    assert_eq!(body["albums"][0]["id"], "album-okc");
    assert_eq!(body["albums"][0]["musicbrainz_id"], OK_COMPUTER);
    assert_eq!(body["albums"][0]["in_library"], true);
    assert_eq!(body["albums"][1]["musicbrainz_id"], Value::Null);
    assert!(body["albums"][2]["id"].is_null());
    assert_eq!(body["albums"][2]["requested"], true);
    assert_eq!(body["top_album"]["title"], "OK Computer");
    assert_eq!(
        titles(&body["artists"]),
        ["Radiohead", "Radiohead Tribute Band"]
    );
    assert_eq!(body["artists"][0]["in_library"], true);
    assert_eq!(body["artists"][1]["in_library"], false);
    assert_eq!(body["artists"][1]["disambiguation"], "Polish tribute act");

    let page = get(&app, "/api/v3/search/albums?q=ok%20computer&limit=2").await;
    assert_eq!(page["status"], "ok");
    assert_eq!(titles(&page["results"]), ["OK Computer", "Kid A"]);
    assert_eq!(page["results"][0]["id"], "album-okc");

    let suggest = get(&app, "/api/v3/search/suggest?q=ok%20computer&limit=3").await;
    let album = suggest["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "album")
        .unwrap();
    assert_eq!(album["title"], "OK Computer");
    assert_eq!(album["id"], "album-okc");
    assert_eq!(album["in_library"], true);
}

#[tokio::test]
async fn auth_matrix_rejects_anonymous_callers_and_bad_ids() {
    let fixtures = Fixtures::start().await;
    let app = app(&fixtures).await;
    for uri in [
        format!("/api/v3/artists/{ARTIST}"),
        format!("/api/v3/artists/{ARTIST}/extended"),
        format!("/api/v3/artists/{ARTIST}/releases"),
        format!("/api/v3/artists/{ARTIST}/similar"),
        format!("/api/v3/artists/{ARTIST}/top-songs"),
        format!("/api/v3/artists/{ARTIST}/top-albums"),
        format!("/api/v3/artists/{ARTIST}/lastfm?artist_name=Radiohead"),
        format!("/api/v3/artists/{ARTIST}/purchase-options"),
        format!("/api/v3/albums/{OK_COMPUTER}"),
        format!("/api/v3/albums/{OK_COMPUTER}/basic"),
        format!("/api/v3/albums/{OK_COMPUTER}/tracks"),
        format!("/api/v3/albums/{OK_COMPUTER}/editions"),
        format!("/api/v3/albums/{OK_COMPUTER}/similar?artist_id={ARTIST}"),
        format!("/api/v3/albums/{OK_COMPUTER}/more-by-artist?artist_id={ARTIST}"),
        format!("/api/v3/albums/{OK_COMPUTER}/lastfm?artist_name=a&album_name=b"),
        format!("/api/v3/albums/{OK_COMPUTER}/purchase-options"),
    ] {
        let (status, body) = call(&app, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(body["error"]["code"], "UNAUTHORIZED", "{uri}");
    }
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v3/albums/{OK_COMPUTER}/refresh"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = call(&app, Method::GET, "/api/v3/artists/radiohead", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_INPUT");
    assert_eq!(
        fixtures.hits("/mb/ws/2/artist/radiohead"),
        0,
        "bad ids never reach MusicBrainz"
    );
}
