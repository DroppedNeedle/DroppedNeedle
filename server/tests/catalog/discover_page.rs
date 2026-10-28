//! The discover page end to end over a real database: the first visit
//! starts a background build and answers `loading`, the built page carries
//! its shelves and is saved, a restart serves the saved page at once,
//! activity is recorded for the warm cycle, and refresh is accepted.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle_catalog::reads::discover::cache::{
    PageKind, Timing, UserPageCache, Variant, clear_all,
};
use droppedneedle_catalog::reads::discover::listening::{
    AlbumRow, ArtistRow, ListenedAlbum, RankedAlbum, ScoredArtist, SourceContext, UserMusic,
};
use droppedneedle_kernel::db::Lane;
use droppedneedle_providers::Providers;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::AuthRig;
use crate::discover_queue::{Script, after_commits, discover_deps, discover_routes};

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
                listen_count: i64::from(n) * 100,
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
            listen_count: 50,
        }],
        fresh: vec![ranked(90, &mbid('f', 1), 0).album],
        sitewide_artists: vec![ArtistRow {
            name: "Everyone's Favourite".to_owned(),
            mbid: Some(mbid('c', 1)),
            listen_count: 9000,
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
                 VALUES ('user-1', 'Ada', 'user', '2026-01-01T00:00:00Z')",
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
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM catalog_cached_pages \
             WHERE user_id = 'user-1' AND page = 'discover' AND listenbrainz = 1 AND lastfm = 0",
        )
        .fetch_one(db.sqlx())
        .await
        .expect("snapshot count")
    };
    after_commits(&db, "the built page was saved", || async {
        (saved().await == 1).then_some(())
    })
    .await;
    let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
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
    let recorded: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM catalog_discover_activity \
         WHERE user_id = 'user-1' AND feature = 'discover'",
    )
    .fetch_one(db.sqlx())
    .await
    .expect("activity count");
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
                 VALUES ('user-1', 'Ada', 'user', '2026-01-01T00:00:00Z')",
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

/// A build whose save hears a busy database is not a failed build: its
/// page is served at once and written on a later visit.
#[tokio::test]
async fn a_busy_save_keeps_the_built_page() {
    let rig = AuthRig::new("discover-busy-save").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let cache = page_cache(&rig);
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
        Some("page")
    );
    assert!(!cache.due("user-1", served.as_ref()), "no rebuild, no wait");

    held.execute_batch("ROLLBACK").expect("lock released");
    drop(held);
    assert!(cache.load("user-1", &built_for()).await.is_some());
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_cached_pages")
        .fetch_one(db.sqlx())
        .await
        .expect("rows counted");
    assert_eq!(rows, 1, "the next visit writes the page");
}

/// Clearing the cache wins over a build already running: what it built
/// is not kept, and a failed build's wait is dropped.
#[tokio::test]
async fn a_clear_wins_over_a_running_build() {
    let rig = AuthRig::new("discover-clear-build").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let cache = page_cache(&rig);

    let claim = cache.claim("user-1").await.expect("first build");
    assert!(
        !cache
            .run(claim, &built_for(), async { None::<String> })
            .await
    );
    assert!(!cache.due("user-1", None), "a failed build waits");
    let claim = {
        clear_all(&db).await.expect("cleared");
        assert!(cache.load("user-1", &built_for()).await.is_none());
        assert!(cache.due("user-1", None), "the clear drops the wait");
        cache.claim("user-1").await.expect("a build")
    };
    clear_all(&db).await.expect("cleared again");
    cache
        .run(claim, &built_for(), async { Some("stale".to_owned()) })
        .await;
    assert!(
        cache.load("user-1", &built_for()).await.is_none(),
        "a build claimed before the clear keeps nothing"
    );
}

/// A page saved in an older shape is dropped on its first load, so it is
/// rebuilt and never logged again.
#[tokio::test]
async fn a_page_in_an_old_shape_is_dropped() {
    let rig = AuthRig::new("discover-old-shape").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let old = page_cache(&rig);
    let claim = old.claim("user-1").await.expect("first build");
    old.run(claim, &built_for(), async { Some("old shape".to_owned()) })
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
    let loaded = new.load("user-1", &built_for()).await;
    assert!(loaded.is_none(), "an old shape reads as missing");
    assert!(new.due("user-1", loaded.as_ref()), "and is rebuilt");
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_cached_pages")
        .fetch_one(db.sqlx())
        .await
        .expect("rows counted");
    assert_eq!(rows, 0, "the old row is gone");
}
