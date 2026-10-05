//! Queue and provider stats.
//!
//! Queue stats port v2's lane-occupancy gauge onto the v3 durable fabric:
//! per-channel requested/consumed demand plus the job-registry rows. Reads
//! are plain pool queries; the registry decode reuses the fabric's own
//! [`crate::db::DurableWorkWakeups::list_jobs`].
//!
//! Provider stats port v2's provider-counters snapshot onto the v3 pacing
//! primitives: per-provider limiter posture (verified row plus live bucket)
//! and slot-lane occupancy.

use std::sync::Arc;

use super::{
    AdminDb,
    error::AdminError,
    models::{
        JobView, ProviderLimiterView, ProviderStatsResponse, QueueStatsResponse, SlotView,
        WakeupChannelView,
    },
};
use crate::providers::Providers;

/// Channel demand plus registry rows.
pub async fn queue_stats(db: &AdminDb) -> Result<QueueStatsResponse, AdminError> {
    let rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT channel, seq, consumed_seq FROM durable_work_wakeups ORDER BY channel",
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|error| AdminError::internal(&format_args!("queue demand read failed: {error}")))?;
    let jobs = db.wakeups.list_jobs().await.map_err(|error| {
        AdminError::internal(&format_args!("job registry read failed: {error}"))
    })?;
    Ok(QueueStatsResponse {
        channels: rows
            .into_iter()
            .map(|(channel, requested_seq, consumed_seq)| WakeupChannelView {
                channel,
                requested_seq,
                consumed_seq,
                pending: requested_seq > consumed_seq,
            })
            .collect(),
        jobs: jobs
            .into_iter()
            .map(|job| JobView {
                name: job.name,
                kind: job.kind.as_str().to_owned(),
                wakeup_channel: job
                    .wakeup_channel
                    .map(|channel| channel.as_str().to_owned()),
                state: job.state.as_str().to_owned(),
                last_heartbeat_at: job.last_heartbeat_at,
            })
            .collect(),
    })
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
