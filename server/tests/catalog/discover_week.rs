//! The weekly set over a real database and the production builder, on a
//! manual clock: the set day's pass builds one set of at most eight from the
//! pool, with its reasons and the build's counts frozen, and nothing but
//! what the person does moves it after. The next week deals what is left. A
//! pool waiting for facts writes nothing, a short pool writes a short set,
//! a first set waits for the history import, and a person long away gets
//! theirs on their visit. A pick's state is read, never kept, and another
//! person reads none of it.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;

use droppedneedle_catalog::discover::due::Due;
use droppedneedle_catalog::discover::engine::weekly::{self, Depth, SetDay, ShortReason, WEEK_MS};
use droppedneedle_catalog::discover::facts::KeptFacts;
use droppedneedle_catalog::discover::facts::job::FactJobs;
use droppedneedle_catalog::discover::listening::UserMusic;
use droppedneedle_catalog::discover::models::{
    DiscoverPrefs, PickReasonKind, PickState, SetSource, WeekStatus,
};
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_kernel::page::PageRequest;
use droppedneedle_providers::Providers;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_pool::id;
use crate::discover_queue::{Script, call, discover_deps, discover_routes, insert, own};

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;
const SEEDS: usize = 10;

fn seed(n: usize) -> String {
    id(1, n)
}

fn album(n: usize) -> String {
    id(3, n)
}

/// `user`'s taste build at `at`: `plays` plays and 59 imported listens over
/// `span` ms up to `at`, ten seeds in three groups (shares 0.5, 0.3, 0.2),
/// each seed in group `n % 3`.
fn taste(user: &str, at: i64, plays: i64, span: i64) -> String {
    let mut rows = format!(
        "INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, history_from_at, history_to_at, \
         incomplete) VALUES ('{user}', {at}, {plays}, 59, {SEEDS}, {SEEDS}, {from}, {at}, 0);",
        from = at - span
    );
    for (no, share) in [(0, 0.5), (1, 0.3), (2, 0.2)] {
        rows += &format!(
            "INSERT INTO catalog_taste_groups (user_id, group_no, anchor_artist_mbid, \
             anchor_name, share, recent_share, trend) VALUES ('{user}', {no}, '{anchor}', \
             'Anchor {no}', {share}, {share}, 'steady');",
            anchor = seed(no)
        );
    }
    for n in 0..SEEDS {
        rows += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed, group_no) \
             VALUES ('{user}', '{s}', '{s}', 'Seed {n}', 5, 0, 1, {weight}, 1, {group});",
            s = seed(n),
            weight = 2.0 - n as f64 / 10.0,
            group = n % 3
        );
    }
    rows
}

/// `user`'s pool built at `at`, `incomplete` and waiting for `waiting`, of
/// the albums `albums` in that order: album `n` by its own artist, found
/// through seed `n % 10` and filed under that seed's group.
fn pool(
    user: &str,
    at: i64,
    incomplete: bool,
    waiting: &[String],
    albums: std::ops::Range<usize>,
) -> String {
    let mut rows = format!(
        "DELETE FROM catalog_discover_candidate_paths WHERE user_id = '{user}'; \
         DELETE FROM catalog_discover_candidates WHERE user_id = '{user}'; \
         DELETE FROM catalog_discover_pool_builds WHERE user_id = '{user}'; \
         INSERT INTO catalog_discover_pool_builds (user_id, built_at, incomplete, waiting_for, \
         seeds_used, neighbours_found, known_left_out, hubs_left_out, artists_ranked, \
         artists_without_album, new_artist_candidates, source_pick_candidates, gap_candidates, \
         shelf_candidates, fresh_candidates, thread_candidates) VALUES ('{user}', {at}, \
         {incomplete}, '{waiting}', {SEEDS}, 1256, 69, 3, 40, 0, 20, 0, 0, 0, 0, 0);",
        incomplete = i64::from(incomplete),
        waiting = serde_json::to_string(waiting).expect("ids"),
    );
    for (rank, n) in albums.enumerate() {
        let (s, group) = (seed(n % SEEDS), (n % SEEDS) % 3);
        rows += &format!(
            "INSERT INTO catalog_discover_candidates (user_id, release_group_mbid_lower, rank, \
             kind, title, artist_mbid, artist_name, credited_artist_mbids, primary_type, \
             first_release_date, album_listeners, artist_listeners, support, group_no, \
             group_support) VALUES ('{user}', '{a}', {rank}, 'new_artist', 'Album {n}', \
             '{artist}', 'Artist {n}', '[]', 'Album', '2010-05-01', 5000, 50000, {support}, \
             {group}, {support}); \
             INSERT INTO catalog_discover_candidate_paths (user_id, release_group_mbid_lower, \
             path_no, kind, source, fetched_at, artist_mbid, artist_name, artist_plays, \
             artist_weight, artist_albums_owned, link_strength, link_rank, link_of, \
             named_by_labs, named_by_lastfm) VALUES ('{user}', '{a}', 0, 'seed', \
             'listenbrainz_labs', {at}, '{s}', 'Seed {seed_no}', 5, 1.0, 0, 0.8, 2, 100, 1, 0);",
            a = album(n),
            artist = id(2, n),
            support = 100.0 - rank as f64,
            seed_no = n % SEEDS,
        );
    }
    rows
}

/// One of ListenBrainz's weekly picks for `user`, album `n`, in no taste
/// group, at the end of their pool.
fn source_pick(user: &str, n: usize, at: i64) -> String {
    format!(
        "INSERT INTO catalog_discover_candidates (user_id, release_group_mbid_lower, rank, \
         kind, title, artist_mbid, artist_name, credited_artist_mbids, primary_type, \
         album_listeners, artist_listeners, support, group_support) VALUES ('{user}', \
         '{a}', 900, 'source_pick', 'Album {n}', '{artist}', 'Artist {n}', '[]', 'Album', \
         5000, 50000, 0.0, 0.0); \
         INSERT INTO catalog_discover_candidate_paths (user_id, release_group_mbid_lower, \
         path_no, kind, source, fetched_at, named_by_labs, named_by_lastfm) \
         VALUES ('{user}', '{a}', 0, 'source', 'listenbrainz', {at}, 0, 0);",
        a = album(n),
        artist = id(2, n),
    )
}

const USERS: &str = "INSERT INTO auth_users (id, display_name, role, created_at) \
     VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0);";

/// The clock moved on to 01:00 on the next Friday; answers that Friday.
fn to_friday(rig: &AuthRig) -> i64 {
    let now = rig.clock().now_ms();
    let friday = weekly::day_on_or_after(now + 1, SetDay::Friday);
    rig.clock().advance(friday + HOUR - now);
    friday
}

fn deps(rig: &AuthRig, dir: &std::path::Path, script: Script) -> DiscoverDeps {
    let script = Arc::new(script);
    discover_deps(rig, dir, Providers::closed(), move |slot| *slot = script)
}

/// What queues Discover's work, with the warm cycle `enabled`.
pub(crate) fn due(rig: &AuthRig, enabled: bool) -> Due {
    let (db, clock) = (rig.platform.db.clone(), rig.platform.clock.clone());
    Due::new(
        db.clone(),
        JobQueue::new(db.clone(), clock.clone()),
        clock.clone(),
        enabled,
        (
            FactJobs::new(KeptFacts::new(db, clock), Arc::new(Script::default())).due(),
            crate::common::history_store(&rig.platform),
            crate::discover_queue::fresh_feed(&rig.platform),
        ),
    )
}

/// Whether `user`'s job of `kind` is queued, clearing that kind's jobs.
pub(crate) fn queued(db: &Db, kind: &str, user: &str) -> bool {
    let queued: i64 = db.one(
        "SELECT COUNT(*) FROM durable_jobs WHERE kind = ?1 AND idempotency_key = ?2 \
         AND state = 'queued'",
        [kind, user],
    );
    db.exec("DELETE FROM durable_jobs WHERE kind = ?1", [kind]);
    queued > 0
}

/// `user` was last seen at `at`.
pub(crate) async fn seen_at(db: &Db, user: &str, at: i64) {
    insert(
        db,
        format!(
            "INSERT INTO auth_tokens (id, user_id, token_hash, issued_at, expires_at, \
             last_seen_at, revoked) VALUES ('{user}', '{user}', '{user}', 0, {end}, {at}, 0) \
             ON CONFLICT (id) DO UPDATE SET last_seen_at = excluded.last_seen_at;",
            end = at + 365 * DAY
        ),
    )
    .await;
}

async fn build(deps: &DiscoverDeps, user: &str) {
    deps.weekly
        .run_once(user, |_| Ok(()))
        .await
        .expect("the weekly set job runs");
}

/// `user`'s set picks, every week, as stored.
fn kept(db: &Db, user: &str) -> Vec<(i64, i64, String, Option<i64>, String, String)> {
    db.rows(
        "SELECT week_start_at, pick_no, release_group_mbid_lower, taste_no, reason_line, \
         evidence FROM catalog_weekly_set_picks WHERE user_id = ?1 \
         ORDER BY week_start_at, pick_no",
        [user],
    )
}

/// The albums of `user`'s set for the week starting `week`.
fn albums_of(db: &Db, user: &str, week: i64) -> Vec<String> {
    db.column(
        "SELECT release_group_mbid_lower FROM catalog_weekly_set_picks \
         WHERE user_id = ?1 AND week_start_at = ?2 ORDER BY pick_no",
        rusqlite::params![user, week],
    )
}

/// On the set day the clock's pass queues the set and the job builds one
/// set of eight from the pool, filed under the tastes that got slots, each
/// reason frozen with its evidence and offered in the pick record, and its
/// counts the taste and pool builds' own. Running it again, building the
/// pool again and choosing another day leave it as it is. Bob has none.
#[tokio::test]
async fn discover_week_the_set_day_builds_one_set_that_holds_still() {
    let dir = crate::common::ScratchDir::new("discover-week");
    let rig = AuthRig::new("discover-week").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 400, 400 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..20);
    insert(&db, rows).await;
    seen_at(&db, "user-1", at - DAY).await;
    let deps = deps(&rig, &dir, Script::default());

    due(&rig, true).clock_pass().await;
    assert!(
        queued(&db, weekly::KIND.name(), "user-1"),
        "the set day queues the set"
    );
    build(&deps, "user-1").await;
    let set = kept(&db, "user-1");
    assert_eq!(set.len(), 8, "eight picks");
    assert!(set.iter().all(|pick| pick.0 == friday));
    assert!(
        set.iter().all(|pick| pick.4
            == format!("Close to Seed {}.", {
                let n = (0..20).find(|n| album(*n) == pick.2).expect("a pool album");
                n % SEEDS
            })),
        "each reason is its path: {set:?}"
    );
    let counts: (i64, i64, i64, i64, i64, i64, Option<i64>, i64, i64) = db.row(
        "SELECT built_at, plays_counted, history_to_at - history_from_at, seeds, \
         neighbours_found, known_left_out, hubs_left_out, shelf_artists, linked \
         FROM catalog_weekly_sets WHERE user_id = 'user-1'",
        [],
    );
    assert_eq!(
        counts,
        (at, 459, 400 * DAY, 10, 1256, 69, Some(3), 0, 0),
        "the taste and pool builds' counts"
    );
    let offered: i64 = db.one(
        "SELECT COUNT(*) FROM catalog_discover_picks WHERE user_id = 'user-1' \
         AND first_surface = 'weekly_set'",
        [],
    );
    assert_eq!(offered, 8, "each pick offered on the weekly set");

    let week = deps.weekly.week("user-1").await.expect("the week");
    assert_eq!(week.status, WeekStatus::Ready);
    let shown = week.set.expect("this week's set");
    assert_eq!(shown.week_start_at, friday);
    assert_eq!(shown.next_set_at, friday + WEEK_MS);
    assert_eq!(shown.short_reason, None);
    let tastes: Vec<(String, Option<f64>, i64)> = shown
        .tastes
        .iter()
        .map(|taste| (taste.name.clone(), taste.share, taste.picks))
        .collect();
    assert_eq!(
        tastes,
        [
            ("Around Anchor 0".to_owned(), Some(0.5), 3),
            ("Around Anchor 1".to_owned(), Some(0.3), 3),
            ("Around Anchor 2".to_owned(), Some(0.2), 2),
        ],
        "the tastes that got slots, largest first, each with its picks"
    );
    assert!(shown.picks.iter().all(|pick| pick.state == PickState::Open));
    assert!(
        shown
            .picks
            .iter()
            .all(|pick| pick.album.year == Some(2010) && pick.taste_no.is_some())
    );

    // A second run, a pool built again and another day change nothing.
    build(&deps, "user-1").await;
    rig.clock().advance(HOUR);
    insert(
        &db,
        pool("user-1", rig.clock().now_ms(), false, &[], 20..40),
    )
    .await;
    build(&deps, "user-1").await;
    deps.weekly
        .save_prefs(
            "user-1",
            DiscoverPrefs {
                set_day: SetDay::Monday,
                depth: Depth::NextDoor,
                app_playlists: true,
            },
        )
        .await
        .expect("a new day");
    rig.clock().advance(3 * DAY);
    build(&deps, "user-1").await;
    assert_eq!(kept(&db, "user-1"), set, "the set holds still");
    let week = deps.weekly.week("user-1").await.expect("the week");
    assert_eq!(
        week.status,
        WeekStatus::Ready,
        "Monday counts from next week"
    );
    let monday = weekly::day_on_or_after(friday + WEEK_MS, SetDay::Monday);
    assert_eq!(week.set.expect("the set").next_set_at, monday);

    let bobs: i64 = db.one(
        "SELECT COUNT(*) FROM catalog_weekly_sets WHERE user_id = 'user-2'",
        [],
    );
    assert_eq!(bobs, 0, "Bob has no set");
}

/// The next week deals a new set from what is left: no album of last
/// week's, passed over or not, and last week's still reads whole.
#[tokio::test]
async fn discover_week_the_next_week_deals_a_new_set_and_keeps_the_last() {
    let dir = crate::common::ScratchDir::new("discover-week-next");
    let rig = AuthRig::new("discover-week-next").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 400, 400 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..20);
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Script::default());

    build(&deps, "user-1").await;
    let first = kept(&db, "user-1");
    let last_week = albums_of(&db, "user-1", friday);
    assert_eq!(last_week.len(), 8);

    rig.clock().advance(WEEK_MS);
    build(&deps, "user-1").await;
    let this_week = albums_of(&db, "user-1", friday + WEEK_MS);
    assert_eq!(this_week.len(), 8, "a new set of eight");
    assert!(
        this_week.iter().all(|album| !last_week.contains(album)),
        "nothing of last week's: {this_week:?} {last_week:?}"
    );
    let all = kept(&db, "user-1");
    assert_eq!(&all[..8], &first[..], "last week's reads whole");
    let week = deps.weekly.week("user-1").await.expect("the week");
    assert_eq!(
        week.set.expect("this week's").week_start_at,
        friday + WEEK_MS
    );
}

/// A pool still waiting for facts with five found writes nothing and says
/// five are found; once it is built again whole a visit asks for the set
/// and the week gets it. The week after, with MusicBrainz failing, last
/// week's set is kept and the answer says since when.
#[tokio::test]
async fn discover_week_an_incomplete_pool_writes_nothing_until_its_facts_arrive() {
    let dir = crate::common::ScratchDir::new("discover-week-waiting");
    let rig = AuthRig::new("discover-week-waiting").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let waiting = vec![id(2, 99)];
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 400, 400 * DAY);
    rows += &pool("user-1", at - HOUR, true, &waiting, 0..5);
    insert(&db, rows).await;
    seen_at(&db, "user-1", at).await;
    let deps = deps(&rig, &dir, Script::default());

    build(&deps, "user-1").await;
    assert!(kept(&db, "user-1").is_empty(), "nothing written");
    let week = deps.weekly.week("user-1").await.expect("the week");
    assert_eq!(
        (week.status, week.found_so_far, week.set),
        (WeekStatus::BeingSetOut, Some(5), None)
    );

    // The facts arrive: the pool is built again whole, and a visit asks.
    let visits = due(&rig, false);
    visits.visit("user-1").await;
    db.exec("DELETE FROM durable_jobs", []);
    rig.clock().advance(60_000);
    insert(&db, pool("user-1", rig.clock().now_ms(), false, &[], 0..12)).await;
    visits.visit("user-1").await;
    assert!(
        queued(&db, weekly::KIND.name(), "user-1"),
        "a new pool asks for the set again"
    );
    build(&deps, "user-1").await;
    assert_eq!(albums_of(&db, "user-1", friday).len(), 8);

    // Next week, seen lately, the pool waits on MusicBrainz, which keeps
    // failing.
    rig.clock().advance(WEEK_MS);
    seen_at(&db, "user-1", rig.clock().now_ms()).await;
    let failing_since = rig.clock().now_ms() - 2 * HOUR;
    insert(
        &db,
        pool("user-1", rig.clock().now_ms(), true, &waiting, 12..17)
            + &format!(
                "INSERT INTO catalog_fact_fetches (artist_mbid, kind, source, outcome, \
                 failing_since_at, failures, due_at, read_at) VALUES ('{artist}', 'albums', \
                 'musicbrainz', 'failed', {failing_since}, 3, {due}, {failing_since});",
                artist = waiting[0],
                due = rig.clock().now_ms() + HOUR
            ),
    )
    .await;
    build(&deps, "user-1").await;
    assert!(albums_of(&db, "user-1", friday + WEEK_MS).is_empty());
    let week = deps.weekly.week("user-1").await.expect("the week");
    assert_eq!(week.status, WeekStatus::KeptLastWeek);
    let failing = week.failing.expect("what fails");
    assert_eq!(
        (failing.source, failing.since),
        (SetSource::Musicbrainz, failing_since)
    );
    assert_eq!(
        week.set.expect("last week's").week_start_at,
        friday,
        "last week's set stays"
    );
}

/// A complete pool of five writes a set of five, never padded, that says
/// why: a history of four months. Every pick is one of the pool's, none a
/// chart's.
#[tokio::test]
async fn discover_week_a_complete_pool_of_five_writes_a_short_set() {
    let dir = crate::common::ScratchDir::new("discover-week-short");
    let rig = AuthRig::new("discover-week-short").await;
    let db = rig.platform.db.clone();
    to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 40, 122 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..5);
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Script::default());

    build(&deps, "user-1").await;
    let week = deps.weekly.week("user-1").await.expect("the week");
    assert_eq!(week.status, WeekStatus::Ready);
    let set = week.set.expect("a short set");
    assert_eq!(set.picks.len(), 5, "never padded");
    assert_eq!(set.short_reason, Some(ShortReason::ShortHistory));
    assert_eq!(
        set.short_line.as_deref(),
        Some("Your history is 4 months long, and nothing from the charts is used to fill a set.")
    );
    let pool_albums: Vec<String> = (0..5).map(album).collect();
    assert!(set.picks.iter().all(|pick| {
        pick.reason.kind == PickReasonKind::YourArtists
            && pick
                .album
                .musicbrainz_id
                .as_ref()
                .is_some_and(|id| pool_albums.contains(id))
    }));
}

/// Home's teaser reads the week, and what the page saw happen to a pick
/// is stamped on its record: a pick shown counts once per UTC day
/// however often it is shown, its first sample and opening are stamped, and
/// an event about an album never offered writes nothing.
#[tokio::test]
async fn discover_week_counts_open_picks_and_stamps_what_was_seen() {
    let dir = crate::common::ScratchDir::new("discover-week-events");
    let rig = AuthRig::new("discover-week-events").await;
    let db = rig.platform.db.clone();
    to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 40, 122 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..5);
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Script::default());
    let app = discover_routes(deps.clone());
    let teaser = || async {
        weekly::teaser(&db, "user-1", rig.clock().now_ms())
            .await
            .expect("the teaser reads")
    };
    assert_eq!(teaser().await, None, "no set yet");

    build(&deps, "user-1").await;
    let built = teaser().await.expect("a set is out");
    assert_eq!(built.albums.len(), 5, "five picks");
    assert_eq!(
        built.next.map(|pick| pick.pick_no),
        Some(1),
        "none answered"
    );

    let card = album(0);
    let event = |id: &str, kind: &str| {
        Some(json!({"release_group_mbid": id.to_uppercase(), "event": kind}))
    };
    let events = "/api/v3/discover/picks/events";
    for _ in 0..2 {
        let (status, body) = call(&app, "POST", events, event(&card, "shown")).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }
    let first = rig.clock().now_ms();
    rig.clock().advance(DAY);
    for kind in ["shown", "sampled", "opened", "sampled"] {
        let (status, body) = call(&app, "POST", events, event(&card, kind)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{kind}: {body}");
    }
    let next = rig.clock().now_ms();
    let seen: (i64, i64, i64, i64, i64) = db.row(
        "SELECT shown_days, first_shown_at, last_shown_at, sampled_at, opened_at \
         FROM catalog_discover_picks WHERE user_id = 'user-1' AND release_group_mbid_lower = ?1",
        [&card],
    );
    assert_eq!(seen, (2, first, next, next, next));
    // A sample is taste, never a play: no play row, no album play and no
    // playback event.
    for table in [
        "library_play_history",
        "library_user_album_plays",
        "media_playback_events",
    ] {
        let rows: i64 = db.one(&format!("SELECT COUNT(*) FROM {table}"), []);
        assert_eq!(rows, 0, "a sample wrote to {table}");
    }

    let offered: i64 = db.one("SELECT COUNT(*) FROM catalog_discover_picks", []);
    let (status, _) = call(&app, "POST", events, event(&album(9), "shown")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let after: i64 = db.one("SELECT COUNT(*) FROM catalog_discover_picks", []);
    assert_eq!(after, offered, "an album never offered writes nothing");
    let (status, _) = call(&app, "POST", events, event("not-an-mbid", "shown")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A linked person whose import is still being read gets "reading your
/// history" and no set; it is built once the import is current. Another,
/// whose import runs on, gets theirs six hours after their first visit.
#[tokio::test]
async fn discover_week_a_first_set_waits_for_the_import() {
    let dir = crate::common::ScratchDir::new("discover-week-history");
    let rig = AuthRig::new("discover-week-history").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    for user in ["user-1", "user-2"] {
        rows += &taste(user, at - DAY, 400, 400 * DAY);
        rows += &pool(user, at - HOUR, false, &[], 0..20);
        rows += &format!(
            "INSERT INTO media_history_imports (user_id, state, source, updated_at) \
             VALUES ('{user}', 'importing', 'export', {at});"
        );
    }
    insert(&db, rows).await;
    let linked = Script {
        user: UserMusic {
            listenbrainz: Some("someone".to_owned()),
            ..UserMusic::default()
        },
        ..Script::default()
    };
    let deps = deps(&rig, &dir, linked);
    let visits = due(&rig, false);

    visits.visit("user-1").await;
    visits.visit("user-2").await;
    build(&deps, "user-1").await;
    assert!(kept(&db, "user-1").is_empty(), "no set while it is read");
    let week = deps.weekly.week("user-1").await.expect("the week");
    assert_eq!(week.status, WeekStatus::ReadingHistory);

    insert(
        &db,
        "UPDATE media_history_imports SET state = 'current' WHERE user_id = 'user-1'".to_owned(),
    )
    .await;
    build(&deps, "user-1").await;
    assert_eq!(
        albums_of(&db, "user-1", friday).len(),
        8,
        "built once current"
    );
    let linked_row: i64 = db.one(
        "SELECT linked FROM catalog_weekly_sets WHERE user_id = 'user-1'",
        [],
    );
    assert_eq!(linked_row, 1);

    rig.clock().advance(6 * HOUR - 60_000);
    build(&deps, "user-2").await;
    assert!(
        kept(&db, "user-2").is_empty(),
        "under six hours, still waiting"
    );
    rig.clock().advance(60_000);
    build(&deps, "user-2").await;
    assert_eq!(
        albums_of(&db, "user-2", friday).len(),
        8,
        "six hours after the first visit it is built from what is there"
    );
}

/// The clock builds a set for someone seen ten days ago from the pool they
/// have, and none for someone not seen in forty days, who gets theirs on
/// their visit.
#[tokio::test]
async fn discover_week_a_person_long_away_gets_their_set_on_their_visit() {
    let dir = crate::common::ScratchDir::new("discover-week-away");
    let rig = AuthRig::new("discover-week-away").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    for user in ["user-1", "user-2"] {
        rows += &taste(user, at - 41 * DAY, 400, 400 * DAY);
        rows += &pool(user, at - 41 * DAY, false, &[], 0..20);
    }
    insert(&db, rows).await;
    seen_at(&db, "user-1", at - 40 * DAY).await;
    seen_at(&db, "user-2", at - 10 * DAY).await;
    let deps = deps(&rig, &dir, Script::default());

    let clock = due(&rig, true);
    let sets = || -> Vec<String> {
        let queued = db.column(
            "SELECT idempotency_key FROM durable_jobs WHERE kind = ?1 AND state = 'queued'",
            [weekly::KIND.name()],
        );
        db.exec("DELETE FROM durable_jobs", []);
        queued
    };
    clock.clock_pass().await;
    assert_eq!(
        sets(),
        ["user-2"],
        "the clock builds for ten days away only"
    );
    // While nothing it is built from changes, a set that did not come is
    // asked for once a day, not on every pass.
    rig.clock().advance(20 * 60_000);
    clock.clock_pass().await;
    assert!(sets().is_empty(), "not asked again within the day");
    rig.clock().advance(DAY);
    clock.clock_pass().await;
    assert_eq!(sets(), ["user-2"], "asked again the next day");

    due(&rig, false).visit("user-1").await;
    assert!(queued(&db, weekly::KIND.name(), "user-1"), "the visit asks");
    build(&deps, "user-1").await;
    assert_eq!(albums_of(&db, "user-1", friday).len(), 8);
}

/// What became of each pick is read from the person's requests, the
/// library, their Later pile and what they told Discover. A rejected
/// request reads as refused, never as kept.
#[tokio::test]
async fn discover_week_a_refused_request_does_not_read_as_kept() {
    let dir = crate::common::ScratchDir::new("discover-week-states");
    let rig = AuthRig::new("discover-week-states").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 400, 400 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..20);
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Script::default());
    build(&deps, "user-1").await;
    let set = albums_of(&db, "user-1", friday);
    assert_eq!(set.len(), 8);

    let mut rows = String::new();
    for (pick, status) in [
        (0, "pending"),
        (1, "awaiting_approval"),
        (2, "rejected"),
        (3, "imported"),
    ] {
        rows += &format!(
            "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
             album_title, requested_at, status, user_id) VALUES ('{g}', '{g}', 'Someone', \
             'Album', {at}, '{status}', 'user-1'); \
             INSERT INTO request_history_requesters (user_id, musicbrainz_id_lower, \
             requested_at) VALUES ('user-1', '{g}', {at});",
            g = set[pick]
        );
    }
    rows += &format!(
        "INSERT INTO catalog_later_albums (user_id, release_group_mbid_lower, \
         release_group_mbid, title, artist_name, added_at) VALUES ('user-1', '{later}', \
         '{later}', 'Album', 'Someone', {at}); \
         INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'album', '{no}', 'no', {at}); \
         UPDATE catalog_discover_picks SET sampled_at = {at} \
         WHERE user_id = 'user-1' AND release_group_mbid_lower = '{heard}';",
        later = set[4],
        no = set[5],
        heard = set[6],
    );
    insert(&db, rows).await;
    own(&db, std::slice::from_ref(&set[3])).await;

    let week = deps.weekly.week("user-1").await.expect("the week");
    let states: Vec<(PickState, bool)> = week
        .set
        .expect("the set")
        .picks
        .iter()
        .map(|pick| (pick.state, pick.heard))
        .collect();
    assert_eq!(
        states,
        [
            (PickState::Asked, false),
            (PickState::AwaitingApproval, false),
            (PickState::Refused, false),
            (PickState::Arrived, false),
            (PickState::Later, false),
            (PickState::No, false),
            (PickState::Open, true),
            (PickState::Open, false),
        ]
    );
    let first_open = weekly::teaser(&db, "user-1", rig.clock().now_ms())
        .await
        .expect("the teaser")
        .and_then(|teaser| teaser.next)
        .expect("an open pick");
    assert_eq!(first_open.pick_no, 7, "the first pick not answered");

    // A kept pick whose evidence no longer reads keeps its line.
    db.exec(
        "UPDATE catalog_weekly_set_picks SET evidence = 'not json' WHERE pick_no = 8",
        [],
    );
    let week = deps.weekly.week("user-1").await.expect("the week");
    let picks = week.set.expect("the set").picks;
    assert_eq!(picks.len(), 8, "the week reads whole");
    let line: String = db.one(
        "SELECT reason_line FROM catalog_weekly_set_picks WHERE pick_no = 8",
        [],
    );
    assert_eq!(picks[7].reason.line, line);
}

/// "Dig further" deals the pool past the set in its order, a page at a
/// time, ending with no cursor; Bob reads none of it.
#[tokio::test]
async fn discover_week_more_deals_past_the_set_in_order() {
    let dir = crate::common::ScratchDir::new("discover-week-more");
    let rig = AuthRig::new("discover-week-more").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 400, 400 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..20);
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Script::default());
    build(&deps, "user-1").await;
    let set = albums_of(&db, "user-1", friday);
    let mut past: Vec<String> = (0..20).map(album).filter(|a| !set.contains(a)).collect();
    // One of them was shown on the deck and passed over: it rests.
    let passed = past.remove(0);
    insert(
        &db,
        format!(
            "INSERT INTO catalog_discover_picks (user_id, release_group_mbid_lower, \
             release_group_mbid, first_surface, first_offered_at, last_offered_at, evidence, \
             first_shown_at, last_shown_at, shown_days) VALUES ('user-1', '{passed}', \
             '{passed}', 'deck', {at}, {at}, '{{}}', {at}, {at}, 1);"
        ),
    )
    .await;

    let mut dealt = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let paging = PageRequest::read(Some(5), cursor.as_deref(), 20, 50).expect("paging");
        let page = deps.weekly.more("user-1", paging).await.expect("a page");
        assert_eq!(page.total, Some(11));
        dealt.extend(
            page.items
                .into_iter()
                .filter_map(|item| item.album.musicbrainz_id),
        );
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(
        dealt, past,
        "past the set, in the pool's order, the passed-over one resting"
    );

    let paging = PageRequest::read(None, None, 20, 50).expect("paging");
    let bobs = deps.weekly.more("user-2", paging).await.expect("Bob's");
    assert!(bobs.items.is_empty());
    let week = deps.weekly.week("user-2").await.expect("Bob's week");
    assert_eq!((week.status, week.set), (WeekStatus::NothingYet, None));
}

/// Whether a visit by Ada now queues her pool build.
async fn pool_due(db: &Db, visits: &Due) -> bool {
    db.exec("DELETE FROM durable_jobs", []);
    visits.visit("user-1").await;
    queued(db, "discover.pool", "user-1")
}

/// A person with a taste build is due a pool build while they have none,
/// or while theirs is older than their latest taste build.
#[tokio::test]
async fn discover_week_a_missing_or_older_pool_is_due() {
    let rig = AuthRig::new("discover-week-pool-due").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms();
    insert(
        &db,
        String::from(USERS) + &taste("user-1", at - DAY, 400, 400 * DAY),
    )
    .await;
    let visits = due(&rig, false);
    assert!(pool_due(&db, &visits).await, "no pool yet");
    insert(
        &rig.platform.db,
        pool("user-1", at - 2 * DAY, false, &[], 0..3),
    )
    .await;
    rig.clock().advance(20 * 60_000);
    assert!(pool_due(&db, &visits).await, "built before the taste build");

    // While a pool build of hers runs, none more is asked for; one that
    // failed is asked for again a day later, not on every pass.
    let now = rig.clock().now_ms();
    db.exec("DELETE FROM durable_jobs", []);
    db.exec(
        "INSERT INTO durable_jobs (kind, idempotency_key, state, attempts, not_before_at, \
         lease_owner, lease_expires_at, payload_json, created_at, updated_at) \
         VALUES ('discover.pool', 'user-1', 'running', 1, ?1, 'worker', ?2, 'null', ?1, ?1)",
        [now, now + 60_000],
    );
    rig.clock().advance(20 * 60_000);
    visits.visit("user-1").await;
    let waiting = || -> i64 {
        db.one(
            "SELECT COUNT(*) FROM durable_jobs WHERE kind = 'discover.pool' \
             AND state = 'queued'",
            [],
        )
    };
    assert_eq!(waiting(), 0, "not while one runs");
    db.exec(
        "UPDATE durable_jobs SET state = 'failed', lease_owner = NULL, \
         lease_expires_at = NULL, finished_at = ?1",
        [rig.clock().now_ms()],
    );
    rig.clock().advance(20 * 60_000);
    visits.visit("user-1").await;
    assert_eq!(waiting(), 0, "not again right after it failed");
    rig.clock().advance(DAY);
    visits.visit("user-1").await;
    assert_eq!(waiting(), 1, "a day after it failed");

    insert(&rig.platform.db, pool("user-1", at, false, &[], 0..3)).await;
    rig.clock().advance(20 * 60_000);
    assert!(!pool_due(&db, &visits).await, "built after it");
}

/// A ListenBrainz pick in the pool never takes a slot: where the person's
/// tastes give five, the set is five and says why, and the pick is in dig
/// further.
#[tokio::test]
async fn discover_week_a_listenbrainz_pick_never_fills_a_slot() {
    let dir = crate::common::ScratchDir::new("discover-week-source");
    let rig = AuthRig::new("discover-week-source").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 400, 400 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..5);
    rows += &source_pick("user-1", 50, at - HOUR);
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Script::default());

    build(&deps, "user-1").await;
    let set = albums_of(&db, "user-1", friday);
    assert_eq!(set, (0..5).map(album).collect::<Vec<_>>(), "never padded");
    let short: Option<String> = db.one(
        "SELECT short_reason FROM catalog_weekly_sets WHERE user_id = 'user-1'",
        [],
    );
    assert_eq!(short.as_deref(), Some("rest_known"));
    let paging = PageRequest::read(None, None, 20, 50).expect("paging");
    let more = deps.weekly.more("user-1", paging).await.expect("a page");
    let further: Vec<Option<String>> = more
        .items
        .into_iter()
        .map(|item| item.album.musicbrainz_id)
        .collect();
    assert_eq!(further, [Some(album(50))], "the pick is in dig further");
}

/// For someone not seen in seven days the set job asks for no pool build:
/// with a pool older than their taste build it writes nothing, and the week
/// keeps the last set or has nothing yet. With a pool that still waits for
/// facts it is built from what is there, short and saying why. Someone seen
/// lately has their pool built first.
#[tokio::test]
async fn discover_week_a_quiet_person_gets_no_pool_build_from_the_set() {
    let dir = crate::common::ScratchDir::new("discover-week-quiet");
    let rig = AuthRig::new("discover-week-quiet").await;
    let db = rig.platform.db.clone();
    let friday = to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('user-3', 'Cy', 'user', 0);";
    rows += &taste("user-1", at - DAY, 400, 400 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..20);
    rows += &taste("user-2", at - HOUR, 400, 400 * DAY);
    rows += &pool("user-2", at - DAY, false, &[], 0..20);
    rows += &taste("user-3", at - DAY, 400, 400 * DAY);
    rows += &pool("user-3", at - HOUR, true, &[id(2, 99)], 0..5);
    insert(&db, rows).await;
    for user in ["user-1", "user-2", "user-3"] {
        seen_at(&db, user, at - 10 * DAY).await;
    }
    let deps = deps(&rig, &dir, Script::default());

    // Ada's first week is built; next week her taste is newer than her pool.
    build(&deps, "user-1").await;
    rig.clock().advance(WEEK_MS);
    let later = rig.clock().now_ms();
    insert(
        &db,
        taste("user-1", later, 400, 400 * DAY).replace(
            "INSERT INTO catalog_taste_builds",
            "DELETE FROM catalog_taste_groups WHERE user_id = 'user-1'; \
         DELETE FROM catalog_taste_artists WHERE user_id = 'user-1'; \
         DELETE FROM catalog_taste_builds WHERE user_id = 'user-1'; \
         INSERT INTO catalog_taste_builds",
        ),
    )
    .await;
    db.exec("DELETE FROM durable_jobs", []);
    for user in ["user-1", "user-2"] {
        build(&deps, user).await;
        assert!(
            !queued(&db, "discover.pool", user),
            "{user}: no pool build while away"
        );
    }
    assert!(albums_of(&db, "user-1", friday + WEEK_MS).is_empty());
    let week = deps.weekly.week("user-1").await.expect("Ada's week");
    assert_eq!(
        (week.status, week.failing),
        (WeekStatus::KeptLastWeek, None)
    );
    assert_eq!(week.set.expect("last week's").week_start_at, friday);
    let week = deps.weekly.week("user-2").await.expect("Bob's week");
    assert_eq!((week.status, week.set), (WeekStatus::NothingYet, None));

    // Cy's pool still waits for facts that will not come while he is away.
    build(&deps, "user-3").await;
    let week = deps.weekly.week("user-3").await.expect("Cy's week");
    assert_eq!(week.status, WeekStatus::Ready);
    let set = week.set.expect("a short set");
    assert_eq!(
        (set.picks.len(), set.short_reason),
        (5, Some(ShortReason::NotChecked))
    );

    // Seen today, Bob's pool is built first.
    seen_at(&db, "user-2", rig.clock().now_ms()).await;
    build(&deps, "user-2").await;
    assert!(queued(&db, "discover.pool", "user-2"), "his pool first");
    let week = deps.weekly.week("user-2").await.expect("Bob's week");
    assert_eq!(
        (week.status, week.found_so_far),
        (WeekStatus::BeingSetOut, Some(0)),
        "nothing counted from a pool older than his taste"
    );
}

/// A first visit, before any taste build, starts the first set's wait for
/// the history being imported.
#[tokio::test]
async fn discover_week_a_first_visit_notes_the_wait_before_any_taste() {
    let rig = AuthRig::new("discover-week-first-visit").await;
    let db = rig.platform.db.clone();
    let at = rig.clock().now_ms();
    insert(
        &db,
        String::from(USERS)
            + &format!(
                "INSERT INTO media_history_imports (user_id, state, source, updated_at) \
                 VALUES ('user-1', 'pending', 'export', {at});"
            ),
    )
    .await;
    due(&rig, false).visit("user-1").await;
    let noted: Option<i64> = db.one(
        "SELECT history_wait_from_at FROM catalog_discover_prefs WHERE user_id = 'user-1'",
        [],
    );
    assert_eq!(noted, Some(at), "the wait runs from the first visit");
}

/// Dig further is not held to two albums a seed: with six albums behind
/// each of ten seeds, everything past the set is dealt.
#[tokio::test]
async fn discover_week_dig_further_deals_every_album_of_a_seed() {
    let dir = crate::common::ScratchDir::new("discover-week-dig");
    let rig = AuthRig::new("discover-week-dig").await;
    let db = rig.platform.db.clone();
    to_friday(&rig);
    let at = rig.clock().now_ms();
    let mut rows = String::from(USERS);
    rows += &taste("user-1", at - DAY, 400, 400 * DAY);
    rows += &pool("user-1", at - HOUR, false, &[], 0..60);
    insert(&db, rows).await;
    let deps = deps(&rig, &dir, Script::default());
    build(&deps, "user-1").await;

    let paging = PageRequest::read(Some(50), None, 20, 50).expect("paging");
    let page = deps.weekly.more("user-1", paging).await.expect("a page");
    assert_eq!(page.total, Some(52), "all 52 past the set");
}
