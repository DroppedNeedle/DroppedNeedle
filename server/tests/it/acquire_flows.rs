//! Acquisition flows over the durable stores: the wanted watcher, status
//! sync, follow poll, upgrade sweep, drop import and free music.
//!
//! Time is a manual clock throughout: cadences assert by advancing the
//! clock, never by sleeping. Stores sit on a scratch database; downloads,
//! search and polls are scripted seams.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle::acquire::db::AcquireDb;
use droppedneedle::acquire::flows::{
    self,
    loops::{
        FollowDeps, LoopState, ManualSleeper, PruneDeps, PruneSettings, SYNC_INITIAL_DELAY,
        SYNC_INTERVAL, SYNC_JOB, SweepDeps, SyncDeps, WantedDeps, WantedSettings, follow_tick,
        interval_seconds, prune_tick, spawn_sync_loop, sweep_tick, sync_tick, wanted_tick,
    },
    operations::{
        DROP_IMPORT_JOB, DropImportDeps, FreeMusicDeps, FreeMusicOutcome, FreeMusicRequest,
        OpState, OpStore, ResolveDecision, create_drop_job, process_drop_job, register_durable_ops,
        resolve_quarantined_item, run_free_music, settle_free_music, title_containment,
    },
    seams::{
        Candidate, DispatchKind, ManualClock, MemoryHandoff, MemoryOrganise, MemoryTicks,
        ObservedRelease, ScriptedDownloads, ScriptedPoll, ScriptedSearch, ScriptedVerify,
        SystemClock, VerifyVerdict,
    },
    stores::{
        AdminDirectory, FollowStore, LibraryPresence, MemoryWorklist, QuarantineStore, UpgradeItem,
        UpgradePolicy,
    },
};
use droppedneedle::acquire::requests::{
    ledger::{BeginOutcome, RequestRecord},
    models::RequestKind,
    sqlite::{RequestStore, WantedStore},
};
use droppedneedle::acquire::search_jobs::store::JobStore;
use droppedneedle::db::{DbConfig, DbRuntime, JobState, open_runtime};
use droppedneedle::runtime_config::sections::UserPreferences;

const NOW: i64 = 1_700_000_000;
/// Scratch directory, removed when the test ends.
fn scratch_dir(name: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(name)
}

/// A served runtime for the tests that need the job registry.
async fn runtime(name: &str) -> (DbRuntime, AcquireDb, crate::common::ScratchDir) {
    let dir = scratch_dir(name);
    let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
        .await
        .unwrap();
    let db = AcquireDb::from_runtime(&runtime);
    (runtime, db, dir)
}

/// One request row in `status`, owned by `user-1`.
async fn seed_request(store: &RequestStore, key: &str, status: &str, task: Option<&str>) {
    let record = RequestRecord {
        key: key.to_owned(),
        kind: RequestKind::Album,
        status: status.to_owned(),
        artist_name: "Massive Attack".to_owned(),
        album_title: "Blue Lines".to_owned(),
        artist_mbid: None,
        year: None,
        release_mbid: None,
        track_title: None,
        duration_seconds: None,
        track_release_group_mbid: None,
        user_id: Some("user-1".to_owned()),
        requested_by_name: None,
        requesters: Vec::new(),
        requested_at: NOW as u64,
        completed_at: None,
        task_id: task.map(str::to_owned),
        generation: 0,
        dispatch_authorized: true,
        monitor_artist: false,
        auto_download_artist: false,
        reviewed_by_name: None,
        reviewed_at: None,
    };
    let outcomes = store
        .begin_batch(vec![record], "user-1", None)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(outcomes[0], BeginOutcome::Won(_)));
}

async fn status_of(store: &RequestStore, key: &str) -> String {
    store
        .get(RequestKind::Album, key)
        .await
        .unwrap()
        .unwrap()
        .status
}

struct Wanted {
    deps: WantedDeps,
    search: Arc<ScriptedSearch>,
    downloads: Arc<ScriptedDownloads>,
    library: Arc<LibraryPresence>,
    ticks: Arc<MemoryTicks>,
}

fn wanted(db: &AcquireDb) -> Wanted {
    let search = Arc::new(ScriptedSearch::new());
    let downloads = Arc::new(ScriptedDownloads::new());
    let library = Arc::new(LibraryPresence::new());
    let ticks = Arc::new(MemoryTicks::new());
    Wanted {
        deps: WantedDeps {
            settings: Arc::new(WantedSettings::default),
            watches: WantedStore::new(db.clone()),
            ledger: RequestStore::new(db.clone()),
            search: search.clone(),
            downloads: downloads.clone(),
            library: library.clone(),
            ticks: ticks.clone(),
        },
        search,
        downloads,
        library,
        ticks,
    }
}

// A failed request enrols a durable watch; once due with a candidate it
// auto-dispatches and links the task to the request.
#[tokio::test]
async fn wanted_enrols_then_auto_downloads() {
    let db = AcquireDb::scratch().unwrap();
    let rig = wanted(&db);
    seed_request(&rig.deps.ledger, "rg-blue", "failed", None).await;

    let first = wanted_tick(NOW, &mut LoopState::new(), &rig.deps).await;
    assert_eq!(first.enrolled, 1);
    let watch = WantedStore::new(db.clone())
        .get("rg-blue")
        .await
        .unwrap()
        .expect("watch persisted");
    assert_eq!(watch.state, "watching");
    // A second sweep never enrols the same request twice.
    let again = wanted_tick(NOW, &mut LoopState::new(), &rig.deps).await;
    assert_eq!(again.enrolled, 0);

    rig.search.set(
        "Massive Attack",
        "Blue Lines",
        Ok(vec![Candidate {
            title: "Blue Lines".to_owned(),
            source: "slskd".to_owned(),
        }]),
    );
    let due = watch.next_check_at as i64;
    let checked = wanted_tick(due, &mut LoopState::new(), &rig.deps).await;
    assert_eq!((checked.checked, checked.dispatched), (1, 1));
    assert_eq!(rig.downloads.dispatched()[0].origin, "wanted");
    let row = rig
        .deps
        .ledger
        .get(RequestKind::Album, "rg-blue")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.task_id.as_deref(), Some("task-1"));
    assert_eq!(rig.ticks.of_kind("wanted.dispatched").len(), 1);
}

// A watch the library now holds is fulfilled and its request imported.
#[tokio::test]
async fn wanted_satisfied_from_the_library() {
    let db = AcquireDb::scratch().unwrap();
    let rig = wanted(&db);
    seed_request(&rig.deps.ledger, "rg-blue", "failed", None).await;
    wanted_tick(NOW, &mut LoopState::new(), &rig.deps).await;
    rig.library.add(&["rg-blue"]);
    let due = NOW + interval_seconds(None, 0, NOW);
    let summary = wanted_tick(due, &mut LoopState::new(), &rig.deps).await;
    assert_eq!(summary.fulfilled, 1);
    assert_eq!(status_of(&rig.deps.ledger, "rg-blue").await, "imported");
    let watch = rig.deps.watches.get("rg-blue").await.unwrap().unwrap();
    assert_eq!(watch.state, "fulfilled");
    assert_eq!(rig.ticks.of_kind("request_fulfilled").len(), 1);
}

// Status sync maps task states onto requests, stamps terminal rows, and
// falls back to the album's active task for taskless rows.
#[tokio::test]
async fn status_sync_maps_and_stamps() {
    let db = AcquireDb::scratch().unwrap();
    let ledger = RequestStore::new(db.clone());
    seed_request(&ledger, "rg-done", "downloading", Some("task-done")).await;
    seed_request(&ledger, "rg-short", "downloading", Some("task-short")).await;
    seed_request(&ledger, "rg-taskless", "pending", None).await;
    let downloads = Arc::new(ScriptedDownloads::new());
    downloads.set_status("task-done", "completed");
    downloads.set_status("task-short", "partial");
    downloads.set_album_task(
        "rg-taskless",
        flows::seams::DownloadTaskView {
            task_id: "task-x".to_owned(),
            status: "downloading".to_owned(),
            album_mbid: Some("rg-taskless".to_owned()),
        },
    );
    let ticks = Arc::new(MemoryTicks::new());
    let deps = SyncDeps {
        ledger: ledger.clone(),
        downloads,
        library: Arc::new(LibraryPresence::new()),
        ticks: ticks.clone(),
    };
    let summary = sync_tick(NOW, &mut LoopState::new(), &deps).await;
    assert_eq!((summary.reconciled, summary.imported), (3, 1));
    assert_eq!(status_of(&ledger, "rg-done").await, "imported");
    assert_eq!(status_of(&ledger, "rg-short").await, "incomplete");
    assert_eq!(status_of(&ledger, "rg-taskless").await, "downloading");
    let done = ledger
        .get(RequestKind::Album, "rg-done")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.completed_at, Some(NOW as u64));
    assert_eq!(ticks.of_kind("request_fulfilled").len(), 1);
}

// The follow poll baselines first, then adds new releases to the feed and
// queues one download for the first approved auto-download follower;
// future releases are held, owned and filtered ones never appear.
#[tokio::test]
async fn follow_poll_baselines_then_emits() {
    let db = AcquireDb::scratch().unwrap();
    db.write("seed follows", |tx| {
        tx.execute_batch(
            "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
               ('user-1', 'Admin', 'admin', '2024-01-01'), \
               ('user-2', 'Pending', 'user', '2024-01-01'); \
             INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
               artist_name, auto_download, followed_at, updated_at) VALUES \
               ('user-2', 'Artist-1', 'artist-1', 'The Band', 1, 1, 1), \
               ('user-1', 'Artist-1', 'artist-1', 'The Band', 1, 1, 1);",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let follows = FollowStore::new(db.clone());
    let release = |rg: &str, date: &str, kind: &str, secondary: &[&str]| ObservedRelease {
        rg_mbid: rg.to_owned(),
        title: rg.to_owned(),
        first_release_date: Some(date.to_owned()),
        primary_type: Some(kind.to_owned()),
        secondary_types: secondary.iter().map(|value| value.to_string()).collect(),
    };
    let poll = Arc::new(ScriptedPoll::new());
    poll.set(
        "Artist-1",
        Ok(vec![release("rg-old", "2020-01-01", "Album", &[])]),
    );
    let downloads = Arc::new(ScriptedDownloads::new());
    let library = Arc::new(LibraryPresence::new());
    library.add(&["rg-owned"]);
    let deps = FollowDeps {
        follows: follows.clone(),
        poll: poll.clone(),
        downloads: downloads.clone(),
        library,
        ticks: Arc::new(MemoryTicks::new()),
        preferences: Arc::new(UserPreferences::default),
        today: Arc::new(|| "2024-06-01".to_owned()),
    };
    let first = follow_tick(NOW, &mut LoopState::new(), &deps).await;
    assert_eq!(first.baselined, 1);
    assert!(downloads.dispatched().is_empty());
    let again = follow_tick(NOW + 3_600, &mut LoopState::new(), &deps).await;
    assert_eq!(again.artists_polled, 0, "a polled artist waits a day");

    poll.set(
        "Artist-1",
        Ok(vec![
            release("rg-old", "2020-01-01", "Album", &[]),
            release("rg-new", "2024-06-01", "Album", &[]),
            release("rg-future", "2025-01-01", "Single", &[]),
            release("rg-sloppy", "2024-06", "Album", &[]),
            release("rg-live", "2024-06-01", "Album", &["Live"]),
            release("rg-owned", "2024-06-01", "Album", &[]),
        ]),
    );
    let second = follow_tick(NOW + 86_400, &mut LoopState::new(), &deps).await;
    assert_eq!(
        second.new_releases, 2,
        "rg-new and rg-future reach the feed"
    );
    assert_eq!(second.enqueued, 1, "one task per release");
    let dispatched = downloads.dispatched();
    assert_eq!(
        dispatched[0].user_id, "user-1",
        "only the approved follower"
    );
    assert_eq!(dispatched[0].mbid, "rg-new");
    assert_eq!(dispatched[0].artist, "The Band");
    let cursor = follows.get("artist-1").await.unwrap().unwrap();
    assert_eq!(cursor.pending.len(), 1);
    assert_eq!(cursor.pending[0].rg_mbid, "rg-future");
    let feed: Vec<String> = sqlx::query_scalar(
        "SELECT release_group_mbid FROM new_release_feed ORDER BY release_group_mbid",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(feed, ["rg-future", "rg-new"]);
}

// The upgrade sweep needs both gates and an admin, and honours its cap.
#[tokio::test]
async fn upgrade_sweep_gates_and_cap() {
    let db = AcquireDb::scratch().unwrap();
    let worklist = MemoryWorklist(
        (0..4)
            .map(|n| UpgradeItem {
                rg_mbid: format!("rg-{n}"),
                artist: "A".to_owned(),
                title: "T".to_owned(),
                current_tier: "low",
                track_count: 1,
                year: None,
                artist_mbid: None,
            })
            .collect(),
    );
    let admins = AdminDirectory::over_users(db.clone());
    let downloads = Arc::new(ScriptedDownloads::new());
    downloads.mark_in_library("rg-0");
    let policy = UpgradePolicy {
        upgrade_allowed: true,
        cutoff: "lossless".to_owned(),
        scan_enabled: true,
        max_per_run: 2,
        interval_hours: 12,
    };
    let deps = SweepDeps {
        policy: Arc::new(move || policy.clone()),
        worklist: Arc::new(worklist),
        admins,
        downloads: downloads.clone(),
        ticks: Arc::new(MemoryTicks::new()),
    };
    let idle = sweep_tick(NOW, &mut LoopState::new(), &deps).await;
    assert!(idle.skipped_no_admin);
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('admin-1', 'Admin', 'admin', '2026-01-01T00:00:00Z')",
    )
    .execute(db.pool())
    .await
    .unwrap();
    let swept = sweep_tick(NOW, &mut LoopState::new(), &deps).await;
    assert_eq!(swept.enqueued, 2, "already-in-library never counts");
    let grabs = downloads.upgrades();
    assert_eq!(grabs[0].mbid, "rg-1");
    assert_eq!(grabs[0].user_id, "admin-1");
}

// Drop import: good files resolve, bad sources quarantine durably under a
// job-prefixed name (a second job's same-named file never overwrites it),
// and a hand match clears the entry.
#[tokio::test]
async fn drop_import_quarantines_and_resolves() {
    let (runtime, db, dir) = runtime("drop").await;
    register_durable_ops(runtime.wakeups(), runtime.lane())
        .await
        .unwrap();
    let ops = OpStore::new(db.clone());
    let staging = dir.join("staging");
    let ledger = RequestStore::new(db.clone());
    seed_request(&ledger, "rg-blue", "downloading", None).await;
    let verify = Arc::new(ScriptedVerify::new());
    verify.set(
        "bad.flac",
        VerifyVerdict::BadSource("wrong album".to_owned()),
    );
    let organise = Arc::new(MemoryOrganise::new());
    let deps = DropImportDeps {
        verify,
        organise: organise.clone(),
        quarantine: QuarantineStore::new(db.clone()),
        ledger: ledger.clone(),
        ticks: Arc::new(MemoryTicks::new()),
        clock: Arc::new(ManualClock::new(NOW)),
    };
    let mut jobs = Vec::new();
    for seq in 1..=2 {
        let upload_dir = dir.join(format!("upload-{seq}"));
        std::fs::create_dir_all(&upload_dir).unwrap();
        let mut uploads = Vec::new();
        for name in ["good.flac", "bad.flac"] {
            let path = upload_dir.join(name);
            std::fs::write(&path, format!("job {seq}")).unwrap();
            uploads.push((name.to_owned(), path));
        }
        let mut job = create_drop_job(
            runtime.wakeups(),
            runtime.lane(),
            &ops,
            &staging,
            seq,
            "user-1",
            &uploads,
            NOW,
        )
        .await
        .unwrap();
        process_drop_job(
            runtime.wakeups(),
            runtime.lane(),
            &ops,
            &deps,
            &staging,
            &mut job,
            Some("rg-blue"),
        )
        .await
        .unwrap();
        jobs.push(job);
    }
    let held = QuarantineStore::new(db.clone()).list().await.unwrap();
    assert_eq!(held.len(), 2);
    for (seq, job) in jobs.iter().enumerate() {
        let staged = &job.items[1].staged_path;
        assert_eq!(
            std::fs::read_to_string(staged).unwrap(),
            format!("job {}", seq + 1)
        );
    }
    assert_eq!(status_of(&ledger, "rg-blue").await, "imported");

    let matched = resolve_quarantined_item(
        &deps,
        &mut jobs[0],
        "bad.flac",
        ResolveDecision::Match,
        None,
    )
    .await
    .unwrap();
    assert!(matched);
    assert!(
        !deps
            .quarantine
            .is_quarantined("drop-1:bad.flac")
            .await
            .unwrap()
    );
    assert_eq!(organise.placements().len(), 3);
    runtime.shutdown().await;
}

// Free music runs as a durable operation and lands once its download
// completes; on restart a drop-import run cut off mid-way is failed while
// open free-music runs stay for their settle.
#[tokio::test]
async fn free_music_lands_and_operations_recover() {
    let (runtime, db, _dir) = runtime("free-music").await;
    register_durable_ops(runtime.wakeups(), runtime.lane())
        .await
        .unwrap();
    let ops = OpStore::new(db.clone());
    let search = Arc::new(ScriptedSearch::new());
    search.set(
        "Ketsa",
        "Live at Freiraum",
        Ok(vec![Candidate {
            title: "Live at Freiraum".to_owned(),
            source: "archive".to_owned(),
        }]),
    );
    let downloads = Arc::new(ScriptedDownloads::new());
    let handoff = Arc::new(MemoryHandoff::new());
    let deps = FreeMusicDeps {
        search,
        downloads: downloads.clone(),
        handoff: handoff.clone(),
        ticks: Arc::new(MemoryTicks::new()),
        clock: Arc::new(ManualClock::new(NOW)),
        enabled: true,
    };
    let request = FreeMusicRequest {
        user_id: "user-1".to_owned(),
        kind: DispatchKind::Album,
        mbid: "rg-ketsa".to_owned(),
        artist: "Ketsa".to_owned(),
        title: "Live at Freiraum".to_owned(),
    };
    let FreeMusicOutcome::Pending { op_id, task_id } =
        run_free_music(runtime.wakeups(), runtime.lane(), &ops, &deps, &request)
            .await
            .unwrap()
    else {
        panic!("expected pending while downloading");
    };
    let drop_op = ops.register(DROP_IMPORT_JOB, "drop-9", NOW).await.unwrap();
    ops.transition(&drop_op.id, OpState::Running, NOW, "")
        .await
        .unwrap();

    let restarted = OpStore::new(db.clone());
    let report = restarted.recover(NOW + 5).await.unwrap();
    assert_eq!((report.interrupted, report.resumed), (1, 1));
    assert_eq!(
        restarted.get(&drop_op.id).await.unwrap().unwrap().state,
        OpState::Failed
    );

    downloads.set_status(&task_id, "completed");
    let settled = settle_free_music(
        runtime.wakeups(),
        runtime.lane(),
        &restarted,
        &deps,
        &op_id,
        &request,
    )
    .await
    .unwrap();
    assert!(matches!(settled, FreeMusicOutcome::Landed { .. }));
    assert_eq!(handoff.landings().len(), 1);
    // The same request reuses its operation instead of queuing another.
    let again = restarted
        .register("free-music", "user-1:rg-ketsa:album", NOW + 10)
        .await
        .unwrap();
    assert_eq!(again.id, op_id);
    runtime.shutdown().await;
}

// The sync loop waits its startup delay, runs a pass, sleeps the honest
// interval and parks the job stopped on shutdown; a shutdown during the
// delay still parks it stopped without running a pass.
#[tokio::test]
async fn loop_cadence_and_shutdown() {
    let (runtime, db, _dir) = runtime("loops").await;
    let ledger = RequestStore::new(db.clone());
    seed_request(&ledger, "rg-1", "downloading", Some("task-9")).await;
    let downloads = Arc::new(ScriptedDownloads::new());
    downloads.set_status("task-9", "completed");
    let deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        downloads,
        library: Arc::new(LibraryPresence::new()),
        ticks: Arc::new(MemoryTicks::new()),
    });

    let early = ManualSleeper::new();
    early.shut_down();
    let (handle, _) = spawn_sync_loop(
        early,
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        Arc::new(SystemClock),
        deps.clone(),
    )
    .await
    .unwrap();
    handle.task.await.unwrap();
    assert_eq!(status_of(&ledger, "rg-1").await, "downloading");

    let sleeper = ManualSleeper::new();
    let (handle, _) = spawn_sync_loop(
        sleeper.clone(),
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        Arc::new(SystemClock),
        deps,
    )
    .await
    .unwrap();
    wait_for_requests(&sleeper, 1).await;
    sleeper.wake();
    wait_for_requests(&sleeper, 2).await;
    assert_eq!(status_of(&ledger, "rg-1").await, "imported");
    sleeper.shut_down();
    handle.task.await.unwrap();
    assert_eq!(sleeper.requested(), vec![SYNC_INITIAL_DELAY, SYNC_INTERVAL]);
    let job = runtime.wakeups().get_job(SYNC_JOB).await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Stopped);
    runtime.shutdown().await;
}

// The wanted recheck table matches v2's age bands, and the free-music
// title floor keeps unrelated records out.
#[test]
fn recheck_bands_and_title_floor() {
    let now = 1_716_720_000; // 2024-05-26 UTC
    assert_eq!(interval_seconds(Some("2024-05-20"), 0, now), 2 * 86_400);
    assert_eq!(interval_seconds(Some("2024-04-01"), 0, now), 4 * 86_400);
    assert_eq!(interval_seconds(Some("2024-01-15"), 0, now), 7 * 86_400);
    assert_eq!(interval_seconds(Some("2020-01-01"), 0, now), 14 * 86_400);
    assert_eq!(interval_seconds(Some("2020-01-01"), 10, now), 28 * 86_400);
    assert_eq!(interval_seconds(None, 0, now), 14 * 86_400);
    assert!(title_containment("Blue Lines", "Blue Lines (Deluxe Edition)") >= 0.60);
    assert!(title_containment("Blue Lines", "Tribute Band Sings Jazz") < 0.60);
}

/// Spin until the loop records `count` sleep requests.
async fn wait_for_requests(sleeper: &ManualSleeper, count: usize) {
    for _ in 0..500 {
        if sleeper.requested().len() >= count && sleeper.waits() > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("loop never requested {count} sleep(s)");
}

// The store prune drops settled requests past retention, never an active
// one.
#[tokio::test]
async fn prune_drops_only_settled_rows_past_retention() {
    let db = AcquireDb::scratch().unwrap();
    let ledger = RequestStore::new(db.clone());
    seed_request(&ledger, "rg-done", "imported", None).await;
    seed_request(&ledger, "rg-live", "pending", None).await;
    let deps = PruneDeps {
        settings: Arc::new(PruneSettings::default),
        ledger: ledger.clone(),
        watches: WantedStore::new(db.clone()),
        search_jobs: JobStore::new(db.clone()),
    };

    let summary = prune_tick(NOW + 200 * 86_400, &deps).await.unwrap();
    assert_eq!(summary.requests, 1);
    assert!(
        ledger
            .get(RequestKind::Album, "rg-done")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(status_of(&ledger, "rg-live").await, "pending");
}
