//! Downloads v2 held back for review, with the held files themselves.
//!
//! v2 copies a file it will not import on its own to `<cache>/held` and
//! records why in `held_imports`; a curator later imports or discards it.
//! Only rows still waiting on that decision travel: their files go in the
//! bundle, land in v3's own `<cache>/held` folder and join v3's held list.
//! Rows already decided have no file left to act on and stay behind.

use super::requests::{real, whole, whole_or};
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
        Column::new("track_number").from_sql_of("track_number", whole!("track_number")),
        Column::new("disc_number").from_sql_of("disc_number", whole!("disc_number")),
        col("track_title"),
        col("artist_name"),
        col("artist_mbid"),
        col("album_title"),
        Column::new("year").from_sql_of("year", whole!("year")),
        col("held_path"),
        col("original_filename"),
        col("file_format"),
        Column::new("duration_seconds").from_sql_of("duration_seconds", real!("duration_seconds")),
        Column::new("expected_duration_seconds").from_sql_of(
            "expected_duration_seconds",
            real!("expected_duration_seconds"),
        ),
        // The importer maps the reason and sets the state and origin itself.
        col("reason"),
        col("reason_detail"),
        col("evidence_title"),
        col("evidence_artist"),
        Column::new("evidence_score").from_sql_of("evidence_score", real!("evidence_score")),
        col("source"),
        col("source_task_id"),
        col("origin"),
        col("naming_template"),
        Column::new("management_retry_count").from_sql_of(
            "management_retry_count",
            whole_or!("management_retry_count", 0),
        ),
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
