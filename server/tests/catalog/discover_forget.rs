//! "Forget my history" over a real database and Discover's routes as the
//! production builder wires them: one person's rows go from every table
//! `server/forget-coverage.txt` lists as forgotten, another person's stay,
//! every kept table is left as it was, a forget cut off after its first
//! step is finished by asking again, and the person's Discover page is
//! built again. A build or a history import of theirs running across the
//! forget keeps nothing, and another person's is left alone.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_catalog::discover::cache::{
    self, ForgetCounts, Generation, PageKind, Timing, UserPageCache, Variant,
};
use droppedneedle_catalog::discover::record::{self, Evidence, MadeFrom, Offer};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_providers::Providers;
use tower::ServiceExt as _;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_page::page_script;
use crate::discover_queue::{after_commits, discover_deps, discover_routes, insert};

const DAY: i64 = 86_400_000;
const FORGET: &str = "/api/v3/discover/history";

/// The tables `server/forget-coverage.txt` lists as `class`.
fn listed(class: &str) -> Vec<&'static str> {
    include_str!("../../forget-coverage.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, rest) = line.split_once(' ')?;
            let listed = rest.split(':').next().unwrap_or("").trim();
            (listed == class).then_some(name)
        })
        .collect()
}

/// One row of `user`'s in the forgotten `table`, written at `at`, and the
/// column that dates it. A table listed as forgotten needs its row here and
/// its step in `discover/forget.rs`.
fn forgotten_row(table: &str, user: &str, at: i64) -> (String, &'static str) {
    match table {
        "catalog_cached_pages" => (
            format!(
                "INSERT INTO catalog_cached_pages (user_id, page, source, listenbrainz, \
                 lastfm, payload, built_at) VALUES ('{user}', 'home', 'official', 0, 0, \
                 CAST('{{}}' AS BLOB), {at});"
            ),
            "built_at",
        ),
        "catalog_discover_picks" => (
            format!(
                "INSERT INTO catalog_discover_picks (user_id, release_group_mbid_lower, \
                 release_group_mbid, first_surface, first_offered_at, last_offered_at, \
                 evidence) VALUES ('{user}', 'eeeeeeee-0000-4000-8000-000000000001', \
                 'eeeeeeee-0000-4000-8000-000000000001', 'deck', {at}, {at}, '{{}}');"
            ),
            "first_offered_at",
        ),
        "catalog_discover_activity" => (
            format!(
                "INSERT INTO catalog_discover_activity (user_id, feature, artist_mbid, \
                 section, provider, source, used_at) VALUES ('{user}', 'artist', \
                 'cccccccc-0000-4000-8000-000000000001', 'similar', 'listenbrainz', \
                 'official', {at});"
            ),
            "used_at",
        ),
        "media_playback_events" => (
            format!(
                "INSERT INTO media_playback_events (id, user_id, track_name, artist_name, \
                 source, client, started_at, ended_at, listened_ms, ended_by, came_from, \
                 counted) VALUES ('event-{user}', '{user}', 'Song', 'Someone', 'local', \
                 'web', {at}, {at}, 1000, 'skipped', 'album', 1);"
            ),
            "ended_at",
        ),
        "media_imported_listens" => (
            format!(
                "INSERT INTO media_imported_listens (user_id, listened_at, artist_name, \
                 track_name, imported_at) VALUES ('{user}', {at}, 'Someone', 'Song', {at});"
            ),
            "imported_at",
        ),
        "media_history_imports" => (
            format!(
                "INSERT INTO media_history_imports (user_id, state, source, \
                 last_synced_at, updated_at) VALUES ('{user}', 'current', 'upload', \
                 {at}, {at});"
            ),
            "last_synced_at",
        ),
        other => panic!("{other} is listed as forgotten: give it a row here and a step"),
    }
}

fn count(db: &Db, sql: &str) -> i64 {
    db.one::<i64>(sql, [])
}

async fn call(app: &Router, method: &str, uri: &str) -> StatusCode {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("request builds");
    app.clone()
        .oneshot(request)
        .await
        .expect("router answers")
        .status()
}

async fn forget(app: &Router) -> StatusCode {
    call(app, "DELETE", FORGET).await
}

/// `user-1`'s saved Discover page, built at or after `since`.
async fn page_built(db: &Db, since: i64, what: &str) {
    after_commits(db, what, || async {
        (count(
            db,
            &format!(
                "SELECT COUNT(*) FROM catalog_cached_pages WHERE user_id = 'user-1' \
                 AND page = 'discover' AND built_at >= {since}"
            ),
        ) == 1)
            .then_some(())
    })
    .await;
}

#[tokio::test]
async fn forgetting_takes_the_callers_history_and_nothing_else() {
    let dir = crate::common::ScratchDir::new("discover-forget");
    let rig = AuthRig::new("discover-forget").await;
    let db = rig.platform.db.clone();
    insert(
        &db,
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0)"
            .to_owned(),
    )
    .await;
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |network| {
        *network = Arc::new(page_script());
    }));

    // A first visit builds and saves the page, which offers its picks.
    assert_eq!(call(&app, "GET", "/api/v3/discover").await, StatusCode::OK);
    let start = rig.clock().now_ms();
    page_built(&db, start, "the first page was saved").await;
    assert!(
        count(
            &db,
            "SELECT COUNT(*) FROM catalog_discover_picks WHERE user_id = 'user-1'"
        ) > 0,
        "the page offered its picks"
    );

    // A row in every forgotten table for both people, and something kept.
    let forgotten = listed("forgotten");
    let mut seed = String::new();
    let mut dated = Vec::new();
    for table in &forgotten {
        for user in ["user-1", "user-2"] {
            let (row, column) = forgotten_row(table, user, start);
            seed.push_str(&row);
            if user == "user-1" {
                dated.push((*table, column));
            }
        }
    }
    seed.push_str(
        "INSERT INTO library_play_history (id, user_id, track_name, artist_name, played_at) \
         VALUES ('play-1', 'user-1', 'Song', 'Someone', 1); \
         INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'album', 'dddddddd-0000-4000-8000-000000000001', \
         'no', 1);",
    );
    insert(&db, seed).await;
    let kept = listed("kept");
    let kept_counts = || -> Vec<i64> {
        kept.iter()
            .map(|table| count(&db, &format!("SELECT COUNT(*) FROM {table}")))
            .collect()
    };
    let kept_before = kept_counts();
    let user_rows = |table: &str, user: &str| {
        count(
            &db,
            &format!("SELECT COUNT(*) FROM {table} WHERE user_id = '{user}'"),
        )
    };
    let pages_before = user_rows("catalog_cached_pages", "user-1");

    // Cut off after its second step: the playback events and the imported
    // listens are gone, and the picks, the activity and the saved pages are
    // all still there.
    insert(
        &db,
        "CREATE TRIGGER test_hold_picks BEFORE DELETE ON catalog_discover_picks \
         BEGIN SELECT RAISE(ABORT, 'held'); END;"
            .to_owned(),
    )
    .await;
    assert_eq!(forget(&app).await, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(user_rows("media_playback_events", "user-1"), 0);
    assert_eq!(user_rows("media_imported_listens", "user-1"), 0);
    assert!(user_rows("catalog_discover_picks", "user-1") > 0);
    assert_eq!(user_rows("catalog_discover_activity", "user-1"), 1);
    assert_eq!(user_rows("catalog_cached_pages", "user-1"), pages_before);
    insert(&db, "DROP TRIGGER test_hold_picks".to_owned()).await;

    // Asked again a day later, it finishes, and the page is built again.
    rig.clock().advance(DAY);
    let forgot_at = rig.clock().now_ms();
    assert_eq!(forget(&app).await, StatusCode::NO_CONTENT);
    page_built(&db, forgot_at, "the page was built again").await;

    for (table, column) in dated {
        assert_eq!(
            count(
                &db,
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE user_id = 'user-1' \
                     AND {column} < {before}",
                    before = forgot_at - DAY / 2
                )
            ),
            0,
            "user-1's rows from before the forget are gone from {table}"
        );
        assert_eq!(
            user_rows(table, "user-2"),
            1,
            "user-2's row in {table} stays"
        );
    }
    assert_eq!(kept_counts(), kept_before, "every kept table is as it was");
    assert_eq!(user_rows("library_play_history", "user-1"), 1);
    assert_eq!(user_rows("catalog_discover_feedback", "user-1"), 1);

    // Asking once more, with nothing left of before, is still a success.
    assert_eq!(forget(&app).await, StatusCode::NO_CONTENT);
}

/// One Top Picks offer by a build started under `built`.
async fn offer(db: &Db, user: &str, built: Generation, forgets: &ForgetCounts) {
    let offer = Offer {
        release_group_mbid: "eeeeeeee-0000-4000-8000-000000000002".to_owned(),
        artist_mbid: String::new(),
        album_name: "Album".to_owned(),
        artist_name: "Someone".to_owned(),
        evidence: Evidence {
            version: 1,
            reasons: Vec::new(),
            seed_artist: None,
            chart: false,
            service: None,
        },
    };
    record::offered(
        db,
        user,
        MadeFrom::TopPicks,
        vec![offer],
        1,
        record::Built {
            generation: built,
            forgets,
        },
    )
    .await;
}

/// A build claimed before a person's forget keeps no page and records no
/// pick for them; a build for someone else running across it saves and
/// records, and one of theirs claimed after it saves and records.
#[tokio::test]
async fn a_forget_stops_only_that_persons_running_builds() {
    let rig = AuthRig::new("discover-forget-builds").await;
    let db = rig.platform.db.clone();
    insert(
        &db,
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0)"
            .to_owned(),
    )
    .await;
    let forgets = ForgetCounts::default();
    let pages: UserPageCache<String> = UserPageCache::new(
        db.clone(),
        rig.platform.clock.clone(),
        PageKind::Discover,
        Timing {
            fresh_ms: 300_000,
            retry_ms: 300_000,
        },
        forgets.clone(),
    );
    let built_for = Variant {
        source: "official".to_owned(),
        listenbrainz: false,
        lastfm: false,
    };
    let ada = pages.claim("user-1").await.expect("Ada's build");
    let bob = pages.claim("user-2").await.expect("Bob's build");
    let (ada_built, bob_built) = (ada.generation(), bob.generation());
    let picks = |user: &str| {
        db.one::<i64>(
            "SELECT COUNT(*) FROM catalog_discover_picks WHERE user_id = ?1",
            [user],
        )
    };

    cache::forget_user(&db, &forgets, "user-1")
        .await
        .expect("Ada forgets");
    offer(&db, "user-1", ada_built, &forgets).await;
    offer(&db, "user-2", bob_built, &forgets).await;
    assert_eq!(
        picks("user-1"),
        0,
        "Ada's build from before records no pick"
    );
    assert_eq!(picks("user-2"), 1, "Bob's build records his pick");
    pages
        .run(ada, &built_for, async { Some("from before".to_owned()) })
        .await;
    pages
        .run(bob, &built_for, async { Some("Bob's page".to_owned()) })
        .await;
    assert!(
        pages.load("user-1", &built_for).await.is_none(),
        "Ada's build from before the forget kept nothing"
    );
    assert_eq!(
        pages
            .load("user-2", &built_for)
            .await
            .map(|page| page.value),
        Some("Bob's page".to_owned()),
        "Bob's build was left alone"
    );

    let after = pages.claim("user-1").await.expect("a build after");
    offer(&db, "user-1", after.generation(), &forgets).await;
    assert_eq!(picks("user-1"), 1, "a build after the forget records");
    pages
        .run(after, &built_for, async { Some("after".to_owned()) })
        .await;
    assert_eq!(
        pages
            .load("user-1", &built_for)
            .await
            .map(|page| page.value),
        Some("after".to_owned()),
        "a build claimed after the forget is kept"
    );
}

/// A history import claimed before a person's forget writes nothing after
/// it, and their queued import is cancelled; another person's import
/// running across the forget keeps its listens. Nothing more is read for
/// the person who forgot.
#[tokio::test]
async fn a_forget_stops_only_that_persons_running_import() {
    use droppedneedle_media::history::{self, store::WriteError};
    use droppedneedle_providers::listenbrainz::history::HistoryListen;

    let rig = AuthRig::new("discover-forget-import").await;
    let db = rig.platform.db.clone();
    insert(
        &db,
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0); \
         INSERT INTO media_history_imports (user_id, state, source, import_id, updated_at) \
         VALUES ('user-1', 'importing', 'upload', 'ada', 0), \
                ('user-2', 'importing', 'upload', 'bob', 0); \
         INSERT INTO durable_jobs (kind, idempotency_key, state, not_before_at, \
         payload_json, created_at, updated_at) \
         VALUES ('listens.import', 'user-1', 'queued', 0, '{}', 0, 0), \
                ('listens.import', 'user-2', 'queued', 0, '{}', 0, 0);"
            .to_owned(),
    )
    .await;
    let store = crate::common::history_store(&rig.platform);
    let ada = store
        .reading("user-1")
        .await
        .expect("read")
        .expect("Ada's run");
    let bob = store
        .reading("user-2")
        .await
        .expect("read")
        .expect("Bob's run");
    // Before the clock the rig starts on, 2024-01-01.
    let listen = HistoryListen {
        listened_at: 1_700_000_000,
        track_name: "Song".to_owned(),
        artist_name: "Someone".to_owned(),
        release_name: None,
        recording_mbid: None,
        release_mbid: None,
        artist_mbid: None,
    };

    history::forget(&store, "user-1")
        .await
        .expect("Ada forgets");
    let late = store
        .write_month("user-1", &ada, vec![listen.clone()], false, |_| Ok(()))
        .await;
    assert!(matches!(late, Err(WriteError::MovedOn)), "{late:?}");
    let kept = store
        .write_month("user-2", &bob, vec![listen], false, |_| Ok(()))
        .await;
    assert!(matches!(kept, Ok(1)), "{kept:?}");

    let count = |sql: &str, user: &str| db.one::<i64>(sql, [user]);
    let listens = "SELECT COUNT(*) FROM media_imported_listens WHERE user_id = ?1";
    let queued = "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'listens.import' \
                  AND idempotency_key = ?1";
    assert_eq!(count(listens, "user-1"), 0, "Ada's run kept nothing");
    assert_eq!(count(listens, "user-2"), 1, "Bob's run kept his listen");
    assert_eq!(
        count(queued, "user-1"),
        0,
        "Ada's queued import is cancelled"
    );
    assert!(count(queued, "user-2") > 0, "Bob's import stays queued");
    assert!(
        store.reading("user-1").await.expect("read").is_none(),
        "nothing more is read for Ada"
    );
    assert_eq!(
        db.one::<String>(
            "SELECT state FROM media_history_imports WHERE user_id = ?1",
            ["user-1"]
        ),
        "forgotten"
    );
}
