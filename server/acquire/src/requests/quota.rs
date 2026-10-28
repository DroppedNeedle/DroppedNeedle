//! Cost-control enforcement.
//!
//! Two layers on purpose, not one choke point:
//!
//! - Layer 1, request-count quota, at submit. The ask lands in history long
//!   before any download task exists, so the count gate runs where the ask is
//!   made: [`QuotaLedger::request_gate`] builds the limit and the request
//!   store counts the asker's window inside the same write that admits the
//!   ask. Rolling window; pending asks count.
//! - Layer 2, byte caps, at dispatch. The global library cap binds every
//!   role; the per-user storage quota binds plain users only. Usage is read
//!   from the library tables at check time.
//!
//! `admin` and `trusted` skip the per-user quotas; the global cap binds
//! everyone; `upgrade`/`retry`/`wanted` origins skip both (already admitted
//! or size-neutral). Caps block at/over and never evict. Zero means unlimited.
//! The policy is read fresh on every check, so saved settings bite at once.

use std::collections::HashMap;
use std::sync::RwLock;

use serde_json::json;

use super::error::RequestsError;
use super::ledger::{QuotaGate, QuotaRefusal};
use super::sql;
use super::sqlite::RequestStore;
use crate::db::{AcquireDb, to_u64};
use crate::dispatch::DispatchOrigin;
use droppedneedle_auth::Role;
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

/// Global download-policy quota knobs.
#[derive(Debug, Clone)]
pub struct QuotaPolicy {
    /// Plain-user asks allowed per window (0 = unlimited).
    pub request_count: u32,
    /// Window length in days.
    pub request_days: u32,
    /// Plain-user library bytes in GiB (0 = unlimited).
    pub storage_gb_per_user: u64,
    /// Whole-library bytes in GiB (0 = unlimited).
    pub max_library_gb: u64,
}

impl QuotaPolicy {
    /// The limits the download-policy section sets.
    pub fn from_policy(policy: &DownloadPolicy) -> Self {
        use crate::settings::{clamp_u32, clamp_u64};
        Self {
            request_count: clamp_u32(policy.default_request_quota_count),
            request_days: clamp_u32(policy.default_request_quota_days).max(1),
            storage_gb_per_user: clamp_u64(policy.default_storage_quota_gb),
            max_library_gb: clamp_u64(policy.max_library_size_gb),
        }
    }
}

impl Default for QuotaPolicy {
    fn default() -> Self {
        Self {
            request_count: 0,
            request_days: 7,
            storage_gb_per_user: 0,
            max_library_gb: 0,
        }
    }
}

/// Per-user override; `None` fields inherit the policy.
#[derive(Debug, Clone, Default)]
pub struct QuotaOverride {
    /// Asks allowed per window.
    pub request_count: Option<u32>,
    /// Window length in days.
    pub request_days: Option<u32>,
    /// Library bytes in GiB.
    pub storage_gb: Option<u64>,
}

/// Effective limits after overrides.
#[derive(Debug, Clone)]
pub struct EffectiveQuota {
    /// Asks allowed per window (0 = unlimited).
    pub request_count: u32,
    /// Window length in days.
    pub request_days: u32,
    /// Library bytes in GiB (0 = unlimited).
    pub storage_gb: u64,
}

/// Quota decisions over the live policy, the admin overrides and the
/// durable request and library rows.
pub struct QuotaLedger {
    /// The download-policy section the limits come from; `None` for no
    /// limits.
    policy: Option<Live<DownloadPolicy>>,
    /// Per-user overrides. The admin face persists them in `user_quotas`
    /// and reloads them at boot.
    overrides: RwLock<HashMap<String, QuotaOverride>>,
    /// Request rows, for window counts.
    requests: RequestStore,
    /// Library rows, for storage usage.
    db: AcquireDb,
}

impl QuotaLedger {
    /// Ledger over the live download policy and the database.
    pub fn new(policy: Live<DownloadPolicy>, db: AcquireDb) -> Self {
        Self::over(Some(policy), db)
    }

    /// Ledger with no limits (the policy defaults).
    pub fn unlimited(db: AcquireDb) -> Self {
        Self::over(None, db)
    }

    fn over(policy: Option<Live<DownloadPolicy>>, db: AcquireDb) -> Self {
        Self {
            policy,
            overrides: RwLock::new(HashMap::new()),
            requests: RequestStore::new(db.clone()),
            db,
        }
    }

    /// The limits as saved right now. A download policy that does not read
    /// is never "unlimited": every check fails until it is saved again.
    fn policy(&self) -> Result<QuotaPolicy, RequestsError> {
        let Some(live) = self.policy.as_ref() else {
            return Ok(QuotaPolicy::default());
        };
        live.read()
            .map(|policy| QuotaPolicy::from_policy(&policy))
            .ok_or_else(|| RequestsError::internal(&"download settings cannot be read"))
    }

    /// Set one user's override row, within the bounds below.
    pub fn set_override(&self, user_id: &str, quota: QuotaOverride) -> Result<(), RequestsError> {
        let bounds = [
            (
                "request_quota_count",
                quota.request_count.map(u64::from),
                0_u64,
            ),
            (
                "request_quota_days",
                quota.request_days.map(u64::from),
                1_u64,
            ),
            ("storage_quota_gb", quota.storage_gb, 0_u64),
        ];
        for (name, value, low) in bounds {
            if let Some(value) = value
                && (value < low || value > 1_000_000)
            {
                return Err(RequestsError::InvalidInput {
                    message: format!("{name} must be between {low} and 1000000"),
                });
            }
        }
        self.overrides
            .write()
            .map_err(|cause| {
                RequestsError::internal(&format_args!("quota overrides write failed: {cause}"))
            })?
            .insert(user_id.to_owned(), quota);
        Ok(())
    }

    /// One user's effective limits: override where set, else policy default.
    pub fn effective_quota(&self, user_id: &str) -> Result<EffectiveQuota, RequestsError> {
        let policy = self.policy()?;
        let row = self
            .overrides
            .read()
            .map_err(|cause| {
                RequestsError::internal(&format_args!("quota overrides read failed: {cause}"))
            })?
            .get(user_id)
            .cloned()
            .unwrap_or_default();
        Ok(EffectiveQuota {
            request_count: row.request_count.unwrap_or(policy.request_count),
            request_days: row.request_days.unwrap_or(policy.request_days).max(1),
            storage_gb: row.storage_gb.unwrap_or(policy.storage_gb_per_user),
        })
    }

    /// Layer 1 limit for one ask, or `None` when nothing limits it
    /// (curators, or a zero count). The request store enforces it inside
    /// the write that admits the ask.
    pub fn request_gate(
        &self,
        user_id: &str,
        role: Role,
        new_requests: u32,
    ) -> Result<Option<QuotaGate>, RequestsError> {
        if role.is_curator() {
            return Ok(None);
        }
        let quota = self.effective_quota(user_id)?;
        if quota.request_count == 0 {
            return Ok(None);
        }
        Ok(Some(QuotaGate {
            limit: quota.request_count,
            window_days: quota.request_days,
            new_requests,
        }))
    }

    /// The error a refused gate answers with.
    pub fn refusal(refusal: &QuotaRefusal) -> RequestsError {
        RequestsError::QuotaExceeded {
            message: format!(
                "Request limit reached ({} per {} days - you've used {})",
                refusal.limit, refusal.window_days, refusal.used,
            ),
            details: json!({
                "used": refusal.used,
                "limit": refusal.limit,
                "window_days": refusal.window_days,
            }),
        }
    }

    /// Asks by one user inside the trailing window. Every history ask counts
    /// once, pending approvals included.
    pub async fn count_in_window(
        &self,
        user_id: &str,
        window_days: u32,
        now_epoch: u64,
    ) -> Result<u32, RequestsError> {
        let since = now_epoch.saturating_sub(u64::from(window_days.max(1)) * 86_400);
        self.requests.count_asks_since(user_id, since).await
    }

    /// Layer 2 (dispatch-time). Global cap first (all roles), then the
    /// per-user budget (role-gated, trusted/admin exempt). Upgrade, retry,
    /// and wanted origins skip both; caps block at/over.
    pub async fn check_storage_admission(
        &self,
        user_id: &str,
        role: Role,
        origin: DispatchOrigin,
    ) -> Result<(), RequestsError> {
        if matches!(
            origin,
            DispatchOrigin::Upgrade | DispatchOrigin::Retry | DispatchOrigin::Wanted
        ) {
            return Ok(());
        }
        let policy = self.policy()?;
        if policy.max_library_gb > 0 {
            let total = self.library_bytes().await?;
            if total >= policy.max_library_gb * gib() {
                return Err(RequestsError::StorageFull {
                    message: format!(
                        "Library storage limit reached ({})",
                        gb_label(total, policy.max_library_gb),
                    ),
                    details: json!({
                        "used_bytes": total,
                        "limit_gb": policy.max_library_gb,
                    }),
                });
            }
        }
        if role.is_curator() {
            return Ok(());
        }
        let quota = self.effective_quota(user_id)?;
        if quota.storage_gb == 0 {
            return Ok(());
        }
        let used = self.user_bytes(user_id).await?;
        if used >= quota.storage_gb * gib() {
            return Err(RequestsError::StorageFull {
                message: format!(
                    "Your storage budget is full ({})",
                    gb_label(used, quota.storage_gb),
                ),
                details: json!({
                    "used_bytes": used,
                    "limit_gb": quota.storage_gb,
                }),
            });
        }
        Ok(())
    }

    /// Whole-library indexed bytes.
    async fn library_bytes(&self) -> Result<u64, RequestsError> {
        let total: i64 = self
            .db
            .db()
            .read("quota.library_bytes", |conn| {
                conn.query_row(sql::LIBRARY_BYTES.text(), [], |row| row.get(0))
                    .map_err(DbError::from)
            })
            .await
            .map_err(|error| {
                RequestsError::internal(&format_args!("library bytes read: {error}"))
            })?;
        Ok(to_u64(total))
    }

    /// Indexed bytes landed by one user's download tasks.
    /// Scanned files have no task and count only
    /// toward the global cap.
    async fn user_bytes(&self, user_id: &str) -> Result<u64, RequestsError> {
        let user_id = user_id.to_owned();
        let total: i64 = self
            .db
            .db()
            .read("quota.user_bytes", move |conn| {
                conn.query_row(sql::USER_BYTES.text(), [user_id], |row| row.get(0))
                    .map_err(DbError::from)
            })
            .await
            .map_err(|error| RequestsError::internal(&format_args!("user bytes read: {error}")))?;
        Ok(to_u64(total))
    }
}

/// Bytes in a GiB.
fn gib() -> u64 {
    1024 * 1024 * 1024
}

/// `used / cap GB` label, one decimal on the used side.
fn gb_label(bytes_used: u64, cap_gb: u64) -> String {
    format!("{:.1} / {cap_gb} GB", bytes_used as f64 / gib() as f64)
}
