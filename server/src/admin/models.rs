//! Admin-UX wire shapes. Clean-slate v3; no v2 byte compatibility is kept.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One backup on disk, with its manifest identity when present.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackupView {
    /// File name inside the backups directory.
    pub name: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Hex SHA-256 from the manifest, when a manifest sits beside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Schema stamp from the manifest, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_version: Option<i64>,
    /// Manifest creation time as unix seconds, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at_unix: Option<u64>,
}

/// Backup listing, oldest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackupListResponse {
    /// Backups on disk, oldest first.
    pub backups: Vec<BackupView>,
    /// Rolling retention: the directory keeps this many.
    pub keep: u32,
}

/// What one backup run produced.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackupRunResponse {
    /// File name of the new backup.
    pub name: String,
    /// Hex SHA-256 of the new backup.
    pub sha256: String,
    /// New backup size in bytes.
    pub size_bytes: u64,
    /// Schema stamp in the new backup.
    pub user_version: i64,
    /// How long the run took, in milliseconds.
    pub duration_ms: u64,
}

/// One pre-restore verification check.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RestoreCheck {
    /// Check name, e.g. `manifest-sha256`.
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Plain-language detail.
    pub detail: String,
}

/// Pre-restore verification report. Read-only: nothing here writes or moves
/// the live database (the actual restore stays an offline CLI).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RestoreReport {
    /// Backup file name the report covers.
    pub backup: String,
    /// True when every check passed.
    pub ok: bool,
    /// True when an offline restore of this backup should succeed.
    pub restorable: bool,
    /// Per-check results.
    pub checks: Vec<RestoreCheck>,
}

/// Provider byte-cache counters.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CacheStatsResponse {
    /// Entries currently held.
    pub entries: usize,
    /// Registered invalidation roots by source.
    pub sources: Vec<String>,
    /// Cover and artist images on disk.
    pub cover_images: u64,
    /// Their total size in bytes.
    pub cover_bytes: u64,
}

/// Cache-clear request. Empty scope clears everything.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct CacheClearBody {
    /// `all` (default: provider responses and images), `source` (one
    /// provider's responses), `covers` (cover and artist images only) or
    /// `audiodb` (TheAudioDB answers only).
    #[serde(default)]
    pub scope: Option<String>,
    /// Source name when `scope` is `source`.
    #[serde(default)]
    pub source: Option<String>,
}

/// What one cache clear dropped.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CacheClearResponse {
    /// Plain-language summary.
    pub message: String,
    /// Entries dropped.
    pub cleared_entries: usize,
    /// Entries left behind.
    pub remaining_entries: usize,
    /// Cover and artist images deleted from disk.
    pub cleared_cover_images: u64,
}

/// `POST /cache/sync/cancel` answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CacheSyncCancelResponse {
    /// `cancelled`.
    pub status: String,
}

/// One durable-work channel's demand.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WakeupChannelView {
    /// Channel name.
    pub channel: String,
    /// Wakeups requested.
    pub requested_seq: i64,
    /// Wakeups consumed.
    pub consumed_seq: i64,
    /// True when requested work is still unconsumed.
    pub pending: bool,
}

/// One registered background job.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JobView {
    /// Job name, the registry key.
    pub name: String,
    /// `durable` or `ephemeral`.
    pub kind: String,
    /// Waking channel, when the job has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wakeup_channel: Option<String>,
    /// `idle`, `running`, `stopped`, or `failed`.
    pub state: String,
    /// Last heartbeat as unix seconds, when the job ever beat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_heartbeat_at: Option<f64>,
}

/// Queue and job-registry snapshot.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QueueStatsResponse {
    /// Demand per durable-work channel.
    pub channels: Vec<WakeupChannelView>,
    /// Registered background jobs in name order.
    pub jobs: Vec<JobView>,
}

/// One provider's limiter posture.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderLimiterView {
    /// Provider name, e.g. `musicbrainz`.
    pub source: String,
    /// Sustained tokens per second.
    pub per_second: f64,
    /// Bucket capacity.
    pub burst: u32,
    /// Whole tokens currently available.
    pub remaining: u32,
}

/// Slot-lane posture.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SlotView {
    /// Free user-lane permits.
    pub user_slots_available: usize,
    /// Free image-lane permits.
    pub image_slots_available: usize,
    /// Free background-lane permits.
    pub background_slots_available: usize,
    /// Whether user activity is inside the quiet window.
    pub user_active: bool,
    /// Background callers currently waiting for quiet.
    pub background_waiters: usize,
}

/// Provider limiter and slot snapshot.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProviderStatsResponse {
    /// Limiter posture per provider.
    pub providers: Vec<ProviderLimiterView>,
    /// Slot-lane posture.
    pub slots: SlotView,
}

/// Per-user quota overrides. `None` fields inherit the global download-policy
/// defaults; an all-`None` body clears the row back to pure inherit.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct QuotaOverrideBody {
    /// Asks allowed per window (0 = unlimited).
    pub request_quota_count: Option<u32>,
    /// Window length in days.
    pub request_quota_days: Option<u32>,
    /// Library bytes in GiB (0 = unlimited).
    pub storage_quota_gb: Option<u64>,
}

/// One user's quota standing for the admin editor.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuotaResponse {
    /// User id.
    pub user_id: String,
    /// Stored override row (`None` fields inherit).
    pub quota_override: QuotaOverrideView,
    /// Effective asks per window after overrides.
    pub effective_request_quota_count: u32,
    /// Effective window length in days after overrides.
    pub effective_request_quota_days: u32,
    /// Effective storage in GiB after overrides.
    pub effective_storage_quota_gb: u64,
    /// Asks inside the trailing window.
    pub requests_in_window: u32,
    /// Landed download bytes attributed to the user.
    pub storage_bytes: u64,
    /// True for admin/trusted accounts, which skip per-user quotas.
    pub exempt: bool,
}

/// Stored override row for display.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QuotaOverrideView {
    /// Asks allowed per window, when overridden.
    pub request_quota_count: Option<u32>,
    /// Window length in days, when overridden.
    pub request_quota_days: Option<u32>,
    /// Library bytes in GiB, when overridden.
    pub storage_quota_gb: Option<u64>,
}

/// One precache run starting. The run continues in the background under
/// the `precache-library` job name; a second start while it is live is 409.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PrecacheRunResponse {
    /// Registered job name of the run.
    pub job: String,
    /// `started`.
    pub status: String,
}

/// Latest checkpoint pass, for the health payload. `None` on `/health`
/// until the checkpoint loop records its first pass.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CheckpointView {
    /// `passive` or `truncate`.
    pub mode: String,
    /// True when a lock contender blocked the pass.
    pub busy: bool,
    /// Uncheckpointed bytes, or -1 on an unmeasured pass.
    pub active_bytes: i64,
    /// `-wal` file allocation in bytes.
    pub wal_file_bytes: u64,
    /// True when background producers must yield.
    pub suspended: bool,
    /// Pass time as unix seconds.
    pub at_unix: u64,
    /// Non-lock failure text, when the pass failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
