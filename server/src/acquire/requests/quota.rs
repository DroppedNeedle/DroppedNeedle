//! Cost-control enforcement, ported from v2 `quota_service.py`.
//!
//! Two layers on purpose, not one choke point:
//!
//! - Layer 1, request-count quota, at submit. The ask lands in history long
//!   before any download task exists, so the count gate runs where the ask is
//!   made. Rolling window; pending asks count.
//! - Layer 2, byte caps, at dispatch. The global library cap binds every
//!   role; the per-user storage quota binds plain users only.
//!
//! `admin` and `trusted` skip the per-user quotas; the global cap binds
//! everyone; `upgrade`/`retry`/`wanted` origins skip both (already admitted
//! or size-neutral). Caps block at/over and never evict. Zero means unlimited.

use std::collections::HashMap;
use std::sync::RwLock;

use serde_json::{Value, json};

use super::auth::Role;
use super::dispatch::DispatchOrigin;
use super::error::RequestsError;

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

/// In-memory quota ledger. Wiring swaps the usage reads for the durable
/// stores; the decision math stays here.
pub struct QuotaLedger {
    /// Active policy.
    policy: RwLock<QuotaPolicy>,
    /// Per-user overrides.
    overrides: RwLock<HashMap<String, QuotaOverride>>,
    /// (user, asked-at epoch) for every recorded ask.
    asks: RwLock<Vec<(String, u64)>>,
    /// Library bytes attributed per user.
    user_bytes: RwLock<HashMap<String, u64>>,
    /// Whole-library bytes.
    library_bytes: RwLock<u64>,
}

impl QuotaLedger {
    /// Ledger under one policy.
    pub fn new(policy: QuotaPolicy) -> Self {
        Self {
            policy: RwLock::new(policy),
            overrides: RwLock::new(HashMap::new()),
            asks: RwLock::new(Vec::new()),
            user_bytes: RwLock::new(HashMap::new()),
            library_bytes: RwLock::new(0),
        }
    }

    /// Unbounded ledger for tests that do not probe quotas.
    pub fn unlimited() -> Self {
        Self::new(QuotaPolicy::default())
    }

    /// Replace the active policy.
    pub fn set_policy(&self, policy: QuotaPolicy) {
        if let Ok(mut slot) = self.policy.write() {
            *slot = policy;
        }
    }

    /// Set one user's override row. Bounds mirror v2 (`set_override`).
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
        self.write_overrides()?.insert(user_id.to_owned(), quota);
        Ok(())
    }

    /// Attribute library bytes to one user and the whole library.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_usage(&self, user_id: &str, user_bytes: u64, library_bytes: u64) {
        if let Ok(mut bytes) = self.user_bytes.write() {
            bytes.insert(user_id.to_owned(), user_bytes);
        }
        if let Ok(mut total) = self.library_bytes.write() {
            *total = library_bytes;
        }
    }

    /// One user's effective limits: override where set, else policy default.
    pub fn effective_quota(&self, user_id: &str) -> Result<EffectiveQuota, RequestsError> {
        let policy = self.read_policy()?.clone();
        let quota = self.read_overrides()?;
        let row = quota.get(user_id).cloned().unwrap_or_default();
        Ok(EffectiveQuota {
            request_count: row.request_count.unwrap_or(policy.request_count),
            request_days: row.request_days.unwrap_or(policy.request_days),
            storage_gb: row.storage_gb.unwrap_or(policy.storage_gb_per_user),
        })
    }

    /// Asks by one user inside the trailing window. Every history ask counts
    /// once, pending approvals included (v2 `count_requests_in_window`).
    pub fn count_in_window(
        &self,
        user_id: &str,
        window_days: u32,
        now_epoch: u64,
    ) -> Result<u32, RequestsError> {
        let window = u64::from(window_days.max(1)) * 86_400;
        let since = now_epoch.saturating_sub(window);
        let asks = self.read_asks()?;
        let mut count = 0_u32;
        for (owner, asked_at) in asks.iter() {
            if owner == user_id && *asked_at >= since {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    /// Record one admitted ask.
    pub fn record_ask(&self, user_id: &str, now_epoch: u64) -> Result<(), RequestsError> {
        self.write_asks()?.push((user_id.to_owned(), now_epoch));
        Ok(())
    }

    /// Layer 1 (submit-time). Raises when the ask would spend past the
    /// user's rolling quota. Trusted/admin are exempt; 0 means unlimited.
    pub fn check_request_quota(
        &self,
        user_id: &str,
        role: Role,
        new_requests: u32,
        now_epoch: u64,
    ) -> Result<(), RequestsError> {
        if role.is_curator() {
            return Ok(());
        }
        let quota = self.effective_quota(user_id)?;
        if quota.request_count == 0 {
            return Ok(());
        }
        let used = self.count_in_window(user_id, quota.request_days, now_epoch)?;
        if used.saturating_add(new_requests) > quota.request_count {
            return Err(RequestsError::QuotaExceeded {
                message: format!(
                    "Request limit reached ({} per {} days - you've used {used})",
                    quota.request_count, quota.request_days,
                ),
                details: json!({
                    "used": used,
                    "limit": quota.request_count,
                    "window_days": quota.request_days,
                }),
            });
        }
        Ok(())
    }

    /// Layer 2 (dispatch-time). Global cap first (all roles), then the
    /// per-user budget (role-gated, trusted/admin exempt). Upgrade, retry,
    /// and wanted origins skip both; caps block at/over.
    pub fn check_storage_admission(
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
        let policy = self.read_policy()?.clone();
        if policy.max_library_gb > 0 {
            let total = self.read_library_bytes()?;
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
        let used = self.read_user_bytes()?.get(user_id).copied().unwrap_or(0);
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

    /// One user's standing for the quota details payloads.
    pub fn usage_details(&self, user_id: &str, now_epoch: u64) -> Result<Value, RequestsError> {
        let quota = self.effective_quota(user_id)?;
        let used = self.count_in_window(user_id, quota.request_days, now_epoch)?;
        Ok(json!({
            "requests_in_window": used,
            "request_limit": quota.request_count,
            "window_days": quota.request_days,
        }))
    }

    fn read_policy(&self) -> Result<std::sync::RwLockReadGuard<'_, QuotaPolicy>, RequestsError> {
        self.policy.read().map_err(|cause| {
            RequestsError::internal(&format_args!("quota policy read failed: {cause}"))
        })
    }

    fn read_overrides(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, HashMap<String, QuotaOverride>>, RequestsError> {
        self.overrides.read().map_err(|cause| {
            RequestsError::internal(&format_args!("quota overrides read failed: {cause}"))
        })
    }

    fn write_overrides(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, HashMap<String, QuotaOverride>>, RequestsError>
    {
        self.overrides.write().map_err(|cause| {
            RequestsError::internal(&format_args!("quota overrides write failed: {cause}"))
        })
    }

    fn read_asks(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, Vec<(String, u64)>>, RequestsError> {
        self.asks.read().map_err(|cause| {
            RequestsError::internal(&format_args!("quota asks read failed: {cause}"))
        })
    }

    fn write_asks(
        &self,
    ) -> Result<std::sync::RwLockWriteGuard<'_, Vec<(String, u64)>>, RequestsError> {
        self.asks.write().map_err(|cause| {
            RequestsError::internal(&format_args!("quota asks write failed: {cause}"))
        })
    }

    fn read_user_bytes(
        &self,
    ) -> Result<std::sync::RwLockReadGuard<'_, HashMap<String, u64>>, RequestsError> {
        self.user_bytes.read().map_err(|cause| {
            RequestsError::internal(&format_args!("quota user-bytes read failed: {cause}"))
        })
    }

    fn read_library_bytes(&self) -> Result<u64, RequestsError> {
        self.library_bytes
            .read()
            .map(|total| *total)
            .map_err(|cause| {
                RequestsError::internal(&format_args!("quota library-bytes read failed: {cause}"))
            })
    }
}

/// Bytes in a GiB.
fn gib() -> u64 {
    1024 * 1024 * 1024
}

/// `used / cap GB` label, one decimal on the used side (v2 `_gb_label`).
fn gb_label(bytes_used: u64, cap_gb: u64) -> String {
    format!("{:.1} / {cap_gb} GB", bytes_used as f64 / gib() as f64)
}
