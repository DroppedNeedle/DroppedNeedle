//! How far picks go, over a real database and the production builder: next
//! door orders the pool and deals the set exactly as by support alone, home
//! ground and far out give other eights whose artists' listeners fall from
//! one to the next, every gate holds at each, and a new depth queues one
//! pool build in its own write and never touches the week's set, which
//! records the depth it was dealt at.

use std::collections::HashSet;
use std::sync::Arc;

use droppedneedle_catalog::discover::engine::pool_store;
use droppedneedle_catalog::discover::engine::select::{self, Exposure, Floors, Share};
use droppedneedle_catalog::discover::engine::weekly::{self, Depth, SetDay, WEEK_MS};
use droppedneedle_catalog::discover::models::DiscoverPrefs;
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Db;
use droppedneedle_providers::Providers;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_pool::{album, albums, counted, id, list};
use crate::discover_queue::{Script, discover_deps, insert, own};

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;
const SEEDS: usize = 4;
const NEIGHBOURS: usize = 12;
/// Listeners of neighbour `j`, by `j % 3`.
const TIERS: [i64; 3] = [1_000, 50_000, 2_500_000];
/// Seed 1's neighbour counted with no listeners.
const ZERO: usize = 50;
/// Seed 2's neighbour whose listeners are not counted.
const UNKNOWN: usize = 51;

fn seed(s: usize) -> String {
    id(1, s)
}

fn artist(s: usize, j: usize) -> String {
    id(2, s * 100 + j)
}

fn group(s: usize, j: usize) -> String {
    id(3, s * 100 + j)
}

/// Ada's taste build at `at`: four seeds of one weight in one group.
fn taste(at: i64) -> String {
    let mut rows = format!(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0); \
         INSERT INTO auth_tokens (id, user_id, token_hash, issued_at, expires_at, \
         last_seen_at, revoked) VALUES ('t1', 'user-1', 't1', 0, {end}, {at}, 0); \
         INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, history_from_at, history_to_at, \
         incomplete) VALUES ('user-1', {at}, 400, 0, {SEEDS}, {SEEDS}, {from}, {at}, 0); \
         INSERT INTO catalog_taste_groups (user_id, group_no, anchor_artist_mbid, \
         anchor_name, share, recent_share, trend) VALUES ('user-1', 0, '{anchor}', 'Anchor', \
         1.0, 1.0, 'steady');",
        end = at + 365 * DAY,
        from = at - 400 * DAY,
        anchor = seed(0),
    );
    for s in 0..SEEDS {
        rows += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed, group_no) \
             VALUES ('user-1', '{id}', '{id}', 'Seed {s}', 5, 0, 1, 1.0, 1, 0);",
            id = seed(s)
        );
    }
    rows
}

/// Every seed's list: twelve neighbours of its own, neighbour `j` with
/// strength `0.99 - 0.02 j` once its fame is divided out and the listeners
/// of its tier, each with one counted album. Seed 0's list leads with
/// three more whose albums no depth may offer: one owned, one told no and
/// a single. Seed 1 also names one counted with no listeners, well below
/// its best, and seed 2 one not counted at all.
fn lists(at: i64) -> String {
    let mut rows = String::new();
    for s in 0..SEEDS {
        let mut neighbours = Vec::new();
        if s == 0 {
            for j in 90..93 {
                neighbours.push((artist(0, j), (TIERS[2] as f64).sqrt()));
            }
        }
        for j in 0..NEIGHBOURS {
            let strength = 0.99 - 0.02 * j as f64;
            neighbours.push((artist(s, j), strength * (TIERS[j % 3] as f64).sqrt()));
        }
        if s == 1 {
            // Counted with no listeners, at a strength of about 0.39.
            neighbours.push((artist(1, ZERO), 0.385));
            rows += &counted(&artist(1, ZERO), 0, at);
            let own_group = group(1, ZERO);
            rows += &albums(
                &artist(1, ZERO),
                &[album(&own_group, "Album", "[]", &[&artist(1, ZERO)])],
                at,
            );
        }
        if s == 2 {
            // Not counted: labs files it under the last counted neighbour
            // (0.77 of the best 0.99), here at a strength of 0.75.
            let best = 0.95 * (TIERS[2] as f64).sqrt();
            neighbours.push((artist(2, UNKNOWN), 0.75 * best * 0.99 / 0.77));
            let own_group = group(2, UNKNOWN);
            rows += &albums(
                &artist(2, UNKNOWN),
                &[album(&own_group, "Album", "[]", &[&artist(2, UNKNOWN)])],
                at,
            );
        }
        rows += &list(&seed(s), &neighbours, at);
        for j in 0..NEIGHBOURS {
            rows += &counted(&artist(s, j), TIERS[j % 3], at);
            let own_group = group(s, j);
            rows += &albums(
                &artist(s, j),
                &[album(&own_group, "Album", "[]", &[&artist(s, j)])],
                at,
            );
        }
    }
    for j in 90..93 {
        rows += &counted(&artist(0, j), TIERS[2], at);
        let primary = if j == 92 { "Single" } else { "Album" };
        let gated = group(0, j);
        rows += &albums(
            &artist(0, j),
            &[album(&gated, primary, "[]", &[&artist(0, j)])],
            at,
        );
    }
    rows += &format!(
        "INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'album', '{}', 'no', 1);",
        group(0, 91)
    );
    rows
}

fn deps(rig: &AuthRig, dir: &std::path::Path) -> DiscoverDeps {
    let script = Arc::new(Script::default());
    discover_deps(rig, dir, Providers::closed(), move |slot| *slot = script)
}

async fn build_pool(deps: &DiscoverDeps) {
    deps.pool
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the pool job runs");
}

async fn choose(deps: &DiscoverDeps, set_day: SetDay, depth: Depth) {
    deps.weekly
        .save_prefs("user-1", DiscoverPrefs { set_day, depth })
        .await
        .expect("the prefs are kept");
}

/// How many pool builds are queued for Ada, clearing them.
fn pool_builds_queued(db: &Db) -> i64 {
    let queued = db.one(
        "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'discover.pool' \
         AND idempotency_key = 'user-1' AND state = 'queued'",
        [],
    );
    db.exec("DELETE FROM durable_jobs WHERE kind = 'discover.pool'", []);
    queued
}

/// The stored pool, in order: each album with its support and artist's
/// listeners.
fn stored(db: &Db) -> Vec<(String, f64, Option<i64>, String)> {
    db.rows(
        "SELECT release_group_mbid_lower, support, artist_listeners, artist_mbid \
         FROM catalog_discover_candidates WHERE user_id = 'user-1' ORDER BY rank",
        [],
    )
}

/// The eight the pool deals at `depth`, as the weekly set selects them.
async fn eight(rig: &AuthRig, depth: Depth) -> Vec<(String, Option<i64>)> {
    let pool = pool_store::ranked(&rig.platform.db, "user-1", rig.clock().now_ms())
        .await
        .expect("the pool");
    let shares = [Share {
        group: 0,
        share: 1.0,
    }];
    select::select(
        &pool,
        weekly::SET_SIZE,
        &shares,
        &Exposure::default(),
        Floors::for_depth(depth),
    )
    .picks
    .into_iter()
    .map(|pick| (pick.release_group_mbid.clone(), pick.artist_listeners))
    .collect()
}

fn albums_of(picks: &[(String, Option<i64>)]) -> HashSet<String> {
    picks.iter().map(|(group, _)| group.clone()).collect()
}

/// The albums of neighbours `js` of every seed.
fn of_neighbours(js: &[usize]) -> HashSet<String> {
    (0..SEEDS)
        .flat_map(|s| js.iter().map(move |j| group(s, *j)))
        .collect()
}

fn median(picks: &[(String, Option<i64>)]) -> f64 {
    let mut counts: Vec<i64> = picks.iter().filter_map(|(_, count)| *count).collect();
    counts.sort_unstable();
    let mid = counts.len() / 2;
    (counts[mid - 1] + counts[mid]) as f64 / 2.0
}

fn set_rows(db: &Db) -> Vec<(i64, i64, String, String)> {
    db.rows(
        "SELECT week_start_at, pick_no, release_group_mbid_lower, reason_line \
         FROM catalog_weekly_set_picks WHERE user_id = 'user-1' ORDER BY week_start_at, pick_no",
        [],
    )
}

/// Next door is the pool by support alone and the eight that order deals,
/// and the set dealt from it records next door. Far out and home ground
/// each queue one pool build as they are saved and give another eight;
/// the median listeners fall from home ground to next door to far out. The
/// owned, told-no and single albums are in no pool. Two quick saves join
/// one build, a save that keeps the depth queues none, and this week's set
/// stays as it was; the next week's set records far out.
#[tokio::test]
async fn discover_depth_moves_the_order_and_floors_but_never_the_week() {
    let dir = crate::common::ScratchDir::new("discover-depth");
    let rig = AuthRig::new("discover-depth").await;
    let db = rig.platform.db.clone();
    let now = rig.clock().now_ms();
    let friday = weekly::day_on_or_after(now + 1, SetDay::Friday);
    rig.clock().advance(friday + HOUR - now);
    let at = rig.clock().now_ms() - HOUR;
    insert(&db, taste(at - HOUR) + &lists(at)).await;
    own(&db, &[group(0, 90)]).await;
    let deps = deps(&rig, &dir);
    let gated: HashSet<String> = (90..93).map(|j| group(0, j)).collect();

    // Next door, chosen by no one: support alone orders the pool.
    build_pool(&deps).await;
    let pool = stored(&db);
    let mut by_support = pool.clone();
    by_support.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.3.cmp(&b.3)));
    assert_eq!(pool, by_support, "next door orders by support, then id");
    assert_eq!(pool.len(), SEEDS * NEIGHBOURS + 2);
    let next_door = eight(&rig, Depth::NextDoor).await;
    assert_eq!(albums_of(&next_door), of_neighbours(&[0, 1]));

    deps.weekly
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the weekly set job runs");
    let set = set_rows(&db);
    let dealt: HashSet<String> = set.iter().map(|pick| pick.2.clone()).collect();
    assert_eq!(dealt, albums_of(&next_door), "the set is next door's eight");
    let depth: Option<String> = db.one(
        "SELECT depth FROM catalog_weekly_sets WHERE user_id = 'user-1'",
        [],
    );
    assert_eq!(depth.as_deref(), Some("next_door"));

    choose(&deps, SetDay::Friday, Depth::FarOut).await;
    assert_eq!(pool_builds_queued(&db), 1, "a new depth queues the pool");
    build_pool(&deps).await;
    let pool = stored(&db);
    assert!(pool.iter().all(|row| !gated.contains(&row.0)));
    let zero = group(1, ZERO);
    assert!(pool.iter().any(|row| row.0 == zero), "in the pool");
    assert_ne!(pool[0].0, zero, "no listeners does not lead far out");
    // Not counted reads as the median count: behind every album of the
    // least-heard tier, ahead of every album of the most-heard.
    let unknown = group(2, UNKNOWN);
    let at = pool.iter().position(|row| row.0 == unknown);
    let tier = SEEDS * NEIGHBOURS / 3;
    assert!(
        at.is_some_and(|at| at >= tier && at < pool.len() - tier),
        "not counted sits mid-pack far out: {at:?} of {}",
        pool.len()
    );
    let far_out = eight(&rig, Depth::FarOut).await;
    assert_eq!(albums_of(&far_out), of_neighbours(&[0, 3]));

    choose(&deps, SetDay::Friday, Depth::HomeGround).await;
    assert_eq!(pool_builds_queued(&db), 1);
    build_pool(&deps).await;
    assert!(stored(&db).iter().all(|row| !gated.contains(&row.0)));
    let home_ground = eight(&rig, Depth::HomeGround).await;
    assert_eq!(albums_of(&home_ground), of_neighbours(&[2, 5]));

    let medians = [median(&home_ground), median(&next_door), median(&far_out)];
    assert!(
        medians[0] > medians[1] && medians[1] > medians[2],
        "{medians:?}"
    );

    choose(&deps, SetDay::Friday, Depth::NextDoor).await;
    choose(&deps, SetDay::Friday, Depth::FarOut).await;
    assert_eq!(pool_builds_queued(&db), 1, "two quick saves join one build");
    choose(&deps, SetDay::Monday, Depth::FarOut).await;
    assert_eq!(pool_builds_queued(&db), 0, "the same depth queues nothing");
    choose(&deps, SetDay::Friday, Depth::FarOut).await;

    deps.weekly
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the weekly set job runs");
    assert_eq!(set_rows(&db), set, "this week's set holds still");

    // Next week the pool is still home ground's: the set waits for the
    // build far out queued.
    rig.clock().advance(WEEK_MS);
    deps.weekly
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the weekly set job runs");
    assert_eq!(set_rows(&db), set, "no set from a pool of another depth");
    build_pool(&deps).await;
    deps.weekly
        .run_once("user-1", |_| Ok(()))
        .await
        .expect("the weekly set job runs");
    let depths: Vec<Option<String>> = db.column(
        "SELECT depth FROM catalog_weekly_sets WHERE user_id = 'user-1' ORDER BY week_start_at",
        [],
    );
    assert_eq!(
        depths,
        [Some("next_door".to_owned()), Some("far_out".to_owned())],
        "each set records the depth it was dealt at"
    );
}
