//! The discover page end to end over a real database: the first visit
//! starts a background build and answers `loading`, the built page carries
//! its shelves and is saved, a restart serves the saved page at once,
//! activity is recorded for the warm cycle, and refresh is accepted. A
//! user with Last.fm and ListenBrainz both linked gets shelves grown from
//! both, unless the server turns the blend off. Unreadable settings hold
//! every build, a page built past a failed read is rebuilt soon and keeps
//! the sections it had, each section not plainly built says why, and a
//! background warm that keeps nothing waits before it is tried again. Top
//! Picks never hold an album the library has, nor a chart album.

use crate::common::Probe;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::response::IntoResponse;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, Uri},
};
use droppedneedle_catalog::discover::cache::{PageKind, Timing, UserPageCache, Variant, clear_all};
use droppedneedle_catalog::discover::listening::shared::{self, Lender};
use droppedneedle_catalog::discover::listening::{
    AlbumRow, ArtistRow, Ask, LastFmAlbum, ListenedAlbum, ListeningData, PlaylistTrack,
    RankedAlbum, ScoredArtist, SourceContext, UserMusic, WeeklyPlaylist,
};
use droppedneedle_catalog::discover::ports::BoxFuture;
use droppedneedle_catalog::discover::scheduler::activity::ActivityDb;
use droppedneedle_catalog::discover::scheduler::{ArtistDetail, Feature, Warm, WarmScheduler};
use droppedneedle_catalog::discover::wiring::{deps_over, live_network};
use droppedneedle_catalog::metadata::Catalog;
use droppedneedle_catalog::metadata::library::LocalCatalog;
use droppedneedle_catalog::metadata::upstream::{LiveCatalogSettings, Upstream};
use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::db::cache::CacheDb;
use droppedneedle_kernel::http_client::HttpClientFactory;
use droppedneedle_kernel::runtime_config::secret_sections::{AdvancedSettings, LastFmSettings};
use droppedneedle_kernel::runtime_config::sections::ListenSource;
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, LegacyConfig};
use droppedneedle_media::playback::listening::PrefsPatch;
use droppedneedle_providers::Providers;
use droppedneedle_providers::executor::Method;
use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
use droppedneedle_providers::{ProviderEndpoints, RequestPriority, ReqwestWire, listenbrainz};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::AuthRig;
use crate::common::platform::TEST_KEY;
use crate::discover_queue::{
    Script, after_commits, discover_deps, discover_inputs, discover_routes, insert, library_artist,
    own, plays,
};

const SEED: &str = "11111111-1111-4111-8111-111111111111";

fn mbid(prefix: char, n: u8) -> String {
    let p = prefix.to_string().repeat(4);
    format!("{n:08x}-{p}-4{}-8{}-{}", &p[..3], &p[..3], p.repeat(3))
}

fn ranked(n: u8, artist: &str, listens: i64) -> RankedAlbum {
    RankedAlbum {
        album: AlbumRow {
            release_group_mbid: mbid('a', n),
            title: format!("Album {n}"),
            artist_name: format!("Artist {artist}"),
            artist_mbid: Some(artist.to_owned()),
            facts: Default::default(),
        },
        listen_count: listens,
    }
}

fn listened(album: RankedAlbum) -> ListenedAlbum {
    ListenedAlbum {
        release_group_mbid: Some(album.album.release_group_mbid),
        title: album.album.title,
        artist_name: album.album.artist_name,
        artist_mbid: album.album.artist_mbid,
        listen_count: album.listen_count,
    }
}

/// A ListenBrainz user with one seed artist, three similar artists with
/// two albums each, a worldwide chart and one fresh release.
pub(crate) fn page_script() -> Script {
    let similar: Vec<ScoredArtist> = (1..=3)
        .map(|n| ScoredArtist {
            artist: ArtistRow {
                name: format!("Similar {n}"),
                mbid: Some(mbid('b', n)),
                listen_count: Some(i64::from(n) * 100),
            },
            score: f64::from(n) * 100.0,
        })
        .collect();
    let artist_albums = similar
        .iter()
        .filter_map(|scored| scored.artist.mbid.clone())
        .map(|artist| {
            let n = artist.as_bytes()[7];
            let albums = vec![ranked(n, &artist, 500), ranked(n + 20, &artist, 300)];
            (artist, albums)
        })
        .collect();
    Script {
        user: UserMusic {
            listenbrainz: Some("listener".to_owned()),
            ..UserMusic::default()
        },
        source: Some(SourceContext {
            mode: "official".to_owned(),
            id: "source-1".to_owned(),
            generation: 3,
        }),
        user_artists: vec![ArtistRow {
            name: "Seed".to_owned(),
            mbid: Some(SEED.to_owned()),
            listen_count: Some(50),
        }],
        fresh: vec![ranked(90, &mbid('f', 1), 0).album],
        sitewide_artists: vec![ArtistRow {
            name: "Everyone's Favourite".to_owned(),
            mbid: Some(mbid('c', 1)),
            listen_count: Some(9000),
        }],
        sitewide_albums: vec![listened(ranked(70, &mbid('d', 1), 10_000))],
        similar: HashMap::from([(SEED.to_owned(), similar)]),
        artist_albums,
        genres: vec![("shoegaze".to_owned(), 40)],
        ..Script::default()
    }
}

fn page_app(rig: &AuthRig, dir: &std::path::Path) -> Router {
    discover_routes(discover_deps(rig, dir, Providers::closed(), |network| {
        *network = Arc::new(page_script());
    }))
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).expect("request builds"))
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body reads");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// How section `key` of a page stands, without its zone, `null` when it is
/// plainly built.
fn state_of(page: &Value, key: &str) -> Value {
    let mut state = page["section_states"]
        .as_array()
        .and_then(|states| states.iter().find(|state| state["key"] == key))
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(state) = state.as_object_mut() {
        state.remove("zone");
    }
    state
}

/// The content of section `key` of a page.
fn content<'a>(page: &'a Value, key: &str) -> &'a Value {
    page["sections"]
        .as_array()
        .and_then(|sections| sections.iter().find(|section| section["key"] == key))
        .map(|section| &section["content"])
        .unwrap_or(&Value::Null)
}

#[tokio::test]
async fn page_builds_in_the_background_is_saved_and_records_activity() {
    let dir = crate::common::ScratchDir::new("discover-page");
    let rig = AuthRig::new("discover-page").await;
    let db = rig.platform.db.clone();
    db.write(Lane::Foreground, "test.seed", |tx| {
        tx.execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-1', 'Ada', 'user', 0)",
            [],
        )?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("user row");
    let app = page_app(&rig, &dir);

    let (status, first) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["refreshing"], true);
    assert_eq!(first["sections"], json!([]));

    let saved = || async {
        db.one::<i64>(
            "SELECT COUNT(*) FROM catalog_cached_pages \
             WHERE user_id = 'user-1' AND page = 'discover' AND listenbrainz = 1 AND lastfm = 0",
            [],
        )
    };
    after_commits(&db, "the built page was saved", || async {
        (saved().await == 1).then_some(())
    })
    .await;
    let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
    let row: Value = serde_json::from_str(&db.one::<String>(
        "SELECT CAST(payload AS TEXT) FROM catalog_cached_pages \
         WHERE user_id = 'user-1' AND page = 'discover'",
        [],
    ))
    .expect("the saved page decodes");
    assert_eq!(
        row["health"]["seeds_used"], 1,
        "the saved row carries its health"
    );
    assert_eq!(
        row["health"]["sections"].as_u64(),
        page["sections"].as_array().map(|s| s.len() as u64)
    );
    assert_eq!(
        content(&page, "because_you_listen_to")["entries"][0]["section"]["title"],
        "Because You Listen To Seed"
    );
    assert_eq!(
        content(&page, "globally_trending")["shelf"]["items"][0]["name"],
        "Everyone's Favourite"
    );
    assert_eq!(
        content(&page, "fresh_releases")["shelf"]["items"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let picks = content(&page, "top_picks")["picks"]["items"]
        .as_array()
        .expect("top picks")
        .clone();
    assert!(!picks.is_empty());
    assert_eq!(picks[0]["reasons"][0], "Because you listen to Seed");
    assert_eq!(
        content(&page, "genre_list")["shelf"]["items"][0]["name"],
        "shoegaze"
    );

    // A restart serves the saved page straight away, built and ready.
    let restarted = page_app(&rig, &dir);
    let (_, again) = call(&restarted, "GET", "/api/v3/discover", None).await;
    assert_eq!(again["refreshing"], false, "the build finished");
    assert_eq!(
        content(&again, "because_you_listen_to")["entries"][0]["seed_artist"],
        "Seed",
        "served from the snapshot"
    );

    let (status, cursor) = call(
        &restarted,
        "POST",
        "/api/v3/discover/activity",
        Some(json!({ "feature": "discover" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cursor["source_id"], "source-1");
    assert_eq!(cursor["generation"], 3);
    let recorded: i64 = db.one(
        "SELECT COUNT(*) FROM catalog_discover_activity \
         WHERE user_id = 'user-1' AND feature = 'discover'",
        [],
    );
    assert_eq!(recorded, 1);
    let (status, _) = call(
        &restarted,
        "POST",
        "/api/v3/discover/activity",
        Some(json!({ "feature": "artist", "section": "similar" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "artist activity needs an artist"
    );

    let (status, _) = call(&restarted, "POST", "/api/v3/discover/refresh", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

async fn seed_user(db: &droppedneedle_kernel::db::Db) {
    db.write(Lane::Foreground, "test.seed", |tx| {
        tx.execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-1', 'Ada', 'user', 0)",
            [],
        )?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("user row");
}

fn page_cache(rig: &AuthRig) -> UserPageCache<String> {
    UserPageCache::new(
        rig.platform.db.clone(),
        rig.platform.clock.clone(),
        PageKind::Discover,
        Timing {
            fresh_ms: 300_000,
            retry_ms: 300_000,
        },
    )
}

fn built_for() -> Variant {
    Variant {
        source: "source-1:g3".to_owned(),
        listenbrainz: true,
        lastfm: false,
    }
}

/// A weekly exploration playlist dated `date`, one track long.
pub(crate) fn weekly_playlist(date: &str) -> WeeklyPlaylist {
    WeeklyPlaylist {
        title: "Weekly Exploration".to_owned(),
        date: date.to_owned(),
        source_url: String::new(),
        tracks: vec![PlaylistTrack {
            title: "Song".to_owned(),
            creator: "Someone".to_owned(),
            album: "Album".to_owned(),
            recording_mbid: None,
            artist_mbid: None,
            caa_release_mbid: None,
            duration_ms: None,
        }],
    }
}

/// The page built for a ListenBrainz user whose weekly playlist is dated
/// `date`, with Last.fm switched on or off on the server. The rig's clock
/// reads 2024-01-01 00:00 UTC.
async fn weekly_page(date: &str, lastfm_on: bool) -> Value {
    let dir = crate::common::ScratchDir::new("discover-weekly");
    let rig = AuthRig::new("discover-weekly").await;
    seed_user(&rig.platform.db).await;
    rig.config
        .save_secret_blocking(LastFmSettings {
            enabled: lastfm_on,
            ..LastFmSettings::default()
        })
        .expect("Last.fm settings save");
    let script = Script {
        weekly: Some(weekly_playlist(date)),
        ..page_script()
    };
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |network| {
        *network = Arc::new(script);
    }));
    call(&app, "GET", "/api/v3/discover", None).await;
    settled_page(&rig, &app, "the weekly page was built").await
}

fn offers_lastfm(page: &Value) -> bool {
    page["service_prompts"]
        .as_array()
        .expect("service prompts")
        .iter()
        .any(|prompt| prompt["service"] == "lastfm")
}

/// The weekly exploration playlist shows only while it is this week's:
/// one made eight days before the build is left off. The Last.fm card is
/// offered only while the server has Last.fm switched on.
#[tokio::test]
async fn only_this_weeks_playlist_and_no_card_for_a_service_switched_off() {
    let stale = weekly_page("2023-12-24T00:00:00.054723+00:00", false).await;
    assert_eq!(content(&stale, "weekly_exploration"), &Value::Null);
    assert!(!offers_lastfm(&stale), "Last.fm is switched off");

    let fresh = weekly_page("2023-12-31T23:00:00.054723+00:00", true).await;
    assert_eq!(
        content(&fresh, "weekly_exploration")["weekly"]["tracks"][0]["title"],
        "Song"
    );
    assert!(offers_lastfm(&fresh), "switched on, and not readable yet");
}

/// The page cache keeps what it should, one user per case. A build whose
/// save hears a busy database is not a failed build: its page is served at
/// once and written on a later visit. Clearing the cache wins over a build
/// already running: what it built is not kept, and a failed build's wait is
/// dropped. A page saved in an older shape is dropped on its first load, so
/// it is rebuilt and never logged again.
#[tokio::test]
async fn the_page_cache_keeps_what_it_should() {
    let rig = AuthRig::new("discover-page-cache").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    db.write(Lane::Foreground, "test.seed", |tx| {
        tx.execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-2', 'Bea', 'user', 0), ('user-3', 'Cy', 'user', 0)",
            [],
        )?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("user rows");
    let cache = page_cache(&rig);

    // A busy save keeps the built page.
    let held = rusqlite::Connection::open(rig.runtime().path()).expect("side connection");
    held.execute_batch("BEGIN IMMEDIATE")
        .expect("write lock held");
    let claim = cache.claim("user-1").await.expect("first build");
    cache
        .run(claim, &built_for(), async { Some("page".to_owned()) })
        .await;
    let served = cache.load("user-1", &built_for()).await;
    assert_eq!(
        served.as_ref().map(|page| page.value.as_str()),
        Some("page"),
        "busy save"
    );
    assert!(!cache.due("user-1", served.as_ref()), "no rebuild, no wait");
    held.execute_batch("ROLLBACK").expect("lock released");
    drop(held);
    assert!(
        cache.load("user-1", &built_for()).await.is_some(),
        "busy save"
    );
    let rows: i64 = db.one("SELECT COUNT(*) FROM catalog_cached_pages", []);
    assert_eq!(rows, 1, "the next visit writes the page");

    // A clear wins over a running build.
    let claim = cache.claim("user-2").await.expect("first build");
    assert!(
        !cache
            .run(claim, &built_for(), async { None::<String> })
            .await,
        "a failed build"
    );
    assert!(!cache.due("user-2", None), "a failed build waits");
    let claim = {
        clear_all(&db).await.expect("cleared");
        assert!(
            cache.load("user-2", &built_for()).await.is_none(),
            "cleared"
        );
        assert!(cache.due("user-2", None), "the clear drops the wait");
        cache.claim("user-2").await.expect("a build")
    };
    clear_all(&db).await.expect("cleared again");
    cache
        .run(claim, &built_for(), async { Some("stale".to_owned()) })
        .await;
    assert!(
        cache.load("user-2", &built_for()).await.is_none(),
        "a build claimed before the clear keeps nothing"
    );

    // A page in an old shape is dropped. The clears above left no rows.
    let claim = cache.claim("user-3").await.expect("first build");
    cache
        .run(claim, &built_for(), async { Some("old shape".to_owned()) })
        .await;
    let new: UserPageCache<HashMap<String, i64>> = UserPageCache::new(
        db.clone(),
        rig.platform.clock.clone(),
        PageKind::Discover,
        Timing {
            fresh_ms: 300_000,
            retry_ms: 300_000,
        },
    );
    let loaded = new.load("user-3", &built_for()).await;
    assert!(loaded.is_none(), "an old shape reads as missing");
    assert!(new.due("user-3", loaded.as_ref()), "and is rebuilt");
    let rows: i64 = db.one("SELECT COUNT(*) FROM catalog_cached_pages", []);
    assert_eq!(rows, 0, "the old row is gone");
}

fn scored(prefix: char, n: u8, score: f64) -> ScoredArtist {
    ScoredArtist {
        artist: ArtistRow {
            name: format!("Artist {prefix}{n}"),
            mbid: Some(mbid(prefix, n)),
            listen_count: None,
        },
        score,
    }
}

fn seed(name: &str, n: u8) -> ArtistRow {
    ArtistRow {
        name: name.to_owned(),
        mbid: Some(mbid('e', n)),
        listen_count: Some(10),
    }
}

/// The Because You Listen To entries of a page built for a user who
/// listens on ListenBrainz (primary) and Last.fm, with the blend `on` or
/// off: (seed, badge, first similar artist) per entry.
async fn because_entries(on: bool) -> Vec<(String, String, String)> {
    let dir = crate::common::ScratchDir::new("discover-blend");
    let rig = AuthRig::new("discover-blend").await;
    seed_user(&rig.platform.db).await;
    rig.config
        .save_secret_blocking(AdvancedSettings {
            discover_blend_listening_services: on,
            ..AdvancedSettings::default()
        })
        .expect("advanced settings save");
    let (lb_only, both, lastfm_only) =
        (seed("LB Only", 1), seed("Both", 2), seed("Last.fm Only", 3));
    let script = Script {
        user: UserMusic {
            listenbrainz: Some("listener".to_owned()),
            lastfm_username: Some("scrobbler".to_owned()),
            lastfm: true,
            primary: ListenSource::Listenbrainz,
            ..UserMusic::default()
        },
        user_artists: vec![lb_only.clone(), both.clone()],
        lastfm_user_artists: vec![both.clone(), lastfm_only.clone()],
        similar: HashMap::from([
            (
                lb_only.mbid.clone().unwrap_or_default(),
                (1..=3).map(|n| scored('b', n, 90.0)).collect(),
            ),
            (
                both.mbid.clone().unwrap_or_default(),
                (4..=6).map(|n| scored('b', n, 90.0)).collect(),
            ),
            (
                lastfm_only.mbid.clone().unwrap_or_default(),
                (7..=9).map(|n| scored('b', n, 90.0)).collect(),
            ),
        ]),
        // Last.fm's list for "Both" shares ListenBrainz's last pick, named
        // without its id; it is not blended in.
        lastfm_similar: HashMap::from([(
            both.mbid.clone().unwrap_or_default(),
            vec![
                scored('c', 1, 0.9),
                scored('c', 2, 0.8),
                ScoredArtist {
                    artist: ArtistRow {
                        name: "Artist b6".to_owned(),
                        mbid: None,
                        listen_count: None,
                    },
                    score: 0.7,
                },
            ],
        )]),
        ..Script::default()
    };
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |network| {
        *network = Arc::new(script);
    }));
    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the blended page was built").await;
    content(&page, "because_you_listen_to")["entries"]
        .as_array()
        .expect("because entries")
        .iter()
        .map(|entry| {
            let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
            (
                text(&entry["seed_artist"]),
                text(&entry["section"]["source"]),
                text(&entry["section"]["items"][0]["name"]),
            )
        })
        .collect()
}

/// Blended, the seeds come from both histories with the artist both name
/// first, and the shelves carry the blended badge; each seed's similar
/// artists are ListenBrainz's one answer, with nothing of Last.fm's mixed
/// in. Turned off, only the primary service's seeds are read.
#[tokio::test]
async fn both_services_blend_unless_the_server_turns_it_off() {
    let entry = |seed: &str, source: &str, first: &str| {
        (seed.to_owned(), source.to_owned(), first.to_owned())
    };
    assert_eq!(
        because_entries(true).await,
        vec![
            entry("Both", "blended", "Artist b4"),
            entry("LB Only", "blended", "Artist b1"),
            entry("Last.fm Only", "blended", "Artist b7"),
        ]
    );
    assert_eq!(
        because_entries(false).await,
        vec![
            entry("LB Only", "listenbrainz", "Artist b1"),
            entry("Both", "listenbrainz", "Artist b4"),
        ]
    );
}

/// The user's saved discover page, once no build is running.
async fn settled_page(_rig: &AuthRig, app: &Router, what: &str) -> Value {
    // The build's last write comes before its in-flight mark clears, so no
    // commit follows the clear: this asks until the page says it has settled.
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (_, page) = call(app, "GET", "/api/v3/discover", None).await;
            if page["refreshing"] == false && page["generated_at"].is_number() {
                return page;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never happened: {what}"))
}

/// While the advanced settings cannot be read no build starts, asked for
/// or not: visits get the last saved page. Saving the settings again lets
/// the next refresh build.
#[tokio::test]
async fn unreadable_settings_hold_builds_and_serve_the_saved_page() {
    let dir = crate::common::ScratchDir::new("discover-unreadable");
    let mut rig = AuthRig::new("discover-unreadable").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let app = page_app(&rig, &dir);
    call(&app, "GET", "/api/v3/discover", None).await;
    settled_page(&rig, &app, "the first page was built").await;

    db.write(Lane::Foreground, "test.break_settings", |tx| {
        tx.execute(
            "INSERT INTO config_sections (key, value_json, revision, updated_at) \
             VALUES ('advanced_settings', '{\"audiodb_api_key\": \"not sealed\"}', 50, 1) \
             ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, \
             revision = excluded.revision",
            [],
        )?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("a secret that does not decrypt stored");
    rig.config = Arc::new(
        ConfigStore::open(
            db.clone(),
            Crypto::from_key_bytes(&TEST_KEY).expect("test key"),
            rig.platform.clock.clone(),
            LegacyConfig::at(&dir, false),
        )
        .await
        .expect("settings reopen"),
    );
    let restarted = page_app(&rig, &dir);

    let (status, _) = call(&restarted, "POST", "/api/v3/discover/refresh", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, page) = call(&restarted, "GET", "/api/v3/discover", None).await;
    assert_eq!(page["refreshing"], false, "no build started");
    assert_eq!(
        content(&page, "because_you_listen_to")["entries"][0]["seed_artist"],
        "Seed",
        "the saved page is served"
    );

    rig.config
        .save_secret_blocking(AdvancedSettings::default())
        .expect("settings saved again");
    call(&restarted, "POST", "/api/v3/discover/refresh", None).await;
    let (_, page) = call(&restarted, "GET", "/api/v3/discover", None).await;
    assert_eq!(page["refreshing"], true, "builds resume");
    settled_page(&rig, &restarted, "the rebuild finished").await;
}

/// A page built while a read failed is kept, without the shelf that read
/// fed, only until the retry wait is up: the next visit then builds again
/// and the shelf comes back. A page built with every read answered keeps
/// its hour.
#[tokio::test]
async fn a_shelf_whose_read_failed_is_built_again_soon() {
    let dir = crate::common::ScratchDir::new("discover-failed-shelf");
    let rig = AuthRig::new("discover-failed-shelf").await;
    seed_user(&rig.platform.db).await;
    let script = Arc::new(page_script());
    script.fresh_down.store(true, Ordering::SeqCst);
    let network = Arc::clone(&script);
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = network;
    }));
    let fresh = |page: &Value| {
        content(page, "fresh_releases")["shelf"]["items"]
            .as_array()
            .map(Vec::len)
    };

    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the first page was built").await;
    assert_eq!(fresh(&page), None, "the failed shelf is left out");
    assert_eq!(
        state_of(&page, "fresh_releases"),
        json!({"key": "fresh_releases", "state": "source_down", "source": "listenbrainz"}),
        "and says which service failed"
    );

    script.fresh_down.store(false, Ordering::SeqCst);
    rig.clock().advance(301_000);
    let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(page["refreshing"], true, "a rebuild started");
    let page = after_commits(&rig.platform.db, "the shelf came back", || async {
        let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
        (fresh(&page) == Some(1)).then_some(page)
    })
    .await;
    assert_eq!(fresh(&page), Some(1));
    assert_eq!(state_of(&page, "fresh_releases"), Value::Null);
    settled_page(&rig, &app, "the rebuild finished").await;

    rig.clock().advance(301_000);
    let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(page["refreshing"], false, "a whole page is kept");
}

/// The page once a build newer than `before` has landed and settled.
async fn page_after(app: &Router, before: &Value, what: &str) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (_, page) = call(app, "GET", "/api/v3/discover", None).await;
            if page["refreshing"] == false && page["generated_at"] != before["generated_at"] {
                return page;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("never happened: {what}"))
}

/// A rebuild whose similar-artist reads fail keeps the Because shelves the
/// page had, as they were, and says when they are from. A section whose
/// reads all answered with nothing says it is empty.
#[tokio::test]
async fn a_failed_read_keeps_the_sections_the_page_had() {
    let dir = crate::common::ScratchDir::new("discover-kept");
    let rig = AuthRig::new("discover-kept").await;
    seed_user(&rig.platform.db).await;
    let script = Arc::new(page_script());
    let network = Arc::clone(&script);
    let providers = Providers::closed();
    let answers = providers.cache.clone();
    let app = discover_routes(discover_deps(&rig, &dir, providers, |slot| {
        *slot = network;
    }));

    call(&app, "GET", "/api/v3/discover", None).await;
    let first = settled_page(&rig, &app, "the first page was built").await;
    let because = content(&first, "because_you_listen_to").clone();
    assert!(
        because["entries"].as_array().is_some_and(|e| !e.is_empty()),
        "{first}"
    );
    assert_eq!(state_of(&first, "because_you_listen_to"), Value::Null);
    assert_eq!(
        state_of(&first, "anniversaries"),
        json!({"key": "anniversaries", "state": "empty"}),
        "an empty library has no anniversaries"
    );

    script.similar_down.store(true, Ordering::SeqCst);
    answers.clear().await.expect("provider answers cleared");
    rig.clock().advance(60_000);
    let (status, _) = call(&app, "POST", "/api/v3/discover/refresh", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let page = page_after(&app, &first, "the rebuild landed").await;
    assert_eq!(
        content(&page, "because_you_listen_to"),
        &because,
        "the shelves are kept as they were"
    );
    assert_eq!(
        state_of(&page, "because_you_listen_to"),
        json!({
            "key": "because_you_listen_to",
            "state": "kept",
            "source": "listenbrainz",
            "since": first["generated_at"],
        })
    );
}

/// For a person with nothing linked, the Listening Lounge and the shelves
/// grown from listening ask for ListenBrainz, and a library shelf with
/// nothing in it says it is empty.
#[tokio::test]
async fn sections_say_what_a_person_with_nothing_linked_needs() {
    let dir = crate::common::ScratchDir::new("discover-unlinked");
    let rig = AuthRig::new("discover-unlinked").await;
    seed_user(&rig.platform.db).await;
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script {
            user: UserMusic::default(),
            ..page_script()
        });
    }));

    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the page was built").await;
    let needs_listenbrainz =
        |key: &str| json!({"key": key, "state": "needs_link", "source": "listenbrainz"});
    assert_eq!(
        state_of(&page, "listeners_like_you"),
        needs_listenbrainz("listeners_like_you")
    );
    assert_eq!(
        state_of(&page, "because_you_listen_to"),
        needs_listenbrainz("because_you_listen_to")
    );
    assert_eq!(
        state_of(&page, "anniversaries"),
        json!({"key": "anniversaries", "state": "empty"})
    );
    assert_eq!(
        page["section_states"][0]["zone"], "essentials",
        "each state says where its section sits"
    );
}

/// A person with nothing linked who plays music in the app gets a "Because
/// You Listen To" row for the artist they play most, with their own play
/// count and the app's own badge, and the saved health counts every seed
/// they have.
#[tokio::test]
async fn plays_in_the_app_seed_a_person_with_nothing_linked() {
    let dir = crate::common::ScratchDir::new("discover-local-seeds");
    let rig = AuthRig::new("discover-local-seeds").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let at = rig.platform.clock.now_ms();
    insert(
        &db,
        [
            library_artist("s", "Seed", SEED, "group"),
            library_artist("x", "Second", &mbid('e', 9), "group"),
            plays("user-1", "s", 5, at),
            plays("user-1", "x", 2, at),
        ]
        .concat(),
    )
    .await;
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script {
            user: UserMusic::default(),
            ..page_script()
        });
    }));

    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the page was built").await;
    let entry = &content(&page, "because_you_listen_to")["entries"][0];
    assert_eq!(entry["seed_artist"], "Seed", "{page}");
    assert_eq!(entry["listen_count"], 5, "their own plays");
    assert_eq!(entry["section"]["source"], "local");
    let row: Value = serde_json::from_str(&db.one::<String>(
        "SELECT CAST(payload AS TEXT) FROM catalog_cached_pages \
         WHERE user_id = 'user-1' AND page = 'discover'",
        [],
    ))
    .expect("the saved page decodes");
    assert_eq!(row["health"]["seeds_found"], 2);
    assert_eq!(row["health"]["seeds_used"], 2);
}

/// Top Picks built while an artist's albums would not read are kept for
/// five minutes, not four hours: the rebuild after the retry wait takes
/// the albums up.
#[tokio::test]
async fn top_picks_built_past_a_failed_read_are_built_again_soon() {
    let dir = crate::common::ScratchDir::new("discover-picks-retry");
    let rig = AuthRig::new("discover-picks-retry").await;
    seed_user(&rig.platform.db).await;
    let script = Arc::new(page_script());
    script.albums_down.store(true, Ordering::SeqCst);
    let network = Arc::clone(&script);
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = network;
    }));
    let personal = |page: &Value| {
        content(page, "top_picks")["picks"]["items"]
            .as_array()
            .map_or(0, |items| {
                items
                    .iter()
                    .filter(|pick| pick["seed_artist"].is_string())
                    .count()
            })
    };

    call(&app, "GET", "/api/v3/discover", None).await;
    let first = settled_page(&rig, &app, "the first page was built").await;
    assert_eq!(personal(&first), 0, "no artist's albums read");

    script.albums_down.store(false, Ordering::SeqCst);
    rig.clock().advance(361_000);
    let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(page["refreshing"], true, "a rebuild started");
    let page = page_after(&app, &first, "the rebuild landed").await;
    assert!(personal(&page) > 0, "{}", content(&page, "top_picks"));
}

/// The health the user's saved discover page carries.
fn saved_health(db: &droppedneedle_kernel::db::Db) -> Value {
    let row: Value = serde_json::from_str(&db.one::<String>(
        "SELECT CAST(payload AS TEXT) FROM catalog_cached_pages \
         WHERE user_id = 'user-1' AND page = 'discover'",
        [],
    ))
    .expect("the saved page decodes");
    row["health"].clone()
}

/// The album ids of a page's Top Picks.
fn pick_ids(page: &Value) -> Vec<String> {
    content(page, "top_picks")["picks"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|pick| pick["album"]["musicbrainz_id"].as_str().map(str::to_owned))
        .collect()
}

/// Albums of similar artists the library already holds are never picked:
/// the other candidates are, and the saved health counts no owned pick.
#[tokio::test]
async fn top_picks_never_hold_an_album_the_library_has() {
    let dir = crate::common::ScratchDir::new("discover-picks-owned");
    let rig = AuthRig::new("discover-picks-owned").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    // The first album of the first similar artist, the second of the third.
    let owned = [mbid('a', b'1'), mbid('a', b'3' + 20)];
    own(&db, &owned).await;
    let app = page_app(&rig, &dir);

    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the page was built").await;
    let ids = pick_ids(&page);
    assert!(!ids.is_empty(), "{}", content(&page, "top_picks"));
    assert!(
        ids.iter().all(|id| !owned.contains(id)),
        "no owned pick: {ids:?}"
    );
    assert_eq!(saved_health(&db)["owned_picks"], 0);
}

/// Top Picks hold no chart album: a person with three seeds gets only the
/// picks grown from them, though they are fewer than asked for. A person
/// with nothing to go on gets no Top Picks, the section saying what to
/// link, or that it is empty when ListenBrainz is linked. No saved page
/// counts a chart item under a personal heading.
#[tokio::test]
async fn top_picks_hold_no_chart_album_with_seeds_or_without() {
    let seed_mbid = |n: u8| mbid('e', n);
    let similar = |n: u8| mbid('b', n);
    let chart: Vec<ListenedAlbum> = (1..=12)
        .map(|n| listened(ranked(100 + n, &mbid('d', n), 10_000)))
        .collect();
    let seeded = Script {
        user_artists: (1..=3)
            .map(|n| ArtistRow {
                name: format!("Seed {n}"),
                mbid: Some(seed_mbid(n)),
                listen_count: Some(50),
            })
            .collect(),
        similar: (1..=3)
            .map(|n| {
                let row = ScoredArtist {
                    artist: ArtistRow {
                        name: format!("Similar {n}"),
                        mbid: Some(similar(n)),
                        listen_count: Some(100),
                    },
                    score: 100.0,
                };
                (seed_mbid(n), vec![row])
            })
            .collect(),
        artist_albums: (1..=3)
            .map(|n| {
                let artist = similar(n);
                let albums = vec![ranked(n, &artist, 500), ranked(n + 20, &artist, 300)];
                (artist, albums)
            })
            .collect(),
        sitewide_albums: chart.clone(),
        ..page_script()
    };
    let nothing = Script {
        user: UserMusic::default(),
        user_artists: Vec::new(),
        similar: HashMap::new(),
        sitewide_albums: chart.clone(),
        ..page_script()
    };
    let linked = Script {
        user_artists: Vec::new(),
        similar: HashMap::new(),
        sitewide_albums: chart,
        ..page_script()
    };
    let needs_link = json!({"key": "top_picks", "state": "needs_link", "source": "listenbrainz"});
    let empty = json!({"key": "top_picks", "state": "empty"});

    for (name, script, picks, state) in [
        ("seeded", seeded, 6, Value::Null),
        ("nothing", nothing, 0, needs_link),
        ("linked", linked, 0, empty),
    ] {
        let dir = crate::common::ScratchDir::new(&format!("discover-picks-{name}"));
        let rig = AuthRig::new(&format!("discover-picks-{name}")).await;
        seed_user(&rig.platform.db).await;
        let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
            *slot = Arc::new(script);
        }));

        call(&app, "GET", "/api/v3/discover", None).await;
        let page = settled_page(&rig, &app, "the page was built").await;
        let items = content(&page, "top_picks")["picks"]["items"].clone();
        assert_eq!(pick_ids(&page).len(), picks, "{name}: {items}");
        let items = items.as_array().cloned().unwrap_or_default();
        assert!(
            items.iter().all(|pick| pick["seed_artist"].is_string()),
            "{name}: every pick grew from a seed: {items:?}"
        );
        assert_eq!(
            saved_health(&rig.platform.db)["chart_under_personal"],
            0,
            "{name}"
        );
        assert_eq!(state_of(&page, "top_picks"), state, "{name}");
    }
}

/// A Last.fm person whose seeds' similar artists have one Last.fm album
/// each, under releases from `first`. Releases from 41 are matched to
/// release groups already; the lookups of any other never answer.
fn lastfm_picks_script(first: u8) -> Script {
    Script {
        user: UserMusic {
            lastfm_username: Some("scrobbler".to_owned()),
            lastfm: true,
            primary: ListenSource::Lastfm,
            ..UserMusic::default()
        },
        lastfm_user_artists: vec![ArtistRow {
            name: "Seed".to_owned(),
            mbid: Some(SEED.to_owned()),
            listen_count: Some(50),
        }],
        lastfm_albums: (1..=3)
            .map(|n| {
                let album = LastFmAlbum {
                    name: format!("Last.fm {n}"),
                    artist_name: format!("Similar {n}"),
                    mbid: Some(mbid('e', first + n)),
                };
                (mbid('b', n), vec![album])
            })
            .collect(),
        release_lookups_hang: first != 40,
        ..page_script()
    }
}

/// Top Picks that run out of time matching Last.fm albums failed: the page
/// keeps the picks it had, saying since when, rather than an empty shelf.
#[tokio::test]
async fn top_picks_that_run_out_of_time_keep_the_ones_the_page_had() {
    let dir = crate::common::ScratchDir::new("discover-picks-lastfm-timeout");
    let rig = AuthRig::new("discover-picks-lastfm-timeout").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    rig.config
        .save_secret_blocking(LastFmSettings {
            enabled: true,
            ..LastFmSettings::default()
        })
        .expect("Last.fm settings save");
    let matched: Vec<String> = (41..=43)
        .map(|n| {
            let (release, group) = (mbid('e', n), mbid('d', n));
            format!(
                "INSERT INTO catalog_mbid_resolutions (source_mbid_lower, source_mbid, \
                 release_group_mbid) VALUES ('{release}', '{release}', '{group}');"
            )
        })
        .collect();
    db.write(Lane::Foreground, "test.resolutions", move |tx| {
        tx.execute_batch(&matched.concat())?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("resolutions");
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(lastfm_picks_script(40));
    }));
    call(&app, "GET", "/api/v3/discover", None).await;
    let first = settled_page(&rig, &app, "the first page was built").await;
    let picks = pick_ids(&first);
    assert_eq!(picks.len(), 3, "{}", content(&first, "top_picks"));

    // Past the page's and the picks' reuse windows, a restart whose album
    // lookups never answer.
    rig.clock().advance(5 * 3_600_000);
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(lastfm_picks_script(60));
    }));
    // The time bounds then pass at once instead of in real time.
    tokio::time::pause();
    let started = std::time::Instant::now();
    let page = loop {
        let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
        if page["refreshing"] == false && page["generated_at"] != first["generated_at"] {
            break page;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the rebuild never landed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    };
    assert_eq!(pick_ids(&page), picks, "{}", content(&page, "top_picks"));
    let state = state_of(&page, "top_picks");
    assert_eq!(state["state"], "kept", "{state}");
    assert_eq!(state["since"], first["generated_at"]);
}

/// A home warm that keeps nothing, as every warm does while ListenBrainz
/// is down, counting its tries.
#[derive(Default)]
struct KeepsNothing(AtomicUsize);

impl Warm for KeepsNothing {
    fn feature(&self) -> Feature {
        Feature::Home
    }

    fn warm<'a>(&'a self, _user_id: &'a str) -> BoxFuture<'a, bool> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { false })
    }
}

/// The warm cycle passes every 90 seconds. A warm that kept nothing is
/// tried again after 15 minutes, not on the next pass, and a build that
/// failed is not claimed again within the page's wait, which the warm
/// cycle claims through without asking whether a build is due.
#[tokio::test]
async fn a_warm_that_kept_nothing_waits_before_it_is_tried_again() {
    let rig = AuthRig::new("discover-warm-wait").await;
    seed_user(&rig.platform.db).await;
    let warm = Arc::new(KeepsNothing::default());
    let scheduler = WarmScheduler::new(
        ActivityDb::new(rig.platform.db.clone()),
        ListeningData::new(Arc::new(Script::default()), Providers::closed().cache),
        rig.platform.clock.clone(),
        true,
        Arc::new(droppedneedle_catalog::discover::due::Due::new(
            rig.platform.db.clone(),
            droppedneedle_kernel::jobs::queue::JobQueue::new(
                rig.platform.db.clone(),
                rig.platform.clock.clone(),
            ),
            rig.platform.clock.clone(),
            true,
        )),
        vec![warm.clone()],
    );
    let tries = || warm.0.load(Ordering::SeqCst);
    scheduler
        .record("user-1", Feature::Home, ArtistDetail::default())
        .await
        .expect("activity recorded");
    scheduler.tick().await;
    assert_eq!(tries(), 1, "the first pass warms");
    for _ in 0..9 {
        rig.clock().advance(90_000);
        scheduler.tick().await;
    }
    assert_eq!(tries(), 1, "no pass within 15 minutes tries again");
    rig.clock().advance(90_000);
    scheduler.tick().await;
    assert_eq!(tries(), 2, "the pass after them does");

    let cache = page_cache(&rig);
    let claim = cache.claim("user-1").await.expect("first build");
    assert!(
        !cache
            .run(claim, &built_for(), async { None::<String> })
            .await,
        "a failed build"
    );
    rig.clock().advance(299_000);
    assert!(
        cache.claim("user-1").await.is_none(),
        "no build is claimed within the wait"
    );
    rig.clock().advance(2_000);
    assert!(cache.claim("user-1").await.is_some(), "one is after it");
}

/// The similar artists a stub ListenBrainz knows for [`SEED`], with their
/// co-listening score and listener count (none for the first), and the
/// order fame divided out gives them.
const NEIGHBOURS: [(&str, i64, Option<i64>); 5] = [
    ("Uncounted", 1_000, None),
    ("Famous", 900, Some(1_000_000)),
    ("Middle", 600, Some(40_000)),
    ("Cult", 400, Some(1_000)),
    ("Small", 100, Some(100)),
];
const RANKED: [&str; 5] = ["Cult", "Small", "Middle", "Famous", "Uncounted"];

fn neighbour_id(at: usize) -> String {
    mbid('9', at as u8 + 1)
}

/// A ListenBrainz, labs, Last.fm and MusicBrainz stub for one linked
/// listener whose one top artist is [`SEED`]: its labs list, its
/// neighbours' counts, and one album each on ListenBrainz and on Last.fm
/// (whose release MusicBrainz places). Anything else answers 204. Answers
/// the origin and the calls per path.
async fn similar_stub() -> (String, Arc<Mutex<HashMap<String, usize>>>) {
    let hits: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
    let seen = hits.clone();
    let stub = Router::new().fallback(move |uri: Uri| {
        let seen = seen.clone();
        async move {
            let path = uri.path().to_owned();
            *seen.lock().unwrap().entry(path.clone()).or_default() += 1;
            let param = |name: &str| {
                uri.query()
                    .unwrap_or_default()
                    .split('&')
                    .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
                    .unwrap_or_default()
                    .to_owned()
            };
            let body = if path == "/lastfm/" && param("method") == "artist.getTopAlbums" {
                json!({"topalbums": {"album": [{"name": "A Last.fm Album",
                    "mbid": param("mbid").replace("9999", "7777"), "playcount": "9",
                    "artist": {"name": "Someone"}}]}})
            } else if let Some(release) = path.strip_prefix("/musicbrainz/release/") {
                json!({"id": release, "title": "A Release",
                    "release-group": {"id": release.replace("7777", "6666"), "title": "A Group"}})
            } else if path == "/labs/similar-artists/json" {
                json!(
                    NEIGHBOURS
                        .iter()
                        .enumerate()
                        .map(
                            |(at, (name, score, _))| json!({"artist_mbid": neighbour_id(at),
                            "name": name, "score": score, "reference_mbid": SEED})
                        )
                        .collect::<Vec<_>>()
                )
            } else if path == "/listenbrainz/1/popularity/artist" {
                json!(
                    NEIGHBOURS
                        .iter()
                        .enumerate()
                        .map(
                            |(at, (_, _, listeners))| json!({"artist_mbid": neighbour_id(at),
                            "total_listen_count": listeners.map(|n| n * 10),
                            "total_user_count": listeners})
                        )
                        .collect::<Vec<_>>()
                )
            } else if path == "/listenbrainz/1/stats/user/listener/artists" {
                json!({"payload": {"artists": [{"artist_name": "Seed", "listen_count": 50,
                    "artist_mbids": [SEED]}]}})
            } else if let Some(artist) =
                path.strip_prefix("/listenbrainz/1/popularity/top-release-groups-for-artist/")
            {
                json!([{"release_group_mbid": artist.replace("9999", "8888"),
                    "total_listen_count": 100, "release_group": {"name": "An Album"},
                    "artist": {"name": "Someone"}}])
            } else {
                return StatusCode::NO_CONTENT.into_response();
            };
            axum::Json(body).into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    (origin, hits)
}

/// One answer serves every "similar to" list: the Because shelf, the artist
/// page, the album page (a Last.fm-first person's too) and the Weekly Mix's
/// source give one order for one artist, with fame divided out (a
/// neighbour with a lower score and far fewer listeners leads a famous
/// one, and one with no count comes last). The list and the counts are
/// each read once: the second ask makes no call. Once the person hides an
/// artist and says no to an album, the artist page and the album page
/// leave them out.
#[tokio::test]
async fn one_similar_answer_leads_every_list_in_one_order() {
    let (origin, hits) = similar_stub().await;
    let dir = crate::common::ScratchDir::new("discover-one-similar");
    let rig = AuthRig::new("discover-one-similar").await;
    seed_user(&rig.platform.db).await;
    rig.seed_user("2", droppedneedle_auth::Role::User).await;
    rig.set_lastfm_instance_key("instance-key");
    let store = crate::common::listening_store(&rig.platform);
    store
        .save_listenbrainz("user-1", "listener", "token")
        .await
        .expect("link");
    store
        .update_prefs(
            "user-2",
            &PrefsPatch {
                primary_music_source: Some(ListenSource::Lastfm),
                ..PrefsPatch::default()
            },
        )
        .await
        .expect("Last.fm first");
    let http = HttpClientFactory::new().unwrap();
    let providers = Providers::unpaced_on(
        Arc::new(ReqwestWire::new(&http)),
        rig.platform.clock.clone(),
        ProviderEndpoints {
            listenbrainz: format!("{origin}/listenbrainz"),
            listenbrainz_labs: format!("{origin}/labs"),
            lastfm: format!("{origin}/lastfm/"),
            musicbrainz: format!("{origin}/musicbrainz"),
            brainzmash: format!("{origin}/musicbrainz"),
            ..ProviderEndpoints::closed()
        },
    );
    let inputs = discover_inputs(&rig, &dir, providers);
    let providers = inputs.providers.clone();
    let network = live_network(&inputs, &rig.config);
    let app = discover_routes(deps_over(inputs, &rig.config, network));
    let catalog = Catalog::new(
        Upstream::new(
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(&rig.config)),
            rig.deps.clone(),
            store.clone(),
        ),
        LocalCatalog::new(rig.platform.db.clone()),
        crate::common::bundles::quiet_page_sources(crate::common::bundles::cover_warm_at(
            &rig.platform.db,
            rig.platform.clock.clone(),
        )),
    );
    let name_of = |id: &str| {
        (0..NEIGHBOURS.len())
            .find(|at| neighbour_id(*at) == id)
            .map_or("?", |at| NEIGHBOURS[at].0)
    };

    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the page was built").await;
    let shelf: Vec<String> =
        content(&page, "because_you_listen_to")["entries"][0]["section"]["items"]
            .as_array()
            .expect("a Because shelf")
            .iter()
            .map(|item| item["name"].as_str().unwrap_or_default().to_owned())
            .collect();
    assert_eq!(shelf, RANKED, "{page}");

    let artist_page = catalog
        .similar_artists("user-1", SEED, 15, None, RequestPriority::UserInitiated)
        .await
        .expect("the artist page's similar artists");
    let names: Vec<&str> = artist_page
        .similar_artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect();
    assert_eq!(names, RANKED, "the artist page");
    assert_eq!(
        artist_page.similar_artists[0].listen_count,
        Some(10_000),
        "a neighbour carries its own listens"
    );

    let album_page = catalog
        .similar_albums("user-1", &mbid('a', 1), SEED, 5)
        .await
        .expect("the album page's similar albums");
    let by: Vec<&str> = album_page
        .albums
        .iter()
        .filter_map(|album| album.artist_id.as_deref())
        .map(name_of)
        .collect();
    assert_eq!(by, RANKED, "the album page");

    let lastfm_first = catalog
        .similar_albums("user-2", &mbid('a', 1), SEED, 5)
        .await
        .expect("a Last.fm-first person's similar albums");
    assert_eq!(
        lastfm_first.source,
        ListenSource::Lastfm,
        "{lastfm_first:?}"
    );
    let by: Vec<&str> = lastfm_first
        .albums
        .iter()
        .filter_map(|album| album.artist_id.as_deref())
        .map(name_of)
        .collect();
    assert_eq!(by, RANKED, "a Last.fm-first person's album page");

    let mix = shared::similar_artists(
        &providers.cache,
        &providers.listenbrainz,
        SEED,
        10,
        &Lender::Person(store, "user-1".to_owned()),
        RequestPriority::BackgroundSync,
    )
    .await
    .expect("the Weekly Mix's similar artists");
    let names: Vec<&str> = mix.iter().map(|row| row.artist.name.as_str()).collect();
    assert_eq!(names, RANKED, "the Weekly Mix's source");

    // "Not this artist" on Famous, "no" to Small's album.
    let at = |name: &str| (0..NEIGHBOURS.len()).find(|at| NEIGHBOURS[*at].0 == name);
    let famous = neighbour_id(at("Famous").expect("Famous"));
    let small_album = neighbour_id(at("Small").expect("Small")).replace("9999", "8888");
    let seed = format!(
        "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'artist', '{famous}', 'not_artist', 1), \
         ('user-1', 'album', '{small_album}', 'no', 1)"
    );
    rig.platform
        .db
        .write(Lane::Foreground, "test.feedback", move |tx| {
            tx.execute_batch(&seed)?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("feedback rows");
    let left_out = |by: Vec<String>, what: &str| {
        assert!(!by.is_empty(), "{what} still has rows");
        assert!(
            by.iter().all(|row| *row != famous && *row != small_album),
            "{what} leaves out what was hidden: {by:?}"
        );
    };
    let artist_page = catalog
        .similar_artists("user-1", SEED, 15, None, RequestPriority::UserInitiated)
        .await
        .expect("the artist page's similar artists");
    let names: Vec<&str> = artist_page
        .similar_artists
        .iter()
        .map(|artist| artist.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["Cult", "Small", "Middle", "Uncounted"],
        "the artist page"
    );
    let album_page = catalog
        .similar_albums("user-1", &mbid('a', 1), SEED, 5)
        .await
        .expect("the album page's similar albums");
    left_out(
        album_page
            .albums
            .iter()
            .flat_map(|album| [album.musicbrainz_id.clone(), album.artist_id.clone()])
            .flatten()
            .collect(),
        "the album page",
    );

    let hits = hits.lock().unwrap();
    assert_eq!(hits.get("/labs/similar-artists/json"), Some(&1), "{hits:?}");
    assert_eq!(
        hits.get("/listenbrainz/1/popularity/artist"),
        Some(&1),
        "{hits:?}"
    );
}

/// After a good answer, a labs read that fails (labs no longer offers the
/// dataset) keeps serving the kept list. The failure rests labs reads, so
/// an artist with nothing kept is not asked for meanwhile.
#[tokio::test]
async fn a_failed_labs_read_keeps_the_list_it_had() {
    const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
    let rig = AuthRig::new("discover-labs-kept").await;
    let wire = Arc::new(ScriptedWire::new());
    let labs = format!("{}/similar-artists/json", listenbrainz::LABS_BASE_URL);
    wire.answer(
        &labs,
        200,
        &[],
        include_str!("../fixtures/listenbrainz/labs_similar_artists_radiohead.json"),
    );
    wire.answer(
        &labs,
        400,
        &[("content-type", "text/html")],
        include_str!("../fixtures/listenbrainz/labs_unknown_algorithm.html"),
    );
    wire.on(
        Method::Post,
        &format!("{}/1/popularity/artist", listenbrainz::DEFAULT_BASE_URL),
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: include_bytes!("../fixtures/listenbrainz/popularity_artist_post.json").to_vec(),
        },
    );
    let providers = Providers::unpaced_on(
        wire.clone(),
        rig.platform.clock.clone(),
        ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            listenbrainz_labs: listenbrainz::LABS_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    );
    let read = |artist: &'static str| {
        shared::similar_artists(
            &providers.cache,
            &providers.listenbrainz,
            artist,
            100,
            &Lender::Anonymous,
            RequestPriority::BackgroundSync,
        )
    };
    let labs_reads = || {
        wire.sent()
            .iter()
            .filter(|sent| sent.url.path() == "/similar-artists/json")
            .count()
    };

    let first = read(RADIOHEAD).await.expect("the recorded list");
    assert_eq!(first.len(), 100);
    assert_eq!(
        first[0].artist.name, "Red Hot Chili Peppers",
        "the one neighbour with a count leads"
    );
    rig.clock().advance(15 * 86_400_000);
    let kept = read(RADIOHEAD).await.expect("the kept list stands in");
    assert_eq!(kept, first);
    assert_eq!(labs_reads(), 2, "the stale list was asked for again");

    let other = read(SEED).await;
    assert!(other.is_err(), "nothing kept, and the reads rest");
    assert_eq!(labs_reads(), 2, "a resting read sends nothing");
}

/// A pass over several artists stops asking at its first failed similar
/// read: the rest answer only from kept lists.
#[tokio::test]
async fn a_similar_pass_stops_at_its_first_failed_read() {
    let script = Arc::new(Script::default());
    script.similar_down.store(true, Ordering::SeqCst);
    let listening = ListeningData::new(script.clone(), Providers::closed().cache);
    let ask = Ask::build(RequestPriority::BackgroundSync);
    let artists: Vec<String> = (1..=4).map(|n| mbid('d', n)).collect();
    let answers = listening
        .similar_artists_each("user-1", &artists, 10, &ask)
        .await;
    assert!(answers.iter().all(Result::is_err), "{answers:?}");
    assert_eq!(script.similar_reads.load(Ordering::SeqCst), 1);
}

/// Labs reads rest after a failed one for every caller, in memory, so a
/// pass and the passes after it stop at the first failure with the
/// provider cache off.
#[tokio::test]
async fn labs_reads_rest_after_a_failure_with_the_cache_off() {
    let hits: Arc<Mutex<usize>> = Arc::default();
    let seen = hits.clone();
    let stub = Router::new().fallback(move || {
        let seen = seen.clone();
        async move {
            *seen.lock().unwrap() += 1;
            (
                StatusCode::BAD_REQUEST,
                [("content-type", "text/html")],
                include_str!("../fixtures/listenbrainz/labs_unknown_algorithm.html"),
            )
                .into_response()
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let rig = AuthRig::new("discover-labs-rest").await;
    let dir = crate::common::ScratchDir::new("discover-labs-rest");
    // A cache file that would be the database's own is refused: the cache
    // is off.
    let cache = CacheDb::open_blocking(&dir, &dir.join(droppedneedle_kernel::db::cache::FILE_NAME));
    assert!(!cache.is_on());
    let providers = Providers::build(
        &rig.platform,
        ProviderEndpoints {
            listenbrainz_labs: format!("{origin}/labs"),
            ..ProviderEndpoints::closed()
        },
        cache,
    );
    let artists: Vec<String> = (1..=3).map(|n| mbid('d', n)).collect();
    let pass = || {
        shared::similar_artists_each(
            &providers.cache,
            &providers.listenbrainz,
            &artists,
            10,
            &Lender::Anonymous,
            RequestPriority::BackgroundSync,
        )
    };
    assert!(pass().await.iter().all(Result::is_err));
    assert_eq!(
        *hits.lock().unwrap(),
        1,
        "the pass stopped at its first failure"
    );
    assert!(pass().await.iter().all(Result::is_err));
    assert_eq!(*hits.lock().unwrap(), 1, "the next pass sends nothing");
}

/// A listener-count read that fails after a good list keeps the list: it
/// answers in score order, and the next ask reads it without asking labs.
#[tokio::test]
async fn a_failed_count_read_keeps_the_list() {
    const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{}/similar-artists/json", listenbrainz::LABS_BASE_URL),
        200,
        &[],
        include_str!("../fixtures/listenbrainz/labs_similar_artists_radiohead.json"),
    );
    wire.on(
        Method::Post,
        &format!("{}/1/popularity/artist", listenbrainz::DEFAULT_BASE_URL),
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: br#"{"payload": 7}"#.to_vec(),
        },
    );
    let providers = Providers::unpaced_at(
        wire.clone(),
        ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            listenbrainz_labs: listenbrainz::LABS_BASE_URL.to_owned(),
            ..ProviderEndpoints::closed()
        },
    );
    let read = || {
        shared::similar_artists(
            &providers.cache,
            &providers.listenbrainz,
            RADIOHEAD,
            3,
            &Lender::Anonymous,
            RequestPriority::BackgroundSync,
        )
    };
    let names = |rows: Vec<ScoredArtist>| -> Vec<String> {
        rows.into_iter().map(|row| row.artist.name).collect()
    };
    let first = names(read().await.expect("the list answers uncounted"));
    assert_eq!(first, ["Nirvana", "Red Hot Chili Peppers", "Muse"]);
    let again = names(read().await.expect("the kept list"));
    assert_eq!(again, first);
    let labs_reads = wire
        .sent()
        .iter()
        .filter(|sent| sent.url.path() == "/similar-artists/json")
        .count();
    assert_eq!(labs_reads, 1, "the list was kept");
}

/// People with nothing linked, on an install where nobody has linked
/// ListenBrainz, get Missing Essentials from the artist their library holds
/// most of: its albums they do not have, less one they said no to. Two of
/// them asking for that artist cost one discography read and one count
/// read.
#[tokio::test]
async fn people_with_nothing_linked_get_album_cards_for_one_read() {
    let dir = crate::common::ScratchDir::new("discover-unlinked-albums");
    let rig = AuthRig::new("discover-unlinked-albums").await;
    let artist = mbid('e', 1);
    // Ada said no to one of the missing albums; Bob did not.
    let passed = mbid('a', 104);
    let mut seed = format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
         ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0); \
         INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'album', '{passed}', 'no', 1); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('a1', 'Collected', 'collected', 'group', 1, 1); \
         INSERT INTO local_artist_external_identities (local_artist_id, provider, \
         provider_artist_id, decision_source, selected_at) \
         VALUES ('a1', 'musicbrainz', '{artist}', 'manual', 1);"
    );
    for n in 100..103u8 {
        let group = mbid('a', n);
        seed.push_str(&format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) \
             VALUES ('al{n}', 'r1', 'g{n}', 'Owned', 'owned', 'a1', 'automatic', 1, 1); \
             INSERT INTO local_album_artists (local_album_id, position, local_artist_id, \
             role) VALUES ('al{n}', 0, 'a1', 'main'); \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) \
             VALUES ('al{n}', 'musicbrainz', '{group}', 'manual', 1); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
             relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, \
             title_folded, album_title, album_title_folded, file_format, availability, \
             ingest_source, imported_at, membership_source) VALUES ('t{n}', 'al{n}', 'r1', \
             '/music/t{n}.flac', 't{n}.flac', 'h{n}', 1, 1, 's{n}', 'Song', 'song', 'Owned', \
             'owned', 'flac', 'indexed', 'scan', 1, 'automatic');"
        ));
    }
    rig.platform
        .db
        .write(Lane::Foreground, "test.seed", move |tx| {
            tx.execute_batch(&seed)?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("library rows");
    let script = Arc::new(Script {
        artist_albums: HashMap::from([(
            artist.clone(),
            (100..106u8)
                .map(|n| ranked(n, &artist, 1_000 - i64::from(n)))
                .collect(),
        )]),
        ..Script::default()
    });
    let network = Arc::clone(&script);
    let service = discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = network;
    })
    .content;
    let settled = |user: &'static str| {
        let service = service.clone();
        async move {
            tokio::time::timeout(crate::common::PATIENCE, async {
                loop {
                    let page = service.discover(user).await.expect("page");
                    let page = serde_json::to_value(page).expect("the page encodes");
                    if page["refreshing"] == false && page["generated_at"].is_number() {
                        return page;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the build finished")
        }
    };

    let (first, second) = tokio::join!(settled("user-1"), settled("user-2"));
    let missing: Vec<String> = (103..106u8).map(|n| mbid('a', n)).collect();
    let not_passed: Vec<String> = missing
        .iter()
        .filter(|id| **id != passed)
        .cloned()
        .collect();
    for (page, wanted) in [(first, &not_passed), (second, &missing)] {
        let shown: Vec<&str> = content(&page, "missing_essentials")["shelf"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["musicbrainz_id"].as_str())
            .collect();
        assert_eq!(
            shown, *wanted,
            "the album Ada passed on is hers alone to miss: {page}"
        );
    }
    assert_eq!(*script.discography_reads.lock().unwrap(), [artist]);
    assert_eq!(script.count_reads.lock().unwrap().len(), 1);
}

/// Every album and artist row a served page shows, as (section, kind, id,
/// artist id): shelves, shelves of several, because shelves, Top Picks.
fn page_rows(page: &Value) -> Vec<(String, String, String, String)> {
    let mut rows = Vec::new();
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    for section in page["sections"].as_array().into_iter().flatten() {
        let key = text(&section["key"]);
        let content = &section["content"];
        let mut items: Vec<&Value> = Vec::new();
        items.extend(content["shelf"]["items"].as_array().into_iter().flatten());
        for shelf in content["shelves"].as_array().into_iter().flatten() {
            items.extend(shelf["items"].as_array().into_iter().flatten());
        }
        for entry in content["entries"].as_array().into_iter().flatten() {
            items.extend(entry["section"]["items"].as_array().into_iter().flatten());
        }
        for pick in content["picks"]["items"].as_array().into_iter().flatten() {
            rows.push((
                key.clone(),
                "album".to_owned(),
                text(&pick["album"]["musicbrainz_id"]),
                text(&pick["album"]["artist_id"]),
            ));
        }
        for item in items {
            rows.push((
                key.clone(),
                text(&item["kind"]),
                text(&item["musicbrainz_id"]),
                text(&item["artist_id"]),
            ));
        }
    }
    rows
}

/// What a person tells Discover leaves the page they are served at once,
/// with no rebuild: an album they said no to goes from every section that
/// held it, and an artist they hid goes from the because shelves and from
/// every album shelf as a row filed under them, while an album filed under
/// another artist that only credits them stays. Sections that are not
/// about the person keep them: the worldwide chart and their own Last.fm
/// chart. Home's weekly exploration playlist leaves out the hidden
/// artist's song too.
#[tokio::test]
async fn what_a_person_hid_leaves_the_served_page_at_once() {
    let dir = crate::common::ScratchDir::new("discover-feedback");
    let rig = AuthRig::new("discover-feedback").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let hidden_artist = mbid('b', 3);
    let credited = mbid('f', 1);
    let charted = mbid('c', 1);
    let scrobbled = mbid('e', 1);
    let mut script = page_script();
    script.user.lastfm_username = Some("scrobbler".to_owned());
    script.user.lastfm = true;
    script.fresh = vec![AlbumRow {
        artist_name: "Similar 3 & Friends".to_owned(),
        ..ranked(90, &credited, 0).album
    }];
    script.lastfm_weekly_artists = vec![ArtistRow {
        name: "Scrobbled".to_owned(),
        mbid: Some(scrobbled.clone()),
        listen_count: Some(40),
    }];
    let mut weekly = weekly_playlist("2023-12-31T23:00:00+00:00");
    let mut by_hidden_song = weekly.tracks[0].clone();
    by_hidden_song.title = "Hidden Song".to_owned();
    by_hidden_song.artist_mbid = Some(hidden_artist.clone());
    weekly.tracks.push(by_hidden_song);
    script.weekly = Some(weekly);
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |network| {
        *network = Arc::new(script);
    }));
    call(&app, "GET", "/api/v3/discover", None).await;
    let built = settled_page(&rig, &app, "the page was built").await;
    let before = page_rows(&built);
    let home_weekly = || async {
        let (_, home) = call(&app, "GET", "/api/v3/home", None).await;
        content(&home, "weekly_exploration")["weekly"]["tracks"]
            .as_array()
            .map(|tracks| {
                tracks
                    .iter()
                    .map(|track| track["title"].as_str().unwrap_or_default().to_owned())
                    .collect::<Vec<_>>()
            })
    };
    let weekly_before = tokio::time::timeout(crate::common::PATIENCE, async {
        loop {
            if let Some(titles) = home_weekly().await {
                return titles;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("home's weekly playlist was built");
    assert_eq!(weekly_before, ["Song", "Hidden Song"]);

    // The album the most sections hold.
    let mut held: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    for (key, kind, id, _) in &before {
        if kind == "album" {
            held.entry(id.clone()).or_default().insert(key.clone());
        }
    }
    let (passed, sections) = held
        .into_iter()
        .max_by_key(|(id, sections)| (sections.len(), id.clone()))
        .expect("the page shows albums");
    assert!(
        sections.len() > 1,
        "an album on several sections: {sections:?}"
    );
    let by = |artist: &str, rows: &[(String, String, String, String)]| {
        rows.iter()
            .filter(|(_, kind, id, by)| {
                (kind == "artist" && id == artist) || (kind == "album" && by == artist)
            })
            .map(|(key, ..)| key.clone())
            .collect::<std::collections::HashSet<_>>()
    };
    let shown = by(&hidden_artist, &before);
    assert!(
        shown.contains("because_you_listen_to") && shown.len() > 1,
        "the artist is on a because shelf and an album shelf: {shown:?}"
    );
    assert!(
        before
            .iter()
            .any(|(key, _, id, artist)| key == "fresh_releases"
                && *id == mbid('a', 90)
                && *artist == credited),
        "the album that only credits them is shown"
    );
    let kept = [
        (charted.clone(), "globally_trending"),
        (scrobbled.clone(), "lastfm_weekly_artist_chart"),
    ];
    for (artist, key) in &kept {
        assert_eq!(
            by(artist, &before),
            [key.to_string()].into(),
            "shown on {key}"
        );
    }

    let ids = [
        passed.clone(),
        hidden_artist.clone(),
        charted.clone(),
        scrobbled.clone(),
    ];
    db.write(Lane::Foreground, "test.feedback", move |tx| {
        tx.execute(
            "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
             release_group_mbid, created_at) VALUES ('user-1', 'album', ?1, 'no', ?1, 1), \
             ('user-1', 'artist', ?2, 'not_artist', '', 1), \
             ('user-1', 'artist', ?3, 'not_artist', '', 1), \
             ('user-1', 'artist', ?4, 'not_artist', '', 1)",
            ids,
        )?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("feedback rows");
    let (_, served) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(
        served["generated_at"], built["generated_at"],
        "the same build"
    );
    let after = page_rows(&served);
    assert!(
        after.iter().all(|(_, _, id, _)| *id != passed),
        "no section holds the album passed on"
    );
    assert_eq!(
        by(&hidden_artist, &after),
        Default::default(),
        "no row by the hidden artist"
    );
    assert!(
        after
            .iter()
            .any(|(key, _, id, _)| key == "fresh_releases" && *id == mbid('a', 90)),
        "an album filed under another artist stays"
    );
    for (artist, key) in &kept {
        assert_eq!(
            by(artist, &after),
            [key.to_string()].into(),
            "{key} is not about the person, so it keeps them"
        );
    }
    assert_eq!(
        home_weekly().await.expect("home's weekly playlist"),
        ["Song"],
        "home's weekly playlist leaves out the hidden artist's song"
    );
}
