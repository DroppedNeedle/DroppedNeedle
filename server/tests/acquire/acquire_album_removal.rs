//! A removed album's clean-up, run as the job its removal queued.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::library_removal::{AcquireAlbumCleanup, SPEC};
use droppedneedle_acquire::wiring::AcquireStores;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::jobs::queue::{Handlers, JobQueue, State, start_runners};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_library::mutations::{ALBUM_REMOVED, AlbumRemovedJob};

use crate::common::TestPlatform;
use crate::common::bundles::quiet_ticks;

/// The rows the album's clean-up may touch, as one line each.
fn rows(db: &Path) -> Vec<String> {
    let conn = droppedneedle_kernel::db::open_connection(db).expect("db opens");
    let mut lines = Vec::new();
    for sql in [
        "SELECT id || ' ' || status FROM download_tasks ORDER BY id",
        "SELECT path || ' ' || state FROM acquire_held_files ORDER BY path",
        "SELECT identity FROM download_quarantine ORDER BY identity",
        "SELECT state FROM wanted_watches",
    ] {
        let mut statement = conn.prepare(sql).expect("query");
        let found = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows read");
        lines.extend(found);
    }
    lines
}

/// Every watch's state, by album.
fn watch_states(conn: &rusqlite::Connection) -> Vec<(String, String)> {
    let mut statement = conn
        .prepare("SELECT release_group_mbid_lower, state FROM wanted_watches ORDER BY 1")
        .expect("query");
    statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows read")
}

/// A platform on `db` with the clean-up's runner started.
async fn start(db: &AcquireDb) -> (Platform, JobQueue) {
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let platform = TestPlatform::on(db.db(), &dir).build().await;
    let queue = JobQueue::new(platform.db.clone(), platform.clock.clone());
    start_runners(
        &queue,
        &platform.supervisor,
        Handlers::default().with(
            SPEC,
            Arc::new(AcquireAlbumCleanup::new(&AcquireStores::open(
                &platform,
                quiet_ticks(),
            ))),
        ),
    )
    .expect("runner starts");
    (platform, queue)
}

/// Queue the clean-up under `key`; answers the queued row.
async fn enqueue(
    db: &droppedneedle_kernel::db::Db,
    queue: &JobQueue,
    key: &str,
    job: &AlbumRemovedJob,
) -> i64 {
    let (queue_in, key_in, job_in) = (queue.clone(), key.to_owned(), job.clone());
    db.write(Lane::Foreground, "test.enqueue", move |tx| {
        queue_in.enqueue(tx, ALBUM_REMOVED, &key_in, &job_in)
    })
    .await
    .expect("job queued")
    .id
}

/// Wait for row `id` of `key` to finish.
async fn wait_done(queue: &JobQueue, key: &str, id: i64) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let latest = queue
            .latest(ALBUM_REMOVED, key)
            .await
            .expect("job reads")
            .expect("job row");
        if latest.id == id && latest.state == State::Done {
            return;
        }
        assert!(Instant::now() < deadline, "the clean-up never finished");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Queue the clean-up under `key` and wait for that run to finish.
async fn run_job(
    db: &droppedneedle_kernel::db::Db,
    queue: &JobQueue,
    key: &str,
    job: &AlbumRemovedJob,
) {
    let id = enqueue(db, queue, key, job).await;
    wait_done(queue, key, id).await;
}

/// A removal time with a fraction, in a second already past.
fn removal_time() -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_secs_f64();
    now.floor() - 0.5
}

/// A clean-up job acts only on what the album had when it was removed: a
/// request made since keeps its task, held file and watch, even in the
/// same held entry. Run again with
/// the same payload, as a restored backup re-runs a finished job, it
/// changes nothing.
#[tokio::test]
async fn album_cleanup_leaves_what_came_after_the_removal() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("fan", "Fan", "user").await.expect("user");
    let (platform, queue) = start(&db).await;

    let removed_at = removal_time();
    let (before, after) = (removed_at - 60.0, removed_at + 60.0);
    let after_ms = millis(after);
    let held_dir = platform.config.cache_dir.join("held");
    std::fs::create_dir_all(&held_dir).expect("held folder");
    let old_held = held_dir.join("held-old.flac");
    let new_held = held_dir.join("held-new.flac");
    std::fs::write(&old_held, b"old").expect("old held file");
    std::fs::write(&new_held, b"new").expect("new held file");
    let conn = droppedneedle_kernel::db::open_connection(db.path()).expect("db opens");
    conn.execute_batch(&format!(
        "INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, album_title, \
         status, created_at, updated_at) VALUES \
         ('task-old', 'fan', 'rg-1', 'Band', 'LP', 'failed', {before_ms}, {before_ms}), \
         ('task-new', 'fan', 'rg-1', 'Band', 'LP', 'failed', {after_ms}, {after_ms}); \
         INSERT INTO acquire_held_entries (id, user_id, origin, group_key, release_group_mbid, \
         state, created_at, expires_at) \
         VALUES (1, 'fan', 'download', 'album:rg-1', 'rg-1', 'held', {before_ms}, {after_ms}); \
         INSERT INTO acquire_held_files (entry_id, root, path, original_name, source, reason, \
         state, created_at) \
         VALUES (1, 'held', 'held-old.flac', '01.flac', 'soulseek', 'weak_match', 'held', \
         {before_ms}), \
         (1, 'held', 'held-new.flac', '01.flac', 'soulseek', 'weak_match', 'held', {after_ms}); \
         INSERT INTO download_quarantine (identity, release_group_mbid, reason, quarantined_at) \
         VALUES ('peer-old', 'rg-1', 'corrupt', {before_ms}), \
         ('peer-new', 'rg-1', 'corrupt', {after_ms}); \
         INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, user_id, \
         artist_name, album_title, kind, state, created_at, next_check_at) \
         VALUES ('rg-1', 'rg-1', 'fan', 'Band', 'LP', 'missing', 'watching', {after_ms}, \
         {after_ms});",
        before_ms = millis(before),
    ))
    .expect("rows seeded");

    let job = AlbumRemovedJob {
        release_group_mbid: "rg-1".to_owned(),
        stop_wanted: true,
        removed_at,
    };

    run_job(&platform.db, &queue, "removal-1", &job).await;
    let kept = vec![
        "task-new failed".to_owned(),
        "task-old cancelled".to_owned(),
        "held-new.flac held".to_owned(),
        "held-old.flac discarded".to_owned(),
        "peer-new".to_owned(),
        "watching".to_owned(),
    ];
    assert_eq!(rows(db.path()), kept, "only the album's old rows went");
    assert!(!old_held.exists(), "the old held file went");
    assert!(new_held.exists(), "the new request's held file stays");

    run_job(&platform.db, &queue, "removal-1", &job).await;
    assert_eq!(rows(db.path()), kept, "a second run changes nothing");
    assert!(new_held.exists(), "the new request's held file still stays");
}

/// The clean-up claims the entries before it deletes their copies, as a
/// person's action does: an entry a person is acting on keeps its rows and
/// copy, nobody can take up an entry while its copies go, and a file held
/// into a claimed entry after the claim keeps its row and its copy. Only
/// the rows whose copies are gone are discarded.
#[tokio::test]
async fn album_cleanup_claims_before_it_deletes() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("fan", "Fan", "user").await.expect("user");
    let (platform, queue) = start(&db).await;
    let removed_at = removal_time();
    let before_ms = millis(removed_at - 60.0);
    let held_dir = platform.config.cache_dir.join("held");
    std::fs::create_dir_all(&held_dir).expect("held folder");
    // Many old copies keep the handler busy deleting them between its read
    // and its write, so the test can hold a file in that window.
    let old_held: Vec<_> = (0..OLD_COPIES)
        .map(|n| held_dir.join(format!("held-old-{n:04}.flac")))
        .collect();
    for path in &old_held {
        std::fs::write(path, b"old").expect("old held file");
    }
    let late_held = held_dir.join("held-late.flac");
    std::fs::write(&late_held, b"late").expect("late held file");
    let taken_held = held_dir.join("held-taken.flac");
    std::fs::write(&taken_held, b"taken").expect("taken held file");
    let conn = droppedneedle_kernel::db::open_connection(db.path()).expect("db opens");
    conn.execute_batch(&format!(
        "INSERT INTO acquire_held_entries (id, user_id, origin, group_key, release_group_mbid, \
         state, created_at, expires_at) \
         VALUES (1, 'fan', 'download', 'album:rg-1', 'rg-1', 'held', {before_ms}, \
         {expires_ms}), \
         (2, 'fan', 'download', 'album:rg-1:taken', 'rg-1', 'placing', {before_ms}, \
         {expires_ms}); \
         INSERT INTO acquire_held_files (entry_id, root, path, original_name, source, reason, \
         state, created_at) \
         VALUES (2, 'held', 'held-taken.flac', '01.flac', 'soulseek', 'weak_match', 'held', \
         {before_ms}); \
         WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < {OLD_COPIES}) \
         INSERT INTO acquire_held_files (entry_id, root, path, original_name, source, reason, \
         state, created_at) \
         SELECT 1, 'held', printf('held-old-%04d.flac', i), '01.flac', 'soulseek', \
         'weak_match', 'held', {before_ms} FROM n;",
        expires_ms = millis(removed_at + 86_400.0),
    ))
    .expect("rows seeded");

    let job = AlbumRemovedJob {
        release_group_mbid: "rg-1".to_owned(),
        stop_wanted: true,
        removed_at,
    };
    let id = enqueue(&platform.db, &queue, "removal-1", &job).await;
    // Once the handler has claimed the first entry, hold the writer (so its
    // last write cannot commit) until it has started deleting the copies,
    // then hold the late file and try to take the entry up as a person would.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let old = old_held.clone();
        let unclaimed = platform
            .db
            .write(Lane::Foreground, "test.hold_late", move |tx| {
                let state: String = tx
                    .query_row(
                        "SELECT state FROM durable_jobs WHERE id = ?1",
                        [id],
                        |row| row.get(0),
                    )
                    .map_err(DbError::from)?;
                let claimed: String = tx
                    .query_row(
                        "SELECT state FROM acquire_held_entries WHERE id = 1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(DbError::from)?;
                if state != "running" || claimed != "placing" {
                    return Ok::<_, DbError>(Some(state));
                }
                let wait = Instant::now() + Duration::from_secs(10);
                while old.iter().all(|path| path.exists()) && Instant::now() < wait {
                    std::thread::sleep(Duration::from_millis(1));
                }
                tx.execute(
                    "INSERT INTO acquire_held_files (entry_id, root, path, original_name, \
                     source, reason, state, created_at) VALUES (1, 'held', 'held-late.flac', \
                     '02.flac', 'soulseek', 'weak_match', 'held', ?1)",
                    [before_ms],
                )
                .map_err(DbError::from)?;
                let took = tx
                    .execute(
                        "UPDATE acquire_held_entries SET state = 'placing' \
                         WHERE id = 1 AND state = 'held'",
                        [],
                    )
                    .map_err(DbError::from)?;
                assert_eq!(took, 0, "a person took up an entry whose copies were going");
                Ok(None)
            })
            .await
            .expect("test write");
        match unclaimed {
            None => break,
            Some(state) => assert!(
                state == "queued" || state == "running",
                "the job finished before it was held: {state}"
            ),
        }
        assert!(Instant::now() < deadline, "the clean-up was never claimed");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    wait_done(&queue, "removal-1", id).await;

    let states: Vec<(i64, bool, String, i64)> = conn
        .prepare(
            "SELECT entry_id, path = 'held-late.flac', state, COUNT(*) \
             FROM acquire_held_files GROUP BY 1, 2, 3 ORDER BY 1, 2",
        )
        .expect("query")
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("rows read");
    assert_eq!(
        states,
        vec![
            (1, false, "discarded".to_owned(), OLD_COPIES),
            (1, true, "held".to_owned(), 1),
            (2, false, "held".to_owned(), 1),
        ],
        "only the rows whose copies went, of entries it claimed, were discarded"
    );
    let taken: String = conn
        .query_row(
            "SELECT state FROM acquire_held_entries WHERE id = 2",
            [],
            |row| row.get(0),
        )
        .expect("entry reads");
    assert_eq!(
        taken, "placing",
        "the entry a person took up is left to them"
    );
    assert!(
        old_held.iter().all(|path| !path.exists()),
        "the held files read went"
    );
    assert!(late_held.exists(), "the file held since stays");
    assert!(taken_held.exists(), "the copy a person is acting on stays");
    let first: String = conn
        .query_row(
            "SELECT state FROM acquire_held_entries WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .expect("entry reads");
    assert_eq!(first, "held", "the claimed entry is given back");
}

/// Old held copies in the read-then-write test.
const OLD_COPIES: i64 = 400;

/// A watch is enrolled in whole seconds: one armed a second before the
/// removal stops, one armed in the removal's own second (it may have come
/// after) and one armed a second after keep watching.
#[tokio::test]
async fn album_cleanup_stops_only_a_watch_armed_before_the_removal() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("fan", "Fan", "user").await.expect("user");
    let (platform, queue) = start(&db).await;
    let removed_at = removal_time();
    let (before_ms, same_ms, after_ms) = (
        millis(removed_at.floor() - 1.0),
        millis(removed_at.floor()),
        millis(removed_at.floor() + 1.0),
    );
    let conn = droppedneedle_kernel::db::open_connection(db.path()).expect("db opens");
    conn.execute_batch(&format!(
        "INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, user_id, \
         artist_name, album_title, kind, state, created_at, next_check_at) VALUES \
         ('rg-early', 'rg-early', 'fan', 'Band', 'Early', 'missing', 'watching', {before_ms}, \
         {before_ms}), \
         ('rg-same', 'rg-same', 'fan', 'Band', 'Same', 'missing', 'watching', {same_ms}, \
         {same_ms}), \
         ('rg-late', 'rg-late', 'fan', 'Band', 'Late', 'missing', 'watching', {after_ms}, \
         {after_ms});"
    ))
    .expect("watches seeded");

    for group in ["rg-early", "rg-same", "rg-late"] {
        let job = AlbumRemovedJob {
            release_group_mbid: group.to_owned(),
            stop_wanted: true,
            removed_at,
        };
        run_job(&platform.db, &queue, &format!("removal-{group}"), &job).await;
    }
    assert_eq!(
        watch_states(&conn),
        vec![
            ("rg-early".to_owned(), "stopped".to_owned()),
            ("rg-late".to_owned(), "watching".to_owned()),
            ("rg-same".to_owned(), "watching".to_owned()),
        ]
    );
}

/// A removal that wants a replacement rearms a watch fulfilled before it,
/// once: a run again leaves the rearmed watch as it is. A watch fulfilled
/// in the removal's own second and a stopped watch stay as they were.
#[tokio::test]
async fn album_cleanup_rearms_a_watch_fulfilled_before_the_removal() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("fan", "Fan", "user").await.expect("user");
    let (platform, queue) = start(&db).await;
    let removed_at = removal_time();
    let (before_ms, same_ms) = (millis(removed_at - 60.0), millis(removed_at.floor()));
    let conn = droppedneedle_kernel::db::open_connection(db.path()).expect("db opens");
    conn.execute_batch(&format!(
        "INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, user_id, \
         artist_name, album_title, kind, state, created_at, next_check_at, check_count) VALUES \
         ('rg-done', 'rg-done', 'fan', 'Band', 'Done', 'missing', 'fulfilled', {before_ms}, \
         {before_ms}, 4), \
         ('rg-same', 'rg-same', 'fan', 'Band', 'Same', 'missing', 'fulfilled', {same_ms}, \
         {same_ms}, 4), \
         ('rg-stopped', 'rg-stopped', 'fan', 'Band', 'Stopped', 'missing', 'stopped', \
         {before_ms}, {before_ms}, 4);"
    ))
    .expect("watches seeded");

    for group in ["rg-done", "rg-same", "rg-stopped"] {
        let job = AlbumRemovedJob {
            release_group_mbid: group.to_owned(),
            stop_wanted: false,
            removed_at,
        };
        run_job(&platform.db, &queue, &format!("removal-{group}"), &job).await;
    }
    assert_eq!(
        watch_states(&conn),
        vec![
            ("rg-done".to_owned(), "watching".to_owned()),
            ("rg-same".to_owned(), "fulfilled".to_owned()),
            ("rg-stopped".to_owned(), "stopped".to_owned()),
        ]
    );
    let rearm = |conn: &rusqlite::Connection| -> (i64, i64) {
        conn.query_row(
            "SELECT created_at, check_count FROM wanted_watches \
             WHERE release_group_mbid_lower = 'rg-done'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("watch reads")
    };
    let first = rearm(&conn);
    assert!(first.0 > before_ms, "the rearm stamps the watch");
    assert_eq!(first.1, 0, "the rearm resets the counters");
    conn.execute(
        "UPDATE wanted_watches SET state = 'fulfilled', check_count = 2 \
         WHERE release_group_mbid_lower = 'rg-done'",
        [],
    )
    .expect("watch fulfilled again");

    let job = AlbumRemovedJob {
        release_group_mbid: "rg-done".to_owned(),
        stop_wanted: false,
        removed_at,
    };
    run_job(&platform.db, &queue, "removal-rg-done-again", &job).await;
    assert_eq!(
        rearm(&conn),
        (first.0, 2),
        "a run again does not rearm the watch's own rearm"
    );
}

/// Epoch seconds as a watch stores them.
fn millis(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}
