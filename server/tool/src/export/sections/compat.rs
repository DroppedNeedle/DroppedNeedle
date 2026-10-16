//! Saved play queues and bookmarks of Subsonic and Jellyfin clients.
//!
//! v2 keyed queue items and bookmarks by its library track id; v3 keeps
//! the same id in `file_id`. The v2 id is written as is and recorded as a
//! pending link until the library carry confirms it.

use super::{Column, LinkKind, LinkMode, Parent, Source, TableSection, Target, col};

/// One saved queue head per user.
pub const PLAY_QUEUES: TableSection = TableSection {
    name: "compat_play_queue",
    source: Source::Table {
        table: "library_compat_play_queues",
        filter: "",
        requires: &[],
    },
    target: Target::Table("compat_play_queues"),
    columns: &[
        col("user_id"),
        col("current_index"),
        col("position_ms"),
        col("updated_at"),
        col("changed_by_client"),
    ],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "queues of deleted users; nothing to do, those accounts were deleted in v2",
};

/// The tracks of each saved queue.
pub const PLAY_QUEUE_ITEMS: TableSection = TableSection {
    name: "compat_play_queue_item",
    source: Source::Table {
        table: "library_compat_play_queue_items",
        filter: "",
        requires: &[],
    },
    target: Target::Table("compat_play_queue_items"),
    columns: &[
        col("user_id"),
        col("item_index"),
        Column::new("file_id")
            .from_v2("local_track_id")
            .link(LinkKind::Track, LinkMode::AsWritten),
    ],
    key: &["user_id", "item_index"],
    unique: &[],
    parents: &[Parent::required(
        "compat_play_queues",
        &[("user_id", "user_id")],
    )],
    user_column: Some("user_id"),
    left_behind: "queues of deleted users; nothing to do, those accounts were deleted in v2",
};

/// Resume positions.
pub const BOOKMARKS: TableSection = TableSection {
    name: "compat_bookmark",
    source: Source::Table {
        table: "library_compat_bookmarks",
        filter: "",
        requires: &[],
    },
    target: Target::Table("compat_bookmarks"),
    columns: &[
        col("user_id"),
        Column::new("file_id")
            .from_v2("local_track_id")
            .link(LinkKind::Track, LinkMode::AsWritten),
        col("position_ms"),
        col("comment"),
        col("created_at"),
        col("changed_at"),
    ],
    key: &["user_id", "file_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "bookmarks of deleted users; nothing to do, those accounts were deleted in v2",
};
