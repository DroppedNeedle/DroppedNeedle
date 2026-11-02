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
use crate::edition::decisions::live_automatic_edition_undo;
use crate::identify::artist_links::RELEASE_DOCUMENT_MISSING;
use crate::identify::evidence::edition_ids_of;
use crate::identity::reads::stored_release;
use crate::jobs::OperationJob;
use crate::jobs::OperationKind;
use crate::jobs::models::OperationError;
use crate::jobs::store::job_after;
use crate::jobs::{self, NewJob, OperationState, Tally, WorkState};
use crate::matching::decide::{IdStance, id_stance};
use crate::reidentify::store::album_revisions;
use crate::repairs::sql::{
    ConfirmedAlbum, CurrentFindingCount, EditionCandidate, EditionTags, FindingCount,
    FindingPageRow, HistoryRow, IdentifiedRow, OpenFindingRow, PreparationRow, RepairScope,
    RepairScopeKind, RootAlbumCount, SnapshotRow, SubjectIdentity, SubjectTrack, WatermarkRow,
};
use droppedneedle_kernel::db::statement::ListLen;

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
    let row = conn
        .query_row(
            crate::repairs::sql::SNAPSHOT.text(),
            params![job_id],
            SnapshotRow::read,
        )
        .optional()?;
    Ok(row.map(|row| {
        decode_snapshot(
            job_id,
            row.purpose,
            &row.phase,
            &row.scope_json,
            row.target_matcher_version,
            row.result_json,
        )
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

/// The shape of the statement for the albums a job of `purpose` covers,
/// with its bound values in order. One predicate for counting and for the
/// work list.
fn scope_sql(
    purpose: Purpose,
    root_ids: &[String],
    album_ids: &[String],
    scan_run_id: Option<&str>,
    source_matcher_version: Option<&str>,
) -> (RepairScope, Vec<Value>) {
    let mut values: Vec<Value> = root_ids.iter().cloned().map(Value::Text).collect();
    let mut albums = 0;
    let mut after_run = false;
    let kind = match purpose {
        Purpose::ExistingMatches => match source_matcher_version {
            Some(version) => {
                values.push(Value::Text(version.to_owned()));
                RepairScopeKind::MatchesOfVersion
            }
            // A person's choice, made now or carried over from v2, is
            // never rechecked for removal.
            None => RepairScopeKind::RevisableMatches,
        },
        Purpose::ManagementReadiness => RepairScopeKind::ManagementReadiness,
        Purpose::CatalogIdentityHygiene => {
            values.extend(album_ids.iter().cloned().map(Value::Text));
            albums = album_ids.len();
            if let Some(run) = scan_run_id {
                values.extend((0..3).map(|_| Value::Text(run.to_owned())));
                after_run = true;
            }
            RepairScopeKind::CatalogIdentityHygiene
        }
        // Play filing works through play rows, not a list of albums.
        Purpose::PlayHistoryFiling => {
            values.clear();
            RepairScopeKind::Nothing
        }
    };
    let shape = RepairScope {
        kind,
        roots: ListLen::new(root_ids.len()),
        albums: ListLen::new(albums),
        after_run,
    };
    (shape, values)
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
            crate::repairs::sql::JOB_OF_IDEMPOTENCY_KEY.text(),
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
        .query_row(crate::repairs::sql::CATALOG_REVISION.text(), [], |row| {
            row.get(0)
        })
        .optional()?
        .unwrap_or(0);
    // A requester whose account is gone leaves the job unattributed.
    let requested_by: Option<String> = match &new.requested_by_user_id {
        Some(user_id) => tx
            .query_row(
                crate::repairs::sql::USER_EXISTS.text(),
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
    let (scope, scope_values) = scope_sql(
        new.purpose,
        &new.root_ids,
        &new.album_ids,
        new.scan_run_id.as_deref(),
        new.source_matcher_version.as_deref(),
    );
    let mut values = vec![
        Value::Text(new.job_id.clone()),
        Value::Text(action.to_owned()),
        Value::Integer(crate::clock::ms(now)),
    ];
    values.extend(scope_values);
    let staged = tx.execute(
        &crate::repairs::sql::queue_repair_work(scope),
        params_from_iter(values),
    )?;
    let scope = Scope {
        root_ids: new.root_ids.clone(),
        album_ids: new.album_ids.clone(),
        legacy_only: false,
        scan_run_id: new.scan_run_id.clone(),
    };
    tx.execute(
        crate::repairs::sql::CREATE_REPAIR_SNAPSHOT.text(),
        params![
            new.job_id,
            new.purpose,
            serde_json::to_string(&scope)
                .map_err(|error| OperationError::Store(error.to_string()))?,
            new.source_matcher_version,
            new.target_matcher_version,
            crate::clock::ms(now)
        ],
    )?;
    tx.execute(
        crate::repairs::sql::CREATE_REPAIR_MATERIALIZATION.text(),
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
            crate::clock::ms(now)
        ],
    )?;
    tx.execute(
        crate::repairs::sql::SET_EXPECTED_WORK_COUNT.text(),
        params![new.job_id, staged as i64],
    )?;
    job_after(tx, &new.job_id)
}

/// The hygiene job queued once per database (v2's backfill key), so an
/// upgrade's leftovers are cleaned without anyone asking.
pub const HYGIENE_BACKFILL_KEY: &str = "catalog-identity-hygiene:v1:backfill";

pub fn hygiene_candidates_exist(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        crate::repairs::sql::HYGIENE_CANDIDATES_EXIST.text(),
        [],
        |row| row.get(0),
    )
}

pub fn queued(conn: &Connection, idempotency_key: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        crate::repairs::sql::QUEUED.text(),
        params![idempotency_key],
        |row| row.get(0),
    )
}

/// Where the cleanup after identification has looked up to: the
/// finished identification job last seen, by `(updated_at, id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    pub at_ms: i64,
    pub job_id: String,
}

/// Most finished identification jobs one cleanup covers.
pub const IDENTIFIED_PAGE: usize = 200;

fn watermark(row: &rusqlite::Row<'_>) -> rusqlite::Result<Watermark> {
    let row = WatermarkRow::read(row)?;
    Ok(Watermark {
        at_ms: row.at_ms,
        job_id: row.job_id,
    })
}

/// The newest finished identification job, or the start of time when
/// there is none.
fn newest_identified(conn: &Connection) -> rusqlite::Result<Watermark> {
    Ok(conn
        .query_row(crate::repairs::sql::NEWEST_IDENTIFIED.text(), [], watermark)
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
            crate::repairs::sql::IDENTIFIED_WATERMARK.text(),
            [],
            watermark,
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
        crate::repairs::sql::SET_IDENTIFIED_WATERMARK.text(),
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
    let mut stmt = conn.prepare(crate::repairs::sql::IDENTIFIED_AFTER.text())?;
    let rows = stmt
        .query_map(
            params![after.at_ms, after.job_id, IDENTIFIED_PAGE as i64],
            IdentifiedRow::read,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let full = rows.len() == IDENTIFIED_PAGE;
    let mut seen = std::collections::HashSet::new();
    let mut page = IdentifiedPage {
        full,
        ..IdentifiedPage::default()
    };
    for row in rows {
        if row.candidate && seen.insert(row.album_id.clone()) {
            page.albums.push(row.album_id);
        }
        page.last = Some(Watermark {
            at_ms: row.at_ms,
            job_id: row.job_id,
        });
    }
    Ok(page)
}

/// The latest scan run that ended, however it ended (finished, stopped,
/// failed or superseded): what it changed before it ended still needs the
/// per-scan cleanup.
pub fn latest_ended_scan(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row(crate::repairs::sql::LATEST_ENDED_SCAN.text(), [], |row| {
        row.get(0)
    })
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
        &crate::repairs::sql::count_repair_scope(scope),
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
    conn.query_row(crate::repairs::sql::CARRIED_FROM_V2.text(), [], |row| {
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
    let mut stmt = conn.prepare(crate::repairs::sql::TRACK_ID_BACKFILL_ALBUMS.text())?;
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
    let mut stmt = conn.prepare(crate::repairs::sql::GROUP_ONLY_BACKFILL_ALBUMS.text())?;
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
    let mut stmt = conn.prepare(crate::repairs::sql::CONFIRMED_ALBUMS_WITH_GROUP.text())?;
    let confirmed: Vec<ConfirmedAlbum> = stmt
        .query_map(params![PROVIDER], ConfirmedAlbum::read)?
        .collect::<rusqlite::Result<_>>()?;
    let mut files = conn.prepare(crate::repairs::sql::OLDER_CONFIRMATION_ALBUMS.text())?;
    let mut albums = Vec::new();
    for album in confirmed {
        let tagged: Vec<Option<String>> = files
            .query_map(params![album.id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if outside_named_group(&tagged, &album.release_group_mbid) {
            albums.push(album.id);
        }
    }
    Ok(albums)
}

/// Marks the one-time identification of automatic matches confirmed
/// before matching moved an album to the edition its files' barcode or
/// catalog number names.
pub const EDITION_IDS_BACKFILL_KEY: &str = "edition-ids:v1:backfill";

/// What a one-time pass found: the albums to identify again, and whether
/// the pass is done for good, so its key is marked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Backfill {
    pub albums: Vec<String>,
    pub settled: bool,
}

impl Backfill {
    /// A pass that is done once it found an album.
    pub fn found(albums: Vec<String>) -> Self {
        let settled = !albums.is_empty();
        Self { albums, settled }
    }
}

/// Albums whose automatic match is confirmed on a release that states
/// another barcode or catalog number than the one most of their files
/// carry, while no file carries a MusicBrainz release ID. Identified again,
/// such an album moves to the edition the files name when one fits. A
/// person's or imported match, a custom edition, a detached album, a
/// settled review and folders that keep their own tags are left alone,
/// as in [`older_confirmation_albums`]. None while one of them is not
/// ready to tell: a file whose edition tags the scan has not read yet, or
/// a release whose document is still being fetched; the next look (at
/// start or after a scan) asks again. Settled once a look saw at least one
/// such album and none of them waited, whether or not one moved.
pub fn edition_id_albums(conn: &Connection) -> rusqlite::Result<Backfill> {
    let mut stmt = conn.prepare(crate::repairs::sql::EDITION_ID_ALBUMS.text())?;
    let confirmed: Vec<EditionCandidate> = stmt
        .query_map(
            params![PROVIDER, RELEASE_DOCUMENT_MISSING],
            EditionCandidate::read,
        )?
        .collect::<rusqlite::Result<_>>()?;
    let candidates = confirmed.len();
    let mut files = conn.prepare(crate::repairs::sql::ALBUM_EDITION_TAGS.text())?;
    let mut albums = Vec::new();
    let mut waiting = 0usize;
    for album in confirmed {
        if album.unread {
            waiting += 1;
            continue;
        }
        if !album.tagged {
            continue;
        }
        let tags: Vec<EditionTags> = files
            .query_map(params![album.id], EditionTags::read)?
            .collect::<rusqlite::Result<_>>()?;
        let ids = edition_ids_of(
            tags.iter()
                .map(|tag| (tag.barcode.as_deref(), tag.catalog_number.as_deref())),
        );
        if ids.barcode.is_none() && ids.catalog_number.is_none() {
            continue;
        }
        let Some(held) = stored_release(conn, &album.release_mbid, 0.0)? else {
            if album.fetching {
                waiting += 1;
            }
            continue;
        };
        if id_stance(&held, &ids) == IdStance::Contradicts {
            albums.push(album.id);
        }
    }
    if waiting > 0 {
        tracing::debug!(
            waiting,
            "edition re-check waits for edition tags or release documents"
        );
        return Ok(Backfill::default());
    }
    Ok(Backfill {
        albums,
        settled: candidates > 0,
    })
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
        crate::repairs::sql::ACTIVE_COUNT.text(),
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
        &crate::repairs::sql::count_repair_scope(scope),
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
    let (_, values) = scope_sql(Purpose::ManagementReadiness, root_ids, &[], None, None);
    let mut estimate = PreparationEstimate::default();
    let mut stmt = conn.prepare(&crate::repairs::sql::preparation_estimate(ListLen::new(
        root_ids.len(),
    )))?;
    let mut rows = stmt.query(params_from_iter(values))?;
    while let Some(row) = rows.next()? {
        let row = PreparationRow::read(row)?;
        estimate.album_count += 1;
        if row.custom_current {
            estimate.ready_album_count += 1;
        } else if row.release_group_mbid.is_none() || row.release_mbid.is_none() {
            estimate.exact_release_required_count += 1;
        } else if row.mapped == row.tracks {
            estimate.ready_album_count += 1;
        } else {
            estimate.mapping_required_count += 1;
        }
    }
    estimate.queued_preparation_count = active_count(conn, Purpose::ManagementReadiness)?;
    Ok(estimate)
}

/// One page of a purpose's jobs with their snapshots, newest first.
/// `before` is the last page's `(created_at, id)`.
pub fn history(
    conn: &Connection,
    purpose: Purpose,
    limit: usize,
    before: Option<(f64, String)>,
) -> rusqlite::Result<Vec<(String, Result<RepairSnapshot, Unreadable>)>> {
    let (created, id) = before.unzip();
    let mut stmt = conn.prepare(crate::repairs::sql::HISTORY.text())?;
    stmt.query_map(
        params![
            purpose,
            created.map(crate::clock::ms),
            id,
            limit as i64,
            OperationKind::Repair
        ],
        |row| {
            let row = HistoryRow::read(row)?;
            let snap = decode_snapshot(
                &row.id,
                row.purpose,
                &row.phase,
                &row.scope_json,
                row.target_matcher_version,
                row.result_json,
            );
            Ok((row.id, snap))
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
            crate::repairs::sql::SUBJECT_ALBUM_IDENTITY.text(),
            params![album_id, PROVIDER],
            |row| {
                let row = SubjectIdentity::read(row)?;
                Ok((
                    IdentityState {
                        release_group_mbid: row.release_group_mbid,
                        release_mbid: row.release_mbid,
                        manual: row.decision_source.is_persons_choice(),
                    },
                    row.row_revision,
                ))
            },
        )
        .optional()?;
    let mut stmt = conn.prepare(crate::repairs::sql::SUBJECT_TRACK_IDENTITIES.text())?;
    let tracks = stmt
        .query_map(params![album_id, PROVIDER], |row| {
            let row = SubjectTrack::read(row)?;
            Ok(TrackState {
                local_track_id: row.local_track_id,
                recording_mbid: row.recording_mbid,
                release_mbid: row.release_mbid,
                release_track_mbid: row.release_track_mbid,
                medium_position: row.medium_position,
                release_track_position: row.release_track_position,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let custom_current: Option<bool> = conn
        .query_row(
            crate::repairs::sql::CUSTOM_EDITION_IS_CURRENT.text(),
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    let root_id: String = conn.query_row(
        crate::repairs::sql::SUBJECT_ALBUM_ROOT.text(),
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
        crate::repairs::sql::CLAIM_ALBUM.text(),
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
        crate::repairs::sql::WORK_SUBJECT_REVISION.text(),
        params![job_id, ordinal],
        |row| row.get(0),
    )?;
    let candidate = verdict.payload.candidate.as_ref();
    let payload = serde_json::to_string(&verdict.payload)
        .map_err(|error| OperationError::Store(error.to_string()))?;
    tx.execute(
        crate::repairs::sql::WRITE_FINDING.text(),
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
            crate::clock::ms(now),
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
    let accepted = crate::edition::choice::accept_automatic(
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
        crate::repairs::sql::AUTO_ACCEPT.text(),
        params![
            job_id,
            album_id,
            crate::clock::ms(now),
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
        crate::repairs::sql::WRITE_SNAPSHOT_RESULT.text(),
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
    let mut stmt = conn.prepare(crate::repairs::sql::FINDING_COUNTS.text())?;
    let mut rows = stmt.query(params![job_id])?;
    while let Some(row) = rows.next()? {
        let row = FindingCount::read(row)?;
        *summary
            .counts_by_finding
            .entry(row.finding_code.as_str().to_owned())
            .or_default() += row.finding_count;
        *summary.counts_by_reason.entry(row.reason_code).or_default() += row.finding_count;
        summary.total_identities += row.finding_count;
    }
    let mut stmt = conn.prepare(crate::repairs::sql::FINDING_ALBUMS_BY_ROOT.text())?;
    summary.album_counts_by_root = stmt
        .query_map(params![job_id], |row| {
            let row = RootAlbumCount::read(row)?;
            Ok((row.root_id, row.album_count))
        })?
        .collect::<rusqlite::Result<BTreeMap<String, i64>>>()?;
    summary.input_track_count = conn.query_row(
        crate::repairs::sql::FINDING_TRACK_COUNT.text(),
        params![job_id],
        |row| row.get(0),
    )?;
    summary.playable_after_detach_track_count = summary.input_track_count;
    summary.catalog_snapshot_revision = conn.query_row(
        crate::repairs::sql::JOB_INPUT_REVISION.text(),
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
    tx.execute(crate::repairs::sql::CLEAR_JOB_WORK.text(), params![job_id])?;
    let staged = tx.execute(
        crate::repairs::sql::ADD_APPLY_WORK.text(),
        params![job_id, crate::clock::ms(now), FindingState::Open],
    )?;
    jobs::queue_work(tx, job_id, staged as i64, now)?;
    tx.execute(
        crate::repairs::sql::SET_SNAPSHOT_PHASE.text(),
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
    let row = conn
        .query_row(
            crate::repairs::sql::OPEN_FINDING.text(),
            params![job_id, album_id, FindingState::Open],
            OpenFindingRow::read,
        )
        .optional()?;
    Ok(row.map(|row| {
        let payload = serde_json::from_str(&row.suggested_edition_json)
            .map_err(|error| Unreadable::new(job_id, "finding payload", error))?;
        Ok(OpenFinding {
            id: row.id,
            finding_code: row.finding_code,
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
        crate::repairs::sql::SETTLE_FINDING.text(),
        params![finding_id, state, apply_result, crate::clock::ms(now)],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Findings.
// ---------------------------------------------------------------------------

/// The finding codes a category lists, per purpose.
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
    let current = purpose == Purpose::ManagementReadiness;
    let mut values = vec![Value::Text(job_id.to_owned())];
    let by_code = codes.is_some();
    let code_count = codes.map_or(0, <[FindingCode]>::len);
    if let Some(codes) = codes {
        values.extend(
            codes
                .iter()
                .map(|code| Value::Text(code.as_str().to_owned())),
        );
    }
    let after_cursor = cursor.is_some();
    if let Some((updated, id)) = cursor {
        let updated = crate::clock::ms(updated);
        values.extend([
            Value::Integer(updated),
            Value::Integer(updated),
            Value::Text(id),
        ]);
    }
    values.push(Value::Integer(limit as i64 + 1));
    let page = crate::repairs::sql::findings_page(
        current,
        by_code,
        ListLen::new(code_count),
        after_cursor,
    );
    let mut stmt = conn.prepare(&page)?;
    let mut items = stmt
        .query_map(params_from_iter(values), |row| {
            let row = FindingPageRow::read(row)?;
            let raw = row.suggested_edition_json;
            let mut payload: FindingPayload = serde_json::from_str(&raw)
                .map_err(|error| Unreadable::new(job_id, "finding payload", error))?;
            if payload.summary.is_none() {
                // v2 kept the suggestion's summary at the top level.
                payload.summary = serde_json::from_str::<EditionSummary>(&raw)
                    .ok()
                    .filter(|summary| !summary.title.is_empty());
            }
            Ok(FindingRow {
                id: row.id,
                local_album_id: row.local_album_id,
                album_title: row.album_title,
                album_artist_name: row.album_artist_name,
                album_year: row.album_year,
                cover_available: row.cover_available,
                review_id: row.review_id,
                finding_code: row.finding_code,
                reason_code: row.reason_code,
                confidence: row.confidence,
                apply_eligible: row.apply_eligible,
                state: row.state,
                apply_result: row.apply_result,
                suggested_release_mbid: row.suggested_release_mbid,
                suggested_release_group_mbid: row.suggested_release_group_mbid,
                payload,
                automatic_undo: None,
                updated_at: crate::clock::at_secs(row.updated_at),
                row_revision: row.row_revision,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_more = items.len() > limit;
    items.truncate(limit);
    for item in &mut items {
        item.automatic_undo = live_automatic_edition_undo(conn, &item.local_album_id)?;
    }
    let mut stmt = conn.prepare(&crate::repairs::sql::finding_counts(current))?;
    let current_counts_by_finding = stmt
        .query_map(params![job_id], |row| {
            let row = CurrentFindingCount::read(row)?;
            Ok((row.finding_code.as_str().to_owned(), row.finding_count))
        })?
        .collect::<rusqlite::Result<BTreeMap<String, i64>>>()?;
    let next = has_more
        .then(|| items.last().map(|last| (last.updated_at, last.id.clone())))
        .flatten();
    Ok(FindingPage {
        items,
        next,
        current_counts_by_finding,
        refresh_required: false,
    })
}
