//! Durable contribution state over the application database.
//!
//! [`SqliteContributions`] implements both the contribution store and the
//! album identification context over the 0001 tables v2 already used:
//! `library_contribution_drafts`, `..._callback_tokens` and
//! `..._verification_jobs`, plus the catalog the scan writes
//! (`local_albums`, `local_tracks`) and the identity tables identify owns.
//!
//! Every call is one short transaction on its own connection, run on a
//! blocking thread. Writes take an immediate transaction, read the row,
//! apply the same guards v2's store applied, and write the row back under
//! its row revision, so a check and its write can never interleave with
//! another writer. Linking a release commits the album (and, when free, the
//! artist) MusicBrainz identity as a manual decision in that same
//! transaction, so a crash leaves either both or neither.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension as _, Row, TransactionBehavior, params};

use super::error::ContribError;
use super::models::*;
use super::rules::album_input_revisions;
use super::seams::*;

const PROVIDER: &str = "musicbrainz";
/// Attempt trigger v2 stamped on contribution verification attempts.
const ATTEMPT_TRIGGER: &str = "contribution_submission";
const DAY: f64 = 86_400.0;

impl From<rusqlite::Error> for ContribError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

/// SQLite contribution store and identity reader.
#[derive(Clone)]
pub struct SqliteContributions {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteContributions {
    /// Open against the migrated application database.
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = crate::db::open_connection(path).map_err(|error| error.to_string())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
        conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run one read on a blocking thread.
    async fn read<T: Send + 'static>(
        &self,
        what: &'static str,
        op: impl FnOnce(&Connection) -> Result<T, ContribError> + Send + 'static,
    ) -> Result<T, ContribError> {
        let conn = self.conn.clone();
        let outcome = tokio::task::spawn_blocking(move || op(&Self::lock(&conn)))
            .await
            .map_err(|error| ContribError::Storage(error.to_string()))
            .and_then(|result| result);
        log_storage(what, &outcome);
        outcome
    }

    /// Run one write in an immediate transaction on a blocking thread. Any
    /// error rolls the whole transaction back.
    async fn write<T: Send + 'static>(
        &self,
        what: &'static str,
        op: impl FnOnce(&Connection) -> Result<T, ContribError> + Send + 'static,
    ) -> Result<T, ContribError> {
        let conn = self.conn.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut guard = Self::lock(&conn);
            let tx = guard.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = op(&tx)?;
            tx.commit()?;
            Ok(value)
        })
        .await
        .map_err(|error| ContribError::Storage(error.to_string()))
        .and_then(|result| result);
        log_storage(what, &outcome);
        outcome
    }
}

fn log_storage<T>(what: &str, outcome: &Result<T, ContribError>) {
    if let Err(ContribError::Storage(cause)) = outcome {
        tracing::error!(what, %cause, "contribution store failed");
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

fn state_str(state: ContributionState) -> &'static str {
    match state {
        ContributionState::Draft => "draft",
        ContributionState::Ready => "ready",
        ContributionState::Seeded => "seeded",
        ContributionState::Verifying => "verifying",
        ContributionState::Linked => "linked",
        ContributionState::NeedsReview => "needs_review",
        ContributionState::Stale => "stale",
        ContributionState::Cancelled => "cancelled",
    }
}

fn state_from(raw: &str) -> Result<ContributionState, ContribError> {
    Ok(match raw {
        "draft" => ContributionState::Draft,
        "ready" => ContributionState::Ready,
        "seeded" => ContributionState::Seeded,
        "verifying" => ContributionState::Verifying,
        "linked" => ContributionState::Linked,
        "needs_review" => ContributionState::NeedsReview,
        "stale" => ContributionState::Stale,
        "cancelled" => ContributionState::Cancelled,
        other => {
            return Err(ContribError::Data(format!(
                "Unknown contribution state '{other}'."
            )));
        }
    })
}

fn job_state_from(raw: &str) -> VerificationJobState {
    match raw {
        "running" => VerificationJobState::Running,
        "succeeded" => VerificationJobState::Succeeded,
        "needs_review" => VerificationJobState::NeedsReview,
        "failed" => VerificationJobState::Failed,
        "cancelled" => VerificationJobState::Cancelled,
        _ => VerificationJobState::Queued,
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, ContribError> {
    serde_json::to_string(value)
        .map_err(|error| ContribError::Data(format!("Contribution document not encoded: {error}")))
}

fn from_json<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, ContribError> {
    serde_json::from_str(raw)
        .map_err(|_| ContribError::Data("Unsupported persisted contribution document.".into()))
}

fn terminal(state: ContributionState) -> bool {
    matches!(
        state,
        ContributionState::Linked | ContributionState::Cancelled | ContributionState::Stale
    )
}

fn editable(state: ContributionState) -> bool {
    matches!(
        state,
        ContributionState::Draft | ContributionState::Ready | ContributionState::NeedsReview
    )
}

// ---------------------------------------------------------------------------
// Album input (freshness) and rows
// ---------------------------------------------------------------------------

/// The live album a contribution was built from.
struct AlbumInput {
    row_revision: i64,
    artist_id: String,
    input_revision: String,
}

/// The active album with at least one indexed track, or `None` (v2
/// `_active_album_input`).
fn album_input(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<AlbumInput>> {
    let album: Option<(i64, String)> = conn
        .prepare_cached(
            "SELECT row_revision, album_artist_id FROM local_albums \
             WHERE id = ?1 AND retired_into_album_id IS NULL",
        )?
        .query_row(params![album_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((row_revision, artist_id)) = album else {
        return Ok(None);
    };
    let tracks: Vec<(String, TrackInput)> = conn
        .prepare_cached(
            "SELECT id, tag_revision, stat_revision, applied_policy_revision, applied_policy \
             FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed'",
        )?
        .query_map(params![album_id], |row| {
            Ok((
                row.get(0)?,
                TrackInput {
                    tag_revision: row.get(1)?,
                    stat_revision: row.get(2)?,
                    applied_policy_revision: row.get(3)?,
                    applied_policy: row.get(4)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if tracks.is_empty() {
        return Ok(None);
    }
    let (tag, file, policy) =
        album_input_revisions(tracks.iter().map(|(id, input)| (id.as_str(), input)));
    Ok(Some(AlbumInput {
        row_revision,
        artist_id,
        input_revision: format!("{tag}:{file}:{policy}"),
    }))
}

const ROW_COLUMNS: &str = "id, local_album_id, created_by_user_id, updated_by_user_id, state, \
     album_row_revision, input_revision, local_snapshot_json, resolved_draft_json, \
     source_selection_json, provider_snapshot_expires_at, duplicate_result_json, \
     duplicate_checked_at, duplicate_input_revision, result_release_mbid, result_source, \
     result_received_at, seed_snapshot_json, seed_hash, seeded_at, terminal_at, created_at, \
     updated_at, row_revision";

/// Raw column values; JSON decodes after the statement finishes.
struct RawRow {
    id: String,
    local_album_id: String,
    created_by: Option<String>,
    updated_by: Option<String>,
    state: String,
    album_row_revision: i64,
    input_revision: String,
    snapshot: String,
    draft: String,
    selection: String,
    provider_expires: Option<f64>,
    duplicate: Option<String>,
    duplicate_checked_at: Option<f64>,
    duplicate_input_revision: Option<String>,
    result_release_mbid: Option<String>,
    result_source: Option<String>,
    result_received_at: Option<f64>,
    seed_snapshot: Option<String>,
    seed_hash: Option<String>,
    seeded_at: Option<f64>,
    terminal_at: Option<f64>,
    created_at: f64,
    updated_at: f64,
    row_revision: i64,
}

fn raw_row(row: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        id: row.get(0)?,
        local_album_id: row.get(1)?,
        created_by: row.get(2)?,
        updated_by: row.get(3)?,
        state: row.get(4)?,
        album_row_revision: row.get(5)?,
        input_revision: row.get(6)?,
        snapshot: row.get(7)?,
        draft: row.get(8)?,
        selection: row.get(9)?,
        provider_expires: row.get(10)?,
        duplicate: row.get(11)?,
        duplicate_checked_at: row.get(12)?,
        duplicate_input_revision: row.get(13)?,
        result_release_mbid: row.get(14)?,
        result_source: row.get(15)?,
        result_received_at: row.get(16)?,
        seed_snapshot: row.get(17)?,
        seed_hash: row.get(18)?,
        seeded_at: row.get(19)?,
        terminal_at: row.get(20)?,
        created_at: row.get(21)?,
        updated_at: row.get(22)?,
        row_revision: row.get(23)?,
    })
}

/// One contribution row with its freshness join (v2 `_contribution_row`).
fn load_row(
    conn: &Connection,
    contribution_id: &str,
) -> Result<Option<ContributionRow>, ContribError> {
    let raw = conn
        .prepare_cached(&format!(
            "SELECT {ROW_COLUMNS} FROM library_contribution_drafts WHERE id = ?1"
        ))?
        .query_row(params![contribution_id], raw_row)
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let last_failure: Option<String> = conn
        .prepare_cached(
            "SELECT last_failure_code FROM library_contribution_verification_jobs \
             WHERE contribution_id = ?1 AND state IN ('needs_review','failed','cancelled') \
             ORDER BY updated_at DESC, id DESC LIMIT 1",
        )?
        .query_row(params![contribution_id], |row| row.get(0))
        .optional()?
        .flatten();
    let live = album_input(conn, &raw.local_album_id)?;
    let selection: ContributionSourceSelection = from_json(&raw.selection)?;
    let discogs = selection
        .sources
        .iter()
        .find(|s| s.provider == "discogs" && s.entity_type == "release")
        .cloned();
    Ok(Some(ContributionRow {
        id: raw.id,
        local_album_id: raw.local_album_id,
        created_by_user_id: raw.created_by,
        updated_by_user_id: raw.updated_by,
        state: state_from(&raw.state)?,
        album_row_revision: raw.album_row_revision,
        input_revision: raw.input_revision,
        local_snapshot: from_json(&raw.snapshot)?,
        draft: from_json(&raw.draft)?,
        provider_snapshot_expires_at: raw.provider_expires,
        discogs_release_id: discogs.as_ref().map(|s| s.external_id.clone()),
        discogs_canonical_url: discogs.map(|s| s.canonical_url),
        source_selection: selection,
        duplicate_result: raw.duplicate.as_deref().map(from_json).transpose()?,
        duplicate_checked_at: raw.duplicate_checked_at,
        duplicate_input_revision: raw.duplicate_input_revision,
        result_release_mbid: raw.result_release_mbid,
        result_source: raw.result_source,
        result_received_at: raw.result_received_at,
        seeded_at: raw.seeded_at,
        seed_token_hash: None,
        seed_token_expires_at: None,
        seed_snapshot_json: raw.seed_snapshot,
        seed_hash: raw.seed_hash,
        terminal_at: raw.terminal_at,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        row_revision: raw.row_revision,
        last_verification_failure: last_failure,
        album_active: live.is_some(),
        current_input_revision: live
            .as_ref()
            .map(|l| l.input_revision.clone())
            .unwrap_or_default(),
        current_album_row_revision: live.map(|l| l.row_revision).unwrap_or(0),
    }))
}

fn require_row(conn: &Connection, contribution_id: &str) -> Result<ContributionRow, ContribError> {
    load_row(conn, contribution_id)?.ok_or(ContribError::ContributionNotFound)
}

fn active_id_for_album(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT id FROM library_contribution_drafts WHERE local_album_id = ?1 \
         AND state NOT IN ('linked','cancelled','stale') ORDER BY created_at DESC LIMIT 1",
    )?
    .query_row(params![album_id], |row| row.get(0))
    .optional()
}

/// Write every mutable column of `row` back under the revision it was read
/// at. `row.row_revision` already holds the new revision.
fn save_row(
    conn: &Connection,
    row: &ContributionRow,
    expected: i64,
    stale_message: &str,
) -> Result<(), ContribError> {
    let changed = conn
        .prepare_cached(
            "UPDATE library_contribution_drafts SET updated_by_user_id = ?3, state = ?4, \
             album_row_revision = ?5, input_revision = ?6, resolved_draft_json = ?7, \
             source_selection_json = ?8, provider_snapshot_expires_at = ?9, \
             duplicate_result_json = ?10, duplicate_checked_at = ?11, \
             duplicate_input_revision = ?12, result_release_mbid = ?13, result_source = ?14, \
             result_received_at = ?15, seed_snapshot_json = ?16, seed_hash = ?17, \
             seeded_at = ?18, terminal_at = ?19, updated_at = ?20, row_revision = ?21 \
             WHERE id = ?1 AND row_revision = ?2",
        )?
        .execute(params![
            row.id,
            expected,
            row.updated_by_user_id,
            state_str(row.state),
            row.album_row_revision,
            row.input_revision,
            to_json(&row.draft)?,
            to_json(&row.source_selection)?,
            row.provider_snapshot_expires_at,
            row.duplicate_result.as_ref().map(to_json).transpose()?,
            row.duplicate_checked_at,
            row.duplicate_input_revision,
            row.result_release_mbid,
            row.result_source,
            row.result_received_at,
            row.seed_snapshot_json,
            row.seed_hash,
            row.seeded_at,
            row.terminal_at,
            row.updated_at,
            row.row_revision,
        ])?;
    if changed != 1 {
        return Err(ContribError::stale(stale_message));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_row(
    conn: &Connection,
    local_album_id: &str,
    actor_user_id: &str,
    album_row_revision: i64,
    input_revision: &str,
    snapshot: &LocalReleaseSnapshot,
    draft: &ReleaseDraft,
    selection: &ContributionSourceSelection,
    now: f64,
) -> Result<String, ContribError> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.prepare_cached(
        "INSERT INTO library_contribution_drafts (id, local_album_id, created_by_user_id, \
         updated_by_user_id, state, album_row_revision, input_revision, local_snapshot_json, \
         resolved_draft_json, source_selection_json, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?3, 'draft', ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
    )?
    .execute(params![
        id,
        local_album_id,
        actor_user_id,
        album_row_revision,
        input_revision,
        to_json(snapshot)?,
        to_json(draft)?,
        to_json(selection)?,
        now,
    ])?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Tokens and jobs
// ---------------------------------------------------------------------------

fn consume_tokens(conn: &Connection, contribution_id: &str, now: f64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "UPDATE library_contribution_callback_tokens SET consumed_at = ?2 \
         WHERE contribution_id = ?1 AND consumed_at IS NULL",
    )?
    .execute(params![contribution_id, now])?;
    Ok(())
}

fn cancel_jobs(conn: &Connection, contribution_id: &str, now: f64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "UPDATE library_contribution_verification_jobs SET state = 'cancelled', \
         terminal_at = ?2, updated_at = ?2, lease_owner = NULL, lease_expires_at = NULL, \
         heartbeat_at = NULL, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 \
         WHERE contribution_id = ?1 AND state IN ('queued','running')",
    )?
    .execute(params![contribution_id, now])?;
    Ok(())
}

fn active_job(conn: &Connection, contribution_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT id FROM library_contribution_verification_jobs \
         WHERE contribution_id = ?1 AND state IN ('queued','running') LIMIT 1",
    )?
    .query_row(params![contribution_id], |row| row.get(0))
    .optional()
}

fn enqueue_job(
    conn: &Connection,
    contribution_id: &str,
    requested_by: Option<&str>,
    now: f64,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    conn.prepare_cached(
        "INSERT INTO library_contribution_verification_jobs (id, contribution_id, state, \
         attempt_count, not_before, requested_by_user_id, created_at, updated_at) \
         VALUES (?1, ?2, 'queued', 0, ?3, ?4, ?3, ?3)",
    )?
    .execute(params![id, contribution_id, now, requested_by])?;
    Ok(id)
}

const JOB_COLUMNS: &str = "id, contribution_id, state, lease_owner, attempt_count, not_before, \
     created_at, lease_expires_at, last_failure_code, requested_by_user_id, terminal_at, \
     row_revision";

fn job_row(row: &Row<'_>) -> rusqlite::Result<VerificationJobRow> {
    Ok(VerificationJobRow {
        id: row.get(0)?,
        contribution_id: row.get(1)?,
        state: job_state_from(&row.get::<_, String>(2)?),
        worker_id: row.get(3)?,
        attempt_count: row.get::<_, i64>(4)?.max(0) as u32,
        not_before: row.get(5)?,
        created_at: row.get(6)?,
        lease_expires_at: row.get(7)?,
        last_failure_code: row.get(8)?,
        requested_by_user_id: row.get(9)?,
        terminal_at: row.get(10)?,
        row_revision: row.get(11)?,
    })
}

fn load_job(conn: &Connection, job_id: &str) -> rusqlite::Result<Option<VerificationJobRow>> {
    conn.prepare_cached(&format!(
        "SELECT {JOB_COLUMNS} FROM library_contribution_verification_jobs WHERE id = ?1"
    ))?
    .query_row(params![job_id], job_row)
    .optional()
}

/// Close a running job under its lease.
fn close_job(
    conn: &Connection,
    job_id: &str,
    worker_id: &str,
    expected_revision: i64,
    state: &str,
    failure_code: Option<&str>,
    now: f64,
) -> Result<(), ContribError> {
    let changed = conn
        .prepare_cached(
            "UPDATE library_contribution_verification_jobs SET state = ?4, \
             last_failure_code = ?5, terminal_at = ?6, updated_at = ?6, lease_owner = NULL, \
             lease_expires_at = NULL, heartbeat_at = NULL, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = ?1 AND state = 'running' AND lease_owner = ?2 AND row_revision = ?3",
        )?
        .execute(params![
            job_id,
            worker_id,
            expected_revision,
            state,
            failure_code,
            now
        ])?;
    if changed != 1 {
        return Err(ContribError::stale(
            "The contribution verification job changed before completion.",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Identity commits (the identify-owned tables, written as manual decisions)
// ---------------------------------------------------------------------------

fn album_identity(
    conn: &Connection,
    album_id: &str,
) -> rusqlite::Result<Option<(Option<String>, String)>> {
    conn.prepare_cached(
        "SELECT release_mbid, release_group_mbid FROM local_album_external_identities \
         WHERE local_album_id = ?1 AND provider = ?2",
    )?
    .query_row(params![album_id, PROVIDER], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })
    .optional()
}

fn artist_identity(conn: &Connection, artist_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT provider_artist_id FROM local_artist_external_identities \
         WHERE local_artist_id = ?1 AND provider = ?2",
    )?
    .query_row(params![artist_id, PROVIDER], |row| row.get(0))
    .optional()
}

fn artist_owner(conn: &Connection, artist_mbid: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT local_artist_id FROM local_artist_external_identities \
         WHERE provider = ?1 AND provider_artist_id = ?2",
    )?
    .query_row(params![PROVIDER, artist_mbid], |row| row.get(0))
    .optional()
}

/// Record the attempt behind a contribution decision (v2
/// `library_identification_attempts`, trigger `contribution_submission`).
fn insert_attempt(
    conn: &Connection,
    attempt: &ContributionVerificationAttempt,
    input_revision: &str,
    state: &str,
    reason: Option<&str>,
    selected_key: Option<&str>,
) -> rusqlite::Result<()> {
    let mut parts = input_revision.splitn(3, ':');
    let tag = parts.next().unwrap_or("");
    let file = parts.next().unwrap_or("");
    let policy = parts.next().unwrap_or("");
    conn.prepare_cached(
        "INSERT INTO library_identification_attempts (id, local_album_id, trigger, \
         requested_by_user_id, input_tag_revision, input_policy_revision, input_file_revision, \
         matcher_version, state, terminal_reason_code, selected_candidate_key, candidate_count, \
         started_at, completed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?
    .execute(params![
        attempt.id,
        attempt.local_album_id,
        ATTEMPT_TRIGGER,
        attempt.requested_by_user_id,
        tag,
        policy,
        file,
        attempt.matcher_version,
        state,
        reason.unwrap_or(""),
        selected_key,
        attempt.candidate_count as i64,
        attempt.started_at,
        attempt.completed_at,
    ])?;
    Ok(())
}

/// What a link commits besides the contribution row.
struct LinkIdentity<'a> {
    album_id: &'a str,
    artist_id: &'a str,
    album_revision: i64,
    release_mbid: &'a str,
    release_group_mbid: &'a str,
    artist_mbid: Option<&'a str>,
    attempt: &'a ContributionVerificationAttempt,
    selected_by: Option<&'a str>,
}

/// Commit the album identity (and the artist identity when free) as manual
/// decisions; returns the album's new row revision. Callers have already
/// ruled out conflicting album and artist identities.
fn commit_link(conn: &Connection, link: &LinkIdentity<'_>, now: f64) -> Result<i64, ContribError> {
    let mut album_revision = link.album_revision;
    if album_identity(conn, link.album_id)?.is_none() {
        album_revision = conn
            .prepare_cached(
                "UPDATE local_albums SET updated_at = ?3, row_revision = row_revision + 1 \
                 WHERE id = ?1 AND row_revision = ?2 RETURNING row_revision",
            )?
            .query_row(params![link.album_id, link.album_revision, now], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or_else(|| {
                ContribError::stale("The album changed before the release could be linked.")
            })?;
        conn.prepare_cached(
            "INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, matcher_version, attempt_id, \
             selected_by_user_id, selected_at) VALUES (?1, ?2, ?3, ?4, 'manual', ?5, ?6, ?7, ?8)",
        )?
        .execute(params![
            link.album_id,
            PROVIDER,
            link.release_group_mbid,
            link.release_mbid,
            link.attempt.matcher_version,
            link.attempt.id,
            link.selected_by,
            now,
        ])?;
    }
    if let Some(wanted) = link.artist_mbid {
        match artist_owner(conn, wanted)? {
            Some(owner) if owner != link.artist_id => {
                let (left, right) = if link.artist_id < owner.as_str() {
                    (link.artist_id.to_owned(), owner)
                } else {
                    (owner, link.artist_id.to_owned())
                };
                conn.prepare_cached(
                    "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
                     right_artist_id, reason_code, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, 'SHARED_PROVIDER_IDENTITY', ?4, ?4)",
                )?
                .execute(params![
                    format!("provider:{left}:{right}"),
                    left,
                    right,
                    now
                ])?;
            }
            Some(_) => {}
            None if artist_identity(conn, link.artist_id)?.is_none() => {
                conn.prepare_cached(
                    "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 \
                     WHERE id = ?1",
                )?
                .execute(params![link.artist_id, now])?;
                conn.prepare_cached(
                    "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                     provider_artist_id, decision_source, attempt_id, selected_by_user_id, \
                     selected_at) VALUES (?1, ?2, ?3, 'manual', ?4, ?5, ?6)",
                )?
                .execute(params![
                    link.artist_id,
                    PROVIDER,
                    wanted,
                    link.attempt.id,
                    link.selected_by,
                    now
                ])?;
            }
            None => {}
        }
    }
    // An identify review left open for this album is settled by the link.
    conn.prepare_cached(
        "UPDATE library_identify_reviews SET state = 'approved', resolved_by_user_id = ?2, \
         selected_candidate_key = ?3, updated_ms = ?4 \
         WHERE local_album_id = ?1 AND state = 'pending'",
    )?
    .execute(params![
        link.album_id,
        link.selected_by,
        format!("{}:{}", link.release_group_mbid, link.release_mbid),
        (now * 1000.0) as i64,
    ])?;
    Ok(album_revision)
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Apply one guarded update to a loaded row (v2's per-operation store
/// methods). Returns the expected revision and the message a lost race
/// reports.
fn apply_update(
    conn: &Connection,
    row: &mut ContributionRow,
    expected_row_revision: i64,
    actor_user_id: &str,
    now: f64,
    update: ContributionUpdate,
) -> Result<&'static str, ContribError> {
    let check = |row: &ContributionRow, msg: &'static str| {
        if row.row_revision != expected_row_revision {
            Err(ContribError::stale(msg))
        } else {
            Ok(msg)
        }
    };
    let input_moved = |row: &ContributionRow| row.input_revision != row.current_input_revision;
    let msg =
        match update {
            ContributionUpdate::Draft { draft, state } => {
                if !row.album_active {
                    return Err(ContribError::ContributionNotFound);
                }
                if !editable(row.state) {
                    return Err(ContribError::state(
                        "This contribution can no longer be edited.",
                    ));
                }
                let msg = check(row, "The contribution changed before this edit was saved.")?;
                // A race guard only: the service's read already marks a moved
                // album stale before it edits.
                if input_moved(row) || row.album_row_revision != row.current_album_row_revision {
                    return Err(ContribError::stale(
                        "The local album changed. Rebuild the contribution before editing.",
                    ));
                }
                row.draft = draft;
                row.state = state;
                clear_duplicates(row);
                msg
            }
            ContributionUpdate::SelectDiscogs {
                release: _,
                selection,
                expires_at,
            } => {
                if !row.album_active {
                    return Err(ContribError::ContributionNotFound);
                }
                if !editable(row.state) {
                    return Err(ContribError::state(
                        "This contribution can no longer be edited.",
                    ));
                }
                let msg = check(
                    row,
                    "The contribution changed before the source was selected.",
                )?;
                if input_moved(row) {
                    return Err(ContribError::stale(
                        "The local album changed. Rebuild the contribution first.",
                    ));
                }
                row.source_selection = selection;
                row.provider_snapshot_expires_at = Some(expires_at);
                clear_duplicates(row);
                msg
            }
            ContributionUpdate::RemoveDiscogs { draft, state } => {
                if !row.album_active {
                    return Err(ContribError::ContributionNotFound);
                }
                if !editable(row.state) {
                    return Err(ContribError::state(
                        "This contribution can no longer be edited.",
                    ));
                }
                let msg = check(
                    row,
                    "The contribution changed before the source was removed.",
                )?;
                row.source_selection = ContributionSourceSelection::default();
                row.draft = draft;
                row.provider_snapshot_expires_at = None;
                row.state = state;
                clear_duplicates(row);
                msg
            }
            ContributionUpdate::DuplicateResult { result, state } => {
                if !row.album_active {
                    return Err(ContribError::ContributionNotFound);
                }
                if !matches!(
                    row.state,
                    ContributionState::Ready | ContributionState::NeedsReview
                ) {
                    return Err(ContribError::state(
                        "Complete the contribution draft first.",
                    ));
                }
                let msg = check(
                    row,
                    "The contribution changed before duplicate results were saved.",
                )?;
                if input_moved(row) || result.input_revision != row.input_revision {
                    return Err(ContribError::stale(
                        "The local album changed before duplicate results were saved.",
                    ));
                }
                row.duplicate_input_revision = Some(result.input_revision.clone());
                row.duplicate_result = Some(result);
                row.duplicate_checked_at = Some(now);
                row.state = state;
                msg
            }
            ContributionUpdate::AttachExisting {
                release_mbid,
                release_group_mbid,
                artist_mbid,
                attempt,
            } => {
                if !row.album_active {
                    return Err(ContribError::ContributionNotFound);
                }
                if !matches!(
                    row.state,
                    ContributionState::Ready | ContributionState::NeedsReview
                ) {
                    return Err(ContribError::state(
                        "This contribution cannot attach a release now.",
                    ));
                }
                let msg = check(
                    row,
                    "The contribution changed before the release could be attached.",
                )?;
                if input_moved(row)
                    || row.duplicate_input_revision.as_deref() != Some(row.input_revision.as_str())
                {
                    return Err(ContribError::stale(
                        "The local album changed before the release could be attached.",
                    ));
                }
                let in_result = row.duplicate_result.as_ref().is_some_and(|d| {
                    d.candidates
                        .iter()
                        .any(|c| c.release_mbid.as_deref() == Some(release_mbid.as_str()))
                });
                if !in_result {
                    return Err(ContribError::state(
                        "The release is not in the current duplicate-check result.",
                    ));
                }
                let expected_key = format!("{release_group_mbid}:{release_mbid}");
                if attempt.selected_candidate_key.as_deref() != Some(expected_key.as_str())
                    || attempt.local_album_id != row.local_album_id
                {
                    return Err(ContribError::state(
                        "The verified release evidence does not match this contribution.",
                    ));
                }
                if let Some((existing_release, existing_group)) =
                    album_identity(conn, &row.local_album_id)?
                    && (existing_release.as_deref() != Some(release_mbid.as_str())
                        || existing_group != release_group_mbid)
                {
                    return Err(ContribError::state(
                        "The local album already has a different MusicBrainz identity.",
                    ));
                }
                let live = album_input(conn, &row.local_album_id)?
                    .ok_or(ContribError::ContributionNotFound)?;
                if let Some(wanted) = artist_mbid.as_deref()
                    && artist_identity(conn, &live.artist_id)?
                        .is_some_and(|existing| existing != wanted)
                {
                    return Err(ContribError::state(
                        "The local artist already has a different MusicBrainz identity.",
                    ));
                }
                insert_attempt(
                    conn,
                    &attempt,
                    &row.input_revision,
                    "identified",
                    attempt.terminal_reason_code.as_deref(),
                    attempt.selected_candidate_key.as_deref(),
                )?;
                let album_revision = commit_link(
                    conn,
                    &LinkIdentity {
                        album_id: &row.local_album_id,
                        artist_id: &live.artist_id,
                        album_revision: live.row_revision,
                        release_mbid: &release_mbid,
                        release_group_mbid: &release_group_mbid,
                        artist_mbid: artist_mbid.as_deref(),
                        attempt: &attempt,
                        selected_by: Some(actor_user_id),
                    },
                    now,
                )?;
                consume_tokens(conn, &row.id, now)?;
                row.state = ContributionState::Linked;
                row.album_row_revision = album_revision;
                row.current_album_row_revision = album_revision;
                row.result_release_mbid = Some(release_mbid);
                row.result_source = Some("manual".into());
                row.result_received_at = Some(now);
                row.terminal_at = Some(now);
                msg
            }
            ContributionUpdate::PrepareSeed {
                token_hash,
                token_expires_at,
                seed_snapshot_json,
                seed_hash,
            } => {
                if !row.album_active {
                    return Err(ContribError::ContributionNotFound);
                }
                if !matches!(
                    row.state,
                    ContributionState::Ready | ContributionState::Seeded
                ) || row.duplicate_result.is_none()
                {
                    return Err(ContribError::DuplicateCheckRequired(
                        "Run the MusicBrainz duplicate check first.".into(),
                    ));
                }
                let msg = check(
                    row,
                    "The contribution changed before the editor could be opened.",
                )?;
                if input_moved(row)
                    || row.duplicate_input_revision.as_deref() != Some(row.input_revision.as_str())
                {
                    return Err(ContribError::stale(
                        "The local album changed before the editor could be opened.",
                    ));
                }
                if row
                    .duplicate_result
                    .as_ref()
                    .is_some_and(|d| d.candidates.iter().any(|c| c.exact))
                {
                    return Err(ContribError::ExactDuplicate(
                        "An exact MusicBrainz release already exists for this Discogs source."
                            .into(),
                    ));
                }
                consume_tokens(conn, &row.id, now)?;
                conn.prepare_cached(
                "INSERT INTO library_contribution_callback_tokens (token_hash, contribution_id, \
                 requested_by_user_id, expires_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![token_hash, row.id, actor_user_id, token_expires_at, now])?;
                row.state = ContributionState::Seeded;
                row.seed_snapshot_json = Some(seed_snapshot_json);
                row.seed_hash = Some(seed_hash);
                row.seeded_at = Some(now);
                msg
            }
            ContributionUpdate::ManualResult {
                release_mbid,
                replace_existing,
            } => {
                if !matches!(
                    row.state,
                    ContributionState::Seeded
                        | ContributionState::Verifying
                        | ContributionState::NeedsReview
                        | ContributionState::Stale
                ) {
                    return Err(ContribError::state(
                        "This contribution is not waiting for a MusicBrainz result.",
                    ));
                }
                let msg = check(
                    row,
                    "The contribution changed before the result was recorded.",
                )?;
                if let Some(current) = row.result_release_mbid.as_deref()
                    && current != release_mbid
                    && (!replace_existing
                        || !matches!(
                            row.state,
                            ContributionState::NeedsReview | ContributionState::Stale
                        ))
                {
                    return Err(ContribError::state(
                        "Confirm replacement of the existing MusicBrainz result.",
                    ));
                }
                consume_tokens(conn, &row.id, now)?;
                cancel_jobs(conn, &row.id, now)?;
                let verifying = row.album_active && row.state != ContributionState::Stale;
                row.state = if verifying {
                    ContributionState::Verifying
                } else {
                    ContributionState::Stale
                };
                row.result_release_mbid = Some(release_mbid);
                row.result_source = Some("manual".into());
                row.result_received_at = Some(now);
                row.terminal_at = None;
                if verifying {
                    enqueue_job(conn, &row.id, Some(actor_user_id), now)?;
                }
                msg
            }
            ContributionUpdate::RequeueVerification => {
                if !row.album_active {
                    return Err(ContribError::ContributionNotFound);
                }
                if !matches!(
                    row.state,
                    ContributionState::Verifying | ContributionState::NeedsReview
                ) {
                    return Err(ContribError::state(
                        "This contribution cannot be verified now.",
                    ));
                }
                if row.result_release_mbid.is_none() {
                    return Err(ContribError::state(
                        "No MusicBrainz result is ready to verify.",
                    ));
                }
                let msg = check(
                    row,
                    "The contribution changed before verification was retried.",
                )?;
                if active_job(conn, &row.id)?.is_none() {
                    enqueue_job(conn, &row.id, Some(actor_user_id), now)?;
                }
                row.state = ContributionState::Verifying;
                row.terminal_at = None;
                msg
            }
            ContributionUpdate::Cancel => {
                if !row.album_active {
                    return Err(ContribError::ContributionNotFound);
                }
                if terminal(row.state) {
                    return Err(ContribError::state("This contribution is already closed."));
                }
                let msg = check(
                    row,
                    "The contribution changed before it could be cancelled.",
                )?;
                consume_tokens(conn, &row.id, now)?;
                cancel_jobs(conn, &row.id, now)?;
                row.state = ContributionState::Cancelled;
                row.terminal_at = Some(now);
                row.seed_snapshot_json = None;
                msg
            }
            ContributionUpdate::PurgeProviderData { draft, selection } => {
                if row.provider_snapshot_expires_at.is_none()
                    || row.row_revision != expected_row_revision
                {
                    return Err(ContribError::stale(
                        "The contribution changed before cleanup.",
                    ));
                }
                row.draft = draft;
                row.source_selection = selection;
                row.provider_snapshot_expires_at = None;
                clear_duplicates(row);
                row.seed_snapshot_json = None;
                row.updated_at = now;
                row.row_revision += 1;
                // Cleanup is not an edit: the last editor stays recorded.
                return Ok("The contribution changed before cleanup.");
            }
        };
    row.updated_by_user_id = Some(actor_user_id.to_owned());
    row.updated_at = now;
    row.row_revision += 1;
    Ok(msg)
}

fn clear_duplicates(row: &mut ContributionRow) {
    row.duplicate_result = None;
    row.duplicate_checked_at = None;
    row.duplicate_input_revision = None;
}

impl ContributionStore for SqliteContributions {
    fn create_or_get<'a>(
        &'a self,
        local_album_id: &'a str,
        actor_user_id: &'a str,
        album_row_revision: i64,
        input_revision: &'a str,
        snapshot: &'a LocalReleaseSnapshot,
        draft: &'a ReleaseDraft,
        selection: &'a ContributionSourceSelection,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        let (album, actor, input) = (
            local_album_id.to_owned(),
            actor_user_id.to_owned(),
            input_revision.to_owned(),
        );
        let (snapshot, draft, selection) = (snapshot.clone(), draft.clone(), selection.clone());
        Box::pin(self.write("create contribution", move |conn| {
            let live = album_input(conn, &album)?.ok_or(ContribError::AlbumNotFound)?;
            if live.row_revision != album_row_revision || live.input_revision != input {
                return Err(ContribError::stale(
                    "The album changed before the contribution could be created.",
                ));
            }
            let id = match active_id_for_album(conn, &album)? {
                Some(id) => id,
                None => insert_row(
                    conn,
                    &album,
                    &actor,
                    album_row_revision,
                    &input,
                    &snapshot,
                    &draft,
                    &selection,
                    now,
                )?,
            };
            require_row(conn, &id)
        }))
    }

    fn get<'a>(&'a self, contribution_id: &'a str) -> BoxFuture<'a, Option<ContributionRow>> {
        let id = contribution_id.to_owned();
        Box::pin(async move {
            self.read("get contribution", move |conn| load_row(conn, &id))
                .await
                .ok()
                .flatten()
        })
    }

    fn get_active_for_album<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Option<ContributionRow>> {
        let album = album_id.to_owned();
        Box::pin(async move {
            self.read(
                "active contribution",
                move |conn| match active_id_for_album(conn, &album)? {
                    Some(id) => Ok(load_row(conn, &id)?.filter(|row| row.album_active)),
                    None => Ok(None),
                },
            )
            .await
            .ok()
            .flatten()
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        actor_user_id: &'a str,
        now: f64,
        update: ContributionUpdate,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        let (id, actor) = (contribution_id.to_owned(), actor_user_id.to_owned());
        Box::pin(self.write("update contribution", move |conn| {
            let mut row = require_row(conn, &id)?;
            let msg = apply_update(conn, &mut row, expected_row_revision, &actor, now, update)?;
            save_row(conn, &row, expected_row_revision, msg)?;
            require_row(conn, &id)
        }))
    }

    fn mark_stale<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        let id = contribution_id.to_owned();
        Box::pin(self.write("mark contribution stale", move |conn| {
            let row = require_row(conn, &id)?;
            if !terminal(row.state) {
                if row.row_revision != expected_row_revision {
                    return Err(ContribError::stale(
                        "The contribution changed before it could be marked stale.",
                    ));
                }
                let mut stale = row.clone();
                stale.state = ContributionState::Stale;
                stale.terminal_at = Some(now);
                stale.seed_snapshot_json = None;
                stale.updated_at = now;
                stale.row_revision += 1;
                save_row(
                    conn,
                    &stale,
                    expected_row_revision,
                    "The contribution changed before it could be marked stale.",
                )?;
                cancel_jobs(conn, &id, now)?;
            }
            require_row(conn, &id)
        }))
    }

    fn consume_callback_token<'a>(
        &'a self,
        token_hash: &'a str,
        release_mbid: &'a str,
        now: f64,
    ) -> BoxFuture<'a, Result<CallbackConsumption, ContribError>> {
        let (hash, release) = (token_hash.to_owned(), release_mbid.to_owned());
        Box::pin(self.write("consume callback token", move |conn| {
            let token: Option<(String, Option<String>)> = conn
                .prepare_cached(
                    "SELECT contribution_id, requested_by_user_id \
                     FROM library_contribution_callback_tokens \
                     WHERE token_hash = ?1 AND consumed_at IS NULL AND expires_at >= ?2",
                )?
                .query_row(params![hash, now], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            let Some((contribution_id, requested_by)) = token else {
                return Ok(None);
            };
            let Some(mut row) = load_row(conn, &contribution_id)? else {
                return Ok(None);
            };
            if !matches!(
                row.state,
                ContributionState::Seeded | ContributionState::Verifying | ContributionState::Stale
            ) {
                return Err(ContribError::state(
                    "This contribution is not waiting for MusicBrainz.",
                ));
            }
            if row
                .result_release_mbid
                .as_deref()
                .is_some_and(|current| current != release)
            {
                return Err(ContribError::state(
                    "This contribution already has a different MusicBrainz result.",
                ));
            }
            consume_tokens(conn, &row.id, now)?;
            let expected = row.row_revision;
            let verifying = row.album_active && row.state != ContributionState::Stale;
            row.state = if verifying {
                ContributionState::Verifying
            } else {
                ContributionState::Stale
            };
            row.result_release_mbid = Some(release.clone());
            row.result_source = Some("callback".into());
            row.result_received_at = Some(now);
            row.updated_at = now;
            row.row_revision += 1;
            save_row(
                conn,
                &row,
                expected,
                "The contribution changed before the MusicBrainz result was recorded.",
            )?;
            if !verifying {
                return Ok(Some((row.id, None)));
            }
            let job = match active_job(conn, &row.id)? {
                Some(job) => job,
                None => enqueue_job(conn, &row.id, requested_by.as_deref(), now)?,
            };
            Ok(Some((row.id, Some(job))))
        }))
    }

    fn list_for_provider_purge<'a>(
        &'a self,
        now: f64,
        limit: usize,
    ) -> BoxFuture<'a, Vec<ContributionRow>> {
        let bounded = limit.clamp(1, 1_000) as i64;
        Box::pin(async move {
            self.read("list contributions for purge", move |conn| {
                let ids: Vec<String> = conn
                    .prepare_cached(
                        "SELECT id FROM library_contribution_drafts \
                         WHERE provider_snapshot_expires_at IS NOT NULL \
                         AND (provider_snapshot_expires_at <= ?1 \
                         OR state IN ('linked','cancelled','stale')) \
                         ORDER BY provider_snapshot_expires_at, id LIMIT ?2",
                    )?
                    .query_map(params![now, bounded], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let mut rows = Vec::with_capacity(ids.len());
                for id in ids {
                    match load_row(conn, &id) {
                        Ok(Some(row)) => rows.push(row),
                        Ok(None) => {}
                        Err(error) => {
                            tracing::warn!(%error, contribution = id, "contribution skipped by provider purge");
                        }
                    }
                }
                Ok(rows)
            })
            .await
            .unwrap_or_default()
        })
    }

    fn claim_verification<'a>(
        &'a self,
        worker_id: &'a str,
        now: f64,
        lease_seconds: f64,
    ) -> BoxFuture<'a, Option<VerificationJobRow>> {
        let worker = worker_id.to_owned();
        Box::pin(async move {
            self.write("claim verification", move |conn| {
                let id: Option<String> = conn
                    .prepare_cached(
                        "SELECT id FROM library_contribution_verification_jobs \
                         WHERE state = 'queued' AND not_before <= ?1 \
                         ORDER BY not_before, created_at LIMIT 1",
                    )?
                    .query_row(params![now], |row| row.get(0))
                    .optional()?;
                let Some(id) = id else {
                    return Ok(None);
                };
                conn.prepare_cached(
                    "UPDATE library_contribution_verification_jobs SET state = 'running', \
                     attempt_count = attempt_count + 1, lease_owner = ?2, \
                     lease_expires_at = ?3, heartbeat_at = ?4, updated_at = ?4, \
                     row_revision = row_revision + 1, event_revision = event_revision + 1 \
                     WHERE id = ?1",
                )?
                .execute(params![id, worker, now + lease_seconds, now])?;
                Ok(load_job(conn, &id)?)
            })
            .await
            .ok()
            .flatten()
        })
    }

    fn rebuild<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        actor_user_id: &'a str,
        album_row_revision: i64,
        input_revision: &'a str,
        snapshot: &'a LocalReleaseSnapshot,
        draft: &'a ReleaseDraft,
        selection: &'a ContributionSourceSelection,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        let (id, actor, input) = (
            contribution_id.to_owned(),
            actor_user_id.to_owned(),
            input_revision.to_owned(),
        );
        let (snapshot, draft, selection) = (snapshot.clone(), draft.clone(), selection.clone());
        Box::pin(self.write("rebuild contribution", move |conn| {
            let old = require_row(conn, &id)?;
            if old.row_revision != expected_row_revision {
                return Err(ContribError::stale(
                    "The contribution changed before it could be rebuilt.",
                ));
            }
            let live =
                album_input(conn, &old.local_album_id)?.ok_or(ContribError::AlbumNotFound)?;
            if live.row_revision != album_row_revision || live.input_revision != input {
                return Err(ContribError::stale(
                    "The album changed before the contribution could be rebuilt.",
                ));
            }
            let mut retired = old.clone();
            retired.state = ContributionState::Stale;
            retired.terminal_at = Some(now);
            retired.updated_at = now;
            retired.row_revision += 1;
            save_row(
                conn,
                &retired,
                expected_row_revision,
                "The contribution changed before it could be rebuilt.",
            )?;
            consume_tokens(conn, &id, now)?;
            cancel_jobs(conn, &id, now)?;
            let fresh = insert_row(
                conn,
                &old.local_album_id,
                &actor,
                album_row_revision,
                &input,
                &snapshot,
                &draft,
                &selection,
                now,
            )?;
            require_row(conn, &fresh)
        }))
    }

    fn heartbeat_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_row_revision: i64,
        now: f64,
        lease_seconds: f64,
    ) -> BoxFuture<'a, Result<i64, ContribError>> {
        let (job, worker) = (job_id.to_owned(), worker_id.to_owned());
        Box::pin(self.write("heartbeat verification", move |conn| {
            conn.prepare_cached(
                "UPDATE library_contribution_verification_jobs SET lease_expires_at = ?4, \
                 heartbeat_at = ?5, updated_at = ?5, row_revision = row_revision + 1 \
                 WHERE id = ?1 AND state = 'running' AND lease_owner = ?2 AND row_revision = ?3 \
                 RETURNING row_revision",
            )?
            .query_row(
                params![job, worker, expected_row_revision, now + lease_seconds, now],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| ContribError::stale("The contribution verification lease changed."))
        }))
    }

    fn retry_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_row_revision: i64,
        failure_code: &'a str,
        not_before: f64,
        now: f64,
    ) -> BoxFuture<'a, Result<(), ContribError>> {
        let (job, worker, code) = (
            job_id.to_owned(),
            worker_id.to_owned(),
            failure_code.to_owned(),
        );
        Box::pin(self.write("retry verification", move |conn| {
            let changed = conn
                .prepare_cached(
                    "UPDATE library_contribution_verification_jobs SET state = 'queued', \
                     last_failure_code = ?4, not_before = ?5, lease_owner = NULL, \
                     lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?6, \
                     row_revision = row_revision + 1, event_revision = event_revision + 1 \
                     WHERE id = ?1 AND state = 'running' AND lease_owner = ?2 AND row_revision = ?3",
                )?
                .execute(params![job, worker, expected_row_revision, code, not_before, now])?;
            if changed != 1 {
                return Err(ContribError::stale(
                    "The contribution verification job changed before retry.",
                ));
            }
            Ok(())
        }))
    }

    fn finish_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_job_revision: i64,
        expected_contribution_revision: i64,
        expected_album_revision: i64,
        attempt: &'a ContributionVerificationAttempt,
        outcome: VerificationOutcome,
        failure_code: Option<&'a str>,
        identities: &'a FinishIdentities,
        now: f64,
    ) -> BoxFuture<'a, Result<VerificationOutcome, ContribError>> {
        let (job_id, worker) = (job_id.to_owned(), worker_id.to_owned());
        let (attempt, identities) = (attempt.clone(), identities.clone());
        let failure_code = failure_code.map(str::to_owned);
        Box::pin(self.write("finish verification", move |conn| {
            let job = load_job(conn, &job_id)?
                .filter(|job| {
                    job.state == VerificationJobState::Running
                        && job.worker_id.as_deref() == Some(worker.as_str())
                        && job.row_revision == expected_job_revision
                })
                .ok_or_else(|| {
                    ContribError::stale(
                        "The contribution verification job changed before completion.",
                    )
                })?;
            let mut row = require_row(conn, &job.contribution_id)?;
            if !row.album_active
                || row.input_revision != row.current_input_revision
                || row.album_row_revision != row.current_album_row_revision
            {
                let expected = row.row_revision;
                row.state = ContributionState::Stale;
                row.terminal_at = Some(now);
                row.updated_at = now;
                row.row_revision += 1;
                save_row(
                    conn,
                    &row,
                    expected,
                    "The contribution changed before verification completed.",
                )?;
                close_job(
                    conn,
                    &job_id,
                    &worker,
                    expected_job_revision,
                    "cancelled",
                    Some("LOCAL_INPUT_CHANGED"),
                    now,
                )?;
                return Ok(VerificationOutcome::Stale);
            }
            if row.row_revision != expected_contribution_revision {
                return Err(ContribError::stale(
                    "The contribution changed before verification completed.",
                ));
            }
            if row.state != ContributionState::Verifying {
                return Err(ContribError::state(
                    "This contribution is not being verified.",
                ));
            }
            if attempt.local_album_id != row.local_album_id {
                return Err(ContribError::state(
                    "The verification subject does not match.",
                ));
            }
            let live =
                album_input(conn, &row.local_album_id)?.ok_or(ContribError::AlbumNotFound)?;
            if live.row_revision != expected_album_revision {
                return Err(ContribError::stale(
                    "The album changed before verification completed.",
                ));
            }
            let (final_outcome, final_failure) = match outcome {
                VerificationOutcome::Linked => {
                    let selected = attempt.selected_candidate_key.is_some()
                        && identities.release_mbid.is_some()
                        && identities.release_group_mbid.is_some();
                    let existing = album_identity(conn, &row.local_album_id)?;
                    if !selected {
                        (
                            VerificationOutcome::NeedsReview,
                            Some("VERIFICATION_EVIDENCE_MISSING".to_owned()),
                        )
                    } else if identities.release_mbid != row.result_release_mbid {
                        (
                            VerificationOutcome::NeedsReview,
                            Some(FAILURE_RETURNED_RELEASE_MISMATCH.to_owned()),
                        )
                    } else if existing.as_ref().is_some_and(|(release, group)| {
                        release != &identities.release_mbid
                            || Some(group) != identities.release_group_mbid.as_ref()
                    }) {
                        (
                            VerificationOutcome::NeedsReview,
                            Some("EXISTING_IDENTITY_CONFLICT".to_owned()),
                        )
                    } else if let Some(wanted) = identities.artist_mbid.as_deref()
                        && artist_identity(conn, &live.artist_id)?
                            .is_some_and(|existing| existing != wanted)
                    {
                        (
                            VerificationOutcome::NeedsReview,
                            Some("EXISTING_ARTIST_IDENTITY_CONFLICT".to_owned()),
                        )
                    } else {
                        (VerificationOutcome::Linked, None)
                    }
                }
                _ => (VerificationOutcome::NeedsReview, failure_code.clone()),
            };
            let linked = final_outcome == VerificationOutcome::Linked;
            insert_attempt(
                conn,
                &attempt,
                &row.input_revision,
                if linked { "identified" } else { "needs_review" },
                final_failure
                    .as_deref()
                    .or(attempt.terminal_reason_code.as_deref()),
                attempt.selected_candidate_key.as_deref().filter(|_| linked),
            )?;
            let expected = row.row_revision;
            if linked {
                let album_revision = commit_link(
                    conn,
                    &LinkIdentity {
                        album_id: &row.local_album_id,
                        artist_id: &live.artist_id,
                        album_revision: live.row_revision,
                        release_mbid: identities.release_mbid.as_deref().unwrap_or_default(),
                        release_group_mbid: identities
                            .release_group_mbid
                            .as_deref()
                            .unwrap_or_default(),
                        artist_mbid: identities.artist_mbid.as_deref(),
                        attempt: &attempt,
                        selected_by: attempt.requested_by_user_id.as_deref(),
                    },
                    now,
                )?;
                row.state = ContributionState::Linked;
                row.album_row_revision = album_revision;
                row.terminal_at = Some(now);
            } else {
                row.state = ContributionState::NeedsReview;
            }
            row.seed_snapshot_json = None;
            row.updated_at = now;
            row.row_revision += 1;
            save_row(
                conn,
                &row,
                expected,
                "The contribution changed before verification completed.",
            )?;
            close_job(
                conn,
                &job_id,
                &worker,
                expected_job_revision,
                if linked { "succeeded" } else { "needs_review" },
                final_failure.as_deref(),
                now,
            )?;
            Ok(final_outcome)
        }))
    }

    fn recover_verification_leases<'a>(&'a self, now: f64) -> BoxFuture<'a, u64> {
        Box::pin(async move {
            self.write("recover verification leases", move |conn| {
                Ok(conn
                    .prepare_cached(
                        "UPDATE library_contribution_verification_jobs SET state = 'queued', \
                         lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, \
                         not_before = ?1, updated_at = ?1, row_revision = row_revision + 1 \
                         WHERE state = 'running' AND lease_expires_at < ?1",
                    )?
                    .execute(params![now])? as u64)
            })
            .await
            .unwrap_or(0)
        })
    }

    fn clean_records<'a>(&'a self, now: f64) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let _ = self
                .write("clean contribution records", move |conn| {
                    conn.execute(
                        "DELETE FROM library_contribution_callback_tokens WHERE \
                         (consumed_at IS NOT NULL AND consumed_at < ?1) OR \
                         (consumed_at IS NULL AND expires_at < ?2)",
                        params![now - 30.0 * DAY, now - 7.0 * DAY],
                    )?;
                    conn.execute(
                        "UPDATE library_contribution_drafts SET seed_snapshot_json = NULL, \
                         row_revision = row_revision + 1 WHERE seed_snapshot_json IS NOT NULL \
                         AND (state IN ('linked','cancelled','stale') OR (seeded_at IS NOT NULL \
                         AND NOT EXISTS (SELECT 1 FROM library_contribution_callback_tokens t \
                         WHERE t.contribution_id = library_contribution_drafts.id \
                         AND t.consumed_at IS NULL AND t.expires_at >= ?1)))",
                        params![now],
                    )?;
                    conn.execute(
                        "DELETE FROM library_contribution_drafts \
                         WHERE state IN ('cancelled','stale') AND result_release_mbid IS NULL \
                         AND terminal_at < ?1",
                        params![now - 90.0 * DAY],
                    )?;
                    conn.execute(
                        "DELETE FROM library_contribution_verification_jobs \
                         WHERE state IN ('succeeded','needs_review','failed','cancelled') \
                         AND terminal_at < ?1 AND contribution_id IN (SELECT id FROM \
                         library_contribution_drafts WHERE state IN ('linked','cancelled','stale'))",
                        params![now - 90.0 * DAY],
                    )?;
                    Ok(())
                })
                .await;
        })
    }
}

// ---------------------------------------------------------------------------
// Identity context (read side of scan and identify)
// ---------------------------------------------------------------------------

fn album_context(
    conn: &Connection,
    album_id: &str,
) -> Result<Option<AlbumIdentificationContext>, ContribError> {
    let album = conn
        .prepare_cached(
            "SELECT id, row_revision, title, album_artist_name, album_artist_id, \
             original_release_date, year, is_compilation FROM local_albums \
             WHERE id = ?1 AND retired_into_album_id IS NULL",
        )?
        .query_row(params![album_id], |row| {
            Ok(IdentityAlbumRow {
                id: row.get(0)?,
                row_revision: row.get(1)?,
                active: true,
                title: row.get(2)?,
                album_artist_name: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                album_artist_id: row.get(4)?,
                original_release_date: row.get(5)?,
                year: row.get(6)?,
                is_compilation: row.get::<_, i64>(7)? != 0,
            })
        })
        .optional()?;
    let Some(album) = album else {
        return Ok(None);
    };
    let identity = album_identity(conn, album_id)?
        .map(|(release, group)| IdentityAlbumIds {
            release_mbid: release,
            release_group_mbid: Some(group),
        })
        .unwrap_or_default();
    let kind: String = conn
        .prepare_cached("SELECT kind FROM local_artists WHERE id = ?1")?
        .query_row(params![album.album_artist_id], |row| row.get(0))
        .optional()?
        .unwrap_or_else(|| "unknown".to_owned());
    let artist = IdentityArtist {
        kind,
        provider_artist_id: artist_identity(conn, &album.album_artist_id)?,
    };
    let tracks = conn
        .prepare_cached(
            "SELECT t.id, t.disc_number, t.track_number, t.title, t.artist_name, \
             t.duration_seconds, t.availability, t.disc_subtitle, t.relative_path, \
             i.recording_mbid, t.embedded_recording_mbid, t.tag_revision, t.stat_revision, \
             t.applied_policy_revision, t.applied_policy \
             FROM local_tracks t LEFT JOIN local_track_external_identities i \
             ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
             WHERE t.local_album_id = ?1 ORDER BY t.disc_number, t.track_number, t.id",
        )?
        .query_map(params![album_id], |row| {
            Ok(IdentityTrack {
                id: row.get(0)?,
                disc_number: row.get(1)?,
                track_number: row.get(2)?,
                title: row.get(3)?,
                artist_name: row.get(4)?,
                duration_seconds: row.get(5)?,
                availability: row.get(6)?,
                disc_subtitle: row.get(7)?,
                relative_path: row.get(8)?,
                recording_mbid: row.get(9)?,
                embedded_recording_mbid: row.get(10)?,
                input: TrackInput {
                    tag_revision: row.get(11)?,
                    stat_revision: row.get(12)?,
                    applied_policy_revision: row.get(13)?,
                    applied_policy: row.get(14)?,
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(AlbumIdentificationContext {
        album: Some(album),
        identity,
        artist,
        tracks,
    }))
}

impl ContributionIdentity for SqliteContributions {
    fn album_context<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Option<AlbumIdentificationContext>> {
        let album = album_id.to_owned();
        Box::pin(async move {
            match self
                .read("album identification context", move |conn| {
                    album_context(conn, &album)
                })
                .await
            {
                Ok(context) => context,
                Err(error) => {
                    tracing::warn!(%error, album = album_id, "album context unreadable");
                    None
                }
            }
        })
    }

    fn input_revisions(&self, tracks: &[IdentityTrack]) -> (String, String, String) {
        album_input_revisions(tracks.iter().map(|t| (t.id.as_str(), &t.input)))
    }
}
