//! Acquisition-flow briefs: wanted → candidate → auto-download, drop
//! folder → quarantine → resolve, and the sweep/status-sync cadences.
//!
//! The flows module is still standalone (the integrator wires it into the
//! tree), so these briefs include it via `#[path]`, the stage-4 pattern.
//! Time is a [`ManualClock`](flows::seams::ManualClock) throughout: cadences
//! assert by advancing the clock, never by sleeping. Databases and staging
//! dirs are scratch temp paths; no network, no real library.

#[path = "../src/acquire/flows/mod.rs"]
// Partial-view harness: this file exercises a slice of the module, so
// items it never touches read as dead or unused here (they are live in
// the wired lib build).
#[allow(dead_code, unused_imports)]
mod flows;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use droppedneedle::db::{DbConfig, DbRuntime, JobKind, JobState, open_runtime};
use flows::loops::{
    FOLLOW_INTERVAL, FOLLOW_JOB, FOLLOW_MAX_ARTISTS_PER_TICK, FollowDeps, LoopState, ManualSleeper,
    NoJitter, SYNC_INITIAL_DELAY, SYNC_INTERVAL, SYNC_JOB, SweepDeps, SyncDeps,
    UPGRADE_INITIAL_DELAY, WantedDeps, WantedSettings, follow_tick, interval_seconds,
    register_ephemeral_loop, spawn_sync_loop, spawn_wanted_loop, sweep_tick, sync_tick,
    wanted_tick,
};
use flows::operations::{
    DROP_IMPORT_JOB, FREE_MUSIC_JOB, FreeMusicDeps, FreeMusicOutcome, FreeMusicRequest, OpStore,
    ResolveDecision, create_drop_job, process_drop_job, progress_write_due, register_durable_ops,
    resolve_quarantined_item, run_free_music, scan_drop_folder, settle_free_music,
    title_containment,
};
use flows::seams::{
    Candidate, Clock, DispatchKind, ManualClock, MemoryHandoff, MemoryOrganise, MemoryTicks,
    ObservedRelease, ScriptedDownloads, ScriptedPoll, ScriptedSearch, ScriptedVerify, SystemClock,
    VerifyVerdict,
};
use flows::stores::{
    AdminDirectory, FollowCursor, FollowStore, LibraryPresence, PendingRelease, QuarantineStore,
    RequestLedger, RequestRow, UpgradeItem, UpgradePolicy, UpgradeWorklist, WantedStore,
};

static SCRATCH_SEQ: AtomicUsize = AtomicUsize::new(0);

fn scratch_dir(name: &str) -> PathBuf {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("acquire-flows-{name}-{}-{seq}", std::process::id()))
}

async fn open_scratch(name: &str) -> (DbRuntime, PathBuf) {
    let dir = scratch_dir(name);
    let db = dir.join("library.db");
    let runtime = open_runtime(&DbConfig::new(&db)).await.unwrap();
    (runtime, dir)
}

fn row(mbid: &str, kind: &str, user: &str, status: &str) -> RequestRow {
    RequestRow {
        mbid: mbid.to_owned(),
        kind: kind.to_owned(),
        user_id: user.to_owned(),
        artist: "Massive Attack".to_owned(),
        title: "Blue Lines".to_owned(),
        status: status.to_owned(),
        task_id: None,
        generation: 1,
        completed_at: None,
    }
}

fn wanted_deps(
    watches: Arc<WantedStore>,
    ledger: Arc<RequestLedger>,
    search: Arc<ScriptedSearch>,
    downloads: Arc<ScriptedDownloads>,
    library: Arc<LibraryPresence>,
    ticks: Arc<MemoryTicks>,
) -> WantedDeps {
    WantedDeps {
        settings: Arc::new(WantedSettings::default),
        watches,
        ledger,
        search,
        downloads,
        library,
        ticks,
    }
}

/// Failed request → watch → candidate → auto-download, end to end.
#[tokio::test]
async fn wanted_watch_to_candidate_to_auto_download() {
    let now = 1_700_000_000;
    let clock = ManualClock::new(now);
    let watches = Arc::new(WantedStore::new());
    let ledger = Arc::new(RequestLedger::new());
    let search = Arc::new(ScriptedSearch::new());
    let downloads = Arc::new(ScriptedDownloads::new());
    let library = Arc::new(LibraryPresence::new());
    let ticks = Arc::new(MemoryTicks::new());
    ledger.upsert(row("rg-blue", "album", "user-1", "failed"));
    // A previously enrolled watch, already due, with a scripted candidate.
    watches.enrol(flows::stores::Watch {
        rg_mbid: "rg-green".to_owned(),
        user_id: "user-1".to_owned(),
        artist: "Massive Attack".to_owned(),
        title: "Blue Lines".to_owned(),
        first_release_date: None,
        quiet_streak: 0,
        next_check_at: now,
    });
    search.set(
        "Massive Attack",
        "Blue Lines",
        Ok(vec![Candidate {
            title: "Blue Lines".to_owned(),
            source: "slskd".to_owned(),
        }]),
    );
    let deps = wanted_deps(
        watches.clone(),
        ledger.clone(),
        search.clone(),
        downloads.clone(),
        library,
        ticks.clone(),
    );

    assert!(!watches.is_empty());
    assert_eq!(watches.len(), 1);
    let mut state = LoopState::new();
    let summary = wanted_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(summary.enrolled, 1);
    assert_eq!(summary.checked, 1);
    assert_eq!(summary.dispatched, 1);
    assert_eq!(summary.errors, 0);
    assert_eq!(
        search.queries(),
        vec![("Massive Attack".to_owned(), "Blue Lines".to_owned())]
    );
    assert_eq!(ticks.ticks().len(), 2);

    let dispatched = downloads.dispatched();
    assert_eq!(dispatched.len(), 1);
    assert_eq!(dispatched[0].origin, "wanted");
    assert_eq!(dispatched[0].mbid, "rg-green");
    assert_eq!(dispatched[0].user_id, "user-1");
    // The fresh enrolment's first check lands on the age-table cadence.
    let fresh = watches.get("rg-blue").unwrap();
    assert_eq!(fresh.next_check_at, now + 14 * 86_400);
    assert_eq!(ticks.of_kind("wanted.enrolled").len(), 1);
    assert_eq!(ticks.of_kind("wanted.dispatched").len(), 1);

    // Second tick before the 15-minute cadence runs nothing.
    let again = wanted_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(again, Default::default());
}

/// A watch whose album reached the library fulfils without dispatching.
#[tokio::test]
async fn wanted_satisfied_from_library() {
    let now = 1_700_000_000;
    let clock = ManualClock::new(now);
    let watches = Arc::new(WantedStore::new());
    let ledger = Arc::new(RequestLedger::new());
    let search = Arc::new(ScriptedSearch::new());
    let downloads = Arc::new(ScriptedDownloads::new());
    let library = Arc::new(LibraryPresence::new());
    let ticks = Arc::new(MemoryTicks::new());
    ledger.upsert(row("rg-blue", "album", "user-1", "failed"));
    library.add(&["rg-blue"]);
    let deps = wanted_deps(
        watches,
        ledger.clone(),
        search,
        downloads.clone(),
        library.clone(),
        ticks.clone(),
    );

    let mut state = LoopState::new();
    let summary = wanted_tick(clock.now_unix(), &mut state, &deps).await;
    // Enrol skips library-held albums, so nothing enrols and nothing dispatches.
    assert_eq!(summary.enrolled, 0);
    assert!(downloads.dispatched().is_empty());

    // A watch enrolled before the album landed fulfils on its next check.
    ledger.upsert(row("rg-mezz", "album", "user-1", "failed"));
    let deps = wanted_deps(
        Arc::new(WantedStore::new()),
        ledger.clone(),
        Arc::new(ScriptedSearch::new()),
        downloads.clone(),
        Arc::new(LibraryPresence::new()),
        ticks.clone(),
    );
    deps.watches.enrol(flows::stores::Watch {
        rg_mbid: "rg-mezz".to_owned(),
        user_id: "user-1".to_owned(),
        artist: "Massive Attack".to_owned(),
        title: "Mezzanine".to_owned(),
        first_release_date: None,
        quiet_streak: 0,
        next_check_at: now,
    });
    deps.library.add(&["rg-mezz"]);
    let mut state = LoopState::new();
    let summary = wanted_tick(now, &mut state, &deps).await;
    assert_eq!(summary.fulfilled, 1);
    assert_eq!(ledger.get("rg-mezz").unwrap().status, "imported");
    assert_eq!(ticks.of_kind("request_fulfilled").len(), 1);
    assert!(deps.library.all().contains("rg-mezz"));
}

/// A dispatch failure isolates to its own watch: the sweep counts the error
/// and continues instead of dying.
#[tokio::test]
async fn wanted_dispatch_failure_isolates() {
    let now = 1_700_000_000;
    let watches = Arc::new(WantedStore::new());
    watches.enrol(flows::stores::Watch {
        rg_mbid: "rg-green".to_owned(),
        user_id: "user-1".to_owned(),
        artist: "Massive Attack".to_owned(),
        title: "Blue Lines".to_owned(),
        first_release_date: None,
        quiet_streak: 0,
        next_check_at: now,
    });
    let search = Arc::new(ScriptedSearch::new());
    search.set(
        "Massive Attack",
        "Blue Lines",
        Ok(vec![Candidate {
            title: "Blue Lines".to_owned(),
            source: "slskd".to_owned(),
        }]),
    );
    let downloads = Arc::new(ScriptedDownloads::new());
    downloads.fail_dispatches("orchestrator down");
    let deps = wanted_deps(
        watches,
        Arc::new(RequestLedger::new()),
        search,
        downloads,
        Arc::new(LibraryPresence::new()),
        Arc::new(MemoryTicks::new()),
    );
    let mut state = LoopState::new();
    let summary = wanted_tick(now, &mut state, &deps).await;
    assert_eq!(summary.checked, 1);
    assert_eq!(summary.dispatched, 0);
    assert_eq!(summary.errors, 1);
}

/// Badge-only mode notes candidates without dispatching them.
#[tokio::test]
async fn wanted_badge_only_skips_dispatch() {
    let now = 1_700_000_000;
    let watches = Arc::new(WantedStore::new());
    watches.enrol(flows::stores::Watch {
        rg_mbid: "rg-green".to_owned(),
        user_id: "user-1".to_owned(),
        artist: "Massive Attack".to_owned(),
        title: "Blue Lines".to_owned(),
        first_release_date: None,
        quiet_streak: 0,
        next_check_at: now,
    });
    let search = Arc::new(ScriptedSearch::new());
    search.set(
        "Massive Attack",
        "Blue Lines",
        Ok(vec![Candidate {
            title: "Blue Lines".to_owned(),
            source: "slskd".to_owned(),
        }]),
    );
    let downloads = Arc::new(ScriptedDownloads::new());
    let ticks = Arc::new(MemoryTicks::new());
    let mut deps = wanted_deps(
        watches,
        Arc::new(RequestLedger::new()),
        search,
        downloads.clone(),
        Arc::new(LibraryPresence::new()),
        ticks.clone(),
    );
    deps.settings = Arc::new(|| WantedSettings {
        auto_download_on_find: false,
        ..WantedSettings::default()
    });
    let mut state = LoopState::new();
    let summary = wanted_tick(now, &mut state, &deps).await;
    assert_eq!(summary.checked, 1);
    assert_eq!(summary.dispatched, 0);
    assert!(downloads.dispatched().is_empty());
    assert_eq!(ticks.of_kind("wanted.found").len(), 1);
    assert!(ticks.of_kind("wanted.dispatched").is_empty());
}

/// Drop folder → quarantine → resolve, with real scratch files.
#[tokio::test]
async fn drop_folder_to_quarantine_to_resolve() {
    let (runtime, dir) = open_scratch("drop").await;
    let ops = OpStore::new();
    register_durable_ops(runtime.wakeups(), runtime.lane())
        .await
        .unwrap();

    let drop_dir = dir.join("drop");
    let staging_root = dir.join("staging");
    std::fs::create_dir_all(&drop_dir).unwrap();
    for name in ["good.flac", "bad-match.flac", "bad-discard.flac"] {
        std::fs::write(drop_dir.join(name), b"audio").unwrap();
    }
    let scanned = scan_drop_folder(&drop_dir).unwrap();
    assert_eq!(scanned.len(), 3);

    let uploads: Vec<(String, PathBuf)> = scanned
        .iter()
        .map(|path| {
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                path.clone(),
            )
        })
        .collect();
    let mut job = create_drop_job(
        runtime.wakeups(),
        runtime.lane(),
        &ops,
        &staging_root,
        1,
        "user-1",
        &uploads,
        1_700_000_000,
    )
    .await
    .unwrap();
    assert_eq!(job.upload_name, "bad-discard.flac +2 more");
    assert!(
        job.staging_dir
            .starts_with(staging_root.to_string_lossy().as_ref())
    );

    let verify = Arc::new(ScriptedVerify::new());
    verify.set(
        "bad-match.flac",
        VerifyVerdict::BadSource("wrong album".to_owned()),
    );
    verify.set(
        "bad-discard.flac",
        VerifyVerdict::BadSource("truncated".to_owned()),
    );
    let organise = Arc::new(MemoryOrganise::new());
    let quarantine = Arc::new(QuarantineStore::new());
    let ledger = Arc::new(RequestLedger::new());
    let ticks = Arc::new(MemoryTicks::new());
    ledger.upsert(row("rg-blue", "album", "user-1", "downloading"));
    let deps = flows::operations::DropImportDeps {
        verify,
        organise: organise.clone(),
        quarantine: quarantine.clone(),
        ledger: ledger.clone(),
        ticks: ticks.clone(),
        clock: Arc::new(ManualClock::new(1_700_000_000)),
    };
    process_drop_job(
        runtime.wakeups(),
        runtime.lane(),
        &ops,
        &deps,
        &staging_root,
        &mut job,
        Some("rg-blue"),
    )
    .await
    .unwrap();

    assert_eq!(quarantine.list().len(), 2);
    assert!(quarantine.is_quarantined("drop-1:bad-match.flac"));
    assert_eq!(organise.placements().len(), 1);
    assert_eq!(ledger.get("rg-blue").unwrap().status, "imported");
    assert_eq!(ticks.of_kind("drop_import.resolved").len(), 1);
    assert_eq!(ticks.of_kind("drop_import.quarantined").len(), 2);

    // Hand resolve: match the first, discard the second.
    let matched = resolve_quarantined_item(
        &deps,
        &mut job,
        "bad-match.flac",
        ResolveDecision::Match,
        Some("rg-blue"),
    )
    .unwrap();
    assert!(matched);
    assert!(!quarantine.is_quarantined("drop-1:bad-match.flac"));
    assert_eq!(organise.placements().len(), 2);
    let discarded = resolve_quarantined_item(
        &deps,
        &mut job,
        "bad-discard.flac",
        ResolveDecision::Discard,
        None,
    )
    .unwrap();
    assert!(discarded);
    assert!(quarantine.list().is_empty());
    assert_eq!(ticks.of_kind("drop_import.discarded").len(), 1);

    // Resolving a non-quarantined file answers false.
    let again =
        resolve_quarantined_item(&deps, &mut job, "good.flac", ResolveDecision::Match, None)
            .unwrap();
    assert!(!again);

    // Album-scoped clears drop every entry for a retry.
    let retry_bin = QuarantineStore::new();
    for key in ["a:1", "a:2", "b:1"] {
        retry_bin.quarantine(flows::stores::QuarantineEntry {
            key: key.to_owned(),
            album_key: Some(key[..1].to_owned()),
            reason: "bad".to_owned(),
            at: 1_700_000_000,
        });
    }
    assert!(retry_bin.get("a:1").is_some());
    assert_eq!(retry_bin.clear_for_album("a"), 2);
    assert!(retry_bin.get("a:1").is_none());
    assert!(retry_bin.is_quarantined("b:1"));

    // The route streams uploads through the incoming dir.
    assert_eq!(
        flows::operations::incoming_dir(&staging_root),
        staging_root.join(flows::operations::INCOMING_DIR)
    );

    let record = runtime
        .wakeups()
        .get_job(DROP_IMPORT_JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.kind, JobKind::Durable);
    assert_eq!(record.state, JobState::Stopped);
    runtime.shutdown().await;
}

/// Local faults fail the item without quarantining the source.
#[tokio::test]
async fn drop_import_local_fault_never_quarantines() {
    let (runtime, dir) = open_scratch("drop-fault").await;
    let ops = OpStore::new();
    register_durable_ops(runtime.wakeups(), runtime.lane())
        .await
        .unwrap();
    let staging_root = dir.join("staging");
    let tmp = dir.join("tmp.flac");
    std::fs::create_dir_all(&staging_root).unwrap();
    std::fs::write(&tmp, b"audio").unwrap();
    let mut job = create_drop_job(
        runtime.wakeups(),
        runtime.lane(),
        &ops,
        &staging_root,
        1,
        "user-1",
        &[("tmp.flac".to_owned(), tmp)],
        1_700_000_000,
    )
    .await
    .unwrap();
    let verify = Arc::new(ScriptedVerify::new());
    verify.set(
        "tmp.flac",
        VerifyVerdict::LocalFault("destination occupied".to_owned()),
    );
    let deps = flows::operations::DropImportDeps {
        verify,
        organise: Arc::new(MemoryOrganise::new()),
        quarantine: Arc::new(QuarantineStore::new()),
        ledger: Arc::new(RequestLedger::new()),
        ticks: Arc::new(MemoryTicks::new()),
        clock: Arc::new(ManualClock::new(1_700_000_000)),
    };
    process_drop_job(
        runtime.wakeups(),
        runtime.lane(),
        &ops,
        &deps,
        &staging_root,
        &mut job,
        None,
    )
    .await
    .unwrap();
    assert!(deps.quarantine.list().is_empty());
    assert!(matches!(
        job.items[0].outcome,
        Some(flows::operations::DropItemOutcome::Faulted(_))
    ));
    runtime.shutdown().await;
}

/// Free music runs as a registered durable op and lands through drop-import.
#[tokio::test]
async fn free_music_registered_op_lands() {
    let (runtime, _dir) = open_scratch("free-music").await;
    let ops = OpStore::new();
    register_durable_ops(runtime.wakeups(), runtime.lane())
        .await
        .unwrap();
    let record = runtime
        .wakeups()
        .get_job(FREE_MUSIC_JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.kind, JobKind::Durable);

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
    let ticks = Arc::new(MemoryTicks::new());
    let clock = ManualClock::new(1_700_000_000);
    let deps = FreeMusicDeps {
        search,
        downloads: downloads.clone(),
        handoff: handoff.clone(),
        ticks: ticks.clone(),
        clock: Arc::new(clock.clone()),
        enabled: true,
    };
    let request = FreeMusicRequest {
        user_id: "user-1".to_owned(),
        kind: DispatchKind::Album,
        mbid: "rg-ketsa".to_owned(),
        artist: "Ketsa".to_owned(),
        title: "Live at Freiraum".to_owned(),
    };
    // Scripted dispatch starts `downloading`, so the first step pends.
    let outcome = run_free_music(runtime.wakeups(), runtime.lane(), &ops, &deps, &request)
        .await
        .unwrap();
    let (op_id, task_id) = match outcome {
        FreeMusicOutcome::Pending { op_id, task_id } => (op_id, task_id),
        FreeMusicOutcome::Landed { .. } => panic!("expected pending while downloading"),
    };
    let running = runtime
        .wakeups()
        .get_job(FREE_MUSIC_JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(running.state, JobState::Running);

    downloads.set_status(&task_id, "completed");
    let settled = settle_free_music(
        runtime.wakeups(),
        runtime.lane(),
        &ops,
        &deps,
        &op_id,
        &request,
    )
    .await
    .unwrap();
    assert!(matches!(settled, FreeMusicOutcome::Landed { .. }));
    assert_eq!(handoff.landings().len(), 1);
    assert_eq!(handoff.landings()[0].1, "user-1");
    assert_eq!(ticks.of_kind("request_fulfilled").len(), 1);
    let done = runtime
        .wakeups()
        .get_job(FREE_MUSIC_JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.state, JobState::Stopped);
    assert!(done.last_heartbeat_at.is_some());
    runtime.shutdown().await;
}

/// Candidates below the 0.60 title floor never dispatch.
#[tokio::test]
async fn free_music_rejects_title_mismatch() {
    let (runtime, _dir) = open_scratch("free-music-floor").await;
    let ops = OpStore::new();
    register_durable_ops(runtime.wakeups(), runtime.lane())
        .await
        .unwrap();
    let search = Arc::new(ScriptedSearch::new());
    search.set(
        "Ketsa",
        "Live at Freiraum",
        Ok(vec![Candidate {
            title: "Tribute Band Sings Jazz".to_owned(),
            source: "archive".to_owned(),
        }]),
    );
    let downloads = Arc::new(ScriptedDownloads::new());
    let deps = FreeMusicDeps {
        search,
        downloads: downloads.clone(),
        handoff: Arc::new(MemoryHandoff::new()),
        ticks: Arc::new(MemoryTicks::new()),
        clock: Arc::new(ManualClock::new(1_700_000_000)),
        enabled: true,
    };
    let request = FreeMusicRequest {
        user_id: "user-1".to_owned(),
        kind: DispatchKind::Album,
        mbid: "rg-ketsa".to_owned(),
        artist: "Ketsa".to_owned(),
        title: "Live at Freiraum".to_owned(),
    };
    let error = run_free_music(runtime.wakeups(), runtime.lane(), &ops, &deps, &request)
        .await
        .unwrap_err();
    assert_eq!(error, "no matching candidate");
    assert!(downloads.dispatched().is_empty());
    let failed = runtime
        .wakeups()
        .get_job(FREE_MUSIC_JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, JobState::Failed);
    runtime.shutdown().await;
}

/// Upgrade sweep: gates, oldest-admin ownership, cap, and 12h cadence.
#[tokio::test]
async fn upgrade_sweep_cadence_and_cap() {
    let now = 1_700_000_000;
    let clock = ManualClock::new(now);
    let worklist = Arc::new(UpgradeWorklist::new());
    worklist.set(vec![
        UpgradeItem {
            rg_mbid: "rg-1".to_owned(),
            artist: "A".to_owned(),
            title: "One".to_owned(),
        },
        UpgradeItem {
            rg_mbid: "rg-2".to_owned(),
            artist: "B".to_owned(),
            title: "Two".to_owned(),
        },
        UpgradeItem {
            rg_mbid: "rg-3".to_owned(),
            artist: "C".to_owned(),
            title: "Three".to_owned(),
        },
    ]);
    let admins = Arc::new(AdminDirectory::new());
    admins.set(vec!["admin-old".to_owned(), "admin-new".to_owned()]);
    let downloads = Arc::new(ScriptedDownloads::new());
    downloads.mark_in_library("rg-3");
    let ticks = Arc::new(MemoryTicks::new());
    let policy = UpgradePolicy {
        upgrade_allowed: true,
        scan_enabled: true,
        max_per_run: 2,
        interval_hours: 12,
    };
    let deps = SweepDeps {
        policy: Arc::new(move || policy.clone()),
        worklist,
        admins,
        downloads: downloads.clone(),
        ticks,
    };

    let mut state = LoopState::new();
    let summary = sweep_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(summary.enqueued, 2);
    let upgrades = downloads.upgrades();
    assert_eq!(upgrades.len(), 2);
    assert!(upgrades.iter().all(|grab| grab.user_id == "admin-old"));
    assert!(upgrades.iter().all(|grab| grab.origin == "upgrade"));

    // Eleven hours later the sweep still sleeps.
    clock.advance(11 * 3600);
    let quiet = sweep_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(quiet.enqueued, 0);

    // Past twelve hours it runs again; the library-held album never counts.
    clock.advance(3600);
    let second = sweep_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(second.enqueued, 2);
    assert_eq!(downloads.upgrades().len(), 4);
}

/// Upgrade sweep stays quiet without admins or with a closed gate.
#[tokio::test]
async fn upgrade_sweep_skips_without_admin_or_gate() {
    let now = 1_700_000_000;
    let worklist = Arc::new(UpgradeWorklist::new());
    worklist.set(vec![UpgradeItem {
        rg_mbid: "rg-1".to_owned(),
        artist: "A".to_owned(),
        title: "One".to_owned(),
    }]);
    let policy = UpgradePolicy {
        upgrade_allowed: true,
        scan_enabled: true,
        max_per_run: 5,
        interval_hours: 12,
    };
    let deps = SweepDeps {
        policy: Arc::new(move || policy.clone()),
        worklist,
        admins: Arc::new(AdminDirectory::new()),
        downloads: Arc::new(ScriptedDownloads::new()),
        ticks: Arc::new(MemoryTicks::new()),
    };
    let mut state = LoopState::new();
    let summary = sweep_tick(now, &mut state, &deps).await;
    assert!(summary.skipped_no_admin);

    let gated = UpgradePolicy {
        upgrade_allowed: true,
        scan_enabled: false,
        max_per_run: 5,
        interval_hours: 12,
    };
    let admins = Arc::new(AdminDirectory::new());
    admins.set(vec!["admin".to_owned()]);
    let deps = SweepDeps {
        policy: Arc::new(move || gated.clone()),
        worklist: Arc::new(UpgradeWorklist::new()),
        admins,
        downloads: Arc::new(ScriptedDownloads::new()),
        ticks: Arc::new(MemoryTicks::new()),
    };
    let mut state = LoopState::new();
    let summary = sweep_tick(now, &mut state, &deps).await;
    assert_eq!(summary, Default::default());
}

/// Status sync maps task states, stamps terminal rows, and ticks imports.
#[tokio::test]
async fn status_sync_maps_and_stamps() {
    let now = 1_700_000_000;
    let clock = ManualClock::new(now);
    let ledger = Arc::new(RequestLedger::new());
    let downloads = Arc::new(ScriptedDownloads::new());
    let library = Arc::new(LibraryPresence::new());
    let ticks = Arc::new(MemoryTicks::new());

    let mut active = row("rg-active", "album", "user-1", "pending");
    active.task_id = Some("task-1".to_owned());
    ledger.upsert(active);
    downloads.set_status("task-1", "downloading");
    let mut done = row("rg-done", "album", "user-1", "downloading");
    done.task_id = Some("task-2".to_owned());
    ledger.upsert(done);
    downloads.set_status("task-2", "completed");
    let mut partial = row("rg-part", "album", "user-1", "downloading");
    partial.task_id = Some("task-3".to_owned());
    ledger.upsert(partial);
    downloads.set_status("task-3", "partial");
    // Album row with no task but library presence reconciles from the shelf.
    ledger.upsert(row("rg-shelf", "album", "user-1", "downloading"));
    library.add(&["rg-shelf"]);
    // Track row with no task: a recording MBID is not a library key, so
    // library presence must NOT reconcile it (v2 `_reconcile_request` quirk).
    ledger.upsert(row("rec-orphan", "track", "user-1", "downloading"));
    library.add(&["rec-orphan"]);
    // Album row with no linked task reconciles through the any-user fallback.
    ledger.upsert(row("rg-shared", "album", "user-1", "pending"));
    downloads.set_album_task(
        "rg-shared",
        flows::seams::DownloadTaskView {
            task_id: "task-8".to_owned(),
            status: "processing".to_owned(),
            album_mbid: Some("rg-shared".to_owned()),
        },
    );

    let deps = SyncDeps {
        ledger: ledger.clone(),
        downloads,
        library,
        ticks: ticks.clone(),
    };
    let mut state = LoopState::new();
    let summary = sync_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(summary.reconciled, 5);
    assert_eq!(summary.imported, 2);
    assert_eq!(ledger.get("rg-active").unwrap().status, "downloading");
    assert!(ledger.get("rg-active").unwrap().completed_at.is_none());
    let imported = ledger.get("rg-done").unwrap();
    assert_eq!(imported.status, "imported");
    assert_eq!(imported.completed_at, Some(now));
    assert_eq!(ledger.get("rg-part").unwrap().status, "incomplete");
    assert_eq!(ledger.get("rg-shelf").unwrap().status, "imported");
    assert_eq!(ledger.get("rg-shared").unwrap().status, "downloading");
    assert_eq!(ledger.get("rec-orphan").unwrap().status, "downloading");
    assert_eq!(ticks.of_kind("request_fulfilled").len(), 2);

    // A minute has not passed: the next tick is quiet.
    clock.advance(30);
    let quiet = sync_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(quiet, Default::default());
    clock.set(now + 60);
    let _ = sync_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(state.runs, 2);
}

/// Follow poll baselines first, then emits only complete dated releases.
#[tokio::test]
async fn follow_poll_baselines_then_emits() {
    let now = 1_700_000_000;
    let clock = ManualClock::new(now);
    let follows = Arc::new(FollowStore::new());
    follows.upsert(FollowCursor {
        artist_mbid: "artist-1".to_owned(),
        baselined: false,
        cursor_date: None,
        known: Default::default(),
        next_poll_at: now,
        followers: vec!["user-1".to_owned(), "user-2".to_owned()],
        pending: Vec::new(),
    });
    let poll = Arc::new(ScriptedPoll::new());
    poll.set(
        "artist-1",
        Ok(vec![ObservedRelease {
            rg_mbid: "rg-old".to_owned(),
            title: "Old".to_owned(),
            first_release_date: Some("2020-01-01".to_owned()),
            primary_type: Some("Album".to_owned()),
        }]),
    );
    let downloads = Arc::new(ScriptedDownloads::new());
    let ticks = Arc::new(MemoryTicks::new());
    let deps = FollowDeps {
        follows: follows.clone(),
        poll: poll.clone(),
        downloads: downloads.clone(),
        ticks,
        include_types: vec!["Album".to_owned()],
        today: Arc::new(|| "2024-06-01".to_owned()),
    };

    let mut state = LoopState::new();
    let first = follow_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(first.artists_polled, 1);
    assert_eq!(first.baselined, 1);
    assert!(downloads.dispatched().is_empty());

    clock.advance(60);
    poll.set(
        "artist-1",
        Ok(vec![
            ObservedRelease {
                rg_mbid: "rg-old".to_owned(),
                title: "Old".to_owned(),
                first_release_date: Some("2020-01-01".to_owned()),
                primary_type: Some("Album".to_owned()),
            },
            ObservedRelease {
                rg_mbid: "rg-new".to_owned(),
                title: "New".to_owned(),
                first_release_date: Some("2024-06-01".to_owned()),
                primary_type: Some("Album".to_owned()),
            },
            ObservedRelease {
                rg_mbid: "rg-future".to_owned(),
                title: "Future".to_owned(),
                first_release_date: Some("2025-01-01".to_owned()),
                primary_type: Some("Album".to_owned()),
            },
            ObservedRelease {
                rg_mbid: "rg-sloppy".to_owned(),
                title: "Sloppy".to_owned(),
                first_release_date: Some("2024-06".to_owned()),
                primary_type: Some("Album".to_owned()),
            },
            ObservedRelease {
                rg_mbid: "rg-single".to_owned(),
                title: "Single".to_owned(),
                first_release_date: Some("2024-06-01".to_owned()),
                primary_type: Some("Single".to_owned()),
            },
        ]),
    );
    let second = follow_tick(clock.now_unix(), &mut state, &deps).await;
    assert_eq!(second.new_releases, 2);
    // One emitting release fanned out to both followers; the future-dated
    // one waits pending, the partial date and the Single never emit.
    assert_eq!(second.enqueued, 2);
    assert_eq!(downloads.dispatched().len(), 2);
    assert!(
        downloads
            .dispatched()
            .iter()
            .all(|grab| grab.origin == "follow")
    );
    let cursor = follows.get("artist-1").unwrap();
    assert_eq!(
        cursor.pending,
        vec![PendingRelease {
            rg_mbid: "rg-future".to_owned(),
            title: "Future".to_owned(),
            date: "2025-01-01".to_owned(),
        }]
    );
}

/// Follow poll touches at most ten due artists per tick.
#[tokio::test]
async fn follow_poll_caps_artists_per_tick() {
    let now = 1_700_000_000;
    let follows = Arc::new(FollowStore::new());
    let poll = Arc::new(ScriptedPoll::new());
    for index in 0..12 {
        let mbid = format!("artist-{index}");
        follows.upsert(FollowCursor {
            artist_mbid: mbid.clone(),
            baselined: true,
            cursor_date: Some("2024-06-01".to_owned()),
            known: Default::default(),
            next_poll_at: now,
            followers: Vec::new(),
            pending: Vec::new(),
        });
        poll.set(&mbid, Ok(Vec::new()));
    }
    let deps = FollowDeps {
        follows,
        poll: poll.clone(),
        downloads: Arc::new(ScriptedDownloads::new()),
        ticks: Arc::new(MemoryTicks::new()),
        include_types: Vec::new(),
        today: Arc::new(|| "2024-06-01".to_owned()),
    };
    let mut state = LoopState::new();
    let summary = follow_tick(now, &mut state, &deps).await;
    assert_eq!(summary.artists_polled, FOLLOW_MAX_ARTISTS_PER_TICK);
    assert_eq!(poll.polls().len(), FOLLOW_MAX_ARTISTS_PER_TICK);
}

/// Loops register ephemeral, run on the honest cadence, and shut down clean.
#[tokio::test]
async fn loops_register_ephemeral_and_shut_down() {
    let (runtime, _dir) = open_scratch("loops").await;
    register_ephemeral_loop(runtime.wakeups(), runtime.lane(), FOLLOW_JOB)
        .await
        .unwrap();
    let record = runtime
        .wakeups()
        .get_job(FOLLOW_JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.kind, JobKind::Ephemeral);

    let sleeper = ManualSleeper::new();
    let ledger = Arc::new(RequestLedger::new());
    ledger.upsert(row("rg-1", "album", "user-1", "downloading"));
    let downloads = Arc::new(ScriptedDownloads::new());
    let mut active = row("rg-1", "album", "user-1", "downloading");
    active.task_id = Some("task-9".to_owned());
    ledger.upsert(active);
    downloads.set_status("task-9", "completed");
    let deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        downloads,
        library: Arc::new(LibraryPresence::new()),
        ticks: Arc::new(MemoryTicks::new()),
    });
    let (handle, registry) = spawn_sync_loop(
        sleeper.clone(),
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        Arc::new(SystemClock),
        deps,
        None,
    )
    .await
    .unwrap();
    assert_eq!(handle.name, SYNC_JOB);

    wait_for_waits(&sleeper, 1).await;
    assert_eq!(sleeper.requested(), vec![SYNC_INITIAL_DELAY]);
    sleeper.wake();
    wait_for_requests(&sleeper, 2).await;
    assert_eq!(ledger.get("rg-1").unwrap().status, "imported");
    assert!(!registry.is_live(SYNC_JOB));
    sleeper.shut_down();
    handle.task.await.unwrap();
    assert_eq!(sleeper.requested(), vec![SYNC_INITIAL_DELAY, SYNC_INTERVAL]);
    let stopped = runtime.wakeups().get_job(SYNC_JOB).await.unwrap().unwrap();
    assert_eq!(stopped.state, JobState::Stopped);
    runtime.shutdown().await;
}

/// Wanted loop spawn uses the 240s delay and the jittered 900s cadence.
#[tokio::test]
async fn wanted_loop_spawn_uses_honest_intervals() {
    let (runtime, _dir) = open_scratch("wanted-spawn").await;
    let sleeper = ManualSleeper::new();
    let deps = Arc::new(WantedDeps {
        settings: Arc::new(|| WantedSettings {
            enabled: false,
            ..WantedSettings::default()
        }),
        watches: Arc::new(WantedStore::new()),
        ledger: Arc::new(RequestLedger::new()),
        search: Arc::new(ScriptedSearch::new()),
        downloads: Arc::new(ScriptedDownloads::new()),
        library: Arc::new(LibraryPresence::new()),
        ticks: Arc::new(MemoryTicks::new()),
    });
    let (handle, _) = spawn_wanted_loop(
        sleeper.clone(),
        NoJitter,
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        Arc::new(SystemClock),
        deps,
        None,
    )
    .await
    .unwrap();
    wait_for_waits(&sleeper, 1).await;
    sleeper.wake();
    wait_for_requests(&sleeper, 2).await;
    sleeper.shut_down();
    handle.task.await.unwrap();
    assert_eq!(
        sleeper.requested(),
        vec![Duration::from_secs(240), Duration::from_secs(900)]
    );
    let _ = UPGRADE_INITIAL_DELAY;
    let _ = FOLLOW_INTERVAL;
    runtime.shutdown().await;
}

/// The wanted recheck table matches v2's age bands.
#[test]
fn wanted_interval_table_matches_v2_bands() {
    // 2024-06-01 midday UTC.
    let now = 1_716_720_000;
    assert_eq!(interval_seconds(Some("2024-05-20"), 0, now), 2 * 86_400);
    assert_eq!(interval_seconds(Some("2024-04-01"), 0, now), 4 * 86_400);
    assert_eq!(interval_seconds(Some("2024-01-15"), 0, now), 7 * 86_400);
    assert_eq!(interval_seconds(Some("2020-01-01"), 0, now), 14 * 86_400);
    assert_eq!(interval_seconds(Some("2020-01-01"), 10, now), 28 * 86_400);
    assert_eq!(interval_seconds(None, 0, now), 14 * 86_400);
    assert_eq!(interval_seconds(Some("not-a-date"), 0, now), 14 * 86_400);
}

/// Title floor and progress throttle behave as the v2 constants require.
#[test]
fn title_floor_and_progress_throttle() {
    assert!(title_containment("Blue Lines", "Blue Lines (Deluxe Edition)") >= 0.60);
    assert!(title_containment("Blue Lines", "Tribute Band Sings Jazz") < 0.60);
    assert!(progress_write_due(10.0, 11.0));
    assert!(!progress_write_due(10.5, 11.0));
}

/// Cancelled operations leave the unfinished set for startup recovery.
#[test]
fn free_music_cancel_and_recovery() {
    use flows::operations::{OpState, cancel_free_music};
    let ops = OpStore::new();
    let clock = ManualClock::new(1_700_000_000);
    let op = ops.register("free-music", "user-1:rg-x:album", clock.now_unix());
    assert_eq!(ops.unfinished().len(), 1);
    assert!(cancel_free_music(&ops, &clock, &op.id));
    assert_eq!(ops.get(&op.id).unwrap().state, OpState::Cancelled);
    assert!(ops.unfinished().is_empty());
    // Cancelling twice, or an unknown id, answers false.
    assert!(!cancel_free_music(&ops, &clock, &op.id));
    assert!(!cancel_free_music(&ops, &clock, "op-missing"));
}

/// Track requests run the same registered path with a recording MBID.
#[tokio::test]
async fn free_music_track_request_lands() {
    let (runtime, _dir) = open_scratch("free-music-track").await;
    let ops = OpStore::new();
    register_durable_ops(runtime.wakeups(), runtime.lane())
        .await
        .unwrap();
    let search = Arc::new(ScriptedSearch::new());
    search.set(
        "Ketsa",
        "Morning Light",
        Ok(vec![Candidate {
            title: "Morning Light".to_owned(),
            source: "archive".to_owned(),
        }]),
    );
    let downloads = Arc::new(ScriptedDownloads::new());
    let deps = FreeMusicDeps {
        search,
        downloads: downloads.clone(),
        handoff: Arc::new(MemoryHandoff::new()),
        ticks: Arc::new(MemoryTicks::new()),
        clock: Arc::new(ManualClock::new(1_700_000_000)),
        enabled: true,
    };
    let request = FreeMusicRequest {
        user_id: "user-1".to_owned(),
        kind: DispatchKind::Track,
        mbid: "rec-morning".to_owned(),
        artist: "Ketsa".to_owned(),
        title: "Morning Light".to_owned(),
    };
    let outcome = run_free_music(runtime.wakeups(), runtime.lane(), &ops, &deps, &request)
        .await
        .unwrap();
    let (op_id, task_id) = match outcome {
        FreeMusicOutcome::Pending { op_id, task_id } => (op_id, task_id),
        FreeMusicOutcome::Landed { .. } => panic!("expected pending while downloading"),
    };
    assert_eq!(downloads.dispatched()[0].kind, DispatchKind::Track);
    downloads.set_status(&task_id, "completed");
    let settled = settle_free_music(
        runtime.wakeups(),
        runtime.lane(),
        &ops,
        &deps,
        &op_id,
        &request,
    )
    .await
    .unwrap();
    assert!(matches!(settled, FreeMusicOutcome::Landed { .. }));
    runtime.shutdown().await;
}

/// Follow and sweep loops spawn on their honest cadences and shut down.
#[tokio::test]
async fn follow_and_sweep_loops_spawn_and_shut_down() {
    use flows::loops::{
        FOLLOW_INITIAL_DELAY, Jitter as _, ThreadJitter, UPGRADE_DEFAULT_INTERVAL_HOURS,
        UPGRADE_JOB, spawn_follow_loop, spawn_sweep_loop,
    };
    let (runtime, _dir) = open_scratch("follow-sweep-spawn").await;

    let follow_sleeper = ManualSleeper::new();
    let follow_deps = Arc::new(FollowDeps {
        follows: Arc::new(FollowStore::new()),
        poll: Arc::new(ScriptedPoll::new()),
        downloads: Arc::new(ScriptedDownloads::new()),
        ticks: Arc::new(MemoryTicks::new()),
        include_types: Vec::new(),
        today: Arc::new(|| "2024-06-01".to_owned()),
    });
    let (follow_handle, _) = spawn_follow_loop(
        follow_sleeper.clone(),
        NoJitter,
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        Arc::new(SystemClock),
        follow_deps,
        None,
    )
    .await
    .unwrap();
    assert_eq!(follow_handle.name, FOLLOW_JOB);
    wait_for_waits(&follow_sleeper, 1).await;
    assert_eq!(follow_sleeper.requested(), vec![FOLLOW_INITIAL_DELAY]);
    follow_sleeper.wake();
    wait_for_requests(&follow_sleeper, 2).await;
    assert_eq!(follow_sleeper.requested()[1], FOLLOW_INTERVAL);
    follow_sleeper.shut_down();
    follow_handle.task.await.unwrap();

    let sweep_sleeper = ManualSleeper::new();
    let policy = UpgradePolicy::default();
    let sweep_deps = Arc::new(SweepDeps {
        policy: Arc::new(move || policy.clone()),
        worklist: Arc::new(UpgradeWorklist::new()),
        admins: Arc::new(AdminDirectory::new()),
        downloads: Arc::new(ScriptedDownloads::new()),
        ticks: Arc::new(MemoryTicks::new()),
    });
    let (sweep_handle, _) = spawn_sweep_loop(
        sweep_sleeper.clone(),
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        Arc::new(SystemClock),
        sweep_deps,
        None,
    )
    .await
    .unwrap();
    assert_eq!(sweep_handle.name, UPGRADE_JOB);
    wait_for_waits(&sweep_sleeper, 1).await;
    assert_eq!(sweep_sleeper.requested(), vec![UPGRADE_INITIAL_DELAY]);
    sweep_sleeper.wake();
    wait_for_requests(&sweep_sleeper, 2).await;
    assert_eq!(
        sweep_sleeper.requested()[1],
        Duration::from_secs(UPGRADE_DEFAULT_INTERVAL_HOURS * 3600)
    );
    sweep_sleeper.shut_down();
    sweep_handle.task.await.unwrap();
    let record = runtime
        .wakeups()
        .get_job(UPGRADE_JOB)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.kind, JobKind::Ephemeral);

    // Production seams construct: the tokio sleeper over a shutdown watch,
    // and jitter inside the v2 ±20% band.
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let _tokio_sleeper = flows::loops::TokioSleeper::new(rx);
    let factor = ThreadJitter.factor();
    assert!((0.8..=1.200_001).contains(&factor));
    runtime.shutdown().await;
}

/// A shutdown that lands during the startup delay still parks the job
/// as stopped: the early return sets the state instead of skipping it.
#[tokio::test]
async fn loop_shutdown_during_startup_delay_still_stops() {
    let (runtime, _dir) = open_scratch("loop-early-shutdown").await;
    let sleeper = ManualSleeper::new();
    sleeper.shut_down();
    let ledger = Arc::new(RequestLedger::new());
    ledger.upsert(row("rg-1", "album", "user-1", "downloading"));
    let deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        downloads: Arc::new(ScriptedDownloads::new()),
        library: Arc::new(LibraryPresence::new()),
        ticks: Arc::new(MemoryTicks::new()),
    });
    let (handle, _) = spawn_sync_loop(
        sleeper.clone(),
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        Arc::new(SystemClock),
        deps,
        None,
    )
    .await
    .unwrap();
    handle.task.await.unwrap();
    // No pass ran, and the job still reads stopped.
    assert_eq!(ledger.get("rg-1").unwrap().status, "downloading");
    let stopped = runtime.wakeups().get_job(SYNC_JOB).await.unwrap().unwrap();
    assert_eq!(stopped.state, JobState::Stopped);
    runtime.shutdown().await;
}

/// Spin until the manual sleeper parks `count` waiters.
async fn wait_for_waits(sleeper: &ManualSleeper, count: usize) {
    for _ in 0..500 {
        if sleeper.waits() == count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("sleeper never parked {count} waiter(s)");
}

/// Spin until the loop records `count` sleep requests: the pass between
/// the sleeps has fully run, which a waiter count alone cannot prove.
async fn wait_for_requests(sleeper: &ManualSleeper, count: usize) {
    for _ in 0..500 {
        if sleeper.requested().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("loop never requested {count} sleep(s)");
}
