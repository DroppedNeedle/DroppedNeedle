use droppedneedle_kernel::db::scratch::ScratchDir;
use droppedneedle_kernel::db::{DbConfig, open_runtime};
use droppedneedle_kernel::schema::template::copy_to;

/// The job table's lease statements keep the promises the kernel's
/// lease core relies on: a stale owner changes nothing, a lease that
/// ran out is requeued for its kind, and every held job is requeued at
/// start.
#[tokio::test]
async fn the_job_table_keeps_the_lease_promises() {
    let dir = ScratchDir::new("library-jobs-lease").unwrap();
    let path = dir.join("library.db");
    copy_to(&path).unwrap();
    let runtime = open_runtime(&DbConfig::new(
        &path,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    ))
    .await
    .unwrap();
    let kind = super::OperationKind::ExplicitReidentification.as_str();
    super::lease::check_conformance(runtime.db(), super::STATEMENTS, kind, move |tx| {
        tx.execute(
            "INSERT INTO library_operation_jobs (id, kind, state, created_at, updated_at) \
             VALUES ('job', ?1, 'queued', 0, 0)",
            [kind],
        )?;
        tx.query_row(
            "SELECT rowid FROM library_operation_jobs WHERE id = 'job'",
            [],
            |row| row.get(0),
        )
    })
    .await
    .unwrap();
    runtime.shutdown().await;
}
