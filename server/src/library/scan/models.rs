//! Scan models: runs, scopes, requests, inventory, failures.
//!
//! A direct port of v2's library work scan structs. Field
//! names and state spellings match v2 exactly so later SQLite work can reuse
//! the same durable vocabulary.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Per-run counters keyed by the v2 counter names (`total_count`,
/// `discovered_count`, `inspected_count`, `new_count`, `changed_count`,
/// `indexed_count`, `unchanged_count`, `excluded_count`, `missing_count`,
/// `errored_count`, `identification_enqueued_count`).
pub type Counters = HashMap<String, i64>;

/// Scan kind. `rescan_files` re-reads every in-policy file even when its
/// stat revision is unchanged; `policy_reconcile` walks but skips tag reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanKind {
    Incremental,
    RescanFiles,
    PolicyReconcile,
}

/// What triggered the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanTrigger {
    Manual,
    Automatic,
    Subsonic,
    StartupResume,
    PolicyApply,
}

/// Durable run state. Spelling matches v2 `ScanState` exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    Queued,
    Discovering,
    Indexing,
    Reconciling,
    Pausing,
    Paused,
    Stopping,
    Completed,
    Cancelled,
    SupersededPolicyChanged,
    Failed,
}

impl ScanState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ScanState::Completed
                | ScanState::Cancelled
                | ScanState::SupersededPolicyChanged
                | ScanState::Failed
        )
    }

    /// States that hold the worker: nothing else may be claimed while one
    /// of these exists (v2 `claim_next_scan_run`).
    pub fn blocks_claim(self) -> bool {
        matches!(
            self,
            ScanState::Discovering
                | ScanState::Indexing
                | ScanState::Reconciling
                | ScanState::Pausing
                | ScanState::Paused
                | ScanState::Stopping
        )
    }
}

/// Worker phase. Spelling matches v2 `ScanPhase` exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Queued,
    Discovering,
    Indexing,
    Reconciling,
}

/// Pause/stop control requested through `request_control`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanControl {
    Pause,
    Stop,
}

/// Requested-control latch stored on the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedControl {
    #[default]
    None,
    Pause,
    Stop,
}

/// One rooted subtree to walk: a whole root (`.`) or a subpath.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanScope {
    pub root_id: String,
    pub scope_id: Option<String>,
    pub relative_path: String,
    pub root_path: Option<String>,
    pub effective_policy: EffectivePolicy,
    pub policy_revision: String,
    pub estimated_count: Option<i64>,
}

impl ScanScope {
    pub fn root(root_id: &str, root_path: &str, policy_revision: &str) -> Self {
        Self {
            root_id: root_id.to_owned(),
            scope_id: Some(root_id.to_owned()),
            relative_path: ".".to_owned(),
            root_path: Some(root_path.to_owned()),
            effective_policy: EffectivePolicy::Automatic,
            policy_revision: policy_revision.to_owned(),
            estimated_count: None,
        }
    }
}

/// Effective policy for one scope or inventory row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectivePolicy {
    LocalMetadata,
    Automatic,
    Excluded,
}

/// Durable scan run. Counters live alongside the row, as in v2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRun {
    pub id: String,
    pub kind: ScanKind,
    pub trigger: ScanTrigger,
    pub state: ScanState,
    pub phase: ScanPhase,
    pub requested_by_user_id: Option<String>,
    pub aggregate_scope: String,
    pub queued_at: f64,
    pub started_at: Option<f64>,
    pub updated_at: f64,
    pub terminal_at: Option<f64>,
    pub resume_phase: Option<ScanPhase>,
    pub requested_control: RequestedControl,
    pub terminal_code: Option<String>,
    pub coalesced_request_count: i64,
    pub row_revision: u64,
    pub event_revision: u64,
    pub counters: Counters,
    pub phase_timings: HashMap<String, f64>,
}

/// A request for scan work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRequest {
    pub kind: ScanKind,
    pub trigger: ScanTrigger,
    pub scopes: Vec<ScanScope>,
    pub requested_by_user_id: Option<String>,
    pub policy_revision: String,
}

/// How `request_run` disposed of a request (v2 `ScanRequestResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Started,
    Queued,
    Coalesced,
    Expanded,
    Conflict,
}

/// Outcome of `request_run`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanRequestResult {
    pub run_id: String,
    pub disposition: Disposition,
    pub state: ScanState,
    pub row_revision: u64,
    pub queued_reason: Option<String>,
    pub conflicting_kind: Option<ScanKind>,
}

/// One discovered audio file with its classify verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanInventoryItem {
    pub root_id: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub file_size_bytes: u64,
    pub file_mtime_ns: i64,
    pub stat_revision: String,
    pub effective_policy: EffectivePolicy,
    pub comparison_result: Verdict,
    pub policy_revision: String,
    pub local_track_id: Option<String>,
    pub scope_relative_path: String,
}

/// Classify verdict. Spelling matches v2 `comparison_result` exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    New,
    Changed,
    Unchanged,
    Excluded,
    CandidateMissing,
}

/// One auditable skip or failure row (v2 `ScanFailureRecord`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanFailureRecord {
    pub root_id: String,
    pub relative_path: String,
    pub failure_code: String,
    pub recorded_at: f64,
    pub failure_detail: String,
    pub phase: ScanPhase,
}

/// Discovery state per (run, scope). Matches the v2 scope-discovery column
/// values: a clean walk lands `completed`, a degraded one `partially_read`,
/// an unreachable root `unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeDiscoveryState {
    Pending,
    Completed,
    PartiallyRead,
    Unavailable,
}

/// Failure codes shared by every scan phase. Same spellings as v2.
pub mod failure_codes {
    pub const ROOT_UNAVAILABLE: &str = "ROOT_UNAVAILABLE";
    pub const ROOT_PERMISSION_DENIED: &str = "ROOT_PERMISSION_DENIED";
    pub const WALK_TIMEOUT: &str = "WALK_TIMEOUT";
    pub const WALKER_UNAVAILABLE: &str = "WALKER_UNAVAILABLE";
    pub const PROBE_UNAVAILABLE: &str = "PROBE_UNAVAILABLE";
    pub const WALK_SUPERSEDED: &str = "WALK_SUPERSEDED";
    pub const WALK_ERROR: &str = "WALK_ERROR";
    pub const SYMLINK_ESCAPE_OUT: &str = "SYMLINK_ESCAPE_OUT";
    pub const NON_REGULAR_FILE: &str = "NON_REGULAR_FILE";
    pub const WALK_NAME_ENCODING: &str = "WALK_NAME_ENCODING";
    pub const NFC_TWIN_COLLISION: &str = "NFC_TWIN_COLLISION";
    pub const TAG_READ_DEFERRED: &str = "TAG_READ_DEFERRED";
    pub const TAG_READ_FAILED: &str = "TAG_READ_FAILED";
    pub const MTIME_SKEW: &str = "MTIME_SKEW";
    pub const SUPERSEDED_POLICY_CHANGED: &str = "SUPERSEDED_POLICY_CHANGED";
    pub const UNEXPECTED_WORKER_FAILURE: &str = "UNEXPECTED_WORKER_FAILURE";
}

/// Counter names shared by the coordinator and the store.
pub mod counter_names {
    pub const TOTAL: &str = "total_count";
    pub const DISCOVERED: &str = "discovered_count";
    pub const INSPECTED: &str = "inspected_count";
    pub const NEW: &str = "new_count";
    pub const CHANGED: &str = "changed_count";
    pub const INDEXED: &str = "indexed_count";
    pub const UNCHANGED: &str = "unchanged_count";
    pub const EXCLUDED: &str = "excluded_count";
    pub const MISSING: &str = "missing_count";
    pub const ERRORED: &str = "errored_count";
    pub const IDENTIFICATION_ENQUEUED: &str = "identification_enqueued_count";
}

/// True when `ancestor` covers `descendant` within one root (v2
/// `_scan_scope_covers_path`: `.` covers everything, otherwise exact or
/// slash-prefix match).
pub fn scope_covers_path(ancestor: &str, descendant: &str) -> bool {
    ancestor == "." || ancestor == descendant || descendant.starts_with(&format!("{ancestor}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_cover_rules_match_v2() {
        assert!(scope_covers_path(".", "a/b.flac"));
        assert!(scope_covers_path("a", "a"));
        assert!(scope_covers_path("a", "a/b.flac"));
        assert!(!scope_covers_path("a", "ab/c.flac"));
        assert!(!scope_covers_path("a/b", "a"));
    }
}
