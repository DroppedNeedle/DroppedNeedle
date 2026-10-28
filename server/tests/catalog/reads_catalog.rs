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
use droppedneedle_auth::session::middleware::require_session;
use droppedneedle_auth::users::models::LastFmConnection;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_catalog::reads::catalog::{
    self, Catalog, CatalogDeps,
    library::LocalCatalog,
    upstream::{CatalogSettings, Upstream},
};
use droppedneedle_catalog::reads::search::{self, SearchDeps, service::SearchService};
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_kernel::runtime_config::sections::{
    ListenSource, MbSourceMode, MusicBrainzSettings, UserPreferences,
};
use droppedneedle_providers::Providers;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt as _;

use crate::common::TestApp;
use crate::common::auth::AuthRig;

const TOKEN: &str = "dn-catalog-test-token-000000000001";
/// The test user: the rig's id for `ada`.
const USER: &str = "user-ada";

const ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
const KID_A: &str = "1c6d2e3b-0a5f-4d8e-9f3a-5d6e8b2c4a10";
const RELEASE_CD: &str = "6b1c7d8a-5f0e-4cd3-8e8f-0c1d3a7b9f65";
const UNKNOWN_ALBUM: &str = "00000000-1111-4222-8333-444444444444";
const DUMMY_RELEASE: &str = "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f";
const DUMMY_GROUP: &str = "4d5e6f70-8b9c-4dae-9f10-2b3c4d5e6f70";

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
        Self::start_with(routes()).await
    }

    async fn start_with(routes: Vec<Route>) -> Self {
        let routes = Arc::new(routes);
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

    /// The providers with every catalog provider on this server.
    fn providers(&self, http: &droppedneedle_kernel::http_client::HttpClientFactory) -> Providers {
        Providers::unpaced_at(
            Arc::new(droppedneedle_providers::ReqwestWire::new(http)),
            droppedneedle_providers::ProviderEndpoints {
                listenbrainz: format!("{}/lb", self.base),
                lastfm: format!("{}/lastfm/2.0/", self.base),
                audiodb: format!("{}/audiodb", self.base),
                wikidata: format!("{}/wikidata", self.base),
                wikipedia: format!("{}/wikipedia-{{lang}}", self.base),
                commons: format!("{}/commons", self.base),
                itunes: format!("{}/itunes/search", self.base),
                deezer: format!("{}/deezer", self.base),
                lrclib: format!("{}/lrclib", self.base),
                ..droppedneedle_providers::ProviderEndpoints::closed()
            },
        )
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
            "/lastfm/2.0/".to_owned(),
            Some(("mbid", ARTIST)),
            upstream_fixture("lastfm_artist_top_albums"),
        ),
        route(
            "/lastfm/2.0/".to_owned(),
            Some(("method", "artist.getTopAlbums")),
            upstream_fixture("lastfm_similar_artist_top_albums"),
        ),
        route(
            format!("/mb/ws/2/release/{DUMMY_RELEASE}"),
            None,
            upstream_fixture("mb_release_dummy"),
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
    fn musicbrainz(&self) -> Option<MusicBrainzSettings> {
        Some(MusicBrainzSettings {
            source_mode: MbSourceMode::Mirror,
            api_url: self.mb_url.clone(),
            rate_limit: 0.0,
            source_id: "test-source".to_owned(),
            generation: 1,
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
        true
    }

    fn primary_source(&self) -> ListenSource {
        ListenSource::Listenbrainz
    }

    fn store_region(&self) -> Option<String> {
        Some("US".to_owned())
    }
}

/// The local catalog over the migrated database in `dir`.
fn local_catalog(dir: &crate::common::ScratchDir) -> LocalCatalog {
    LocalCatalog::new(
        droppedneedle_kernel::db::Db::open_for_tests(&dir.join("app.db")).expect("database opens"),
    )
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
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
         VALUES ('album-okc', 0, 'artist-radiohead', 'primary'), \
         ('album-bootleg', 0, 'artist-radiohead', 'primary')"
            .to_owned(),
        format!(
            "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, release_mbid, decision_source, \
             selected_at) VALUES ('album-okc', 'musicbrainz', '{OK_COMPUTER}', '{RELEASE_CD}', \
             'automatic', 0)"
        ),
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         artist_name, artist_name_folded, album_title, album_title_folded, disc_number, \
         track_number, duration_seconds, file_format, ingest_source, imported_at, \
         membership_source) VALUES ('track-airbag', 'album-okc', 'root-1', \
         '/music/airbag.flac', 'airbag.flac', 'hash-1', 1000, 0, 'stat-1', 'Airbag', 'airbag', \
         'Radiohead', 'radiohead', 'OK Computer', 'ok computer', 1, 1, 284.0, 'flac', 'scan', \
         0, 'manual'), ('track-bootleg', 'album-bootleg', 'root-1', '/music/bootleg.flac', \
         'bootleg.flac', 'hash-2', 1000, 0, 'stat-2', 'Lucky', 'lucky', 'Radiohead', \
         'radiohead', 'OK Computer Live Bootleg', 'ok computer live bootleg', 1, 1, 260.0, \
         'flac', 'scan', 0, 'manual')"
            .to_owned(),
        format!(
            "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
             album_title, requested_at, status, request_kind) VALUES \
             ('{KID_A}', '{KID_A}', 'Radiohead', 'Kid A', 1788220800000, 'pending', \
             'album')"
        ),
    ];
    for statement in statements {
        sqlx::query(&statement).execute(pool).await.unwrap();
    }
}

/// The catalog and search routes behind the session gate and request
/// scope, with MusicBrainz at `mb_url`. The test user has a Last.fm key
/// and is signed in with [`TOKEN`]. Hold the rig and the app while the
/// router serves: the rig's database and the request wrapper go with them.
async fn app_with(fixtures: &Fixtures, mb_url: String) -> (axum::Router, (AuthRig, TestApp)) {
    let (pool, dir) = crate::common::migrated_pool("reads-catalog").await;
    seed_library(&pool).await;
    let rig = AuthRig::new("reads-catalog").await;
    rig.seed_user("ada", Role::User).await;
    rig.sign_in(USER, TOKEN).await;
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
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
    let upstream = Upstream::new(
        Arc::new(fixtures.providers(&http)),
        Arc::new(TestSettings { mb_url }),
        rig.deps.clone(),
    );
    let catalog = Catalog::new(upstream, local_catalog(&dir));
    let search = SearchDeps::new(
        SearchService::new(pool).with_remote(catalog.clone()),
        Arc::new(search::ports::UnconfiguredEnrichment),
    );
    let scope = TestApp::start("reads-catalog").await;
    let app = scope
        .request_scope(
            catalog::routes()
                .with_state(CatalogDeps { catalog })
                .into_router()
                .merge(search::routes().with_state(search).into_router())
                .layer(middleware::from_fn_with_state(
                    rig.session_gate(),
                    require_session,
                )),
        )
        .fallback(droppedneedle::handlers::fallback_404);
    (app, (rig, scope))
}

async fn app(fixtures: &Fixtures) -> (axum::Router, (AuthRig, TestApp)) {
    app_with(fixtures, format!("{}/mb/ws/2", fixtures.base)).await
}

async fn call(
    app: &axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _, body) = call_full(app, method, uri, token).await;
    (status, body)
}

/// As [`call`], with the response headers.
async fn call_full(
    app: &axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
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
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
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
    let (app, _auth) = app(&fixtures).await;

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
    assert_eq!(titles(&body["albums"]), ["Kid A", "OK Computer"]);
    assert_eq!(titles(&body["eps"]), ["My Iron Lung"]);
    assert_eq!(titles(&body["singles"]), ["Creep"]);
    assert_eq!(body["albums"][1]["in_library"], true);
    assert_eq!(body["release_group_count"], 6);
    assert_eq!(body["appears_in_library"], false);
    assert_eq!(body["followed"], false);
    assert_eq!(body["auto_download_state"], "none");

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
    let (app, _auth) = app(&fixtures).await;

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
    let (app, _auth) = app(&fixtures).await;

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
    let (app, _auth) = app(&fixtures).await;

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

    let lastfm_albums = get(
        &app,
        &format!("/api/v3/artists/{ARTIST}/top-albums?source=lastfm"),
    )
    .await;
    assert_eq!(lastfm_albums["source"], "lastfm");
    let groups: Vec<&Value> = lastfm_albums["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|album| &album["musicbrainz_id"])
        .collect();
    assert_eq!(
        groups,
        [&json!(OK_COMPUTER), &json!(KID_A), &Value::Null],
        "release ids and deluxe titles match the discography; unknown titles stay unlinked"
    );
    assert_eq!(lastfm_albums["albums"][0]["in_library"], true);
    assert_eq!(lastfm_albums["albums"][1]["requested"], true);

    let similar_albums = get(
        &app,
        &format!("/api/v3/albums/{OK_COMPUTER}/similar?artist_id={ARTIST}&count=2"),
    )
    .await;
    assert_eq!(
        similar_albums["albums"],
        json!([{
            "musicbrainz_id": DUMMY_GROUP,
            "title": "Dummy",
            "artist_name": "Portishead",
            "artist_id": "b7539c32-53e7-4908-bda3-81449c367da6",
            "in_library": false,
            "requested": false,
            "cover_url": format!("/api/v3/covers/release-group/{DUMMY_GROUP}?size=500")
        }]),
        "no ListenBrainz albums for the similar artists, so Last.fm fills in"
    );

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
    let (app, _auth) = app(&fixtures).await;

    let album = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}")).await;
    assert_eq!(album["title"], "OK Computer");
    assert_eq!(album["artist_name"], "Radiohead");
    assert_eq!(album["artist_id"], ARTIST);
    assert_eq!(album["year"], 1997);
    assert_eq!(album["in_library"], true);
    assert_eq!(
        album["cover_url"],
        format!("/api/v3/covers/release-group/{OK_COMPUTER}?size=500")
    );
    assert_eq!(album["selected_release_mbid"], RELEASE_CD);
    assert_eq!(album["pick_basis"], "owned");
    assert_eq!(titles(&album["tracks"]), ["Airbag", "Paranoid Android"]);
    assert_eq!(album["tracks"][0]["length"], 284_000);
    assert_eq!(album["total_length"], 667_000);
    assert_eq!(album["label"], "Parlophone");
    let by_local = get(&app, "/api/v3/albums/album-okc/basic").await;
    assert_eq!(
        by_local["musicbrainz_id"], OK_COMPUTER,
        "library ids resolve"
    );
    let (status, _) = call(
        &app,
        Method::GET,
        "/api/v3/albums/album-bootleg/basic",
        Some(TOKEN),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unidentified library album"
    );

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
    let (app, _auth) = app_with(&fixtures, "http://127.0.0.1:9/ws/2".to_owned()).await;

    let artist = get(&app, &format!("/api/v3/artists/{ARTIST}")).await;
    assert_eq!(artist["source"], "library");
    assert_eq!(artist["name"], "Radiohead");
    assert_eq!(artist["service_status"], json!({"musicbrainz": "error"}));
    assert_eq!(titles(&artist["albums"]), ["OK Computer"]);

    let releases = get(&app, &format!("/api/v3/artists/{ARTIST}/releases")).await;
    assert_eq!(releases["source"], "library");
    assert_eq!(titles(&releases["albums"]), ["OK Computer"]);

    let album = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}")).await;
    assert_eq!(album["source"], "library");
    assert_eq!(titles(&album["tracks"]), ["Airbag"]);

    let (status, headers, body) = call_full(
        &app,
        Method::GET,
        &format!("/api/v3/albums/{UNKNOWN_ALBUM}/basic"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "PROVIDER_UNAVAILABLE");
    assert_eq!(body["error"]["details"]["provider"], "musicbrainz");
    assert_eq!(
        body["error"]["details"]["error_id"],
        headers["x-request-id"].to_str().unwrap()
    );

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
    let (app, _auth) = app(&fixtures).await;

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
    // The library-only bootleg must show in "see all" too, not only in the overview.
    assert_eq!(
        titles(&page["results"]),
        ["OK Computer", "OK Computer Live Bootleg", "Kid A"]
    );
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

/// The library scope answers from the library alone: MusicBrainz is never
/// asked, and only library rows come back.
#[tokio::test]
async fn library_scoped_search_never_asks_musicbrainz() {
    let fixtures = Fixtures::start().await;
    let (app, _auth) = app(&fixtures).await;

    let body = get(&app, "/api/v3/search?q=ok%20computer&scope=library").await;

    assert_eq!(
        titles(&body["albums"]),
        ["OK Computer", "OK Computer Live Bootleg"]
    );
    for row in body["albums"].as_array().unwrap() {
        assert!(row["id"].is_string(), "library rows only: {row}");
    }
    assert_eq!(fixtures.hits("/mb/ws/2/release-group"), 0);
    assert_eq!(fixtures.hits("/mb/ws/2/artist"), 0);
}

#[tokio::test]
async fn lastfm_reads_fall_back_to_the_instance_key() {
    let rig = AuthRig::new("reads-catalog").await;
    let upstream = Upstream::new(
        Arc::new(Providers::unpaced(Arc::new(
            droppedneedle_providers::executor::ScriptedWire::new(),
        ))),
        Arc::new(TestSettings {
            mb_url: String::new(),
        }),
        rig.deps.clone(),
    );
    assert!(
        upstream.lastfm("no-key-user").await.is_none(),
        "no user key and no instance key"
    );
    rig.set_lastfm_instance_key("instance-key");
    let (_, creds) = upstream.lastfm("no-key-user").await.expect("instance key");
    assert_eq!(creds.api_key, "instance-key");
}

#[tokio::test]
async fn auth_matrix_rejects_anonymous_callers_and_bad_ids() {
    let fixtures = Fixtures::start().await;
    let (app, _auth) = app(&fixtures).await;
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

/// A portrait lookup that failed is not "no image": with TheAudioDB
/// holding no thumbnail and Commons down, the artist's image is asked
/// again after the short outage wait, never held blank for hours.
#[tokio::test]
async fn a_failed_portrait_lookup_keeps_the_short_outage_marker() {
    use droppedneedle_catalog::reads::catalog::precache::CatalogArtistImages;
    use droppedneedle_catalog::reads::platform::artwork::{
        ArtworkService, OUTAGE_TTL_SECS,
        cache::{ArtworkCache, KeyEntry},
        local::LocalArtwork,
    };

    let mut routes = routes();
    for route in &mut routes {
        if route.path == "/audiodb/123/artist-mb.php" {
            route.body = r#"{"artists": null}"#.to_owned();
        }
        if route.path == "/commons/w/api.php" {
            route.status = 503;
        }
    }
    let fixtures = Fixtures::start_with(routes).await;
    let (pool, dir) = crate::common::migrated_pool("reads-catalog-portrait").await;
    let rig = AuthRig::new("reads-catalog-portrait").await;
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
    let providers = Arc::new(fixtures.providers(&http));
    let catalog = Catalog::new(
        Upstream::new(
            providers.clone(),
            Arc::new(TestSettings {
                mb_url: format!("{}/mb/ws/2", fixtures.base),
            }),
            rig.deps.clone(),
        ),
        local_catalog(&dir),
    );
    let config = crate::common::config_store(&dir.join("settings"));
    let artwork = ArtworkService::new(
        ArtworkCache::new(
            dir.join("covers"),
            1024 * 1024,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
        LocalArtwork::new(pool),
        None,
        config.live(),
    )
    .with_images(Arc::new(providers.images.clone()))
    .with_artist_source(Arc::new(CatalogArtistImages(catalog)));

    let before = unix_now();
    assert!(
        !artwork.warm_artist_image(ARTIST).await,
        "no image to cache"
    );
    assert!(
        fixtures.hits("/commons/w/api.php") > 0,
        "the portrait was asked for"
    );
    let cache = ArtworkCache::new(
        dir.join("covers"),
        1024 * 1024,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let Some(KeyEntry::Unreachable { until }) = cache.key(&format!("artist:{ARTIST}")).await else {
        panic!("the outage is remembered");
    };
    assert!(
        (before + OUTAGE_TTL_SECS..=unix_now() + OUTAGE_TTL_SECS).contains(&until),
        "remembered for the outage wait, until {until} (asked at {before})"
    );
}

/// TheAudioDB still names some artist pictures over plain http. Such a
/// picture is fetched over https from the same host, not refused, which
/// stored a four-hour "no picture" for artists TheAudioDB has a thumbnail
/// for.
#[tokio::test]
async fn an_audiodb_picture_named_over_http_is_fetched_over_https() {
    use droppedneedle_catalog::reads::catalog::precache::CatalogArtistImages;
    use droppedneedle_catalog::reads::platform::artwork::{
        ArtworkService, cache::ArtworkCache, local::LocalArtwork,
    };
    use droppedneedle_providers::executor::ScriptedWire;
    use droppedneedle_providers::{ProviderEndpoints, audiodb, musicbrainz};

    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{}/artist/{ARTIST}", musicbrainz::MB_API_BASE),
        200,
        &[],
        fixture("mb_artist.json"),
    );
    // Shaped as TheAudioDB answered for Hiroyuki Sawano on 2026-10-22
    // (thumbnail over http), with this suite's artist.
    wire.answer(
        &format!("{}/123/artist-mb.php", audiodb::DEFAULT_BASE_URL),
        200,
        &[],
        json!({"artists": [{
            "idArtist": "111239",
            "strArtist": "Radiohead",
            "strMusicBrainzID": ARTIST,
            "strArtistThumb": "http://r2.theaudiodb.com/images/media/artist/thumb/radiohead.jpg",
        }]})
        .to_string(),
    );
    wire.answer(
        "https://r2.theaudiodb.com/images/media/artist/thumb/radiohead.jpg",
        200,
        &[("content-type", "image/png")],
        PNG,
    );
    let providers = Arc::new(Providers::over_wire(
        wire.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        ProviderEndpoints {
            musicbrainz: musicbrainz::MB_API_BASE.to_owned(),
            audiodb: audiodb::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let (pool, dir) = crate::common::migrated_pool("reads-catalog-audiodb-http").await;
    let rig = AuthRig::new("reads-catalog-audiodb-http").await;
    let catalog = Catalog::new(
        Upstream::new(
            providers.clone(),
            Arc::new(TestSettings {
                mb_url: musicbrainz::MB_API_BASE.to_owned(),
            }),
            rig.deps.clone(),
        ),
        local_catalog(&dir),
    );
    let config = crate::common::config_store(&dir.join("settings"));
    let artwork = ArtworkService::new(
        ArtworkCache::new(
            dir.join("covers"),
            1024 * 1024,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
        LocalArtwork::new(pool),
        None,
        config.live(),
    )
    .with_images(Arc::new(providers.images.clone()))
    .with_artist_source(Arc::new(CatalogArtistImages(catalog)));

    assert!(
        artwork.warm_artist_image(ARTIST).await,
        "the picture is fetched and stored"
    );
}

/// A cold artist image waits its turn behind a busy MusicBrainz instead of
/// being turned down like a page: the image resolve asks at image
/// priority, so it finds the artist and its portrait rather than coming
/// back incomplete, which would hold the image blank for the outage wait.
/// TheAudioDB, with nothing cached, is left to the background fill.
#[tokio::test]
async fn a_cold_artist_image_waits_out_a_busy_musicbrainz() {
    use droppedneedle_catalog::reads::catalog::precache::CatalogArtistImages;
    use droppedneedle_catalog::reads::platform::artwork::ArtistImageSource as _;
    use droppedneedle_providers::executor::ScriptedWire;
    use droppedneedle_providers::{
        ProviderEndpoints, RequestPriority, audiodb, musicbrainz, wikidata,
    };
    use std::time::Duration;

    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{}/artist/{ARTIST}", musicbrainz::MB_API_BASE),
        200,
        &[],
        fixture("mb_artist.json"),
    );
    wire.answer(
        &format!("{}/123/artist-mb.php", audiodb::DEFAULT_BASE_URL),
        200,
        &[],
        upstream_fixture("audiodb_artist"),
    );
    // No Wikidata item carries the MBID, so the portrait comes through
    // MusicBrainz's Wikidata link.
    wire.answer(
        &format!("{}/w/api.php", wikidata::WIKIDATA_BASE),
        200,
        &[],
        include_str!("../fixtures/wikidata/search_p434_no_hits.json"),
    );
    wire.answer(
        &format!("{}/w/api.php", wikidata::WIKIDATA_BASE),
        200,
        &[],
        upstream_fixture("wikidata_claims"),
    );
    wire.answer(
        &format!("{}/w/api.php", wikidata::COMMONS_BASE),
        200,
        &[],
        upstream_fixture("commons_imageinfo"),
    );
    let providers = Arc::new(Providers::over_wire(
        wire.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        ProviderEndpoints {
            musicbrainz: musicbrainz::MB_API_BASE.to_owned(),
            audiodb: audiodb::DEFAULT_BASE_URL.to_owned(),
            wikidata: wikidata::WIKIDATA_BASE.to_owned(),
            commons: wikidata::COMMONS_BASE.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let dir = crate::common::ScratchDir::new("reads-catalog-busy-mb");
    crate::common::migrated_db(&dir.join("app.db"));
    let rig = AuthRig::new("reads-catalog-busy-mb").await;
    let catalog = Catalog::new(
        Upstream::new(
            providers.clone(),
            Arc::new(TestSettings {
                mb_url: musicbrainz::MB_API_BASE.to_owned(),
            }),
            rig.deps.clone(),
        ),
        local_catalog(&dir),
    );

    // One request a second, resting two more: a page would wait three.
    tokio::time::pause();
    providers
        .limiter("musicbrainz")
        .unwrap()
        .cool_down(Duration::from_secs(2));
    let started = tokio::time::Instant::now();
    let candidates = CatalogArtistImages(catalog)
        .candidates(ARTIST, RequestPriority::ImageFetch)
        .await;
    assert!(!candidates.incomplete, "the resolve is settled");
    let urls: Vec<&str> = candidates
        .images
        .iter()
        .map(|image| image.url.as_str())
        .collect();
    assert_eq!(
        urls,
        ["https://upload.wikimedia.org/wikipedia/commons/a/a1/Radiohead_2016.jpg"]
    );
    assert!(started.elapsed() >= Duration::from_secs(2), "it waited");
    assert!(
        wire.sent()
            .iter()
            .all(|sent| !sent.url.as_str().starts_with(audiodb::DEFAULT_BASE_URL)),
        "TheAudioDB is not asked while the page waits"
    );
}

/// A Commons portrait keeps its licence attribution: once the artist's
/// picture is resolved from Commons, the credit route answers the author
/// and licence its file page gives, the author's HTML reduced to plain
/// text, and a link to the file page.
#[tokio::test]
async fn a_commons_portrait_answers_its_credit_as_plain_text() {
    use droppedneedle_catalog::reads::catalog::precache::CatalogArtistImages;
    use droppedneedle_catalog::reads::platform::artwork::{
        ArtworkService, cache::ArtworkCache, local::LocalArtwork,
    };
    use droppedneedle_catalog::reads::platform::covers::{self, CoversState};
    use droppedneedle_providers::executor::ScriptedWire;
    use droppedneedle_providers::{ProviderEndpoints, wikidata};

    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let wire = Arc::new(ScriptedWire::new());
    // One Wikidata item carries the MBID, and its only picture is on Commons.
    let wikidata_api = format!("{}/w/api.php", wikidata::WIKIDATA_BASE);
    wire.answer(
        &wikidata_api,
        200,
        &[],
        include_str!("../fixtures/wikidata/search_p434.json"),
    );
    wire.answer(
        &wikidata_api,
        200,
        &[],
        include_str!("../fixtures/wikidata/claims_p18_found.json"),
    );
    wire.answer(
        &format!("{}/w/api.php", wikidata::COMMONS_BASE),
        200,
        &[],
        include_str!("../fixtures/wikidata/commons_imageinfo_credit.json"),
    );
    wire.answer(
        "https://thumb.wikimedia.org/wikipedia/commons/thumb/a/a1/RadioheadO2211125_composite.jpg/1280px-RadioheadO2211125_composite.jpg",
        200,
        &[("content-type", "image/png")],
        PNG,
    );
    let providers = Arc::new(Providers::over_wire(
        wire.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        ProviderEndpoints {
            wikidata: wikidata::WIKIDATA_BASE.to_owned(),
            commons: wikidata::COMMONS_BASE.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let (pool, dir) = crate::common::migrated_pool("reads-catalog-credit").await;
    let rig = AuthRig::new("reads-catalog-credit").await;
    let catalog = Catalog::new(
        Upstream::new(
            providers.clone(),
            Arc::new(TestSettings {
                mb_url: "http://127.0.0.1:9".to_owned(),
            }),
            rig.deps.clone(),
        ),
        local_catalog(&dir),
    );
    let config = crate::common::config_store(&dir.join("settings"));
    let artwork = ArtworkService::new(
        ArtworkCache::new(
            dir.join("covers"),
            1024 * 1024,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
        LocalArtwork::new(pool),
        None,
        config.live(),
    )
    .with_images(Arc::new(providers.images.clone()))
    .with_artist_source(Arc::new(CatalogArtistImages(catalog)));
    let app = covers::routes()
        .with_state(CoversState::new(Arc::new(artwork)))
        .into_router();
    let get = |uri: String| {
        let app = app.clone();
        async move {
            let response = app
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            (status, body)
        }
    };

    let (status, image) = get(format!("/api/v3/covers/artist/{ARTIST}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&image[..], PNG);

    let (status, body) = get(format!("/api/v3/covers/artist/{ARTIST}/credit")).await;
    assert_eq!(status, StatusCode::OK);
    let credit: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        credit,
        json!({"credit": {
            "author": "Raph_PH",
            "licence": "CC BY 4.0",
            "licence_url": "https://creativecommons.org/licenses/by/4.0",
            "file_url": "https://commons.wikimedia.org/wiki/File:RadioheadO2211125_composite.jpg",
        }})
    );
}

/// A section built past a failed ListenBrainz read is not cached as the
/// answer: once ListenBrainz is back, the next read gets the whole one.
/// Top songs lose their album links and similar albums lose an artist
/// while the read behind them fails.
#[tokio::test]
async fn a_passing_failure_is_not_cached_as_a_short_section() {
    use droppedneedle_catalog::reads::catalog::models::AlbumRef;
    use droppedneedle_kernel::runtime_config::sections::ListenSource;
    use droppedneedle_providers::executor::{Method, Scripted, ScriptedWire};
    use droppedneedle_providers::{
        ProviderEndpoints, RequestPriority, TransportKind, listenbrainz,
    };

    const LANA: &str = "b7539c32-53e7-4908-bda3-81449c367da6";
    const PORTISHEAD: &str = "8f6bd1e4-fbe1-4f50-aa9b-94c450ec0f11";
    const CREEP: &str = "5e6f7a8b-1111-4c2d-8e3f-4a5b6c7d8e9f";
    const BORN_TO_DIE: &str = "5e6f7a8b-2222-4c2d-8e3f-4a5b6c7d8e9f";
    let lb = |path: &str| format!("{}/1/{path}", listenbrainz::DEFAULT_BASE_URL);
    let down = Scripted::Transport(TransportKind::Connect);
    let group = |id: &str, name: &str, artist: &str| {
        json!([{
            "artist_mbids": [],
            "artist_name": artist,
            "release_group_mbid": id,
            "release_group": {"name": name},
            "total_listen_count": 100
        }])
        .to_string()
    };

    // Each failing read fails both of its attempts, then answers.
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &lb(&format!("popularity/top-recordings-for-artist/{ARTIST}")),
        200,
        &[],
        json!([{"recording_name": "Creep", "artist_name": "Radiohead",
                "total_listen_count": 90, "recording_mbid": CREEP}])
        .to_string(),
    );
    let recordings = lb("metadata/recording/");
    wire.on(Method::Post, &recordings, down.clone());
    wire.on(Method::Post, &recordings, down.clone());
    wire.on(
        Method::Post,
        &recordings,
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: json!({CREEP: {"release": {"release_group_mbid": OK_COMPUTER}}})
                .to_string()
                .into_bytes(),
        },
    );
    wire.answer(
        &lb(&format!("lb-radio/artist/{ARTIST}")),
        200,
        &[],
        upstream_fixture("lb_radio_artist"),
    );
    let lana = lb(&format!("popularity/top-release-groups-for-artist/{LANA}"));
    wire.on(Method::Get, &lana, down.clone());
    wire.on(Method::Get, &lana, down);
    wire.answer(
        &lana,
        200,
        &[],
        group(BORN_TO_DIE, "Born to Die", "Lana Del Rey"),
    );
    wire.answer(
        &lb(&format!(
            "popularity/top-release-groups-for-artist/{PORTISHEAD}"
        )),
        200,
        &[],
        group(DUMMY_GROUP, "Dummy", "Portishead"),
    );
    let providers = Arc::new(Providers::over_wire(
        wire,
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    // Puts are queued: a waited delete commits after every put before it,
    // so the second reads see whatever the first ones stored.
    let cache = providers.cache.clone();
    let dir = crate::common::ScratchDir::new("reads-catalog-passing");
    crate::common::migrated_db(&dir.join("app.db"));
    let rig = AuthRig::new("reads-catalog-passing").await;
    let catalog = Catalog::new(
        Upstream::new(
            providers,
            Arc::new(TestSettings {
                mb_url: "http://127.0.0.1:9/ws/2".to_owned(),
            }),
            rig.deps.clone(),
        ),
        local_catalog(&dir),
    );
    tokio::time::pause();

    let songs = || {
        catalog.top_songs(
            USER,
            ARTIST,
            5,
            Some(ListenSource::Listenbrainz),
            RequestPriority::UserInitiated,
        )
    };
    let first = songs().await.unwrap();
    assert_eq!(first.songs[0].release_group_mbid, None, "the lookup failed");
    cache.forget(&[]).await;
    let second = songs().await.unwrap();
    assert_eq!(
        second.songs[0].release_group_mbid.as_deref(),
        Some(OK_COMPUTER),
        "the song links its album once ListenBrainz answers"
    );

    let ids = |albums: &[AlbumRef]| {
        albums
            .iter()
            .filter_map(|album| album.musicbrainz_id.clone())
            .collect::<Vec<_>>()
    };
    let first = catalog
        .similar_albums(USER, OK_COMPUTER, ARTIST, 2)
        .await
        .unwrap();
    assert_eq!(ids(&first.albums), [DUMMY_GROUP], "one artist failed");
    cache.forget(&[]).await;
    let second = catalog
        .similar_albums(USER, OK_COMPUTER, ARTIST, 2)
        .await
        .unwrap();
    assert_eq!(
        ids(&second.albums),
        [BORN_TO_DIE, DUMMY_GROUP],
        "both artists once ListenBrainz answers"
    );
}

/// Provider answers outlive a restart: an album header read once from
/// MusicBrainz is answered from the cache file after the app restarts with
/// MusicBrainz gone.
#[tokio::test]
async fn provider_answers_survive_a_restart() {
    const JSON: [(&str, &str); 1] = [("content-type", "application/json")];
    let hits = Arc::new(Mutex::new(0_usize));
    let counter = hits.clone();
    let group_path = format!("/release-group/{OK_COMPUTER}");
    let stub = axum::Router::new().fallback(move |uri: Uri| {
        let counter = counter.clone();
        let found = uri.path().ends_with(&group_path);
        async move {
            if found {
                *counter.lock().unwrap() += 1;
                (StatusCode::OK, JSON, fixture("mb_release_group.json"))
            } else {
                (StatusCode::NOT_FOUND, JSON, "{}".to_owned())
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, stub).await.unwrap();
    });
    let app = TestApp::start_with(
        "provider-cache-restart",
        crate::common::outbound_at(&origin),
    )
    .await;
    let admin = app.login_admin().await;
    let auth = [("authorization", admin.as_str())];
    let basic = format!("/api/v3/albums/{OK_COMPUTER}/basic");

    let (status, body) = app.call("GET", &basic, &auth, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "OK Computer");
    assert_eq!(*hits.lock().unwrap(), 1);
    app.until("the answer is in the cache file", || async {
        let (_, stats) = app
            .call("GET", "/api/v3/admin/cache/stats", &auth, None)
            .await;
        (stats["entries"].as_u64().unwrap_or(0) > 0).then_some(())
    })
    .await;

    server.abort();
    let _ = server.await;
    let app = app.restart().await;
    let (status, body) = app.call("GET", &basic, &auth, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "OK Computer");
    assert_eq!(*hits.lock().unwrap(), 1, "answered from the cache file");
    app.stop().await;
}

/// Now, by the system clock the cache under test runs on.
fn unix_now() -> u64 {
    use droppedneedle_kernel::clock::Clock as _;
    u64::try_from(droppedneedle_kernel::clock::SystemClock.now_unix()).unwrap_or(0)
}
