//! Play history.
//!
//! The library ids on a history row have foreign keys in v3, so they stay
//! empty until the v3 catalog holds them; the names and MBIDs on the row
//! keep stats and Wrapped working meanwhile. v3 keeps a play's time as
//! integer milliseconds where v2 had ISO text; one that does not read as a
//! time becomes 0, and the play still counts.

use super::requests::{millis, whole};
use super::{Column, LinkKind, LinkMode, Source, TableSection, Target, col};

/// Listens.
pub const PLAY_HISTORY: TableSection = TableSection {
    name: "play_history",
    source: Source::Table {
        table: "library_play_history",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_play_history"),
    columns: &[
        col("id"),
        col("user_id"),
        col("local_track_id").link(LinkKind::Track, LinkMode::UntilResolved),
        col("local_album_id").link(LinkKind::Album, LinkMode::UntilResolved),
        col("local_artist_id").link(LinkKind::Artist, LinkMode::UntilResolved),
        col("track_name"),
        col("artist_name"),
        col("album_name"),
        col("recording_mbid"),
        col("release_group_mbid"),
        Column::new("duration_ms").from_sql(whole!("duration_ms")),
        col("source"),
        Column::new("played_at").from_sql(concat!("COALESCE(", millis!("played_at"), ", 0)")),
    ],
    key: &["id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "listens of deleted users; nothing to do, those accounts were deleted in v2",
};
