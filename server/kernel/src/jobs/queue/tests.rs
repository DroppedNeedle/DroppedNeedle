//! The jobs table keeps the lease promises its runner and other leased
//! tables rely on, and a busy put-back never spends an attempt or fails a
//! job.

use std::sync::Arc;

use rusqlite::named_params;

use super::{JobKind, JobQueue, KindSpec, Outcome, State, lease};
use crate::clock::ManualClock;
use crate::db::scratch::ScratchDir;
use crate::db::{DbConfig, DbRuntime, Lane, open_runtime};
use crate::schema::template::copy_to;

const KIND: JobKind = JobKind::new("test.unit");

async fn open(name: &str) -> (DbRuntime, ScratchDir) {
    let dir = ScratchDir::new(&format!("jobs-{name}")).unwrap();
    let path = dir.join("library.db");
    copy_to(&path).unwrap();
    (open_runtime(&DbConfig::new(&path)).await.unwrap(), dir)
}

#[tokio::test]
async fn the_jobs_table_statements_keep_the_lease_promises() {
    let (runtime, _dir) = open("conformance").await;
    lease::check_conformance(runtime.db(), super::STATEMENTS, "test.unit", |tx| {
        tx.query_row(
            "INSERT INTO durable_jobs (kind, idempotency_key, state, not_before_at, payload_json, created_at, updated_at) \
             VALUES (:kind, 'k', 'queued', 0, '{}', 0, 0) RETURNING id",
            named_params! { ":kind": "test.unit" },
            |row| row.get(0),
        )
    })
    .await
    .unwrap();
    runtime.shutdown().await;
}

#[tokio::test]
async fn a_busy_answer_returns_the_attempt_and_never_fails_the_job() {
    let (runtime, _dir) = open("busy").await;
    let queue = JobQueue::new(runtime.db().clone(), Arc::new(ManualClock::at(1_000_000)));
    let spec = KindSpec::new(KIND).max_attempts(1);
    let claimed = {
        let (queue, spec) = (queue.clone(), spec);
        runtime
            .db()
            .write(Lane::Background, "test-claim", move |tx| {
                queue.enqueue(tx, KIND, "k", &())?;
                queue.claim_in(tx, &spec)
            })
            .await
            .unwrap()
            .unwrap()
    };
    assert_eq!(claimed.attempt, 1);

    queue
        .finish(&claimed.lease, &spec, Outcome::Busy, 0)
        .await
        .unwrap();

    let row = queue.get(claimed.lease.id()).await.unwrap().unwrap();
    assert_eq!(
        (row.state, row.attempts, row.last_error.as_deref()),
        (State::Queued, 0, Some("BUSY"))
    );
    assert_eq!(row.not_before_at, 1_002_000);
    runtime.shutdown().await;
}
