//! Durable contribution state over the application database.
//!
//! [`SqliteContributions`] implements both the contribution store and the
//! album identification context over the 0001 tables v2 already used:
//! `library_contribution_drafts` and `..._callback_tokens`, plus the
//! catalog the scan writes (`local_albums`, `local_tracks`) and the
//! identity tables identify owns. Verification jobs are rows of
//! [`VERIFY`] on the jobs table, queued and cancelled in the
//! contribution's own write.
//!
//! Every call is one short read or write on the shared database. Writes
//! run on the writer lane in the caller's class: they read the row, run
//! [`transition`] on it, write the result back under the revision they
//! read and run its effects, all in one transaction, so a check and its
//! write can never interleave with another writer. Linking a release
//! commits the album (and, when free, the artist) MusicBrainz identity as
//! a manual decision in that same transaction, so a crash leaves either
//! both or neither.

use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_kernel::jobs::queue::{JobError, JobQueue};
use futures_util::future::BoxFuture;
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params};

use super::error::ContribError;
use super::models::*;
use super::rules::album_input_revisions;
use super::seams::*;
use super::transition::{
    ContributionUpdate, Effect, Finish, Invalidation, Update, transition, verification_outcome,
};
use super::worker::{Completion, VERIFY};
use crate::identity;
use crate::identity::sql::PERSONS;

const PROVIDER: &str = "musicbrainz";
/// Attempt trigger v2 stamped on contribution verification attempts.
const ATTEMPT_TRIGGER: &str = "contribution_submission";
const DAY: f64 = 86_400.0;

impl From<rusqlite::Error> for ContribError {
    fn from(error: rusqlite::Error) -> Self {
        droppedneedle_kernel::db::DbError::from(error).into()
    }
}

impl From<JobError> for ContribError {
    fn from(error: JobError) -> Self {
        match error {
            JobError::LeaseLost => {
                Self::stale("The contribution verification job changed before completion.")
            }
            JobError::Db(error) => error.into(),
            other => Self::Storage(other.to_string()),
        }
    }
}

/// SQLite contribution store and identity reader.
#[derive(Clone)]
pub struct SqliteContributions {
    db: Db,
    queue: JobQueue,
}

impl SqliteContributions {
    /// The store on `db`, queueing verification on `queue`.
    pub fn new(db: Db, queue: JobQueue) -> Self {
        Self { db, queue }
    }

    /// The jobs table verification runs on.
    pub fn queue(&self) -> &JobQueue {
        &self.queue
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn db(&self) -> &Db {
        &self.db
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

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, ContribError> {
    serde_json::to_string(value)
        .map_err(|error| ContribError::Data(format!("Contribution document not encoded: {error}")))
}

fn from_json<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, ContribError> {
    serde_json::from_str(raw)
        .map_err(|_| ContribError::Data("Unsupported persisted contribution document.".into()))
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
     updated_at, row_revision, last_verification_failure";

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
    last_verification_failure: Option<String>,
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
        last_verification_failure: row.get(24)?,
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
    let live = album_input(conn, &raw.local_album_id)?;
    let links = AlbumLinks {
        identity: album_identity(conn, &raw.local_album_id)?,
        chosen_artist_mbid: match &live {
            Some(live) => chosen_artist_identity(conn, &live.artist_id)?,
            None => None,
        },
        artist_id: live.as_ref().map(|live| live.artist_id.clone()),
    };
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
        last_verification_failure: raw.last_verification_failure,
        album_active: live.is_some(),
        current_input_revision: live
            .as_ref()
            .map(|l| l.input_revision.clone())
            .unwrap_or_default(),
        current_album_row_revision: live.map(|l| l.row_revision).unwrap_or(0),
        links,
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
/// at: the one place a contribution's state is written. `row.row_revision`
/// already holds the new revision.
fn save_row(conn: &Connection, row: &ContributionRow, expected: i64) -> Result<(), ContribError> {
    let changed = conn
        .prepare_cached(
            "UPDATE library_contribution_drafts SET state = ?3, updated_by_user_id = ?4, \
             album_row_revision = ?5, input_revision = ?6, resolved_draft_json = ?7, \
             source_selection_json = ?8, provider_snapshot_expires_at = ?9, \
             duplicate_result_json = ?10, duplicate_checked_at = ?11, \
             duplicate_input_revision = ?12, result_release_mbid = ?13, result_source = ?14, \
             result_received_at = ?15, seed_snapshot_json = ?16, seed_hash = ?17, \
             seeded_at = ?18, terminal_at = ?19, updated_at = ?20, row_revision = ?21, \
             last_verification_failure = ?22 WHERE id = ?1 AND row_revision = ?2",
        )?
        .execute(params![
            row.id,
            expected,
            state_str(row.state),
            row.updated_by_user_id,
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
            row.last_verification_failure,
        ])?;
    if changed != 1 {
        return Err(ContribError::stale(
            "The contribution changed before it could be saved.",
        ));
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

/// End the contribution's queued and running verification.
fn cancel_jobs(queue: &JobQueue, tx: &Transaction, contribution_id: &str) -> Result<(), JobError> {
    queue.cancel(tx, VERIFY, contribution_id)?;
    Ok(())
}

/// Queue a verification of the contribution; a queued one already there
/// stands.
fn enqueue_job(queue: &JobQueue, tx: &Transaction, contribution_id: &str) -> Result<(), JobError> {
    queue.enqueue(tx, VERIFY, contribution_id, &serde_json::json!({}))?;
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

/// The artist's MusicBrainz link when a person or v2 made it. The app's
/// own links are revisable, so a contribution may replace them.
fn chosen_artist_identity(conn: &Connection, artist_id: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(&format!(
        "SELECT provider_artist_id FROM local_artist_external_identities \
         WHERE local_artist_id = ?1 AND provider = ?2 AND decision_source IN {PERSONS}"
    ))?
    .query_row(params![artist_id, PROVIDER], |row| row.get(0))
    .optional()
}

/// The row linked to `artist_mbid`, and whether the app made the link.
fn artist_owner(conn: &Connection, artist_mbid: &str) -> rusqlite::Result<Option<(String, bool)>> {
    conn.prepare_cached(&format!(
        "SELECT local_artist_id, decision_source NOT IN {PERSONS} \
         FROM local_artist_external_identities \
         WHERE provider = ?1 AND lower(provider_artist_id) = lower(?2) \
         ORDER BY local_artist_id LIMIT 1"
    ))?
    .query_row(params![PROVIDER, artist_mbid], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })
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
    tracks: &'a [TrackEvidence],
}

/// Commit the album identity (and the artist identity when free) as manual
/// decisions; returns the album's new row revision. Callers have already
/// ruled out conflicting album and artist identities.
fn commit_link(
    conn: &Transaction<'_>,
    link: &LinkIdentity<'_>,
    now: f64,
) -> Result<i64, ContribError> {
    let mut album_revision = link.album_revision;
    let album_unlinked = album_identity(conn, link.album_id)?.is_none();
    if album_unlinked {
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
    }
    if let Some(wanted) = link.artist_mbid {
        let owner = artist_owner(conn, wanted)?;
        // The app's own link of the id on another row gives way to the
        // person's, with no merge to suggest.
        if let Some((other, true)) = &owner
            && other != link.artist_id
        {
            conn.prepare_cached(&format!(
                "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1 \
                 AND provider = ?2 AND decision_source NOT IN {PERSONS}"
            ))?
            .execute(params![other, PROVIDER])?;
            conn.prepare_cached(
                "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 \
                 WHERE id = ?1",
            )?
            .execute(params![other, now])?;
        }
        match owner {
            Some((owner, false)) if owner != link.artist_id => {
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
            // A person's or v2's link of the row stays; the app's own link,
            // to this id or another, is replaced by the person's.
            _ if chosen_artist_identity(conn, link.artist_id)?.is_none() => {
                conn.prepare_cached(
                    "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 \
                     WHERE id = ?1",
                )?
                .execute(params![link.artist_id, now])?;
                conn.prepare_cached(
                    "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                     provider_artist_id, decision_source, attempt_id, selected_by_user_id, \
                     selected_at) VALUES (?1, ?2, ?3, 'manual', ?4, ?5, ?6) \
                     ON CONFLICT (local_artist_id, provider) DO UPDATE SET \
                     provider_artist_id = excluded.provider_artist_id, \
                     decision_source = excluded.decision_source, \
                     attempt_id = excluded.attempt_id, \
                     selected_by_user_id = excluded.selected_by_user_id, \
                     selected_at = excluded.selected_at, \
                     row_revision = local_artist_external_identities.row_revision + 1",
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
            _ => {}
        }
    }
    // The album's match when it had none, and each paired file's track
    // identity on the release, as v2 wrote it: recording, release track and
    // position, a manual decision. Only files of this album are touched.
    let positive = |value: Option<i64>| value.filter(|position| *position > 0);
    let tracks: Vec<identity::LinkedTrack<'_>> = link
        .tracks
        .iter()
        .map(|track| identity::LinkedTrack {
            local_track_id: &track.local_track_id,
            recording_mbid: &track.recording_mbid,
            release_track_mbid: track.release_track_mbid.as_deref(),
            disc: positive(track.medium_position),
            position: positive(track.track_position),
        })
        .collect();
    identity::link(
        conn,
        &identity::Link {
            album_id: link.album_id,
            release_group_mbid: link.release_group_mbid,
            release_mbid: link.release_mbid,
            matcher_version: &link.attempt.matcher_version,
            attempt_id: &link.attempt.id,
            selected_by: link.selected_by,
        },
        album_unlinked,
        &tracks,
        now,
    )?;
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

/// A saved change and what the catalog is told once it commits.
#[derive(Debug, Clone)]
pub struct Applied {
    pub row: ContributionRow,
    pub invalidate: Option<Invalidation>,
}

/// Run `update` on the loaded `row`: save the result under the revision it
/// was read at, then run its effects in the same transaction. Returns the
/// saved row as `transition` left it and the catalog invalidation to run
/// after the commit.
fn apply(
    conn: &Transaction<'_>,
    queue: &JobQueue,
    row: &ContributionRow,
    update: Update,
    now: f64,
) -> Result<(ContributionRow, Option<Invalidation>), ContribError> {
    let (next, effects) = transition(row, update, now)?;
    if next.row_revision != row.row_revision {
        save_row(conn, &next, row.row_revision)?;
    }
    let mut invalidate = None;
    for effect in effects {
        match effect {
            Effect::ConsumeTokens => consume_tokens(conn, &next.id, now)?,
            Effect::InsertToken {
                token_hash,
                requested_by_user_id,
                expires_at,
            } => {
                conn.prepare_cached(
                    "INSERT INTO library_contribution_callback_tokens (token_hash, \
                     contribution_id, requested_by_user_id, expires_at, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )?
                .execute(params![
                    token_hash,
                    next.id,
                    requested_by_user_id,
                    expires_at,
                    now
                ])?;
            }
            Effect::CancelJobs => cancel_jobs(queue, conn, &next.id)?,
            Effect::EnqueueJob => enqueue_job(queue, conn, &next.id)?,
            Effect::InsertAttempt {
                attempt,
                input_revision,
                state,
                reason,
                selected_key,
            } => insert_attempt(
                conn,
                &attempt,
                &input_revision,
                state,
                reason.as_deref(),
                selected_key.as_deref(),
            )?,
            Effect::LinkIdentity {
                local_album_id,
                artist_id,
                album_revision,
                release_mbid,
                release_group_mbid,
                artist_mbid,
                attempt,
                selected_by,
                tracks,
            } => {
                let linked = commit_link(
                    conn,
                    &LinkIdentity {
                        album_id: &local_album_id,
                        artist_id: &artist_id,
                        album_revision,
                        release_mbid: &release_mbid,
                        release_group_mbid: &release_group_mbid,
                        artist_mbid: artist_mbid.as_deref(),
                        attempt: &attempt,
                        selected_by: selected_by.as_deref(),
                        tracks: &tracks,
                    },
                    now,
                )?;
                // The row was saved with the revision the link leaves.
                if linked != next.album_row_revision {
                    return Err(ContribError::stale(
                        "The album changed before the release could be linked.",
                    ));
                }
            }
            Effect::InvalidateCatalog(invalidation) => invalidate = Some(invalidation),
        }
    }
    Ok((next, invalidate))
}

impl SqliteContributions {
    // The arguments mirror the v2 persistence signatures one for one so
    // guards stay comparable; bundling them would hide the mapping.
    #[allow(clippy::too_many_arguments)]
    pub fn create_or_get<'a>(
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
        Box::pin(
            self.db
                .write(Lane::Foreground, "contrib.create", move |conn| {
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
                }),
        )
    }

    pub fn get<'a>(
        &'a self,
        contribution_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ContributionRow>, ContribError>> {
        let id = contribution_id.to_owned();
        Box::pin(self.db.read("contrib.get", move |conn| load_row(conn, &id)))
    }

    pub fn get_active_for_album<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ContributionRow>, ContribError>> {
        let album = album_id.to_owned();
        Box::pin(self.db.read(
            "contrib.active_for_album",
            move |conn| match active_id_for_album(conn, &album)? {
                Some(id) => Ok(load_row(conn, &id)?.filter(|row| row.album_active)),
                None => Ok(None),
            },
        ))
    }

    /// Apply a curator's change to the current row; a row read at another
    /// revision than `expected_row_revision` is refused. Written on the
    /// caller's lane.
    pub fn compare_and_set<'a>(
        &'a self,
        lane: Lane,
        contribution_id: &'a str,
        expected_row_revision: i64,
        actor_user_id: &'a str,
        now: f64,
        update: ContributionUpdate,
    ) -> BoxFuture<'a, Result<Applied, ContribError>> {
        let (id, actor) = (contribution_id.to_owned(), actor_user_id.to_owned());
        let queue = self.queue.clone();
        Box::pin(self.db.write(lane, "contrib.update", move |conn| {
            let row = require_row(conn, &id)?;
            let update = Update::Edit {
                expected_row_revision,
                actor_user_id: actor,
                update: Box::new(update),
            };
            let (_, invalidate) = apply(conn, &queue, &row, update, now)?;
            Ok(Applied {
                row: require_row(conn, &id)?,
                invalidate,
            })
        }))
    }

    /// Mark a contribution stale, on the caller's lane.
    pub fn mark_stale<'a>(
        &'a self,
        lane: Lane,
        contribution_id: &'a str,
        expected_row_revision: i64,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        let id = contribution_id.to_owned();
        let queue = self.queue.clone();
        Box::pin(self.db.write(lane, "contrib.mark_stale", move |conn| {
            let row = require_row(conn, &id)?;
            let update = Update::MarkStale {
                expected_row_revision,
            };
            apply(conn, &queue, &row, update, now)?;
            require_row(conn, &id)
        }))
    }

    /// Returns the contribution id, its verification queued unless it went
    /// stale; `None` means the token is unknown, consumed, or expired (v2
    /// `ResourceNotFoundError`). State conflicts surface as `Err` so they
    /// keep their own message.
    pub fn consume_callback_token<'a>(
        &'a self,
        token_hash: &'a str,
        release_mbid: &'a str,
        now: f64,
    ) -> BoxFuture<'a, Result<CallbackConsumption, ContribError>> {
        let (hash, release) = (token_hash.to_owned(), release_mbid.to_owned());
        let queue = self.queue.clone();
        Box::pin(self.db.write(
            Lane::Foreground,
            "contrib.consume_callback_token",
            move |conn| {
                let token: Option<String> = conn
                    .prepare_cached(
                        "SELECT contribution_id FROM library_contribution_callback_tokens \
                     WHERE token_hash = ?1 AND consumed_at IS NULL AND expires_at >= ?2",
                    )?
                    .query_row(params![hash, now], |row| row.get(0))
                    .optional()?;
                let Some(contribution_id) = token else {
                    return Ok(None);
                };
                let Some(row) = load_row(conn, &contribution_id)? else {
                    return Ok(None);
                };
                let update = Update::Callback {
                    release_mbid: release,
                };
                apply(conn, &queue, &row, update, now)?;
                Ok(Some(row.id))
            },
        ))
    }

    /// Contributions whose provider data is due to go. A row that no
    /// longer decodes is logged and left out, so the others still go.
    pub fn list_for_provider_purge<'a>(
        &'a self,
        now: f64,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<ContributionRow>, ContribError>> {
        let bounded = limit.clamp(1, 1_000) as i64;
        Box::pin(async move {
            self.db.read("contrib.list_for_provider_purge", move |conn| {
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
        })
    }

    /// Rebuild retires the stale row and inserts a fresh row (new id,
    /// state draft); returns the new row (v2 `rebuild_library_contribution`).
    #[allow(clippy::too_many_arguments)]
    pub fn rebuild<'a>(
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
        let queue = self.queue.clone();
        Box::pin(
            self.db
                .write(Lane::Foreground, "contrib.rebuild", move |conn| {
                    let old = require_row(conn, &id)?;
                    let update = Update::Retire {
                        expected_row_revision,
                        album_row_revision,
                        input_revision: input.clone(),
                    };
                    apply(conn, &queue, &old, update, now)?;
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
                }),
        )
    }

    /// End a verification in one write: the contribution's outcome, its
    /// failure code and the job's completion (`complete`), which rolls the
    /// write back when another worker holds the job. Returns the outcome
    /// and, on a link, what the catalog is told.
    pub fn finish_verification<'a>(
        &'a self,
        contribution_id: &'a str,
        complete: Completion,
        finish: Finish,
        now: f64,
    ) -> BoxFuture<'a, Result<(VerificationOutcome, Option<Invalidation>), ContribError>> {
        let contribution_id = contribution_id.to_owned();
        let queue = self.queue.clone();
        Box::pin(self.db.write(
            Lane::Background,
            "contrib.finish_verification",
            move |conn| {
                let row = require_row(conn, &contribution_id)?;
                let update = Update::FinishVerification(Box::new(finish));
                let (next, invalidate) = apply(conn, &queue, &row, update, now)?;
                complete(conn)?;
                Ok((verification_outcome(&next), invalidate))
            },
        ))
    }

    /// Queue a verification for every verifying contribution that has
    /// none queued or running: one a v2 import carried, or one whose job
    /// failed for good. Returns how many were queued.
    pub async fn queue_missing_verifications(&self) -> Result<u64, ContribError> {
        let queue = self.queue.clone();
        self.db
            .write(Lane::Background, "contrib.queue_missing", move |conn| {
                let ids: Vec<String> = conn
                    .prepare_cached(super::sql::VERIFYING_WITHOUT_JOB.text())?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                for id in &ids {
                    enqueue_job(&queue, conn, id)?;
                }
                Ok(ids.len() as u64)
            })
            .await
    }

    /// Drop old callback tokens, seed snapshots nobody can use any more,
    /// and closed contributions that never got a result. A row whose seed
    /// snapshot is due to go but no longer decodes is logged and left.
    pub fn clean_records<'a>(&'a self, now: f64) -> BoxFuture<'a, Result<(), ContribError>> {
        let queue = self.queue.clone();
        Box::pin(async move {
            self.db
                .write(Lane::Background, "contrib.clean_records", move |conn| {
                    conn.execute(
                        "DELETE FROM library_contribution_callback_tokens WHERE \
                         (consumed_at IS NOT NULL AND consumed_at < ?1) OR \
                         (consumed_at IS NULL AND expires_at < ?2)",
                        params![now - 30.0 * DAY, now - 7.0 * DAY],
                    )?;
                    let ids: Vec<String> = conn
                        .prepare_cached(
                            "SELECT id FROM library_contribution_drafts \
                             WHERE seed_snapshot_json IS NOT NULL \
                             AND (state IN ('linked','cancelled','stale') OR (seeded_at IS NOT NULL \
                             AND NOT EXISTS (SELECT 1 FROM library_contribution_callback_tokens t \
                             WHERE t.contribution_id = library_contribution_drafts.id \
                             AND t.consumed_at IS NULL AND t.expires_at >= ?1))) ORDER BY id",
                        )?
                        .query_map(params![now], |row| row.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    for id in ids {
                        match load_row(conn, &id) {
                            Ok(Some(row)) => {
                                apply(conn, &queue, &row, Update::ClearSeedSnapshot, now)?;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                // A row that no longer decodes still loses its snapshot.
                                tracing::warn!(%error, contribution = id, "contribution row does not decode; clearing its seed snapshot directly");
                                conn.execute(
                                    "UPDATE library_contribution_drafts SET seed_snapshot_json = NULL, \
                                     row_revision = row_revision + 1 WHERE id = ?1",
                                    params![id],
                                )?;
                            }
                        }
                    }
                    conn.execute(
                        "DELETE FROM library_contribution_drafts \
                         WHERE state IN ('cancelled','stale') AND result_release_mbid IS NULL \
                         AND terminal_at < ?1",
                        params![now - 90.0 * DAY],
                    )?;
                    Ok(())
                })
                .await
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

/// The album identification context contributions read.
impl SqliteContributions {
    /// `Ok(None)` when the album is gone; `Err` when it could not be read.
    pub fn album_context<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumIdentificationContext>, ContribError>> {
        let album = album_id.to_owned();
        Box::pin(self.db.read("contrib.album_context", move |conn| {
            album_context(conn, &album)
        }))
    }

    /// Input revisions owned by scan and identify (v2 `album_input_revisions`):
    /// tag, file, policy. The contribution input revision is the
    /// colon-joined triple.
    pub fn input_revisions(&self, tracks: &[IdentityTrack]) -> (String, String, String) {
        album_input_revisions(tracks.iter().map(|t| (t.id.as_str(), &t.input)))
    }
}
