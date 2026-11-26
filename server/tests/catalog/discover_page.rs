//! The discover page end to end over a real database: the first visit
//! starts a background build and answers `loading`, the built page carries
//! its shelves and is saved, a restart serves the saved page at once,
//! activity is recorded for the warm cycle, and refresh is accepted. A
//! user with Last.fm and ListenBrainz both linked gets shelves grown from
//! both, unless the server turns the blend off. Unreadable settings hold
//! every build, a page built past a failed read is rebuilt soon and keeps
//! the sections it had, each section not plainly built says why, and a
//! background warm that keeps nothing waits before it is tried again. The
//! sections that left Discover are gone, none of those left is a chart, a
//! switch saved for one changes nothing, and nothing is read for them. The
//! person's own Last.fm lists answer on their charts page instead.

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
use droppedneedle_catalog::discover::cache::{
    ForgetCounts, PageKind, Timing, UserPageCache, Variant, clear_all,
};
use droppedneedle_catalog::discover::content::LiveContent;
use droppedneedle_catalog::discover::facts::{KeptFacts, job::FactJobs};
use droppedneedle_catalog::discover::listening::shared;
use droppedneedle_catalog::discover::listening::{
    AlbumRow, ArtistRow, Ask, FreshRow, LastFmChartAlbum, ListenedAlbum, ListeningData,
    PlaylistTrack, RankedAlbum, ScoredArtist, SourceContext, UserMusic, WeeklyPlaylist,
};
use droppedneedle_catalog::discover::models::{SectionKey, SectionZone};
use droppedneedle_catalog::discover::ports::BoxFuture;
use droppedneedle_catalog::discover::scheduler::activity::ActivityDb;
use droppedneedle_catalog::discover::scheduler::{ArtistDetail, Feature, Warm, WarmScheduler};
use droppedneedle_catalog::discover::sections::{self, Basis, Page};
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
    Script, after_commits, discover_deps, discover_inputs, discover_routes, insert, keep_facts_of,
    library_artist, plays, seen_now,
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

/// `script` with a weekly playlist made the day before the rig's clock.
fn with_weekly(script: Script) -> Script {
    Script {
        weekly: Some(weekly_playlist("2023-12-31T23:00:00+00:00")),
        ..script
    }
}

fn page_app(rig: &AuthRig, dir: &std::path::Path) -> Router {
    page_parts(rig, dir).0
}

/// The page's routes and the content service behind them, for a test that
/// asks for a rebuild as the server's own triggers do.
fn page_parts(rig: &AuthRig, dir: &std::path::Path) -> (Router, Arc<LiveContent>) {
    let deps = discover_deps(rig, dir, Providers::closed(), |network| {
        *network = Arc::new(page_script());
    });
    let content = Arc::clone(&deps.content);
    (discover_routes(deps), content)
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
    keep_facts_of(&rig, Arc::new(page_script())).await;
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
    let shown: Vec<&str> = page["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .filter_map(|section| section["key"].as_str())
        .collect();
    for gone in [
        "top_picks",
        "listeners_like_you",
        "because_you_listen_to",
        "artists_you_might_like",
        "popular_in_your_genres",
        "unexplored_genres",
        "globally_trending",
        "anniversaries",
        "lastfm_recent_scrobbles",
        "lastfm_weekly_artist_chart",
        "lastfm_weekly_album_chart",
    ] {
        assert!(!shown.contains(&gone), "{gone} left Discover: {shown:?}");
    }
    let charts = sections::of(Page::Discover)
        .filter(|def| def.basis == Basis::Chart)
        .map(|def| def.key)
        .collect::<Vec<_>>();
    assert_eq!(charts, [], "no section on Discover is a chart");
    assert!(
        sections::find(Page::Home, SectionKey::Anniversaries)
            .is_some_and(|def| def.zone == SectionZone::YourLibrary),
        "Milestone Anniversaries sits in Home's library"
    );
    assert_eq!(
        content(&page, "radio_sections")["shelves"][0]["title"],
        "Radio: Seed"
    );
    assert_eq!(
        content(&page, "genre_list")["shelf"]["items"][0]["name"],
        "shoegaze"
    );

    // A restart serves the saved page straight away, built and ready.
    let restarted = page_app(&rig, &dir);
    let (_, again) = call(&restarted, "GET", "/api/v3/discover", None).await;
    assert_eq!(again["refreshing"], false, "the build finished");
    assert_eq!(
        content(&again, "radio_sections")["shelves"][0]["title"],
        "Radio: Seed",
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
        ForgetCounts::default(),
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
        ForgetCounts::default(),
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

/// The radio shelves of a page built for a user who listens on
/// ListenBrainz (primary) and Last.fm, with the blend `on` or off:
/// (title, badge, the artists its albums are by) per shelf.
async fn radio_shelves(on: bool) -> Vec<(String, String, Vec<String>)> {
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
    let artist_albums = (1..=9)
        .map(|n| ('b', n))
        .chain([('c', 1), ('c', 2)])
        .enumerate()
        .map(|(at, (prefix, n))| {
            let artist = mbid(prefix, n);
            let album = ranked(10 + at as u8, &artist, 100);
            (artist, vec![album])
        })
        .collect();
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
        artist_albums,
        ..Script::default()
    };
    let script = Arc::new(script);
    keep_facts_of(&rig, Arc::clone(&script)).await;
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |network| {
        *network = script;
    }));
    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the blended page was built").await;
    content(&page, "radio_sections")["shelves"]
        .as_array()
        .expect("radio shelves")
        .iter()
        .map(|shelf| {
            let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
            let mut by: Vec<String> = shelf["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|item| text(&item["artist_id"]))
                .collect();
            by.sort();
            by.dedup();
            (text(&shelf["title"]), text(&shelf["source"]), by)
        })
        .collect()
}

/// Blended, the seeds come from both histories with the artist both name
/// first, and the shelves carry the blended badge; each seed's similar
/// artists are ListenBrainz's one answer, with nothing of Last.fm's mixed
/// in. Turned off, only the primary service's seeds are read.
#[tokio::test]
async fn both_services_blend_unless_the_server_turns_it_off() {
    let shelf = |seed: &str, source: &str, similar: std::ops::RangeInclusive<u8>| {
        (
            format!("Radio: {seed}"),
            source.to_owned(),
            similar.map(|n| mbid('b', n)).collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        radio_shelves(true).await,
        vec![
            shelf("Both", "blended", 4..=6),
            shelf("LB Only", "blended", 1..=3),
            shelf("Last.fm Only", "blended", 7..=9),
        ]
    );
    assert_eq!(
        radio_shelves(false).await,
        vec![
            shelf("LB Only", "listenbrainz", 1..=3),
            shelf("Both", "listenbrainz", 4..=6),
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
    keep_facts_of(&rig, Arc::new(page_script())).await;
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
    let (restarted, service) = page_parts(&rig, &dir);

    service
        .trigger_refresh("user-1")
        .await
        .expect("refresh asked");
    let (_, page) = call(&restarted, "GET", "/api/v3/discover", None).await;
    assert_eq!(page["refreshing"], false, "no build started");
    assert_eq!(
        content(&page, "radio_sections")["shelves"][0]["title"],
        "Radio: Seed",
        "the saved page is served"
    );

    rig.config
        .save_secret_blocking(AdvancedSettings::default())
        .expect("settings saved again");
    service
        .trigger_refresh("user-1")
        .await
        .expect("refresh asked");
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
    keep_facts_of(&rig, Arc::new(page_script())).await;
    let script = Arc::new(with_weekly(page_script()));
    script.weekly_down.store(true, Ordering::SeqCst);
    let network = Arc::clone(&script);
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = network;
    }));
    let fresh = |page: &Value| {
        content(page, "weekly_exploration")["weekly"]["tracks"]
            .as_array()
            .map(Vec::len)
    };

    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the first page was built").await;
    assert_eq!(fresh(&page), None, "the failed shelf is left out");
    assert_eq!(
        state_of(&page, "weekly_exploration"),
        json!({"key": "weekly_exploration", "state": "source_down", "source": "listenbrainz"}),
        "and says which service failed"
    );

    script.weekly_down.store(false, Ordering::SeqCst);
    rig.clock().advance(301_000);
    let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(page["refreshing"], true, "a rebuild started");
    let page = after_commits(&rig.platform.db, "the shelf came back", || async {
        let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
        (fresh(&page) == Some(1)).then_some(page)
    })
    .await;
    assert_eq!(fresh(&page), Some(1));
    assert_eq!(state_of(&page, "weekly_exploration"), Value::Null);
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

/// A rebuild that finds no similar-artist list kept (provider data was
/// cleared and labs is down) keeps the radio shelves the page had, as
/// they were, and says when they are from; Weekly Exploration, whose
/// ListenBrainz read failed, is kept too and names ListenBrainz. A section whose reads all
/// answered with nothing says it is empty.
#[tokio::test]
async fn a_failed_read_keeps_the_sections_the_page_had() {
    let dir = crate::common::ScratchDir::new("discover-kept");
    let rig = AuthRig::new("discover-kept").await;
    seed_user(&rig.platform.db).await;
    keep_facts_of(&rig, Arc::new(page_script())).await;
    let script = Arc::new(with_weekly(page_script()));
    let network = Arc::clone(&script);
    let providers = Providers::closed();
    let answers = providers.cache.clone();
    let deps = discover_deps(&rig, &dir, providers, |slot| {
        *slot = network;
    });
    let service = Arc::clone(&deps.content);
    let app = discover_routes(deps);

    call(&app, "GET", "/api/v3/discover", None).await;
    let first = settled_page(&rig, &app, "the first page was built").await;
    let radio = content(&first, "radio_sections").clone();
    assert!(
        radio["shelves"].as_array().is_some_and(|s| !s.is_empty()),
        "{first}"
    );
    assert_eq!(state_of(&first, "radio_sections"), Value::Null);
    let fresh = content(&first, "weekly_exploration").clone();
    assert!(
        fresh["weekly"]["tracks"]
            .as_array()
            .is_some_and(|i| !i.is_empty())
    );
    assert_eq!(
        state_of(&first, "gaps_in_your_artists"),
        json!({"key": "gaps_in_your_artists", "state": "empty"}),
        "an empty library has no gaps"
    );

    script.similar_down.store(true, Ordering::SeqCst);
    script.weekly_down.store(true, Ordering::SeqCst);
    answers.clear().await.expect("provider answers cleared");
    droppedneedle_catalog::discover::facts::clear(&rig.platform.db)
        .await
        .expect("kept facts cleared");
    rig.clock().advance(60_000);
    service
        .trigger_refresh("user-1")
        .await
        .expect("refresh asked");
    let page = page_after(&app, &first, "the rebuild landed").await;
    assert_eq!(
        content(&page, "radio_sections"),
        &radio,
        "the shelves are kept as they were"
    );
    assert_eq!(
        state_of(&page, "radio_sections"),
        json!({
            "key": "radio_sections",
            "state": "kept",
            "since": first["generated_at"],
        })
    );
    assert_eq!(content(&page, "weekly_exploration"), &fresh);
    assert_eq!(
        state_of(&page, "weekly_exploration"),
        json!({
            "key": "weekly_exploration",
            "state": "kept",
            "source": "listenbrainz",
            "since": first["generated_at"],
        })
    );
}

/// For a person with nothing linked, the shelves grown from listening ask
/// for ListenBrainz, and a library shelf with nothing in it says it is
/// empty.
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
        state_of(&page, "radio_sections"),
        needs_listenbrainz("radio_sections")
    );
    assert_eq!(
        state_of(&page, "gaps_in_your_artists"),
        json!({"key": "gaps_in_your_artists", "state": "empty"})
    );
    assert_eq!(
        page["section_states"][0]["zone"], "essentials",
        "each state says where its section sits"
    );
}

/// A person with nothing linked who plays music in the app gets a radio
/// shelf for the artist they play, with the app's own badge, and the saved
/// health counts every seed they have.
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
    keep_facts_of(&rig, Arc::new(page_script())).await;
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script {
            user: UserMusic::default(),
            ..page_script()
        });
    }));

    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the page was built").await;
    let radio = &content(&page, "radio_sections")["shelves"][0];
    assert_eq!(radio["title"], "Radio: Seed", "{page}");
    assert_eq!(radio["source"], "local");
    let row: Value = serde_json::from_str(&db.one::<String>(
        "SELECT CAST(payload AS TEXT) FROM catalog_cached_pages \
         WHERE user_id = 'user-1' AND page = 'discover'",
        [],
    ))
    .expect("the saved page decodes");
    assert_eq!(row["health"]["seeds_found"], 2);
    assert_eq!(row["health"]["seeds_used"], 2);
}

/// A switch saved for a section that has left Discover changes nothing
/// until its row is carried over: the page builds as if it were not there,
/// and the weekly set reads as on though the deck it replaces was switched
/// off. A switch for a section that stays still holds. Nothing is read for
/// the sections that left: no other person's albums.
#[tokio::test]
async fn a_switch_for_a_section_that_left_changes_nothing() {
    let dir = crate::common::ScratchDir::new("discover-stale-switch");
    let rig = AuthRig::new("discover-stale-switch").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    insert(
        &db,
        "INSERT INTO app_section_prefs (user_id, page, section_key, enabled, updated_at) \
         VALUES ('user-1', 'discover', 'discover_queue', 0, 1), \
         ('user-1', 'discover', 'top_picks', 0, 1), \
         ('user-1', 'discover', 'listeners_like_you', 0, 1), \
         ('user-1', 'discover', 'genre_list', 0, 1)"
            .to_owned(),
    )
    .await;
    keep_facts_of(&rig, Arc::new(page_script())).await;
    let script = Arc::new(Script {
        user_albums: vec![listened(ranked(80, &mbid('d', 2), 99))],
        ..page_script()
    });
    let network = Arc::clone(&script);
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = network;
    }));

    call(&app, "GET", "/api/v3/discover", None).await;
    let page = settled_page(&rig, &app, "the page was built").await;
    assert_eq!(page["weekly_set_enabled"], true, "{page}");
    assert_eq!(content(&page, "genre_list"), &Value::Null);
    assert_eq!(
        content(&page, "radio_sections")["shelves"][0]["title"],
        "Radio: Seed"
    );
    assert_eq!(script.user_album_reads.load(Ordering::SeqCst), 0);
}

/// A person with Last.fm linked gets their recent scrobbles and this
/// week's top albums and artists on their charts page, an album once in
/// the scrobbles and an artist only with an id. A person without Last.fm
/// linked gets none.
#[tokio::test]
async fn the_lastfm_lists_answer_on_the_charts_page() {
    let dir = crate::common::ScratchDir::new("your-lastfm");
    let rig = AuthRig::new("your-lastfm").await;
    seed_user(&rig.platform.db).await;
    let scrobble = |name: &str| LastFmChartAlbum {
        name: name.to_owned(),
        artist_name: "Aurora".to_owned(),
        mbid: None,
        playcount: Some(3),
        image_url: format!("https://lastfm.example/{name}.png"),
    };
    let listened = |name: &str, mbid: Option<String>| ArtistRow {
        name: name.to_owned(),
        mbid,
        listen_count: Some(40),
    };
    let linked = Script {
        user: UserMusic {
            lastfm_username: Some("scrobbler".to_owned()),
            lastfm: true,
            ..UserMusic::default()
        },
        lastfm_recent: vec![scrobble("Twice"), scrobble("Twice"), scrobble("Once")],
        lastfm_weekly_albums: vec![scrobble("Top")],
        lastfm_weekly_artists: vec![
            listened("Aurora", Some(mbid('e', 1))),
            listened("No Id", None),
        ],
        ..Script::default()
    };
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(linked);
    }));
    let (status, lists) = call(&app, "GET", "/api/v3/home/your-top/lastfm", None).await;
    assert_eq!(status, StatusCode::OK, "{lists}");
    let shelves: Vec<(String, Vec<String>)> = lists["shelves"]
        .as_array()
        .expect("shelves")
        .iter()
        .map(|shelf| {
            let rows = shelf["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|item| {
                    item["title"]
                        .as_str()
                        .or(item["name"].as_str())
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
            (shelf["title"].as_str().unwrap_or_default().to_owned(), rows)
        })
        .collect();
    let owned = |list: &[&str]| list.iter().map(|row| (*row).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        shelves,
        [
            ("Recently Scrobbled".to_owned(), owned(&["Twice", "Once"])),
            ("Your Top Albums This Week".to_owned(), owned(&["Top"])),
            ("Your Weekly Top Artists".to_owned(), owned(&["Aurora"])),
        ]
    );
    assert_eq!(lists["shelves"][0]["source"], "lastfm");

    let unlinked = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script {
            lastfm_recent: vec![scrobble("Unseen")],
            ..Script::default()
        });
    }));
    let (status, none) = call(&unlinked, "GET", "/api/v3/home/your-top/lastfm", None).await;
    assert_eq!((status, none), (StatusCode::OK, json!({"shelves": []})));
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
    let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
    let scheduler = WarmScheduler::new(
        ActivityDb::new(rig.platform.db.clone()),
        ListeningData::new(
            Arc::new(Script::default()),
            Providers::closed().cache,
            facts.clone(),
        ),
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
            (
                FactJobs::new(facts, Arc::new(Script::default())).due(),
                crate::common::history_store(&rig.platform),
                crate::discover_queue::fresh_feed(&rig.platform),
            ),
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

/// One answer serves every "similar to" list: the artist page, the album
/// page (a Last.fm-first person's too) and the Weekly Mix's source give one
/// order for one artist, with fame divided out (a
/// neighbour with a lower score and far fewer listeners leads a famous
/// one, and one with no count comes last). The artist page a person opens
/// reads the list and the counts once and keeps them; every other list
/// reads the kept rows and makes no call. Once the person hides an artist
/// and says no to an album, the artist page and the album page leave them
/// out.
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
        crate::common::bundles::quiet_page_sources(&rig.platform.db, rig.platform.clock.clone()),
    );
    let name_of = |id: &str| {
        (0..NEIGHBOURS.len())
            .find(|at| neighbour_id(*at) == id)
            .map_or("?", |at| NEIGHBOURS[at].0)
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
    assert_eq!(names, RANKED, "the artist page");
    assert_eq!(
        artist_page.similar_artists[0].listen_count,
        Some(10_000),
        "a neighbour carries its own listens"
    );

    call(&app, "GET", "/api/v3/discover", None).await;
    settled_page(&rig, &app, "the page was built").await;

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
        &KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone()),
        &providers.cache,
        &providers.listenbrainz,
        SEED,
        10,
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

/// A list a person read is kept and answers later reads, past its age
/// too, with no call: only the refresh job reads it again. A labs read
/// that fails (labs no longer offers the dataset) rests labs reads, so an
/// artist with nothing kept is not asked for meanwhile.
#[tokio::test]
async fn a_kept_list_answers_and_a_failed_labs_read_rests_labs() {
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
    let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
    let read = |artist: &'static str| {
        shared::similar_artists(
            &facts,
            &providers.cache,
            &providers.listenbrainz,
            artist,
            100,
            RequestPriority::UserInitiated,
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
    let kept = read(RADIOHEAD).await.expect("the kept list answers");
    assert_eq!(kept, first);
    assert_eq!(labs_reads(), 1, "a kept list is not read again for a page");

    let other = read(SEED).await;
    assert!(other.is_err(), "the dataset is gone");
    assert_eq!(labs_reads(), 2);
    let after = read("b10bbbfc-cf9e-42e0-be17-e2c3e1d2600d").await;
    assert!(after.is_err(), "nothing kept, and the reads rest");
    assert_eq!(labs_reads(), 2, "a resting read sends nothing");
}

/// A pass over several artists a person waits on stops asking at its
/// first failed similar read: the rest answer only from kept lists.
#[tokio::test]
async fn a_similar_pass_stops_at_its_first_failed_read() {
    let rig = AuthRig::new("discover-similar-pass").await;
    let script = Arc::new(Script::default());
    script.similar_down.store(true, Ordering::SeqCst);
    let listening = ListeningData::new(
        script.clone(),
        Providers::closed().cache,
        KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone()),
    );
    let ask = Ask::build(RequestPriority::UserInitiated);
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
    let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
    let pass = || {
        shared::similar_artists_each(
            &facts,
            &providers.cache,
            &providers.listenbrainz,
            &artists,
            10,
            RequestPriority::UserInitiated,
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
    let rig = AuthRig::new("discover-count-failed").await;
    let facts = KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone());
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
            &facts,
            &providers.cache,
            &providers.listenbrainz,
            RADIOHEAD,
            3,
            RequestPriority::UserInitiated,
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

/// Every album and artist row a served page shows, as (section, kind, id,
/// artist id): shelves and shelves of several.
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
        for item in items {
            rows.push((
                key.clone(),
                text(&item["kind"]),
                text(&item["musicbrainz_id"]),
                text(&item["artist_id"]),
            ));
        }
        for row in content["new_releases"]["releases"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let album = &row["album"];
            rows.push((
                key.clone(),
                "album".to_owned(),
                text(&album["musicbrainz_id"]),
                text(&album["artist_id"]),
            ));
        }
    }
    rows
}

/// What a person tells Discover leaves the page they are served at once,
/// with no rebuild: an album they said no to goes from every section that
/// held it, and an artist they hid goes from every album shelf as a row
/// filed under them, while an album filed under another artist that only
/// credits them stays. Home's weekly exploration playlist leaves out the
/// hidden artist's song too.
#[tokio::test]
async fn what_a_person_hid_leaves_the_served_page_at_once() {
    let dir = crate::common::ScratchDir::new("discover-feedback");
    let rig = AuthRig::new("discover-feedback").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let hidden_artist = mbid('b', 3);
    let credited = mbid('f', 1);
    // One album each for the similar artists, so the radio shelf keeps
    // the one New from your artists shows too.
    let one_each = || {
        let mut script = page_script();
        for albums in script.artist_albums.values_mut() {
            albums.truncate(1);
        }
        script
    };
    let mut script = one_each();
    // New releases by artists close to the seed in the person's pool, from
    // the sitewide feed; Similar 2's album is on the radio shelf too.
    let near = |n: u8, artist: &str, name: &str| {
        let candidate = mbid('c', n);
        format!(
            "INSERT INTO catalog_discover_candidates (user_id, release_group_mbid_lower, \
             rank, kind, title, artist_mbid, artist_name, credited_artist_mbids, support, \
             group_support) VALUES ('user-1', '{candidate}', {n}, 'new_artist', 'Pooled', \
             '{artist}', '{name}', '[]', 1.0, 1.0); \
             INSERT INTO catalog_discover_candidate_paths (user_id, \
             release_group_mbid_lower, path_no, kind, source, artist_mbid, artist_name, \
             link_strength, named_by_labs, named_by_lastfm) VALUES ('user-1', \
             '{candidate}', 0, 'seed', 'listenbrainz_labs', '{SEED}', 'Seed', 1.0, 1, 0);"
        )
    };
    insert(
        &db,
        [
            near(1, &credited, "Similar 3 & Friends"),
            near(2, &mbid('b', 2), "Similar 2"),
            near(3, &hidden_artist, "Hidden"),
        ]
        .concat(),
    )
    .await;
    let fresh = |group: String, artist: &str, name: &str| FreshRow {
        release_group_mbid: group,
        title: "New Album".to_owned(),
        artist_name: name.to_owned(),
        artist_mbids: vec![artist.to_owned()],
        release_date: "2023-12-20".to_owned(),
        primary_type: Some("Album".to_owned()),
        secondary_type: None,
    };
    script.fresh = vec![
        fresh(mbid('a', 90), &credited, "Similar 3 & Friends"),
        fresh(mbid('a', b'2'), &mbid('b', 2), "Similar 2"),
        fresh(mbid('a', 91), &hidden_artist, "Hidden"),
    ];
    let mut weekly = weekly_playlist("2023-12-31T23:00:00+00:00");
    let mut by_hidden_song = weekly.tracks[0].clone();
    by_hidden_song.title = "Hidden Song".to_owned();
    by_hidden_song.artist_mbid = Some(hidden_artist.clone());
    weekly.tracks.push(by_hidden_song);
    script.weekly = Some(weekly);
    keep_facts_of(&rig, Arc::new(one_each())).await;
    seen_now(&rig, "user-1").await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |network| {
        *network = Arc::new(script);
    });
    assert!(deps.fresh.refresh().await, "the sitewide releases are read");
    let app = discover_routes(deps);
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
    assert_eq!(
        shown,
        ["new_from_your_artists", "radio_sections"]
            .map(str::to_owned)
            .into(),
        "the artist is on two album shelves"
    );
    assert!(
        before
            .iter()
            .any(|(key, _, id, artist)| key == "new_from_your_artists"
                && *id == mbid('a', 90)
                && *artist == credited),
        "the album that only credits them is shown"
    );

    let ids = [passed.clone(), hidden_artist.clone()];
    db.write(Lane::Foreground, "test.feedback", move |tx| {
        tx.execute(
            "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
             release_group_mbid, created_at) VALUES ('user-1', 'album', ?1, 'no', ?1, 1), \
             ('user-1', 'artist', ?2, 'not_artist', '', 1)",
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
            .any(|(key, _, id, _)| key == "new_from_your_artists" && *id == mbid('a', 90)),
        "an album filed under another artist stays"
    );
    assert_eq!(
        home_weekly().await.expect("home's weekly playlist"),
        ["Song"],
        "home's weekly playlist leaves out the hidden artist's song"
    );
}
