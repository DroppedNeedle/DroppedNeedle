//! Durability tests for the downloads module.
//!
//! Each test pins one crash-safety behavior against a scratch database
//! and throwaway directories: startup recovery classifies tasks from
//! their rows, failover hands work over only past the lease, and
//! orphan/quarantine sweeps fail closed. Nothing here touches the real
//! library.

use crate::common::Probe;
use droppedneedle_acquire::downloads;

use std::sync::Arc;
use std::time::Instant;

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::dispatch::DispatchOrigin;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::worker::run_startup_recovery;
use droppedneedle_acquire::held::store::{
    EntryOrigin, GroupKey, HeldRoot, HeldRows, NewEntry, NewFile,
};
use droppedneedle_acquire::reason::Reason;
use droppedneedle_providers::health::ServiceHealth;

use crate::common::bundles::quiet_ticks;

fn mount_gone() -> TaskOutcome<'static> {
    TaskOutcome::new(Reason::DownloadFailed, "mount gone")
}
use downloads::quarantine::QuarantineReason;
use downloads::recovery::{FAILOVER_CLAIM_LIMIT, FAILOVER_LEASE_SECONDS, StartupAction};
use downloads::state::{AttemptDisposition, AttemptState};
use downloads::store::{NewTask, ReimportStart, TaskOutcome};
use downloads::{
    DownloadStore, OrphanDecision, OrphanEvidence, OrphanPolicy, PollSample, QuarantineDir,
    RetryPolicy, TaskStatus, Watchdog, WatchdogConfig, WatchdogOutcome,
    canonical_soulseek_identity, classify_startup, plan_retry,
};
use rusqlite::Connection;

/// Scratch directory, removed when the test ends.
fn scratch_dir(tag: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(tag)
}

/// Scratch journal: a temp-file copy of the migrated database.
fn scratch_store(dir: &std::path::Path) -> Connection {
    crate::common::migrated_db(&dir.join("brief.db"));
    let conn = Connection::open(dir.join("brief.db")).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    conn.execute(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'brief', 'user', 0)",
        [],
    )
    .unwrap();
    conn
}

fn new_task(id: &str) -> NewTask {
    NewTask {
        id: id.to_string(),
        user_id: "u1".to_string(),
        artist_name: "artist".to_string(),
        album_title: "album".to_string(),
        release_group_mbid: "rg-1".to_string(),
        origin: "user".to_string(),
        retry_count: 0,
    }
}

// Startup recovery reads rows only. Queued tasks re-dispatch; a task
// that was landing (processing with an acquiring, in-use, preserved or
// cleanup-pending attempt) resumes its landing and is never fetched
// again; a downloading or processing task resumes polling while a live
// attempt journaled its client handle, and restarts clean under its keys
// otherwise. Settled tasks are left alone.
#[tokio::test]
async fn startup_recovery_follows_the_task_rows() {
    use StartupAction::{Noop, Redispatch, RestartClean, ResumeLanding, ResumePoll};
    const HANDLE: &str =
        r#"{"source":"soulseek","username":"peer","filenames":["a.flac"],"job_name":""}"#;
    let attempts: [(&str, Option<(AttemptState, &str)>); 10] = [
        ("none", None),
        ("acquiring", Some((AttemptState::Acquiring, HANDLE))),
        ("acquiring-bare", Some((AttemptState::Acquiring, ""))),
        ("in-use", Some((AttemptState::InUse, HANDLE))),
        ("in-use-bare", Some((AttemptState::InUse, ""))),
        ("preserved", Some((AttemptState::Preserved, HANDLE))),
        ("cleanup", Some((AttemptState::CleanupPending, HANDLE))),
        ("removed", Some((AttemptState::WorkspaceRemoved, HANDLE))),
        ("complete", Some((AttemptState::Complete, HANDLE))),
        ("attention", Some((AttemptState::NeedsAttention, HANDLE))),
    ];
    let expected: [(TaskStatus, [StartupAction; 10]); 4] = [
        (TaskStatus::Queued, [Redispatch; 10]),
        (
            TaskStatus::Downloading,
            [
                RestartClean,
                ResumePoll,
                RestartClean,
                ResumePoll,
                RestartClean,
                RestartClean,
                RestartClean,
                RestartClean,
                RestartClean,
                RestartClean,
            ],
        ),
        (
            TaskStatus::Processing,
            [
                RestartClean,
                ResumeLanding,
                ResumeLanding,
                ResumeLanding,
                ResumeLanding,
                ResumeLanding,
                ResumeLanding,
                RestartClean,
                RestartClean,
                RestartClean,
            ],
        ),
        (TaskStatus::Failed, [Noop; 10]),
    ];
    let db = AcquireDb::scratch().unwrap();
    db.add_user("u1", "brief", "user").await.unwrap();
    let journal = Journal::new(db, quiet_ticks());
    let mut cases = Vec::new();
    for (status, actions) in expected {
        for ((attempt, row), action) in attempts.iter().zip(actions) {
            let id = format!("{}-{attempt}", status.as_str());
            let (task_id, row) = (id.clone(), *row);
            journal
                .run("test.seed", move |store| {
                    store.insert_task(&new_task(&task_id), 1.0)?;
                    if status != TaskStatus::Queued {
                        store.transition_task(&task_id, status, 1.0, None)?;
                    }
                    if let Some((state, handle)) = row {
                        store.insert_attempt(
                            &format!("{task_id}-a0"),
                            &task_id,
                            "soulseek",
                            0,
                            "",
                            handle,
                            state,
                            1.0,
                        )?;
                    }
                    Ok(())
                })
                .await
                .unwrap();
            cases.push((id, status, action));
        }
    }
    for (id, status, action) in &cases {
        let rows = journal.read_attempts(id).await.unwrap();
        assert_eq!(classify_startup(*status, &rows), *action, "{id}");
    }

    let report = run_startup_recovery(&journal).await.unwrap();
    let count = |wanted: &[StartupAction]| {
        cases
            .iter()
            .filter(|(_, _, action)| wanted.contains(action))
            .count()
    };
    assert_eq!(report.redispatched, count(&[Redispatch]));
    assert_eq!(report.resumed, count(&[ResumeLanding, ResumePoll]));
    assert_eq!(report.restarted, count(&[RestartClean]));
    for (id, status, action) in &cases {
        let now = journal.read_task(id).await.unwrap().unwrap().status;
        let after = if *action == RestartClean {
            TaskStatus::Queued
        } else {
            *status
        };
        assert_eq!(now, after, "{id}");
    }
}

/// One scratch store: a cleanup lease passes to a successor only once it
/// expires and a stale revision loses, terminal rows never move, and an
/// idempotency key answers the first task.
#[test]
fn the_task_store_hands_each_task_to_one_owner() {
    let dir = scratch_dir("one-owner");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;

    // Failover: the lease.
    store.insert_task(&new_task("lease"), now).unwrap();
    store
        .insert_attempt(
            "a1",
            "lease",
            "usenet",
            0,
            "droppedneedle-job",
            "{}",
            AttemptState::CleanupPending,
            now,
        )
        .unwrap();

    // First orchestrator claims the row.
    let claimed = store
        .claim_cleanup_attempts(
            "worker-a",
            now,
            FAILOVER_CLAIM_LIMIT,
            FAILOVER_LEASE_SECONDS,
        )
        .unwrap();
    assert_eq!(claimed.len(), 1);

    // A successor cannot steal the live lease.
    let stolen = store
        .claim_cleanup_attempts(
            "worker-b",
            now + 10.0,
            FAILOVER_CLAIM_LIMIT,
            FAILOVER_LEASE_SECONDS,
        )
        .unwrap();
    assert!(stolen.is_empty());

    // Past the lease, the successor takes over.
    let taken = store
        .claim_cleanup_attempts(
            "worker-b",
            now + FAILOVER_LEASE_SECONDS + 1.0,
            FAILOVER_CLAIM_LIMIT,
            FAILOVER_LEASE_SECONDS,
        )
        .unwrap();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].lease_owner.as_deref(), Some("worker-b"));

    // A revision-CAS transition against the stale revision fails instead
    // of clobbering the new owner's state.
    let stale = store
        .transition_attempt(
            "a1",
            claimed[0].row_revision,
            AttemptState::Complete,
            now,
            None,
            None,
            true,
        )
        .unwrap();
    assert!(stale.is_none());

    // Terminal rows never move, not even back to queued.
    store.insert_task(&new_task("done"), now).unwrap();
    store
        .transition_task("done", TaskStatus::Failed, now, None)
        .unwrap();
    assert!(
        store
            .transition_task("done", TaskStatus::Queued, now, None)
            .is_err()
    );
    // Finalizing into a non-terminal status is refused, not asserted.
    assert!(
        store
            .finalize_task_and_attempt("done", TaskStatus::Downloading, now, None, None, false)
            .is_err()
    );
    // Startup classification leaves misfed terminal rows alone.
    for status in [
        TaskStatus::Completed,
        TaskStatus::Partial,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ] {
        assert_eq!(
            classify_startup(status, &[]),
            StartupAction::Noop,
            "{status:?}"
        );
    }

    // An idempotency key answers the task that claimed it first.
    store.insert_task(&new_task("task-a"), now).unwrap();
    assert!(
        store
            .claim_key("dispatch:k1", "task-a", "dispatch", now)
            .unwrap()
    );
    assert!(
        !store
            .claim_key("dispatch:k1", "task-b", "dispatch", now)
            .unwrap()
    );
    assert_eq!(
        store.task_id_for_key("dispatch:k1").unwrap().as_deref(),
        Some("task-a")
    );
    assert_eq!(store.task_id_for_key("dispatch:missing").unwrap(), None);
}

#[test]
fn stale_tasks_reap_failed_but_keep_their_attempts() {
    let dir = scratch_dir("reap");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;
    let watchdog = Watchdog::new(WatchdogConfig::default());

    store.insert_task(&new_task("dead"), now - 7200.0).unwrap();
    store
        .transition_task("dead", TaskStatus::Downloading, now - 7200.0, None)
        .unwrap();
    store
        .insert_attempt(
            "a-dead",
            "dead",
            "soulseek",
            0,
            "",
            "{}",
            AttemptState::InUse,
            now - 7200.0,
        )
        .unwrap();

    store.insert_task(&new_task("live"), now).unwrap();
    store
        .transition_task("live", TaskStatus::Downloading, now, None)
        .unwrap();

    let unpolled = store
        .list_unpolled_active(now, watchdog.reap_threshold_seconds())
        .unwrap();
    let ids: Vec<&str> = unpolled.iter().map(|task| task.id.as_str()).collect();
    assert!(ids.contains(&"dead"));
    assert!(!ids.contains(&"live"));

    // The reap sweep fails the dead task but preserves its attempt, so
    // cleanup evidence survives the reap.
    store
        .finalize_task_and_attempt(
            "dead",
            TaskStatus::Failed,
            now,
            Some(TaskOutcome::new(
                Reason::DownloadFailed,
                "Download interrupted - no progress after a restart",
            )),
            Some("a-dead"),
            true,
        )
        .unwrap();
    let task = store.get_task("dead").unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert!(task.completed_at.is_some());
    let attempt = store.get_attempt("a-dead").unwrap().unwrap();
    assert_eq!(attempt.state, AttemptState::Preserved);
}

#[test]
fn auto_retry_sweep_honors_backoff_newest_wins_and_held_gate() {
    let dir = scratch_dir("retry");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;
    let policy = RetryPolicy::default();

    assert_eq!(policy.next_retry_at(0, now, "failed"), Some(now + 900.0));
    assert_eq!(policy.next_retry_at(6, now, "failed"), None);
    assert_eq!(policy.next_retry_at(0, now, "completed"), None);

    // Original failure plus its retry successor: only the newest seeds.
    let mut first = new_task("orig");
    first.release_group_mbid = "rg-retry".to_string();
    store.insert_task(&first, now - 10_000.0).unwrap();
    store
        .transition_task(
            "orig",
            TaskStatus::Failed,
            now - 9000.0,
            Some(TaskOutcome::new(Reason::DownloadFailed, "peer vanished")),
        )
        .unwrap();
    let mut second = new_task("orig-r1");
    second.release_group_mbid = "rg-retry".to_string();
    second.origin = "retry".to_string();
    second.retry_count = 1;
    store.insert_task(&second, now - 8000.0).unwrap();
    store
        .transition_task(
            "orig-r1",
            TaskStatus::Failed,
            now - 7000.0,
            Some(TaskOutcome::new(Reason::DownloadFailed, "peer vanished")),
        )
        .unwrap();

    // A failed upgrade never auto-retries.
    let mut upgrade = new_task("up");
    upgrade.origin = "upgrade".to_string();
    store.insert_task(&upgrade, now - 6000.0).unwrap();
    store
        .transition_task("up", TaskStatus::Failed, now - 5000.0, None)
        .unwrap();

    let retryable = store.list_retryable(6).unwrap();
    let ids: Vec<&str> = retryable.iter().map(|task| task.id.as_str()).collect();
    assert_eq!(ids, vec!["orig-r1"]);

    // A held track pauses auto-retry for its task, while its entry is open.
    let held = HeldRows::new(&conn);
    let (entry, _) = held
        .hold(
            &NewEntry {
                user_id: "u1".to_owned(),
                origin: EntryOrigin::Download,
                key: GroupKey::Album("rg-retry".to_owned()),
                release_group_mbid: Some("rg-retry".to_owned()),
                recording_mbid: None,
                drop_item_id: None,
                artist_name: None,
                album_title: None,
            },
            &[NewFile {
                source_task_id: Some("orig-r1".to_owned()),
                ..NewFile::new(HeldRoot::Held, "01.flac", Reason::WeakMatch, "soulseek")
            }],
            1,
        )
        .unwrap();
    assert!(held.task_holds("orig-r1").unwrap());
    assert!(!held.task_holds("orig").unwrap());
    held.release(entry, 2).unwrap();
    assert!(held.claim(entry).unwrap());
    assert!(
        held.task_holds("orig-r1").unwrap(),
        "an entry being acted on is open"
    );

    // Retry spawns keep the original terminal and carry the generation.
    let spawn = plan_retry("orig-r1", TaskStatus::Failed, "retry", 1).unwrap();
    assert_eq!(spawn.task_id, "orig-r1-r2");
    assert_eq!(spawn.origin, DispatchOrigin::Retry);
    assert_eq!(spawn.retry_count, 2);
    let upgrade_spawn = plan_retry("up", TaskStatus::Failed, "upgrade", 0).unwrap();
    assert_eq!(upgrade_spawn.origin, DispatchOrigin::Upgrade);
    assert!(plan_retry("live", TaskStatus::Downloading, "user", 0).is_none());
}

/// Quarantine rows expire and clear on a manual retry, and a suspect file
/// moves only into the sandbox, never under a stamp that escapes it.
#[test]
fn quarantine_expires_and_holds_files_only_in_its_sandbox() {
    let dir = scratch_dir("quar");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;
    let ttl = 7.0 * 24.0 * 3600.0;

    let identity = canonical_soulseek_identity("Peer\\Music\\Album");
    store
        .record_quarantine(
            "soulseek",
            &identity,
            QuarantineReason::VerifyFailed.as_str(),
            Some("rg-1"),
            now,
            ttl,
        )
        .unwrap();
    let live = store.load_quarantine_set(now + 60.0, ttl).unwrap();
    assert_eq!(live, vec![("soulseek".to_string(), identity.clone())]);

    // Expired entries disappear from reads and are pruned by the next write.
    assert!(
        store
            .load_quarantine_set(now + ttl + 1.0, ttl)
            .unwrap()
            .is_empty()
    );
    store
        .record_quarantine(
            "usenet",
            "job-1",
            QuarantineReason::Corrupt.as_str(),
            None,
            now + ttl + 2.0,
            ttl,
        )
        .unwrap();
    let rows = store.list_quarantine().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].identity, "job-1");

    // Manual retry clears the album scope so the release is reconsidered.
    store
        .record_quarantine(
            "soulseek",
            &identity,
            QuarantineReason::VerifyFailed.as_str(),
            Some("rg-1"),
            now + ttl + 3.0,
            ttl,
        )
        .unwrap();
    assert_eq!(store.delete_quarantine_for_album("rg-1").unwrap(), 1);

    // Suspect files move into the sandbox only, and a stamp that would
    // climb out of it is refused with the file left where it was.
    let sandbox = QuarantineDir::sandbox(dir.join("quarantine"));
    let suspect = dir.join("suspect.flac");
    std::fs::write(&suspect, b"bad bytes").unwrap();
    assert!(sandbox.hold_file(&suspect, "../escape").is_err());
    assert!(sandbox.hold_file(&suspect, "a/b").is_err());
    assert!(sandbox.hold_file(&suspect, "..").is_err());
    assert!(suspect.exists());
    let held = sandbox.hold_file(&suspect, "20240101T000000").unwrap();
    assert!(!suspect.exists());
    assert!(held.exists());
    assert!(held.starts_with(dir.join("quarantine")));
}

#[test]
fn orphan_reconcile_removes_only_proven_debris() {
    let policy = OrphanPolicy::default();
    let name = "droppedneedle-0123456789abcdef0123456789abcdef-3";
    let settled = OrphanEvidence {
        has_cleanup_debt: false,
        task_active: false,
        bundles_settled: true,
        mount_healthy: true,
        client_job_active: false,
        age_seconds: 7.0 * 3600.0,
    };
    assert_eq!(
        policy.evaluate(name, false, Some(settled)),
        OrphanDecision::Remove
    );

    // Every ambiguous answer keeps the folder.
    let ambiguous = [
        OrphanEvidence {
            has_cleanup_debt: true,
            ..settled
        },
        OrphanEvidence {
            task_active: true,
            ..settled
        },
        OrphanEvidence {
            bundles_settled: false,
            ..settled
        },
        OrphanEvidence {
            mount_healthy: false,
            ..settled
        },
        OrphanEvidence {
            client_job_active: true,
            ..settled
        },
        OrphanEvidence {
            age_seconds: 60.0,
            ..settled
        },
    ];
    for evidence in ambiguous {
        assert_eq!(
            policy.evaluate(name, false, Some(evidence)),
            OrphanDecision::Keep
        );
    }
    assert_eq!(policy.evaluate(name, false, None), OrphanDecision::Keep);

    // Foreign names and symlinks are never touched.
    assert_eq!(
        policy.evaluate("someones-album", false, Some(settled)),
        OrphanDecision::Ignore
    );
    assert_eq!(
        policy.evaluate(name, true, Some(settled)),
        OrphanDecision::Ignore
    );

    // End to end over a fake complete dir: only the proven debris goes.
    let mount = scratch_dir("complete");
    let debris = mount.join(name);
    std::fs::create_dir_all(&debris).unwrap();
    std::fs::write(debris.join("01.flac"), b"audio").unwrap();
    let foreign = mount.join("someones-album");
    std::fs::create_dir_all(&foreign).unwrap();

    let mut removed = 0;
    for entry in std::fs::read_dir(&mount).unwrap() {
        let entry = entry.unwrap();
        let entry_name = entry.file_name().to_str().unwrap().to_string();
        let is_symlink = entry.file_type().unwrap().is_symlink();
        if policy.evaluate(&entry_name, is_symlink, Some(settled)) == OrphanDecision::Remove {
            std::fs::remove_dir_all(entry.path()).unwrap();
            removed += 1;
        }
    }
    assert_eq!(removed, 1);
    assert!(!debris.exists());
    assert!(foreign.exists());
}

#[test]
fn watchdog_orders_terminal_ceiling_stall_and_queue() {
    let watchdog = Watchdog::new(WatchdogConfig::default());
    let base = PollSample {
        elapsed_seconds: 10.0,
        idle_seconds: 0.0,
        has_active_transfer: true,
        downloaded_bytes: 100,
        all_terminal: false,
        all_succeeded: false,
        materialize_wait_seconds: 0.0,
    };
    assert_eq!(watchdog.evaluate(&base), WatchdogOutcome::Continue);

    let done = PollSample {
        all_terminal: true,
        all_succeeded: true,
        ..base.clone()
    };
    assert_eq!(watchdog.evaluate(&done), WatchdogOutcome::Completed);
    let failed = PollSample {
        all_terminal: true,
        all_succeeded: false,
        ..base.clone()
    };
    assert_eq!(watchdog.evaluate(&failed), WatchdogOutcome::Terminal);

    let over = PollSample {
        elapsed_seconds: 6.0 * 3600.0 + 1.0,
        ..base.clone()
    };
    assert_eq!(watchdog.evaluate(&over), WatchdogOutcome::Deadline);

    let stalled = PollSample {
        idle_seconds: 31.0 * 60.0,
        ..base.clone()
    };
    assert_eq!(watchdog.evaluate(&stalled), WatchdogOutcome::Stalled);

    let queued = PollSample {
        has_active_transfer: false,
        downloaded_bytes: 0,
        idle_seconds: 121.0 * 60.0,
        elapsed_seconds: 121.0 * 60.0,
        materialize_wait_seconds: 0.0,
        ..base.clone()
    };
    assert_eq!(watchdog.evaluate(&queued), WatchdogOutcome::QueuedTimeout);

    // Past the stall mark with no active transfer and past the materialize
    // window: the materialize fast-fail fires first.
    let never_materialized = PollSample {
        has_active_transfer: false,
        downloaded_bytes: 0,
        idle_seconds: 31.0 * 60.0,
        materialize_wait_seconds: 120.0,
        ..base.clone()
    };
    assert_eq!(
        watchdog.evaluate(&never_materialized),
        WatchdogOutcome::MaterializeTimeout
    );
}

/// Reimport needs a failed or partial task whose attempt still names its
/// files and moves it to processing with its error cleared. Kept files go
/// to cleanup after 90 days or when their task stops, and a reimport and
/// cleanup each take the attempt by compare and set, so neither works on
/// files the other has.
#[test]
fn reimport_and_cleanup_never_share_kept_files() {
    // Case "reimport": the guard is keyed on the attempt journal.
    let dir = scratch_dir("reimport");
    let conn = scratch_store(&dir);
    let now = 1_700_000_000.0;
    let store = DownloadStore::new(&conn);
    for id in ["kept", "bare", "live"] {
        store.insert_task(&new_task(id), now).unwrap();
    }
    let handle = r#"{"source":"soulseek","username":"peer","filenames":["a.flac"],"job_name":""}"#;
    store
        .insert_attempt(
            "kept-a0",
            "kept",
            "soulseek",
            0,
            "",
            handle,
            AttemptState::Preserved,
            now,
        )
        .unwrap();
    store
        .insert_attempt(
            "live-a0",
            "live",
            "soulseek",
            0,
            "",
            handle,
            AttemptState::InUse,
            now,
        )
        .unwrap();
    store
        .transition_task("kept", TaskStatus::Failed, now, Some(mount_gone()))
        .unwrap();
    store
        .transition_task("bare", TaskStatus::Failed, now, Some(mount_gone()))
        .unwrap();

    assert!(store.is_reimportable("kept").unwrap());
    assert!(!store.is_reimportable("bare").unwrap());
    assert!(!store.is_reimportable("live").unwrap());
    assert!(!store.is_reimportable("missing").unwrap());

    assert_eq!(
        store.begin_reimport("kept", "kept-a0", now).unwrap(),
        ReimportStart::Begun
    );
    let row = store.get_task("kept").unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Processing);
    assert!(row.error_message.is_none());
    assert!(row.completed_at.is_none());
    assert_eq!(
        store.begin_reimport("kept", "kept-a0", now).unwrap(),
        ReimportStart::NotReimportable,
        "already running"
    );
    assert_eq!(
        store.begin_reimport("live", "live-a0", now).unwrap(),
        ReimportStart::NotReimportable
    );
    assert_eq!(
        store.begin_reimport("missing", "missing-a0", now).unwrap(),
        ReimportStart::NotReimportable
    );

    // Case "kept files": a reimport that started keeps them past the 90
    // days, a reimport takes back files cleanup has not leased yet, and
    // files cleanup has leased are never reimported. Its own store, so the
    // 90-day release sees only these rows.
    let dir = scratch_dir("kept-files");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 100.0 * 86_400.0;
    let kept_before = now - 90.0 * 86_400.0;
    let handle = r#"{"source":"soulseek","username":"peer","filenames":["a.flac"],"job_name":""}"#;
    for (id, state) in [
        ("young", AttemptState::Preserved),
        ("old", AttemptState::Preserved),
        ("reimported", AttemptState::Preserved),
        ("stopped", AttemptState::Preserved),
        ("leased", AttemptState::CleanupPending),
        ("taken", AttemptState::CleanupPending),
    ] {
        store.insert_task(&new_task(id), 1.0).unwrap();
        store
            .insert_attempt(
                &format!("{id}-a0"),
                id,
                "soulseek",
                0,
                "",
                handle,
                state,
                1.0,
            )
            .unwrap();
        store
            .transition_task(id, TaskStatus::Failed, 1.0, Some(mount_gone()))
            .unwrap();
    }
    conn.execute(
        "UPDATE download_attempts SET completed_at = ?1 WHERE task_id = 'young'",
        [((now - 89.0 * 86_400.0) * 1000.0) as i64],
    )
    .unwrap();
    conn.execute(
        "UPDATE download_attempts SET lease_owner = 'cleaner', lease_expires_at = ?1 \
         WHERE task_id = 'leased'",
        [((now + 60.0) * 1000.0) as i64],
    )
    .unwrap();
    let state = |task: &str| {
        let id = format!("{task}-a0");
        store.get_attempt(&id).unwrap().unwrap().state
    };

    // A reimport that started first keeps its files past the 90 days.
    assert_eq!(
        store
            .begin_reimport("reimported", "reimported-a0", now)
            .unwrap(),
        ReimportStart::Begun
    );
    conn.execute(
        "UPDATE download_attempts SET completed_at = 1.0, updated_at = 1.0 \
         WHERE task_id = 'reimported'",
        [],
    )
    .unwrap();
    // A reimport takes back files cleanup has not leased yet.
    assert_eq!(
        store.begin_reimport("taken", "taken-a0", now).unwrap(),
        ReimportStart::Begun
    );
    assert_eq!(state("taken"), AttemptState::Preserved);
    // Stopping a failed task hands its kept files to cleanup at once.
    let stopped = store.get_task("stopped").unwrap().unwrap();
    store.stop_task(&stopped, now).unwrap();
    assert_eq!(state("stopped"), AttemptState::CleanupPending);

    let released = store.release_kept(now, kept_before).unwrap();
    assert_eq!(released, vec![("old-a0".to_owned(), "old".to_owned())]);
    assert_eq!(state("young"), AttemptState::Preserved);
    assert_eq!(state("reimported"), AttemptState::Preserved);
    assert_eq!(state("old"), AttemptState::CleanupPending);
    let old = store.get_attempt("old-a0").unwrap().unwrap();
    assert_eq!(old.disposition, AttemptDisposition::Discard);

    // Files cleanup has leased are not reimported.
    let claimed = store
        .claim_cleanup_attempts("cleaner", now, 10, 60.0)
        .unwrap();
    assert!(claimed.iter().any(|attempt| attempt.id == "old-a0"));
    for task in ["old", "leased"] {
        assert_eq!(
            store
                .begin_reimport(task, &format!("{task}-a0"), now)
                .unwrap(),
            ReimportStart::FilesGone,
            "{task}"
        );
        assert_eq!(
            store.get_task(task).unwrap().unwrap().status,
            TaskStatus::Failed
        );
    }
}

// A download whose temporary files keep failing to clear shows on service
// health with a warning once its retried cleanup has failed three
// times, and the warning clears as soon as a cleanup succeeds.
#[tokio::test]
async fn failing_cleanup_shows_on_service_health_until_it_clears() {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("u1", "brief", "user").await.unwrap();
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t1"), 1_000.0)?;
            store.transition_task("t1", TaskStatus::Cancelled, 1_000.0, None)?;
            store.insert_attempt(
                "t1-a0",
                "t1",
                "plugin:fixed",
                0,
                "fixed-t1-0",
                "{}",
                AttemptState::CleanupPending,
                1_000.0,
            )
        })
        .await
        .unwrap();
    let dir = db.path().parent().unwrap().to_path_buf();
    let platform = crate::common::TestPlatform::on(db.db(), &dir).build().await;
    let (library, _) = crate::common::library::library_at(&platform);
    let source = crate::common::acquire::FixedPlugin::install(&platform, library.core.clone());
    source.serve(&dir.join("staging/downloads"), &[]);
    source.fail_discards(true);
    let health = Arc::new(ServiceHealth::new());
    let worker = crate::common::acquire::worker_with(
        &platform,
        source.host.clone(),
        journal.clone(),
        &platform.settings,
        dir.join("slskd"),
        Arc::new(droppedneedle_acquire::target::Targets::new(
            journal.clone(),
            Arc::new(crate::common::acquire::FixedAlbums::default()),
        )),
        Arc::new(droppedneedle_acquire::landing::LandingService::new(
            journal,
            Arc::new(
                droppedneedle_acquire::landing::library::LibraryLanding::new(
                    &library,
                    library.imports.releases.clone(),
                ),
            ),
            platform.settings.live(),
            dir.join("held"),
        )),
        health.clone(),
        droppedneedle_kernel::events::EventHub::new(),
    );
    // Each failed cleanup backs off; make the row due again without waiting.
    let due_now = || {
        db.write("test.due", |tx| {
            tx.execute("UPDATE download_attempts SET next_retry_at = 0", [])?;
            Ok(())
        })
    };

    for pass in 1..=2 {
        due_now().await.unwrap();
        worker.run_once(pass).await;
        assert!(
            health.current(Instant::now()).is_empty(),
            "failure {pass} is a blip"
        );
    }
    due_now().await.unwrap();
    worker.run_once(3).await;
    let live = health.current(Instant::now());
    assert_eq!(live.len(), 1, "{live:?}");
    assert_eq!(live[0].service, "acquisition_cleanup");
    assert_eq!(live[0].capability, "source files");
    assert_eq!(
        live[0].message,
        "Temporary files couldn't be removed for 1 download. \
         Your library is safe. Retrying automatically."
    );

    source.fail_discards(false);
    due_now().await.unwrap();
    worker.run_once(4).await;
    assert!(health.current(Instant::now()).is_empty());
}

const HELD_GROUP: &str = "6d0b7a1e-0000-4000-8000-0000000000a1";
const HELD_RELEASE: &str = "6d0b7a1e-0000-4000-8000-0000000000a2";

const LIVE_GROUP: &str = "6d0b7a1e-0000-4000-8000-0000000000a3";
const LIVE_RELEASE: &str = "6d0b7a1e-0000-4000-8000-0000000000a4";
const DELUXE_RELEASE: &str = "6d0b7a1e-0000-4000-8000-0000000000a5";
const REMASTER_RELEASE: &str = "6d0b7a1e-0000-4000-8000-0000000000a6";
const MYSTERONS: &str = "6d0b7a1e-0000-4000-8000-0000000000c0";
const SOUR_TIMES: &str = "6d0b7a1e-0000-4000-8000-0000000000c1";

/// MusicBrainz as a fixed set of releases.
struct Releases(Vec<droppedneedle_library::matching::Release>);

impl droppedneedle_library::identify::sources::ReleaseSource for Releases {
    fn search<'a>(
        &'a self,
        _title: &'a str,
        _artist: &'a str,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<
            Vec<droppedneedle_library::identify::sources::ReleaseHit>,
            droppedneedle_library::identify::sources::SourceError,
        >,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn release<'a>(
        &'a self,
        mbid: &'a str,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<
            Option<droppedneedle_library::matching::Release>,
            droppedneedle_library::identify::sources::SourceError,
        >,
    > {
        let found = self
            .0
            .iter()
            .find(|release| release.answers_to(mbid))
            .cloned();
        Box::pin(async move { Ok(found) })
    }

    fn canonical_recording<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<Option<String>, droppedneedle_library::identify::sources::SourceError>,
    > {
        Box::pin(async { Ok(None) })
    }
}

/// Portishead's "Dummy" with two tracks.
fn held_release() -> droppedneedle_library::matching::Release {
    portishead(
        HELD_RELEASE,
        HELD_GROUP,
        "Dummy",
        &[(MYSTERONS, "Mysterons"), (SOUR_TIMES, "Sour Times")],
    )
}

/// A Portishead album with the tracks `(recording, title)`; each track's
/// id is made from the release's, so no two releases share one.
fn portishead(
    id: &str,
    group: &str,
    title: &str,
    tracks: &[(&str, &str)],
) -> droppedneedle_library::matching::Release {
    use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseMedium, ReleaseTrack};
    Release {
        id: id.to_owned(),
        release_group_id: group.to_owned(),
        title: title.to_owned(),
        artists: vec![CreditedArtist {
            id: "8f6bd1e4-fbe1-4f50-aa9b-94c450ec0f11".to_owned(),
            name: "Portishead".to_owned(),
            sort_name: Some("Portishead".to_owned()),
            join: String::new(),
        }],
        status: Some("Official".to_owned()),
        media: vec![ReleaseMedium {
            position: 1,
            format: Some("CD".to_owned()),
            title: None,
            track_count: tracks.len() as u32,
        }],
        tracks: tracks
            .iter()
            .enumerate()
            .map(|(index, (recording, title))| ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: format!("{}b{}{index}", &id[..id.len() - 3], &id[id.len() - 1..]),
                recording_id: (*recording).to_owned(),
                title: (*title).to_owned(),
                artists: Vec::new(),
                disc: 1,
                position: index as u32 + 1,
                absolute_position: index as u32 + 1,
                length_ms: None,
            })
            .collect(),
        ..Release::default()
    }
}

/// A held copy in `dir`: a library fixture tagged as track `track`.
fn held_copy(dir: &std::path::Path, name: &str, fixture: &str, title: &str, track: u32) {
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};
    std::fs::create_dir_all(dir).expect("held dir");
    let path = dir.join(name);
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/library")
        .join(fixture);
    std::fs::copy(source, &path).expect("copy fixture");
    let edits = vec![
        TagEdit::new(TagField::Title, vec![title.to_owned()]),
        TagEdit::new(TagField::Artist, vec!["Portishead".to_owned()]),
        TagEdit::new(TagField::AlbumArtist, vec!["Portishead".to_owned()]),
        TagEdit::new(TagField::Album, vec!["Dummy".to_owned()]),
        TagEdit::new(TagField::TrackNumber, vec![track.to_string()]),
    ];
    save_tags(&path, &edits).expect("tags save");
}

/// Hold the files `(path, track, reason)` in a new download entry of
/// u-ada's under `key`, in `state`. Answers the entry.
async fn seed_held(
    db: &AcquireDb,
    key: &str,
    state: &str,
    files: Vec<(&str, droppedneedle_library::matching::ReleaseTrack, Reason)>,
) -> i64 {
    seed_held_on(db, key, HELD_GROUP, HELD_RELEASE, state, files).await
}

/// [`seed_held`] for an entry about album `group` whose files are paired
/// to tracks of `release`.
async fn seed_held_on(
    db: &AcquireDb,
    key: &str,
    group: &'static str,
    release: &'static str,
    state: &str,
    files: Vec<(&str, droppedneedle_library::matching::ReleaseTrack, Reason)>,
) -> i64 {
    let (key, state) = (key.to_owned(), state.to_owned());
    let files: Vec<(String, String, String, u32, &'static str)> = files
        .into_iter()
        .map(|(path, track, reason)| {
            (
                path.to_owned(),
                track.id,
                track.recording_id,
                track.position,
                reason.code(),
            )
        })
        .collect();
    db.write("test.held", move |tx| {
        tx.execute(
            "INSERT INTO acquire_held_entries (user_id, origin, group_key, \
             release_group_mbid, state, created_at, expires_at) VALUES ('u-ada', \
             'download', ?1, ?2, ?3, 1, 9999999999999)",
            rusqlite::params![key, group, state],
        )?;
        let entry = tx.last_insert_rowid();
        for (path, track, recording, position, reason) in &files {
            tx.execute(
                "INSERT INTO acquire_held_files (entry_id, root, path, original_name, \
                 release_mbid, release_track_mbid, recording_mbid, disc_number, \
                 track_number, reason, source, state, created_at) VALUES (?1, 'held', ?2, \
                 ?2, ?3, ?4, ?5, 1, ?6, ?7, 'soulseek', 'held', 1)",
                rusqlite::params![entry, path, release, track, recording, position, reason],
            )?;
        }
        Ok(entry)
    })
    .await
    .expect("held rows")
}

/// An entry's state, tries and next try.
fn retry_of(db: &AcquireDb, entry: i64) -> (String, i64, Option<i64>) {
    db.db().row(
        "SELECT state, retry_tries, retry_at FROM acquire_held_entries WHERE id = ?1",
        rusqlite::params![entry],
    )
}

/// A held file's state and reason, by its path.
fn held_file(db: &AcquireDb, path: &str) -> (String, String) {
    db.db().row(
        "SELECT state, reason FROM acquire_held_files WHERE path = ?1",
        rusqlite::params![path],
    )
}

/// A file held because its place in the library was taken is retried by
/// itself, exactly as its owner pressing Retry: at the first pass, then 5
/// and 15 minutes later while the place stays taken, and it goes in at the
/// pass after the place is freed. A file a person decides on, in the same
/// entry, is never touched, and an entry a person is acting on is skipped.
const MINUTE: i64 = 60_000;

/// The held list over a real library on a scratch database, with
/// MusicBrainz as `releases` and the clock in the test's hands.
struct HeldRig {
    clock: Arc<droppedneedle_kernel::test_support::ManualClock>,
    db: AcquireDb,
    acquire: droppedneedle_acquire::AcquireSetup,
    music: std::path::PathBuf,
    held_dir: std::path::PathBuf,
    _platform: droppedneedle_kernel::platform::Platform,
    _scratch: AcquireDb,
}

impl HeldRig {
    async fn new(releases: Vec<droppedneedle_library::matching::Release>) -> Self {
        use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
        use droppedneedle_kernel::test_support::ManualClock;

        let scratch = AcquireDb::scratch().expect("scratch db");
        let clock = Arc::new(ManualClock::at(1_800_000_000_000));
        let db = AcquireDb::new(scratch.db().clone(), clock.clone());
        db.add_user("u-ada", "Ada", "user").await.expect("user");
        let dir = scratch.path().parent().expect("scratch dir").to_path_buf();
        let config = Arc::new(
            ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
                .expect("config"),
        );
        let platform = crate::common::TestPlatform::on(db.db(), &dir)
            .clock(clock.clone())
            .settings(config)
            .build()
            .await;
        let (library, _) = crate::common::library::library_at(&platform);
        let music = dir.join("music");
        std::fs::create_dir_all(&music).expect("music root");
        library
            .scans
            .add_root(
                Some("music".to_owned()),
                music.to_string_lossy().into_owned(),
                droppedneedle_library::scan::EffectivePolicy::Automatic,
            )
            .expect("root adds");
        let landing = droppedneedle_acquire::landing::library::LibraryLanding::new(
            &library,
            crate::common::library::every_priority(Arc::new(Releases(releases))),
        );
        let acquire = crate::common::bundles::acquire_on(&platform, &library, Arc::new(landing));
        let held_dir = acquire.held.folders().held.clone();
        Self {
            clock,
            db,
            acquire,
            music,
            held_dir,
            _platform: platform,
            _scratch: scratch,
        }
    }
}

#[tokio::test]
async fn held_files_the_library_could_not_take_are_retried_by_themselves() {
    use droppedneedle_kernel::clock::Clock as _;

    let rig = HeldRig::new(vec![held_release()]).await;
    let tracks = held_release().tracks;
    let (clock, db, acquire, music, held_dir) =
        (&rig.clock, &rig.db, &rig.acquire, &rig.music, &rig.held_dir);
    held_copy(held_dir, "taken.flac", "flac_full_01.flac", "Mysterons", 1);
    held_copy(held_dir, "weak.flac", "flac_full_02.flac", "Sour Times", 2);
    held_copy(held_dir, "free.flac", "flac_full_02.flac", "Sour Times", 2);
    held_copy(
        held_dir,
        "claimed.flac",
        "flac_full_02.flac",
        "Sour Times",
        2,
    );
    let album = music.join("Portishead/Dummy");
    std::fs::create_dir_all(&album).expect("album folder");
    let taken = album.join("0101 Mysterons.flac");
    std::fs::write(&taken, b"someone else's file").expect("stray file");

    let waiting = seed_held(
        db,
        &format!("album:{HELD_GROUP}"),
        "held",
        vec![
            ("taken.flac", tracks[0].clone(), Reason::TargetOccupied),
            ("weak.flac", tracks[1].clone(), Reason::WeakMatch),
        ],
    )
    .await;
    let claimed = seed_held(
        db,
        "task:t-claimed",
        "placing",
        vec![("claimed.flac", tracks[1].clone(), Reason::TargetOccupied)],
    )
    .await;
    let fresh = seed_held(
        db,
        "task:t-fresh",
        "held",
        vec![("free.flac", tracks[1].clone(), Reason::TargetOccupied)],
    )
    .await;

    let start = clock.now_ms();
    acquire.held.tend().await;
    assert_eq!(
        held_file(db, "free.flac").0,
        "imported",
        "a fresh hold whose place is free goes in at the first pass"
    );
    assert!(album.join("0102 Sour Times.flac").is_file());
    assert_eq!(retry_of(db, fresh), ("resolved".to_owned(), 0, None));
    assert_eq!(
        retry_of(db, waiting),
        ("held".to_owned(), 1, Some(start + 5 * MINUTE)),
        "a place still taken waits five minutes"
    );
    assert_eq!(
        held_file(db, "taken.flac"),
        ("held".to_owned(), "target_occupied".to_owned())
    );
    assert_eq!(
        std::fs::read(&taken).expect("stray file"),
        b"someone else's file",
        "the file in the way is never overwritten"
    );
    assert_eq!(
        held_file(db, "weak.flac"),
        ("held".to_owned(), "weak_match".to_owned()),
        "a file a person decides on is not retried"
    );
    assert!(held_dir.join("weak.flac").is_file());
    assert_eq!(
        retry_of(db, claimed),
        ("placing".to_owned(), 0, None),
        "an entry a person is acting on is skipped"
    );
    assert!(held_dir.join("claimed.flac").is_file());

    acquire.held.tend().await;
    assert_eq!(
        retry_of(db, waiting),
        ("held".to_owned(), 1, Some(start + 5 * MINUTE)),
        "nothing is retried before its time"
    );

    clock.advance(5 * MINUTE);
    acquire.held.tend().await;
    assert_eq!(
        retry_of(db, waiting),
        ("held".to_owned(), 2, Some(start + 20 * MINUTE)),
        "the next gap is fifteen minutes"
    );

    std::fs::remove_file(&taken).expect("place freed");
    clock.advance(15 * MINUTE);
    acquire.held.tend().await;
    assert_eq!(held_file(db, "taken.flac").0, "imported");
    assert!(taken.is_file(), "the held file took its place");
    assert!(!held_dir.join("taken.flac").exists());
    assert_eq!(
        retry_of(db, waiting),
        ("held".to_owned(), 0, None),
        "with nothing left to retry the entry waits for its person"
    );
    assert_eq!(
        held_file(db, "weak.flac"),
        ("held".to_owned(), "weak_match".to_owned())
    );
}

/// A download a curator matched to another album, then held because its
/// place there was taken, is retried onto that album: never back onto the
/// album it was downloaded for, though the same recording is on both.
#[tokio::test]
async fn a_matched_download_is_retried_onto_its_match() {
    let dummy = held_release();
    let live = portishead(
        LIVE_RELEASE,
        LIVE_GROUP,
        "Roseland",
        &[(MYSTERONS, "Mysterons")],
    );
    let rig = HeldRig::new(vec![dummy.clone(), live.clone()]).await;
    let dummy_dir = rig.music.join("Portishead/Dummy");

    // The library holds its copy of Dummy, so Dummy has a chosen edition.
    held_copy(
        &rig.held_dir,
        "sour.flac",
        "flac_full_02.flac",
        "Sour Times",
        2,
    );
    seed_held(
        &rig.db,
        "task:t-sour",
        "held",
        vec![("sour.flac", dummy.tracks[1].clone(), Reason::TargetOccupied)],
    )
    .await;
    rig.acquire.held.tend().await;
    assert!(dummy_dir.join("0102 Sour Times.flac").is_file());

    held_copy(
        &rig.held_dir,
        "matched.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    let taken = rig.music.join("Portishead/Roseland/0101 Mysterons.flac");
    std::fs::create_dir_all(taken.parent().expect("album folder")).expect("album folder");
    std::fs::write(&taken, b"someone else's file").expect("stray file");
    seed_held_on(
        &rig.db,
        &format!("album:{HELD_GROUP}"),
        HELD_GROUP,
        LIVE_RELEASE,
        "held",
        vec![(
            "matched.flac",
            live.tracks[0].clone(),
            Reason::TargetOccupied,
        )],
    )
    .await;

    rig.acquire.held.tend().await;
    assert!(
        !dummy_dir.join("0101 Mysterons.flac").exists(),
        "never placed on the album the curator matched it away from"
    );
    assert_eq!(
        held_file(&rig.db, "matched.flac"),
        ("held".to_owned(), "target_occupied".to_owned())
    );

    std::fs::remove_file(&taken).expect("place freed");
    rig.clock.advance(5 * MINUTE);
    rig.acquire.held.tend().await;
    assert_eq!(held_file(&rig.db, "matched.flac").0, "imported");
    assert!(taken.is_file(), "placed on the album it was matched to");
    assert!(!dummy_dir.join("0101 Mysterons.flac").exists());
}

/// A file held because its place was taken, whose track the library has
/// meanwhile got on another copy of the album (it holds two copies, neither
/// of the file's edition), is never placed beside it by the retry: it waits
/// for a person, its held copy kept, and is not retried again.
#[tokio::test]
async fn a_retry_never_adds_a_second_copy_by_itself() {
    let dummy = held_release();
    let deluxe = portishead(
        DELUXE_RELEASE,
        HELD_GROUP,
        "Dummy Deluxe",
        &[(MYSTERONS, "Mysterons")],
    );
    let remaster = portishead(
        REMASTER_RELEASE,
        HELD_GROUP,
        "Dummy Remaster",
        &[(SOUR_TIMES, "Sour Times")],
    );
    let rig = HeldRig::new(vec![dummy.clone(), deluxe.clone(), remaster.clone()]).await;
    let taken = rig.music.join("Portishead/Dummy/0101 Mysterons.flac");
    std::fs::create_dir_all(taken.parent().expect("album folder")).expect("album folder");
    std::fs::write(&taken, b"someone else's file").expect("stray file");
    held_copy(
        &rig.held_dir,
        "taken.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    let entry = seed_held_on(
        &rig.db,
        "task:t-taken",
        "",
        HELD_RELEASE,
        "held",
        vec![(
            "taken.flac",
            dummy.tracks[0].clone(),
            Reason::TargetOccupied,
        )],
    )
    .await;
    rig.acquire.held.tend().await;
    assert_eq!(retry_of(&rig.db, entry).1, 1);

    // The library gets the track on its Deluxe copy, and another track on
    // a Remaster copy.
    held_copy(
        &rig.held_dir,
        "deluxe.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    held_copy(
        &rig.held_dir,
        "remaster.flac",
        "flac_full_02.flac",
        "Sour Times",
        1,
    );
    for (key, release, file, track) in [
        (
            "task:t-deluxe",
            DELUXE_RELEASE,
            "deluxe.flac",
            &deluxe.tracks[0],
        ),
        (
            "task:t-remaster",
            REMASTER_RELEASE,
            "remaster.flac",
            &remaster.tracks[0],
        ),
    ] {
        seed_held_on(
            &rig.db,
            key,
            "",
            release,
            "held",
            vec![(file, track.clone(), Reason::TargetOccupied)],
        )
        .await;
    }
    rig.acquire.held.tend().await;
    assert!(
        rig.music
            .join("Portishead/Dummy Deluxe/0101 Mysterons.flac")
            .is_file()
    );
    assert!(
        rig.music
            .join("Portishead/Dummy Remaster/0101 Sour Times.flac")
            .is_file()
    );

    std::fs::remove_file(&taken).expect("place freed");
    rig.clock.advance(5 * MINUTE);
    rig.acquire.held.tend().await;
    assert!(!taken.exists(), "no second copy");
    assert_eq!(
        held_file(&rig.db, "taken.flac"),
        ("held".to_owned(), "owned_elsewhere".to_owned()),
        "a person decides"
    );
    assert!(
        rig.held_dir.join("taken.flac").is_file(),
        "its copy is kept"
    );
    assert_eq!(
        retry_of(&rig.db, entry),
        ("held".to_owned(), 0, None),
        "no further retry"
    );
}

/// A file held because its place was taken, whose track has since got a
/// lower-quality copy on the same album, is never swapped in by the retry:
/// the library's copy stays, nothing goes to the recycle bin, and the file
/// waits for a person.
#[tokio::test]
async fn a_retry_never_replaces_a_library_copy_by_itself() {
    let dummy = held_release();
    let rig = HeldRig::new(vec![dummy.clone()]).await;
    let album = rig.music.join("Portishead/Dummy");
    held_copy(&rig.held_dir, "low.mp3", "mp3_full_01.mp3", "Mysterons", 1);
    seed_held(
        &rig.db,
        "task:t-low",
        "held",
        vec![("low.mp3", dummy.tracks[0].clone(), Reason::TargetOccupied)],
    )
    .await;
    rig.acquire.held.tend().await;
    let yours = album.join("0101 Mysterons.mp3");
    assert!(yours.is_file(), "the album has its lower-quality copy");

    held_copy(
        &rig.held_dir,
        "better.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    let entry = seed_held(
        &rig.db,
        "task:t-better",
        "held",
        vec![(
            "better.flac",
            dummy.tracks[0].clone(),
            Reason::TargetOccupied,
        )],
    )
    .await;
    rig.acquire.held.tend().await;
    assert!(yours.is_file(), "the library's copy stays");
    assert!(!album.join("0101 Mysterons.flac").exists());
    assert!(
        std::fs::read_dir(rig.music.join(".recycle"))
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "nothing went to the recycle bin"
    );
    assert_eq!(
        held_file(&rig.db, "better.flac"),
        ("held".to_owned(), "owned_on_album".to_owned()),
        "a person decides"
    );
    assert!(rig.held_dir.join("better.flac").is_file());
    assert_eq!(
        retry_of(&rig.db, entry),
        ("held".to_owned(), 0, None),
        "no further retry"
    );
}

/// A better copy held because the album it would replace a file on has a
/// match to check is never put in by the held list's retry, before or
/// after the album is checked: only a person's import replaces the old
/// copy, which then goes to the recycle bin.
#[tokio::test]
async fn a_better_copy_on_a_flagged_album_waits_for_a_persons_import() {
    let dummy = held_release();
    let rig = HeldRig::new(vec![dummy.clone()]).await;
    let album = rig.music.join("Portishead/Dummy");
    held_copy(&rig.held_dir, "low.mp3", "mp3_full_01.mp3", "Mysterons", 1);
    seed_held(
        &rig.db,
        "task:t-low",
        "held",
        vec![("low.mp3", dummy.tracks[0].clone(), Reason::TargetOccupied)],
    )
    .await;
    rig.acquire.held.tend().await;
    let yours = album.join("0101 Mysterons.mp3");
    assert!(yours.is_file(), "the album has its lower-quality copy");
    let flag = |sql: &'static str| {
        let db = rig.db.clone();
        async move {
            db.write("test.flag", move |tx| {
                tx.execute(sql, [])?;
                Ok(())
            })
            .await
            .expect("flag write");
        }
    };
    flag(
        "INSERT INTO library_album_match_state (local_album_id, state, reason_code, updated_at) \
         SELECT id, 'unconfirmed', 'WEAK_MATCH', 1 FROM local_albums WHERE title = 'Dummy'",
    )
    .await;

    held_copy(
        &rig.held_dir,
        "better.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    let entry = seed_held(
        &rig.db,
        "task:t-better",
        "held",
        vec![(
            "better.flac",
            dummy.tracks[0].clone(),
            Reason::AlbumNeedsCheck,
        )],
    )
    .await;
    rig.acquire.held.tend().await;
    assert!(
        yours.is_file(),
        "the library's copy stays while the flag does"
    );
    assert_eq!(
        held_file(&rig.db, "better.flac"),
        ("held".to_owned(), "album_needs_check".to_owned()),
    );

    // Checking the album is not a decision to replace: the copy still waits.
    flag("DELETE FROM library_album_match_state").await;
    rig.clock.advance(60 * MINUTE);
    rig.acquire.held.tend().await;
    assert!(yours.is_file(), "nothing replaces your copy by itself");
    assert_eq!(
        held_file(&rig.db, "better.flac"),
        ("held".to_owned(), "album_needs_check".to_owned()),
    );

    // A person's import replaces it, the better copy being better.
    let viewer = droppedneedle_acquire::downloads::queue_rows::Viewer {
        user_id: "u-ada".to_owned(),
        admin: false,
    };
    rig.acquire
        .held
        .import(&viewer, entry, None, "u-ada")
        .await
        .expect("the import runs");
    assert!(
        album.join("0101 Mysterons.flac").is_file(),
        "the better copy went in"
    );
    assert!(!yours.exists(), "yours went to the recycle bin");
    assert_eq!(held_file(&rig.db, "better.flac").0, "imported");
}

/// A pass that finds the database busy spends no try: the entry waits for
/// the next pass as it was, never left claimed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_database_spends_no_retry() {
    use droppedneedle_kernel::db::{DbConfig, DbError, Lane, LaneLimits, open_runtime};

    let dir = scratch_dir("held-retry-busy");
    let path = dir.join("app.db");
    crate::common::migrated_db(&path);
    let runtime = open_runtime(
        &DbConfig::new(
            &path,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        )
        .with_lane_limits(LaneLimits {
            foreground_wait: std::time::Duration::from_millis(200),
            watch_tick: std::time::Duration::from_millis(10),
            ..LaneLimits::default()
        }),
    )
    .await
    .expect("runtime opens");
    let db = AcquireDb::new(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
        .build()
        .await;
    let (library, _) = crate::common::library::library_at(&platform);
    let acquire = crate::common::bundles::acquire_at(
        &platform,
        crate::common::auth::AuthRig::on(&platform).deps,
        &library,
        &crate::common::bundles::collections_at(&platform).stores,
    );
    let track = held_release().tracks[0].clone();
    let entry = seed_held(
        &db,
        "task:t-busy",
        "held",
        vec![("taken.flac", track, Reason::TargetOccupied)],
    )
    .await;

    let (entered, inside) = std::sync::mpsc::channel();
    let (let_go, release) = std::sync::mpsc::channel::<()>();
    let writer = {
        let db = runtime.db().clone();
        std::thread::spawn(move || {
            let _ = db.write_blocking(Lane::Foreground, "test.hold", |_| {
                let _ = entered.send(());
                let _ = release.recv();
                Ok::<_, DbError>(())
            });
        })
    };
    inside
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the writer is held");
    acquire.held.tend().await;
    drop(let_go);
    writer.join().expect("writer joins");
    assert_eq!(
        retry_of(&db, entry),
        ("held".to_owned(), 0, None),
        "a busy database spends no try"
    );

    drop((acquire, library, platform));
    runtime.shutdown().await;
}
