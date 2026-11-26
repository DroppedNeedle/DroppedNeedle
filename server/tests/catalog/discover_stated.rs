//! Artists a person names as ones they love, over a real database and the
//! production builder: each is a seed their taste and pool grow from, known
//! to them so never offered as a pick, weighed down as they play it, gone
//! from the seeds once removed, at most 30 a person and never a
//! special-purpose id. Another person's list is never touched.

use std::sync::Arc;

use droppedneedle_catalog::discover::facts::KeptFacts;
use droppedneedle_catalog::discover::listening::ListeningData;
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_catalog::discover::stated::StatedError;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Db;
use droppedneedle_providers::Providers;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_pool::{album, albums, counted, id, list};
use crate::discover_queue::{Script, discover_deps, insert};

const DAY: i64 = 86_400_000;
const MINUTE: i64 = 60_000;
const VARIOUS_ARTISTS: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

const USERS: &str = "INSERT INTO auth_users (id, display_name, role, created_at) \
     VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0);";

async fn name(deps: &DiscoverDeps, user: &str, artist: &str) {
    deps.stated
        .name(user, artist, &format!("Artist {}", &artist[..8]))
        .await
        .expect("named");
}

async fn taste(deps: &DiscoverDeps, user: &str) {
    deps.taste
        .run_once(user, |_| Ok(()))
        .await
        .expect("the taste job runs");
}

fn named(db: &Db, user: &str) -> Vec<String> {
    db.column(
        "SELECT artist_mbid_lower FROM catalog_stated_artists WHERE user_id = ?1 \
         ORDER BY stated_at, artist_mbid_lower",
        [user],
    )
}

fn seeds(db: &Db, user: &str) -> Vec<String> {
    db.column(
        "SELECT artist_mbid_lower FROM catalog_taste_artists WHERE user_id = ?1 \
         AND is_seed = 1 ORDER BY artist_mbid_lower",
        [user],
    )
}

fn weight(db: &Db, user: &str, artist: &str) -> f64 {
    db.one(
        "SELECT weight FROM catalog_taste_artists WHERE user_id = ?1 \
         AND artist_mbid_lower = ?2",
        [user, artist],
    )
}

/// Whether `user`'s taste build is queued, clearing it.
async fn taste_queued(db: &Db, user: &str) -> bool {
    let queued: i64 = db.one(
        "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'discover.taste' \
         AND idempotency_key = ?1 AND state = 'queued'",
        [user],
    );
    insert(
        db,
        "DELETE FROM durable_jobs WHERE kind = 'discover.taste'".to_owned(),
    )
    .await;
    queued > 0
}

/// `count` listens imported for `user` of `artist`, the latest at `at`.
fn listens(user: &str, artist: &str, count: i64, at: i64) -> String {
    (0..count)
        .map(|n| {
            format!(
                "INSERT INTO media_imported_listens (user_id, listened_at, artist_mbid, \
                 artist_name, track_name, imported_at) \
                 VALUES ('{user}', {}, '{artist}', 'Named', 'Song {n}', 1);",
                at - MINUTE * n
            )
        })
        .collect()
}

/// Ada has no plays, hearts or follows. She names three artists: each
/// write queues her taste build, and the three are her seeds. The build
/// wants their similar-artist lists; once those are kept her pool holds
/// albums of the artists they point at, never one by an artist she named.
/// Removing one takes its seed with it. Bob's list is his own.
#[tokio::test]
async fn discover_stated_artists_seed_a_person_with_nothing_else() {
    let dir = crate::common::ScratchDir::new("discover-stated");
    let rig = AuthRig::new("discover-stated").await;
    let db = rig.platform.db.clone();
    insert(&db, USERS.to_owned()).await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script::default());
    });
    let (a1, a2, a3) = (id(1, 1), id(1, 2), id(1, 3));
    let (n1, n2) = (id(2, 1), id(2, 2));
    let bobs = id(1, 9);

    name(&deps, "user-2", &bobs).await;
    assert!(taste_queued(&db, "user-2").await);
    for artist in [&a1, &a2, &a3] {
        name(&deps, "user-1", artist).await;
        assert!(taste_queued(&db, "user-1").await, "naming queues her taste");
        rig.clock().advance(1_000);
    }
    name(&deps, "user-1", &a1).await;
    assert!(
        !taste_queued(&db, "user-1").await,
        "naming one again changes nothing"
    );
    let listed: Vec<String> = deps
        .stated
        .list("user-1")
        .await
        .expect("the list reads")
        .into_iter()
        .map(|artist| artist.artist_mbid)
        .collect();
    assert_eq!(
        listed,
        [a1.clone(), a2.clone(), a3.clone()],
        "in order named"
    );

    let listening = ListeningData::new(
        Arc::new(Script::default()),
        Providers::closed().cache,
        KeptFacts::new(db.clone(), rig.platform.clock.clone()),
    );
    let page_seeds = droppedneedle_catalog::test_support::seed_artists(
        &listening,
        &db,
        "user-1",
        rig.clock().now_ms(),
    )
    .await;
    assert_eq!(page_seeds, [a1.clone(), a2.clone(), a3.clone()]);

    taste(&deps, "user-1").await;
    assert_eq!(seeds(&db, "user-1"), [a1.clone(), a2.clone(), a3.clone()]);
    let wanted: i64 = db.one(
        "SELECT COUNT(*) FROM catalog_fact_fetches WHERE kind = 'neighbours' \
         AND wanted_at IS NOT NULL",
        [],
    );
    assert_eq!(wanted, 3, "her three lists are wanted");

    // The facts arrive: A1 points at A2, whom she named, and at N1; A2 and
    // A3 point at N1 and N2. Every one of them has an album.
    let at = rig.clock().now_ms() - DAY;
    let mut rows = list(&a1, &[(a2.clone(), 100.0), (n1.clone(), 90.0)], at);
    rows += &list(&a2, &[(n1.clone(), 100.0)], at);
    rows += &list(&a3, &[(n2.clone(), 100.0)], at);
    for (n, artist) in [&a1, &a2, &a3, &n1, &n2].into_iter().enumerate() {
        rows += &counted(artist, 10_000, at);
        let group = id(3, n);
        rows += &albums(artist, &[album(&group, "Album", "[]", &[artist])], at);
    }
    insert(&db, rows).await;
    taste(&deps, "user-1").await;
    deps.pool
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the pool job runs");
    let offered: Vec<String> = db.column(
        "SELECT artist_mbid FROM catalog_discover_candidates WHERE user_id = 'user-1' \
         ORDER BY artist_mbid",
        [],
    );
    assert_eq!(
        offered,
        [n1, n2],
        "the artists they point at, never one she named"
    );

    deps.stated.remove("user-1", &a3).await.expect("removed");
    assert!(
        taste_queued(&db, "user-1").await,
        "removing queues her taste"
    );
    deps.stated
        .remove("user-1", &a3)
        .await
        .expect("removed again");
    assert!(!taste_queued(&db, "user-1").await, "nothing left to take");
    taste(&deps, "user-1").await;
    assert_eq!(seeds(&db, "user-1"), [a1, a2], "its seed went with it");
    assert_eq!(named(&db, "user-2"), [bobs], "Bob's list is his");
}

/// A named artist starts at 2.0 for a person with no other seed. Plays of
/// it add their own weight, and from the 50th what she said counts for
/// half: Ada's named artist weighs Bob's same listens plus 2.0 at 49 and
/// plus 1.0 at 50.
#[tokio::test]
async fn discover_stated_artist_weighs_half_after_50_plays() {
    let dir = crate::common::ScratchDir::new("discover-stated-plays");
    let rig = AuthRig::new("discover-stated-plays").await;
    let db = rig.platform.db.clone();
    insert(&db, USERS.to_owned()).await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script::default());
    });
    let artist = id(1, 1);
    name(&deps, "user-1", &artist).await;
    taste(&deps, "user-1").await;
    assert!((weight(&db, "user-1", &artist) - 2.0).abs() < 1e-9);

    let at = rig.clock().now_ms() - DAY;
    let both = listens("user-1", &artist, 49, at) + &listens("user-2", &artist, 49, at);
    insert(&db, both).await;
    for user in ["user-1", "user-2"] {
        taste(&deps, user).await;
    }
    let said = weight(&db, "user-1", &artist) - weight(&db, "user-2", &artist);
    assert!((said - 2.0).abs() < 1e-9, "at 49 plays: {said}");

    let one_more =
        listens("user-1", &artist, 1, at + MINUTE) + &listens("user-2", &artist, 1, at + MINUTE);
    insert(&db, one_more).await;
    for user in ["user-1", "user-2"] {
        taste(&deps, user).await;
    }
    let said = weight(&db, "user-1", &artist) - weight(&db, "user-2", &artist);
    assert!((said - 1.0).abs() < 1e-9, "at 50 plays: {said}");
}

/// Thirty is the most one person names: the 31st is refused while naming
/// one already there still answers. A special-purpose id or one that is
/// not an id is refused. Bob's list stays empty throughout.
#[tokio::test]
async fn discover_stated_refuses_the_31st_and_special_ids() {
    let dir = crate::common::ScratchDir::new("discover-stated-limit");
    let rig = AuthRig::new("discover-stated-limit").await;
    let db = rig.platform.db.clone();
    insert(&db, USERS.to_owned()).await;
    let deps = discover_deps(&rig, &dir, Providers::closed(), |slot| {
        *slot = Arc::new(Script::default());
    });
    for n in 0..30 {
        name(&deps, "user-1", &id(1, n)).await;
    }
    let refused = deps.stated.name("user-1", &id(1, 30), "Thirty-one").await;
    assert!(matches!(refused, Err(StatedError::Full)), "{refused:?}");
    name(&deps, "user-1", &id(1, 0)).await;
    assert_eq!(named(&db, "user-1").len(), 30);

    for artist in [VARIOUS_ARTISTS, "not-an-id"] {
        let refused = deps.stated.name("user-2", artist, "Someone").await;
        assert!(
            matches!(refused, Err(StatedError::Invalid(_))),
            "{artist}: {refused:?}"
        );
    }
    assert!(named(&db, "user-2").is_empty());
}
