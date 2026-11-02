//! The operations service's re-identification calls.

use crate::clock::LibraryClock as _;
use crate::identify::queue::queue_retry;
use crate::jobs::OperationJob;
use crate::jobs::models::OperationError;
use crate::jobs::service::Operations;
use crate::jobs::{self, OperationKind};
use crate::reidentify::store;
use crate::reidentify::store::NewReidentification;
use droppedneedle_kernel::db::Lane;

impl Operations {
    /// A re-identification of one album.
    pub(crate) fn plain_reidentification(
        &self,
        album_id: &str,
        user_id: &str,
        idempotency_key: Option<String>,
        one_off_local_metadata: bool,
    ) -> NewReidentification {
        NewReidentification {
            job_id: self.ids.new_id(),
            requested_by_user_id: user_id.to_owned(),
            idempotency_key,
            local_album_id: album_id.to_owned(),
            one_off_local_metadata,
        }
    }

    /// Requeue every re-identification and review job an earlier process
    /// left running. The loop's first turn calls this. Blocking.
    pub fn recover_at_start(&self) -> Result<u64, OperationError> {
        self.db
            .write_blocking(Lane::Background, "operations.recover_at_start", |tx| {
                let now = self.clock.now_secs();
                let mut requeued = 0;
                for kind in [
                    OperationKind::ExplicitReidentification,
                    OperationKind::BulkReviewApply,
                ] {
                    requeued += jobs::recover_at_start(tx, kind, now)?;
                }
                Ok::<_, OperationError>(requeued)
            })
    }

    /// Requeue the re-identifications whose worker vanished, then claim
    /// the next due one and hand its album to the identification queue,
    /// all in one write. Returns the job as it was left, or `None` when
    /// nothing was due. Blocking.
    pub fn run_next(&self, worker: &str) -> Result<Option<OperationJob>, OperationError> {
        self.db
            .write_blocking(Lane::Background, "operations.run_next", |tx| {
                let now = self.clock.now_secs();
                let claimed =
                    jobs::claim(tx, OperationKind::ExplicitReidentification, worker, now)?;
                let Some(job) = claimed else {
                    return Ok(None);
                };
                if jobs::claim_work(tx, &job.id, now)?.is_none() {
                    return Ok(store::fail(tx, &job.id, worker, "MISSING_WORK", now)?);
                }
                // A pause or stop asked for while the job was queued wins.
                if let Some(halted) = jobs::checkpoint(tx, &job.id, worker, now)? {
                    return Ok(Some(halted));
                }
                let Some(snap) = store::snapshot(tx, &job.id)? else {
                    return Ok(store::fail(tx, &job.id, worker, "MISSING_SNAPSHOT", now)?);
                };
                let gate = store::retry_gate(
                    tx,
                    &self.management,
                    &snap.local_album_id,
                    snap.one_off_local_metadata,
                );
                match gate {
                    Ok(_) => {}
                    Err(
                        OperationError::NotFound(reason)
                        | OperationError::Invalid(reason)
                        | OperationError::Conflict(reason),
                    ) => return Ok(store::fail(tx, &job.id, worker, reason.code, now)?),
                    Err(other) => return Err(other),
                }
                queue_retry(
                    tx,
                    &snap.local_album_id,
                    job.requested_by_user_id.as_deref(),
                    self.clock.now_ms(),
                )?;
                Ok(store::hand_over(tx, &job.id, worker, now)?)
            })
    }
}
