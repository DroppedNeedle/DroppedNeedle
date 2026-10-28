//! Small per-user preferences.

use super::library::live_user;
use super::requests::{millis, now_millis};
use super::{Column, Source, TableSection, Target, col};

/// Scrobble targets, now-playing visibility and the personal-mix switch.
pub const LISTENING_PREFS: TableSection = TableSection {
    name: "listening_prefs",
    source: Source::Table {
        table: "user_listening_prefs",
        filter: "",
        requires: &[],
    },
    target: Target::Table("user_listening_prefs"),
    columns: &[
        col("user_id"),
        col("scrobble_to_lastfm"),
        col("scrobble_to_listenbrainz"),
        col("navidrome_handles_external_scrobbles"),
        col("primary_music_source"),
        col("now_playing_visibility"),
        col("updated_at"),
        col("auto_request_personal_mix"),
    ],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "preferences of deleted users; nothing to do, those accounts were deleted in v2",
};

/// Admin decisions on personal-mix auto requests.
pub const PERSONAL_MIX_APPROVALS: TableSection = TableSection {
    name: "personal_mix_approval",
    source: Source::Table {
        table: "personal_mix_approvals",
        filter: "",
        requires: &[],
    },
    target: Target::Table("personal_mix_approvals"),
    columns: &[
        col("user_id"),
        // A state outside the four asks again.
        Column::new("state").from_sql(
            "CASE WHEN s.state IN ('pending', 'approved', 'rejected', 'revoked') \
             THEN s.state ELSE 'pending' END",
        ),
        Column::new("requested_at").from_sql_of(
            "requested_at",
            concat!(
                "COALESCE(",
                millis!("requested_at"),
                ", ",
                now_millis!(),
                ")"
            ),
        ),
        Column::new("reviewed_by_id").from_sql(live_user!("reviewed_by_id")),
        col("reviewed_by_name"),
        Column::new("reviewed_at").from_sql_of("reviewed_at", millis!("reviewed_at")),
    ],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "approvals of deleted users; nothing to do, those accounts were deleted in v2",
};

/// Which home and page sections each user switched on.
pub const SECTION_PREFS: TableSection = TableSection {
    name: "section_pref",
    source: Source::Table {
        table: "user_section_prefs",
        filter: "",
        requires: &[],
    },
    target: Target::Table("user_section_prefs"),
    columns: &[
        col("user_id"),
        col("page"),
        col("section_key"),
        col("enabled"),
        col("updated_at"),
    ],
    key: &["user_id", "page", "section_key"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "preferences of deleted users; nothing to do, those accounts were deleted in v2",
};

/// Navidrome music folders each user picked.
pub const NAVIDROME_FOLDER_PREFS: TableSection = TableSection {
    name: "navidrome_folder_pref",
    source: Source::Table {
        table: "user_navidrome_folder_preferences",
        filter: "",
        requires: &[],
    },
    target: Target::Table("user_navidrome_folder_preferences"),
    columns: &[
        col("user_id"),
        col("mode"),
        col("selected_ids_json"),
        col("server_identity"),
        col("updated_at"),
    ],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "preferences of deleted users; nothing to do, those accounts were deleted in v2",
};

/// When each user last looked at the new-release feed.
pub const NEW_RELEASE_SEEN: TableSection = TableSection {
    name: "new_release_seen",
    source: Source::Table {
        table: "user_new_release_seen",
        filter: "",
        requires: &[],
    },
    target: Target::Table("user_new_release_seen"),
    columns: &[col("user_id"), col("seen_at")],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "markers of deleted users; nothing to do, those accounts were deleted in v2",
};
