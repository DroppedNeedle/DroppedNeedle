//! Downloads v2 held back for review, with the held files themselves.
//!
//! v2 copies a file it will not import on its own to `<cache>/held` and
//! records why in `held_imports`; a curator later imports or discards it.
//! Only rows still waiting on that decision travel: their files go in the
//! bundle and land in v3's own `<cache>/held` folder, and the row's
//! `held_path` is rewritten to the new place. Rows already decided have
//! no file left to act on and stay behind.

use super::{Column, FileSet, Source, TableSection, Target, col};

/// Held downloads still waiting on a decision.
pub const HELD_IMPORTS: TableSection = TableSection {
    name: "held_import",
    source: Source::TableWithFiles {
        table: "held_imports",
        filter: "s.status = 'held'",
        files: FileSet::HeldImports,
    },
    target: Target::HeldFiles,
    columns: &[
        col("id"),
        col("user_id"),
        col("release_group_mbid"),
        col("release_mbid"),
        col("release_track_mbid"),
        col("recording_mbid"),
        col("track_number"),
        col("disc_number"),
        col("track_title"),
        col("artist_name"),
        col("artist_mbid"),
        col("album_title"),
        col("year"),
        col("held_path"),
        col("original_filename"),
        col("file_format"),
        col("duration_seconds"),
        col("expected_duration_seconds"),
        col("reason"),
        col("reason_detail"),
        col("evidence_title"),
        col("evidence_artist"),
        col("evidence_score"),
        col("source"),
        col("source_task_id"),
        col("origin"),
        col("naming_template"),
        col("management_retry_count"),
        col("management_next_retry_at"),
        col("status"),
        col("created_at"),
        col("resolved_at"),
        col("file_cleanup_completed_at"),
        // The held file's bytes, filled from v2's held folder.
        Column::new("file").from_sql("NULL"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "held downloads already imported or discarded (nothing left to \
                  decide), or of deleted users",
};
