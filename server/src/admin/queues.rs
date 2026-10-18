//! Queue and provider stats.
//!
//! Queue stats port v2's lane-occupancy gauge: the job-registry rows,
//! read through [`droppedneedle_kernel::db::JobRegistryRows::list_jobs`],
//! and the latest checkpoint pass.
//!
//! Provider stats port v2's provider-counters snapshot onto the v3 pacing
//! primitives: per-provider limiter posture (verified row plus live bucket)
//! and slot-lane occupancy.

use std::sync::Arc;
use std::time::UNIX_EPOCH;

use super::{
    AdminDb,
    error::AdminError,
    models::{
        CheckpointView, JobView, ProviderLimiterView, ProviderStatsResponse, QueueStatsResponse,
        SlotView,
    },
};
use droppedneedle_kernel::db::{CheckpointMode, CheckpointOutcome, CheckpointService};
use droppedneedle_providers::Providers;

/// Registry rows and the latest checkpoint pass.
pub async fn queue_stats(
    db: &AdminDb,
    checkpoint: Option<&CheckpointService>,
) -> Result<QueueStatsResponse, AdminError> {
    let jobs = db.job_rows.list_jobs().await.map_err(|error| {
        AdminError::internal(&format_args!("job registry read failed: {error}"))
    })?;
    Ok(QueueStatsResponse {
        jobs: jobs
            .into_iter()
            .map(|job| JobView {
                name: job.name,
                kind: job.kind.as_str().to_owned(),
                state: job.state.as_str().to_owned(),
                last_heartbeat_at: job.last_heartbeat_at,
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
        mode: match pass.mode {
            CheckpointMode::Passive => "passive".to_owned(),
            CheckpointMode::Truncate => "truncate".to_owned(),
        },
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

/// Limiter posture per provider plus slot-lane occupancy.
pub fn provider_stats(providers: &Arc<Providers>) -> ProviderStatsResponse {
    let mut rows: Vec<ProviderLimiterView> = providers
        .limiters
        .sources()
        .into_iter()
        .filter_map(|source| {
            providers.limiter(source).map(|limiter| {
                let policy = limiter.policy();
                ProviderLimiterView {
                    source: source.to_owned(),
                    per_second: policy.per_second,
                    burst: policy.burst,
                    remaining: limiter.remaining(),
                }
            })
        })
        .collect();
    rows.sort_by(|left, right| left.source.cmp(&right.source));
    let slots = providers.slots.stats();
    ProviderStatsResponse {
        providers: rows,
        slots: SlotView {
            user_slots_available: slots.user_slots_available,
            image_slots_available: slots.image_slots_available,
            background_slots_available: slots.background_slots_available,
            user_active: slots.user_active,
            background_waiters: slots.background_waiters,
        },
    }
}
