//! The pick record over a real database: the daily job works out what came
//! of each pick from the request, library, play and feedback rows, keeps
//! the rows the retention keeps, is queued by a person's visit with the
//! warm cycle off and by the clock only with it on, and forgetting takes
//! one person's rows and no one else's.

use std::sync::Arc;

use droppedneedle_catalog::discover::due::Due;
use droppedneedle_catalog::discover::facts::{KeptFacts, job::FactJobs};
use droppedneedle_catalog::discover::record;
use droppedneedle_catalog::discover::scheduler::{ArtistDetail, Feature};
use droppedneedle_catalog::test_support::refresh_pick_outcomes;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::jobs::queue::JobQueue;
use droppedneedle_providers::Providers;

use crate::common::Probe;
use crate::common::auth::AuthRig;
use crate::discover_queue::{after_commits, discover_deps};

const DAY: i64 = 86_400_000;

/// A pick's outcome columns: requested at and from, arrived, first played,
/// the 30 and 90 day plays, and what the person said and when.
type Outcome = (
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    i64,
    i64,
    Option<String>,
    Option<i64>,
);
const PICK: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const UNASKED: &str = "aaaaaaaa-0000-4000-8000-000000000002";
const ARTIST: &str = "bbbbbbbb-0000-4000-8000-000000000001";

async fn seed(db: &Db, sql: String) {
    db.write(Lane::Foreground, "test.seed", move |tx| {
        tx.execute_batch(&sql)?;
        Ok::<_, DbError>(())
    })
    .await
    .expect("seed rows");
}

/// Two people, `user-1` and `user-2`.
async fn people(db: &Db) {
    seed(
        db,
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-1', 'Ada', 'user', 0), ('user-2', 'Bob', 'user', 0)"
            .to_owned(),
    )
    .await;
}

/// A pick of `user`'s for `group`, offered at `offered` and shown at
/// `shown`, with nothing else known.
fn pick(user: &str, group: &str, offered: i64, shown: Option<i64>) -> String {
    let shown = shown.map_or("NULL".to_owned(), |at| at.to_string());
    format!(
        "INSERT INTO catalog_discover_picks (user_id, release_group_mbid_lower, \
         release_group_mbid, artist_mbid, album_name, artist_name, first_surface, \
         first_offered_at, last_offered_at, evidence, first_shown_at, last_shown_at, \
         shown_days) VALUES ('{user}', '{group}', '{group}', '{ARTIST}', 'Album', 'Someone', \
         'deck', {offered}, {offered}, '{{\"version\":1}}', {shown}, {shown}, \
         CASE WHEN {shown} IS NULL THEN 0 ELSE 1 END);"
    )
}

/// The job sets when the pick was asked for and where, when it arrived,
/// its first play and its 30 and 90 day counts after arrival, and what the
/// person said about it, from that person's rows only. A play before the
/// arrival, after its first 90 days or someone else's does not count, and
/// an ask made, or a copy that came in, before the pick was offered is not
/// its outcome. An undone answer leaves the row.
#[tokio::test]
async fn the_daily_job_works_out_what_came_of_each_pick() {
    let rig = AuthRig::new("discover-record-outcomes").await;
    let db = rig.platform.db.clone();
    people(&db).await;
    let offered = rig.clock().now_ms();
    let asked = offered + DAY;
    let arrived = offered + 2 * DAY;
    let mut rows = pick("user-1", PICK, offered, Some(offered));
    rows.push_str(&pick("user-1", UNASKED, offered, Some(offered)));
    rows.push_str(&format!(
        "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, requested_at, status, user_id) VALUES \
         ('{PICK}', '{PICK}', 'Someone', 'Album', {asked}, 'imported', 'user-2'), \
         ('{UNASKED}', '{UNASKED}', 'Someone', 'Other', {before}, 'imported', 'user-1'); \
         INSERT INTO request_history_requesters (user_id, musicbrainz_id_lower, \
         requested_at, made_from) VALUES ('user-2', '{PICK}', {asked}, 'search'), \
         ('user-1', '{PICK}', {asked}, 'deck'), ('user-1', '{UNASKED}', {before}, 'search'); \
         INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('a1', 'Someone', 'someone', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES \
         ('al0', 'r1', 'g0', 'Album', 'album', 'a1', 'automatic', {before}, {before}), \
         ('al1', 'r1', 'g1', 'Album', 'album', 'a1', 'automatic', {arrived}, {arrived}), \
         ('al2', 'r1', 'g2', 'Album', 'album', 'a1', 'automatic', {later}, {later}); \
         INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at) VALUES \
         ('al0', 'musicbrainz', '{PICK}', 'manual', 1), \
         ('al1', 'musicbrainz', '{PICK}', 'manual', 1), \
         ('al2', 'musicbrainz', '{PICK}', 'manual', 1); \
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
         album_title, album_title_folded, file_format, availability, ingest_source, \
         imported_at, membership_source) VALUES ('t0', 'al0', 'r1', '/music/t0.flac', \
         't0.flac', 'h0', 1, 1, 's0', 'Song', 'song', 'Album', 'album', 'flac', \
         'indexed', 'scan', 1, 'automatic'), ('t1', 'al1', 'r1', '/music/t1.flac', \
         't1.flac', 'h1', 1, 1, 's1', 'Song', 'song', 'Album', 'album', 'flac', \
         'indexed', 'scan', 1, 'automatic'), ('t2', 'al2', 'r1', '/music/t2.flac', \
         't2.flac', 'h2', 1, 1, 's2', 'Song', 'song', 'Album', 'album', 'flac', \
         'indexed', 'scan', 1, 'automatic'); \
         INSERT INTO library_play_history (id, user_id, local_album_id, track_name, \
         artist_name, release_group_mbid, played_at) VALUES \
         ('p0', 'user-1', 'al1', 'Song', 'Someone', NULL, {arrived} - 1), \
         ('p1', 'user-1', 'al1', 'Song', 'Someone', NULL, {arrived} + {DAY}), \
         ('p2', 'user-1', NULL, 'Song', 'Someone', upper('{PICK}'), {arrived} + 10 * {DAY}), \
         ('p3', 'user-1', 'al2', 'Song', 'Someone', NULL, {arrived} + 40 * {DAY}), \
         ('p4', 'user-1', 'al1', 'Song', 'Someone', NULL, {arrived} + 100 * {DAY}), \
         ('p5', 'user-2', 'al1', 'Song', 'Someone', NULL, {arrived} + 2 * {DAY}); \
         INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind, \
         created_at) VALUES ('user-1', 'artist', '{ARTIST}', 'not_artist', {asked}), \
         ('user-1', 'album', '{PICK}', 'know_it', {arrived}), \
         ('user-2', 'album', '{UNASKED}', 'no', {asked});",
        before = offered - DAY,
        later = arrived + DAY,
    ));
    seed(&db, rows).await;

    let now = arrived + 120 * DAY;
    refresh_pick_outcomes(&db, "user-1", now)
        .await
        .expect("the job runs");
    let outcome = |group: &str| -> Outcome {
        db.row(
            "SELECT requested_at, requested_from, arrived_at, first_played_at, plays_30d, \
             plays_90d, feedback_kind, feedback_at FROM catalog_discover_picks \
             WHERE user_id = 'user-1' AND release_group_mbid_lower = ?1",
            [group],
        )
    };
    assert_eq!(
        outcome(PICK),
        (
            Some(asked),
            Some("deck".to_owned()),
            Some(arrived),
            Some(arrived + DAY),
            2,
            3,
            Some("know_it".to_owned()),
            Some(arrived),
        )
    );
    assert_eq!(
        outcome(UNASKED),
        (
            None,
            None,
            None,
            None,
            0,
            0,
            Some("not_artist".to_owned()),
            Some(asked)
        ),
        "an ask before the offer is not its outcome; the artist's answer counts"
    );

    // Taking an answer back clears it; a request and an arrival stay.
    seed(
        &db,
        format!(
            "DELETE FROM catalog_discover_feedback WHERE user_id = 'user-1'; \
             DELETE FROM request_history WHERE musicbrainz_id_lower = '{PICK}'; \
             DELETE FROM library_play_history WHERE id = 'p1';"
        ),
    )
    .await;
    refresh_pick_outcomes(&db, "user-1", now)
        .await
        .expect("the job runs again");
    assert_eq!(
        outcome(PICK),
        (
            Some(asked),
            Some("deck".to_owned()),
            Some(arrived),
            Some(arrived + 10 * DAY),
            1,
            2,
            None,
            None,
        )
    );
}

/// A long history of many arrived picks and many plays still finishes the
/// job, each pick's count right. The plays are one pass over the person's
/// plays per branch, each pick's stopping at its 90 days, read on the long
/// pool outside any write.
#[tokio::test]
async fn a_long_history_still_finishes() {
    const PICKS: i64 = 300;
    let rig = AuthRig::new("discover-record-heavy").await;
    let db = rig.platform.db.clone();
    people(&db).await;
    let offered = rig.clock().now_ms();
    let arrived = offered + DAY;
    seed(
        &db,
        format!(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
             updated_at) VALUES ('a1', 'Someone', 'someone', 'group', 1, 1); \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {PICKS}) \
             INSERT INTO catalog_discover_picks (user_id, release_group_mbid_lower, \
             release_group_mbid, first_surface, first_offered_at, last_offered_at, evidence, \
             first_shown_at, last_shown_at, shown_days) \
             SELECT 'user-1', printf('dddddddd-0000-4000-8000-%012d', i), \
             printf('dddddddd-0000-4000-8000-%012d', i), 'deck', {offered}, {offered}, '{{}}', \
             {offered}, {offered}, 1 FROM n; \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {PICKS}) \
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) \
             SELECT 'hal' || i, 'r1', 'hg' || i, 'Album', 'album', 'a1', 'automatic', \
             {arrived}, {arrived} FROM n; \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) \
             SELECT id, 'musicbrainz', printf('dddddddd-0000-4000-8000-%012d', \
             CAST(substr(id, 4) AS INTEGER)), 'manual', 1 FROM local_albums \
             WHERE id LIKE 'hal%'; \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
             relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, \
             title_folded, album_title, album_title_folded, file_format, availability, \
             ingest_source, imported_at, membership_source) \
             SELECT 'ht' || substr(id, 4), id, 'r1', '/music/' || id || '.flac', \
             id || '.flac', 'hh' || id, 1, 1, 'hs' || id, 'Song', 'song', 'Album', 'album', \
             'flac', 'indexed', 'scan', 1, 'automatic' FROM local_albums WHERE id LIKE 'hal%'; \
             WITH RECURSIVE k(j) AS (SELECT 0 UNION ALL SELECT j + 1 FROM k WHERE j < 129) \
             INSERT INTO library_play_history (id, user_id, local_album_id, track_name, \
             artist_name, played_at) \
             SELECT 'hp' || a.id || '-' || j, 'user-1', a.id, 'Song', 'Someone', \
             CASE WHEN j < 10 THEN {arrived} + j * 5 * {DAY} \
             ELSE {arrived} + 100 * {DAY} + j * 3600000 END \
             FROM local_albums a, k WHERE a.id LIKE 'hal%';"
        ),
    )
    .await;
    let plays: i64 = db.one("SELECT COUNT(*) FROM library_play_history", []);
    assert_eq!(plays, PICKS * 130);

    refresh_pick_outcomes(&db, "user-1", arrived + 120 * DAY)
        .await
        .expect("the job finishes");
    let counts: Vec<(i64, i64, i64, i64)> = db.rows(
        "SELECT COUNT(*), first_played_at, plays_30d, plays_90d FROM catalog_discover_picks \
         GROUP BY first_played_at, plays_30d, plays_90d",
        [],
    );
    assert_eq!(
        counts,
        [(PICKS, arrived, 6, 10)],
        "every pick: first played on arrival, 6 plays in 30 days, 10 in 90"
    );
}

/// A row never shown goes 30 days after it was last offered, a row shown
/// goes 13 months after it was last offered or shown, and a row with any
/// outcome stays however old it is.
#[tokio::test]
async fn the_retention_keeps_what_the_person_did() {
    let rig = AuthRig::new("discover-record-retention").await;
    let db = rig.platform.db.clone();
    people(&db).await;
    let now = rig.clock().now_ms();
    let group = |n: u8| format!("cccccccc-0000-4000-8000-0000000000{n:02}");
    let mut rows = String::new();
    rows.push_str(&pick("user-1", &group(1), now - 31 * DAY, None));
    rows.push_str(&pick("user-1", &group(2), now - 29 * DAY, None));
    rows.push_str(&pick(
        "user-1",
        &group(3),
        now - 800 * DAY,
        Some(now - 397 * DAY),
    ));
    rows.push_str(&pick(
        "user-1",
        &group(4),
        now - 800 * DAY,
        Some(now - 360 * DAY),
    ));
    rows.push_str(&pick("user-1", &group(5), now - 800 * DAY, None));
    rows.push_str(&format!(
        "UPDATE catalog_discover_picks SET opened_at = 1 \
         WHERE release_group_mbid_lower = '{}';",
        group(5)
    ));
    rows.push_str(&pick("user-2", &group(1), now - 31 * DAY, None));
    seed(&db, rows).await;

    let removed = refresh_pick_outcomes(&db, "user-1", now)
        .await
        .expect("the job runs");
    assert_eq!(removed, 2);
    let kept: Vec<String> = db.column(
        "SELECT user_id || ':' || substr(release_group_mbid_lower, -2) \
         FROM catalog_discover_picks ORDER BY 1",
        [],
    );
    assert_eq!(
        kept,
        ["user-1:02", "user-1:04", "user-1:05", "user-2:01"],
        "only the person's own rows are judged"
    );
}

/// With the warm cycle off the clock queues nothing, and a person's own
/// visit still queues their outcomes job, as the production builder wires
/// it; someone with no picks gets none. With the cycle on, the clock
/// queues one for each person with picks.
#[tokio::test]
async fn a_visit_queues_the_job_with_the_warm_cycle_off() {
    let dir = crate::common::ScratchDir::new("discover-record-due");
    let rig = AuthRig::new("discover-record-due").await;
    let db = rig.platform.db.clone();
    people(&db).await;
    let now = rig.clock().now_ms();
    seed(
        &db,
        pick("user-1", PICK, now, None) + &pick("user-2", PICK, now, None),
    )
    .await;
    let jobs = |db: &Db| -> Vec<String> {
        db.column(
            "SELECT idempotency_key FROM durable_jobs WHERE kind = 'discover.outcomes' \
             ORDER BY idempotency_key",
            [],
        )
    };

    let deps = discover_deps(&rig, &dir, Providers::closed(), |_| {});
    deps.content.run_due_tick().await;
    assert_eq!(jobs(&db), Vec::<String>::new(), "the clock is off");
    deps.content
        .record_activity("user-1", Feature::Queue, ArtistDetail::default())
        .await
        .expect("activity recorded");
    after_commits(&db, "the visit queued the job", || async {
        (jobs(&db) == ["user-1"]).then_some(())
    })
    .await;

    let on = Due::new(
        db.clone(),
        JobQueue::new(db.clone(), rig.platform.clock.clone()),
        Arc::clone(&rig.platform.clock) as Arc<dyn Clock>,
        true,
        (
            FactJobs::new(
                KeptFacts::new(db.clone(), rig.platform.clock.clone()),
                Arc::new(crate::discover_queue::Script::default()),
            )
            .due(),
            crate::common::history_store(&rig.platform),
            crate::discover_queue::fresh_feed(&rig.platform),
        ),
    );
    on.visit("nobody").await;
    on.clock_pass().await;
    assert_eq!(jobs(&db), ["user-1", "user-2"]);
}

/// Forgetting takes every row of one person's and leaves another's.
#[tokio::test]
async fn forget_takes_one_persons_rows() {
    let rig = AuthRig::new("discover-record-forget").await;
    let db = rig.platform.db.clone();
    people(&db).await;
    let now = rig.clock().now_ms();
    seed(
        &db,
        pick("user-1", PICK, now, Some(now))
            + &pick("user-1", UNASKED, now, None)
            + &pick("user-2", PICK, now, None),
    )
    .await;
    assert_eq!(record::forget(&db, "user-1").await.expect("forget"), 2);
    let kept: Vec<String> = db.column("SELECT user_id FROM catalog_discover_picks", []);
    assert_eq!(kept, ["user-2"]);
}
