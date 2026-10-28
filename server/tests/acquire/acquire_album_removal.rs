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
/// request made since keeps its task, held file and watch, even in the
/// same held entry. Run again with
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
                &platform,
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
