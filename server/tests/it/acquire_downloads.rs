//! Durability briefs for the downloads slice.
//!
//! Each brief pins one crash-safety behavior against a scratch database
//! and throwaway directories: restarts resume without a duplicate fetch,
//! manifest-absent restarts come up clean, failover hands work over only
//! past the lease, and orphan/quarantine sweeps fail closed. Nothing here
//! touches the real library.

use droppedneedle::acquire::downloads;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use downloads::quarantine::{QuarantineReason, is_local_fault};
use downloads::recovery::{
    FAILOVER_CLAIM_LIMIT, FAILOVER_LEASE_SECONDS, StartupAction, StartupCtx,
};
use downloads::state::{AttemptState, can_transition};
use downloads::store::{NewTask, apply_test_schema as apply_schema_again};
use downloads::{
    DownloadManifest, DownloadStore, ExpectedFile, ManifestCodec, OrphanDecision, OrphanEvidence,
    OrphanPolicy, PollSample, QuarantineDir, RecycleBin, RetryPolicy, TaskStatus, Watchdog,
    WatchdogConfig, WatchdogOutcome, apply_test_schema, canonical_soulseek_identity,
    classify_startup, is_terminal, plan_retry,
};
use rusqlite::Connection;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "dn-acquire-downloads-{}-{}-{id}",
        std::process::id(),
        tag
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Scratch journal: a temp-file database with the real migrations applied.
fn scratch_store(dir: &std::path::Path) -> Connection {
    let conn = Connection::open(dir.join("brief.db")).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    apply_test_schema(&conn).unwrap();
    // The harness path and the re-exported path run the same SQL; both
    // stay idempotent so a double apply never corrupts a scratch db.
    apply_schema_again(&conn).unwrap();
    conn.execute(
        "INSERT INTO auth_users (id, display_name, role, created_at) VALUES ('u1', 'brief', 'user', 'now')",
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

fn manifest(task_id: &str) -> DownloadManifest {
    DownloadManifest {
        task_id: task_id.to_string(),
        release_group_mbid: "rg-1".to_string(),
        artist_name: "artist".to_string(),
        album_title: "album".to_string(),
        naming_template: "{artist}/{album}".to_string(),
        target_files: vec![ExpectedFile {
            filename: "01.flac".to_string(),
            size: 10,
            duration: Some(180.0),
        }],
        source_username: Some("peer".to_string()),
        handle: None,
        expected_tracks: vec![],
        release_mbid: None,
        artist_mbid: None,
        year: None,
        is_track: false,
        hold_on_wrong_track: false,
        origin: "user".to_string(),
        requested_by_user_id: None,
        attempt_id: Some("a1".to_string()),
    }
}

#[test]
fn restart_mid_download_resumes_without_duplicate_fetch() {
    let dir = scratch_dir("resume");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;

    store.insert_task(&new_task("t1"), now).unwrap();
    store
        .transition_task("t1", TaskStatus::Downloading, now, None)
        .unwrap();
    // First enqueue claims the key; the crash happens after the fetch.
    assert!(
        store
            .claim_key("enqueue:t1:0", "t1", "enqueue", now)
            .unwrap()
    );
    ManifestCodec
        .write(&dir.join("staging"), &manifest("t1"))
        .unwrap();
    store
        .insert_attempt(
            "a1",
            "t1",
            "soulseek",
            0,
            "",
            "{}",
            AttemptState::InUse,
            now,
        )
        .unwrap();

    // Simulate the restart: a fresh store over the same database.
    let store = DownloadStore::new(&conn);
    assert!(
        !store
            .claim_key("enqueue:t1:0", "t1", "enqueue", now + 1.0)
            .unwrap()
    );

    let task = store.get_task("t1").unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Downloading);
    let action = classify_startup(StartupCtx {
        status: task.status,
        manifest_present: ManifestCodec::path(&dir.join("staging"), "t1").exists(),
        manifest_matches_attempt: Some(true),
    });
    assert_eq!(action, StartupAction::ResumePoll);
}

#[test]
fn manifest_absent_restart_starts_clean() {
    let dir = scratch_dir("clean");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;

    // The crash landed before the manifest was ever written.
    store.insert_task(&new_task("t1"), now).unwrap();
    store
        .transition_task("t1", TaskStatus::Downloading, now, None)
        .unwrap();

    let task = store.get_task("t1").unwrap().unwrap();
    let action = classify_startup(StartupCtx {
        status: task.status,
        manifest_present: ManifestCodec::path(&dir.join("staging"), "t1").exists(),
        manifest_matches_attempt: None,
    });
    assert_eq!(action, StartupAction::RestartClean);

    // A queued task that never started re-dispatches instead of failing.
    store.insert_task(&new_task("t2"), now).unwrap();
    let queued = store.get_task("t2").unwrap().unwrap();
    let action = classify_startup(StartupCtx {
        status: queued.status,
        manifest_present: false,
        manifest_matches_attempt: None,
    });
    assert_eq!(action, StartupAction::Redispatch);
}

#[test]
fn candidate_mismatch_restart_starts_clean() {
    let dir = scratch_dir("mismatch");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;

    // The prior candidate was made cleanup-eligible, then the process died
    // before the next enqueue replaced its manifest.
    store.insert_task(&new_task("t1"), now).unwrap();
    store
        .transition_task("t1", TaskStatus::Processing, now, None)
        .unwrap();
    ManifestCodec
        .write(&dir.join("staging"), &manifest("t1"))
        .unwrap();

    let task = store.get_task("t1").unwrap().unwrap();
    let action = classify_startup(StartupCtx {
        status: task.status,
        manifest_present: true,
        manifest_matches_attempt: Some(false),
    });
    assert_eq!(action, StartupAction::RestartClean);
}

#[test]
fn orchestrator_failover_claims_only_past_the_lease() {
    let dir = scratch_dir("failover");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;

    store.insert_task(&new_task("t1"), now).unwrap();
    store
        .insert_attempt(
            "a1",
            "t1",
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
            Some("Download interrupted - no progress after a restart"),
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

    assert_eq!(policy.ladder_minutes(), vec![15, 30, 60, 120, 240, 480]);
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
            Some("peer vanished"),
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
            Some("peer vanished"),
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

    // A held track pauses auto-retry for its task.
    store
        .insert_held(
            "u1",
            "/held/01.flac",
            "verify_failed",
            Some("orig-r1"),
            Some("rg-retry"),
            now,
        )
        .unwrap();
    assert!(store.has_unresolved_held_for_task("orig-r1").unwrap());
    assert!(!store.has_unresolved_held_for_task("orig").unwrap());

    // Retry spawns keep the original terminal and carry the generation.
    let spawn = plan_retry("orig-r1", TaskStatus::Failed, "retry", 1).unwrap();
    assert_eq!(spawn.task_id, "orig-r1-r2");
    assert_eq!(spawn.origin, "retry");
    assert_eq!(spawn.retry_count, 2);
    let upgrade_spawn = plan_retry("up", TaskStatus::Failed, "upgrade", 0).unwrap();
    assert_eq!(upgrade_spawn.origin, "upgrade");
    assert!(plan_retry("live", TaskStatus::Downloading, "user", 0).is_none());
}

#[test]
fn management_held_retry_sweep_fires_only_when_due() {
    let dir = scratch_dir("held");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;

    let due = store
        .insert_held(
            "u1",
            "/held/a.flac",
            "management:rename",
            Some("t1"),
            None,
            now,
        )
        .unwrap();
    let waiting = store
        .insert_held(
            "u1",
            "/held/b.flac",
            "management:rename",
            Some("t2"),
            None,
            now,
        )
        .unwrap();
    let other = store
        .insert_held("u1", "/held/c.flac", "verify_failed", Some("t3"), None, now)
        .unwrap();
    store
        .schedule_held_retry(&[due], 1, Some(now - 5.0))
        .unwrap();
    store
        .schedule_held_retry(&[waiting], 1, Some(now + 3600.0))
        .unwrap();
    // Non-management reasons never take a scheduled retry.
    store
        .schedule_held_retry(&[other], 1, Some(now - 5.0))
        .unwrap();

    let units = store.list_due_held_units(now, 10).unwrap();
    assert_eq!(units, vec![("t1".to_string(), "u1".to_string())]);
}

#[test]
fn quarantine_blocks_expires_and_clears_on_manual_retry() {
    let dir = scratch_dir("quar");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;
    let ttl = 7.0 * 24.0 * 3600.0;

    assert!(!is_local_fault(Some("CRC mismatch in segment 4")));
    assert!(is_local_fault(Some("SABnzbd: disk is full, pausing")));

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

    // Suspect files move into the sandbox only.
    let sandbox = QuarantineDir::sandbox(dir.join("quarantine"));
    let suspect = dir.join("suspect.flac");
    std::fs::write(&suspect, b"bad bytes").unwrap();
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
fn recycle_bin_prunes_only_past_retention() {
    let root = scratch_dir("recycle");
    let lib = root.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let resolved = RecycleBin::resolve("", &[lib.to_str().unwrap().to_string()]).unwrap();
    assert_eq!(resolved, lib.join(".recycle"));

    let bin = RecycleBin::at(resolved.clone(), 30);
    std::fs::create_dir_all(&resolved).unwrap();
    let old = resolved.join("20200101T000000-aaaabbbb");
    let young = resolved.join("29990101T000000-ccccdddd");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("track.flac"), b"old").unwrap();
    std::fs::create_dir_all(&young).unwrap();
    std::fs::write(young.join("track.flac"), b"young").unwrap();

    let removed = bin.prune(SystemTime::now()).unwrap();
    assert_eq!(removed, 1);
    assert!(!old.exists());
    assert!(young.exists());

    // A missing bin prunes to zero instead of erroring.
    let missing = RecycleBin::at(root.join("nope"), 30);
    assert_eq!(missing.prune(SystemTime::now()).unwrap(), 0);
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
}

#[test]
fn terminal_states_close_the_task_but_open_a_retry() {
    for status in [
        TaskStatus::Completed,
        TaskStatus::Partial,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ] {
        assert!(is_terminal(status.as_str()));
        for next in [
            TaskStatus::Queued,
            TaskStatus::Downloading,
            TaskStatus::Processing,
            TaskStatus::Completed,
            TaskStatus::Partial,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
            assert!(!can_transition(status, next));
        }
    }
    assert!(can_transition(TaskStatus::Queued, TaskStatus::Downloading));
    assert!(!is_terminal("downloading"));

    // Quarantine reasons stay within the CHECK vocabulary.
    let reasons: Vec<&str> = [
        QuarantineReason::VerifyFailed,
        QuarantineReason::Corrupt,
        QuarantineReason::FingerprintMismatch,
        QuarantineReason::DurationMismatch,
        QuarantineReason::DownloadFailed,
        QuarantineReason::Manual,
    ]
    .iter()
    .map(|reason| reason.as_str())
    .collect();
    assert_eq!(
        reasons,
        vec![
            "verify_failed",
            "corrupt",
            "fingerprint_mismatch",
            "duration_mismatch",
            "download_failed",
            "manual"
        ]
    );
    // Watchdog defaults stay ordered: stall < queued < 6h deadline, and
    // the materialize + reap windows sit under the stall timeout.
    let config = WatchdogConfig::default();
    assert_eq!(config.deadline_seconds, 6.0 * 3600.0);
    assert!(config.stall_timeout_seconds < config.queued_timeout_seconds);
    assert!(config.queued_timeout_seconds < config.deadline_seconds);
    assert!(config.materialize_seconds < config.stall_timeout_seconds);
    assert!(config.reap_threshold_seconds <= config.stall_timeout_seconds);
}

#[test]
fn terminal_tasks_reject_outgoing_transitions() {
    let dir = scratch_dir("term-guard");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;
    store.insert_task(&new_task("t1"), now).unwrap();
    store
        .transition_task("t1", TaskStatus::Failed, now, None)
        .unwrap();
    // Terminal rows never move, not even back to queued.
    assert!(
        store
            .transition_task("t1", TaskStatus::Queued, now, None)
            .is_err()
    );
    // Finalizing into a non-terminal status is refused, not asserted.
    assert!(
        store
            .finalize_task_and_attempt("t1", TaskStatus::Downloading, now, None, None, false)
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
            classify_startup(StartupCtx {
                status,
                manifest_present: true,
                manifest_matches_attempt: Some(true),
            }),
            StartupAction::Noop
        );
    }
}

#[test]
fn idempotency_keys_answer_the_original_task() {
    let dir = scratch_dir("idem-key");
    let conn = scratch_store(&dir);
    let store = DownloadStore::new(&conn);
    let now = 1_700_000_000.0;
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
fn quarantine_hold_rejects_escaping_stamps() {
    let dir = scratch_dir("quar-stamp");
    let sandbox = QuarantineDir::sandbox(dir.join("quarantine"));
    let suspect = dir.join("suspect.flac");
    std::fs::write(&suspect, b"bad bytes").unwrap();
    assert!(sandbox.hold_file(&suspect, "../escape").is_err());
    assert!(sandbox.hold_file(&suspect, "a/b").is_err());
    assert!(sandbox.hold_file(&suspect, "..").is_err());
    assert!(suspect.exists());
    let held = sandbox.hold_file(&suspect, "20240101T000000").unwrap();
    assert!(held.starts_with(dir.join("quarantine")));
}

#[test]
fn recycle_bin_guarded_refuses_protected_roots() {
    let staging = PathBuf::from("/data/staging");
    let mount = PathBuf::from("/data/complete");
    let protected = vec![staging.clone(), mount.clone()];
    assert!(RecycleBin::guarded(PathBuf::from("/"), 7, &protected).is_none());
    assert!(RecycleBin::guarded(staging.clone(), 7, &protected).is_none());
    assert!(RecycleBin::guarded(PathBuf::from("/data"), 7, &protected).is_none());
    assert!(RecycleBin::guarded(PathBuf::from("relative/bin"), 7, &protected).is_none());
    assert!(RecycleBin::guarded(PathBuf::from("/data/recycle"), 7, &protected).is_some());
    // Belt and suspenders: a `/` root never prunes.
    let root_bin = RecycleBin::at(PathBuf::from("/"), 7);
    assert_eq!(root_bin.prune(SystemTime::now()).unwrap(), 0);
}

#[test]
fn manifest_paths_reject_escape() {
    let staging = PathBuf::from("/tmp/staging");
    assert!(ManifestCodec::checked_path(&staging, "../evil").is_none());
    assert!(ManifestCodec::checked_path(&staging, "a/b").is_none());
    assert!(ManifestCodec::checked_path(&staging, "").is_none());
    let hex = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        ManifestCodec::checked_path(&staging, hex),
        Some(staging.join(hex).join("manifest.json"))
    );
}

// Reimport requeues a linked failed task with its candidate kept, and
// refuses unlinked, live, or missing tasks (v2 reimport guard).
#[test]
fn reimport_requeues_linked_failed_tasks() {
    let dir = scratch_dir("reimport");
    let conn = scratch_store(&dir);
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let store = DownloadStore::new(&conn);
    store.insert_task(&new_task("linked"), now).unwrap();
    store.insert_task(&new_task("bare"), now).unwrap();
    store.insert_task(&new_task("live"), now).unwrap();
    store
        .link_candidate("linked", "peer", "job-1", 2, now)
        .unwrap();
    store
        .link_candidate("live", "peer", "job-1", 0, now)
        .unwrap();
    store
        .transition_task("linked", TaskStatus::Failed, now, Some("mount gone"))
        .unwrap();
    store
        .transition_task("bare", TaskStatus::Failed, now, Some("mount gone"))
        .unwrap();

    assert!(store.is_reimportable("linked").unwrap());
    assert!(!store.is_reimportable("bare").unwrap());
    assert!(!store.is_reimportable("live").unwrap());
    assert!(!store.is_reimportable("missing").unwrap());

    let row = store.reimport_task("linked", now).unwrap().unwrap();
    assert_eq!(row.status, TaskStatus::Queued);
    assert!(row.error_message.is_none());
    assert_eq!(row.candidate_index, Some(2));
    assert_eq!(row.source_username.as_deref(), Some("peer"));
    assert_eq!(row.search_job_id.as_deref(), Some("job-1"));
    // Back in line, so the guard no longer passes for it.
    assert!(!store.is_reimportable("linked").unwrap());

    assert!(store.reimport_task("bare", now).unwrap().is_none());
    assert!(store.reimport_task("live", now).unwrap().is_none());
    assert!(store.reimport_task("missing", now).unwrap().is_none());
}
