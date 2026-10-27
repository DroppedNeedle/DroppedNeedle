//! Repair job SQL over the 0001 tables: `library_operation_jobs` and
//! `library_operation_work` (kind `repair`), `library_repair_snapshots`
//! (scope, rules version, phase, report summary),
//! `library_repair_materialization` (the work list, sealed at creation)
//! and `library_identity_repair_findings` (one verdict per album).
//!
//! Plain SQL on the caller's connection or transaction; the service owns
//! the transactions. The job and work rows themselves are queued, claimed,
//! counted and ended through [`crate::jobs`].

use std::collections::BTreeMap;

use droppedneedle_providers::musicbrainz::is_valid_mbid;
use rusqlite::types::{Type, Value};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};

use super::classify::{IdentityState, TrackState};
use super::models::{
    ApplyResult, EditionSummary, FindingCode, FindingPage, FindingPayload, FindingRow,
    FindingState, Guard, NewRepair, Phase, PreparationEstimate, Purpose, RepairEstimate, Scope,
    Summary, Verdict,
};
use super::reasons;
use crate::identify::artist_links::RELEASE_DOCUMENT_MISSING;
use crate::identify::evidence::edition_ids_of;
use crate::identity::DecisionSource;
use crate::identity::reads::stored_release;
use crate::identity::sql::REVISABLE;
use crate::jobs::{self, NewJob, OperationState, Tally, WorkState};
use crate::matching::decide::{IdStance, id_stance};
use crate::operations::decisions::live_automatic_edition_undo;
use crate::operations::models::{OperationError, OperationJob, OperationKind};
use crate::operations::store::{album_revisions, job_after};

const PROVIDER: &str = "musicbrainz";

/// What `library_repair_snapshots` keeps for one job.
#[derive(Debug, Clone)]
pub struct RepairSnapshot {
    pub purpose: Purpose,
    pub phase: Phase,
    pub scope: Scope,
    pub target_matcher_version: String,
    pub summary: Option<Summary>,
}

/// Stored repair state that does not decode. Made through
/// [`Unreadable::new`], which logs it: such a row is never read as a
/// default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable(pub String);

impl Unreadable {
    pub fn new(job_id: &str, what: &'static str, cause: impl std::fmt::Display) -> Self {
        tracing::error!(job_id, what, %cause, "stored repair state is unreadable");
        Self(format!("repair job {job_id}: {what} is unreadable"))
    }
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreadable {}

impl From<Unreadable> for OperationError {
    fn from(unreadable: Unreadable) -> Self {
        Self::Store(unreadable.0)
    }
}

impl From<Unreadable> for rusqlite::Error {
    fn from(unreadable: Unreadable) -> Self {
        Self::FromSqlConversionFailure(0, Type::Text, Box::new(unreadable))
    }
}

impl Unreadable {
    /// Whether `error` is stored repair state that did not decode.
    pub fn is(error: &rusqlite::Error) -> bool {
        matches!(error, rusqlite::Error::FromSqlConversionFailure(_, _, cause)
            if cause.downcast_ref::<Self>().is_some())
    }
}

/// One job's snapshot, `Err` when a part of it does not decode: its
/// purpose (a row whose scope was unreadable when the column was added
/// has none), phase, scope or report summary.
pub fn snapshot(
    conn: &Connection,
    job_id: &str,
) -> rusqlite::Result<Option<Result<RepairSnapshot, Unreadable>>> {
    type Row = (Option<String>, String, String, String, Option<String>);
    let row: Option<Row> = conn
        .query_row(
            "SELECT purpose, phase, scope_json, target_matcher_version, result_json \
             FROM library_repair_snapshots WHERE job_id = ?1",
            params![job_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(|(purpose, phase, scope, target, result)| {
        decode_snapshot(job_id, purpose, &phase, &scope, target, result)
    }))
}

fn decode_snapshot(
    job_id: &str,
    purpose: Option<String>,
    phase: &str,
    scope: &str,
    target_matcher_version: String,
    result: Option<String>,
) -> Result<RepairSnapshot, Unreadable> {
    let purpose = match purpose.as_deref() {
        Some(raw) => Purpose::parse(raw).ok_or_else(|| Unreadable::new(job_id, "purpose", raw))?,
        None => return Err(Unreadable::new(job_id, "purpose", "none stored")),
    };
    let phase = Phase::parse(phase).ok_or_else(|| Unreadable::new(job_id, "phase", phase))?;
    let scope: Scope =
        serde_json::from_str(scope).map_err(|error| Unreadable::new(job_id, "scope", error))?;
    let summary = result
        .map(|raw| serde_json::from_str(&raw))
        .transpose()
        .map_err(|error| Unreadable::new(job_id, "summary", error))?;
    Ok(RepairSnapshot {
        purpose,
        phase,
        scope,
        target_matcher_version,
        summary,
    })
}

/// The job and its snapshot when the job is a repair of `purpose`.
pub fn repair_job(
    conn: &Connection,
    job_id: &str,
    purpose: Purpose,
) -> Result<(OperationJob, RepairSnapshot), OperationError> {
    let job = jobs::job(conn, job_id)?.filter(|job| job.kind == OperationKind::Repair.as_str());
    let snap = snapshot(conn, job_id)?.transpose()?;
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
                // A person's choice, made now or carried over from v2, is
                // never rechecked for removal.
                None => format!("AND i.decision_source IN {REVISABLE}"),
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
        // Play filing works through play rows, not a list of albums.
        Purpose::PlayHistoryFiling => {
            values.clear();
            "FROM local_albums a WHERE 0".to_owned()
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
    // A requester whose account is gone leaves the job unattributed.
    let requested_by: Option<String> = match &new.requested_by_user_id {
        Some(user_id) => tx
            .query_row(
                "SELECT id FROM auth_users WHERE id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()?,
        None => None,
    };
    jobs::insert(
        tx,
        &NewJob {
            id: &new.job_id,
            kind: OperationKind::Repair,
            requested_by: requested_by.as_deref(),
            idempotency_key: Some(&new.idempotency_key),
            catalog_revision: Some(catalog_revision),
            expected_work_count: 0,
            now,
        },
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
        legacy_only: false,
        scan_run_id: new.scan_run_id.clone(),
    };
    tx.execute(
        "INSERT INTO library_repair_snapshots (job_id, purpose, scope_json, \
         source_matcher_version, target_matcher_version, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            new.job_id,
            new.purpose,
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
    job_after(tx, &new.job_id)
}

/// The hygiene job queued once per database (v2's backfill key), so an
/// upgrade's leftovers are cleaned without anyone asking.
pub const HYGIENE_BACKFILL_KEY: &str = "catalog-identity-hygiene:v1:backfill";

/// Albums the cleanup could apply to: imported groupings, automatic
/// albums with no file left, and albums whose row names the "Unknown
/// Artist" placeholder while a file's tags name an artist, unless a
/// curator undid that repair on the album.
const HYGIENE_CANDIDATE: &str = "(a.grouping_source = 'legacy_import' \
     OR (a.grouping_source = 'automatic' AND NOT EXISTS \
     (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id)) \
     OR (a.album_artist_name = 'Unknown Artist' \
     AND a.grouping_locked = 0 AND a.grouping_source <> 'manual' \
     AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
     AND t.availability = 'indexed' AND t.album_artist_provenance = 'tag' \
     AND t.album_artist_name <> 'Unknown Artist') \
     AND NOT EXISTS (SELECT 1 FROM library_catalog_actions u \
     WHERE u.local_album_id = a.id AND u.action_kind = 'album_artist_undone')))";

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

pub fn queued(conn: &Connection, idempotency_key: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_operation_jobs WHERE idempotency_key = ?1)",
        params![idempotency_key],
        |row| row.get(0),
    )
}

/// Where the cleanup after identification has looked up to: the
/// finished identification job last seen, by `(updated_ms, id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    pub at_ms: i64,
    pub job_id: String,
}

/// Identification states that end a job.
const IDENTIFY_FINISHED: &str = "('succeeded','failed','attention')";

/// Most finished identification jobs one cleanup covers.
pub const IDENTIFIED_PAGE: usize = 200;

/// The newest finished identification job, or the start of time when
/// there is none.
fn newest_identified(conn: &Connection) -> rusqlite::Result<Watermark> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT updated_ms, id FROM library_identify_jobs \
                 WHERE state IN {IDENTIFY_FINISHED} ORDER BY updated_ms DESC, id DESC LIMIT 1"
            ),
            [],
            |row| {
                Ok(Watermark {
                    at_ms: row.get(0)?,
                    job_id: row.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or(Watermark {
            at_ms: 0,
            job_id: String::new(),
        }))
}

/// The stored watermark. The migration that made it seeded it, so a
/// missing row is an error: it is logged and `Err` carries the newest
/// finished job to store, so nothing identified before is looked at again.
pub fn identified_watermark(conn: &Connection) -> rusqlite::Result<Result<Watermark, Watermark>> {
    let stored = conn
        .query_row(
            "SELECT identified_at, identified_job_id FROM library_repair_watermarks \
             WHERE singleton = 1",
            [],
            |row| {
                Ok(Watermark {
                    at_ms: row.get(0)?,
                    job_id: row.get(1)?,
                })
            },
        )
        .optional()?;
    match stored {
        Some(mark) => Ok(Ok(mark)),
        None => {
            tracing::error!(
                "the identification cleanup watermark is missing; it starts again from the newest finished identification"
            );
            Ok(Err(newest_identified(conn)?))
        }
    }
}

pub fn set_identified_watermark(tx: &Transaction<'_>, mark: &Watermark) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO library_repair_watermarks (singleton, identified_at, identified_job_id) \
         VALUES (1, ?1, ?2) ON CONFLICT (singleton) DO UPDATE SET \
         identified_at = excluded.identified_at, identified_job_id = excluded.identified_job_id",
        params![mark.at_ms, mark.job_id],
    )?;
    Ok(())
}

/// One page of finished identification jobs after a watermark.
#[derive(Debug, Clone, Default)]
pub struct IdentifiedPage {
    /// Albums of the page the cleanup could apply to, each once.
    pub albums: Vec<String>,
    /// The page's last job: the watermark once the page is covered.
    pub last: Option<Watermark>,
    /// The page was full, so more may follow.
    pub full: bool,
}

/// The next [`IDENTIFIED_PAGE`] finished identification jobs after
/// `after`, oldest first.
pub fn identified_after(conn: &Connection, after: &Watermark) -> rusqlite::Result<IdentifiedPage> {
    let mut stmt = conn.prepare(&format!(
        "SELECT j.updated_ms, j.id, a.id, \
         a.retired_into_album_id IS NULL AND {HYGIENE_CANDIDATE} \
         FROM library_identify_jobs j JOIN local_albums a ON a.id = j.local_album_id \
         WHERE j.state IN {IDENTIFY_FINISHED} \
         AND (j.updated_ms > ?1 OR (j.updated_ms = ?1 AND j.id > ?2)) \
         ORDER BY j.updated_ms, j.id LIMIT ?3"
    ))?;
    let rows = stmt
        .query_map(
            params![after.at_ms, after.job_id, IDENTIFIED_PAGE as i64],
            |row| {
                Ok((
                    Watermark {
                        at_ms: row.get(0)?,
                        job_id: row.get(1)?,
                    },
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let full = rows.len() == IDENTIFIED_PAGE;
    let mut seen = std::collections::HashSet::new();
    let mut page = IdentifiedPage {
        full,
        ..IdentifiedPage::default()
    };
    for (mark, album, candidate) in rows {
        if candidate && seen.insert(album.clone()) {
            page.albums.push(album);
        }
        page.last = Some(mark);
    }
    Ok(page)
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

/// True once a v2 import has been committed into this database: the
/// migration tool writes one `import_runs` row per committed import.
pub fn carried_from_v2(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM import_runs)", [], |row| {
        row.get(0)
    })
}

/// Marks the one-time identification of albums an older matcher left on
/// another edition, or unmatched, because one file's track id named
/// another edition.
pub const TRACK_ID_BACKFILL_KEY: &str = "track-id-elsewhere:v1:backfill";

/// Albums whose identification may have obeyed a file's stray track id.
/// On an identity, a file carrying release, track and recording ids counts
/// when it names another release than the identity's (by id or a merged
/// id), or when the stored document of the release it names lacks its
/// track id. An unmatched or unconfirmed album counts when its recorded
/// candidates show a release vetoed by a file's track id (its documents
/// may have aged out). Only automatic identities and flagged albums; never
/// an album a person decided: a chosen or imported identity, a match taken
/// off, or a review a person settled.
pub fn track_id_backfill_albums(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id FROM local_albums a \
         LEFT JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?1 \
         LEFT JOIN library_album_match_state m ON m.local_album_id = a.id \
         WHERE a.retired_into_album_id IS NULL \
         AND (i.local_album_id IS NOT NULL OR m.local_album_id IS NOT NULL) \
         AND (i.decision_source IS NULL OR i.decision_source IN {REVISABLE}) \
         AND COALESCE(m.reason_code, '') != 'DETACHED_BY_PERSON' \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = a.id AND r.state IN ('rejected','dismissed','excluded')) \
         AND ((i.local_album_id IS NOT NULL AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
             AND COALESCE(t.embedded_release_mbid, '') != '' \
             AND COALESCE(t.embedded_release_track_mbid, '') != '' \
             AND COALESCE(t.embedded_recording_mbid, '') != '' \
             AND ((i.local_album_id IS NOT NULL \
                 AND lower(t.embedded_release_mbid) != lower(COALESCE(i.release_mbid, '')) \
                 AND NOT EXISTS (SELECT 1 FROM library_management_metadata_snapshots s, \
                     json_each(s.canonical_payload_json, '$.old_ids') old \
                     WHERE s.provider = ?1 AND s.entity_kind = 'release' \
                     AND s.entity_id = lower(i.release_mbid) \
                     AND lower(old.value) = lower(t.embedded_release_mbid))) \
             OR EXISTS (SELECT 1 FROM library_management_metadata_snapshots s \
                 WHERE s.provider = ?1 AND s.entity_kind = 'release' \
                 AND s.entity_id = lower(t.embedded_release_mbid) \
                 AND NOT EXISTS (SELECT 1 FROM json_each(s.canonical_payload_json, '$.tracks') rt \
                     WHERE lower(json_extract(rt.value, '$.id')) \
                         = lower(t.embedded_release_track_mbid)))))) \
         OR EXISTS (SELECT 1 FROM json_each(m.candidates_json) candidate, \
             json_each(candidate.value, '$.track_evidence') track, \
             json_each(track.value, '$.evidence_kinds') kind \
             WHERE kind.value = 'release_track_mbid_conflict')) \
         ORDER BY a.id"
    ))?;
    stmt.query_map(params![PROVIDER], |row| row.get(0))?
        .collect()
}

/// Marks the one-time identification of albums matched to a release
/// group with no release in it, as v2 left some.
pub const GROUP_ONLY_BACKFILL_KEY: &str = "group-only-edition:v1:backfill";

/// Albums whose automatic match names a release group but no release,
/// with every indexed file in a folder identified automatically. Never an
/// album a person decided: a chosen or imported match, a custom edition,
/// or a review a person settled.
pub fn group_only_backfill_albums(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id FROM local_albums a \
         JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?1 \
         WHERE a.retired_into_album_id IS NULL \
         AND i.release_mbid IS NULL AND i.decision_source IN {REVISABLE} \
         AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = a.id AND r.state IN ('rejected','dismissed','excluded')) \
         AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         AND NOT EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
             AND t.applied_policy != 'automatic') \
         ORDER BY a.id"
    ))?;
    stmt.query_map(params![PROVIDER], |row| row.get(0))?
        .collect()
}

/// Marks the one-time identification of automatic matches confirmed
/// before matching read the release group the files' tags name.
pub const OLDER_CONFIRMATIONS_BACKFILL_KEY: &str = "older-confirmations:v1:backfill";

/// Albums whose automatic match is confirmed on a release outside the one
/// release group every indexed file's tag names. Matching now keeps such
/// a match as a guess, but only when the album is identified again. Never
/// an album a person decided, detached or settled a review of, a custom
/// edition, or one with a file in a folder not identified automatically.
pub fn older_confirmation_albums(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id, i.release_group_mbid FROM local_albums a \
         JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?1 \
         WHERE a.retired_into_album_id IS NULL \
         AND i.release_mbid IS NOT NULL AND i.decision_source IN {REVISABLE} \
         AND NOT EXISTS (SELECT 1 FROM library_album_match_state m \
             WHERE m.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = a.id AND r.state IN ('rejected','dismissed','excluded')) \
         AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         AND NOT EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
             AND t.applied_policy != 'automatic') \
         ORDER BY a.id"
    ))?;
    let confirmed: Vec<(String, String)> = stmt
        .query_map(params![PROVIDER], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut files = conn.prepare(
        "SELECT embedded_release_group_mbid FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed'",
    )?;
    let mut albums = Vec::new();
    for (album, group) in confirmed {
        let tagged: Vec<Option<String>> = files
            .query_map(params![album], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if outside_named_group(&tagged, &group) {
            albums.push(album);
        }
    }
    Ok(albums)
}

/// Marks the one-time identification of automatic matches confirmed
/// before matching moved an album to the edition its files' barcode or
/// catalog number names.
pub const EDITION_IDS_BACKFILL_KEY: &str = "edition-ids:v1:backfill";

/// Albums whose automatic match is confirmed on a release that states
/// another barcode or catalog number than the one most of their files
/// carry, while no file carries a MusicBrainz release ID. Identified again,
/// such an album moves to the edition the files name when one fits. A
/// person's or imported match, a custom edition, a detached album, a
/// settled review and folders that keep their own tags are left alone,
/// as in [`older_confirmation_albums`]. None while one of them is not
/// ready to tell: a file whose edition tags the scan has not read yet, or
/// a release whose document is still being fetched; the next look (at
/// start or after a scan) asks again.
pub fn edition_id_albums(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id, i.release_mbid, EXISTS (SELECT 1 FROM library_artist_reconciliation_state s \
             WHERE s.local_album_id = a.id AND s.state = 'waiting_for_identity' \
             AND s.reason_code = ?2) \
         FROM local_albums a \
         JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?1 \
         WHERE a.retired_into_album_id IS NULL \
         AND i.release_mbid IS NOT NULL AND i.decision_source IN {REVISABLE} \
         AND NOT EXISTS (SELECT 1 FROM library_album_match_state m \
             WHERE m.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = a.id AND r.state IN ('rejected','dismissed','excluded')) \
         AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         AND NOT EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
             AND (t.applied_policy != 'automatic' \
                 OR trim(COALESCE(t.embedded_release_mbid, '')) != '')) \
         ORDER BY a.id"
    ))?;
    let confirmed: Vec<(String, String, bool)> = stmt
        .query_map(params![PROVIDER, RELEASE_DOCUMENT_MISSING], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut files = conn.prepare(
        "SELECT h.local_track_id IS NOT NULL, h.barcode, h.catalog_number \
         FROM local_tracks t LEFT JOIN local_track_edition_tags h ON h.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND t.availability = 'indexed'",
    )?;
    let mut albums = Vec::new();
    let mut waiting = 0usize;
    for (album, release, fetching) in confirmed {
        let tags: Vec<(bool, Option<String>, Option<String>)> = files
            .query_map(params![album], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if tags.iter().any(|(read, _, _)| !read) {
            waiting += 1;
            continue;
        }
        let ids = edition_ids_of(
            tags.iter()
                .map(|(_, barcode, catalog)| (barcode.as_deref(), catalog.as_deref())),
        );
        if ids.barcode.is_none() && ids.catalog_number.is_none() {
            continue;
        }
        let Some(held) = stored_release(conn, &release, 0.0)? else {
            if fetching {
                waiting += 1;
            }
            continue;
        };
        if id_stance(&held, &ids) == IdStance::Contradicts {
            albums.push(album);
        }
    }
    if waiting > 0 {
        tracing::debug!(
            waiting,
            "edition re-check waits for edition tags or release documents"
        );
        return Ok(Vec::new());
    }
    Ok(albums)
}

/// Every file names the same valid release group, and it is not `group`.
fn outside_named_group(files: &[Option<String>], group: &str) -> bool {
    let named: Option<Vec<String>> = files
        .iter()
        .map(|tagged| {
            let tagged = tagged.as_deref()?.trim().to_ascii_lowercase();
            is_valid_mbid(&tagged).then_some(tagged)
        })
        .collect();
    named.is_some_and(|named| {
        named.first().is_some_and(|first| {
            named.iter().all(|other| other == first) && !first.eq_ignore_ascii_case(group.trim())
        })
    })
}

/// Record that a one-time identification was queued: a finished repair
/// job under its key ([`TRACK_ID_BACKFILL_KEY`],
/// [`GROUP_ONLY_BACKFILL_KEY`], [`OLDER_CONFIRMATIONS_BACKFILL_KEY`]), so
/// it never runs again on this database.
pub fn mark_backfill(
    tx: &Transaction<'_>,
    job_id: &str,
    key: &str,
    now: f64,
) -> rusqlite::Result<()> {
    jobs::insert_done(
        tx,
        job_id,
        OperationKind::Repair,
        "IDENTIFY_QUEUED",
        key,
        now,
    )
}

// ---------------------------------------------------------------------------
// Estimates and history.
// ---------------------------------------------------------------------------

fn active_count(conn: &Connection, purpose: Purpose) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM library_operation_jobs j \
         JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.kind = ?2 AND j.state IN (?3, ?4, ?5, ?6) \
         AND s.purpose = ?1",
        params![
            purpose,
            OperationKind::Repair,
            OperationState::Queued,
            OperationState::Running,
            OperationState::Paused,
            OperationState::Ready,
        ],
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

/// One page of a purpose's jobs with their snapshots, newest first.
/// `before` is the last page's `(created_at, id)`.
pub fn history(
    conn: &Connection,
    purpose: Purpose,
    limit: usize,
    before: Option<(f64, String)>,
) -> rusqlite::Result<Vec<(String, Result<RepairSnapshot, Unreadable>)>> {
    let (created, id) = before.unzip();
    let mut stmt = conn.prepare(
        "SELECT j.id, s.purpose, s.phase, s.scope_json, s.target_matcher_version, \
         s.result_json FROM library_operation_jobs j \
         JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.kind = ?5 AND s.purpose = ?1 \
         AND (?2 IS NULL OR j.created_at < ?2 OR (j.created_at = ?2 AND j.id < ?3)) \
         ORDER BY j.created_at DESC, j.id DESC LIMIT ?4",
    )?;
    stmt.query_map(
        params![purpose, created, id, limit as i64, OperationKind::Repair],
        |row| {
            let job_id: String = row.get(0)?;
            let phase: String = row.get(2)?;
            let scope: String = row.get(3)?;
            let snap = decode_snapshot(
                &job_id,
                row.get(1)?,
                &phase,
                &scope,
                row.get(4)?,
                row.get(5)?,
            );
            Ok((job_id, snap))
        },
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
                        manual: row.get::<_, DecisionSource>(2)?.is_persons_choice(),
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

/// Close one work item and count it on the job, while `worker` holds
/// the job.
#[allow(clippy::too_many_arguments)]
pub fn finish_work(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    ordinal: i64,
    state: WorkState,
    failure_code: Option<&str>,
    result: Option<&serde_json::Value>,
    now: f64,
) -> rusqlite::Result<()> {
    if !jobs::holds(tx, job_id, worker)? {
        return Ok(());
    }
    jobs::finish_work(
        tx,
        job_id,
        ordinal,
        state,
        failure_code,
        result.map(|value| value.to_string()).as_deref(),
        now,
    )
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
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?14, ?13, ?13) \
         ON CONFLICT (job_id, local_album_id, finding_code) DO UPDATE SET \
         expected_album_revision = excluded.expected_album_revision, \
         expected_identity_revision = excluded.expected_identity_revision, \
         confidence = excluded.confidence, reason_code = excluded.reason_code, \
         apply_eligible = excluded.apply_eligible, \
         suggested_release_mbid = excluded.suggested_release_mbid, \
         suggested_release_group_mbid = excluded.suggested_release_group_mbid, \
         suggested_edition_json = excluded.suggested_edition_json, state = excluded.state, \
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
            now,
            FindingState::Open
        ],
    )?;
    finish_work(
        tx,
        job_id,
        worker,
        ordinal,
        WorkState::Succeeded,
        None,
        Some(&serde_json::json!({
            "finding_code": verdict.finding_code.as_str(),
            "reason_code": verdict.reason_code,
        })),
        now,
    )?;
    Ok(())
}

/// Apply a suggestion that passed the automatic acceptance gate, if the
/// album still looks the way it was checked (the identity command checks
/// that inside its write). The finding becomes
/// `exact_release_auto_accepted`; otherwise it stays a suggestion.
pub fn auto_accept(
    tx: &Transaction<'_>,
    job_id: &str,
    album_id: &str,
    verdict: &Verdict,
    now: f64,
) -> Result<bool, OperationError> {
    let (Some(candidate), Some(guard)) = (
        verdict.payload.candidate.as_ref(),
        verdict.payload.guard.as_ref(),
    ) else {
        return Ok(false);
    };
    if !verdict.payload.auto_accept {
        return Ok(false);
    }
    let expected = crate::identity::Expected {
        album_revision: guard.album_revision,
        input_revision: guard.input_revision.clone(),
        identity_revision: guard.identity_revision.clone(),
    };
    let accepted = crate::operations::choice::accept_automatic(
        tx,
        album_id,
        candidate,
        "EDITION_AUTO_ACCEPTED",
        Some(job_id),
        Some(&expected),
        now,
    )?;
    if accepted.is_none() {
        return Ok(false);
    }
    tx.execute(
        "UPDATE library_identity_repair_findings SET finding_code = ?4, \
         reason_code = 'EXACT_EDITION_AUTO_ACCEPTED', apply_eligible = 0, state = ?5, \
         apply_result = ?6, updated_at = ?3, row_revision = row_revision + 1 \
         WHERE job_id = ?1 AND local_album_id = ?2 AND finding_code = ?7",
        params![
            job_id,
            album_id,
            now,
            FindingCode::ExactReleaseAutoAccepted,
            FindingState::Applied,
            ApplyResult::EditionAutoAccepted,
            FindingCode::ExactReleaseSuggested
        ],
    )?;
    Ok(true)
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
    let summary = serde_json::to_string(&summary)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?;
    tx.execute(
        "UPDATE library_repair_snapshots SET result_json = ?2 WHERE job_id = ?1",
        params![job_id, summary],
    )?;
    jobs::report_ready(tx, job_id, worker, "DRY_RUN_READY", now)?;
    jobs::job(tx, job_id)
}

/// End `worker`'s running job in `state` with `code`, its counts as the
/// work left them.
pub fn end(
    tx: &Transaction<'_>,
    job_id: &str,
    worker: &str,
    state: OperationState,
    code: &str,
    now: f64,
) -> rusqlite::Result<Option<OperationJob>> {
    jobs::finish(tx, job_id, worker, state, code, Tally::AsCounted, now)?;
    jobs::job(tx, job_id)
}

fn summarize(conn: &Connection, job_id: &str, snap: &RepairSnapshot) -> rusqlite::Result<Summary> {
    let mut summary = Summary::new(snap.purpose, snap.target_matcher_version.clone());
    let mut stmt = conn.prepare(
        "SELECT finding_code, reason_code, COUNT(*) FROM library_identity_repair_findings \
         WHERE job_id = ?1 GROUP BY finding_code, reason_code",
    )?;
    let mut rows = stmt.query(params![job_id])?;
    while let Some(row) = rows.next()? {
        let (finding, reason, count): (FindingCode, String, i64) =
            (row.get(0)?, row.get(1)?, row.get(2)?);
        *summary
            .counts_by_finding
            .entry(finding.as_str().to_owned())
            .or_default() += count;
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
    let count = |code: FindingCode| {
        summary
            .counts_by_finding
            .get(code.as_str())
            .copied()
            .unwrap_or(0)
    };
    summary.estimated_apply_changes = match snap.purpose {
        Purpose::ManagementReadiness => {
            count(FindingCode::MappingReady) + count(FindingCode::ExactReleaseSuggested)
        }
        _ => count(FindingCode::SafeDetach),
    };
    summary.ready_album_count = count(FindingCode::Ready);
    summary.mapping_candidate_count = count(FindingCode::MappingReady);
    summary.exact_release_required_count =
        count(FindingCode::ExactReleaseRequired) + count(FindingCode::ExactReleaseSuggested);
    summary.needs_review_count = count(FindingCode::NeedsReview);
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
    if job.state != OperationState::Ready || job.row_revision != expected_row_revision {
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
         WHERE f.job_id = ?1 AND f.apply_eligible = 1 AND f.state = ?3",
        params![job_id, now, FindingState::Open],
    )?;
    jobs::queue_work(tx, job_id, staged as i64, now)?;
    tx.execute(
        "UPDATE library_repair_snapshots SET phase = ?2 WHERE job_id = ?1",
        params![job_id, Phase::Apply],
    )?;
    job_after(tx, job_id)
}

/// The code a dismissed preparation report ends with.
const DISCARDED: &str = "IDENTITY_PREPARATION_DISCARDED";

/// Dismiss a ready preparation report without changing anything.
pub fn discard(
    tx: &Transaction<'_>,
    job_id: &str,
    expected_row_revision: i64,
    now: f64,
) -> Result<OperationJob, OperationError> {
    let (job, snap) = repair_job(tx, job_id, Purpose::ManagementReadiness)?;
    if job.state == OperationState::Cancelled && job.terminal_code.as_deref() == Some(DISCARDED) {
        return Ok(job);
    }
    if job.state != OperationState::Ready
        || snap.phase != Phase::DryRun
        || job.row_revision != expected_row_revision
    {
        return Err(OperationError::Conflict(reasons::REPORT_CHANGED));
    }
    jobs::cancel_ready(tx, job_id, DISCARDED, now)?;
    job_after(tx, job_id)
}

/// The open, eligible finding Apply works on for one album.
#[derive(Debug, Clone)]
pub struct OpenFinding {
    pub id: String,
    pub finding_code: FindingCode,
    pub payload: FindingPayload,
}

/// `Err` inside when the finding's payload does not decode.
pub fn open_finding(
    conn: &Connection,
    job_id: &str,
    album_id: &str,
) -> rusqlite::Result<Option<Result<OpenFinding, Unreadable>>> {
    let row: Option<(String, FindingCode, String)> = conn
        .query_row(
            "SELECT id, finding_code, suggested_edition_json \
             FROM library_identity_repair_findings \
             WHERE job_id = ?1 AND local_album_id = ?2 AND apply_eligible = 1 AND state = ?3 \
             ORDER BY id LIMIT 1",
            params![job_id, album_id, FindingState::Open],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(row.map(|(id, finding_code, raw)| {
        let payload = serde_json::from_str(&raw)
            .map_err(|error| Unreadable::new(job_id, "finding payload", error))?;
        Ok(OpenFinding {
            id,
            finding_code,
            payload,
        })
    }))
}

/// Record what Apply did with a finding.
pub fn settle_finding(
    tx: &Transaction<'_>,
    finding_id: &str,
    state: FindingState,
    apply_result: ApplyResult,
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
pub fn category_codes(purpose: Purpose, category: &str) -> Option<&'static [FindingCode]> {
    use FindingCode as F;
    let codes: &[FindingCode] = match (purpose, category) {
        (Purpose::ManagementReadiness, "ready") => &[F::Ready],
        (Purpose::ManagementReadiness, "mapping_ready") => &[F::MappingReady],
        (Purpose::ManagementReadiness, "exact_release_required") => {
            &[F::ExactReleaseRequired, F::ExactReleaseSuggested]
        }
        (Purpose::ManagementReadiness, "exact_release_auto_accepted") => {
            &[F::ExactReleaseAutoAccepted]
        }
        (_, "needs_review") => &[F::NeedsReview],
        (_, "unverifiable") => &[F::Unverifiable, F::Stale],
        (Purpose::ExistingMatches, "valid") => &[F::Valid],
        (Purpose::ExistingMatches, "safe_detach") => &[F::SafeDetach],
        (Purpose::ExistingMatches, "manual_identity") => &[F::ManualIdentity],
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
    codes: Option<&[FindingCode]>,
    cursor: Option<(f64, String)>,
    limit: usize,
) -> rusqlite::Result<FindingPage> {
    let current = if purpose == Purpose::ManagementReadiness {
        format!(
            "AND ((f.state = '{open}' AND a.row_revision = f.expected_album_revision \
             AND ((f.expected_identity_revision IS NULL AND i.row_revision IS NULL) \
             OR i.row_revision = f.expected_identity_revision)) \
             OR (f.finding_code = '{accepted}' AND f.state = '{applied}' \
             AND EXISTS (SELECT 1 FROM library_automatic_edition_undo u \
             WHERE u.local_album_id = f.local_album_id AND u.consumed_at IS NULL)))",
            open = FindingState::Open.as_str(),
            accepted = FindingCode::ExactReleaseAutoAccepted.as_str(),
            applied = FindingState::Applied.as_str(),
        )
    } else {
        String::new()
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
        values.extend(
            codes
                .iter()
                .map(|code| Value::Text(code.as_str().to_owned())),
        );
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
            let id: String = row.get(0)?;
            let raw: String = row.get(15)?;
            let mut payload: FindingPayload = serde_json::from_str(&raw)
                .map_err(|error| Unreadable::new(job_id, "finding payload", error))?;
            if payload.summary.is_none() {
                // v2 kept the suggestion's summary at the top level.
                payload.summary = serde_json::from_str::<EditionSummary>(&raw)
                    .ok()
                    .filter(|summary| !summary.title.is_empty());
            }
            Ok(FindingRow {
                id,
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
        .query_map(params![job_id], |row| {
            Ok((
                row.get::<_, FindingCode>(0)?.as_str().to_owned(),
                row.get(1)?,
            ))
        })?
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
