//! Scan models: runs, scopes, requests, inventory, failures.
//!
//! Field names and stored spellings match v2's library scan structs, so a
//! database v2 wrote reads the same. Every stored value has one spelling,
//! [`as_str`](ScanState::as_str) to write it and `parse` to read it back;
//! a value no release wrote fails to read.
//!
//! A run's state is one [`RunState`], changed only by [`transition`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Give a fieldless enum its stored spelling: `as_str` and a `parse` that
/// answers `None` for any other text.
macro_rules! spelled {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            /// The stored spelling.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            /// The value `text` spells, if any.
            pub fn parse(text: &str) -> Option<Self> {
                match text {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

/// A run's counters, one per column of the runs table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Counters {
    /// Files the run will look at.
    pub total_count: i64,
    /// Files the walk found.
    pub discovered_count: i64,
    /// Files the index phase looked at.
    pub inspected_count: i64,
    /// Files new to the catalog.
    pub new_count: i64,
    /// Files changed since the last scan.
    pub changed_count: i64,
    /// Files written to the catalog.
    pub indexed_count: i64,
    /// Files unchanged since the last scan.
    pub unchanged_count: i64,
    /// Files a policy leaves out.
    pub excluded_count: i64,
    /// Catalog files the walk no longer found.
    pub missing_count: i64,
    /// Files that could not be read or written.
    pub errored_count: i64,
    /// Albums offered for identification.
    pub identification_enqueued_count: i64,
}

/// One of a run's counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counter {
    Total,
    Discovered,
    Inspected,
    New,
    Changed,
    Indexed,
    Unchanged,
    Excluded,
    Missing,
    Errored,
    IdentificationEnqueued,
}

spelled!(Counter {
    Total => "total_count",
    Discovered => "discovered_count",
    Inspected => "inspected_count",
    New => "new_count",
    Changed => "changed_count",
    Indexed => "indexed_count",
    Unchanged => "unchanged_count",
    Excluded => "excluded_count",
    Missing => "missing_count",
    Errored => "errored_count",
    IdentificationEnqueued => "identification_enqueued_count",
});

impl Counter {
    /// Every counter.
    pub const ALL: [Counter; 11] = [
        Counter::Total,
        Counter::Discovered,
        Counter::Inspected,
        Counter::New,
        Counter::Changed,
        Counter::Indexed,
        Counter::Unchanged,
        Counter::Excluded,
        Counter::Missing,
        Counter::Errored,
        Counter::IdentificationEnqueued,
    ];

    /// The runs-table column that holds it, which is also its name.
    pub const fn column(self) -> &'static str {
        self.as_str()
    }
}

impl Counters {
    /// One counter's value.
    pub fn get(&self, counter: Counter) -> i64 {
        *self.field(counter)
    }

    /// One counter's slot.
    pub fn slot(&mut self, counter: Counter) -> &mut i64 {
        match counter {
            Counter::Total => &mut self.total_count,
            Counter::Discovered => &mut self.discovered_count,
            Counter::Inspected => &mut self.inspected_count,
            Counter::New => &mut self.new_count,
            Counter::Changed => &mut self.changed_count,
            Counter::Indexed => &mut self.indexed_count,
            Counter::Unchanged => &mut self.unchanged_count,
            Counter::Excluded => &mut self.excluded_count,
            Counter::Missing => &mut self.missing_count,
            Counter::Errored => &mut self.errored_count,
            Counter::IdentificationEnqueued => &mut self.identification_enqueued_count,
        }
    }

    fn field(&self, counter: Counter) -> &i64 {
        match counter {
            Counter::Total => &self.total_count,
            Counter::Discovered => &self.discovered_count,
            Counter::Inspected => &self.inspected_count,
            Counter::New => &self.new_count,
            Counter::Changed => &self.changed_count,
            Counter::Indexed => &self.indexed_count,
            Counter::Unchanged => &self.unchanged_count,
            Counter::Excluded => &self.excluded_count,
            Counter::Missing => &self.missing_count,
            Counter::Errored => &self.errored_count,
            Counter::IdentificationEnqueued => &self.identification_enqueued_count,
        }
    }
}

/// Scan kind. `rescan_files` re-reads every in-policy file even when its
/// stat revision is unchanged; `policy_reconcile` walks but skips tag reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanKind {
    Incremental,
    RescanFiles,
    PolicyReconcile,
}

spelled!(ScanKind {
    Incremental => "incremental",
    RescanFiles => "rescan_files",
    PolicyReconcile => "policy_reconcile",
});

/// What triggered the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanTrigger {
    Manual,
    Automatic,
    Subsonic,
    StartupResume,
    PolicyApply,
}

spelled!(ScanTrigger {
    Manual => "manual",
    Automatic => "automatic",
    Subsonic => "subsonic",
    StartupResume => "startup_resume",
    PolicyApply => "policy_apply",
});

/// A run's state, as the API reports it and the runs table stores it.
// The run itself holds a `RunState`; this is how its state is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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

spelled!(ScanState {
    Queued => "queued",
    Discovering => "discovering",
    Indexing => "indexing",
    Reconciling => "reconciling",
    Pausing => "pausing",
    Paused => "paused",
    Stopping => "stopping",
    Completed => "completed",
    Cancelled => "cancelled",
    SupersededPolicyChanged => "superseded_policy_changed",
    Failed => "failed",
});

/// A run's phase as the API reports it and the runs table stores it:
/// `queued` before the run starts, then the phase it is in or reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanPhase {
    Queued,
    Discovering,
    Indexing,
    Reconciling,
}

spelled!(ScanPhase {
    Queued => "queued",
    Discovering => "discovering",
    Indexing => "indexing",
    Reconciling => "reconciling",
});

/// The phase of a run that has started, in the order a run goes through
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Discovering,
    Indexing,
    Reconciling,
}

impl Phase {
    /// The phase as the API spells it.
    pub const fn scan_phase(self) -> ScanPhase {
        match self {
            Phase::Discovering => ScanPhase::Discovering,
            Phase::Indexing => ScanPhase::Indexing,
            Phase::Reconciling => ScanPhase::Reconciling,
        }
    }

    /// The started phase `phase` names; `None` for `queued`.
    pub const fn of(phase: ScanPhase) -> Option<Phase> {
        match phase {
            ScanPhase::Queued => None,
            ScanPhase::Discovering => Some(Phase::Discovering),
            ScanPhase::Indexing => Some(Phase::Indexing),
            ScanPhase::Reconciling => Some(Phase::Reconciling),
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Cancelled,
    SupersededPolicyChanged,
    Failed,
}

/// Where a run is. The worker works only on a `Running` run; a pause or a
/// stop asked of it is `Pausing` or `Stopping` until the worker reaches a
/// point where it can settle, and a paused run resumes in the phase it
/// left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Queued,
    Running(Phase),
    Pausing(Phase),
    Paused(Phase),
    Stopping(Phase),
    /// `reached` is the last phase the run was in, `None` if it never
    /// started.
    Ended {
        outcome: Outcome,
        reached: Option<Phase>,
    },
}

/// What can happen to a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The worker takes a queued run.
    Claim,
    /// The worker finished the run's phase: on to the next, or done.
    Advance,
    /// A person asks the run to pause.
    Pause,
    /// A person asks a paused run to go on.
    Resume,
    /// A person asks the run to stop.
    Stop,
    /// A pause or a stop asked of the run takes effect.
    Settle,
    /// The worker could not go on.
    Fail,
    /// The library settings changed under the run.
    Supersede,
}

/// An event the run's state does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Illegal {
    pub from: RunState,
    pub event: Event,
}

/// The state `event` moves a run in `state` to.
pub fn transition(state: RunState, event: Event) -> Result<RunState, Illegal> {
    use RunState::{Ended, Paused, Pausing, Queued, Running, Stopping};
    let ended = |outcome, reached| Ended {
        outcome,
        reached: Some(reached),
    };
    let next = match (state, event) {
        (Queued, Event::Claim) => Running(Phase::Discovering),
        (Running(Phase::Discovering), Event::Advance) => Running(Phase::Indexing),
        (Running(Phase::Indexing), Event::Advance) => Running(Phase::Reconciling),
        (Running(Phase::Reconciling), Event::Advance) => {
            ended(Outcome::Completed, Phase::Reconciling)
        }
        (Running(phase), Event::Pause) => Pausing(phase),
        (Paused(phase), Event::Resume) => Running(phase),
        (Queued, Event::Stop) => Ended {
            outcome: Outcome::Cancelled,
            reached: None,
        },
        (Running(phase) | Pausing(phase), Event::Stop) => Stopping(phase),
        (Paused(phase), Event::Stop) => ended(Outcome::Cancelled, phase),
        (Pausing(phase), Event::Settle) => Paused(phase),
        (Stopping(phase), Event::Settle) => ended(Outcome::Cancelled, phase),
        (Running(phase), Event::Fail) => ended(Outcome::Failed, phase),
        (Running(phase) | Pausing(phase) | Paused(phase), Event::Supersede) => {
            ended(Outcome::SupersededPolicyChanged, phase)
        }
        (from, event) => return Err(Illegal { from, event }),
    };
    Ok(next)
}

/// Pending control on a run, as the runs table stores it and the API
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestedControl {
    None,
    Pause,
    Stop,
}

spelled!(RequestedControl {
    None => "none",
    Pause => "pause",
    Stop => "stop",
});

impl RunState {
    /// The state as the API reports it.
    pub const fn scan_state(self) -> ScanState {
        match self {
            RunState::Queued => ScanState::Queued,
            RunState::Running(Phase::Discovering) => ScanState::Discovering,
            RunState::Running(Phase::Indexing) => ScanState::Indexing,
            RunState::Running(Phase::Reconciling) => ScanState::Reconciling,
            RunState::Pausing(_) => ScanState::Pausing,
            RunState::Paused(_) => ScanState::Paused,
            RunState::Stopping(_) => ScanState::Stopping,
            RunState::Ended { outcome, .. } => match outcome {
                Outcome::Completed => ScanState::Completed,
                Outcome::Cancelled => ScanState::Cancelled,
                Outcome::SupersededPolicyChanged => ScanState::SupersededPolicyChanged,
                Outcome::Failed => ScanState::Failed,
            },
        }
    }

    /// The phase the run is in or reached; `queued` before it started.
    pub const fn phase(self) -> ScanPhase {
        match self {
            RunState::Queued | RunState::Ended { reached: None, .. } => ScanPhase::Queued,
            RunState::Running(phase)
            | RunState::Pausing(phase)
            | RunState::Paused(phase)
            | RunState::Stopping(phase)
            | RunState::Ended {
                reached: Some(phase),
                ..
            } => phase.scan_phase(),
        }
    }

    /// The phase a pausing or paused run picks up from.
    pub const fn resume_phase(self) -> Option<ScanPhase> {
        match self {
            RunState::Pausing(phase) | RunState::Paused(phase) => Some(phase.scan_phase()),
            _ => None,
        }
    }

    /// The control asked of the run and not yet undone.
    pub const fn requested_control(self) -> RequestedControl {
        match self {
            RunState::Pausing(_) | RunState::Paused(_) => RequestedControl::Pause,
            RunState::Stopping(_) => RequestedControl::Stop,
            _ => RequestedControl::None,
        }
    }

    /// The phase the worker is working through, while it is.
    pub const fn running(self) -> Option<Phase> {
        match self {
            RunState::Running(phase) => Some(phase),
            _ => None,
        }
    }

    /// The phase whose time is running: the worker is in it, or still
    /// winding down to a pause or a stop.
    pub const fn timed(self) -> Option<Phase> {
        match self {
            RunState::Running(phase) | RunState::Pausing(phase) | RunState::Stopping(phase) => {
                Some(phase)
            }
            _ => None,
        }
    }

    /// True once the run has ended.
    pub const fn is_ended(self) -> bool {
        matches!(self, RunState::Ended { .. })
    }

    /// True while a pause or a stop waits to take effect.
    pub const fn is_settling(self) -> bool {
        matches!(self, RunState::Pausing(_) | RunState::Stopping(_))
    }

    /// The state the runs table's `state` and `phase` columns spell, or
    /// `None` when they spell none: an unknown word, or a paused or
    /// stopping run with no phase.
    pub fn from_columns(state: &str, phase: &str) -> Option<RunState> {
        let phase = Phase::of(ScanPhase::parse(phase)?);
        let ended = |outcome| RunState::Ended {
            outcome,
            reached: phase,
        };
        Some(match ScanState::parse(state)? {
            ScanState::Queued => RunState::Queued,
            ScanState::Discovering => RunState::Running(Phase::Discovering),
            ScanState::Indexing => RunState::Running(Phase::Indexing),
            ScanState::Reconciling => RunState::Running(Phase::Reconciling),
            ScanState::Pausing => RunState::Pausing(phase?),
            ScanState::Paused => RunState::Paused(phase?),
            ScanState::Stopping => RunState::Stopping(phase?),
            ScanState::Completed => ended(Outcome::Completed),
            ScanState::Cancelled => ended(Outcome::Cancelled),
            ScanState::SupersededPolicyChanged => ended(Outcome::SupersededPolicyChanged),
            ScanState::Failed => ended(Outcome::Failed),
        })
    }
}

/// Seconds a run spent in each phase it reached. A phase's time stops
/// while the run is paused.
pub type PhaseTimings = BTreeMap<Phase, f64>;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectivePolicy {
    LocalMetadata,
    Automatic,
    Excluded,
}

spelled!(EffectivePolicy {
    LocalMetadata => "local_metadata",
    Automatic => "automatic",
    Excluded => "excluded",
});

/// Durable scan run. Counters live alongside the row, as in v2.
#[derive(Debug, Clone)]
pub struct ScanRun {
    pub id: String,
    pub kind: ScanKind,
    pub trigger: ScanTrigger,
    pub state: RunState,
    pub requested_by_user_id: Option<String>,
    pub aggregate_scope: String,
    pub queued_at: f64,
    pub started_at: Option<f64>,
    pub updated_at: f64,
    pub terminal_at: Option<f64>,
    pub terminal_code: Option<FailureCode>,
    pub coalesced_request_count: i64,
    pub row_revision: u64,
    pub event_revision: u64,
    pub counters: Counters,
    pub phase_timings: PhaseTimings,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ScanDisposition)]
pub enum Disposition {
    /// A new run started.
    Started,
    /// A new run waits behind the active one.
    Queued,
    /// An existing run already covers it.
    Coalesced,
    /// A queued run grew to cover it.
    Expanded,
    /// A run of another kind is in the way.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ScanFileVerdict)]
pub enum Verdict {
    New,
    Changed,
    Unchanged,
    Excluded,
    CandidateMissing,
}

spelled!(Verdict {
    New => "new",
    Changed => "changed",
    Unchanged => "unchanged",
    Excluded => "excluded",
    CandidateMissing => "candidate_missing",
});

/// One auditable skip or failure row (v2 `ScanFailureRecord`).
#[derive(Debug, Clone)]
pub struct ScanFailureRecord {
    pub root_id: String,
    pub relative_path: String,
    pub failure_code: FailureCode,
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

spelled!(ScopeDiscoveryState {
    Pending => "pending",
    Completed => "completed",
    PartiallyRead => "partially_read",
    Unavailable => "unavailable",
});

/// Why a scan skipped or failed a path, or why a run ended early. The
/// `WALK_E*` codes name the operating system's error for a folder the scan
/// could not read.
// Spelled as v2 spelled them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FailureCode {
    RootUnavailable,
    RootPermissionDenied,
    WalkTimeout,
    WalkerUnavailable,
    ProbeUnavailable,
    /// Written by earlier releases only.
    WalkSuperseded,
    WalkError,
    WalkEperm,
    WalkEnoent,
    WalkEio,
    WalkEacces,
    WalkEnotdir,
    WalkEisdir,
    WalkEinval,
    WalkEnfile,
    WalkEmfile,
    WalkEnospc,
    WalkErofs,
    WalkEnametoolong,
    WalkEloop,
    WalkEunknown,
    SymlinkEscapeOut,
    NonRegularFile,
    WalkNameEncoding,
    NfcTwinCollision,
    TagReadDeferred,
    TagReadFailed,
    MtimeSkew,
    SupersededPolicyChanged,
    UnexpectedWorkerFailure,
    /// A clean walk would have marked most of a scope missing at once (an
    /// unmounted share looks exactly like this), so reconcile held off.
    MassMissingGuard,
    /// The catalog refused one file's rows; the file is skipped this run.
    CatalogCommitFailed,
    /// Internal: the run has ended and still owes the end passes named by
    /// the row's path. Never listed among the run's failures.
    RunEndPending,
}

spelled!(FailureCode {
    RootUnavailable => "ROOT_UNAVAILABLE",
    RootPermissionDenied => "ROOT_PERMISSION_DENIED",
    WalkTimeout => "WALK_TIMEOUT",
    WalkerUnavailable => "WALKER_UNAVAILABLE",
    ProbeUnavailable => "PROBE_UNAVAILABLE",
    WalkSuperseded => "WALK_SUPERSEDED",
    WalkError => "WALK_ERROR",
    WalkEperm => "WALK_EPERM",
    WalkEnoent => "WALK_ENOENT",
    WalkEio => "WALK_EIO",
    WalkEacces => "WALK_EACCES",
    WalkEnotdir => "WALK_ENOTDIR",
    WalkEisdir => "WALK_EISDIR",
    WalkEinval => "WALK_EINVAL",
    WalkEnfile => "WALK_ENFILE",
    WalkEmfile => "WALK_EMFILE",
    WalkEnospc => "WALK_ENOSPC",
    WalkErofs => "WALK_EROFS",
    WalkEnametoolong => "WALK_ENAMETOOLONG",
    WalkEloop => "WALK_ELOOP",
    WalkEunknown => "WALK_EUNKNOWN",
    SymlinkEscapeOut => "SYMLINK_ESCAPE_OUT",
    NonRegularFile => "NON_REGULAR_FILE",
    WalkNameEncoding => "WALK_NAME_ENCODING",
    NfcTwinCollision => "NFC_TWIN_COLLISION",
    TagReadDeferred => "TAG_READ_DEFERRED",
    TagReadFailed => "TAG_READ_FAILED",
    MtimeSkew => "MTIME_SKEW",
    SupersededPolicyChanged => "SUPERSEDED_POLICY_CHANGED",
    UnexpectedWorkerFailure => "UNEXPECTED_WORKER_FAILURE",
    MassMissingGuard => "MASS_MISSING_GUARD",
    CatalogCommitFailed => "CATALOG_COMMIT_FAILED",
    RunEndPending => "RUN_END_PENDING",
});

impl rusqlite::types::ToSql for FailureCode {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl std::fmt::Display for FailureCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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

    /// The pause, resume and stop routes promise a refusal (409) for an
    /// ended run, and the store answers it from `transition`.
    #[test]
    fn an_ended_run_takes_no_control() {
        for outcome in [
            Outcome::Completed,
            Outcome::Cancelled,
            Outcome::SupersededPolicyChanged,
            Outcome::Failed,
        ] {
            for reached in [None, Some(Phase::Discovering), Some(Phase::Reconciling)] {
                let from = RunState::Ended { outcome, reached };
                for event in [Event::Pause, Event::Resume, Event::Stop] {
                    assert_eq!(transition(from, event), Err(Illegal { from, event }));
                }
            }
        }
    }
}
