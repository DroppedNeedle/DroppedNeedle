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

use crate::library::publish::snapshots::sha256_hex;

/// Why the app stepped back from an album.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// A file the app wrote was changed outside it.
    FilesChanged,
    /// A scan followed the album across a retag.
    RetagFollowed,
    /// A person undid a run of the album's profile.
    RunUndone,
}

impl Cause {
    pub const ALL: [Self; 3] = [Self::FilesChanged, Self::RetagFollowed, Self::RunUndone];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilesChanged => "files_changed",
            Self::RetagFollowed => "retag_followed",
            Self::RunUndone => "run_undone",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cause| cause.as_str() == value)
    }
}

/// True for a state row (alias `s`) that came from v2 with its record.
/// Every v3 writer of `managed_path_revision` sets the job too, and the
/// upgrade leaves v2's job behind.
const V2_ROW_SQL: &str = "s.written_sha256 IS NULL AND s.managed_path_revision IS NOT NULL \
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
