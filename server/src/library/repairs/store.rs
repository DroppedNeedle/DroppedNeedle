//! Repair job SQL over the 0001 tables: `library_operation_jobs` and
//! `library_operation_work` (kind `repair`), `library_repair_snapshots`
//! (scope, rules version, phase, report summary),
//! `library_repair_materialization` (the work list, sealed at creation)
//! and `library_identity_repair_findings` (one verdict per album).
//!
//! Plain SQL on the caller's connection or transaction; the service owns
//! the transactions. Job claiming, controls, leases and checkpoints are
//! the shared operation job SQL in [`crate::library::operations::store`].

use std::collections::BTreeMap;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};

use super::classify::{IdentityState, TrackState};
use super::models::{
    EditionSummary, FindingPage, FindingPayload, FindingRow, Guard, NewRepair, PreparationEstimate,
    Purpose, RepairEstimate, Scope, Summary, Verdict,
};
use super::reasons;
use crate::library::operations::decisions::live_automatic_edition_undo;
use crate::library::operations::models::{OperationError, OperationJob, OperationKind};
use crate::library::operations::store::{self as jobs, album_revisions};

const PROVIDER: &str = "musicbrainz";

/// Phase a repair job is in: checking (`dry_run`) or applying (`apply`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    DryRun,
    Apply,
}

/// What `library_repair_snapshots` keeps for one job.
#[derive(Debug, Clone)]
pub struct RepairSnapshot {
    pub purpose: Purpose,
    pub phase: Phase,
    pub target_matcher_version: String,
    pub summary: Option<Summary>,
}

pub fn snapshot(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<RepairSnapshot>> {
    let row: Option<(String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT scope_json, phase, target_matcher_version, result_json \
             FROM library_repair_snapshots WHERE job_id = ?1",
            params![job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    Ok(row.and_then(|(scope, phase, target, result)| {
        let scope: Scope = serde_json::from_str(&scope).unwrap_or_default();
        Some(RepairSnapshot {
            purpose: Purpose::parse(scope.purpose.as_deref())?,
            phase: if phase == "apply" {
                Phase::Apply
            } else {
                Phase::DryRun
            },
            target_matcher_version: target,
            summary: result.and_then(|raw| serde_json::from_str(&raw).ok()),
        })
    }))
}

/// The job and its snapshot when the job is a repair of `purpose`.
pub fn repair_job(
    conn: &Connection,
    job_id: &str,
    purpose: Purpose,
) -> Result<(OperationJob, RepairSnapshot), OperationError> {
    let job = jobs::job(conn, job_id)?.filter(|job| job.kind == OperationKind::Repair.as_str());
    let snap = snapshot(conn, job_id)?;
    match (job, snap) {
        (Some(job), Some(snap)) if snap.purpose == purpose => Ok((job, snap)),
        _ => Err(OperationError::NotFound(reasons::REPAIR_NOT_FOUND)),
    }
}

// ---------------------------------------------------------------------------
// Scope.
// ---------------------------------------------------------------------------

fn root_clause(root_ids: &[String], values: &mut Vec<Value>) -> String {
    if root_ids.is_empty() {
        return String::new();
    }
    values.extend(root_ids.iter().cloned().map(Value::Text));
    format!("AND a.root_id IN ({})", vec!["?"; root_ids.len()].join(","))
}

/// `FROM ... WHERE ...` for the albums a job of `purpose` covers, with
/// its bound values. One predicate for counting and for the work list.
fn scope_sql(
    purpose: Purpose,
    root_ids: &[String],
    album_ids: &[String],
    scan_run_id: Option<&str>,
    source_matcher_version: Option<&str>,
) -> (String, Vec<Value>) {
    let mut values = Vec::new();
    let roots = root_clause(root_ids, &mut values);
    let indexed = "AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
                   AND t.availability = 'indexed')";
    let sql = match purpose {
        Purpose::ExistingMatches => {
            let identity = match source_matcher_version {
                Some(version) => {
                    values.push(Value::Text(version.to_owned()));
                    "AND i.matcher_version = ?".to_owned()
                }
                // A person's choice is never rechecked for removal.
                None => "AND i.decision_source IN ('automatic','legacy_import')".to_owned(),
            };
            format!(
                "FROM local_albums a JOIN local_album_external_identities i \
                 ON i.local_album_id = a.id AND i.provider = '{PROVIDER}' \
                 WHERE a.retired_into_album_id IS NULL {indexed} {roots} {identity}"
            )
        }
        Purpose::ManagementReadiness => format!(
            "FROM local_albums a WHERE a.retired_into_album_id IS NULL {indexed} \
             AND NOT EXISTS (SELECT 1 FROM library_management_exclusions x \
             WHERE x.local_album_id = a.id) {roots}"
        ),
        Purpose::CatalogIdentityHygiene => {
            let albums = if album_ids.is_empty() {
                String::new()
            } else {
                values.extend(album_ids.iter().cloned().map(Value::Text));
                format!("AND a.id IN ({})", vec!["?"; album_ids.len()].join(","))
            };
            let run = match scan_run_id {
                // Albums the run touched: new or changed files, files that
                // moved in, and albums files moved out of (the shells the
                // run emptied).
                Some(run) => {
                    values.extend((0..3).map(|_| Value::Text(run.to_owned())));
                    "AND (EXISTS (SELECT 1 FROM library_scan_inventory inv \
                     JOIN local_tracks t ON t.id = inv.local_track_id WHERE inv.run_id = ? \
                     AND inv.comparison_result IN ('new','changed') \
                     AND t.local_album_id = a.id) \
                     OR EXISTS (SELECT 1 FROM library_scan_album_moves m \
                     JOIN local_tracks t ON t.id = m.local_track_id \
                     WHERE m.run_id = ? AND t.local_album_id = a.id) \
                     OR EXISTS (SELECT 1 FROM library_scan_album_moves m \
                     WHERE m.run_id = ? AND m.previous_album_id = a.id))"
                }
                None => "",
            };
            format!(
                "FROM local_albums a WHERE a.retired_into_album_id IS NULL \
                 AND {HYGIENE_CANDIDATE} {roots} {albums} {run}"
            )
        }
    };
    (sql, values)
}

// ---------------------------------------------------------------------------
// Creating a job.
// ---------------------------------------------------------------------------

/// Store a repair job with its whole work list, or hand back the job an
/// earlier request with the same idempotency key created. The work list
/// is fixed here: albums that appear later belong to the next check.
pub fn create(
    tx: &Transaction<'_>,
    new: &NewRepair,
    now: f64,
) -> Result<OperationJob, OperationError> {
    if new.idempotency_key.trim().is_empty() {
        return Err(OperationError::Invalid(reasons::IDEMPOTENCY_KEY_EMPTY));
    }
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM library_operation_jobs WHERE idempotency_key = ?1",
            params![new.idempotency_key],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(job_id) = existing {
        return match repair_job(tx, &job_id, new.purpose) {
            Ok((job, _)) => Ok(job),
            Err(_) => Err(OperationError::Conflict(reasons::IDEMPOTENCY_KEY_REUSED)),
        };
    }
    let catalog_revision: i64 = tx
        .query_row(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO library_operation_jobs (id, kind, state, requested_by_user_id, \
         input_catalog_revision, expected_work_count, idempotency_key, created_at, updated_at) \
         VALUES (?1, 'repair', 'queued', (SELECT id FROM auth_users WHERE id = ?2), ?3, 0, ?4, \
         ?5, ?5)",
        params![
            new.job_id,
            new.requested_by_user_id,
            catalog_revision,
            new.idempotency_key,
            now
        ],
    )?;
    let action = match new.purpose {
        Purpose::CatalogIdentityHygiene => "catalog_identity_hygiene",
        _ => "repair_audit",
    };
    let (scope_sql, scope_values) = scope_sql(
        new.purpose,
        &new.root_ids,
        &new.album_ids,
        new.scan_run_id.as_deref(),
        new.source_matcher_version.as_deref(),
    );
    let mut values = vec![
        Value::Text(new.job_id.clone()),
        Value::Text(action.to_owned()),
        Value::Real(now),
    ];
    values.extend(scope_values);
    let staged = tx.execute(
        &format!(
            "INSERT INTO library_operation_work (job_id, ordinal, local_album_id, \
             expected_subject_revision, expected_input_revision, action, idempotency_key, \
             updated_at) SELECT ?1, ROW_NUMBER() OVER (ORDER BY a.id) - 1, a.id, \
             a.row_revision, '', ?2, ?1 || ':' || a.id || ':' || ?2, ?3 {scope_sql}"
        ),
        params_from_iter(values),
    )?;
    let scope = Scope {
        root_ids: new.root_ids.clone(),
        album_ids: new.album_ids.clone(),
        purpose: Some(new.purpose.as_str().to_owned()),
        legacy_only: false,
        scan_run_id: new.scan_run_id.clone(),
    };
    tx.execute(
        "INSERT INTO library_repair_snapshots (job_id, scope_json, source_matcher_version, \
         target_matcher_version, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            new.job_id,
            serde_json::to_string(&scope)
                .map_err(|error| OperationError::Store(error.to_string()))?,
            new.source_matcher_version,
            new.target_matcher_version,
            now
        ],
    )?;
    tx.execute(
        "INSERT INTO library_repair_materialization (job_id, pinned_catalog_revision, \
         eligibility_version, purpose, staging_cursor, staged_ordinal, staged_count, sealed, \
         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, 1, ?7, ?7)",
        params![
            new.job_id,
            catalog_revision,
            format!(
                "{}:{}:{}",
                new.purpose.as_str(),
                new.source_matcher_version.as_deref().unwrap_or(""),
                new.target_matcher_version
            ),
            new.purpose.as_str(),
            staged as i64 - 1,
            staged as i64,
            now
        ],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET expected_work_count = ?2 WHERE id = ?1",
        params![new.job_id, staged as i64],
    )?;
    jobs::job_after(tx, &new.job_id)
}

/// The hygiene job queued once per database (v2's backfill key), so an
/// upgrade's leftovers are cleaned without anyone asking.
pub const HYGIENE_BACKFILL_KEY: &str = "catalog-identity-hygiene:v1:backfill";

/// Albums the cleanup could apply to: imported groupings, and automatic
/// albums with no file left.
const HYGIENE_CANDIDATE: &str = "(a.grouping_source = 'legacy_import' \
     OR (a.grouping_source = 'automatic' AND NOT EXISTS \
     (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id)))";

pub fn hygiene_candidates_exist(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM local_albums a \
             WHERE a.retired_into_album_id IS NULL AND {HYGIENE_CANDIDATE})"
        ),
        [],
        |row| row.get(0),
    )
}

/// Albums identification finished with after `after_ms` that the cleanup
/// could apply to (at most 200), and the newest finish time seen.
pub fn identified_since(
    conn: &Connection,
    after_ms: i64,
) -> rusqlite::Result<(Vec<String>, Option<i64>)> {
    let newest: Option<i64> = conn.query_row(
        "SELECT MAX(updated_ms) FROM library_identify_jobs \
         WHERE state IN ('succeeded','failed','attention') AND updated_ms > ?1",
        params![after_ms],
        |row| row.get(0),
    )?;
    let Some(newest) = newest else {
        return Ok((Vec::new(), None));
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT a.id FROM library_identify_jobs j JOIN local_albums a \
         ON a.id = j.local_album_id WHERE j.state IN ('succeeded','failed','attention') \
         AND j.updated_ms > ?1 AND j.updated_ms <= ?2 AND a.retired_into_album_id IS NULL \
         AND {HYGIENE_CANDIDATE} ORDER BY a.id LIMIT 200"
    ))?;
    let albums = stmt
        .query_map(params![after_ms, newest], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok((albums, Some(newest)))
}

/// The latest scan run that ended, however it ended (finished, stopped,
/// failed or superseded): what it changed before it ended still needs the
/// per-scan cleanup.
pub fn latest_ended_scan(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM library_scan_runs WHERE terminal_at IS NOT NULL AND state IN \
         ('completed','cancelled','failed','superseded_policy_changed') \
         ORDER BY terminal_at DESC, id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .optional()
}

/// How many albums a cleanup after `run_id` would look at.
pub fn hygiene_count_after_scan(conn: &Connection, run_id: &str) -> rusqlite::Result<i64> {
    let (scope, values) = scope_sql(
        Purpose::CatalogIdentityHygiene,
        &[],
        &[],
        Some(run_id),
        None,
    );
    conn.query_row(
        &format!("SELECT COUNT(*) {scope}"),
        params_from_iter(values),
        |row| row.get(0),
    )
}

/// Idempotency key of the cleanup after one scan run.
pub fn hygiene_scan_key(run_id: &str) -> String {
    format!("catalog-identity-hygiene:v1:scan:{run_id}")
}

pub fn hygiene_backfill_queued(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_operation_jobs WHERE idempotency_key = ?1)",
        params![HYGIENE_BACKFILL_KEY],
        |row| row.get(0),
    )
}

// ---------------------------------------------------------------------------
// Estimates and history.
// ---------------------------------------------------------------------------

fn active_count(conn: &Connection, purpose: Purpose) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM library_operation_jobs j \
         JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.kind = 'repair' AND j.state IN ('queued','running','paused','ready') \
         AND COALESCE(json_extract(s.scope_json, '$.purpose'), 'existing_matches') = ?1",
        params![purpose.as_str()],
        |row| row.get(0),
    )
}

pub fn estimate_repair(conn: &Connection, root_ids: &[String]) -> rusqlite::Result<RepairEstimate> {
    let (scope, values) = scope_sql(Purpose::ExistingMatches, root_ids, &[], None, None);
    let identity_count = conn.query_row(
        &format!("SELECT COUNT(*) {scope}"),
        params_from_iter(values),
        |row| row.get(0),
    )?;
    Ok(RepairEstimate {
        identity_count,
        queued_repair_count: active_count(conn, Purpose::ExistingMatches)?,
    })
}

/// Where each album Library Management may touch stands: ready (every
/// file placed on its exact release, or a current custom edition), in
/// need of placements, or without an exact release.
pub fn estimate_preparation(
    conn: &Connection,
    root_ids: &[String],
) -> rusqlite::Result<PreparationEstimate> {
    let (scope, values) = scope_sql(Purpose::ManagementReadiness, root_ids, &[], None, None);
    let sql = format!(
        "SELECT i.release_group_mbid, i.release_mbid, \
         (SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id = a.id \
          AND t.availability = 'indexed') AS tracks, \
         (SELECT COUNT(DISTINCT ti.release_track_mbid) FROM local_tracks t \
          JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
          AND ti.provider = '{PROVIDER}' WHERE t.local_album_id = a.id \
          AND t.availability = 'indexed' AND ti.release_track_mbid IS NOT NULL \
          AND ti.release_mbid = i.release_mbid AND ti.medium_position IS NOT NULL \
          AND ti.release_track_position IS NOT NULL) AS mapped, \
         {CUSTOM_CURRENT} AS custom_current \
         {from} ",
        from = scope.replacen(
            "FROM local_albums a",
            &format!(
                "FROM local_albums a LEFT JOIN local_album_external_identities i \
                 ON i.local_album_id = a.id AND i.provider = '{PROVIDER}'"
            ),
            1
        )
    );
    let mut estimate = PreparationEstimate::default();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(values))?;
    while let Some(row) = rows.next()? {
        let group: Option<String> = row.get(0)?;
        let release: Option<String> = row.get(1)?;
        let tracks: i64 = row.get(2)?;
        let mapped: i64 = row.get(3)?;
        let custom_current: bool = row.get(4)?;
        estimate.album_count += 1;
        if custom_current {
            estimate.ready_album_count += 1;
        } else if group.is_none() || release.is_none() {
            estimate.exact_release_required_count += 1;
        } else if mapped == tracks {
            estimate.ready_album_count += 1;
        } else {
            estimate.mapping_required_count += 1;
        }
    }
    estimate.queued_preparation_count = active_count(conn, Purpose::ManagementReadiness)?;
    Ok(estimate)
}

/// True when album `a` has an active custom edition sealed against its
/// current revisions and files.
const CUSTOM_CURRENT: &str = "COALESCE((SELECT m.source_album_revision = a.row_revision \
     AND COALESCE(m.source_identity_revision, 0) = COALESCE((SELECT ai.row_revision \
       FROM local_album_external_identities ai WHERE ai.local_album_id = a.id \
       AND ai.provider = 'musicbrainz'), 0) \
     AND (SELECT COUNT(*) FROM library_custom_edition_tracks ct JOIN local_tracks ctt \
       ON ctt.id = ct.local_track_id WHERE ct.manifest_id = m.id \
       AND ctt.local_album_id = a.id AND ctt.availability = 'indexed' \
       AND ct.stat_revision = ctt.stat_revision) = (SELECT COUNT(*) FROM local_tracks it \
       WHERE it.local_album_id = a.id AND it.availability = 'indexed') \
   FROM library_custom_edition_active ac JOIN library_custom_edition_manifests m \
   ON m.id = ac.manifest_id WHERE ac.local_album_id = a.id), 0)";

/// One page of a purpose's jobs, newest first. `before` is the last
/// page's `(created_at, id)`.
pub fn history(
    conn: &Connection,
    purpose: Purpose,
    limit: usize,
    before: Option<(f64, String)>,
) -> rusqlite::Result<Vec<String>> {
    let (created, id) = before.unzip();
    let mut stmt = conn.prepare(
        "SELECT j.id FROM library_operation_jobs j \
         JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.kind = 'repair' \
         AND COALESCE(json_extract(s.scope_json, '$.purpose'), 'existing_matches') = ?1 \
         AND (?2 IS NULL OR j.created_at < ?2 OR (j.created_at = ?2 AND j.id < ?3)) \
         ORDER BY j.created_at DESC, j.id DESC LIMIT ?4",
    )?;
    stmt.query_map(
        params![purpose.as_str(), created, id, limit as i64],
        |row| row.get(0),
    )?
    .collect()
}

// ---------------------------------------------------------------------------
// One album, as the check sees it.
// ---------------------------------------------------------------------------

/// The album's identity, files and revisions.
#[derive(Debug, Clone)]
pub struct Subject {
    pub identity: Option<IdentityState>,
    pub identity_revision: Option<i64>,
    pub tracks: Vec<TrackState>,
    pub guard: Guard,
    /// Policies the album's files were scanned under.
    pub policies: Vec<String>,
    /// `Some(current)` when a custom edition is active.
    pub custom_current: Option<bool>,
    /// The library root the album lives in.
    pub root_id: String,
}

/// The album as it is now; `None` once it is retired or has no files.
pub fn subject(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Subject>> {
    let Some(revisions) = album_revisions(conn, album_id)? else {
        return Ok(None);
    };
    let identity: Option<(IdentityState, i64)> = conn
        .query_row(
            "SELECT release_group_mbid, release_mbid, decision_source, row_revision \
             FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
            params![album_id, PROVIDER],
            |row| {
                Ok((
                    IdentityState {
                        release_group_mbid: row.get(0)?,
                        release_mbid: row.get(1)?,
                        manual: row.get::<_, String>(2)? == "manual",
                    },
                    row.get(3)?,
                ))
            },
        )
        .optional()?;
    let mut stmt = conn.prepare(
        "SELECT t.id, ti.recording_mbid, ti.release_mbid, ti.release_track_mbid, \
         ti.medium_position, ti.release_track_position FROM local_tracks t \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         AND ti.provider = ?2 WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
         ORDER BY t.id",
    )?;
    let tracks = stmt
        .query_map(params![album_id, PROVIDER], |row| {
            Ok(TrackState {
                local_track_id: row.get(0)?,
                recording_mbid: row.get(1)?,
                release_mbid: row.get(2)?,
                release_track_mbid: row.get(3)?,
                medium_position: row.get(4)?,
                release_track_position: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let custom_current: Option<bool> = conn
        .query_row(
            &format!(
                "SELECT {CUSTOM_CURRENT} FROM local_albums a WHERE a.id = ?1 AND EXISTS \
                 (SELECT 1 FROM library_custom_edition_active ac WHERE ac.local_album_id = a.id)"
            ),
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let root_id: String = conn.query_row(
        "SELECT root_id FROM local_albums WHERE id = ?1",
        params![album_id],
        |row| row.get(0),
    )?;
    Ok(Some(Subject {
        root_id,
        identity_revision: identity.as_ref().map(|(_, revision)| *revision),
        identity: identity.map(|(state, _)| state),
        tracks,
        guard: Guard {
            album_revision: revisions.album_revision,
            input_revision: revisions.input_revision,
            identity_revision: revisions.identity_revision,
        },
        policies: revisions.policies,
        custom_current,
    }))
}

/// The album still looks the way it did when it was checked.
pub fn guard_holds(
    conn: &Connection,
    album_id: &str,
    guard: Option<&Guard>,
) -> rusqlite::Result<bool> {
    let Some(guard) = guard else {
        return Ok(false);
    };
    Ok(album_revisions(conn, album_id)?.is_some_and(|now| {
        now.album_revision == guard.album_revision
            && now.input_revision == guard.input_revision
            && now.identity_revision == guard.identity_revision
    }))
}

// ---------------------------------------------------------------------------
// Work.
// ---------------------------------------------------------------------------

/// Claim the job's next pending album: `(ordinal, album_id)`.
pub fn claim_album(
    tx: &Transaction<'_>,
    job_id: &str,
    now: f64,
) -> rusqlite::Result<Option<(i64, String)>> {
    let Some(ordinal) = jobs::claim_work(tx, job_id, now)? else {
        return Ok(None);
    };
    let album: Option<String> = tx.query_row(
        "SELECT local_album_id FROM library_operation_work WHERE job_id = ?1 AND ordinal = ?2",
        params![job_id, ordinal],
        |row| row.get(0),
    )?;
    Ok(Some((ordinal, album.unwrap_or_default())))
}

/// Close one work item and count it on the job.
#[allow(clippy::too_many_arguments)]
pub fn finish_work(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    ordinal: i64,
    state: &str,
    failure_code: Option<&str>,
    result: Option<&serde_json::Value>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_operation_work SET state = ?3, failure_code = ?4, result_json = ?5, \
         updated_at = ?6, row_revision = row_revision + 1 WHERE job_id = ?1 AND ordinal = ?2",
        params![
            job_id,
            ordinal,
            state,
            failure_code,
            result.map(|value| value.to_string()),
            now
        ],
    )?;
    let counter = match state {
        "succeeded" => "succeeded_count",
        "failed" => "failed_count",
        _ => "skipped_count",
    };
    tx.execute(
        &format!(
            "UPDATE library_operation_jobs SET completed_count = completed_count + 1, \
             {counter} = {counter} + 1, updated_at = ?3, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = ?1 AND state = 'running' AND lease_owner = ?2"
        ),
        params![job_id, worker, now],
    )?;
    Ok(())
}

/// Store one album's verdict and close its work item.
#[allow(clippy::too_many_arguments)]
pub fn save_finding(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    ordinal: i64,
    album_id: &str,
    verdict: &Verdict,
    identity_revision: Option<i64>,
    now: f64,
) -> Result<(), OperationError> {
    let expected_album_revision: i64 = tx.query_row(
        "SELECT expected_subject_revision FROM library_operation_work \
         WHERE job_id = ?1 AND ordinal = ?2",
        params![job_id, ordinal],
        |row| row.get(0),
    )?;
    let candidate = verdict.payload.candidate.as_ref();
    let payload = serde_json::to_string(&verdict.payload)
        .map_err(|error| OperationError::Store(error.to_string()))?;
    tx.execute(
        "INSERT INTO library_identity_repair_findings (id, job_id, local_album_id, \
         expected_album_revision, expected_identity_revision, finding_code, confidence, \
         reason_code, apply_eligible, suggested_release_mbid, suggested_release_group_mbid, \
         suggested_edition_json, state, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'open', ?13, ?13) \
         ON CONFLICT (job_id, local_album_id, finding_code) DO UPDATE SET \
         expected_album_revision = excluded.expected_album_revision, \
         expected_identity_revision = excluded.expected_identity_revision, \
         confidence = excluded.confidence, reason_code = excluded.reason_code, \
         apply_eligible = excluded.apply_eligible, \
         suggested_release_mbid = excluded.suggested_release_mbid, \
         suggested_release_group_mbid = excluded.suggested_release_group_mbid, \
         suggested_edition_json = excluded.suggested_edition_json, state = 'open', \
         apply_result = NULL, updated_at = excluded.updated_at, row_revision = row_revision + 1",
        params![
            uuid::Uuid::new_v4().to_string(),
            job_id,
            album_id,
            expected_album_revision,
            identity_revision,
            verdict.finding_code,
            if verdict.apply_eligible {
                "complete"
            } else {
                "bounded"
            },
            verdict.reason_code,
            verdict.apply_eligible,
            candidate.and_then(|candidate| candidate.evidence.release_mbid.clone()),
            candidate.map(|candidate| candidate.evidence.release_group_mbid.clone()),
            payload,
            now
        ],
    )?;
    finish_work(
        tx,
        job_id,
        worker,
        ordinal,
        "succeeded",
        None,
        Some(&serde_json::json!({
            "finding_code": verdict.finding_code,
            "reason_code": verdict.reason_code,
        })),
        now,
    )?;
    Ok(())
}

/// Apply a suggestion that passed the automatic acceptance gate, if the
/// album still looks the way it was checked. The finding becomes
/// `exact_release_auto_accepted`; otherwise it stays a suggestion.
pub fn auto_accept(
    tx: &Transaction<'_>,
    job_id: &str,
    album_id: &str,
    verdict: &Verdict,
    now: f64,
) -> Result<bool, OperationError> {
    let Some(candidate) = verdict.payload.candidate.as_ref() else {
        return Ok(false);
    };
    if !verdict.payload.auto_accept || !guard_holds(tx, album_id, verdict.payload.guard.as_ref())? {
        return Ok(false);
    }
    let accepted = crate::library::operations::choice::accept_automatic(
        tx,
        album_id,
        candidate,
        "EDITION_AUTO_ACCEPTED",
        Some(job_id),
        now,
    )?;
    if accepted.is_none() {
        return Ok(false);
    }
    tx.execute(
        "UPDATE library_identity_repair_findings SET finding_code = 'exact_release_auto_accepted', \
         reason_code = 'EXACT_EDITION_AUTO_ACCEPTED', apply_eligible = 0, state = 'applied', \
         apply_result = 'EDITION_AUTO_ACCEPTED', updated_at = ?3, row_revision = row_revision + 1 \
         WHERE job_id = ?1 AND local_album_id = ?2 AND finding_code = 'exact_release_suggested'",
        params![job_id, album_id, now],
    )?;
    Ok(true)
}

/// Release a running job back to the queue: it used its turn, or must
/// wait until `retry_at` (MusicBrainz was down). The running item goes
/// back to pending.
pub fn yield_job(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    failure_code: Option<&str>,
    retry_at: Option<f64>,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    tx.execute(
        "UPDATE library_operation_work SET state = 'pending', \
         failure_code = COALESCE(?2, failure_code), updated_at = ?3, \
         row_revision = row_revision + 1 WHERE job_id = ?1 AND state = 'running'",
        params![job_id, failure_code, now],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'queued', lease_owner = NULL, \
         lease_expires_at = NULL, heartbeat_at = NULL, next_attempt_at = ?3, updated_at = ?4, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
        params![job_id, worker, retry_at, now],
    )?;
    jobs::job(tx, job_id)
}

/// Every album was checked: write the report summary and wait for Apply.
pub fn mark_ready(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    snap: &RepairSnapshot,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    let summary = summarize(tx, job_id, snap)?;
    tx.execute(
        "UPDATE library_repair_snapshots SET result_json = ?2 WHERE job_id = ?1",
        params![job_id, serde_json::to_string(&summary).unwrap_or_default()],
    )?;
    end(tx, job_id, worker, "ready", "DRY_RUN_READY", now)
}

/// End a running job (`succeeded` or `failed`) with `code`.
pub fn end(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    state: &str,
    code: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    let terminal = if state == "ready" { None } else { Some(now) };
    tx.execute(
        "UPDATE library_operation_jobs SET state = ?3, terminal_code = ?4, terminal_at = ?5, \
         lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?6, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 \
         WHERE id = ?1 AND state = 'running' AND lease_owner = ?2",
        params![job_id, worker, state, code, terminal, now],
    )?;
    jobs::job(tx, job_id)
}

fn summarize(conn: &Connection, job_id: &str, snap: &RepairSnapshot) -> rusqlite::Result<Summary> {
    let mut summary = Summary {
        purpose: snap.purpose.as_str().to_owned(),
        target_matcher_version: snap.target_matcher_version.clone(),
        ..Summary::default()
    };
    let mut stmt = conn.prepare(
        "SELECT finding_code, reason_code, COUNT(*) FROM library_identity_repair_findings \
         WHERE job_id = ?1 GROUP BY finding_code, reason_code",
    )?;
    let mut rows = stmt.query(params![job_id])?;
    while let Some(row) = rows.next()? {
        let (finding, reason, count): (String, String, i64) =
            (row.get(0)?, row.get(1)?, row.get(2)?);
        *summary.counts_by_finding.entry(finding).or_default() += count;
        *summary.counts_by_reason.entry(reason).or_default() += count;
        summary.total_identities += count;
    }
    let mut stmt = conn.prepare(
        "SELECT a.root_id, COUNT(DISTINCT f.local_album_id) FROM library_identity_repair_findings f \
         JOIN local_albums a ON a.id = f.local_album_id WHERE f.job_id = ?1 \
         GROUP BY a.root_id ORDER BY a.root_id",
    )?;
    summary.album_counts_by_root = stmt
        .query_map(params![job_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<String, i64>>>()?;
    summary.input_track_count = conn.query_row(
        "SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed' AND local_album_id IN \
         (SELECT local_album_id FROM library_identity_repair_findings WHERE job_id = ?1)",
        params![job_id],
        |row| row.get(0),
    )?;
    summary.playable_after_detach_track_count = summary.input_track_count;
    summary.catalog_snapshot_revision = conn.query_row(
        "SELECT COALESCE(input_catalog_revision, 0) FROM library_operation_jobs WHERE id = ?1",
        params![job_id],
        |row| row.get(0),
    )?;
    let count = |code: &str| summary.counts_by_finding.get(code).copied().unwrap_or(0);
    summary.estimated_apply_changes = match snap.purpose {
        Purpose::ManagementReadiness => count("mapping_ready") + count("exact_release_suggested"),
        _ => count("safe_detach"),
    };
    summary.ready_album_count = count("ready");
    summary.mapping_candidate_count = count("mapping_ready");
    summary.exact_release_required_count =
        count("exact_release_required") + count("exact_release_suggested");
    summary.needs_review_count = count("needs_review");
    summary.provider_deferred_count = summary
        .counts_by_reason
        .get("PROVIDER_DEFERRED")
        .copied()
        .unwrap_or(0);
    summary.failed_evidence_count = summary
        .counts_by_reason
        .get("EVIDENCE_UNAVAILABLE")
        .copied()
        .unwrap_or(0);
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Apply and discard.
// ---------------------------------------------------------------------------

/// Turn a ready report into Apply work: one item per open, eligible
/// finding. A second call while applying answers with the job as it is.
pub fn begin_apply(
    tx: &Transaction<'_>,
    job_id: &str,
    purpose: Purpose,
    expected_row_revision: i64,
    now: f64,
) -> Result<OperationJob, OperationError> {
    let (job, snap) = repair_job(tx, job_id, purpose)?;
    if snap.phase == Phase::Apply {
        return Ok(job);
    }
    if purpose == Purpose::ManagementReadiness
        && snap.target_matcher_version != super::models::MANAGEMENT_MAPPING_VERSION
    {
        return Err(OperationError::Invalid(reasons::RULES_CHANGED));
    }
    if job.state.as_str() != "ready" || job.row_revision != expected_row_revision {
        return Err(OperationError::Conflict(reasons::REPORT_CHANGED));
    }
    tx.execute(
        "DELETE FROM library_operation_work WHERE job_id = ?1",
        params![job_id],
    )?;
    let staged = tx.execute(
        "INSERT INTO library_operation_work (job_id, ordinal, local_album_id, \
         expected_subject_revision, expected_input_revision, action, idempotency_key, \
         updated_at) SELECT ?1, ROW_NUMBER() OVER (ORDER BY f.id) - 1, f.local_album_id, \
         f.expected_album_revision, COALESCE(f.expected_identity_revision, '') || ':', \
         'repair_apply', ?1 || ':' || f.id || ':apply', ?2 \
         FROM library_identity_repair_findings f \
         WHERE f.job_id = ?1 AND f.apply_eligible = 1 AND f.state = 'open'",
        params![job_id, now],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'queued', expected_work_count = ?2, \
         completed_count = 0, succeeded_count = 0, failed_count = 0, skipped_count = 0, \
         terminal_code = NULL, terminal_at = NULL, updated_at = ?3, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?1",
        params![job_id, staged as i64, now],
    )?;
    tx.execute(
        "UPDATE library_repair_snapshots SET phase = 'apply' WHERE job_id = ?1",
        params![job_id],
    )?;
    jobs::job_after(tx, job_id)
}

/// Dismiss a ready preparation report without changing anything.
pub fn discard(
    tx: &Transaction<'_>,
    job_id: &str,
    expected_row_revision: i64,
    now: f64,
) -> Result<OperationJob, OperationError> {
    let (job, snap) = repair_job(tx, job_id, Purpose::ManagementReadiness)?;
    if job.state.as_str() == "cancelled"
        && job.terminal_code.as_deref() == Some("IDENTITY_PREPARATION_DISCARDED")
    {
        return Ok(job);
    }
    if job.state.as_str() != "ready"
        || snap.phase != Phase::DryRun
        || job.row_revision != expected_row_revision
    {
        return Err(OperationError::Conflict(reasons::REPORT_CHANGED));
    }
    tx.execute(
        "UPDATE library_operation_jobs SET state = 'cancelled', \
         terminal_code = 'IDENTITY_PREPARATION_DISCARDED', terminal_at = ?2, \
         control_request = 'none', lease_owner = NULL, lease_expires_at = NULL, \
         heartbeat_at = NULL, updated_at = ?2, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 WHERE id = ?1",
        params![job_id, now],
    )?;
    jobs::job_after(tx, job_id)
}

/// The open, eligible finding Apply works on for one album.
#[derive(Debug, Clone)]
pub struct OpenFinding {
    pub id: String,
    pub finding_code: String,
    pub payload: FindingPayload,
}

pub fn open_finding(
    conn: &Connection,
    job_id: &str,
    album_id: &str,
) -> rusqlite::Result<Option<OpenFinding>> {
    conn.query_row(
        "SELECT id, finding_code, suggested_edition_json FROM library_identity_repair_findings \
         WHERE job_id = ?1 AND local_album_id = ?2 AND apply_eligible = 1 AND state = 'open' \
         ORDER BY id LIMIT 1",
        params![job_id, album_id],
        |row| {
            Ok(OpenFinding {
                id: row.get(0)?,
                finding_code: row.get(1)?,
                payload: serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default(),
            })
        },
    )
    .optional()
}

/// Record what Apply did with a finding.
pub fn settle_finding(
    tx: &Transaction<'_>,
    finding_id: &str,
    state: &str,
    apply_result: &str,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_identity_repair_findings SET state = ?2, apply_result = ?3, \
         updated_at = ?4, row_revision = row_revision + 1 WHERE id = ?1",
        params![finding_id, state, apply_result, now],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Findings.
// ---------------------------------------------------------------------------

/// The finding codes a category lists, per purpose (v2's groups).
pub fn category_codes(purpose: Purpose, category: &str) -> Option<&'static [&'static str]> {
    let codes: &[&str] = match (purpose, category) {
        (Purpose::ManagementReadiness, "ready") => &["ready"],
        (Purpose::ManagementReadiness, "mapping_ready") => &["mapping_ready"],
        (Purpose::ManagementReadiness, "exact_release_required") => {
            &["exact_release_required", "exact_release_suggested"]
        }
        (Purpose::ManagementReadiness, "exact_release_auto_accepted") => {
            &["exact_release_auto_accepted"]
        }
        (_, "needs_review") => &["needs_review"],
        (_, "unverifiable") => &["unverifiable", "stale"],
        (Purpose::ExistingMatches, "valid") => &["valid"],
        (Purpose::ExistingMatches, "safe_detach") => &["safe_detach"],
        (Purpose::ExistingMatches, "manual_identity") => &["manual_identity"],
        _ => return None,
    };
    Some(codes)
}

/// One page of a job's findings, newest change first. A preparation
/// lists only findings that still describe the album as it is.
pub fn findings_page(
    conn: &Connection,
    job_id: &str,
    purpose: Purpose,
    codes: Option<&[&str]>,
    cursor: Option<(f64, String)>,
    limit: usize,
) -> rusqlite::Result<FindingPage> {
    let current = if purpose == Purpose::ManagementReadiness {
        "AND ((f.state = 'open' AND a.row_revision = f.expected_album_revision \
         AND ((f.expected_identity_revision IS NULL AND i.row_revision IS NULL) \
         OR i.row_revision = f.expected_identity_revision)) \
         OR (f.finding_code = 'exact_release_auto_accepted' AND f.state = 'applied' \
         AND EXISTS (SELECT 1 FROM library_automatic_edition_undo u \
         WHERE u.local_album_id = f.local_album_id AND u.consumed_at IS NULL)))"
    } else {
        ""
    };
    let from = format!(
        "FROM library_identity_repair_findings f JOIN local_albums a ON a.id = f.local_album_id \
         LEFT JOIN local_album_external_identities i ON i.local_album_id = a.id \
         AND i.provider = '{PROVIDER}' WHERE f.job_id = ? {current}"
    );
    let mut values = vec![Value::Text(job_id.to_owned())];
    let mut filter = String::new();
    if let Some(codes) = codes {
        filter.push_str(&format!(
            " AND f.finding_code IN ({})",
            vec!["?"; codes.len()].join(",")
        ));
        values.extend(codes.iter().map(|code| Value::Text((*code).to_owned())));
    }
    if let Some((updated, id)) = cursor {
        filter.push_str(" AND (f.updated_at < ? OR (f.updated_at = ? AND f.id < ?))");
        values.extend([Value::Real(updated), Value::Real(updated), Value::Text(id)]);
    }
    values.push(Value::Integer(limit as i64 + 1));
    let sql = format!(
        "SELECT f.id, f.local_album_id, a.title, a.album_artist_name, a.year, \
         (EXISTS (SELECT 1 FROM local_album_artwork w WHERE w.local_album_id = a.id) \
          OR i.release_group_mbid IS NOT NULL), \
         (SELECT r.id FROM library_identify_reviews r WHERE r.local_album_id = f.local_album_id \
          ORDER BY r.updated_ms DESC, r.id DESC LIMIT 1), \
         f.finding_code, f.reason_code, f.confidence, f.apply_eligible, f.state, \
         f.apply_result, f.suggested_release_mbid, f.suggested_release_group_mbid, \
         f.suggested_edition_json, f.updated_at, f.row_revision \
         {from}{filter} ORDER BY f.updated_at DESC, f.id DESC LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut items = stmt
        .query_map(params_from_iter(values), |row| {
            let raw: String = row.get(15)?;
            let mut payload: FindingPayload = serde_json::from_str(&raw).unwrap_or_default();
            if payload.summary.is_none() {
                // v2 kept the suggestion's summary at the top level.
                payload.summary = serde_json::from_str::<EditionSummary>(&raw)
                    .ok()
                    .filter(|summary| !summary.title.is_empty());
            }
            Ok(FindingRow {
                id: row.get(0)?,
                local_album_id: row.get(1)?,
                album_title: row.get(2)?,
                album_artist_name: row.get(3)?,
                album_year: row.get(4)?,
                cover_available: row.get(5)?,
                review_id: row.get(6)?,
                finding_code: row.get(7)?,
                reason_code: row.get(8)?,
                confidence: row.get(9)?,
                apply_eligible: row.get(10)?,
                state: row.get(11)?,
                apply_result: row.get(12)?,
                suggested_release_mbid: row.get(13)?,
                suggested_release_group_mbid: row.get(14)?,
                payload,
                automatic_undo: None,
                updated_at: row.get(16)?,
                row_revision: row.get(17)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = items.len() > limit;
    items.truncate(limit);
    for item in &mut items {
        item.automatic_undo = live_automatic_edition_undo(conn, &item.local_album_id)?;
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT f.finding_code, COUNT(*) {from} GROUP BY f.finding_code"
    ))?;
    let current_counts_by_finding = stmt
        .query_map(params![job_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<BTreeMap<String, i64>>>()?;
    let next_cursor = has_more
        .then(|| {
            items
                .last()
                .map(|last| format!("{}:{}", last.updated_at, last.id))
        })
        .flatten();
    Ok(FindingPage {
        items,
        next_cursor,
        has_more,
        current_counts_by_finding,
        refresh_required: false,
    })
}
