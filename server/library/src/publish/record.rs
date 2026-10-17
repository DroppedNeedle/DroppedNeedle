//! The app's record of the bytes it wrote, and whether a file was changed
//! since. The catalog commit and the automatic check share this answer.
//!
//! The record is `library_track_management_state.written_sha256`: the
//! SHA-256 of a file as the app last wrote it, last found it the way it
//! wanted it, or was last handed it by a person.
//!
//! Files v2 wrote: v2 kept `managed_path_revision`, the SHA-256 of
//! `root_id NUL relative_path NUL sha256_of_the_file` in hex. A row the
//! upgrade carried over (no `written_sha256`, no job, an outcome v2 wrote
//! under that formula) is a record too, checked by computing the formula
//! again from the bytes just read.
//!
//! Verified 2026-10-14 against v2's code at `main` cf7278a1, not against
//! rows a running v2 wrote (no v2 database with managed rows was at
//! hand). v2's `_hash_file` (`library_management_publisher.py` 4155) and
//! both formula sites (`native_library_store.py` 26439 for an import,
//! 28142 for every other managed write and for an undo) were run on three
//! committed fixtures, one of them under a non-ASCII path, and
//! [`v2_path_revision`] gives the same values. Those two sites store the
//! outcomes `succeeded` and `undone`. A `recycled` row holds a path hash
//! instead and counts as no record. A value that cannot be reproduced,
//! for whatever reason (a writer nobody traced, a root or a path spelled
//! differently), reads as "changed since": the album steps back and
//! nothing is overwritten.

use rusqlite::{Connection, OptionalExtension as _, params};

use super::snapshots::sha256_hex;

/// True for a state row (alias `s`) that came from v2 with its record.
/// Every v3 writer of `managed_path_revision` sets the job too, and the
/// upgrade leaves v2's job behind.
pub(crate) const V2_ROW_SQL: &str = "s.written_sha256 IS NULL AND s.managed_path_revision IS NOT NULL \
     AND s.last_operation_job_id IS NULL AND s.last_outcome IN ('succeeded', 'undone')";

/// SQL condition on a `library_track_management_state` row (alias `s`):
/// the app has a record of the file's bytes.
pub fn record_sql() -> String {
    format!("(s.written_sha256 IS NOT NULL OR ({V2_ROW_SQL}))")
}

/// What the app recorded about one file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Record {
    pub written_sha256: Option<String>,
    /// v2's value, kept only for a row that came from v2.
    v2_path_revision: Option<String>,
}

impl Record {
    /// Whether the app has a record of the file's bytes at all.
    pub fn exists(&self) -> bool {
        self.written_sha256.is_some() || self.v2_path_revision.is_some()
    }

    /// Whether the file, read just now at `root_id` and `relative_path`
    /// with the hash `sha256`, is no longer what the app recorded. A file
    /// with no record was never the app's: false.
    pub fn changed_outside(&self, root_id: &str, relative_path: &str, sha256: &str) -> bool {
        match (&self.written_sha256, &self.v2_path_revision) {
            (Some(written), _) => written != sha256,
            (None, Some(stored)) => *stored != v2_path_revision(root_id, relative_path, sha256),
            (None, None) => false,
        }
    }

    /// The value `written_sha256` keeps when a write must leave this
    /// record as it was. A v2 record becomes v2's stored value, a hash of
    /// path and bytes that equals no file's hash, so the file still reads
    /// as changed once the row stops looking like v2's.
    fn kept(&self) -> Option<String> {
        self.written_sha256
            .clone()
            .or_else(|| self.v2_path_revision.clone())
    }
}

/// v2's `managed_path_revision` for a file (see the module comment).
pub fn v2_path_revision(root_id: &str, relative_path: &str, file_sha256: &str) -> String {
    sha256_hex(format!("{root_id}\u{0}{relative_path}\u{0}{file_sha256}").as_bytes())
}

/// The app's record of one track's file.
pub fn record(conn: &Connection, track_id: &str) -> rusqlite::Result<Record> {
    let row: Option<(Option<String>, Option<String>)> = conn
        .prepare_cached(&format!(
            "SELECT s.written_sha256, CASE WHEN {V2_ROW_SQL} THEN s.managed_path_revision END \
             FROM library_track_management_state s WHERE s.local_track_id = ?1"
        ))?
        .query_row(params![track_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    Ok(
        row.map_or_else(Record::default, |(written_sha256, v2_path_revision)| {
            Record {
                written_sha256,
                v2_path_revision,
            }
        }),
    )
}

/// Forget the app's record of a track's file, so the file counts as one
/// the app never wrote. Clears `written_sha256`, and `managed_path_revision`
/// where no job wrote it: every v3 writer of that column sets the job, so
/// such a value is v2's. Writes only when there is a record; returns
/// whether it wrote.
pub(crate) fn forget(conn: &Connection, track_id: &str) -> rusqlite::Result<bool> {
    let written = conn
        .prepare_cached(&format!(
            "UPDATE library_track_management_state AS s SET written_sha256 = NULL, \
             managed_path_revision = CASE WHEN s.last_operation_job_id IS NULL THEN NULL \
             ELSE s.managed_path_revision END, row_revision = s.row_revision + 1 \
             WHERE s.local_track_id = ?1 AND {}",
            record_sql()
        ))?
        .execute(params![track_id])?;
    Ok(written > 0)
}

/// Whether another track row of the track's album, one whose file went
/// missing, has a record: a file the app wrote went away, and one it never
/// wrote may have taken its place. An excluded row does not count, nor a
/// row a replacement retired (its record is cleared when it is retired).
fn stands_in(conn: &Connection, track_id: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT 1 FROM local_tracks t \
             JOIN local_tracks o ON o.local_album_id = t.local_album_id AND o.id <> t.id \
             AND o.availability = 'missing' \
             JOIN library_track_management_state s ON s.local_track_id = o.id \
             WHERE t.id = ?1 AND {} LIMIT 1",
            record_sql()
        ))?
        .query_row(params![track_id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Whether the file of an existing track row, read at `root_id` and
/// `relative_path` with the hash `sha256`, was changed since the app wrote
/// it. True when it has a record its bytes do not match, or when it has
/// none and another row of its album whose file is gone has one (a file
/// retagged and renamed together comes back as a new row with no record,
/// beside the old one). Reads only the database.
pub fn changed_since_written(
    conn: &Connection,
    track_id: &str,
    root_id: &str,
    relative_path: &str,
    sha256: &str,
) -> rusqlite::Result<bool> {
    let record = record(conn, track_id)?;
    if record.exists() {
        return Ok(record.changed_outside(root_id, relative_path, sha256));
    }
    stands_in(conn, track_id)
}

/// What a write records as the app's for an existing track: the bytes it
/// placed, or, when the file it replaced was changed since the app wrote
/// it, the record as it was (none stays none). `source` is where the write
/// read the file and `source_sha256` the hash it verified there; a write
/// that cannot say keeps the record. Reads only the database.
pub fn written_after(
    conn: &Connection,
    track_id: &str,
    source: Option<(&str, &str)>,
    source_sha256: Option<&str>,
    placed_sha256: &str,
) -> rusqlite::Result<Option<String>> {
    let record = record(conn, track_id)?;
    let changed = if record.exists() {
        match (source, source_sha256) {
            (Some((root_id, relative_path)), Some(sha256)) => {
                record.changed_outside(root_id, relative_path, sha256)
            }
            _ => true,
        }
    } else {
        stands_in(conn, track_id)?
    };
    Ok(if changed {
        record.kept()
    } else {
        Some(placed_sha256.to_owned())
    })
}
