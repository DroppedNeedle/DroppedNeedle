//! The jobs table's crash promises, through the production builder. A job
//! enqueued twice under one key is one row, an enqueue that rolls back
//! leaves nothing, a lease that runs out is recovered and run, a heartbeat
//! keeps a job, a stale worker's finish and its write roll back, retries
//! back off and then fail for good, a put-back folds into a newer request,
//! a key never runs twice at once, and a restart runs what was queued and
//! requeues what was held.
//!
//! Direct cases use a kind with no runner, so nothing races them; runner
//! cases use the scripted kind the builder registers under test hooks.
//! Time moves on the jobs table's manual clock.

use std::time::Duration;

use serde_json::{Value, json};

use droppedneedle::bootstrap::jobs::scripted::{RETRY_CODE, SCRIPTED, ScriptedJobs};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_kernel::jobs::queue::{
    Claimed, Enqueued, JobError, JobKind, JobQueue, JobStatus, KindSpec, State,
};

use crate::common::TestApp;

/// A kind nothing runs.
const IDLE: JobKind = JobKind::new("test.idle");

async fn start(tag: &str) -> TestApp {
    TestApp::start_configured(tag, |config| config.with_test_hooks()).await
}

fn parts(app: &TestApp) -> (JobQueue, Db, ScriptedJobs) {
    let state = app.state();
    (
        state
            .jobs
            .queue()
            .expect("the app has a jobs table")
            .clone(),
        state.library.db.clone(),
        state.jobs.scripted().expect("test hooks are on").clone(),
    )
}

async fn enqueue(db: &Db, queue: &JobQueue, kind: JobKind, key: &str, payload: Value) -> Enqueued {
    let (queue, key) = (queue.clone(), key.to_owned());
    db.write(Lane::Background, "test-enqueue", move |tx| {
        queue.enqueue(tx, kind, &key, &payload)
    })
    .await
    .unwrap()
}

/// Enqueue and claim in one transaction: a worker that dies holding it.
async fn claim_dead(
    db: &Db,
    queue: &JobQueue,
    spec: KindSpec,
    key: &str,
    payload: Value,
) -> Claimed {
    let (queue, key) = (queue.clone(), key.to_owned());
    db.write(Lane::Background, "test-claim", move |tx| {
        queue.enqueue(tx, spec.kind(), &key, &payload)?;
        queue.claim_in(tx, &spec)
    })
    .await
    .unwrap()
    .expect("the job was claimed")
}

async fn claim(db: &Db, queue: &JobQueue, spec: KindSpec) -> Option<Claimed> {
    let queue = queue.clone();
    db.write(Lane::Background, "test-claim", move |tx| {
        queue.claim_in(tx, &spec)
    })
    .await
    .unwrap()
}

async fn row(
    app: &TestApp,
    queue: &JobQueue,
    kind: JobKind,
    key: &str,
    what: &str,
    ok: impl Fn(&JobStatus) -> bool,
) -> JobStatus {
    app.until(what, || async {
        queue.latest(kind, key).await.unwrap().filter(|row| ok(row))
    })
    .await
}

#[tokio::test]
async fn two_enqueues_of_one_key_make_one_row_and_the_admin_view_counts_it() {
    let app = start("jobs-dedupe").await;
    let (queue, db, _) = parts(&app);

    let first = enqueue(&db, &queue, IDLE, "k", json!({"id": 1})).await;
    let second = enqueue(&db, &queue, IDLE, "k", json!({"id": 1})).await;

    assert!(!first.joined);
    assert_eq!(
        second,
        Enqueued {
            id: first.id,
            joined: true
        }
    );
    let admin = app.login_admin().await;
    let (_, stats) = app
        .call(
            "GET",
            "/api/v3/admin/queue-stats",
            &[("authorization", admin.as_str())],
            None,
        )
        .await;
    assert!(
        stats["queues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["kind"] == "test.idle" && row["queued"] == 1),
        "{stats}"
    );
    app.stop().await;
}

#[tokio::test]
async fn an_enqueue_in_a_rolled_back_transaction_leaves_nothing() {
    let app = start("jobs-rollback").await;
    let (queue, db, _) = parts(&app);

    let writer = queue.clone();
    let rolled = db
        .write(Lane::Background, "test-rollback", move |tx| {
            writer.enqueue(tx, IDLE, "gone", &json!({}))?;
            Err::<(), _>(JobError::LeaseLost)
        })
        .await;

    assert!(matches!(rolled, Err(JobError::LeaseLost)));
    assert_eq!(queue.latest(IDLE, "gone").await.unwrap(), None);
    app.stop().await;
}

#[tokio::test]
async fn a_lease_that_runs_out_is_recovered_and_the_job_runs_once() {
    let app = start("jobs-expiry").await;
    let (queue, db, kit) = parts(&app);
    // The runner is past its start recovery once a probe has run.
    enqueue(&db, &queue, SCRIPTED, "probe", json!({"answer": "done"})).await;
    row(&app, &queue, SCRIPTED, "probe", "the probe ran", |row| {
        row.state == State::Done
    })
    .await;

    claim_dead(
        &db,
        &queue,
        ScriptedJobs::spec(),
        "expire",
        json!({"answer": "done", "gate": true}),
    )
    .await;
    kit.clock().set(kit.clock().now_ms() + 61_000);
    queue.wake();

    let running = row(
        &app,
        &queue,
        SCRIPTED,
        "expire",
        "the expired job running again",
        |row| row.state == State::Running && row.attempts == 2,
    )
    .await;
    assert_eq!(running.last_error.as_deref(), Some("LEASE_EXPIRED"));
    kit.open_gate();
    row(&app, &queue, SCRIPTED, "expire", "the job done", |row| {
        row.state == State::Done
    })
    .await;
    assert_eq!(kit.runs("expire"), 1);
    app.stop().await;
}

#[tokio::test]
async fn a_heartbeat_keeps_a_long_job_from_being_reclaimed() {
    let app = start("jobs-heartbeat").await;
    let (queue, db, kit) = parts(&app);
    let spec = KindSpec::new(IDLE);
    let start = kit.clock().now_ms();
    let held = claim_dead(&db, &queue, spec, "long", json!({})).await;

    kit.clock().set(start + 50_000);
    queue.heartbeat(&held.lease, &spec).await.unwrap();
    kit.clock().set(start + 70_000);
    assert_eq!(queue.recover_expired(&spec).await.unwrap(), 0);
    kit.clock().set(start + 111_000);
    assert_eq!(queue.recover_expired(&spec).await.unwrap(), 1);
    app.stop().await;
}

#[tokio::test]
async fn a_stale_owners_complete_changes_nothing_and_rolls_back_its_write() {
    let app = start("jobs-stale").await;
    let (queue, db, kit) = parts(&app);
    let spec = KindSpec::new(IDLE);
    let stale = claim_dead(&db, &queue, spec, "stale", json!({})).await;
    kit.clock().set(kit.clock().now_ms() + 61_000);
    assert_eq!(queue.recover_expired(&spec).await.unwrap(), 1);
    let current = claim(&db, &queue, spec).await.expect("reclaimed");

    let writer = queue.clone();
    let lease = stale.lease.clone();
    let late = db
        .write(Lane::Background, "test-stale-complete", move |tx| {
            writer.enqueue(tx, IDLE, "marker", &json!({}))?;
            writer.complete(tx, &lease)
        })
        .await;

    assert!(matches!(late, Err(JobError::LeaseLost)), "{late:?}");
    assert_eq!(queue.latest(IDLE, "marker").await.unwrap(), None);
    let held = queue.latest(IDLE, "stale").await.unwrap().unwrap();
    assert_eq!((held.state, held.attempts), (State::Running, 2));
    let writer = queue.clone();
    db.write(Lane::Background, "test-complete", move |tx| {
        writer.complete(tx, &current.lease)
    })
    .await
    .unwrap();
    app.stop().await;
}

#[tokio::test]
async fn retries_back_off_then_fail_for_good_at_the_limit() {
    let app = start("jobs-retry").await;
    let (queue, db, kit) = parts(&app);
    let now = kit.clock().now_ms();

    enqueue(
        &db,
        &queue,
        SCRIPTED,
        "at",
        json!({"answer": "retry", "at": now + 5_000}),
    )
    .await;
    let asked = row(&app, &queue, SCRIPTED, "at", "the retry at a time", |row| {
        row.state == State::Queued && row.attempts == 1
    })
    .await;
    assert_eq!(asked.not_before_at, now + 5_000);

    enqueue(&db, &queue, SCRIPTED, "retry", json!({"answer": "retry"})).await;
    for (attempt, first) in [(1_u32, 30_000_i64), (2, 60_000)] {
        let at = kit.clock().now_ms();
        let waiting = row(
            &app,
            &queue,
            SCRIPTED,
            "retry",
            "a backed-off retry",
            |row| row.state == State::Queued && row.attempts == attempt,
        )
        .await;
        let delay = waiting.not_before_at - at;
        assert!(
            (first..=first + first / 4).contains(&delay),
            "attempt {attempt}: {delay}"
        );
        assert_eq!(waiting.last_error.as_deref(), Some(RETRY_CODE));
        kit.clock().set(waiting.not_before_at);
        queue.wake();
    }
    let failed = row(
        &app,
        &queue,
        SCRIPTED,
        "retry",
        "the job failed for good",
        |row| row.state == State::Failed,
    )
    .await;
    assert_eq!(
        (failed.attempts, failed.last_error.as_deref()),
        (3, Some(RETRY_CODE))
    );
    app.stop().await;
}

#[tokio::test]
async fn a_retry_folds_into_a_request_made_while_it_ran() {
    let app = start("jobs-fold").await;
    let (queue, db, kit) = parts(&app);
    let at = kit.clock().now_ms() + 3_600_000;
    let payload = json!({"answer": "retry", "gate": true, "at": at});
    let first = enqueue(&db, &queue, SCRIPTED, "fold", payload.clone()).await;
    row(&app, &queue, SCRIPTED, "fold", "the job running", |row| {
        row.state == State::Running
    })
    .await;

    let second = enqueue(&db, &queue, SCRIPTED, "fold", payload).await;
    assert!(!second.joined);
    kit.open_gate();

    app.until("the running row folded away", || async {
        queue.get(first.id).await.unwrap().is_none().then_some(())
    })
    .await;
    let folded = queue.get(second.id).await.unwrap().unwrap();
    assert_eq!(
        (
            folded.state,
            folded.attempts,
            folded.not_before_at,
            folded.last_error.as_deref()
        ),
        (State::Queued, 1, at, Some(RETRY_CODE))
    );
    app.stop().await;
}

#[tokio::test]
async fn claim_skips_a_key_that_has_a_running_row() {
    let app = start("jobs-one-at-a-time").await;
    let (queue, db, _) = parts(&app);
    let spec = KindSpec::new(IDLE);
    claim_dead(&db, &queue, spec, "busy-key", json!({})).await;

    let again = enqueue(&db, &queue, IDLE, "busy-key", json!({})).await;

    assert!(!again.joined);
    assert!(claim(&db, &queue, spec).await.is_none());
    app.stop().await;
}

#[tokio::test]
async fn a_restart_runs_queued_jobs_and_requeues_held_ones_at_start() {
    let app = start("jobs-restart").await;
    let (queue, db, kit) = parts(&app);
    // Past this start's recovery, so only the next start can requeue it.
    enqueue(&db, &queue, SCRIPTED, "probe", json!({"answer": "done"})).await;
    row(&app, &queue, SCRIPTED, "probe", "the probe ran", |row| {
        row.state == State::Done
    })
    .await;
    let due = kit.clock().now_ms() + 3_600_000;
    {
        let (queue, due) = (queue.clone(), due);
        db.write(Lane::Background, "test-enqueue-later", move |tx| {
            queue.enqueue_at(tx, SCRIPTED, "later", &json!({"answer": "done"}), due)
        })
        .await
        .unwrap();
    }
    let lease = Duration::from_secs(600);
    let claimed_at = kit.clock().now_ms();
    claim_dead(
        &db,
        &queue,
        ScriptedJobs::spec().lease(lease),
        "held",
        json!({"answer": "done"}),
    )
    .await;

    let app = app.restart().await;
    let (queue, _, kit) = parts(&app);

    row(
        &app,
        &queue,
        SCRIPTED,
        "held",
        "the held job run after the restart",
        |row| row.state == State::Done,
    )
    .await;
    assert_eq!(kit.runs("held"), 1);
    assert!(kit.clock().now_ms() < claimed_at + 600_000);
    assert_eq!(kit.runs("later"), 0);
    kit.clock().set(due);
    queue.wake();
    row(
        &app,
        &queue,
        SCRIPTED,
        "later",
        "the queued job run",
        |row| row.state == State::Done,
    )
    .await;
    assert_eq!(kit.runs("later"), 1);
    app.stop().await;
}
