//! YouTube links a user found or set for albums and tracks. v3 keeps their
//! times in integer milliseconds where v2 had ISO text, and a flag that is
//! never NULL; v2's stored track count stays behind, as v3 counts the track
//! links. The rest travels verbatim. The album id is a
//! MusicBrainz release group id, not a library id, so nothing waits on the
//! library carry.

use super::requests::{flag, millis, whole};
use super::{Column, Source, TableSection, Target, col};

/// Album links.
pub const ALBUM_LINKS: TableSection = TableSection {
    name: "youtube_link",
    source: Source::Table {
        table: "youtube_links",
        filter: "",
        requires: &[],
    },
    target: Target::Table("catalog_youtube_links"),
    columns: &[
        col("album_id"),
        col("video_id"),
        col("album_name"),
        col("artist_name"),
        col("embed_url"),
        col("cover_url"),
        Column::new("created_at").from_sql(concat!("COALESCE(", millis!("created_at"), ", 0)")),
        Column::new("is_manual").from_sql(flag!("is_manual")),
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
    target: Target::Table("catalog_youtube_track_links"),
    columns: &[
        col("album_id"),
        col("track_number"),
        col("disc_number"),
        col("album_name"),
        col("track_name"),
        col("video_id"),
        col("artist_name"),
        col("embed_url"),
        Column::new("created_at").from_sql(concat!("COALESCE(", millis!("created_at"), ", 0)")),
    ],
    key: &["album_id", "disc_number", "track_number"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "",
};
