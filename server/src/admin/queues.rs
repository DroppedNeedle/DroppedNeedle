//! Queue and provider stats.
//!
//! Queue stats port v2's lane-occupancy gauge: the loop liveness rows,
//! read through [`droppedneedle_kernel::db::list_loops`], the depth of each
//! job kind on the jobs table, and the latest checkpoint pass.
//!
//! Provider stats show each rate row's limiter (its table row plus the
//! live bucket) and the outbound calls it has sent since start.

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use super::{
    AdminDb,
    error::AdminError,
    models::{
        CheckpointView, JobView, ProviderCallsView, ProviderLimiterView, ProviderStatsResponse,
        QueueDepthView, QueueStatsResponse,
    },
};
use droppedneedle_kernel::db::{CheckpointOutcome, CheckpointService, list_loops};
use droppedneedle_providers::Providers;

/// Loop rows, job queue depths and the latest checkpoint pass.
pub async fn queue_stats(
    db: &AdminDb,
    checkpoint: Option<&CheckpointService>,
) -> Result<QueueStatsResponse, AdminError> {
    let jobs = list_loops(&db.db)
        .await
        .map_err(|error| AdminError::internal(&format_args!("loop rows read failed: {error}")))?;
    let queues = droppedneedle_kernel::jobs::queue::counts(&db.db)
        .await
        .map_err(|error| AdminError::internal(&format_args!("job counts read failed: {error}")))?;
    Ok(QueueStatsResponse {
        jobs: jobs
            .into_iter()
            .map(|job| JobView {
                name: job.name,
                state: job.state,
                last_heartbeat_at: job.last_heartbeat_at,
                last_error: job.last_error,
            })
            .collect(),
        queues: queues
            .into_iter()
            .map(|counts| QueueDepthView {
                kind: counts.kind,
                queued: counts.queued,
                running: counts.running,
                failed: counts.failed,
            })
            .collect(),
        checkpoint: checkpoint
            .and_then(CheckpointService::try_latest)
            .map(checkpoint_view),
    })
}

/// The pass as admins see it. The failure text can carry a path, so it
/// stays in the log and the view gets a fixed code.
fn checkpoint_view(pass: CheckpointOutcome) -> CheckpointView {
    CheckpointView {
        mode: pass.mode,
        busy: pass.busy,
        active_bytes: pass.active_bytes,
        wal_file_bytes: pass.wal_file_bytes,
        suspended: pass.suspended,
        at_unix: pass
            .at
            .duration_since(UNIX_EPOCH)
            .map(|span| span.as_secs())
            .unwrap_or(0),
        error: pass.error.map(|_| "CHECKPOINT_FAILED".to_owned()),
    }
}

/// Limiter posture per rate row plus the calls each row has sent.
pub fn provider_stats(providers: &Arc<Providers>) -> ProviderStatsResponse {
    let mut limiters = Vec::new();
    let mut calls = Vec::new();
    for source in providers.limiters.sources() {
        let Some(limiter) = providers.limiter(source) else {
            continue;
        };
        let policy = limiter.policy();
        limiters.push(ProviderLimiterView {
            source: source.to_owned(),
            per_second: policy.per_second,
            burst: policy.burst,
            remaining: limiter.remaining(),
        });
        calls.extend(limiter.calls().into_iter().map(|count| ProviderCallsView {
            source: source.to_owned(),
            priority: count.priority,
            outcome: count.outcome,
            calls: count.calls,
            calls_last_minute: count.last_minute,
            body_bytes: count.body_bytes,
        }));
    }
    limiters.sort_by(|left, right| left.source.cmp(&right.source));
    ProviderStatsResponse { limiters, calls }
}
