//! Filing plays under the track they were plays of.
//!
//! A play row holds names, and often a recording id, a length and a
//! source. Rows written before plays were filed under the catalog, and rows
//! carried from v2 whose library was scanned again, name no track, album or
//! artist, so player apps leave them out of "recently played", "most
//! played" and the play counts. The repair job here fills those three ids.
//! A play made on a connected server or as a preview goes through
//! [`match_play`] when it is written, by the same rules.
//!
//! A row is filed only when it fits exactly one playable track:
//!
//! - A play of the library (its source is `local` or none) is filed by its
//!   names alone: track, artist and album name, folded as the catalog
//!   folds them, fit one track, and the row's length, when it has one,
//!   agrees within two seconds. A row with no album name needs that
//!   length.
//! - Its recording id never files it and never chooses between two tracks:
//!   the same recording sits on several editions, and the one played may
//!   not be known to hold it yet. The id can only refuse: a track known
//!   only as other recordings than the row's is not a fit.
//! - A play through a connected server or a preview is filed by its
//!   recording id alone and never by names: it was not a play of the
//!   local file, so only the same recording, held by one track and not
//!   overruled by that track's accepted recording, is a clear match.
//!
//! What a row fits depends on what the catalog knows when it is looked at.
//! A row filed by names before its track is identified would be refused
//! afterwards, if identification gives the track another recording than
//! the row's. That is accepted: a filed row is never looked at again, and
//! the names rule already asks for exactly one fit and an agreeing length.
//!
//! Anything else is left as it is: a play under the wrong track is worse
//! than one that is not counted. Names, time and source are never changed,
//! and a row that already has an id is never touched.
//!
//! `library_play_history` belongs to playback. This module writes its
//! three id columns and nothing else, as merges do when they move plays.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::models::{Purpose, Scope, Summary};
use super::store::Unreadable;
use droppedneedle_kernel::db::fold::fold_text;

/// Version of the filing rules, kept on the job.
pub const PLAYS_VERSION: &str = "play-history-filing-v1";
/// Rows one transaction looks at.
pub const BATCH: usize = 200;
/// How far a row's length may be from the track's.
const LENGTH_SLACK_MS: f64 = 2000.0;

/// Terminal code of a filing job that looked at every row.
pub const FILED: &str = "PLAY_HISTORY_FILED";

/// Filed by the recording id.
pub const BY_RECORDING: &str = "RECORDING";
/// Filed by track, artist and album name.
pub const BY_NAMES: &str = "NAMES";
/// No playable track fits.
pub const NO_MATCH: &str = "NO_MATCH";
/// More than one track fits.
pub const SEVERAL_MATCHES: &str = "SEVERAL_MATCHES";
/// A play through a connected server or a preview that carries no
/// recording id.
pub const REMOTE_WITHOUT_RECORDING: &str = "REMOTE_WITHOUT_RECORDING";
/// Track and artist name only, and no length to check them by.
pub const NAMES_WITHOUT_ALBUM_OR_LENGTH: &str = "NAMES_WITHOUT_ALBUM_OR_LENGTH";
/// The one track the names fit is longer or shorter than the play.
pub const LENGTH_DISAGREES: &str = "LENGTH_DISAGREES";
/// The one track the names fit is known to be another recording.
pub const RECORDING_DISAGREES: &str = "RECORDING_DISAGREES";
/// What a play row says about what was played.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayKeys {
    pub track_name: String,
    pub artist_name: String,
    pub album_name: Option<String>,
    pub recording_mbid: Option<String>,
    pub release_group_mbid: Option<String>,
    pub duration_ms: Option<i64>,
    pub source: Option<String>,
}

/// What the catalog makes of one play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matched {
    /// The one track, and the key that found it ([`BY_RECORDING`] or
    /// [`BY_NAMES`]).
    Track { id: String, by: &'static str },
    /// Not filed, and why.
    Left(&'static str),
}

impl PlayKeys {
    /// True for a play of the local library: one that names it, or names
    /// no source. Every other source is a connected server or a preview.
    pub fn is_local(&self) -> bool {
        matches!(
            self.source.as_deref().map(str::trim),
            None | Some("" | "local")
        )
    }
}

fn text(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

struct RecordingFit {
    id: String,
    /// The recording is the track's accepted one, or its tag's with none
    /// accepted: no accepted recording says otherwise.
    unchallenged: bool,
}

/// The playable tracks that hold `recording`, as their accepted recording
/// or in their tags.
fn by_recording(conn: &Connection, recording: &str) -> rusqlite::Result<Vec<RecordingFit>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, COALESCE(ti.recording_mbid, t.embedded_recording_mbid) = ?1 \
         FROM local_tracks t \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         WHERE t.id IN (SELECT local_track_id FROM local_track_external_identities \
                        WHERE recording_mbid = ?1 \
                        UNION SELECT id FROM local_tracks WHERE embedded_recording_mbid = ?1) \
         AND t.availability = 'indexed' ORDER BY t.id",
    )?;
    stmt.query_map(params![recording], |row| {
        Ok(RecordingFit {
            id: row.get(0)?,
            unchallenged: row.get(1)?,
        })
    })?
    .collect()
}

struct NameFit {
    id: String,
    seconds: Option<f64>,
    accepted: Option<String>,
    tag: Option<String>,
}

/// The playable tracks the row's track, artist and (when it has one) album
/// name fit: at most two, which is enough to tell one from several.
fn name_fits(conn: &Connection, keys: &PlayKeys) -> rusqlite::Result<Vec<NameFit>> {
    let title = fold_text(&keys.track_name);
    let artist = fold_text(&keys.artist_name);
    if title.is_empty() || artist.is_empty() {
        return Ok(Vec::new());
    }
    let album = text(keys.album_name.as_deref()).map(fold_text);
    let mut stmt = conn.prepare_cached(
        "SELECT t.id, t.duration_seconds, ti.recording_mbid, t.embedded_recording_mbid \
         FROM local_tracks t \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         WHERE t.title_folded = ?1 AND t.artist_name_folded = ?2 \
         AND (?3 IS NULL OR t.album_title_folded = ?3) AND t.availability = 'indexed' \
         ORDER BY t.id LIMIT 2",
    )?;
    stmt.query_map(params![title, artist, album], |row| {
        Ok(NameFit {
            id: row.get(0)?,
            seconds: row.get(1)?,
            accepted: row.get(2)?,
            tag: row.get(3)?,
        })
    })?
    .collect()
}

/// The one playable track the row's names fit, checked against the row's
/// length and recording.
fn by_names(
    conn: &Connection,
    keys: &PlayKeys,
    recording: Option<&str>,
) -> rusqlite::Result<Matched> {
    let length = keys.duration_ms.filter(|length| *length > 0);
    if text(keys.album_name.as_deref()).is_none() && length.is_none() {
        return Ok(Matched::Left(NAMES_WITHOUT_ALBUM_OR_LENGTH));
    }
    let fits = name_fits(conn, keys)?;
    let fit = match fits.as_slice() {
        [] => return Ok(Matched::Left(NO_MATCH)),
        [one] => one,
        _ => return Ok(Matched::Left(SEVERAL_MATCHES)),
    };
    // Another recording only when the track has one and neither its
    // accepted recording nor its tag is the row's.
    if let Some(wanted) = recording {
        let mut held = [&fit.accepted, &fit.tag]
            .into_iter()
            .filter_map(|id| text(id.as_deref()))
            .peekable();
        if held.peek().is_some() && !held.any(|id| id.eq_ignore_ascii_case(wanted)) {
            return Ok(Matched::Left(RECORDING_DISAGREES));
        }
    }
    if let Some(length) = length {
        let agrees = fit
            .seconds
            .is_some_and(|seconds| (seconds * 1000.0 - length as f64).abs() <= LENGTH_SLACK_MS);
        if !agrees {
            return Ok(Matched::Left(LENGTH_DISAGREES));
        }
    }
    Ok(Matched::Track {
        id: fit.id.clone(),
        by: BY_NAMES,
    })
}

/// The one playable track `keys` is a play of, or why there is none.
pub fn match_play(conn: &Connection, keys: &PlayKeys) -> rusqlite::Result<Matched> {
    let recording = text(keys.recording_mbid.as_deref()).map(str::to_ascii_lowercase);
    if keys.is_local() {
        return by_names(conn, keys, recording.as_deref());
    }
    let Some(recording) = recording else {
        return Ok(Matched::Left(REMOTE_WITHOUT_RECORDING));
    };
    // The recording alone decides: one track holds it, and that track's
    // accepted recording does not say otherwise.
    Ok(match by_recording(conn, &recording)?.as_slice() {
        [one] if one.unchallenged => Matched::Track {
            id: one.id.clone(),
            by: BY_RECORDING,
        },
        [] | [_] => Matched::Left(NO_MATCH),
        _ => Matched::Left(SEVERAL_MATCHES),
    })
}

/// A row no repair has filed: it names no track, album or artist.
const UNFILED: &str =
    "local_track_id IS NULL AND local_album_id IS NULL AND local_artist_id IS NULL";

/// How many play rows name no track, album or artist.
pub fn unfiled(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM library_play_history WHERE {UNFILED}"),
        [],
        |row| row.get(0),
    )
}

/// Queue a filing job when a row is unfiled: after scan run `scan_run`,
/// or at a server start. A start's idempotency key names the catalog
/// revision and the number of rows still unfiled, so a restart with
/// nothing new to match queues nothing. False when nothing was queued.
pub fn queue(
    tx: &Transaction<'_>,
    job_id: &str,
    scan_run: Option<&str>,
    now: f64,
) -> rusqlite::Result<bool> {
    let unfiled = unfiled(tx)?;
    if unfiled == 0 {
        return Ok(false);
    }
    // A job stopped by a restart goes on by itself; the rows it has filed
    // since have changed the count the start key is made of.
    let at_work: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_operation_jobs j \
         JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.kind = 'repair' AND j.state IN ('queued','running','paused') \
         AND s.purpose = ?1)",
        params![Purpose::PlayHistoryFiling],
        |row| row.get(0),
    )?;
    if scan_run.is_none() && at_work {
        return Ok(false);
    }
    let catalog_revision: i64 = tx
        .query_row(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let key = match scan_run {
        Some(run) => format!("play-history-filing:v1:scan:{run}"),
        None => format!("play-history-filing:v1:start:{catalog_revision}:{unfiled}"),
    };
    let queued = tx.execute(
        "INSERT INTO library_operation_jobs (id, kind, state, input_catalog_revision, \
         expected_work_count, idempotency_key, created_at, updated_at) \
         VALUES (?1, 'repair', 'queued', ?2, ?3, ?4, ?5, ?5) \
         ON CONFLICT (idempotency_key) DO NOTHING",
        params![job_id, catalog_revision, unfiled, key, now],
    )?;
    if queued == 0 {
        return Ok(false);
    }
    let purpose = Purpose::PlayHistoryFiling;
    let scope = serde_json::to_string(&Scope::default())
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?;
    tx.execute(
        "INSERT INTO library_repair_snapshots (job_id, purpose, scope_json, \
         target_matcher_version, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![job_id, purpose, scope, PLAYS_VERSION, now],
    )?;
    tx.execute(
        "INSERT INTO library_repair_materialization (job_id, pinned_catalog_revision, \
         eligibility_version, purpose, sealed, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)",
        params![job_id, catalog_revision, PLAYS_VERSION, purpose, now],
    )?;
    Ok(true)
}

/// File one row under `track_id`, its album and its first credited artist
/// (the album artist when the track credits none), all three or none.
/// False when the row has been filed since, or the track is gone.
fn file(tx: &Transaction<'_>, play_rowid: i64, track_id: &str) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE library_play_history SET local_track_id = t.id, \
         local_album_id = t.local_album_id, \
         local_artist_id = COALESCE((SELECT x.local_artist_id FROM local_track_artists x \
                                     WHERE x.local_track_id = t.id ORDER BY x.position LIMIT 1), \
                                    a.album_artist_id) \
         FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
         WHERE t.id = ?2 AND library_play_history.rowid = ?1 \
         AND library_play_history.local_track_id IS NULL \
         AND library_play_history.local_album_id IS NULL \
         AND library_play_history.local_artist_id IS NULL",
        params![play_rowid, track_id],
    )?;
    Ok(changed == 1)
}

/// One transaction of a filing job: look at the next `limit` unfiled rows
/// after the job's cursor, file the ones that fit one track, and move the
/// cursor, the job's counts and its summary with them. True when no row
/// is left to look at. A cursor or summary that does not decode fails
/// with [`Unreadable`], logged, and nothing written.
///
/// The cursor is saved with the rows it passed, so a job that stops
/// between two calls goes on where it was: no row is looked at twice, and
/// the summary's numbers are the rows changed and left.
pub fn step(tx: &Transaction<'_>, job_id: &str, limit: usize, now: f64) -> rusqlite::Result<bool> {
    // No cursor yet: no row was looked at.
    let cursor: Option<String> = tx.query_row(
        "SELECT staging_cursor FROM library_repair_materialization WHERE job_id = ?1",
        params![job_id],
        |row| row.get(0),
    )?;
    let cursor: i64 = match cursor.map(|raw| raw.parse::<i64>()).transpose() {
        Ok(cursor) => cursor.unwrap_or(0),
        Err(error) => return Err(Unreadable::new(job_id, "play filing cursor", error).into()),
    };
    // No summary yet: the first batch starts it.
    let summary: Option<String> = tx.query_row(
        "SELECT result_json FROM library_repair_snapshots WHERE job_id = ?1",
        params![job_id],
        |row| row.get(0),
    )?;
    let mut summary: Summary = match summary.map(|raw| serde_json::from_str(&raw)).transpose() {
        Ok(Some(summary)) => summary,
        Ok(None) => Summary::new(Purpose::PlayHistoryFiling, PLAYS_VERSION.to_owned()),
        Err(error) => return Err(Unreadable::new(job_id, "summary", error).into()),
    };

    let rows = {
        let mut stmt = tx.prepare_cached(&format!(
            "SELECT rowid, CAST(track_name AS TEXT), CAST(artist_name AS TEXT), \
             CAST(album_name AS TEXT), CAST(recording_mbid AS TEXT), \
             CAST(release_group_mbid AS TEXT), CAST(duration_ms AS INTEGER), \
             CAST(source AS TEXT) FROM library_play_history \
             WHERE {UNFILED} AND rowid > ?1 ORDER BY rowid LIMIT ?2"
        ))?;
        stmt.query_map(params![cursor, limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                PlayKeys {
                    track_name: row.get(1)?,
                    artist_name: row.get(2)?,
                    album_name: row.get(3)?,
                    recording_mbid: row.get(4)?,
                    release_group_mbid: row.get(5)?,
                    duration_ms: row.get(6)?,
                    source: row.get(7)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };

    let mut counts: BTreeMap<&'static str, i64> = BTreeMap::new();
    let mut filed = 0;
    for (rowid, keys) in &rows {
        let reason = match match_play(tx, keys)? {
            Matched::Track { id, by } if file(tx, *rowid, &id)? => {
                filed += 1;
                by
            }
            Matched::Track { .. } => NO_MATCH,
            Matched::Left(reason) => reason,
        };
        *counts.entry(reason).or_default() += 1;
    }
    let looked_at = rows.len() as i64;
    let left = looked_at - filed;
    for (reason, count) in counts {
        *summary
            .counts_by_reason
            .entry(reason.to_owned())
            .or_default() += count;
    }
    *summary
        .counts_by_finding
        .entry("filed".to_owned())
        .or_default() += filed;
    *summary
        .counts_by_finding
        .entry("left".to_owned())
        .or_default() += left;
    summary.total_identities += looked_at;

    if let Some((last, _)) = rows.last() {
        tx.execute(
            "UPDATE library_repair_materialization SET staging_cursor = ?2, updated_at = ?3 \
             WHERE job_id = ?1",
            params![job_id, last.to_string(), now],
        )?;
    }
    let summary = serde_json::to_string(&summary)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?;
    tx.execute(
        "UPDATE library_repair_snapshots SET result_json = ?2 WHERE job_id = ?1",
        params![job_id, summary],
    )?;
    tx.execute(
        "UPDATE library_operation_jobs SET completed_count = completed_count + ?2, \
         succeeded_count = succeeded_count + ?3, skipped_count = skipped_count + ?4, \
         updated_at = ?5, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 WHERE id = ?1",
        params![job_id, looked_at, filed, left, now],
    )?;
    Ok(rows.len() < limit)
}
