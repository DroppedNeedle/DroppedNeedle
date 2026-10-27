//! Durable management jobs over the tables v2 left in the baseline.
//!
//! A job is a `library_operation_jobs` row of kind `library_management`
//! plus a `library_management_job_snapshots` row with what it pinned.
//! Planned files live in `library_management_plan_items`; applying adds
//! one `library_operation_work` row per album (`ordinal` is the album's
//! bundle ordinal, as in v2). The job and work rows are written through
//! [`crate::jobs`]; this file holds the snapshot and plan SQL. Migrated v2
//! history reads through the same queries. Plain SQL on a caller's
//! connection; the service owns the transactions.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::models::{
    DesiredDocument, Eligibility, HistoryFilter, HistoryItem, ItemDiff, ItemFacts, ItemFilter,
    ItemPage, JobIntent, JobMode, Origin, PlannedItem, ResultItem, ResultPage, ResultState,
    Revisions, Summary,
};
use super::reasons::{self, ItemReason};
use super::sql;
use crate::jobs::{
    self, ManagementPhase, NewJob as NewJobRow, NewWork, OperationJob, OperationKind,
    OperationState, WorkState,
};
use crate::publish::snapshots::sha256_hex;

pub use crate::jobs::live_job_sql;

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
    pub token_hash: Option<String>,
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

const SNAPSHOT_COLUMNS: &str = "s.mode, s.origin, s.phase, s.selection_json, s.profile_revision, \
     s.settings_revision, s.naming_revision, s.policy_revision, s.catalog_revision, \
     s.profile_snapshot_json, s.preview_token_hash, s.preview_created_at, s.preview_expires_at, \
     s.target_root_id, s.linked_operation_job_id, s.intent_json, s.summary_json, \
     j.idempotency_key, j.lease_expires_at, j.heartbeat_at, j.id, s.proposed_settings_revision";

fn map_snapshot(conn: &Connection, row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Snapshot>> {
    let job_id: String = row.get(20)?;
    let Some(job) = jobs::job(conn, &job_id)? else {
        return Ok(None);
    };
    Ok(Some(Snapshot {
        job,
        mode: row.get(0)?,
        origin: row.get(1)?,
        phase: row.get(2)?,
        selection_json: row.get(3)?,
        profile_revision: row.get(4)?,
        settings_revision: row.get(5)?,
        naming_revision: row.get(6)?,
        policy_revision: row.get(7)?,
        catalog_revision: row.get(8)?,
        profile_snapshot_json: row.get(9)?,
        token_hash: row.get(10)?,
        preview_created_at: row.get(11)?,
        preview_expires_at: row.get(12)?,
        target_root_id: row.get(13)?,
        linked_job_id: row.get(14)?,
        intent_json: row.get(15)?,
        summary_json: row.get(16)?,
        idempotency_key: row.get(17)?,
        lease_expires_at: row.get(18)?,
        heartbeat_at: row.get(19)?,
        proposed_settings_revision: row.get(21)?,
    }))
}

/// One management job, or `None` when the id names no management job.
pub fn snapshot(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<Snapshot>> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT {SNAPSHOT_COLUMNS} FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id WHERE s.job_id = ?1 AND j.kind = ?2"
    ))?;
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
    pub token_hash: &'a str,
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
        "INSERT INTO library_management_job_snapshots (job_id, mode, origin, phase, \
         selection_json, profile_revision, settings_revision, naming_revision, policy_revision, \
         catalog_revision, profile_snapshot_json, preview_token_hash, preview_created_at, \
         preview_expires_at, target_root_id, linked_operation_job_id, intent_json, summary_json, \
         created_at, updated_at, proposed_settings_revision) \
         VALUES (?1, ?2, ?17, ?19, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
         ?14, ?15, ?16, ?11, ?11, ?18)",
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
            job.token_hash,
            job.now,
            job.expires_at,
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
        "SELECT phase FROM library_management_job_snapshots WHERE job_id = ?1",
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
        "UPDATE library_management_job_snapshots SET phase = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE job_id = ?1",
        params![job_id, phase, now],
    )?;
    Ok(())
}

/// Drop planned files before (re)planning.
pub fn clear_items(conn: &Connection, job_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM library_management_plan_items WHERE job_id = ?1",
        params![job_id],
    )?;
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
    let mut statement = tx.prepare_cached(
        "INSERT INTO library_management_plan_items (job_id, ordinal, bundle_ordinal, \
         local_album_id, local_track_id, expected_album_revision, expected_identity_revision, \
         expected_album_identity_revision, expected_override_revision, expected_catalog_revision, \
         expected_policy_revision, expected_profile_revision, expected_root_id, \
         expected_relative_path, expected_stat_revision, expected_tag_revision, \
         expected_file_fingerprint, source_path_identity, destination_root_id, \
         destination_relative_path, desired_document_json, desired_document_hash, diff_json, \
         capability_json, eligibility, reason_code, estimated_temporary_bytes, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, '', ?16, ?17, \
         ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)",
    )?;
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
            now,
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
        "UPDATE library_management_job_snapshots SET phase = ?4, summary_json = ?2, \
         updated_at = ?3, row_revision = row_revision + 1 WHERE job_id = ?1",
        params![
            job_id,
            serde_json::to_string(summary)?,
            now,
            ManagementPhase::Ready
        ],
    )?;
    Ok(true)
}

const ITEM_COLUMNS: &str = "ordinal, bundle_ordinal, local_album_id, local_track_id, \
     expected_album_revision, expected_root_id, expected_relative_path, destination_root_id, \
     destination_relative_path, eligibility, estimated_temporary_bytes, expected_file_fingerprint, \
     desired_document_json, diff_json, capability_json, reason_code";

fn map_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlannedItem> {
    let parse_or_default = |index: usize| -> rusqlite::Result<String> { row.get(index) };
    let desired: DesiredDocument = serde_json::from_str(&parse_or_default(12)?).unwrap_or_default();
    let diff: ItemDiff = serde_json::from_str(&parse_or_default(13)?).unwrap_or_default();
    let mut facts: ItemFacts = serde_json::from_str(&parse_or_default(14)?).unwrap_or_default();
    let reason_code: Option<String> = row.get(15)?;
    if facts.reason.is_none()
        && let Some(code) = reason_code
    {
        // v2 rows carry only the code; the pages label it.
        facts.reason = Some(ItemReason {
            message: String::new(),
            action: String::new(),
            code,
        });
    }
    Ok(PlannedItem {
        ordinal: row.get(0)?,
        bundle_ordinal: row.get(1)?,
        album_id: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        album_revision: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
        track_id: row.get(3)?,
        source_root: row.get(5)?,
        source_rel: row.get(6)?,
        dest_root: row.get(7)?,
        dest_rel: row.get(8)?,
        eligibility: Eligibility::parse(&row.get::<_, String>(9)?),
        estimated_bytes: u64::try_from(row.get::<_, i64>(10)?).unwrap_or(0),
        fingerprint: row.get(11)?,
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
    let mut statement = conn.prepare_cached(&format!(
        "SELECT {ITEM_COLUMNS} FROM library_management_plan_items WHERE job_id = ?1 \
         AND ordinal > ?2 AND (?3 IS NULL OR eligibility = ?3) \
         AND (?4 IS NULL OR reason_code = ?4) AND (?5 IS NULL OR expected_root_id = ?5) \
         AND (?6 IS NULL OR local_album_id = ?6) \
         AND (?7 IS NULL OR json_extract(capability_json, '$.audio_format') = ?7) \
         AND (?8 IS NULL \
           OR (?8 = 'tags' AND json_extract(diff_json, '$.tags_changed') = 1) \
           OR (?8 = 'path' AND json_extract(diff_json, '$.path_changed') = 1) \
           OR (?8 = 'sidecars' AND json_array_length(diff_json, '$.sidecars') > 0) \
           OR (?8 = 'no_change' AND COALESCE(json_extract(diff_json, '$.tags_changed'), 0) = 0 \
               AND COALESCE(json_extract(diff_json, '$.path_changed'), 0) = 0)) \
         ORDER BY ordinal LIMIT ?9"
    ))?;
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
    let mut statement = conn.prepare_cached(&format!(
        "SELECT {ITEM_COLUMNS} FROM library_management_plan_items \
         WHERE job_id = ?1 AND bundle_ordinal = ?2 ORDER BY ordinal"
    ))?;
    let rows = statement.query_map(params![job_id, bundle_ordinal], map_item)?;
    rows.collect()
}

/// Album bundles of a ready job that have something to write: ordinal,
/// album id and album revision.
pub fn writable_bundles(
    conn: &Connection,
    job_id: &str,
) -> rusqlite::Result<Vec<(i64, String, i64)>> {
    let mut statement = conn.prepare(
        "SELECT bundle_ordinal, MIN(local_album_id), COALESCE(MIN(expected_album_revision), 1) \
         FROM library_management_plan_items WHERE job_id = ?1 \
         GROUP BY bundle_ordinal \
         HAVING SUM(eligibility NOT IN ('eligible', 'warning')) = 0 \
         AND SUM(json_extract(desired_document_json, '$.plan') IS NOT NULL) > 0 \
         ORDER BY bundle_ordinal",
    )?;
    let rows = statement.query_map(params![job_id], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    rows.collect()
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
        "UPDATE library_management_job_snapshots SET mode = ?2, phase = ?3, \
         apply_idempotency_key = ?4, updated_at = ?5, row_revision = row_revision + 1 \
         WHERE job_id = ?1",
        params![job_id, mode, phase, idempotency_key, now],
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
        "SELECT apply_idempotency_key FROM library_management_job_snapshots WHERE job_id = ?1",
        params![job_id],
        |row| row.get(0),
    )
    .optional()
    .map(Option::flatten)
}

/// Remember the request key a job was applied with.
pub fn record_apply_key(tx: &Transaction<'_>, job_id: &str, key: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_management_job_snapshots SET apply_idempotency_key = ?2 \
         WHERE job_id = ?1 AND apply_idempotency_key IS NULL",
        params![job_id, key],
    )?;
    Ok(())
}

/// Change a job's mode.
pub fn set_mode(tx: &Transaction<'_>, job_id: &str, mode: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_management_job_snapshots SET mode = ?2, row_revision = row_revision + 1 \
         WHERE job_id = ?1",
        params![job_id, mode],
    )?;
    Ok(())
}

/// The album bundles a job planned.
pub fn bundle_ordinals(conn: &Connection, job_id: &str) -> rusqlite::Result<Vec<i64>> {
    let mut statement = conn.prepare(
        "SELECT DISTINCT bundle_ordinal FROM library_management_plan_items WHERE job_id = ?1 \
         ORDER BY bundle_ordinal",
    )?;
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

/// Replace the preview token (a lost browser tab asks for a new one).
pub fn rotate_token(
    conn: &Connection,
    job_id: &str,
    token_hash: &str,
    now: f64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE library_management_job_snapshots SET preview_token_hash = ?2, updated_at = ?3 \
         WHERE job_id = ?1",
        params![job_id, token_hash, now],
    )?;
    Ok(())
}

/// One album's work row: state, failure code, result and bundle id.
type WorkRow = (WorkState, Option<String>, Option<String>, Option<String>);

/// Result rows of an applied job, with each album's work outcome.
pub fn results(
    conn: &Connection,
    job_id: &str,
    after_ordinal: i64,
    limit: i64,
) -> rusqlite::Result<ResultPage> {
    let page = items_page(conn, job_id, after_ordinal, limit, &ItemFilter::default())?;
    let mut work = conn.prepare_cached(sql::WORK_RESULT.text())?;
    let mut items = Vec::with_capacity(page.items.len());
    for plan in page.items {
        let row: Option<WorkRow> = work
            .query_row(params![job_id, plan.bundle_ordinal], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .optional()?;
        let (work_state, failure, result) = match row {
            Some((state, code, result, bundle_id)) => {
                let mut result: serde_json::Value = result
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

/// Where in the recycle bin the files a bundle replaced went (or go,
/// while its cleanup is pending).
pub fn replaced_files(conn: &Connection, bundle_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT r.recycle_to FROM library_publish_replacements r \
         JOIN library_publish_journal j ON j.id = r.journal_id \
         WHERE j.bundle_id = ?1 ORDER BY r.journal_id",
    )?;
    let rows = statement.query_map(params![bundle_id], |row| row.get(0))?;
    rows.collect()
}

/// An album's revision and its indexed files now.
pub fn album_state(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<(Option<i64>, Vec<String>)> {
    let revision = conn
        .query_row(
            "SELECT row_revision FROM local_albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let mut statement = conn.prepare_cached(
        "SELECT id FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed'",
    )?;
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
    let mut statement = conn.prepare(
        "SELECT j.state, s.phase FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id \
         WHERE s.linked_operation_job_id = ?1 AND s.mode = 'undo' \
         AND j.state NOT IN (?3, ?4, ?5) \
         AND NOT (s.phase IN (?6, ?7) AND COALESCE(s.preview_expires_at, 0) <= ?2)",
    )?;
    let rows = statement.query_map(
        params![
            job_id,
            now,
            OperationState::Failed,
            OperationState::Cancelled,
            OperationState::Stopped,
            ManagementPhase::Planning,
            ManagementPhase::Ready,
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    rows.collect()
}

/// Albums of `job_id` an earlier undo already put back (bundle ordinals).
pub fn undone_ordinals(
    conn: &Connection,
    job_id: &str,
) -> rusqlite::Result<std::collections::HashSet<i64>> {
    let mut statement = conn.prepare(
        "SELECT w.ordinal FROM library_operation_work w \
         JOIN library_management_job_snapshots s ON s.job_id = w.job_id \
         WHERE s.linked_operation_job_id = ?1 AND s.mode = 'undo' AND w.state = ?2",
    )?;
    let rows = statement.query_map(params![job_id, WorkState::Succeeded], |row| row.get(0))?;
    rows.collect()
}

/// Published bundles of an applied job: (bundle ordinal, bundle id).
/// An album whose publish recovery finished after the job stopped (a
/// crash, then a stop) counts too: its bundle committed.
pub fn applied_bundles(conn: &Connection, job_id: &str) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut statement = conn.prepare(sql::APPLIED_BUNDLES.text())?;
    let rows = statement.query_map(params![job_id, WorkState::Succeeded], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    rows.collect()
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
        params![job_id, today, WorkState::Succeeded],
        |row| {
            Ok(UndoCounts {
                available: row.get(0)?,
                expired: row.get(1)?,
                first_expiry_day: row.get(2)?,
            })
        },
    )
}

/// Files of a job with a saved original a baseline restore could use.
pub fn baseline_available(conn: &Connection, job_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(DISTINCT p.local_track_id) FROM library_management_plan_items p \
         JOIN library_publish_baselines b ON b.track_id = p.local_track_id \
         WHERE p.job_id = ?1",
        params![job_id],
        |row| row.get(0),
    )
}

/// The media-server refreshes an apply asked for, as the pages show them.
pub fn external_refreshes(
    conn: &Connection,
    job_id: &str,
) -> rusqlite::Result<Vec<serde_json::Value>> {
    let mut statement = conn.prepare_cached(
        "SELECT target, state, attempts, max_attempts, failure_code, updated_at, completed_at \
         FROM library_management_external_refresh_deliveries WHERE operation_job_id = ?1 \
         ORDER BY target",
    )?;
    let rows = statement.query_map(params![job_id], |row| {
        Ok(serde_json::json!({
            "target": row.get::<_, String>(0)?,
            "state": row.get::<_, String>(1)?,
            "attempts": row.get::<_, i64>(2)?,
            "max_attempts": row.get::<_, i64>(3)?,
            "failure_code": row.get::<_, Option<String>>(4)?,
            "updated_at": row.get::<_, f64>(5)?,
            "completed_at": row.get::<_, Option<f64>>(6)?,
        }))
    })?;
    rows.collect()
}

/// One page of management history, newest first.
pub fn history(
    conn: &Connection,
    filter: &HistoryFilter,
) -> rusqlite::Result<(Vec<HistoryItem>, Option<String>)> {
    let (cursor_at, cursor_id) = filter
        .cursor
        .clone()
        .map_or((None, None), |(at, id)| (Some(at), Some(id)));
    let mut statement = conn.prepare(&format!(
        "SELECT {SNAPSHOT_COLUMNS} FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id WHERE j.kind = ?1 \
         AND (?2 IS NULL OR s.origin = ?2) AND (?3 IS NULL OR s.target_root_id = ?3 \
           OR s.selection_json LIKE '%' || ?3 || '%') \
         AND (?4 IS NULL OR j.state = ?4) AND (?5 IS NULL OR s.mode = ?5) \
         AND (?6 IS NULL OR j.created_at >= ?6) AND (?7 IS NULL OR j.created_at <= ?7) \
         AND (?8 IS NULL OR json_extract(s.profile_snapshot_json, '$.profile.id') = ?8) \
         AND (?9 IS NULL OR j.created_at < ?9 OR (j.created_at = ?9 AND j.id < ?10)) \
         ORDER BY j.created_at DESC, j.id DESC LIMIT ?11"
    ))?;
    let mut rows = statement.query(params![
        KIND,
        filter.origin,
        filter.root_id,
        filter.state,
        filter.mode,
        filter.created_from,
        filter.created_to,
        filter.profile_id,
        cursor_at,
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
            .map(|item| format!("{}|{}", item.job.created_at, item.job.id))
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
    /// The file's place and stat signature after the run.
    pub path_revision: Option<String>,
    /// The file's SHA-256 as the plan read it, for a file the run left as
    /// it found it. `None` for a file the run published: the commit
    /// recorded the bytes it placed, and the plan's hash is of the bytes
    /// before the write.
    pub found_sha256: Option<&'a str>,
}

/// Record that a profile run wrote (or left as it wanted) one file.
pub fn mark_managed(
    tx: &Transaction<'_>,
    mark: &ManagedMark<'_>,
    now: f64,
) -> rusqlite::Result<()> {
    let revision = |value: Option<u64>| value.and_then(|value| i64::try_from(value).ok());
    tx.execute(
        "INSERT INTO library_track_management_state (local_track_id, applied_profile_id, \
         applied_profile_revision, applied_naming_script_revision, applied_override_revision, \
         last_operation_job_id, managed_root_id, last_managed_at, last_outcome, \
         managed_path_revision, written_sha256) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'succeeded', ?9, ?10) \
         ON CONFLICT (local_track_id) DO UPDATE SET \
         written_sha256 = COALESCE(excluded.written_sha256, \
         library_track_management_state.written_sha256), \
         applied_profile_id = excluded.applied_profile_id, \
         applied_profile_revision = excluded.applied_profile_revision, \
         applied_naming_script_revision = excluded.applied_naming_script_revision, \
         applied_override_revision = excluded.applied_override_revision, \
         last_operation_job_id = excluded.last_operation_job_id, \
         managed_root_id = excluded.managed_root_id, \
         last_managed_at = excluded.last_managed_at, last_outcome = 'succeeded', \
         managed_path_revision = excluded.managed_path_revision, \
         last_reason_code = NULL, \
         row_revision = library_track_management_state.row_revision + 1",
        params![
            mark.track_id,
            mark.profile_id,
            mark.profile_revision,
            mark.naming_revision,
            mark.override_revision.to_string(),
            mark.job_id,
            mark.root_id,
            now,
            mark.path_revision,
            mark.found_sha256,
        ],
    )?;
    tx.execute(
        "INSERT INTO library_track_management_identity (local_track_id, \
         applied_album_identity_revision, applied_track_identity_revision) VALUES (?1, ?2, ?3) \
         ON CONFLICT (local_track_id) DO UPDATE SET \
         applied_album_identity_revision = excluded.applied_album_identity_revision, \
         applied_track_identity_revision = excluded.applied_track_identity_revision",
        params![
            mark.track_id,
            revision(mark.album_identity_revision),
            revision(mark.track_identity_revision),
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
        "INSERT INTO library_track_management_state (local_track_id, last_operation_job_id, \
         last_managed_at, last_outcome) VALUES (?1, ?2, ?3, 'restored') \
         ON CONFLICT (local_track_id) DO UPDATE SET applied_profile_id = NULL, \
         applied_profile_revision = NULL, applied_naming_script_revision = NULL, \
         applied_override_revision = NULL, managed_path_revision = NULL, \
         last_operation_job_id = excluded.last_operation_job_id, \
         last_managed_at = excluded.last_managed_at, last_outcome = 'restored', \
         row_revision = library_track_management_state.row_revision + 1",
        params![track_id, job_id, now],
    )?;
    tx.execute(
        "DELETE FROM library_track_management_identity WHERE local_track_id = ?1",
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
}

pub fn track_state(conn: &Connection, track_id: &str) -> rusqlite::Result<Option<TrackState>> {
    conn.query_row(
        "SELECT s.applied_profile_id, s.applied_profile_revision, \
         s.applied_naming_script_revision, s.applied_override_revision, s.managed_root_id, \
         s.last_outcome, s.last_managed_at, i.applied_album_identity_revision, \
         i.applied_track_identity_revision, s.managed_path_revision \
         FROM library_track_management_state s \
         LEFT JOIN library_track_management_identity i ON i.local_track_id = s.local_track_id \
         WHERE s.local_track_id = ?1",
        params![track_id],
        |row| {
            Ok(TrackState {
                profile_id: row.get(0)?,
                profile_revision: row.get(1)?,
                naming_revision: row.get(2)?,
                override_revision: row.get(3)?,
                root_id: row.get(4)?,
                outcome: row.get(5)?,
                managed_at: row.get(6)?,
                album_identity_revision: row.get(7)?,
                track_identity_revision: row.get(8)?,
                path_revision: row.get(9)?,
            })
        },
    )
    .optional()
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
    use super::models::TagEditMode;
    if edit.mode == TagEditMode::WriteOnce {
        return Ok(());
    }
    for field in &edit.fields {
        let album = field.subject_kind == "album";
        let (subject_id, column, table) = if album {
            (&edit.local_album_id, "local_album_id", "local_albums")
        } else {
            (&edit.local_track_id, "local_track_id", "local_tracks")
        };
        if edit.mode == TagEditMode::ResetCanonical {
            tx.execute(
                &format!(
                    "DELETE FROM library_management_overrides WHERE subject_kind = ?1 \
                     AND {column} = ?2 AND field_name = ?3"
                ),
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
            &format!(
                "UPDATE library_management_overrides SET value_json = ?4, mode = ?5, \
                 actor_user_id = ?6, updated_at = ?7, row_revision = row_revision + 1 \
                 WHERE subject_kind = ?1 AND {column} = ?2 AND field_name = ?3 \
                 AND (value_json <> ?4 OR mode <> ?5)"
            ),
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
                field.subject_kind, field.field_name
            )
            .as_bytes(),
        );
        tx.execute(
            &format!(
                "INSERT OR IGNORE INTO library_management_overrides (id, subject_kind, {column}, \
                 field_name, value_json, mode, actor_user_id, reason, subject_revision, \
                 created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'tag_editor', \
                 COALESCE((SELECT row_revision FROM {table} WHERE id = ?3), 1), ?8, ?8)"
            ),
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
            "SELECT json_extract(result_json, '$.override_revisions') FROM library_operation_work \
             WHERE job_id = ?1 AND ordinal = ?2",
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
    use super::models::TagEditMode;
    if edit.mode == TagEditMode::WriteOnce {
        return Ok(());
    }
    for field in &edit.fields {
        let album = field.subject_kind == "album";
        let (subject_id, column, table) = if album {
            (&edit.local_album_id, "local_album_id", "local_albums")
        } else {
            (&edit.local_track_id, "local_track_id", "local_tracks")
        };
        match &field.previous {
            None => {
                tx.execute(
                    &format!(
                        "DELETE FROM library_management_overrides WHERE subject_kind = ?1 \
                         AND {column} = ?2 AND field_name = ?3"
                    ),
                    params![field.subject_kind, subject_id, field.field_name],
                )?;
            }
            Some((value, mode)) => {
                let updated = tx.execute(
                    &format!(
                        "UPDATE library_management_overrides SET value_json = ?4, mode = ?5, \
                         updated_at = ?6, row_revision = row_revision + 1 \
                         WHERE subject_kind = ?1 AND {column} = ?2 AND field_name = ?3"
                    ),
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
                        &format!(
                            "INSERT OR IGNORE INTO library_management_overrides (id, subject_kind, \
                             {column}, field_name, value_json, mode, reason, subject_revision, \
                             created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'tag_editor', \
                             COALESCE((SELECT row_revision FROM {table} WHERE id = ?3), 1), ?7, ?7)"
                        ),
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

/// Media-server refreshes to send for a finished apply, one per server.
pub fn queue_refreshes(
    tx: &Transaction<'_>,
    job_id: &str,
    targets: &[&str],
    max_attempts: i64,
    retry_delay_seconds: i64,
    now: f64,
) -> rusqlite::Result<()> {
    for target in targets {
        tx.execute(
            "INSERT OR IGNORE INTO library_management_external_refresh_deliveries (id, \
             operation_job_id, target, state, max_attempts, retry_delay_seconds, not_before, \
             created_at, updated_at) VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6 + 15.0, ?6, ?6)",
            params![
                &sha256_hex(format!("{job_id}\u{0}{target}").as_bytes())[..32],
                job_id,
                target,
                max_attempts.clamp(1, 21),
                retry_delay_seconds.clamp(1, 3600),
                now
            ],
        )?;
    }
    Ok(())
}

/// One media-server refresh due now.
#[derive(Debug, Clone)]
pub struct DueRefresh {
    pub id: String,
    pub target: String,
    pub attempts: i64,
    pub max_attempts: i64,
    pub retry_delay_seconds: i64,
}

/// Claim the next refresh due now.
pub fn claim_refresh(
    tx: &Transaction<'_>,
    worker: &str,
    now: f64,
) -> rusqlite::Result<Option<DueRefresh>> {
    let due = tx
        .query_row(
            "SELECT id, target, attempts, max_attempts, retry_delay_seconds \
             FROM library_management_external_refresh_deliveries \
             WHERE (state IN ('pending', 'retry_wait') AND not_before <= ?1) \
             OR (state = 'delivering' AND COALESCE(lease_expires_at, 0) <= ?1) \
             ORDER BY not_before, id LIMIT 1",
            params![now],
            |row| {
                Ok(DueRefresh {
                    id: row.get(0)?,
                    target: row.get(1)?,
                    attempts: row.get(2)?,
                    max_attempts: row.get(3)?,
                    retry_delay_seconds: row.get(4)?,
                })
            },
        )
        .optional()?;
    if let Some(due) = &due {
        tx.execute(
            "UPDATE library_management_external_refresh_deliveries SET state = 'delivering', \
             attempts = MIN(attempts + 1, max_attempts), lease_owner = ?2, \
             lease_expires_at = ?3, updated_at = ?4, row_revision = row_revision + 1 \
             WHERE id = ?1",
            params![due.id, worker, now + 120.0, now],
        )?;
    }
    Ok(due)
}

/// Record how one refresh attempt went: `succeeded`, `unavailable`
/// (the server is not set up), or a failure that waits and retries until
/// its attempts run out.
pub fn finish_refresh(
    tx: &Transaction<'_>,
    due: &DueRefresh,
    outcome: Result<(), &str>,
    now: f64,
) -> rusqlite::Result<()> {
    let attempts = due.attempts + 1;
    let (state, code, not_before, done) = match outcome {
        Ok(()) => ("succeeded", None, now, Some(now)),
        Err("NOT_CONFIGURED") => ("unavailable", Some("NOT_CONFIGURED"), now, Some(now)),
        Err(code) if attempts >= due.max_attempts => ("failed", Some(code), now, Some(now)),
        Err(code) => (
            "retry_wait",
            Some(code),
            now + due.retry_delay_seconds as f64,
            None,
        ),
    };
    tx.execute(
        "UPDATE library_management_external_refresh_deliveries SET state = ?2, \
         failure_code = ?3, not_before = ?4, completed_at = ?5, lease_owner = NULL, \
         lease_expires_at = NULL, updated_at = ?6, row_revision = row_revision + 1 \
         WHERE id = ?1",
        params![due.id, state, code, not_before, done, now],
    )?;
    // One rescan covers every run that ended before it: the others for
    // the same server are done too.
    if state == "succeeded" {
        tx.execute(
            "UPDATE library_management_external_refresh_deliveries SET state = 'succeeded', \
             failure_code = 'COALESCED', completed_at = ?3, updated_at = ?3, \
             row_revision = row_revision + 1 \
             WHERE target = ?2 AND id <> ?1 AND state IN ('pending', 'retry_wait')",
            params![due.id, due.target, now],
        )?;
    }
    Ok(())
}
