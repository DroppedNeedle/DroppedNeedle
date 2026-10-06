//! The library activity feed: what the library is doing right now.
//!
//! A port of v2's `GET /library/activity` assembly. `items` drive the
//! activity strip (one scan card, one identification card); `work_items`
//! drive the work stack, sorted most urgent first. Building the feed is
//! pure: the service reads the runs and queue counts, this decides what
//! the cards say.

use std::collections::BTreeMap;

use serde::Serialize;
use utoipa::ToSchema;

use super::store::{DeferredJob, IdentificationSnapshot};
use crate::library::scan::models::{Counters, ScanPhase, ScanRun, ScanState, counter_names};

/// A failed scan stays on the strip this long after it ended.
const FAILURE_WINDOW_SECS: f64 = 24.0 * 60.0 * 60.0;

/// Card label for scans.
const SCAN_LABEL: &str = "Updating the local library";
/// Card label for identification.
const IDENTIFY_LABEL: &str = "Identifying albums";

/// Work stack id of the synthetic drain card. It never names a run, so
/// run lookups with it answer not-found.
pub const DRAIN_ITEM_ID: &str = "identification-drain";

/// What one activity card describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Scan,
    Identification,
}

/// What kind of work one stack entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Scan,
    Identification,
    IdentityPreparation,
    Reidentification,
    IdentityReview,
    Maintenance,
    LibraryManagement,
    Recovery,
}

/// What the work does to the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkEffect {
    /// Changes only the catalog, never a file.
    CatalogOnly,
    /// Writes music files.
    FileWriting,
    /// Needs someone to look at it.
    Attention,
}

/// What a work entry counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkUnit {
    Files,
    Albums,
    Releases,
    Items,
}

/// One deferred identification job, named.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct DeferredJobView {
    pub job_id: String,
    #[schema(required = true)]
    pub local_album_id: Option<String>,
    #[schema(required = true)]
    pub album_title: Option<String>,
    #[schema(required = true)]
    pub artist_name: Option<String>,
    /// Why the last attempt did not finish.
    pub last_failure_code: String,
    pub attempt_count: u64,
    /// Unix seconds before which the job will not run again.
    #[schema(required = true)]
    pub not_before: Option<f64>,
    pub updated_at: f64,
}

impl From<&DeferredJob> for DeferredJobView {
    fn from(job: &DeferredJob) -> Self {
        Self {
            job_id: job.job_id.clone(),
            local_album_id: job.local_album_id.clone(),
            album_title: job.album_title.clone(),
            artist_name: job.artist_name.clone(),
            last_failure_code: job.last_failure_code.clone(),
            attempt_count: job.attempt_count,
            not_before: job.not_before,
            updated_at: job.updated_at,
        }
    }
}

/// One activity strip card.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ActivityItem {
    pub kind: ActivityKind,
    /// A scan run state, or `running`, `pausing`, `paused`, `idle` or
    /// `failed` for identification.
    pub state: String,
    pub label: String,
    pub processed: u64,
    #[schema(required = true)]
    pub total: Option<u64>,
    /// True when the total is not known yet.
    pub indeterminate: bool,
    pub updated_at: f64,
    #[schema(required = true)]
    pub started_at: Option<f64>,
    pub waiting_count: u64,
    pub identified_count: u64,
    pub kept_local_count: u64,
    pub needs_review_count: u64,
    pub failed_count: u64,
    pub deferred_count: u64,
    pub deferred_reason_counts: BTreeMap<String, u64>,
    pub deferred_jobs: Vec<DeferredJobView>,
    pub attention_count: u64,
    /// The queue band the next job comes from.
    #[schema(required = true)]
    pub priority_band: Option<String>,
    #[schema(required = true)]
    pub oldest_backlog_at: Option<f64>,
    /// True while MusicBrainz is unreachable.
    pub provider_unavailable: bool,
    /// Pause switch revision to send back with pause or resume.
    #[schema(required = true)]
    pub control_revision: Option<u64>,
    #[schema(required = true)]
    pub failure_event_id: Option<String>,
    #[schema(required = true)]
    pub failure_at: Option<f64>,
    pub foreground_operation_count: u64,
}

impl ActivityItem {
    fn new(kind: ActivityKind, state: &str, label: &str) -> Self {
        Self {
            kind,
            state: state.to_owned(),
            label: label.to_owned(),
            processed: 0,
            total: None,
            indeterminate: false,
            updated_at: 0.0,
            started_at: None,
            waiting_count: 0,
            identified_count: 0,
            kept_local_count: 0,
            needs_review_count: 0,
            failed_count: 0,
            deferred_count: 0,
            deferred_reason_counts: BTreeMap::new(),
            deferred_jobs: Vec::new(),
            attention_count: 0,
            priority_band: None,
            oldest_backlog_at: None,
            provider_unavailable: false,
            control_revision: None,
            failure_event_id: None,
            failure_at: None,
            foreground_operation_count: 0,
        }
    }
}

/// One work stack entry.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct WorkItem {
    /// Run id, or a fixed id for queue-wide entries.
    pub id: String,
    pub kind: WorkKind,
    pub state: String,
    #[schema(required = true)]
    pub phase: Option<String>,
    #[schema(required = true)]
    pub mode: Option<String>,
    pub effect: WorkEffect,
    pub processed: u64,
    #[schema(required = true)]
    pub total: Option<u64>,
    pub unit: WorkUnit,
    pub indeterminate: bool,
    #[schema(required = true)]
    pub remaining_count: Option<u64>,
    #[schema(required = true)]
    pub subject_count: Option<u64>,
    #[schema(required = true)]
    pub started_at: Option<f64>,
    pub updated_at: f64,
    #[schema(required = true)]
    pub origin: Option<String>,
    #[schema(required = true)]
    pub profile_name: Option<String>,
    /// Administrators see which part of the library a scan covers.
    #[schema(required = true)]
    pub scope_label: Option<String>,
    pub new_count: u64,
    pub changed_count: u64,
    pub missing_count: u64,
    pub warning_count: u64,
    pub blocked_count: u64,
    pub succeeded_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
    /// Lower sorts first: 0 needs attention, 20 is the running scan.
    pub priority: i64,
    #[schema(required = true)]
    pub failure_event_id: Option<String>,
    #[schema(required = true)]
    pub failure_at: Option<f64>,
    /// True for the drain card, which names no real run.
    pub synthetic: bool,
    /// False while a settled scan still has albums waiting to identify.
    pub catalog_settled: Option<bool>,
    /// Albums still waiting to identify, on the drain card.
    pub pending_identification: Option<u64>,
}

impl WorkItem {
    fn new(id: &str, kind: WorkKind, state: &str, unit: WorkUnit) -> Self {
        Self {
            id: id.to_owned(),
            kind,
            state: state.to_owned(),
            phase: None,
            mode: None,
            effect: WorkEffect::CatalogOnly,
            processed: 0,
            total: None,
            unit,
            indeterminate: false,
            remaining_count: None,
            subject_count: None,
            started_at: None,
            updated_at: 0.0,
            origin: None,
            profile_name: None,
            scope_label: None,
            new_count: 0,
            changed_count: 0,
            missing_count: 0,
            warning_count: 0,
            blocked_count: 0,
            succeeded_count: 0,
            failed_count: 0,
            skipped_count: 0,
            priority: 100,
            failure_event_id: None,
            failure_at: None,
            synthetic: false,
            catalog_settled: None,
            pending_identification: None,
        }
    }
}

/// The whole feed.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LibraryActivity {
    pub items: Vec<ActivityItem>,
    pub work_items: Vec<WorkItem>,
    /// Change revisions per stream (`scan`, `identification`,
    /// `operation`, `catalog`), as the `activity.changed` event sends.
    pub revisions: BTreeMap<String, u64>,
}

/// Everything the feed is built from.
pub struct FeedInputs<'a> {
    /// Current runs (active first), each with its counters.
    pub runs: &'a [(ScanRun, Counters)],
    /// The latest finished run, with its counters.
    pub latest_terminal: Option<&'a (ScanRun, Counters)>,
    pub identification: &'a IdentificationSnapshot,
    pub revisions: BTreeMap<String, u64>,
    pub admin: bool,
    pub provider_unavailable: bool,
    /// Unix seconds.
    pub now: f64,
}

fn counter(counters: &Counters, name: &str) -> Option<u64> {
    counters.get(name).map(|value| (*value).max(0) as u64)
}

/// v2's `total_count or discovered_count`: a zero total falls through.
fn run_total(counters: &Counters) -> Option<u64> {
    counter(counters, counter_names::TOTAL)
        .filter(|total| *total != 0)
        .or_else(|| counter(counters, counter_names::DISCOVERED))
}

fn snake<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn priority_band(priority: i64) -> String {
    match priority {
        20 => "New and changed albums",
        30 => "Administrator retries",
        40 => "Existing-library backlog",
        50 => "Supporting maintenance",
        _ => "Queued work",
    }
    .to_owned()
}

/// The failed scan card and work entry for a run that failed recently.
fn failed_scan(run: &ScanRun, counters: &Counters) -> (ActivityItem, WorkItem) {
    let total = run_total(counters);
    let inspected = counter(counters, counter_names::INSPECTED).unwrap_or(0);
    let failure_at = run.terminal_at.unwrap_or(run.updated_at);
    let mut item = ActivityItem::new(ActivityKind::Scan, "failed", SCAN_LABEL);
    item.processed = inspected;
    item.total = total;
    item.indeterminate = total.unwrap_or(0) == 0;
    item.updated_at = run.updated_at;
    item.started_at = run.started_at;
    item.failure_event_id = Some(run.id.clone());
    item.failure_at = Some(failure_at);
    let mut work = WorkItem::new(&run.id, WorkKind::Scan, "failed", WorkUnit::Files);
    work.phase = Some(snake(&run.phase));
    work.effect = WorkEffect::Attention;
    work.processed = inspected;
    work.total = total;
    work.indeterminate = total.unwrap_or(0) == 0;
    work.started_at = run.started_at;
    work.updated_at = run.updated_at;
    work.failed_count = counter(counters, counter_names::ERRORED).unwrap_or(0);
    work.priority = 0;
    work.failure_event_id = Some(run.id.clone());
    work.failure_at = Some(failure_at);
    (item, work)
}

/// Identification state as the cards spell it.
fn identification_state(snapshot: &IdentificationSnapshot, failed_counts: bool) -> &'static str {
    if snapshot.paused && snapshot.running > 0 {
        "pausing"
    } else if snapshot.paused {
        "paused"
    } else if snapshot.running > 0 || snapshot.claimable > 0 {
        "running"
    } else if failed_counts && snapshot.failure.is_some() {
        "failed"
    } else {
        "idle"
    }
}

/// Assemble the feed (v2 `library_activity`).
pub fn build(inputs: FeedInputs<'_>) -> LibraryActivity {
    let mut items: Vec<ActivityItem> = Vec::new();
    let mut work_items: Vec<WorkItem> = Vec::new();
    let latest_failure = inputs.latest_terminal.filter(|(run, _)| {
        run.state == ScanState::Failed
            && run
                .terminal_at
                .is_some_and(|at| inputs.now - at < FAILURE_WINDOW_SECS)
    });

    for (index, (run, counters)) in inputs.runs.iter().enumerate() {
        let discovering = run.state == ScanState::Discovering;
        let total = if discovering {
            None
        } else {
            run_total(counters)
        };
        let processed = counter(
            counters,
            if discovering {
                counter_names::DISCOVERED
            } else {
                counter_names::INSPECTED
            },
        )
        .unwrap_or(0);
        let indeterminate = discovering || total.unwrap_or(0) == 0;
        if index == 0 {
            let mut item = ActivityItem::new(ActivityKind::Scan, &snake(&run.state), SCAN_LABEL);
            item.processed = processed;
            item.total = total;
            item.indeterminate = indeterminate;
            item.updated_at = run.updated_at;
            item.started_at = run.started_at;
            if let Some((failed, _)) = latest_failure {
                item.failure_event_id = Some(failed.id.clone());
                item.failure_at = failed.terminal_at;
            }
            items.push(item);
        }
        let mut work = WorkItem::new(&run.id, WorkKind::Scan, &snake(&run.state), WorkUnit::Files);
        work.phase = Some(snake(&run.phase));
        work.processed = match (run.phase, total) {
            (ScanPhase::Reconciling, Some(total)) => total,
            _ => processed,
        };
        work.total = total;
        work.indeterminate = indeterminate;
        work.started_at = run.started_at;
        work.updated_at = run.updated_at;
        work.scope_label = inputs.admin.then(|| {
            if run.aggregate_scope == "all" {
                "Whole library".to_owned()
            } else {
                "Selected library scopes".to_owned()
            }
        });
        work.new_count = counter(counters, counter_names::NEW).unwrap_or(0);
        work.changed_count = counter(counters, counter_names::CHANGED).unwrap_or(0);
        work.missing_count = counter(counters, counter_names::MISSING).unwrap_or(0);
        work.failed_count = counter(counters, counter_names::ERRORED).unwrap_or(0);
        work.priority = if index == 0 && run.state != ScanState::Queued {
            20
        } else {
            70
        };
        work_items.push(work);
    }
    if let Some((run, counters)) = latest_failure {
        let (item, work) = failed_scan(run, counters);
        if inputs.runs.is_empty() {
            items.push(item);
        }
        work_items.push(work);
    }

    let ident = inputs.identification;
    let waiting = ident.waiting();
    let failure_id = ident.failure.as_ref().map(|(id, _)| id.clone());
    let failure_at = ident.failure.as_ref().map(|(_, at)| *at);
    let updated_at = ident.updated_at.or(failure_at).unwrap_or(0.0);
    let band = ident.active_priority.map(priority_band);
    if waiting > 0 || ident.foreground_operations > 0 || failure_id.is_some() {
        let state = identification_state(ident, true);
        let total = ident.completed() + waiting;
        let mut item = ActivityItem::new(ActivityKind::Identification, state, IDENTIFY_LABEL);
        item.processed = ident.completed();
        item.total = (total > 0).then_some(total);
        item.indeterminate = total == 0;
        item.updated_at = updated_at;
        item.started_at = ident.started_at;
        item.waiting_count = waiting;
        item.identified_count = ident.identified;
        item.kept_local_count = ident.kept_local;
        item.needs_review_count = ident.needs_review;
        item.failed_count = ident.failed + ident.attention;
        item.deferred_count = ident.deferred_count;
        item.deferred_reason_counts = ident.deferred_reason_counts.clone();
        item.deferred_jobs = ident
            .deferred_jobs
            .iter()
            .map(DeferredJobView::from)
            .collect();
        item.attention_count = ident.attention;
        item.priority_band = band.clone();
        item.oldest_backlog_at = ident.started_at;
        item.provider_unavailable = inputs.provider_unavailable;
        item.control_revision = Some(ident.control_revision);
        item.failure_event_id = failure_id.clone();
        item.failure_at = failure_at;
        item.foreground_operation_count = ident.foreground_operations;
        items.push(item);

        if waiting > 0 || failure_id.is_some() {
            let settled_failure = state == "failed" && waiting == 0;
            let mut work = WorkItem::new(
                "identification",
                WorkKind::Identification,
                state,
                WorkUnit::Albums,
            );
            work.phase = Some("identifying_albums".to_owned());
            work.mode = band.clone();
            work.effect = if settled_failure {
                WorkEffect::Attention
            } else {
                WorkEffect::CatalogOnly
            };
            work.indeterminate = waiting > 0;
            work.remaining_count = Some(waiting);
            work.started_at = ident.started_at;
            work.updated_at = updated_at;
            work.warning_count = ident.deferred_count;
            work.failed_count = ident.failed + ident.attention;
            work.priority = if settled_failure { 0 } else { 90 };
            if settled_failure {
                work.failure_event_id = failure_id.clone();
                work.failure_at = failure_at;
            }
            work_items.push(work);
            if let (true, Some(id)) = (waiting > 0, failure_id.as_ref()) {
                let mut failed = WorkItem::new(
                    &format!("identification-failure:{id}"),
                    WorkKind::Identification,
                    "failed",
                    WorkUnit::Albums,
                );
                failed.phase = Some("identifying_albums".to_owned());
                failed.effect = WorkEffect::Attention;
                failed.indeterminate = true;
                failed.updated_at = failure_at.or(ident.updated_at).unwrap_or(0.0);
                failed.failed_count = ident.failed + ident.attention;
                failed.priority = 0;
                failed.failure_event_id = Some(id.clone());
                failed.failure_at = failure_at;
                work_items.push(failed);
            }
        }
    }
    if inputs.runs.is_empty() && waiting + ident.deferred_count > 0 {
        let mut drain = WorkItem::new(
            DRAIN_ITEM_ID,
            WorkKind::Identification,
            identification_state(ident, false),
            WorkUnit::Albums,
        );
        drain.phase = Some("awaiting_identification".to_owned());
        drain.indeterminate = true;
        drain.remaining_count = Some(waiting);
        drain.started_at = ident.started_at;
        drain.updated_at = updated_at;
        drain.warning_count = ident.deferred_count;
        drain.failed_count = ident.failed + ident.attention;
        drain.priority = 90;
        drain.synthetic = true;
        drain.catalog_settled = Some(false);
        drain.pending_identification = Some(waiting);
        work_items.push(drain);
    }
    work_items.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then(right.updated_at.total_cmp(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    LibraryActivity {
        items,
        work_items,
        revisions: inputs.revisions,
    }
}
