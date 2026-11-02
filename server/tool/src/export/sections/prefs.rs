//! Small per-user preferences.

use super::library::live_user;
use super::requests::{flag, millis, now_millis, whole};
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
        Column::new("scrobble_to_lastfm").from_sql(flag!("scrobble_to_lastfm")),
        Column::new("scrobble_to_listenbrainz").from_sql(flag!("scrobble_to_listenbrainz")),
        Column::new("navidrome_handles_external_scrobbles").from_sql_of(
            "navidrome_handles_external_scrobbles",
            flag!("navidrome_handles_external_scrobbles"),
        ),
        // A value outside the allowed ones becomes the default. A visibility
        // that is not text says nothing, so the song stays hidden.
        Column::new("primary_music_source").from_sql(
            "CASE WHEN s.primary_music_source IN ('listenbrainz', 'lastfm') \
             THEN s.primary_music_source ELSE 'listenbrainz' END",
        ),
        Column::new("now_playing_visibility").from_sql_of(
            "now_playing_visibility",
            "CASE WHEN typeof(s.now_playing_visibility) <> 'text' THEN 'track_hidden' \
             WHEN s.now_playing_visibility IN ('full', 'track_hidden', 'offline') \
             THEN s.now_playing_visibility ELSE 'full' END",
        ),
        Column::new("updated_at").from_sql(concat!("COALESCE(", millis!("updated_at"), ", 0)")),
        Column::new("auto_request_personal_mix").from_sql_of(
            "auto_request_personal_mix",
            flag!("auto_request_personal_mix"),
        ),
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
    target: Target::Table("media_navidrome_folder_preferences"),
    columns: &[
        col("user_id"),
        col("mode"),
        col("selected_ids_json"),
        col("server_identity"),
        Column::new("updated_at").from_sql(concat!("COALESCE(", millis!("updated_at"), ", 0)")),
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
    target: Target::Table("catalog_new_release_seen_marks"),
    columns: &[
        col("user_id"),
        Column::new("seen_at").from_sql(concat!("COALESCE(", millis!("seen_at"), ", 0)")),
    ],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "markers of deleted users; nothing to do, those accounts were deleted in v2",
};
