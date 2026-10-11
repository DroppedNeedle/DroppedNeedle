//! Durable management jobs over the tables v2 left in the baseline.
//!
//! A job is a `library_operation_jobs` row of kind `library_management`
//! plus a `library_management_job_snapshots` row with what it pinned.
//! Planned files live in `library_management_plan_items`; applying adds
//! one `library_operation_work` row per album (`ordinal` is the album's
//! bundle ordinal, as in v2). Migrated v2 history reads through the same
//! queries. Plain SQL on a caller's connection; the service owns the
//! transactions.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::models::{
    DesiredDocument, Eligibility, HistoryFilter, HistoryItem, ItemDiff, ItemFacts, ItemFilter,
    ItemPage, PlannedItem, ResultItem, ResultPage, Revisions, Summary,
};
use super::reasons::{self, ItemReason};
use crate::library::operations::models::OperationJob;
use crate::library::operations::store as ops;
use crate::library::operations::store::LEASE_SECS;
use crate::library::publish::snapshots::sha256_hex;

pub const KIND: &str = "library_management";

/// A job's snapshot row with its operation.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub job: OperationJob,
    pub mode: String,
    pub origin: String,
    pub phase: String,
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
}

impl Snapshot {
    pub fn summary(&self) -> Summary {
        serde_json::from_str(&self.summary_json).unwrap_or_default()
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
     j.idempotency_key, j.lease_expires_at, j.heartbeat_at, j.id";

fn map_snapshot(conn: &Connection, row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<Snapshot>> {
    let job_id: String = row.get(20)?;
    let Some(job) = ops::job(conn, &job_id)? else {
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
    conn.query_row(
        "SELECT id FROM library_operation_jobs WHERE idempotency_key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
}

/// A job to create.
pub struct NewJob<'a> {
    pub id: &'a str,
    pub requested_by: &'a str,
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
}

/// Create a queued job in its planning phase.
pub fn insert_job(
    tx: &Transaction<'_>,
    job: &NewJob<'_>,
) -> Result<(), super::models::ManagementError> {
    tx.execute(
        "INSERT INTO library_operation_jobs (id, kind, state, requested_by_user_id, \
         input_catalog_revision, idempotency_key, created_at, updated_at) \
         VALUES (?1, ?2, 'queued', ?3, ?4, ?5, ?6, ?6)",
        params![
            job.id,
            KIND,
            job.requested_by,
            job.catalog_revision,
            job.idempotency_key,
            job.now
        ],
    )?;
    tx.execute(
        "INSERT INTO library_management_job_snapshots (job_id, mode, origin, phase, \
         selection_json, profile_revision, settings_revision, naming_revision, policy_revision, \
         catalog_revision, profile_snapshot_json, preview_token_hash, preview_created_at, \
         preview_expires_at, target_root_id, linked_operation_job_id, intent_json, summary_json, \
         created_at, updated_at) \
         VALUES (?1, ?2, 'manual', 'planning', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
         ?14, ?15, ?16, ?11, ?11)",
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
            serde_json::json!({ "overrides_revision": job.revisions.overrides }).to_string(),
            serde_json::to_string(job.summary)?,
        ],
    )?;
    Ok(())
}

/// Claim the next queued management job for `worker`: its id and phase.
pub fn claim(
    tx: &Transaction<'_>,
    worker: &str,
    now: f64,
) -> rusqlite::Result<Option<(String, String)>> {
    let candidate: Option<(String, String)> = tx
        .query_row(
            "SELECT j.id, s.phase FROM library_operation_jobs j \
             JOIN library_management_job_snapshots s ON s.job_id = j.id \
             WHERE j.kind = ?1 AND j.state = 'queued' \
             AND s.phase IN ('planning', 'applying', 'undoing') \
             ORDER BY j.updated_at, j.created_at, j.id LIMIT 1",
            params![KIND],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((job_id, phase)) = candidate else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'running', \
         started_at = COALESCE(started_at, ?1), lease_owner = ?2, lease_expires_at = ?3, \
         heartbeat_at = ?1, updated_at = ?1, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 WHERE id = ?4 AND state = 'queued'",
        params![now, worker, now + LEASE_SECS, job_id],
    )?;
    Ok(Some((job_id, phase)))
}

/// Keep the lease while a long step runs. False when the job is no longer
/// this worker's.
pub fn heartbeat(
    conn: &Connection,
    job_id: &str,
    worker: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE library_operation_jobs SET heartbeat_at = ?3, lease_expires_at = ?4 \
         WHERE id = ?1 AND lease_owner = ?2 AND state = 'running'",
        params![job_id, worker, now, now + LEASE_SECS],
    )?;
    Ok(changed == 1)
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
    let changed = tx.execute(
        "UPDATE library_operation_jobs SET state = 'ready', lease_owner = NULL, \
         lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?3, \
         expected_work_count = 0, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 \
         WHERE id = ?1 AND lease_owner = ?2 AND state = 'running'",
        params![job_id, worker, now],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    tx.execute(
        "UPDATE library_management_job_snapshots SET phase = 'ready', summary_json = ?2, \
         updated_at = ?3, row_revision = row_revision + 1 WHERE job_id = ?1",
        params![job_id, serde_json::to_string(summary)?, now],
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
    phase: &str,
    idempotency_key: &str,
    bundles: &[(i64, String, i64)],
    now: f64,
) -> rusqlite::Result<()> {
    let input = sha256_hex(job_id.as_bytes());
    for (ordinal, album_id, revision) in bundles {
        tx.execute(
            "INSERT INTO library_operation_work (job_id, ordinal, local_album_id, \
             expected_subject_revision, expected_input_revision, action, idempotency_key, \
             state, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, 'manage_album', ?6, 'pending', ?7)",
            params![
                job_id,
                ordinal,
                album_id,
                revision,
                input,
                format!("management:{job_id}:{ordinal}"),
                now
            ],
        )?;
    }
    tx.execute(
        "UPDATE library_management_job_snapshots SET mode = ?2, phase = ?3, \
         apply_idempotency_key = ?4, updated_at = ?5, row_revision = row_revision + 1 \
         WHERE job_id = ?1",
        params![job_id, mode, phase, idempotency_key, now],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'queued', expected_work_count = ?2, \
         completed_count = 0, succeeded_count = 0, failed_count = 0, skipped_count = 0, \
         terminal_code = NULL, updated_at = ?3, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 WHERE id = ?1",
        params![job_id, bundles.len() as i64, now],
    )?;
    Ok(())
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
        "failed"
    } else {
        "succeeded"
    };
    tx.execute(
        "UPDATE library_operation_work SET state = ?3, failure_code = ?4, result_json = ?5, \
         updated_at = ?6, row_revision = row_revision + 1 WHERE job_id = ?1 AND ordinal = ?2",
        params![
            job_id,
            ordinal,
            state,
            failure.map(|reason| reason.code.clone()),
            result.to_string(),
            now
        ],
    )?;
    let column = if failure.is_some() {
        "failed_count"
    } else {
        "succeeded_count"
    };
    tx.execute(
        &format!(
            "UPDATE library_operation_jobs SET completed_count = completed_count + 1, \
             {column} = {column} + 1, heartbeat_at = ?2, lease_expires_at = ?3, updated_at = ?2, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?1"
        ),
        params![job_id, now, now + LEASE_SECS],
    )?;
    Ok(())
}

/// All work is done: the job ends, failed when any album failed.
pub fn finish_apply(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let failed: i64 = tx.query_row(
        "SELECT failed_count FROM library_operation_jobs WHERE id = ?1",
        params![job_id],
        |row| row.get(0),
    )?;
    let (state, code) = if failed > 0 {
        ("failed", "MANAGEMENT_PARTIAL")
    } else {
        ("succeeded", "MANAGEMENT_APPLIED")
    };
    let changed = tx.execute(
        "UPDATE library_operation_jobs SET state = ?3, terminal_code = ?4, terminal_at = ?5, \
         lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?5, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1 AND lease_owner = ?2 AND state = 'running'",
        params![job_id, worker, state, code, now],
    )?;
    if changed == 1 {
        tx.execute(
            "UPDATE library_management_job_snapshots SET phase = 'complete', updated_at = ?2, \
             row_revision = row_revision + 1 WHERE job_id = ?1",
            params![job_id, now],
        )?;
    }
    Ok(changed == 1)
}

/// Hand a running job back to the queue (the server is stopping).
pub fn release(tx: &Transaction<'_>, job_id: &str, worker: &str, now: f64) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_operation_work SET state = 'pending', updated_at = ?2, \
         row_revision = row_revision + 1 WHERE job_id = ?1 AND state = 'running'",
        params![job_id, now],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'queued', lease_owner = NULL, \
         lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?3, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1 AND lease_owner = ?2 AND state = 'running'",
        params![job_id, worker, now],
    )?;
    Ok(())
}

/// End a running job that cannot go on.
pub fn fail_job(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'failed', terminal_code = ?3, terminal_at = ?4, \
         lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?4, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1 AND lease_owner = ?2 AND state = 'running'",
        params![job_id, worker, code, now],
    )?;
    Ok(())
}

/// Set a ready preview aside. Only a ready preview can be discarded.
pub fn discard(
    tx: &Transaction<'_>,
    job_id: &str,
    expected_revision: i64,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE library_operation_jobs SET state = 'cancelled', terminal_code = 'DISCARDED', \
         terminal_at = ?3, updated_at = ?3, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 \
         WHERE id = ?1 AND kind = 'library_management' AND row_revision = ?2 \
         AND state IN ('ready', 'queued', 'paused', 'stopped', 'failed') \
         AND id IN (SELECT job_id FROM library_management_job_snapshots \
         WHERE phase IN ('planning', 'ready'))",
        params![job_id, expected_revision, now],
    )?;
    Ok(changed == 1)
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

/// Result rows of an applied job, with each album's work outcome.
pub fn results(
    conn: &Connection,
    job_id: &str,
    after_ordinal: i64,
    limit: i64,
) -> rusqlite::Result<ResultPage> {
    let page = items_page(conn, job_id, after_ordinal, limit, &ItemFilter::default())?;
    let mut work = conn.prepare_cached(
        "SELECT state, failure_code, result_json FROM library_operation_work \
         WHERE job_id = ?1 AND ordinal = ?2",
    )?;
    let mut items = Vec::with_capacity(page.items.len());
    for plan in page.items {
        let row: Option<(String, Option<String>, Option<String>)> = work
            .query_row(params![job_id, plan.bundle_ordinal], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        let (work_state, failure, result) = match row {
            Some((state, code, result)) => {
                let result: serde_json::Value = result
                    .as_deref()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or(serde_json::Value::Null);
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
                (state, failure, result)
            }
            None => {
                let failure = if plan.eligibility.writes() {
                    plan.diff.changes_anything().then(reasons::not_scheduled)
                } else {
                    plan.reason().cloned()
                };
                ("not_scheduled".to_owned(), failure, serde_json::Value::Null)
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

/// The bundle id an album's work row recorded before publishing, if any.
pub fn work_bundle(
    conn: &Connection,
    job_id: &str,
    ordinal: i64,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT json_extract(result_json, '$.bundle_id') FROM library_operation_work \
         WHERE job_id = ?1 AND ordinal = ?2",
        params![job_id, ordinal],
        |row| row.get(0),
    )
    .optional()
    .map(Option::flatten)
}

/// Record the bundle id an album is about to publish under.
pub fn set_work_bundle(
    conn: &Connection,
    job_id: &str,
    ordinal: i64,
    bundle_id: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE library_operation_work SET result_json = ?3, row_revision = row_revision + 1 \
         WHERE job_id = ?1 AND ordinal = ?2",
        params![
            job_id,
            ordinal,
            serde_json::json!({ "bundle_id": bundle_id }).to_string()
        ],
    )?;
    Ok(())
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

/// Undo jobs made for `job_id` that did not end in failure or discard:
/// (state, phase).
pub fn live_undos(conn: &Connection, job_id: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut statement = conn.prepare(
        "SELECT j.state, s.phase FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id \
         WHERE s.linked_operation_job_id = ?1 AND s.mode = 'undo' \
         AND j.state NOT IN ('failed', 'cancelled', 'stopped')",
    )?;
    let rows = statement.query_map(params![job_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Published bundles of an applied job: (bundle ordinal, bundle id).
pub fn applied_bundles(conn: &Connection, job_id: &str) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut statement = conn.prepare(
        "SELECT ordinal, json_extract(result_json, '$.bundle_id') FROM library_operation_work \
         WHERE job_id = ?1 AND state = 'succeeded' \
         AND json_extract(result_json, '$.bundle_id') IS NOT NULL ORDER BY ordinal",
    )?;
    let rows = statement.query_map(params![job_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Files of a job that can still be undone today.
pub fn undo_available(conn: &Connection, job_id: &str, today: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM library_publish_snapshots WHERE expires_day > ?2 AND bundle_id IN \
         (SELECT json_extract(result_json, '$.bundle_id') FROM library_operation_work \
         WHERE job_id = ?1 AND state = 'succeeded')",
        params![job_id, today],
        |row| row.get(0),
    )
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
