//! Durable management jobs over the tables v2 left in the baseline.
//!
//! A job is a `library_operation_jobs` row of kind `library_management`
//! plus a `library_management_job_snapshots` row with what it pinned.
//! Planned files live in `library_management_plan_items`; applying adds
//! one `library_operation_work` row per album (`ordinal` is the album's
//! bundle ordinal). The job and work rows are written through
//! [`crate::jobs`]; this file holds the snapshot and plan SQL. Migrated v2
//! history reads through the same queries. Plain SQL on a caller's
//! connection; the service owns the transactions.

use crate::management::sql::OverrideSubject;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::after_apply::RefreshTarget;
use super::models::{
    DesiredDocument, Eligibility, HistoryFilter, HistoryItem, ItemDiff, ItemFacts, ItemFilter,
    ItemPage, ItemScope, JobIntent, JobMode, Origin, PlannedItem, ResultItem, ResultPage,
    ResultState, Revisions, Summary, TagScope,
};
use super::reasons::{self, ItemReason};
use super::sql::{
    self, AppliedBundle, ItemRow, LiveUndo, RefreshJobs, SnapshotRow, TrackStateRow, UndoCountsRow,
    WorkOutcome, WritableBundle,
};
use crate::clock::at_secs;
use crate::jobs::{
    self, ManagementPhase, NewJob as NewJobRow, NewWork, OperationJob, OperationKind,
    OperationState, WorkState,
};
use crate::publish::snapshots::sha256_hex;
use droppedneedle_kernel::page::TimeKey;

pub const KIND: &str = OperationKind::LibraryManagement.as_str();

/// A job's snapshot row with its operation.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub job: OperationJob,
    pub mode: String,
    pub origin: String,
    pub phase: ManagementPhase,
    pub selection_json: String,
    pub profile_revision: String,
    pub settings_revision: String,
    pub naming_revision: String,
    pub policy_revision: String,
    pub catalog_revision: i64,
    pub profile_snapshot_json: String,
    pub preview_created_at: Option<f64>,
    pub preview_expires_at: Option<f64>,
    pub target_root_id: Option<String>,
    pub linked_job_id: Option<String>,
    pub intent_json: String,
    pub summary_json: String,
    pub idempotency_key: Option<String>,
    pub lease_expires_at: Option<f64>,
    pub heartbeat_at: Option<f64>,
    /// For an activation dry run: the settings it was made for.
    pub proposed_settings_revision: Option<String>,
}

impl Snapshot {
    pub fn summary(&self) -> Summary {
        serde_json::from_str(&self.summary_json).unwrap_or_default()
    }

    /// What the job carries besides its selection; v2 rows read as empty.
    pub fn intent(&self) -> JobIntent {
        serde_json::from_str(&self.intent_json).unwrap_or_default()
    }

    pub fn job_mode(&self) -> Option<JobMode> {
        JobMode::parse(&self.mode)
    }

    /// The pinned profile's id and name, from v3 or v2 snapshots alike.
    pub fn profile_label(&self) -> (String, String) {
        let value: serde_json::Value =
            serde_json::from_str(&self.profile_snapshot_json).unwrap_or_default();
        let text = |pointer: &str| {
            value
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        (text("/profile/id"), text("/profile/name"))
    }
}

fn map_snapshot(conn: &Connection, row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Snapshot>> {
    let row = SnapshotRow::read(row)?;
    let Some(job) = jobs::job(conn, &row.id)? else {
        return Ok(None);
    };
    Ok(Some(Snapshot {
        job,
        mode: row.mode,
        origin: row.origin,
        phase: row.phase,
        selection_json: row.selection_json,
        profile_revision: row.profile_revision,
        settings_revision: row.settings_revision,
        naming_revision: row.naming_revision,
        policy_revision: row.policy_revision,
        catalog_revision: row.catalog_revision,
        profile_snapshot_json: row.profile_snapshot_json,
        preview_created_at: row.preview_created_at.map(at_secs),
        preview_expires_at: row.preview_expires_at.map(at_secs),
        target_root_id: row.target_root_id,
        linked_job_id: row.linked_operation_job_id,
        intent_json: row.intent_json,
        summary_json: row.summary_json,
        idempotency_key: row.idempotency_key,
        lease_expires_at: row.lease_expires_at.map(at_secs),
        heartbeat_at: row.heartbeat_at.map(at_secs),
        proposed_settings_revision: row.proposed_settings_revision,
    }))
}

/// One management job, or `None` when the id names no management job.
pub fn snapshot(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<Snapshot>> {
    let mut statement = conn.prepare_cached(crate::management::sql::SNAPSHOT.text())?;
    let mut rows = statement.query(params![job_id, KIND])?;
    match rows.next()? {
        Some(row) => map_snapshot(conn, row),
        None => Ok(None),
    }
}

/// The job an idempotency key already made, if any.
pub fn job_for_key(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    Ok(jobs::job_by_key(conn, key)?.map(|job| job.id))
}

/// A job to create.
pub struct NewJob<'a> {
    pub id: &'a str,
    pub requested_by: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
    pub mode: &'a str,
    pub selection_json: &'a str,
    pub profile_snapshot_json: &'a str,
    pub revisions: &'a Revisions,
    pub catalog_revision: i64,
    pub now: f64,
    pub expires_at: f64,
    pub target_root_id: Option<&'a str>,
    pub linked_job_id: Option<&'a str>,
    pub summary: &'a Summary,
    pub origin: Origin,
    pub intent: &'a JobIntent,
    pub proposed_settings_revision: Option<&'a str>,
}

/// Create a queued job in its planning phase.
pub fn insert_job(
    tx: &Transaction<'_>,
    job: &NewJob<'_>,
) -> Result<(), super::models::ManagementError> {
    jobs::insert(
        tx,
        &NewJobRow {
            id: job.id,
            kind: OperationKind::LibraryManagement,
            requested_by: job.requested_by,
            idempotency_key: job.idempotency_key,
            catalog_revision: Some(job.catalog_revision),
            expected_work_count: 0,
            now: job.now,
        },
    )?;
    tx.execute(
        crate::management::sql::INSERT_JOB.text(),
        params![
            job.id,
            job.mode,
            job.selection_json,
            job.revisions.profile,
            job.revisions.settings,
            job.revisions.naming,
            job.revisions.policy,
            job.catalog_revision,
            job.profile_snapshot_json,
            crate::clock::ms(job.now),
            crate::clock::ms(job.expires_at),
            job.target_root_id,
            job.linked_job_id,
            serde_json::to_string(&JobIntent {
                overrides_revision: job.revisions.overrides.clone(),
                ..job.intent.clone()
            })?,
            serde_json::to_string(job.summary)?,
            job.origin.as_str(),
            job.proposed_settings_revision,
            ManagementPhase::Planning,
        ],
    )?;
    Ok(())
}

/// A job's phase, or `None` when the id names no management job.
pub fn phase(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<ManagementPhase>> {
    conn.query_row(
        crate::management::sql::PHASE.text(),
        params![job_id],
        |row| row.get(0),
    )
    .optional()
}

/// Move a job to `phase`.
fn set_phase(
    tx: &Transaction<'_>,
    job_id: &str,
    phase: ManagementPhase,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        crate::management::sql::SET_PHASE.text(),
        params![job_id, phase, crate::clock::ms(now)],
    )?;
    Ok(())
}

/// Drop planned files before (re)planning.
pub fn clear_items(conn: &Connection, job_id: &str) -> rusqlite::Result<()> {
    conn.execute(crate::management::sql::CLEAR_ITEMS.text(), params![job_id])?;
    Ok(())
}

/// Store one album's planned files.
pub fn insert_items(
    tx: &Transaction<'_>,
    job_id: &str,
    snapshot: &Snapshot,
    items: &[PlannedItem],
    now: f64,
) -> Result<(), super::models::ManagementError> {
    let mut statement = tx.prepare_cached(crate::management::sql::INSERT_ITEMS.text())?;
    for item in items {
        let plan = item.desired.plan.as_ref();
        let positive = |value: u64| i64::try_from(value).ok().filter(|value| *value >= 1);
        let desired = serde_json::to_string(&item.desired)?;
        statement.execute(params![
            job_id,
            item.ordinal,
            item.bundle_ordinal,
            item.album_id,
            item.track_id,
            (item.album_revision >= 1).then_some(item.album_revision),
            plan.and_then(|plan| positive(plan.identity.mapping_revision)),
            plan.and_then(|plan| positive(plan.identity.album_identity_revision)),
            plan.map(|plan| plan.override_revision.to_string()),
            snapshot.catalog_revision,
            snapshot.policy_revision,
            snapshot.profile_revision,
            item.source_root,
            item.source_rel,
            plan.map(|plan| plan.fingerprint.size.to_string())
                .unwrap_or_default(),
            item.fingerprint,
            format!("{}/{}", item.source_root, item.source_rel),
            item.dest_root,
            item.dest_rel,
            desired,
            sha256_hex(desired.as_bytes()),
            serde_json::to_string(&item.diff)?,
            serde_json::to_string(&item.facts)?,
            item.eligibility.as_str(),
            item.reason().map(|reason| reason.code.clone()),
            i64::try_from(item.estimated_bytes).unwrap_or(i64::MAX),
            crate::clock::ms(now),
        ])?;
    }
    Ok(())
}

/// Planning is done: the preview waits for apply.
pub fn finish_planning(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    summary: &Summary,
    now: f64,
) -> Result<bool, super::models::ManagementError> {
    if !jobs::park_ready(tx, job_id, worker, now)? {
        return Ok(false);
    }
    tx.execute(
        crate::management::sql::FINISH_PLANNING.text(),
        params![
            job_id,
            serde_json::to_string(summary)?,
            crate::clock::ms(now),
            ManagementPhase::Ready
        ],
    )?;
    Ok(true)
}

fn map_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlannedItem> {
    let row = ItemRow::read(row)?;
    let desired: DesiredDocument =
        serde_json::from_str(&row.desired_document_json).unwrap_or_default();
    let diff: ItemDiff = serde_json::from_str(&row.diff_json).unwrap_or_default();
    let mut facts: ItemFacts = serde_json::from_str(&row.capability_json).unwrap_or_default();
    if facts.reason.is_none()
        && let Some(code) = row.reason_code
    {
        // v2 rows carry only the code; the pages label it.
        facts.reason = Some(ItemReason {
            message: String::new(),
            action: String::new(),
            code,
        });
    }
    Ok(PlannedItem {
        ordinal: row.ordinal,
        bundle_ordinal: row.bundle_ordinal,
        album_id: row.local_album_id.unwrap_or_default(),
        album_revision: row.expected_album_revision.unwrap_or(0),
        track_id: row.local_track_id,
        source_root: row.expected_root_id,
        source_rel: row.expected_relative_path,
        dest_root: row.destination_root_id,
        dest_rel: row.destination_relative_path,
        eligibility: row.eligibility,
        estimated_bytes: u64::try_from(row.estimated_temporary_bytes).unwrap_or(0),
        fingerprint: row.expected_file_fingerprint,
        desired,
        diff,
        facts,
    })
}

/// One page of planned files after `after_ordinal`.
pub fn items_page(
    conn: &Connection,
    job_id: &str,
    after_ordinal: i64,
    limit: i64,
    filter: &ItemFilter,
) -> rusqlite::Result<ItemPage> {
    let change = filter.change_kind.as_deref();
    let mut statement = conn.prepare_cached(crate::management::sql::ITEMS_PAGE.text())?;
    let rows = statement.query_map(
        params![
            job_id,
            after_ordinal,
            filter.eligibility.map(Eligibility::as_str),
            filter.reason_code,
            filter.root_id,
            filter.album_id,
            filter.audio_format,
            change,
            limit + 1,
            filter.scope.map(ItemScope::as_str),
            filter.artist_id,
        ],
        map_item,
    )?;
    let mut items = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = items.len() as i64 > limit;
    items.truncate(usize::try_from(limit).unwrap_or(0));
    Ok(ItemPage {
        next_after_ordinal: if has_more {
            items.last().map(|item| item.ordinal)
        } else {
            None
        },
        has_more,
        items,
    })
}

/// Every planned file of one album bundle.
pub fn bundle_items(
    conn: &Connection,
    job_id: &str,
    bundle_ordinal: i64,
) -> rusqlite::Result<Vec<PlannedItem>> {
    let mut statement = conn.prepare_cached(crate::management::sql::BUNDLE_ITEMS.text())?;
    let rows = statement.query_map(params![job_id, bundle_ordinal], map_item)?;
    rows.collect()
}

/// Album bundles of a ready job that have something to write: ordinal,
/// album id and album revision.
pub fn writable_bundles(
    conn: &Connection,
    job_id: &str,
) -> rusqlite::Result<Vec<(i64, String, i64)>> {
    let mut statement = conn.prepare(crate::management::sql::WRITABLE_BUNDLES.text())?;
    let rows = statement.query_map(params![job_id], WritableBundle::read)?;
    rows.map(|row| row.map(|row| (row.bundle_ordinal, row.local_album_id, row.album_revision)))
        .collect()
}

/// Turn a ready preview into queued apply work, one row per album.
pub fn begin_apply(
    tx: &Transaction<'_>,
    job_id: &str,
    mode: &str,
    phase: ManagementPhase,
    idempotency_key: &str,
    bundles: &[(i64, String, i64)],
    now: f64,
) -> rusqlite::Result<()> {
    let input = sha256_hex(job_id.as_bytes());
    for (ordinal, album_id, revision) in bundles {
        jobs::insert_work(
            tx,
            &NewWork {
                job_id,
                ordinal: *ordinal,
                local_album_id: Some(album_id),
                local_track_id: None,
                expected_subject_revision: *revision,
                expected_input_revision: &input,
                action: "manage_album",
                idempotency_key: &format!("management:{job_id}:{ordinal}"),
                now,
            },
        )?;
    }
    tx.execute(
        crate::management::sql::BEGIN_APPLY.text(),
        params![job_id, mode, phase, idempotency_key, crate::clock::ms(now)],
    )?;
    jobs::queue_work(tx, job_id, bundles.len() as i64, now)
}

/// Record one album's outcome and count it.
pub fn finish_work(
    tx: &Transaction<'_>,
    job_id: &str,
    ordinal: i64,
    failure: Option<&ItemReason>,
    result: &serde_json::Value,
    now: f64,
) -> rusqlite::Result<()> {
    let state = if failure.is_some() {
        WorkState::Failed
    } else {
        WorkState::Succeeded
    };
    jobs::finish_work(
        tx,
        job_id,
        ordinal,
        state,
        failure.map(|reason| reason.code.as_str()),
        Some(&result.to_string()),
        now,
    )
}

/// All work is done: the job ends, failed when any album failed. False
/// when `worker` no longer held it.
pub fn finish_apply(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let failed = jobs::job(tx, job_id)?.is_some_and(|job| job.failed_count > 0);
    let (state, code) = if failed {
        (OperationState::Failed, "MANAGEMENT_PARTIAL")
    } else {
        (OperationState::Succeeded, "MANAGEMENT_APPLIED")
    };
    let finished = jobs::finish(tx, job_id, worker, state, code, jobs::Tally::AsCounted, now)?;
    if finished {
        set_phase(tx, job_id, ManagementPhase::Complete, now)?;
    }
    Ok(finished)
}

/// The request key a job was applied with, if any.
pub fn apply_key(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        crate::management::sql::APPLY_KEY.text(),
        params![job_id],
        |row| row.get(0),
    )
    .optional()
    .map(Option::flatten)
}

/// Remember the request key a job was applied with.
pub fn record_apply_key(tx: &Transaction<'_>, job_id: &str, key: &str) -> rusqlite::Result<()> {
    tx.execute(
        crate::management::sql::RECORD_APPLY_KEY.text(),
        params![job_id, key],
    )?;
    Ok(())
}

/// Note the jobs-table row a server's rescan was queued as for a run.
pub fn record_refresh_job(
    tx: &Transaction<'_>,
    job_id: &str,
    target: RefreshTarget,
    queued: i64,
) -> rusqlite::Result<()> {
    let sql = match target {
        RefreshTarget::Jellyfin => crate::management::sql::SET_JELLYFIN_REFRESH_JOB.text(),
        RefreshTarget::Plex => crate::management::sql::SET_PLEX_REFRESH_JOB.text(),
        RefreshTarget::Navidrome => crate::management::sql::SET_NAVIDROME_REFRESH_JOB.text(),
    };
    tx.execute(sql, params![job_id, queued])?;
    Ok(())
}

/// The rescans a run queued, by server.
pub fn refresh_jobs(
    conn: &Connection,
    job_id: &str,
) -> rusqlite::Result<Vec<(RefreshTarget, i64)>> {
    let ids = conn
        .query_row(
            crate::management::sql::REFRESH_JOBS.text(),
            params![job_id],
            RefreshJobs::read,
        )
        .optional()?
        .map(|row| {
            [
                row.jellyfin_refresh_job_id,
                row.plex_refresh_job_id,
                row.navidrome_refresh_job_id,
            ]
        });
    let targets = [
        RefreshTarget::Jellyfin,
        RefreshTarget::Plex,
        RefreshTarget::Navidrome,
    ];
    Ok(ids
        .map(|ids| {
            targets
                .into_iter()
                .zip(ids)
                .filter_map(|(target, id)| Some((target, id?)))
                .collect()
        })
        .unwrap_or_default())
}

/// Change a job's mode.
pub fn set_mode(tx: &Transaction<'_>, job_id: &str, mode: &str) -> rusqlite::Result<()> {
    tx.execute(
        crate::management::sql::SET_MODE.text(),
        params![job_id, mode],
    )?;
    Ok(())
}

/// The album bundles a job planned.
pub fn bundle_ordinals(conn: &Connection, job_id: &str) -> rusqlite::Result<Vec<i64>> {
    let mut statement = conn.prepare(crate::management::sql::BUNDLE_ORDINALS.text())?;
    let rows = statement.query_map(params![job_id], |row| row.get(0))?;
    rows.collect()
}

/// End a ready job that has nothing to write, as done.
pub fn complete_without_work(
    tx: &Transaction<'_>,
    job_id: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    jobs::complete_ready(tx, job_id, code, now)?;
    set_phase(tx, job_id, ManagementPhase::Complete, now)
}

/// End `worker`'s running job: it cannot go on.
pub fn fail_job(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    jobs::finish(
        tx,
        job_id,
        worker,
        OperationState::Failed,
        code,
        jobs::Tally::AsCounted,
        now,
    )
    .map(drop)
}

/// Set a ready preview aside. Only a ready preview can be discarded.
pub fn discard(
    tx: &Transaction<'_>,
    job_id: &str,
    expected_revision: i64,
    now: f64,
) -> rusqlite::Result<bool> {
    jobs::discard_preview(tx, job_id, expected_revision, now)
}

/// Result rows of an applied job, with each album's work outcome.
pub fn results(
    conn: &Connection,
    job_id: &str,
    after_ordinal: i64,
    limit: i64,
    scope: Option<ItemScope>,
) -> rusqlite::Result<ResultPage> {
    let filter = ItemFilter {
        scope,
        ..ItemFilter::default()
    };
    let page = items_page(conn, job_id, after_ordinal, limit, &filter)?;
    let mut work = conn.prepare_cached(sql::WORK_RESULT.text())?;
    let mut items = Vec::with_capacity(page.items.len());
    for plan in page.items {
        let row = work
            .query_row(params![job_id, plan.bundle_ordinal], WorkOutcome::read)
            .optional()?;
        let (work_state, failure, result) = match row {
            Some(WorkOutcome {
                state,
                failure_code: code,
                result_json,
                bundle_id,
            }) => {
                let mut result: serde_json::Value = result_json
                    .as_deref()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or(serde_json::Value::Null);
                // The bundle is shown with the album's outcome.
                if let Some(bundle_id) = bundle_id {
                    if result.is_null() {
                        result = serde_json::json!({});
                    }
                    if let Some(object) = result.as_object_mut() {
                        object.insert("bundle_id".to_owned(), bundle_id.into());
                    }
                }
                let failure = result
                    .get("reason")
                    .and_then(|value| serde_json::from_value::<ItemReason>(value.clone()).ok())
                    .or_else(|| {
                        code.map(|code| ItemReason {
                            code,
                            message: String::new(),
                            action: String::new(),
                        })
                    });
                (state.into(), failure, result)
            }
            None => {
                let failure = if plan.eligibility.writes() {
                    plan.diff.changes_anything().then(reasons::not_scheduled)
                } else {
                    plan.reason().cloned()
                };
                (ResultState::NotScheduled, failure, serde_json::Value::Null)
            }
        };
        items.push(ResultItem {
            plan,
            work_state,
            failure,
            result,
        });
    }
    Ok(ResultPage {
        items,
        next_after_ordinal: page.next_after_ordinal,
        has_more: page.has_more,
    })
}

/// An album's revision and its indexed files now.
pub fn album_state(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<(Option<i64>, Vec<String>)> {
    let revision = conn
        .query_row(
            crate::management::sql::ALBUM_REVISION.text(),
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let mut statement = conn.prepare_cached(crate::management::sql::ALBUM_INDEXED_TRACKS.text())?;
    let tracks = statement
        .query_map(params![album_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok((revision, tracks))
}

/// Undo jobs made for `job_id` that still count: not failed, discarded,
/// stopped, or a preview left to expire.
pub fn live_undos(
    conn: &Connection,
    job_id: &str,
    now: f64,
) -> rusqlite::Result<Vec<(OperationState, ManagementPhase)>> {
    let mut statement = conn.prepare(crate::management::sql::LIVE_UNDOS.text())?;
    let rows = statement.query_map(
        params![
            job_id,
            crate::clock::ms(now),
            OperationState::Failed,
            OperationState::Cancelled,
            OperationState::Stopped,
            ManagementPhase::Planning,
            ManagementPhase::Ready,
        ],
        LiveUndo::read,
    )?;
    rows.map(|row| row.map(|row| (row.state, row.phase)))
        .collect()
}

/// Albums of `job_id` an earlier undo already put back (bundle ordinals).
pub fn undone_ordinals(
    conn: &Connection,
    job_id: &str,
) -> rusqlite::Result<std::collections::HashSet<i64>> {
    let mut statement = conn.prepare(crate::management::sql::UNDONE_ORDINALS.text())?;
    let rows = statement.query_map(params![job_id, WorkState::Succeeded], |row| row.get(0))?;
    rows.collect()
}

/// Published bundles of an applied job: (bundle ordinal, bundle id).
/// An album whose publish recovery finished after the job stopped (a
/// crash, then a stop) counts too: its bundle committed.
pub fn applied_bundles(conn: &Connection, job_id: &str) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut statement = conn.prepare(sql::APPLIED_BUNDLES.text())?;
    let rows = statement.query_map(params![job_id, WorkState::Succeeded], AppliedBundle::read)?;
    rows.map(|row| row.map(|row| (row.ordinal, row.bundle_id)))
        .collect()
}

/// How much of a job's apply can still be undone.
#[derive(Debug, Clone, Copy, Default)]
pub struct UndoCounts {
    pub available: i64,
    pub expired: i64,
    /// The day the first remaining undo runs out.
    pub first_expiry_day: Option<i64>,
}

/// Files of a job that can still be undone today, files whose undo ran
/// out (their before-state was purged, or is past its day), and when the
/// next one runs out.
pub fn undo_counts(conn: &Connection, job_id: &str, today: i64) -> rusqlite::Result<UndoCounts> {
    conn.query_row(
        sql::UNDO_COUNTS.text(),
        params![job_id, today * crate::clock::DAY_MS, WorkState::Succeeded],
        UndoCountsRow::read,
    )
    .map(|row| UndoCounts {
        available: row.available,
        expired: row.expired,
        first_expiry_day: row.first_expiry_day,
    })
}

/// Files of a job with a saved original a baseline restore could use.
pub fn baseline_available(conn: &Connection, job_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        crate::management::sql::BASELINE_AVAILABLE.text(),
        params![job_id],
        |row| row.get(0),
    )
}

/// One page of management history, newest first.
pub fn history(
    conn: &Connection,
    filter: &HistoryFilter,
) -> rusqlite::Result<(Vec<HistoryItem>, Option<TimeKey>)> {
    let (cursor_at, cursor_id) = filter
        .cursor
        .clone()
        .map_or((None, None), |(at, id)| (Some(at), Some(id)));
    let mut statement = conn.prepare(crate::management::sql::HISTORY.text())?;
    let mut rows = statement.query(params![
        KIND,
        filter.origin,
        filter.root_id,
        filter.state,
        filter.mode,
        filter.created_from.map(crate::clock::ms),
        filter.created_to.map(crate::clock::ms),
        filter.profile_id,
        cursor_at.map(crate::clock::ms),
        cursor_id,
        filter.limit + 1,
    ])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let Some(snapshot) = map_snapshot(conn, row)? else {
            continue;
        };
        let (profile_id, profile_name) = snapshot.profile_label();
        items.push(HistoryItem {
            summary: snapshot.summary(),
            selection: serde_json::from_str(&snapshot.selection_json).unwrap_or_default(),
            mode: snapshot.mode,
            origin: snapshot.origin,
            phase: snapshot.phase,
            profile_id,
            profile_name,
            profile_revision: snapshot.profile_revision,
            target_root_id: snapshot.target_root_id,
            expires_at: snapshot.preview_expires_at,
            job: snapshot.job,
        });
    }
    let more = items.len() as i64 > filter.limit;
    items.truncate(usize::try_from(filter.limit).unwrap_or(0));
    let next = if more {
        items
            .last()
            .map(|item| (item.job.created_at, item.job.id.clone()))
    } else {
        None
    };
    Ok((items, next))
}

// ---------------------------------------------------------------------
// After an album is written.
// ---------------------------------------------------------------------

/// What a profile run left on one file, so automatic management can tell
/// a file it already manages from one it has not seen.
pub struct ManagedMark<'a> {
    pub track_id: &'a str,
    pub root_id: &'a str,
    pub profile_id: &'a str,
    pub profile_revision: &'a str,
    pub naming_revision: &'a str,
    pub override_revision: u64,
    pub job_id: &'a str,
    /// The album's and the file's match revisions the run wrote from.
    pub album_identity_revision: Option<u64>,
    pub track_identity_revision: Option<u64>,
    /// The release and release track the run wrote from.
    pub release_mbid: Option<&'a str>,
    pub release_track_mbid: Option<&'a str>,
    /// The file's place and stat signature after the run.
    pub path_revision: Option<String>,
    /// The file's SHA-256 as the plan read it, for a file the run left as
    /// it found it. `None` for a file the run published: the commit
    /// recorded the bytes it placed, and the plan's hash is of the bytes
    /// before the write.
    pub found_sha256: Option<&'a str>,
    /// The plan could not read an outside source for the file (genres,
    /// lyrics, a cover, AcoustID): it is recorded `managed`, not
    /// `succeeded`, so it does not count as organized.
    pub sources_deferred: bool,
}

/// Record that a profile run wrote (or left as it wanted) one file.
pub fn mark_managed(
    tx: &Transaction<'_>,
    mark: &ManagedMark<'_>,
    now: f64,
) -> rusqlite::Result<()> {
    let revision = |value: Option<u64>| value.and_then(|value| i64::try_from(value).ok());
    tx.execute(
        crate::management::sql::WRITE_MANAGED_STATE.text(),
        params![
            mark.track_id,
            mark.profile_id,
            mark.profile_revision,
            mark.naming_revision,
            mark.override_revision.to_string(),
            mark.job_id,
            mark.root_id,
            crate::clock::ms(now),
            mark.path_revision,
            mark.found_sha256,
            if mark.sources_deferred {
                "managed"
            } else {
                "succeeded"
            },
        ],
    )?;
    tx.execute(
        crate::management::sql::WRITE_MANAGED_IDENTITY.text(),
        params![
            mark.track_id,
            revision(mark.album_identity_revision),
            revision(mark.track_identity_revision),
            mark.release_mbid,
            mark.release_track_mbid,
        ],
    )?;
    Ok(())
}

/// Record that a baseline restore put one file back to its original:
/// automatic management leaves it alone until it is activated again.
pub fn mark_restored(
    tx: &Transaction<'_>,
    track_id: &str,
    job_id: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        crate::management::sql::WRITE_RESTORED_STATE.text(),
        params![track_id, job_id, crate::clock::ms(now)],
    )?;
    tx.execute(
        crate::management::sql::DROP_MANAGED_IDENTITY.text(),
        params![track_id],
    )?;
    Ok(())
}

/// One file's management state, as automatic management compares it.
#[derive(Debug, Clone, Default)]
pub struct TrackState {
    pub profile_id: Option<String>,
    pub profile_revision: Option<String>,
    pub naming_revision: Option<String>,
    pub override_revision: Option<String>,
    pub root_id: Option<String>,
    pub outcome: Option<String>,
    pub managed_at: Option<f64>,
    pub album_identity_revision: Option<i64>,
    pub track_identity_revision: Option<i64>,
    pub path_revision: Option<String>,
    /// The job that last wrote the file.
    pub last_operation_job_id: Option<String>,
    /// The release and release track the file was written from.
    pub applied_release_mbid: Option<String>,
    pub applied_release_track_mbid: Option<String>,
}

impl TrackState {
    /// The last run wrote this file. `last_outcome` has no closed set: the
    /// publisher keeps other text there too, so it stays text.
    pub fn last_run_succeeded(&self) -> bool {
        self.outcome.as_deref() == Some("succeeded")
    }

    /// The last run wrote this file, or it was published and left
    /// `managed` (a source its plan could not read, an undo).
    pub fn last_run_wrote(&self) -> bool {
        self.last_run_succeeded() || self.outcome.as_deref() == Some("managed")
    }

    /// A baseline restore put this file back last.
    pub fn restored(&self) -> bool {
        self.outcome.as_deref() == Some("restored")
    }
}

pub fn track_state(conn: &Connection, track_id: &str) -> rusqlite::Result<Option<TrackState>> {
    conn.query_row(
        crate::management::sql::TRACK_STATE.text(),
        params![track_id],
        TrackStateRow::read,
    )
    .optional()
    .map(|row| {
        row.map(|row| TrackState {
            profile_id: row.applied_profile_id,
            profile_revision: row.applied_profile_revision,
            naming_revision: row.applied_naming_script_revision,
            override_revision: row.applied_override_revision,
            root_id: row.managed_root_id,
            outcome: row.last_outcome,
            managed_at: row.last_managed_at.map(at_secs),
            album_identity_revision: row.applied_album_identity_revision,
            track_identity_revision: row.applied_track_identity_revision,
            path_revision: row.managed_path_revision,
            last_operation_job_id: row.last_operation_job_id,
            applied_release_mbid: row.applied_release_mbid,
            applied_release_track_mbid: row.applied_release_track_mbid,
        })
    })
}

/// Save (or drop) the overrides a tag edit asked to keep. Safe to run
/// twice: an override is found by its subject and field, and its id is
/// derived from the job, so a retry updates the same row.
pub fn save_tag_edit(
    tx: &Transaction<'_>,
    edit: &super::models::TagEditIntent,
    job_id: &str,
    actor: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    let now = crate::clock::ms(now);
    use super::models::TagEditMode;
    if edit.mode == TagEditMode::WriteOnce {
        return Ok(());
    }
    for field in &edit.fields {
        let album = field.subject_kind == TagScope::Album;
        let (subject_id, subject) = if album {
            (&edit.local_album_id, OverrideSubject::Album)
        } else {
            (&edit.local_track_id, OverrideSubject::Track)
        };
        if edit.mode == TagEditMode::ResetCanonical {
            tx.execute(
                &crate::management::sql::drop_override(subject),
                params![field.subject_kind, subject_id, field.field_name],
            )?;
            continue;
        }
        let (mode, value) = match field.values.as_slice() {
            [] => ("clear", serde_json::Value::Null),
            [one] => ("replace", serde_json::Value::String(one.clone())),
            many => ("replace", serde_json::Value::from(many.to_vec())),
        };
        let updated = tx.execute(
            &crate::management::sql::change_override(subject),
            params![
                field.subject_kind,
                subject_id,
                field.field_name,
                value.to_string(),
                mode,
                actor,
                now
            ],
        )?;
        if updated > 0 {
            continue;
        }
        let id = sha256_hex(
            format!(
                "{job_id}\u{0}{}\u{0}{subject_id}\u{0}{}",
                field.subject_kind.as_str(),
                field.field_name
            )
            .as_bytes(),
        );
        tx.execute(
            &crate::management::sql::add_override(subject),
            params![
                &id[..32],
                field.subject_kind,
                subject_id,
                field.field_name,
                value.to_string(),
                mode,
                actor,
                now
            ],
        )?;
    }
    Ok(())
}

/// The override revisions a tag edit left on each file of one album:
/// track -> (after the edit, before it).
pub fn edit_override_revisions(
    conn: &Connection,
    job_id: &str,
    ordinal: i64,
) -> rusqlite::Result<std::collections::HashMap<String, (u64, u64)>> {
    let raw: Option<String> = conn
        .query_row(
            crate::management::sql::EDIT_OVERRIDE_REVISIONS.text(),
            params![job_id, ordinal],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(raw
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default())
}

/// Put the kept values back the way they were before a tag edit, once its
/// undo is written: an override the edit made goes, one it changed or
/// dropped comes back.
pub fn revert_tag_edit(
    tx: &Transaction<'_>,
    edit: &super::models::TagEditIntent,
    job_id: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let now = crate::clock::ms(now);
    use super::models::TagEditMode;
    if edit.mode == TagEditMode::WriteOnce {
        return Ok(());
    }
    for field in &edit.fields {
        let album = field.subject_kind == TagScope::Album;
        let (subject_id, subject) = if album {
            (&edit.local_album_id, OverrideSubject::Album)
        } else {
            (&edit.local_track_id, OverrideSubject::Track)
        };
        match &field.previous {
            None => {
                tx.execute(
                    &crate::management::sql::drop_override(subject),
                    params![field.subject_kind, subject_id, field.field_name],
                )?;
            }
            Some((value, mode)) => {
                let updated = tx.execute(
                    &crate::management::sql::restore_override(subject),
                    params![
                        field.subject_kind,
                        subject_id,
                        field.field_name,
                        value,
                        mode,
                        now
                    ],
                )?;
                if updated == 0 {
                    let id = field.override_id.clone().unwrap_or_else(|| {
                        sha256_hex(format!("{job_id}\u{0}undo\u{0}{}", field.field_name).as_bytes())
                            [..32]
                            .to_owned()
                    });
                    tx.execute(
                        &crate::management::sql::readd_override(subject),
                        params![
                            id,
                            field.subject_kind,
                            subject_id,
                            field.field_name,
                            value,
                            mode,
                            now
                        ],
                    )?;
                }
            }
        }
    }
    Ok(())
}
