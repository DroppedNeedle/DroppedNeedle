//! What the follow poller already knows about each artist's releases.
//!
//! Without the known set, v3's first poll would treat every current
//! release as the baseline and miss anything that came out between v2's
//! last poll and the upgrade.

use super::{Source, TableSection, Target, col};

/// Releases each followed artist already had.
pub const KNOWN_RELEASES: TableSection = TableSection {
    name: "known_release",
    source: Source::Table {
        table: "artist_known_releases",
        filter: "",
        requires: &[],
    },
    target: Target::Table("artist_known_releases"),
    columns: &[
        col("artist_mbid_lower"),
        col("rg_mbid_lower"),
        col("auto_policy_revision"),
    ],
    key: &["artist_mbid_lower", "rg_mbid_lower"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "",
};

/// The new-release feed users see.
pub const NEW_RELEASE_FEED: TableSection = TableSection {
    name: "new_release",
    source: Source::Table {
        table: "new_release_feed",
        filter: "",
        requires: &[],
    },
    target: Target::Table("new_release_feed"),
    columns: &[
        col("release_group_mbid_lower"),
        col("release_group_mbid"),
        col("artist_mbid_lower"),
        col("artist_name"),
        col("title"),
        col("primary_type"),
        col("secondary_types"),
        col("first_release_date"),
        col("discovered_at"),
    ],
    key: &["release_group_mbid_lower"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "",
};

/// Releases each user hid from the discover queue. v3's queue reads the
/// same table, so a hidden release stays hidden after the upgrade.
pub const IGNORED_RELEASES: TableSection = TableSection {
    name: "ignored_release",
    source: Source::Table {
        table: "ignored_releases",
        filter: "",
        requires: &[],
    },
    target: Target::Table("ignored_releases"),
    columns: &[
        col("user_id"),
        col("release_group_mbid_lower"),
        col("release_group_mbid"),
        col("artist_mbid"),
        col("release_name"),
        col("artist_name"),
        col("ignored_at"),
    ],
    key: &["user_id", "release_group_mbid_lower"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "hidden releases of deleted users; nothing to do, those accounts were \
                  deleted in v2",
};
