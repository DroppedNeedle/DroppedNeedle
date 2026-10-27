//! A file the app wrote that something else has changed since is the
//! user's. The app remembers the bytes of every file it writes or accepts
//! (`library_track_management_state.written_sha256`). When an automatic
//! run would rewrite the tags or pictures of a file whose bytes are no
//! longer the recorded ones, it writes nothing and marks the album as
//! taken over (`library_album_takeovers`). Automatic management then
//! leaves the album alone until a person hands it back.
//!
//! The mark never touches the album's match, plays, favorites or
//! playlists, and the app never lifts it by itself.
//!
//! The record itself, and whether a file was changed since the app wrote
//! it, are in [`crate::publish::record`]: the publisher's commit
//! asks the same question.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::models::{ManagementError, PlannedItem};
use super::reasons::ItemReason;
use crate::catalog_revision::bump_catalog;
use crate::clock::LibraryClock as _;
use crate::operations::decisions::{CatalogAction, record_action};
use crate::publish::record::changed_since_written;
pub use crate::publish::record::record_sql;
use crate::wiring::LibrarySetup;
use droppedneedle_kernel::db::Lane;

/// Skip and block code of an album the app stepped back from.
pub const TAKEN_OVER: &str = "TAKEN_OVER";
/// Terminal code of an automatic run that wrote nothing because the
/// album's files were changed outside the app.
pub const FILES_CHANGED_OUTSIDE: &str = "FILES_CHANGED_OUTSIDE";
/// Block code of a file a preview of several albums leaves alone because
/// it was changed outside the app since the app wrote it.
pub const CHANGED_OUTSIDE: &str = "CHANGED_OUTSIDE";

/// Why a preview of several albums leaves a file alone.
pub(crate) fn changed_outside_reason() -> ItemReason {
    ItemReason {
        code: CHANGED_OUTSIDE.to_owned(),
        message: "This file was changed outside DroppedNeedle since DroppedNeedle last wrote it."
            .to_owned(),
        action: "Preview the album by itself to see what would change. Applying that preview \
                 overwrites the change."
            .to_owned(),
    }
}

/// Why the app stepped back from an album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ManagementTakeoverCause)]
pub enum Cause {
    /// A file the app wrote was changed outside it.
    FilesChanged,
    /// A scan followed the album across a retag.
    RetagFollowed,
    /// A person undid a run of the album's profile.
    RunUndone,
    /// New files appeared beside an album left unmanaged, kept as a custom
    /// edition or stepped back from, in the scan that saw that album's
    /// files go: they may be a person's retag of it.
    ReplacedHandsOff,
}

impl Cause {
    pub const ALL: [Self; 4] = [
        Self::FilesChanged,
        Self::RetagFollowed,
        Self::RunUndone,
        Self::ReplacedHandsOff,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilesChanged => "files_changed",
            Self::RetagFollowed => "retag_followed",
            Self::RunUndone => "run_undone",
            Self::ReplacedHandsOff => "replaced_hands_off",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cause| cause.as_str() == value)
    }
}

/// Whether the app has a record of this track's file.
pub fn has_record(conn: &Connection, track_id: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT 1 FROM library_track_management_state s \
             WHERE s.local_track_id = ?1 AND {}",
            record_sql()
        ))?
        .query_row(params![track_id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Each track's management state revision, for the tracks of `albums`
/// that have a state row.
pub(crate) fn state_revisions(
    conn: &Connection,
    albums: &[String],
) -> rusqlite::Result<std::collections::HashMap<String, i64>> {
    let mut statement = conn.prepare(
        "SELECT s.local_track_id, s.row_revision FROM library_track_management_state s \
         JOIN local_tracks t ON t.id = s.local_track_id WHERE t.local_album_id = ?1",
    )?;
    let mut revisions = std::collections::HashMap::new();
    for album in albums {
        let rows = statement.query_map(params![album], |row| Ok((row.get(0)?, row.get(1)?)))?;
        for row in rows {
            let (track, revision) = row?;
            revisions.insert(track, revision);
        }
    }
    Ok(revisions)
}

/// The files of a finished plan that were changed since the app wrote
/// them.
#[derive(Debug, Default)]
pub(crate) struct Changed {
    /// Those whose tags or embedded pictures the plan would change, as
    /// (album, track).
    pub counted: Vec<(String, String)>,
    /// Those the plan may write without changing them: the change is only
    /// in what the plan leaves alone. As (track, the hash the plan read,
    /// the track's state revision before planning).
    pub accepted: Vec<(String, String, Option<i64>)>,
}

/// Look through a finished plan for files changed since the app wrote
/// them (see [`changed_since_written`]). A file the app has no record of
/// does not count unless it stands in for one that went missing, nor one
/// that was blocked before its bytes were read, nor one whose state the
/// app itself rewrote while the plan was made (`before` holds each
/// track's state revision from before).
pub(crate) fn changed_outside(
    conn: &Connection,
    items: &[PlannedItem],
    before: &std::collections::HashMap<String, i64>,
) -> rusqlite::Result<Changed> {
    let mut changed = Changed::default();
    for item in items {
        let rewrites = item.diff.tags_changed
            || item.diff.artwork_changed
            || !item.diff.scrubbed_raw_tags.is_empty();
        let Some(track_id) = item.track_id.as_deref() else {
            continue;
        };
        if item.fingerprint.is_empty() || !(rewrites || item.eligibility.writes()) {
            continue;
        }
        let revision: Option<i64> = conn
            .prepare_cached(
                "SELECT row_revision FROM library_track_management_state \
                 WHERE local_track_id = ?1",
            )?
            .query_row(params![track_id], |row| row.get(0))
            .optional()?;
        if revision != before.get(track_id).copied() {
            continue;
        }
        if !changed_since_written(
            conn,
            track_id,
            &item.source_root,
            &item.source_rel,
            &item.fingerprint,
        )? {
            continue;
        }
        if rewrites {
            changed
                .counted
                .push((item.album_id.clone(), track_id.to_owned()));
        } else {
            changed
                .accepted
                .push((track_id.to_owned(), item.fingerprint.clone(), revision));
        }
    }
    Ok(changed)
}

/// Record the bytes a plan read as the app's, for files changed since the
/// app wrote them only in what the plan leaves alone. Skips a file whose
/// state moved since the plan looked.
pub(crate) fn accept(
    tx: &Transaction<'_>,
    accepted: &[(String, String, Option<i64>)],
) -> rusqlite::Result<()> {
    for (track_id, sha256, revision) in accepted {
        match revision {
            Some(revision) => tx.execute(
                "UPDATE library_track_management_state SET written_sha256 = ?2, \
                 row_revision = row_revision + 1 WHERE local_track_id = ?1 AND row_revision = ?3",
                params![track_id, sha256, revision],
            )?,
            None => tx.execute(
                "INSERT INTO library_track_management_state (local_track_id, written_sha256) \
                 VALUES (?1, ?2) ON CONFLICT (local_track_id) DO NOTHING",
                params![track_id, sha256],
            )?,
        };
    }
    Ok(())
}

/// Where a profile run keeps, on its album's work row, the records it
/// replaced (see [`record_applied`]), so its undo can put them back.
const RECORDS_BEFORE: &str = "$.records_before";

/// The records a job's album work row keeps for its undo, by track.
fn records_before(
    tx: &Transaction<'_>,
    job_id: &str,
    ordinal: i64,
) -> rusqlite::Result<serde_json::Map<String, serde_json::Value>> {
    let stored: Option<String> = tx
        .query_row(
            "SELECT json_extract(result_json, ?3) FROM library_operation_work \
             WHERE job_id = ?1 AND ordinal = ?2 AND json_valid(result_json)",
            params![job_id, ordinal, RECORDS_BEFORE],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(stored
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}

/// After a run that applied the whole profile to an album, record the
/// bytes it placed as the app's for each file it wrote, even over a file
/// changed outside: every managed field was just written from the edition,
/// so nothing of the change is left to protect. The commit kept the old
/// record (as for a rename or a tag edit); this replaces it.
///
/// `placed` is each written track with the bytes this bundle staged for
/// it. A row changes only while this job is its last run and the catalog
/// still holds exactly those bytes, so a person's later write is never
/// taken for this one. The records replaced are kept on the album's work
/// row, in this transaction, and returned for the undo
/// ([`restore_records`]); a second call after a crash keeps them.
pub(crate) fn record_applied(
    tx: &Transaction<'_>,
    job_id: &str,
    ordinal: i64,
    placed: &[(String, String)],
) -> rusqlite::Result<serde_json::Map<String, serde_json::Value>> {
    let mut before = records_before(tx, job_id, ordinal)?;
    let mut changed = false;
    for (track_id, staged) in placed {
        let current: Option<Option<String>> = tx
            .query_row(
                "SELECT written_sha256 FROM library_track_management_state \
                 WHERE local_track_id = ?1 AND last_operation_job_id = ?2 \
                 AND applied_projection_hash = ?3",
                params![track_id, job_id, staged],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else { continue };
        if current.as_deref() == Some(staged.as_str()) {
            continue;
        }
        tx.execute(
            "UPDATE library_track_management_state SET written_sha256 = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1",
            params![track_id, staged],
        )?;
        before.insert(track_id.clone(), serde_json::json!(current));
        changed = true;
    }
    if changed {
        tx.execute(
            "UPDATE library_operation_work SET result_json = json_set(CASE \
             WHEN json_valid(result_json) AND json_type(result_json) = 'object' \
             THEN result_json ELSE '{}' END, ?3, json(?4)) \
             WHERE job_id = ?1 AND ordinal = ?2",
            params![
                job_id,
                ordinal,
                RECORDS_BEFORE,
                serde_json::Value::Object(before.clone()).to_string()
            ],
        )?;
    }
    Ok(before)
}

/// After an undo of a profile run, put back the records that run replaced
/// ([`record_applied`]): the undo restores the bytes from before the run,
/// and a hand edit among them is the user's again. Only a row whose record
/// is still the bytes the undo just placed.
pub(crate) fn restore_records(
    tx: &Transaction<'_>,
    source_job_id: &str,
    ordinal: i64,
) -> rusqlite::Result<()> {
    for (track_id, record) in records_before(tx, source_job_id, ordinal)? {
        tx.execute(
            "UPDATE library_track_management_state SET written_sha256 = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1 \
             AND written_sha256 IS applied_projection_hash",
            params![track_id, record.as_str()],
        )?;
    }
    Ok(())
}

/// Mark an album a scan followed across a retag and stamped
/// ([`super::automatic::stamp_retag_followed`]), and clear the stamp.
/// False when the album carries no stamp.
pub(crate) fn mark_followed(
    tx: &Transaction<'_>,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let cleared = tx.execute(
        "UPDATE library_management_automatic_queue SET retag_followed_at = NULL \
         WHERE local_album_id = ?1 AND retag_followed_at IS NOT NULL",
        params![album_id],
    )?;
    if cleared == 0 {
        return Ok(false);
    }
    mark(tx, album_id, Cause::RetagFollowed, &[], None, now)?;
    Ok(true)
}

/// Whether the app stepped back from this album.
pub fn is_marked(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .prepare_cached("SELECT 1 FROM library_album_takeovers WHERE local_album_id = ?1")?
        .query_row(params![album_id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn millis(now: f64) -> i64 {
    (now * 1000.0) as i64
}

/// Mark an album as taken over. Does nothing when it already is; returns
/// whether it wrote. A new mark parks the album for automatic management,
/// records one audit action and moves the catalog revision.
pub(crate) fn mark(
    tx: &Transaction<'_>,
    album_id: &str,
    cause: Cause,
    changed_track_ids: &[String],
    job_id: Option<&str>,
    now: f64,
) -> rusqlite::Result<bool> {
    let wrote = tx.execute(
        "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES (?1, ?2, ?3) ON CONFLICT (local_album_id) DO NOTHING",
        params![album_id, cause.as_str(), millis(now)],
    )?;
    if wrote == 0 {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO library_management_automatic_queue (local_album_id, queued_at, not_before, \
         last_reason_code) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (local_album_id) DO UPDATE SET not_before = excluded.not_before, \
         attempts = 0, last_reason_code = excluded.last_reason_code",
        params![album_id, now, super::automatic::PARKED, TAKEN_OVER],
    )?;
    record_action(
        tx,
        CatalogAction {
            actor: "",
            kind: "management_stepped_back",
            album_id,
            job_id,
            before: serde_json::json!({}),
            after: serde_json::json!({ "changed_track_ids": changed_track_ids }),
            reason: cause.as_str(),
        },
        now,
    )?;
    bump_catalog(tx)?;
    Ok(true)
}

/// Give `to` the mark `from` carries (cause and times kept), unless it
/// has one. A merge and a split carry the mark with the tracks.
pub(crate) fn copy_mark(tx: &Transaction<'_>, from: &str, to: &str) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO library_album_takeovers (local_album_id, cause, noticed_at, \
         handed_back_at) SELECT ?2, cause, noticed_at, handed_back_at \
         FROM library_album_takeovers WHERE local_album_id = ?1",
        params![from, to],
    )?;
    Ok(())
}

/// Stamp every marked album for handing back and record who asked. The
/// worker then hands them back one at a time (see [`next_stamped`]).
/// Returns how many albums were stamped. Reads no file.
pub(crate) fn stamp_all(tx: &Transaction<'_>, actor: &str, now: f64) -> rusqlite::Result<usize> {
    let albums: Vec<String> = tx
        .prepare(
            "UPDATE library_album_takeovers SET handed_back_at = ?1, hand_back_tries = 0, \
             hand_back_retry_at = NULL WHERE handed_back_at IS NULL RETURNING local_album_id",
        )?
        .query_map(params![millis(now)], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for album_id in &albums {
        record_action(tx, handed_back_action(actor, album_id), now)?;
    }
    if !albums.is_empty() {
        bump_catalog(tx)?;
    }
    Ok(albums.len())
}

fn handed_back_action<'a>(actor: &'a str, album_id: &'a str) -> CatalogAction<'a> {
    CatalogAction {
        actor,
        kind: "management_handed_back",
        album_id,
        job_id: None,
        before: serde_json::json!({}),
        after: serde_json::json!({}),
        reason: "MANAGEMENT_HANDED_BACK",
    }
}

/// How many times a stamped album's hand-back is tried before it waits
/// for a person again, as automatic management's own retries.
const HAND_BACK_TRIES: i64 = 8;
/// First wait before a failed hand-back is tried again; it doubles each
/// time, up to six hours.
const HAND_BACK_RETRY_SECONDS: f64 = 60.0;

/// The stamped album whose turn is next and due: oldest stamp first.
pub(crate) fn next_stamped(conn: &Connection, now: f64) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT local_album_id FROM library_album_takeovers WHERE handed_back_at IS NOT NULL \
         AND (hand_back_retry_at IS NULL OR hand_back_retry_at <= ?1) \
         ORDER BY handed_back_at, local_album_id LIMIT 1",
        params![millis(now)],
        |row| row.get(0),
    )
    .optional()
}

/// A stamped album's hand-back failed: try it again later, each wait
/// twice the last. After the last try the stamp goes, the audit log says
/// so, and the album is listed as left unmanaged again for a person to
/// hand back.
pub(crate) fn hand_back_failed(
    conn: &Transaction<'_>,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let tries: i64 = conn.query_row(
        "UPDATE library_album_takeovers SET hand_back_tries = hand_back_tries + 1 \
         WHERE local_album_id = ?1 RETURNING hand_back_tries",
        params![album_id],
        |row| row.get(0),
    )?;
    if tries >= HAND_BACK_TRIES {
        conn.execute(
            "UPDATE library_album_takeovers SET handed_back_at = NULL, hand_back_tries = 0, \
             hand_back_retry_at = NULL WHERE local_album_id = ?1",
            params![album_id],
        )?;
        tracing::info!(
            album_id,
            tries,
            "hand-back given up: the album's files could not be read"
        );
        record_action(
            conn,
            CatalogAction {
                actor: "",
                kind: "management_hand_back_given_up",
                album_id,
                job_id: None,
                before: serde_json::json!({}),
                after: serde_json::json!({ "tries": tries }),
                reason: "HAND_BACK_UNREADABLE",
            },
            now,
        )?;
        bump_catalog(conn)?;
        return Ok(());
    }
    let delay = (HAND_BACK_RETRY_SECONDS * 2f64.powi((tries - 1) as i32)).min(6.0 * 3600.0);
    conn.execute(
        "UPDATE library_album_takeovers SET hand_back_retry_at = ?2 WHERE local_album_id = ?1",
        params![album_id, millis(now + delay)],
    )?;
    Ok(())
}

/// How many times a person has handed the album back, as the audit log
/// has it. It only grows, whatever the clock does.
pub(crate) fn hand_backs(conn: &Connection, album_id: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM library_catalog_actions \
         WHERE local_album_id = ?1 AND action_kind = 'management_handed_back'",
        params![album_id],
        |row| row.get(0),
    )
}

/// Hand one album back to automatic management: record each file's bytes
/// as they are now, lift the mark and queue the album. The next automatic
/// run then finds the files as recorded and writes them from the album's
/// edition; an edit made after this is protected like any other.
///
/// `actor` is the person who asked, recorded in the audit log; `None`
/// when the request for every album already recorded it. False when the
/// album has no mark. Blocking: reads every file of the album, outside any
/// transaction. Writes no file.
pub(crate) fn hand_back(
    setup: &LibrarySetup,
    album_id: &str,
    actor: Option<&str>,
) -> Result<bool, ManagementError> {
    let files = setup
        .db
        .read_blocking("management.hand_back_files", |conn| {
            if !is_marked(conn, album_id)? {
                return Ok(None);
            }
            let mut statement = conn.prepare(
                "SELECT id, root_id, relative_path FROM local_tracks \
             WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY id",
            )?;
            let rows = statement.query_map(params![album_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            Ok::<_, ManagementError>(Some(
                rows.collect::<rusqlite::Result<Vec<(String, String, String)>>>()?,
            ))
        })?;
    let Some(files) = files else {
        return Ok(false);
    };
    // Nothing is recorded and the mark stays when the files cannot be read
    // at all (a share not mounted yet): recording nothing would hand over
    // whatever is there when it comes back.
    let unreadable = || {
        ManagementError::Conflict(
            "The album's files could not be read, so it was not handed back.".to_owned(),
        )
    };
    let sandbox = setup
        .publishing
        .publish_sandbox()
        .map_err(|_| unreadable())?;
    let hashed: Vec<(String, String, String, Option<String>)> = files
        .into_iter()
        .map(|(track_id, root_id, relative_path)| {
            let sha256 = sandbox
                .resolve_no_symlink(&root_id, &relative_path)
                .ok()
                .and_then(|path| crate::publish::paths::hash_regular_file(&path).ok())
                .map(|hashed| hashed.sha256);
            if sha256.is_none() {
                tracing::warn!(
                    track_id,
                    "file unreadable at hand-back; its bytes are not recorded"
                );
            }
            (track_id, root_id, relative_path, sha256)
        })
        .collect();
    if !hashed.is_empty() && hashed.iter().all(|(.., sha256)| sha256.is_none()) {
        return Err(unreadable());
    }
    let now = setup.clock.now_secs();
    // A person's request writes on the foreground lane, the worker's turn
    // through a hand-back of every album on the background lane.
    let lane = if actor.is_some() {
        Lane::Foreground
    } else {
        Lane::Background
    };
    setup.db.write_blocking(lane, "management.hand_back", |tx| {
        let lifted = tx.execute(
            "DELETE FROM library_album_takeovers WHERE local_album_id = ?1",
            params![album_id],
        )?;
        if lifted == 0 {
            return Ok(false);
        }
        // v2's record is spent: from here on the bytes below are the record.
        // Every v3 writer of `managed_path_revision` sets the job, so a value
        // with no job is v2's, even on a row a later write gave
        // `written_sha256`. Kept there, it would be a v2 record again once
        // that is cleared below.
        tx.execute(
            "UPDATE library_track_management_state SET managed_path_revision = NULL, \
             row_revision = row_revision + 1 WHERE managed_path_revision IS NOT NULL \
             AND last_operation_job_id IS NULL AND local_track_id IN \
             (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            params![album_id],
        )?;
        tx.execute(
            "UPDATE library_track_management_state SET written_sha256 = NULL \
             WHERE written_sha256 IS NOT NULL AND local_track_id IN \
             (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
            params![album_id],
        )?;
        for (track_id, root_id, relative_path, sha256) in &hashed {
            let Some(sha256) = sha256 else { continue };
            // Only for a file still where it was read.
            tx.execute(
                "INSERT INTO library_track_management_state (local_track_id, written_sha256) \
                 SELECT id, ?4 FROM local_tracks WHERE id = ?1 AND local_album_id = ?5 \
                 AND root_id = ?2 AND relative_path = ?3 AND availability = 'indexed' \
                 ON CONFLICT (local_track_id) DO UPDATE SET \
                 written_sha256 = excluded.written_sha256",
                params![track_id, root_id, relative_path, sha256, album_id],
            )?;
        }
        tx.execute(
            "UPDATE library_management_automatic_queue SET retag_followed_at = NULL \
             WHERE local_album_id = ?1",
            params![album_id],
        )?;
        if let Some(actor) = actor {
            record_action(tx, handed_back_action(actor, album_id), now)?;
        }
        bump_catalog(tx)?;
        super::automatic::queue(tx, album_id, now)?;
        Ok::<_, ManagementError>(true)
    })
}

/// An album the app stepped back from, for the list a person hands back
/// from.
#[derive(Debug, Clone)]
pub struct TakenOver {
    pub album_id: String,
    pub album_title: String,
    pub artist_name: Option<String>,
    pub cause: Cause,
    /// When the app stepped back, in milliseconds.
    pub noticed_at: i64,
}

/// The albums the app stepped back from and nobody has handed back yet.
#[derive(Debug, Clone, Default)]
pub struct Waiting {
    /// Newest first, `limit` at most.
    pub albums: Vec<TakenOver>,
    /// How many there are in all.
    pub total: i64,
    /// Albums already handed back that wait for the worker to get to them.
    pub handing_back: i64,
}

impl super::service::Management {
    /// The albums the app stepped back from, newest first. Blocking.
    pub fn takeovers(&self, limit: i64) -> Result<Waiting, ManagementError> {
        self.setup.db.read_blocking("management.takeovers", |conn| {
            Ok(waiting(conn, limit.clamp(1, 200))?)
        })
    }
}

/// The albums the app stepped back from, newest first.
pub fn waiting(conn: &Connection, limit: i64) -> rusqlite::Result<Waiting> {
    let (total, handing_back) = conn.query_row(
        "SELECT COALESCE(SUM(handed_back_at IS NULL), 0), \
         COALESCE(SUM(handed_back_at IS NOT NULL), 0) FROM library_album_takeovers",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut statement = conn.prepare(
        "SELECT k.local_album_id, a.title, a.album_artist_name, k.cause, k.noticed_at \
         FROM library_album_takeovers k JOIN local_albums a ON a.id = k.local_album_id \
         WHERE k.handed_back_at IS NULL \
         ORDER BY k.noticed_at DESC, k.local_album_id LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit], |row| {
        let cause: String = row.get(3)?;
        Ok(TakenOver {
            album_id: row.get(0)?,
            album_title: row.get(1)?,
            artist_name: row.get(2)?,
            cause: Cause::parse(&cause).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    format!("unknown takeover cause {cause}").into(),
                )
            })?,
            noticed_at: row.get(4)?,
        })
    })?;
    Ok(Waiting {
        albums: rows.collect::<rusqlite::Result<_>>()?,
        total,
        handing_back,
    })
}
