//! YouTube links a user found or set for albums and tracks. v3 keeps them
//! in the same tables v2 did, so rows travel verbatim. The album id is a
//! MusicBrainz release group id, not a library id, so nothing waits on
//! the library carry.

use super::{Source, TableSection, Target, col};

/// Album links.
pub const ALBUM_LINKS: TableSection = TableSection {
    name: "youtube_link",
    source: Source::Table {
        table: "youtube_links",
        filter: "",
        requires: &[],
    },
    target: Target::Table("youtube_links"),
    columns: &[
        col("album_id"),
        col("video_id"),
        col("album_name"),
        col("artist_name"),
        col("embed_url"),
        col("cover_url"),
        col("created_at"),
        col("is_manual"),
        col("track_count"),
    ],
    key: &["album_id"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "",
};

/// Track links.
pub const TRACK_LINKS: TableSection = TableSection {
    name: "youtube_track_link",
    source: Source::Table {
        table: "youtube_track_links",
        filter: "",
        requires: &[],
    },
    target: Target::Table("youtube_track_links"),
    columns: &[
        col("album_id"),
        col("track_number"),
        col("disc_number"),
        col("album_name"),
        col("track_name"),
        col("video_id"),
        col("artist_name"),
        col("embed_url"),
        col("created_at"),
    ],
    key: &["album_id", "disc_number", "track_number"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "",
};
