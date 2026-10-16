//! Play history.
//!
//! The library ids on a history row have foreign keys in v3, so they stay
//! empty until the v3 catalog holds them; the names and MBIDs on the row
//! keep stats and Wrapped working meanwhile.

use super::{LinkKind, LinkMode, Source, TableSection, Target, col};

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
        col("duration_ms"),
        col("source"),
        col("played_at"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "listens of deleted users; nothing to do, those accounts were deleted in v2",
};
