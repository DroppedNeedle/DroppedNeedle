//! A removed album's clean-up, run as the job its removal queued.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::library_removal::{AcquireAlbumCleanup, SPEC};
use droppedneedle_acquire::wiring::AcquireStores;
use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::jobs::queue::{Handlers, JobQueue, State, start_runners};
use droppedneedle_library::mutations::{ALBUM_REMOVED, AlbumRemovedJob};

use crate::common::TestPlatform;
use crate::common::bundles::quiet_ticks;

/// The rows the album's clean-up may touch, as one line each.
fn rows(db: &Path) -> Vec<String> {
    let conn = droppedneedle_kernel::db::open_connection(db).expect("db opens");
    let mut lines = Vec::new();
    for sql in [
        "SELECT id || ' ' || status FROM download_tasks ORDER BY id",
        "SELECT held_path FROM held_imports ORDER BY held_path",
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

/// Queue the clean-up under `key` and wait for that run to finish.
async fn run_job(
    db: &droppedneedle_kernel::db::Db,
    queue: &JobQueue,
    key: &str,
    job: &AlbumRemovedJob,
) {
    let (queue_in, key_in, job_in) = (queue.clone(), key.to_owned(), job.clone());
    let queued = db
        .write(Lane::Foreground, "test.enqueue", move |tx| {
            queue_in.enqueue(tx, ALBUM_REMOVED, &key_in, &job_in)
        })
        .await
        .expect("job queued");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let latest = queue
            .latest(ALBUM_REMOVED, key)
            .await
            .expect("job reads")
            .expect("job row");
        if latest.id == queued.id && latest.state == State::Done {
            return;
        }
        assert!(Instant::now() < deadline, "the clean-up never finished");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A clean-up job acts only on what the album had when it was removed: a
/// request made since keeps its task, held file and watch. Run again with
/// the same payload, as a restored backup re-runs a finished job, it
/// changes nothing.
#[tokio::test]
async fn album_cleanup_leaves_what_came_after_the_removal() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("fan", "Fan", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let platform = TestPlatform::on(db.db(), &dir).build().await;

    let removed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_secs_f64();
    let (before, after) = (removed_at - 60.0, removed_at + 60.0);
    let after_ms = millis(after);
    let old_held = dir.join("held-old.flac");
    let new_held = dir.join("held-new.flac");
    std::fs::write(&old_held, b"old").expect("old held file");
    std::fs::write(&new_held, b"new").expect("new held file");
    let conn = droppedneedle_kernel::db::open_connection(db.path()).expect("db opens");
    conn.execute_batch(&format!(
        "INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name, album_title, \
         status, created_at, updated_at) VALUES \
         ('task-old', 'fan', 'rg-1', 'Band', 'LP', 'failed', {before}, {before}), \
         ('task-new', 'fan', 'rg-1', 'Band', 'LP', 'failed', {after}, {after}); \
         INSERT INTO held_imports (user_id, release_group_mbid, held_path, reason, created_at) \
         VALUES ('fan', 'rg-1', '{old}', 'verify_failed', {before}), \
         ('fan', 'rg-1', '{new}', 'verify_failed', {after}); \
         INSERT INTO download_quarantine (identity, release_group_mbid, reason, quarantined_at) \
         VALUES ('peer-old', 'rg-1', 'corrupt', {before}), \
         ('peer-new', 'rg-1', 'corrupt', {after}); \
         INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, user_id, \
         artist_name, album_title, kind, state, created_at, next_check_at) \
         VALUES ('rg-1', 'rg-1', 'fan', 'Band', 'LP', 'missing', 'watching', {after_ms}, \
         {after_ms});",
        old = old_held.display(),
        new = new_held.display(),
    ))
    .expect("rows seeded");

    let queue = JobQueue::new(platform.db.clone(), platform.clock.clone());
    start_runners(
        &queue,
        &platform.supervisor,
        Handlers::default().with(
            SPEC,
            Arc::new(AcquireAlbumCleanup::new(&AcquireStores::open(
                &platform.db,
                quiet_ticks(),
            ))),
        ),
    )
    .expect("runner starts");
    let job = AlbumRemovedJob {
        release_group_mbid: "rg-1".to_owned(),
        stop_wanted: true,
        removed_at,
    };

    run_job(&platform.db, &queue, "removal-1", &job).await;
    let kept = vec![
        "task-new failed".to_owned(),
        "task-old cancelled".to_owned(),
        new_held.display().to_string(),
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

/// Watch times are milliseconds and a removal's time is seconds: a watch
/// armed a second before the removal stops, one armed a second after it
/// keeps watching.
#[tokio::test]
async fn album_cleanup_stops_only_a_watch_armed_before_the_removal() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("fan", "Fan", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let platform = TestPlatform::on(db.db(), &dir).build().await;
    let removed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_secs_f64();
    let (before_ms, after_ms) = (millis(removed_at - 1.0), millis(removed_at + 1.0));
    let conn = droppedneedle_kernel::db::open_connection(db.path()).expect("db opens");
    conn.execute_batch(&format!(
        "INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, user_id, \
         artist_name, album_title, kind, state, created_at, next_check_at) VALUES \
         ('rg-early', 'rg-early', 'fan', 'Band', 'Early', 'missing', 'watching', {before_ms}, \
         {before_ms}), \
         ('rg-late', 'rg-late', 'fan', 'Band', 'Late', 'missing', 'watching', {after_ms}, \
         {after_ms});"
    ))
    .expect("watches seeded");

    let queue = JobQueue::new(platform.db.clone(), platform.clock.clone());
    start_runners(
        &queue,
        &platform.supervisor,
        Handlers::default().with(
            SPEC,
            Arc::new(AcquireAlbumCleanup::new(&AcquireStores::open(
                &platform.db,
                quiet_ticks(),
            ))),
        ),
    )
    .expect("runner starts");
    for (key, group) in [("removal-early", "rg-early"), ("removal-late", "rg-late")] {
        let job = AlbumRemovedJob {
            release_group_mbid: group.to_owned(),
            stop_wanted: true,
            removed_at,
        };
        run_job(&platform.db, &queue, key, &job).await;
    }
    let mut statement = conn
        .prepare("SELECT release_group_mbid_lower, state FROM wanted_watches ORDER BY 1")
        .expect("query");
    let states = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows read");
    assert_eq!(
        states,
        vec![
            ("rg-early".to_owned(), "stopped".to_owned()),
            ("rg-late".to_owned(), "watching".to_owned()),
        ]
    );
}

/// Epoch seconds as a watch stores them.
fn millis(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}
