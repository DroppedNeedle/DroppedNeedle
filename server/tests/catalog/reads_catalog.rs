//! Artist and album pages and MusicBrainz-backed search, behind the real
//! session gate, against recorded provider payloads served from a
//! loopback fixture server (tests/fixtures/catalog). One test per page,
//! the MusicBrainz outage fallback, and a bad id refused.

use crate::common::Probe;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, Uri},
    middleware,
};
use droppedneedle_auth::session::middleware::require_session;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::users::models::LastFmConnection;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_catalog::metadata::{
    self, Catalog, CatalogDeps,
    library::LocalCatalog,
    upstream::{CatalogSettings, Upstream},
};
use droppedneedle_catalog::search::{self, SearchDeps, service::SearchService};
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_kernel::runtime_config::sections::{
    MbSourceMode, MusicBrainzSettings, UserPreferences,
};
use droppedneedle_providers::Providers;
use serde_json::{Value, json};
use std::path::Path;
use tower::ServiceExt as _;

use crate::common::TestApp;
use crate::common::auth::{AuthRig, test_principal, with_test_principal};

const TOKEN: &str = "dn-catalog-test-token-000000000001";
/// The test user: the rig's id for `ada`.
const USER: &str = "user-ada";

const ARTIST: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
const KID_A: &str = "1c6d2e3b-0a5f-4d8e-9f3a-5d6e8b2c4a10";
const LIVE: &str = "2d7e3f4c-1b6a-4e9f-8a4b-6e7f9c3d5b21";
const RELEASE_CD: &str = "6b1c7d8a-5f0e-4cd3-8e8f-0c1d3a7b9f65";
const UNKNOWN_ALBUM: &str = "00000000-1111-4222-8333-444444444444";
const DUMMY_RELEASE: &str = "3c4d5e6f-7a8b-4c9d-8e0f-1a2b3c4d5e6f";
const DUMMY_GROUP: &str = "4d5e6f70-8b9c-4dae-9f10-2b3c4d5e6f70";
/// Ben Caplan & The Casual Smokers: credited on two releases, on no
/// release group.
const BAND: &str = "2fe9cf1f-8019-496e-870c-24f16d4d36c1";
/// The band's album, its release group credited to Ben Caplan alone.
const BAND_ALBUM: &str = "deb08635-0f11-4b87-b211-66479b61385f";
/// Portishead, as the recorded ListenBrainz and Last.fm answers name them.
const PORTISHEAD_ID: &str = "b7539c32-53e7-4908-bda3-81449c367da6";

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

/// A recorded answer from `tests/fixtures/{provider}`.
fn recorded(provider: &str, name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/{provider}/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("fixture {path}: {error}"))
}

fn audiodb_fixture(name: &str) -> String {
    recorded("audiodb", name)
}

fn wikidata_fixture(name: &str) -> String {
    recorded("wikidata", name)
}

/// The Radiohead intro in the recorded Wikipedia answer (a placeholder for
/// the article's text).
const WIKIPEDIA_INTRO: &str = "Placeholder intro text for the Radiohead article.";
/// The 1280 px thumbnail in the recorded Commons answer for Radiohead's
/// picture.
const COMMONS_THUMB: &str = "https://thumb.wikimedia.org/wikipedia/commons/thumb/a/a1/RadioheadO2211125_composite.jpg/1280px-RadioheadO2211125_composite.jpg?utm_source=commons.wikimedia.org&utm_campaign=imageinfo&utm_content=thumbnail";
/// Radiohead's thumbnail in the recorded TheAudioDB answer.
const AUDIODB_THUMB: &str =
    "https://r2.theaudiodb.com/images/media/artist/thumb/35poth1668098458.jpg";

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
                // Anything else is a bare 404. It stands in for answers never
                // recorded, such as Special:EntityData for an unknown id,
                // which the client reads as absence without decoding.
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
                listenbrainz_labs: format!("{}/labs", self.base),
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
            Some(("artist", BAND)),
            json!({"release-group-count": 0, "release-group-offset": 0, "release-groups": []})
                .to_string(),
        ),
        // Only the band's releases: every other artist browses as missing.
        route(
            "/mb/ws/2/release".to_owned(),
            Some(("artist", BAND)),
            fixture("mb_artist_releases_band.json"),
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
            audiodb_fixture("artist_mb_found.json"),
        ),
        route(
            "/wikidata/wiki/Special:EntityData/Q44190.json".to_owned(),
            None,
            wikidata_fixture("entity_found.json"),
        ),
        route(
            "/wikidata/w/api.php".to_owned(),
            Some(("action", "wbgetclaims")),
            wikidata_fixture("claims_p18_found.json"),
        ),
        route(
            "/wikipedia-en/w/api.php".to_owned(),
            Some(("prop", "extracts")),
            wikidata_fixture("extract_found.json"),
        ),
        route(
            "/commons/w/api.php".to_owned(),
            Some(("prop", "imageinfo")),
            wikidata_fixture("commons_imageinfo_credit.json"),
        ),
        route(
            "/labs/similar-artists/json".to_owned(),
            Some(("artist_mbids", ARTIST)),
            upstream_fixture("labs_similar_artists"),
        ),
        route(
            "/lb/1/popularity/artist".to_owned(),
            None,
            upstream_fixture("lb_artist_popularity"),
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
            Some(("method", "artist.getSimilar")),
            json!({"similarartists": {"artist": [{
                "name": "Portishead",
                "mbid": PORTISHEAD_ID,
                "match": "1",
            }]}})
            .to_string(),
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

    fn store_region(&self) -> Option<String> {
        Some("US".to_owned())
    }
}

/// The catalog over `upstream` and the migrated database in `dir`, with no
/// follow store and no plugins, its image warms queued on that database's
/// jobs table.
fn catalog_on(upstream: Upstream, dir: &crate::common::ScratchDir) -> Catalog {
    let db =
        droppedneedle_kernel::db::Db::open_for_tests(&dir.join("app.db")).expect("database opens");
    let sources = crate::common::bundles::quiet_page_sources(
        &db,
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    Catalog::new(upstream, LocalCatalog::new(db), sources)
}

/// The library: Radiohead with OK Computer (identified as the CD edition,
/// one indexed track), an unidentified bootleg, and an open request for
/// Kid A.
fn seed_library(app_db: &Path) {
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
        // Radiohead also guests on someone else's album.
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('artist-host', 'Host Band', 'host band', 'group', 0, 0)"
            .to_owned(),
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
         created_at, updated_at) VALUES ('album-host', 'root-1', 'g-host', 'Guest Spot', \
         'guest spot', 'Host Band', 'host band', 'artist-host', 'manual', 0, 0)"
            .to_owned(),
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
         VALUES ('album-host', 0, 'artist-host', 'primary')"
            .to_owned(),
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         artist_name, artist_name_folded, album_title, album_title_folded, disc_number, \
         track_number, duration_seconds, file_format, ingest_source, imported_at, \
         membership_source) VALUES ('track-host', 'album-host', 'root-1', '/music/host.flac', \
         'host.flac', 'hash-host', 1000, 0, 'stat-host', 'Duet', 'duet', 'Host Band', \
         'host band', 'Guest Spot', 'guest spot', 1, 1, 200.0, 'flac', 'scan', 0, 'manual')"
            .to_owned(),
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
         VALUES ('track-host', 0, 'artist-radiohead', 'guest')"
            .to_owned(),
    ];
    for statement in statements {
        app_db.exec(&statement, []);
    }
}

/// Link the test user's ListenBrainz account: ListenBrainz gives an
/// artist's popular songs and albums only to a linked account.
async fn link_listenbrainz(rig: &AuthRig) {
    crate::common::listening_store(&rig.platform)
        .save_listenbrainz(USER, "ada-lb", "ada-token")
        .await
        .expect("ListenBrainz link");
}

/// The catalog and search routes behind the session gate and request
/// scope, with MusicBrainz at `mb_url`. The test user has a Last.fm key
/// and a linked ListenBrainz account, and is signed in with [`TOKEN`]. Hold the rig and the app while the
/// router serves: the rig's database and the request wrapper go with them.
async fn app_with(fixtures: &Fixtures, mb_url: String) -> (axum::Router, (AuthRig, TestApp)) {
    app_seeded(fixtures, mb_url, &[]).await
}

/// As [`app_with`], with `extra` statements run after the shared seed.
async fn app_seeded(
    fixtures: &Fixtures,
    mb_url: String,
    extra: &[String],
) -> (axum::Router, (AuthRig, TestApp)) {
    let (app_db, dir) = crate::common::migrated_file("reads-catalog");
    seed_library(&app_db);
    for statement in extra {
        app_db.exec(statement, []);
    }
    let rig = AuthRig::new("reads-catalog").await;
    rig.seed_user("ada", Role::User).await;
    link_listenbrainz(&rig).await;
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
        crate::common::listening_store(&rig.platform),
    );
    let catalog = catalog_on(upstream, &dir);
    let search = SearchDeps::new(
        SearchService::new(
            droppedneedle_kernel::db::Db::open_for_tests(&app_db).unwrap(),
            catalog.clone(),
        ),
        Arc::new(droppedneedle_catalog::test_support::UnconfiguredEnrichment),
    );
    let scope = TestApp::start("reads-catalog").await;
    let app = scope
        .request_scope(
            metadata::routes()
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
    assert_eq!(body["appears_in_library"], true);
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

    // An id that is no MusicBrainz id is refused before MusicBrainz.
    let (status, body) = call(&app, Method::GET, "/api/v3/artists/radiohead", Some(TOKEN)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a bad id");
    assert_eq!(body["error"]["code"], "INVALID_INPUT", "a bad id");
    assert_eq!(
        fixtures.hits("/mb/ws/2/artist/radiohead"),
        0,
        "bad ids never reach MusicBrainz"
    );
}

/// The artist header's TheAudioDB images and fanart come from the fill,
/// kept under the key they were asked with. TheAudioDB's free key allows
/// one call every two seconds, so a cold header answers its biography and
/// portrait without asking it, and shows the images once the background
/// fill has stored them. Once the saved key changes, art kept under the
/// old key is not served, and the new key is asked.
#[tokio::test]
async fn audiodb_art_comes_from_the_fill_under_its_key() {
    let mut routes = routes();
    routes.push(Route {
        path: "/audiodb/premium-key/artist-mb.php".to_owned(),
        param: Some(("i", ARTIST)),
        status: 200,
        body: audiodb_fixture("artist_mb_not_found.json"),
    });
    let fixtures = Fixtures::start_with(routes).await;
    let (_db, dir) = crate::common::migrated_file("reads-catalog-header");
    let rig = AuthRig::new("reads-catalog-header").await;
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
    let settings = Arc::new(KeyedSettings {
        inner: TestSettings {
            mb_url: format!("{}/mb/ws/2", fixtures.base),
        },
        audiodb_key: Mutex::new(String::new()),
    });
    let catalog = catalog_on(
        Upstream::new(
            Arc::new(fixtures.providers(&http)),
            settings.clone(),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );

    let cold = catalog.artist_extended(ARTIST).await.unwrap();
    assert_eq!(
        cold.description.as_deref(),
        Some(WIKIPEDIA_INTRO),
        "cold header"
    );
    assert_eq!(cold.image.as_deref(), Some(COMMONS_THUMB), "cold header");
    assert_eq!(cold.images, Default::default(), "cold header");
    assert_eq!(
        fixtures.hits("/audiodb/123/artist-mb.php"),
        0,
        "the header never dials out to TheAudioDB"
    );

    assert!(
        catalog.fill_artist_images(ARTIST).await,
        "the free key's fill stores them"
    );
    let warm = catalog.artist_extended(ARTIST).await.unwrap();
    assert_eq!(
        warm.images.thumb_url.as_deref(),
        Some(AUDIODB_THUMB),
        "after the fill"
    );
    assert_eq!(
        fixtures.hits("/audiodb/123/artist-mb.php"),
        1,
        "after the fill"
    );

    *settings.audiodb_key.lock().unwrap() = "premium-key".to_owned();
    catalog.fill_artist_images(ARTIST).await;
    let premium = catalog.artist_extended(ARTIST).await.unwrap();
    assert_eq!(
        fixtures.hits("/audiodb/premium-key/artist-mb.php"),
        1,
        "another key"
    );
    assert_eq!(premium.images.thumb_url, None, "another key");
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

/// A popular song the library holds on the artist's own album points at
/// that copy, at its place there, so the row plays the file. A song of the
/// same title on another artist's album is not the user's copy of it.
#[tokio::test]
async fn a_popular_song_in_the_library_points_at_its_own_copy() {
    let fixtures = Fixtures::start().await;
    let (app, _auth) = app_seeded(
        &fixtures,
        format!("{}/mb/ws/2", fixtures.base),
        &[
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             artist_name, artist_name_folded, album_title, album_title_folded, disc_number, \
             track_number, duration_seconds, file_format, ingest_source, imported_at, \
             membership_source) VALUES ('track-surprises', 'album-okc', 'root-1', \
             '/music/surprises.flac', 'surprises.flac', 'hash-3', 1000, 0, 'stat-3', \
             'No Surprises', 'no surprises', 'Radiohead', 'radiohead', 'OK Computer', \
             'ok computer', 1, 10, 229.0, 'flac', 'scan', 0, 'manual')"
                .to_owned(),
            "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
             VALUES ('track-surprises', 0, 'artist-radiohead', 'primary')"
                .to_owned(),
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             artist_name, artist_name_folded, album_title, album_title_folded, disc_number, \
             track_number, duration_seconds, file_format, ingest_source, imported_at, \
             membership_source) VALUES ('track-host-creep', 'album-host', 'root-1', \
             '/music/host-creep.flac', 'host-creep.flac', 'hash-4', 1000, 0, 'stat-4', \
             'Creep', 'creep', 'Host Band', 'host band', 'Guest Spot', 'guest spot', 1, 2, \
             200.0, 'flac', 'scan', 0, 'manual')"
                .to_owned(),
            "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
             VALUES ('track-host-creep', 0, 'artist-radiohead', 'guest')"
                .to_owned(),
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) VALUES ('album-host', \
             'musicbrainz', 'f00d0000-0000-4000-8000-000000000001', 'manual', 0)"
                .to_owned(),
        ],
    )
    .await;

    let songs = get(&app, &format!("/api/v3/artists/{ARTIST}/top-songs")).await;
    assert_eq!(titles(&songs["songs"]), ["Creep", "No Surprises"]);
    assert_eq!(
        songs["songs"][0]["track_number"],
        Value::Null,
        "a cover on another artist's album is not the user's copy"
    );
    let held = &songs["songs"][1];
    assert_eq!(
        (
            &held["release_group_mbid"],
            &held["release_name"],
            &held["disc_number"],
            &held["track_number"]
        ),
        (
            &json!(OK_COMPUTER),
            &json!("OK Computer"),
            &json!(1),
            &json!(10)
        ),
        "the owned song points at its place on the user's album"
    );
}

#[tokio::test]
async fn owned_releases_list_every_held_group_whatever_its_type() {
    let fixtures = Fixtures::start().await;
    // The bootleg turns out to be the live album: a group the release-type
    // preferences keep out of the discography.
    let (app, _auth) = app_seeded(
        &fixtures,
        format!("{}/mb/ws/2", fixtures.base),
        &[format!(
            "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, decision_source, selected_at) \
             VALUES ('album-bootleg', 'musicbrainz', '{LIVE}', 'automatic', 0)"
        )],
    )
    .await;

    let releases = get(&app, &format!("/api/v3/artists/{ARTIST}/releases")).await;
    assert_eq!(titles(&releases["albums"]), ["Kid A", "OK Computer"]);

    let owned = get(&app, &format!("/api/v3/artists/{ARTIST}/owned-releases")).await;
    assert_eq!(
        owned["items"],
        json!([
            {
                "id": LIVE,
                "title": "I Might Be Wrong: Live Recordings",
                "primary_type": "Album",
                "secondary_types": ["Live"],
                "year": 2001
            },
            {
                "id": OK_COMPUTER,
                "title": "OK Computer",
                "primary_type": "Album",
                "secondary_types": [],
                "year": 1997
            }
        ]),
        "every held group, newest first, the filtered one included"
    );
    assert_eq!(owned["complete"], true);
    assert_eq!(
        fixtures.hits("/mb/ws/2/release-group"),
        1,
        "the discography browse serves both"
    );
}

/// An artist credited on releases but on no release group (a band whose
/// album MusicBrainz credits to its singer) lists the groups of its
/// releases, once each, named by their own credit, and the one the
/// library holds is owned.
#[tokio::test]
async fn an_artist_without_release_groups_lists_those_of_its_releases() {
    let fixtures = Fixtures::start().await;
    let (app, _auth) = app_seeded(
        &fixtures,
        format!("{}/mb/ws/2", fixtures.base),
        &[format!(
            "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, decision_source, selected_at) \
             VALUES ('album-bootleg', 'musicbrainz', '{BAND_ALBUM}', 'automatic', 0)"
        )],
    )
    .await;

    let releases = get(&app, &format!("/api/v3/artists/{BAND}/releases")).await;
    assert_eq!(
        titles(&releases["albums"]),
        ["In the Time of the Great Remembering"],
        "two releases, one group"
    );
    assert_eq!(releases["albums"][0]["id"], BAND_ALBUM);
    assert_eq!(releases["albums"][0]["in_library"], true);
    assert_eq!(releases["source_total_count"], 1);

    let owned = get(&app, &format!("/api/v3/artists/{BAND}/owned-releases")).await;
    assert_eq!(owned["items"][0]["id"], BAND_ALBUM, "{owned}");
    assert_eq!(owned["complete"], true);

    let more = get(
        &app,
        &format!("/api/v3/albums/{OK_COMPUTER}/more-by-artist?artist_id={BAND}&count=2"),
    )
    .await;
    assert_eq!(
        more["artist_name"], "Ben Caplan",
        "a group names the artist its own credit names, as a browsed group does"
    );
    assert_eq!(more["albums"][0]["artist_name"], "Ben Caplan", "{more}");
    assert_eq!(
        (
            fixtures.hits("/mb/ws/2/release-group"),
            fixtures.hits("/mb/ws/2/release")
        ),
        (1, 1),
        "one browse of each serves all three"
    );
}

#[tokio::test]
async fn artist_sections_use_listenbrainz_then_lastfm() {
    let fixtures = Fixtures::start().await;
    let (app, (rig, _scope)) = app(&fixtures).await;

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
        "co-listening first, with fame divided out"
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
    assert_eq!(
        (&songs["asked_source"], &songs["unavailable_sources"]),
        (&json!("listenbrainz"), &json!([])),
        "the page can tell Last.fm stood in for an empty ListenBrainz"
    );
    assert_eq!(similar["asked_source"], "listenbrainz");

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
            "artist_id": PORTISHEAD_ID,
            "in_library": false,
            "requested": false,
            "cover_url": format!("/api/v3/covers/release-group/{DUMMY_GROUP}?size=500")
        }]),
        "no MusicBrainz albums for the similar artists, so Last.fm's fill in"
    );
    assert_eq!(similar_albums["source"], "lastfm", "{similar_albums}");

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

    // Similar artists are ListenBrainz's for everyone, a person asking for
    // Last.fm included.
    let one_answer = get(
        &app,
        &format!("/api/v3/artists/{ARTIST}/similar?count=5&source=lastfm"),
    )
    .await;
    assert_eq!(
        (&one_answer["source"], &one_answer["asked_source"]),
        (&json!("listenbrainz"), &json!("listenbrainz")),
        "{one_answer}"
    );
    assert_eq!(one_answer["similar_artists"][0]["name"], "Lana Del Rey");

    // Asked for Last.fm with no Last.fm key to read with, ListenBrainz
    // stands in.
    rig.deps.lastfm.delete(USER).await.unwrap();
    let stood_in = get(
        &app,
        &format!("/api/v3/artists/{ARTIST}/top-albums?source=lastfm"),
    )
    .await;
    assert_eq!(
        (
            &stood_in["source"],
            &stood_in["asked_source"],
            &stood_in["unavailable_sources"]
        ),
        (&json!("listenbrainz"), &json!("lastfm"), &json!(["lastfm"])),
        "{stood_in}"
    );
    assert_eq!(titles(&stood_in["albums"]), ["OK Computer", "Kid A"]);
}

#[tokio::test]
async fn album_lastfm_skips_release_group_mbid_lookup() {
    let mut routes = routes();
    routes.push(Route {
        path: "/lastfm/2.0/".to_owned(),
        param: Some(("mbid", OK_COMPUTER)),
        status: 200,
        body: json!({"error": 6, "message": "Album not found"}).to_string(),
    });
    routes.push(Route {
        path: "/lastfm/2.0/".to_owned(),
        param: Some(("method", "album.getInfo")),
        status: 200,
        body: json!({
            "album": {
                "name": "OK Computer",
                "artist": "Radiohead",
                "url": "https://www.last.fm/music/Radiohead/OK+Computer",
                "listeners": "2000000",
                "playcount": "50000000",
                "tags": { "tag": [{ "name": "alternative rock", "url": "https://www.last.fm/tag/alternative+rock" }] },
                "wiki": { "summary": "Third studio album by Radiohead." }
            }
        })
        .to_string(),
    });
    let fixtures = Fixtures::start_with(routes).await;
    let (app, _rig) = app(&fixtures).await;

    let body = get(
        &app,
        &format!(
            "/api/v3/albums/{OK_COMPUTER}/lastfm?artist_name=Radiohead&album_name=OK%20Computer"
        ),
    )
    .await;
    assert_eq!(body["listeners"], 2_000_000);
    assert_eq!(body["playcount"], 50_000_000);
    assert_eq!(
        fixtures.hits("/lastfm/2.0/"),
        1,
        "album Last.fm lookup must go straight to artist + album name without a release-group mbid miss"
    );
}

#[tokio::test]
async fn listenbrainz_top_albums_shares_cache_across_counts() {
    let fixtures = Fixtures::start().await;
    let (app, _rig) = app(&fixtures).await;

    let first = get(
        &app,
        &format!("/api/v3/artists/{ARTIST}/top-albums?count=1"),
    )
    .await;
    let second = get(
        &app,
        &format!("/api/v3/artists/{ARTIST}/top-albums?count=5"),
    )
    .await;
    assert_eq!(titles(&first["albums"]), ["OK Computer"]);
    assert_eq!(titles(&second["albums"]), ["OK Computer", "Kid A"]);
    assert_eq!(
        fixtures.hits(&format!(
            "/lb/1/popularity/top-release-groups-for-artist/{ARTIST}"
        )),
        1,
        "different count values must share one ListenBrainz top-release-groups cache entry"
    );
}

#[tokio::test]
async fn album_page_shows_the_owned_edition_and_where_to_buy() {
    let fixtures = Fixtures::start().await;
    let (app, _auth) = app(&fixtures).await;

    let album = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}/basic")).await;
    let tracks = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}/tracks")).await;
    assert_eq!(album["title"], "OK Computer");
    assert_eq!(album["artist_name"], "Radiohead");
    assert_eq!(album["artist_id"], ARTIST);
    assert_eq!(album["year"], 1997);
    assert_eq!(album["in_library"], true);
    assert_eq!(
        album["cover_url"],
        format!("/api/v3/covers/release-group/{OK_COMPUTER}?size=500")
    );
    assert_eq!(tracks["selected_release_mbid"], RELEASE_CD);
    assert_eq!(tracks["pick_basis"], "owned");
    assert_eq!(titles(&tracks["tracks"]), ["Airbag", "Paranoid Android"]);
    assert_eq!(tracks["tracks"][0]["length"], 284_000);
    assert_eq!(tracks["total_length"], 667_000);
    assert_eq!(tracks["label"], "Parlophone");
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

    let album = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}/basic")).await;
    assert_eq!(album["source"], "library");
    let tracks = get(&app, &format!("/api/v3/albums/{OK_COMPUTER}/tracks")).await;
    assert_eq!(titles(&tracks["tracks"]), ["Airbag"]);

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

/// A library album id is shaped like a MusicBrainz id. An old id of a
/// merged album, an alias and a live copy's id each open the group the
/// album holding them now is identified as, without asking MusicBrainz;
/// an id the library does not hold is still asked there, and so is a
/// release group a v2 album carried as an alias.
#[tokio::test]
async fn an_old_or_local_album_id_opens_its_album_without_asking_musicbrainz() {
    const RETIRED: &str = "5e6f7081-9cad-4ebf-8a21-3c4d5e6f7081";
    const ALIAS: &str = "6f708192-adbe-4fc0-9b32-4d5e6f708192";
    const LIVE_COPY: &str = "708192a3-becf-40d1-8c43-5e6f708192a3";
    let fixtures = Fixtures::start().await;
    let extra = [
        format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, year, \
             grouping_source, created_at, updated_at, retired_into_album_id) VALUES \
             ('{RETIRED}', 'root-1', 'g-old', 'OK Computer', 'ok computer', 'Radiohead', \
             'radiohead', 'artist-radiohead', 1997, 'manual', 2, 2, 'album-okc'), \
             ('{LIVE_COPY}', 'root-1', 'g-copy', 'OK Computer', 'ok computer', 'Radiohead', \
             'radiohead', 'artist-radiohead', 1997, 'manual', 3, 3, NULL)"
        ),
        format!(
            "INSERT INTO local_album_aliases (alias, local_album_id, kind, created_at) \
             VALUES ('{ALIAS}', 'album-okc', 'merged_album', 2), \
             ('{UNKNOWN_ALBUM}', 'album-okc', 'legacy_release_group', 2)"
        ),
        format!(
            "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, decision_source, selected_at) \
             VALUES ('{LIVE_COPY}', 'musicbrainz', '{OK_COMPUTER}', 'automatic', 3)"
        ),
    ];
    let (app, _auth) = app_seeded(&fixtures, format!("{}/mb/ws/2", fixtures.base), &extra).await;

    for id in [RETIRED, ALIAS, LIVE_COPY] {
        let album = get(&app, &format!("/api/v3/albums/{id}/basic")).await;
        assert_eq!(album["musicbrainz_id"], OK_COMPUTER, "{id}: {album}");
        assert_eq!(
            fixtures.hits(&format!("/mb/ws/2/release-group/{id}")),
            0,
            "{id} is never asked of MusicBrainz"
        );
    }
    let (status, _) = call(
        &app,
        Method::GET,
        &format!("/api/v3/albums/{UNKNOWN_ALBUM}/basic"),
        Some(TOKEN),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        fixtures.hits(&format!("/mb/ws/2/release-group/{UNKNOWN_ALBUM}")),
        1,
        "a release group the library does not hold, or holds only as a v2 alias, is asked once"
    );
}

/// With MusicBrainz down, the artist's discography from the library lists
/// each group the library owns once, however many copies hold it, and never
/// the group of a guess its files contradict.
#[tokio::test]
async fn the_library_discography_lists_each_owned_group_once() {
    let fixtures = Fixtures::start().await;
    let extra = [
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, year, \
         grouping_source, created_at, updated_at) VALUES \
         ('album-okc-mp3', 'root-1', 'g-okc-mp3', 'OK Computer', 'ok computer', 'Radiohead', \
         'radiohead', 'artist-radiohead', 1997, 'manual', 2, 2), \
         ('album-guess', 'root-1', 'g-guess', 'Kid A?', 'kid a?', 'Radiohead', \
         'radiohead', 'artist-radiohead', 2000, 'manual', 3, 3)"
            .to_owned(),
        format!(
            "INSERT INTO local_album_external_identities \
             (local_album_id, provider, release_group_mbid, decision_source, selected_at) VALUES \
             ('album-okc-mp3', 'musicbrainz', '{OK_COMPUTER}', 'automatic', 2), \
             ('album-guess', 'musicbrainz', '{KID_A}', 'automatic', 3)"
        ),
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
         candidates_json, updated_at) VALUES \
         ('album-guess', 'unconfirmed', 'NAMED_GROUP_DIFFERS', '[]', 3)"
            .to_owned(),
        format!(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             artist_name, artist_name_folded, album_title, album_title_folded, disc_number, \
             track_number, duration_seconds, file_format, ingest_source, imported_at, \
             membership_source, embedded_release_group_mbid) VALUES \
             ('track-okc-mp3', 'album-okc-mp3', 'root-1', '/music/airbag.mp3', 'airbag.mp3', \
             'hash-mp3', 1000, 0, 'stat-mp3', 'Airbag', 'airbag', 'Radiohead', 'radiohead', \
             'OK Computer', 'ok computer', 1, 1, 284.0, 'mp3', 'scan', 0, 'manual', NULL), \
             ('track-guess', 'album-guess', 'root-1', '/music/guess.flac', 'guess.flac', \
             'hash-guess', 1000, 0, 'stat-guess', 'Idioteque', 'idioteque', 'Radiohead', \
             'radiohead', 'Kid A?', 'kid a?', 1, 1, 309.0, 'flac', 'scan', 0, 'manual', \
             '{LIVE}')"
        ),
    ];
    let (app, _auth) = app_seeded(&fixtures, "http://127.0.0.1:9/ws/2".to_owned(), &extra).await;

    let releases = get(&app, &format!("/api/v3/artists/{ARTIST}/releases")).await;
    assert_eq!(releases["source"], "library");
    let ids: Vec<&str> = releases["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|album| album["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [OK_COMPUTER], "{releases}");
}

/// Search joins MusicBrainz with the library unless it is scoped to the
/// library. The library scope goes first: it answers from the library
/// alone, MusicBrainz is never asked, and only library rows come back.
#[tokio::test]
async fn search_joins_musicbrainz_unless_scoped_to_the_library() {
    let fixtures = Fixtures::start().await;
    let (app, _auth) = app(&fixtures).await;

    let body = get(&app, "/api/v3/search?q=ok%20computer&scope=library").await;
    assert_eq!(
        titles(&body["albums"]),
        ["OK Computer", "OK Computer Live Bootleg"],
        "library scope"
    );
    for row in body["albums"].as_array().unwrap() {
        assert!(row["id"].is_string(), "library rows only: {row}");
    }
    assert_eq!(fixtures.hits("/mb/ws/2/release-group"), 0, "library scope");
    assert_eq!(fixtures.hits("/mb/ws/2/artist"), 0, "library scope");

    let body = get(&app, "/api/v3/search?q=ok%20computer").await;
    assert_eq!(body["album_status"], "ok");
    assert_eq!(
        titles(&body["albums"]),
        [
            "OK Computer",
            "OK Computer Live Bootleg",
            "OK Computer OKNOTOK 1997 2017",
            "Kid A"
        ],
        "compilations shown, library-only rows kept, ranked by score"
    );
    assert_eq!(body["albums"][0]["id"], "album-okc");
    assert_eq!(body["albums"][0]["musicbrainz_id"], OK_COMPUTER);
    assert_eq!(body["albums"][0]["in_library"], true);
    assert_eq!(body["albums"][1]["musicbrainz_id"], Value::Null);
    assert!(body["albums"][3]["id"].is_null());
    assert_eq!(body["albums"][3]["requested"], true);
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
        [
            "OK Computer",
            "OK Computer Live Bootleg",
            "OK Computer OKNOTOK 1997 2017"
        ]
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

#[tokio::test]
async fn lastfm_reads_fall_back_to_the_instance_key() {
    let rig = AuthRig::new("reads-catalog").await;
    let upstream = Upstream::new(
        Arc::new(Providers::unpaced(Arc::new(
            droppedneedle_providers::test_support::ScriptedWire::new(),
        ))),
        Arc::new(TestSettings {
            mb_url: String::new(),
        }),
        rig.deps.clone(),
        crate::common::listening_store(&rig.platform),
    );
    assert!(
        upstream.lastfm("no-key-user").await.is_none(),
        "no user key and no instance key"
    );
    rig.set_lastfm_instance_key("instance-key");
    let (_, creds) = upstream.lastfm("no-key-user").await.expect("instance key");
    assert_eq!(creds.api_key, "instance-key");
}

/// A portrait lookup that failed is not "no image": with TheAudioDB
/// holding no thumbnail and Commons down, the artist's image is asked
/// again after the short outage wait, never held blank for hours.
#[tokio::test]
async fn a_failed_portrait_lookup_keeps_the_short_outage_marker() {
    use droppedneedle_catalog::artwork::{
        ArtworkService, OUTAGE_TTL_SECS,
        cache::{ArtworkCache, KeyEntry},
        local::LocalArtwork,
    };
    use droppedneedle_catalog::metadata::precache::CatalogArtistImages;

    let mut routes = routes();
    for route in &mut routes {
        if route.path == "/audiodb/123/artist-mb.php" {
            route.body = audiodb_fixture("artist_mb_not_found.json");
        }
        if route.path == "/commons/w/api.php" {
            route.status = 503;
        }
    }
    let fixtures = Fixtures::start_with(routes).await;
    let (app_db, dir) = crate::common::migrated_file("reads-catalog-portrait");
    let rig = AuthRig::new("reads-catalog-portrait").await;
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
    let providers = Arc::new(fixtures.providers(&http));
    let catalog = catalog_on(
        Upstream::new(
            providers.clone(),
            Arc::new(TestSettings {
                mb_url: format!("{}/mb/ws/2", fixtures.base),
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );
    let config = crate::common::config_store(&dir.join("settings"));
    let artwork = ArtworkService::new(
        ArtworkCache::new(
            dir.join("covers"),
            1024 * 1024,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
        LocalArtwork::new(droppedneedle_kernel::db::Db::open_for_tests(&app_db).unwrap()),
        Arc::new(providers.coverart.clone()),
        config.live(),
        Arc::new(providers.images.clone()),
        Arc::new(CatalogArtistImages(catalog)),
    );

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

/// A cold artist image waits its turn behind a busy MusicBrainz instead of
/// being turned down like a page: the image resolve asks at image
/// priority, so it finds the artist and its portrait rather than coming
/// back incomplete, which would hold the image blank for the outage wait.
/// TheAudioDB, with nothing cached, is left to the background fill.
#[tokio::test]
async fn a_cold_artist_image_waits_out_a_busy_musicbrainz() {
    use droppedneedle_catalog::artwork::ArtistImageSource as _;
    use droppedneedle_catalog::metadata::precache::CatalogArtistImages;
    use droppedneedle_providers::test_support::ScriptedWire;
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
        audiodb_fixture("artist_mb_found.json"),
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
        wikidata_fixture("claims_p18_found.json"),
    );
    wire.answer(
        &format!("{}/w/api.php", wikidata::COMMONS_BASE),
        200,
        &[],
        wikidata_fixture("commons_imageinfo_credit.json"),
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
    let catalog = catalog_on(
        Upstream::new(
            providers.clone(),
            Arc::new(TestSettings {
                mb_url: musicbrainz::MB_API_BASE.to_owned(),
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
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
    assert_eq!(urls, [COMMONS_THUMB]);
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
    use droppedneedle_catalog::artwork::covers::{self, CoversState};
    use droppedneedle_catalog::artwork::{
        ArtworkService, cache::ArtworkCache, local::LocalArtwork,
    };
    use droppedneedle_catalog::metadata::precache::CatalogArtistImages;
    use droppedneedle_providers::test_support::ScriptedWire;
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
    let (app_db, dir) = crate::common::migrated_file("reads-catalog-credit");
    let rig = AuthRig::new("reads-catalog-credit").await;
    let catalog = catalog_on(
        Upstream::new(
            providers.clone(),
            Arc::new(TestSettings {
                mb_url: "http://127.0.0.1:9".to_owned(),
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );
    let config = crate::common::config_store(&dir.join("settings"));
    let artwork = ArtworkService::new(
        ArtworkCache::new(
            dir.join("covers"),
            1024 * 1024,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
        LocalArtwork::new(droppedneedle_kernel::db::Db::open_for_tests(&app_db).unwrap()),
        Arc::new(providers.coverart.clone()),
        config.live(),
        Arc::new(providers.images.clone()),
        Arc::new(CatalogArtistImages(catalog)),
    );
    let app = with_test_principal(
        covers::routes()
            .with_state(CoversState::new(Arc::new(artwork)))
            .into_router(),
        test_principal("u-1", Role::User, "s-1", SessionKind::Standard),
    );
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

/// A section built past a failed read is not cached as the answer: once
/// the service is back, the next read gets the whole one. Top songs lose
/// their album links while ListenBrainz fails, and similar albums lose an
/// artist while its MusicBrainz discography fails.
#[tokio::test]
async fn a_passing_failure_is_not_cached_as_a_short_section() {
    use droppedneedle_catalog::metadata::models::AlbumRef;
    use droppedneedle_kernel::runtime_config::sections::ListenSource;
    use droppedneedle_providers::executor::Method;
    use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
    use droppedneedle_providers::{
        ProviderEndpoints, RequestPriority, TransportKind, listenbrainz,
    };

    const LANA: &str = "b7539c32-53e7-4908-bda3-81449c367da6";
    const PORTISHEAD: &str = "8f6bd1e4-fbe1-4f50-aa9b-94c450ec0f11";
    const CREEP: &str = "5e6f7a8b-1111-4c2d-8e3f-4a5b6c7d8e9f";
    const BORN_TO_DIE: &str = "5e6f7a8b-2222-4c2d-8e3f-4a5b6c7d8e9f";
    let lb = |path: &str| format!("{}/1/{path}", listenbrainz::DEFAULT_BASE_URL);
    let down = Scripted::Transport(TransportKind::Connect);
    let discography = |id: &str, name: &str, (artist, mbid): (&str, &str)| Scripted::Answer {
        status: 200,
        headers: Vec::new(),
        body: json!({"release-group-count": 1, "release-group-offset": 0,
            "release-groups": [{"id": id, "title": name, "primary-type": "Album",
                "secondary-types": [], "artist-credit": [{"name": artist,
                    "joinphrase": "", "artist": {"id": mbid, "name": artist}}]}]})
        .to_string()
        .into_bytes(),
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
        &format!("{}/similar-artists/json", listenbrainz::LABS_BASE_URL),
        200,
        &[],
        upstream_fixture("labs_similar_artists"),
    );
    wire.on(
        Method::Post,
        &lb("popularity/artist"),
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: upstream_fixture("lb_artist_popularity").into_bytes(),
        },
    );
    // One browse address serves both artists' discographies, in the order
    // they are read: Lana Del Rey's fails both attempts, Portishead's
    // answers, and the next read of Lana Del Rey's answers.
    let browse = "http://127.0.0.1:9/ws/2/release-group";
    let portishead = discography(DUMMY_GROUP, "Dummy", ("Portishead", PORTISHEAD));
    for answer in [
        down.clone(),
        down,
        portishead.clone(),
        discography(BORN_TO_DIE, "Born to Die", ("Lana Del Rey", LANA)),
        portishead,
    ] {
        wire.on(Method::Get, browse, answer);
    }
    wire.on(
        Method::Post,
        &lb("popularity/release-group"),
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: b"[]".to_vec(),
        },
    );
    let providers = Arc::new(Providers::over_wire(
        wire,
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            listenbrainz_labs: listenbrainz::LABS_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    // Puts are queued: a waited delete commits after every put before it,
    // so the second reads see whatever the first ones stored.
    let cache = providers.cache.clone();
    let dir = crate::common::ScratchDir::new("reads-catalog-passing");
    crate::common::migrated_db(&dir.join("app.db"));
    let rig = AuthRig::new("reads-catalog-passing").await;
    rig.seed_user("ada", Role::User).await;
    link_listenbrainz(&rig).await;
    let catalog = catalog_on(
        Upstream::new(
            providers,
            Arc::new(TestSettings {
                mb_url: "http://127.0.0.1:9/ws/2".to_owned(),
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
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
        "both artists once MusicBrainz answers"
    );
}

/// [`TestSettings`] with ListenBrainz reads switched off.
struct ListenBrainzOff(TestSettings);

impl CatalogSettings for ListenBrainzOff {
    fn musicbrainz(&self) -> Option<MusicBrainzSettings> {
        self.0.musicbrainz()
    }

    fn preferences(&self) -> UserPreferences {
        self.0.preferences()
    }

    fn advanced(&self) -> AdvancedSettings {
        self.0.advanced()
    }

    fn listenbrainz_enabled(&self) -> bool {
        false
    }

    fn store_region(&self) -> Option<String> {
        self.0.store_region()
    }
}

/// "You Might Also Like" takes its similar artists from the one answer,
/// a Last.fm-first person's too, and their albums from the service the
/// person prefers first, the other one when that is switched off; with
/// neither usable it says it is not set up.
#[tokio::test]
async fn similar_albums_follow_the_persons_service() {
    use droppedneedle_kernel::runtime_config::sections::ListenSource;
    use droppedneedle_media::playback::listening::PrefsPatch;

    let fixtures = Fixtures::start().await;
    let (_app_db, dir) = crate::common::migrated_file("similar-albums-sources");
    let rig = AuthRig::new("similar-albums-sources").await;
    rig.seed_user("ada", Role::User).await;
    rig.seed_user("carl", Role::User).await;
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
    let listening = crate::common::listening_store(&rig.platform);
    let prefer = |source| PrefsPatch {
        primary_music_source: Some(source),
        ..PrefsPatch::default()
    };
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
    let mb_url = format!("{}/mb/ws/2", fixtures.base);
    let catalog = |settings: Arc<dyn CatalogSettings>| {
        catalog_on(
            Upstream::new(
                Arc::new(fixtures.providers(&http)),
                settings,
                rig.deps.clone(),
                listening.clone(),
            ),
            &dir,
        )
    };
    let on = catalog(Arc::new(TestSettings {
        mb_url: mb_url.clone(),
    }));
    let off = catalog(Arc::new(ListenBrainzOff(TestSettings { mb_url })));
    let ids = |albums: &[droppedneedle_catalog::metadata::models::AlbumRef]| {
        albums
            .iter()
            .filter_map(|album| album.musicbrainz_id.clone())
            .collect::<Vec<_>>()
    };
    let labs = "/labs/similar-artists/json";

    listening
        .update_prefs(USER, &prefer(ListenSource::Lastfm))
        .await
        .unwrap();
    let lastfm_first = on
        .similar_albums(USER, OK_COMPUTER, ARTIST, 2)
        .await
        .unwrap();
    assert_eq!(
        (lastfm_first.source, lastfm_first.configured),
        (ListenSource::Lastfm, true)
    );
    assert_eq!(ids(&lastfm_first.albums), [DUMMY_GROUP]);
    assert_eq!(
        fixtures.hits(labs),
        1,
        "the similar artists are ListenBrainz's one answer"
    );

    listening
        .update_prefs(USER, &prefer(ListenSource::Listenbrainz))
        .await
        .unwrap();
    let stood_in = off
        .similar_albums(USER, OK_COMPUTER, ARTIST, 2)
        .await
        .unwrap();
    assert_eq!(
        (stood_in.source, stood_in.configured),
        (ListenSource::Lastfm, true),
        "ListenBrainz is off, so Last.fm answers"
    );
    assert_eq!(ids(&stood_in.albums), [DUMMY_GROUP]);

    let neither = off
        .similar_albums("user-carl", OK_COMPUTER, ARTIST, 2)
        .await
        .unwrap();
    assert_eq!(
        (neither.source, neither.configured, neither.albums.len()),
        (ListenSource::Listenbrainz, false, 0),
        "ListenBrainz off and no Last.fm key: nothing can be asked"
    );
    assert_eq!(
        fixtures.hits(labs),
        1,
        "ListenBrainz is not asked while it is off"
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
    // The release group's own entry, not just any: the page stores others
    // too, and the cache file is written behind the answer.
    let cache_file = app
        .config
        .cache_dir
        .join(droppedneedle_kernel::db::cache::FILE_NAME);
    app.until("the release group is in the cache file", || async {
        let cache = rusqlite::Connection::open(&cache_file).ok()?;
        cache
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM provider_cache_entries \
                 WHERE kind = 'found' AND key LIKE '%:rg:' || ?1)",
                [OK_COMPUTER],
                |row| row.get::<_, bool>(0),
            )
            .ok()?
            .then_some(())
    })
    .await;

    // Background work may have asked for the group again before its entry
    // landed; from here on the network is gone.
    server.abort();
    let _ = server.await;
    let served = *hits.lock().unwrap();
    let app = app.restart().await;
    let (status, body) = app.call("GET", &basic, &auth, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], "OK Computer");
    assert_eq!(
        *hits.lock().unwrap(),
        served,
        "answered from the cache file"
    );
    app.stop().await;
}

/// The test settings with TheAudioDB's key saved as the test sets it.
struct KeyedSettings {
    inner: TestSettings,
    audiodb_key: Mutex<String>,
}

impl CatalogSettings for KeyedSettings {
    fn musicbrainz(&self) -> Option<MusicBrainzSettings> {
        self.inner.musicbrainz()
    }

    fn preferences(&self) -> UserPreferences {
        self.inner.preferences()
    }

    fn advanced(&self) -> AdvancedSettings {
        AdvancedSettings {
            audiodb_api_key: self.audiodb_key.lock().unwrap().as_str().into(),
            ..self.inner.advanced()
        }
    }

    fn listenbrainz_enabled(&self) -> bool {
        self.inner.listenbrainz_enabled()
    }

    fn store_region(&self) -> Option<String> {
        self.inner.store_region()
    }
}

/// A popular song MusicBrainz cannot place is remembered for a day: the
/// artist page does not search for it again before then, and does after.
#[tokio::test]
async fn a_song_nothing_places_is_asked_again_after_a_day() {
    use droppedneedle_kernel::runtime_config::sections::ListenSource;
    use droppedneedle_kernel::test_support::ManualClock;
    use droppedneedle_providers::test_support::ScriptedWire;
    use droppedneedle_providers::{ProviderEndpoints, Source, listenbrainz};
    use std::time::Duration;

    const HOUR: i64 = 3_600_000;
    let mb = "http://127.0.0.1:9/ws/2";
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!(
            "{}/1/popularity/top-recordings-for-artist/{ARTIST}",
            listenbrainz::DEFAULT_BASE_URL
        ),
        200,
        &[],
        json!([{"recording_name": "Creep", "artist_name": "Radiohead",
                "total_listen_count": 90}])
        .to_string(),
    );
    wire.answer(
        &format!("{mb}/recording"),
        200,
        &[],
        json!({"count": 0, "offset": 0, "recordings": []}).to_string(),
    );
    let clock = Arc::new(ManualClock::at(1_790_000_000_000));
    let providers = Arc::new(Providers::over_wire(
        wire.clone(),
        clock.clone(),
        ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let cache = providers.cache.clone();
    let dir = crate::common::ScratchDir::new("reads-catalog-song-miss");
    crate::common::migrated_db(&dir.join("app.db"));
    let rig = AuthRig::new("reads-catalog-song-miss").await;
    rig.seed_user("ada", Role::User).await;
    link_listenbrainz(&rig).await;
    let catalog = catalog_on(
        Upstream::new(
            providers,
            Arc::new(TestSettings {
                mb_url: mb.to_owned(),
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );
    let searches = || {
        wire.sent()
            .iter()
            .filter(|sent| sent.url.path().ends_with("/ws/2/recording"))
            .count()
    };
    let page = || catalog.artist_page_top_songs(USER, ARTIST, 5, Some(ListenSource::Listenbrainz));
    // Waits until `want` searches went out and an answer is stored.
    let searched = |want: usize| {
        let cache = cache.clone();
        async move {
            for _ in 0..1000 {
                cache.forget(&[]).await;
                let stored = cache
                    .stats()
                    .await
                    .iter()
                    .any(|stats| stats.source == Source::MusicBrainz && stats.rows > 0);
                if searches() >= want && stored {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("{want} searches never landed");
        }
    };

    let first = page().await.unwrap();
    assert_eq!(first.songs[0].release_group_mbid, None);
    searched(1).await;

    clock.advance(23 * HOUR);
    page().await.unwrap();
    // A settle for an absence: a search the page started would be out by now.
    cache.forget(&[]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(searches(), 1, "a stored miss holds for a day");

    clock.advance(2 * HOUR);
    page().await.unwrap();
    searched(2).await;
    assert_eq!(searches(), 2, "a miss older than a day is asked again");
}

#[tokio::test]
async fn fill_artist_images_skips_mb_when_audiodb_off_or_name_fallback_off() {
    use droppedneedle_providers::test_support::ScriptedWire;
    use droppedneedle_providers::{ProviderEndpoints, audiodb};

    struct AudioDbSettings {
        inner: TestSettings,
        enabled: bool,
        name_fallback: bool,
    }
    impl CatalogSettings for AudioDbSettings {
        fn musicbrainz(&self) -> Option<MusicBrainzSettings> {
            self.inner.musicbrainz()
        }
        fn preferences(&self) -> UserPreferences {
            self.inner.preferences()
        }
        fn advanced(&self) -> AdvancedSettings {
            AdvancedSettings {
                audiodb_enabled: self.enabled,
                audiodb_name_search_fallback: self.name_fallback,
                ..self.inner.advanced()
            }
        }
        fn listenbrainz_enabled(&self) -> bool {
            self.inner.listenbrainz_enabled()
        }
        fn store_region(&self) -> Option<String> {
            self.inner.store_region()
        }
    }

    let mb = "http://127.0.0.1:9/ws/2";
    let wire = Arc::new(ScriptedWire::new());
    let audiodb_url = format!("{}/123/artist-mb.php", audiodb::DEFAULT_BASE_URL);
    wire.answer(
        &audiodb_url,
        200,
        &[],
        audiodb_fixture("artist_mb_found.json"),
    );
    let providers = Arc::new(Providers::over_wire(
        wire.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        ProviderEndpoints {
            audiodb: audiodb::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let dir = crate::common::ScratchDir::new("reads-catalog-audiodb-fill");
    crate::common::migrated_db(&dir.join("app.db"));
    let rig = AuthRig::new("reads-catalog-audiodb-fill").await;

    let off = catalog_on(
        Upstream::new(
            providers.clone(),
            Arc::new(AudioDbSettings {
                inner: TestSettings {
                    mb_url: mb.to_owned(),
                },
                enabled: false,
                name_fallback: false,
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );
    assert!(!off.fill_artist_images(ARTIST).await);
    assert!(
        wire.sent().is_empty(),
        "audiodb_enabled = false must send 0 MB and 0 AudioDB calls"
    );

    let on_mbid_only = catalog_on(
        Upstream::new(
            providers,
            Arc::new(AudioDbSettings {
                inner: TestSettings {
                    mb_url: mb.to_owned(),
                },
                enabled: true,
                name_fallback: false,
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );
    assert!(on_mbid_only.fill_artist_images(ARTIST).await);
    let sent = wire.sent();
    assert_eq!(sent.len(), 1, "only AudioDB artist-mb lookup should fire");
    assert!(
        sent[0].url.path().ends_with("/artist-mb.php"),
        "expected AudioDB artist-mb.php, got {}",
        sent[0].url
    );
}

#[tokio::test]
async fn owned_top_songs_skip_mb_and_keep_owned_track_numbers() {
    use droppedneedle_kernel::db::Lane;
    use droppedneedle_kernel::runtime_config::sections::ListenSource;
    use droppedneedle_providers::test_support::ScriptedWire;
    use droppedneedle_providers::{ProviderEndpoints, listenbrainz};

    const REC_OWNED: &str = "11111111-2222-4333-8444-555555555555";
    const REC_UNOWNED: &str = "22222222-3333-4444-8555-666666666666";
    const SHARED_REL: &str = "33333333-4444-4555-8666-777777777777";
    const LOCAL_RG: &str = "44444444-5555-4666-8777-888888888888";

    let mb = "http://127.0.0.1:9/ws/2";
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!(
            "{}/1/popularity/top-recordings-for-artist/{ARTIST}",
            listenbrainz::DEFAULT_BASE_URL
        ),
        200,
        &[],
        json!([
            {
                "recording_mbid": REC_OWNED,
                "recording_name": "Owned Song",
                "artist_name": "Radiohead",
                "release_mbid": SHARED_REL,
                "total_listen_count": 500
            },
            {
                "recording_mbid": REC_UNOWNED,
                "recording_name": "Unowned Song",
                "artist_name": "Radiohead",
                "release_mbid": SHARED_REL,
                "total_listen_count": 400
            }
        ])
        .to_string(),
    );
    // Shared release returns disc 1 track 9 for REC_OWNED and disc 1 track 3 for REC_UNOWNED.
    wire.answer(
        &format!("{mb}/release/{SHARED_REL}"),
        200,
        &[],
        json!({
            "id": SHARED_REL,
            "title": "Shared Release",
            "media": [{
                "position": 1,
                "tracks": [
                    {"id": "rt-1", "position": 9, "title": "Owned Song", "recording": {"id": REC_OWNED, "title": "Owned Song"}},
                    {"id": "rt-2", "position": 3, "title": "Unowned Song", "recording": {"id": REC_UNOWNED, "title": "Unowned Song"}}
                ]
            }]
        })
        .to_string(),
    );
    // Unowned song recording lookup returns an official studio album (no search needed!).
    wire.answer(
        &format!("{mb}/recording/{REC_UNOWNED}"),
        200,
        &[],
        json!({
            "id": REC_UNOWNED,
            "title": "Unowned Song",
            "releases": [{
                "id": SHARED_REL,
                "title": "Shared Release",
                "status": "Official",
                "date": "2000-01-01",
                "release-group": {"id": OK_COMPUTER, "primary-type": "Album", "secondary-types": []}
            }]
        })
        .to_string(),
    );

    let providers = Arc::new(Providers::over_wire(
        wire.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let dir = crate::common::ScratchDir::new("reads-catalog-owned-top-songs");
    let db_path = dir.join("app.db");
    crate::common::migrated_db(&db_path);
    let db = droppedneedle_kernel::db::Db::open_for_tests(&db_path).unwrap();
    db.write(Lane::Foreground, "seed owned track", move |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
             VALUES ('a1', 'Radiohead', 'radiohead', 'group', 1, 1); \
             INSERT INTO local_artist_external_identities (local_artist_id, provider, provider_artist_id, decision_source, selected_at) \
             VALUES ('a1', 'musicbrainz', '{ARTIST}', 'manual', 1); \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, created_at, updated_at, has_indexed_track) \
             VALUES ('al1', 'r1', 'g1', 'My Edition', 'my edition', 'Radiohead', 'radiohead', 'a1', 'automatic', 1, 1, 1); \
             INSERT INTO local_album_external_identities (local_album_id, provider, release_group_mbid, decision_source, selected_at) \
             VALUES ('al1', 'musicbrainz', '{LOCAL_RG}', 'manual', 1); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, album_title, album_title_folded, disc_number, track_number, file_format, availability, ingest_source, imported_at, membership_source) \
             VALUES ('t1', 'al1', 'r1', '/music/owned.flac', 'owned.flac', 'h1', 1, 1, 's1', 'Owned Song', 'owned song', 'My Edition', 'my edition', 2, 5, 'flac', 'indexed', 'scan', 1, 'automatic'); \
             INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role, credited_name) \
             VALUES ('t1', 0, 'a1', 'main', 'Radiohead'); \
             INSERT INTO local_track_external_identities (local_track_id, provider, recording_mbid, decision_source, selected_at) \
             VALUES ('t1', 'musicbrainz', '{REC_OWNED}', 'manual', 1);"
        ))?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .unwrap();

    let rig = AuthRig::new("reads-catalog-owned-top-songs").await;
    rig.seed_user("ada", Role::User).await;
    link_listenbrainz(&rig).await;
    let catalog = catalog_on(
        Upstream::new(
            providers.clone(),
            Arc::new(TestSettings {
                mb_url: mb.to_owned(),
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );

    // First call triggers background warming for the unowned song only.
    let first = catalog
        .artist_page_top_songs(USER, ARTIST, 5, Some(ListenSource::Listenbrainz))
        .await
        .unwrap();
    assert_eq!(first.songs[0].disc_number, Some(2));
    assert_eq!(first.songs[0].track_number, Some(5));
    assert_eq!(first.songs[0].release_group_mbid.as_deref(), Some(LOCAL_RG));

    // Wait for background release & recording warming to commit to cache.
    for _ in 0..200 {
        providers.cache.forget(&[]).await;
        let mb_rows = providers
            .cache
            .stats()
            .await
            .iter()
            .find(|stats| stats.source == droppedneedle_providers::Source::MusicBrainz)
            .map_or(0, |stats| stats.rows);
        let sent = wire.sent();
        if mb_rows >= 2
            && sent.iter().any(|s| s.url.path().ends_with(SHARED_REL))
            && sent.iter().any(|s| s.url.path().ends_with(REC_UNOWNED))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Second call reads the now-cached shared release: owned song must keep disc 2 track 5!
    let second = catalog
        .artist_page_top_songs(USER, ARTIST, 5, Some(ListenSource::Listenbrainz))
        .await
        .unwrap();
    assert_eq!(
        second.songs[0].disc_number,
        Some(2),
        "owned song disc_number must not be clobbered by shared release"
    );
    assert_eq!(
        second.songs[0].track_number,
        Some(5),
        "owned song track_number must not be clobbered by shared release"
    );
    assert_eq!(second.songs[1].disc_number, Some(1));
    assert_eq!(second.songs[1].track_number, Some(3));
    assert_eq!(
        second.songs[1].release_group_mbid.as_deref(),
        Some(OK_COMPUTER)
    );

    let sent = wire.sent();
    assert!(
        !sent.iter().any(|s| s.url.path().ends_with(REC_OWNED)),
        "owned song must not trigger lookup_recording"
    );
    assert!(
        !sent
            .iter()
            .any(|s| s.url.path().ends_with("/ws/2/recording")),
        "unowned song with official album release must skip search_recordings_query: {sent:?}"
    );
}

#[tokio::test]
async fn warm_song_group_falls_back_to_search_for_compilation_only_recording() {
    use droppedneedle_kernel::runtime_config::sections::ListenSource;
    use droppedneedle_providers::test_support::ScriptedWire;
    use droppedneedle_providers::{ProviderEndpoints, listenbrainz};

    const REC_COMP: &str = "55555555-6666-4777-8888-999999999999";
    const COMP_RG: &str = "66666666-7777-4888-8999-aaaaaaaaaaaa";
    const COMP_REL: &str = "77777777-8888-4999-8aaa-bbbbbbbbbbbb";

    let mb = "http://127.0.0.1:9/ws/2";
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!(
            "{}/1/popularity/top-recordings-for-artist/{ARTIST}",
            listenbrainz::DEFAULT_BASE_URL
        ),
        200,
        &[],
        json!([{
            "recording_mbid": REC_COMP,
            "recording_name": "Karma Police",
            "artist_name": "Radiohead",
            "total_listen_count": 300
        }])
        .to_string(),
    );
    wire.answer(
        &format!("{mb}/recording/{REC_COMP}"),
        200,
        &[],
        json!({
            "id": REC_COMP,
            "title": "Karma Police",
            "releases": [{
                "id": COMP_REL,
                "title": "Greatest Hits",
                "status": "Official",
                "date": "2008-01-01",
                "release-group": {"id": COMP_RG, "primary-type": "Album", "secondary-types": ["Compilation"]}
            }]
        })
        .to_string(),
    );
    wire.answer(
        &format!("{mb}/recording"),
        200,
        &[],
        json!({
            "count": 1,
            "offset": 0,
            "recordings": [{
                "id": REC_COMP,
                "title": "Karma Police",
                "releases": [{
                    "id": RELEASE_CD,
                    "title": "OK Computer",
                    "status": "Official",
                    "date": "1997-06-16",
                    "release-group": {"id": OK_COMPUTER, "primary-type": "Album", "secondary-types": []}
                }]
            }]
        })
        .to_string(),
    );
    let providers = Arc::new(Providers::over_wire(
        wire.clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let dir = crate::common::ScratchDir::new("reads-catalog-comp-fallback");
    crate::common::migrated_db(&dir.join("app.db"));
    let rig = AuthRig::new("reads-catalog-comp-fallback").await;
    rig.seed_user("ada", Role::User).await;
    link_listenbrainz(&rig).await;
    let catalog = catalog_on(
        Upstream::new(
            providers,
            Arc::new(TestSettings {
                mb_url: mb.to_owned(),
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );

    catalog
        .artist_page_top_songs(USER, ARTIST, 5, Some(ListenSource::Listenbrainz))
        .await
        .unwrap();
    for _ in 0..200 {
        if wire
            .sent()
            .iter()
            .any(|s| s.url.path().ends_with("/ws/2/recording"))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let sent = wire.sent();
    assert!(
        sent.iter().any(|s| s.url.path().ends_with(REC_COMP)),
        "lookup_recording must run first"
    );
    assert!(
        sent.iter()
            .any(|s| s.url.path().ends_with("/ws/2/recording")),
        "compilation-only lookup_recording must fall back to search_recordings_query"
    );
}

#[tokio::test]
async fn precache_library_since_filters_by_watermark() {
    use droppedneedle_kernel::db::Lane;

    let dir = crate::common::ScratchDir::new("reads-catalog-precache-since");
    let db_path = dir.join("app.db");
    crate::common::migrated_db(&db_path);
    let db = droppedneedle_kernel::db::Db::open_for_tests(&db_path).unwrap();
    db.write(Lane::Foreground, "seed old and new albums", move |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) VALUES \
               ('a-old', 'Old Band', 'old band', 'group', 1000, 1000), \
               ('a-new', 'New Band', 'new band', 'group', 5000, 5000); \
             INSERT INTO local_artist_external_identities (local_artist_id, provider, provider_artist_id, decision_source, selected_at) VALUES \
               ('a-old', 'musicbrainz', '{ARTIST}', 'manual', 1000), \
               ('a-new', 'musicbrainz', '{PORTISHEAD_ID}', 'manual', 5000); \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, created_at, updated_at, has_indexed_track) VALUES \
               ('al-old', 'r1', 'g-old', 'Old Album', 'old album', 'Old Band', 'old band', 'a-old', 'automatic', 1000, 1000, 1), \
               ('al-new', 'r1', 'g-new', 'New Album', 'new album', 'New Band', 'new band', 'a-new', 'automatic', 5000, 5000, 1); \
             INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role, credited_name) VALUES \
               ('al-old', 0, 'a-old', 'main', 'Old Band'), \
               ('al-new', 0, 'a-new', 'main', 'New Band'); \
             INSERT INTO local_album_external_identities (local_album_id, provider, release_group_mbid, decision_source, selected_at) VALUES \
               ('al-old', 'musicbrainz', '{OK_COMPUTER}', 'manual', 1000), \
               ('al-new', 'musicbrainz', '{KID_A}', 'manual', 5000);"
        ))?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .unwrap();

    let rig = AuthRig::new("reads-catalog-precache-since").await;
    let catalog = catalog_on(
        Upstream::new(
            Arc::new(Providers::closed()),
            Arc::new(TestSettings {
                mb_url: "http://127.0.0.1:9/ws/2".to_owned(),
            }),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        &dir,
    );

    let full = catalog.precache_library().await.unwrap();
    assert_eq!(full.artists.len(), 2);
    assert_eq!(full.albums.len(), 2);

    let incremental = catalog.precache_library_since(3000).await.unwrap();
    assert_eq!(incremental.artists.len(), 1);
    assert_eq!(incremental.artists[0].mbid, PORTISHEAD_ID);
    assert_eq!(incremental.albums.len(), 1);
    assert_eq!(incremental.albums[0].release_group_mbid, KID_A);
}

/// Now, by the system clock the cache under test runs on.
fn unix_now() -> u64 {
    use droppedneedle_kernel::clock::Clock as _;
    u64::try_from(droppedneedle_kernel::clock::SystemClock.now_unix()).unwrap_or(0)
}
