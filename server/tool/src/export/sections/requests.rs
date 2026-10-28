//! The request ledger, wanted watches and per-user quotas.
//!
//! Only finished requests carry: one still waiting for approval or a
//! download would need v3 to pick the work back up, so it stays behind
//! and the user asks again. Requesters and dismissals follow their request.
//! Request counts per user come from the requester rows, so carrying them
//! keeps quota usage where it was.
//!
//! v3 keeps these times as integer milliseconds where v2 had ISO text or
//! REAL seconds, and has no dormant watch: one carries over watching and
//! due.

use super::library::live_user;
use super::{Column, Parent, Source, TableSection, Target, col};

/// A v2 time column (ISO text, numeric text or REAL seconds) as integer
/// milliseconds; NULL when it does not read as a time.
macro_rules! millis {
    ($column:literal) => {
        concat!(
            "CASE WHEN typeof(s.",
            $column,
            ") = 'integer' THEN s.",
            $column,
            " * 1000 ",
            "WHEN typeof(s.",
            $column,
            ") = 'real' THEN CAST(round(s.",
            $column,
            " * 1000) AS INTEGER) ",
            "WHEN s.",
            $column,
            " GLOB '[0-9]*' AND s.",
            $column,
            " NOT GLOB '*[^0-9.]*' ",
            "THEN CAST(round(CAST(s.",
            $column,
            " AS REAL) * 1000) AS INTEGER) ",
            "ELSE CAST(round((julianday(s.",
            $column,
            ") - 2440587.5) * 86400000.0) AS INTEGER) END"
        )
    };
}

pub(crate) use millis;

/// Now, as integer milliseconds.
macro_rules! now_millis {
    () => {
        "CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)"
    };
}
pub(crate) use now_millis;

/// A v2 whole-number column, NULL when it holds something else.
macro_rules! whole {
    ($column:literal) => {
        concat!(
            "CASE WHEN typeof(s.",
            $column,
            ") = 'integer' THEN s.",
            $column,
            " ",
            "WHEN typeof(s.",
            $column,
            ") = 'real' THEN CAST(round(s.",
            $column,
            ") AS INTEGER) ",
            "WHEN s.",
            $column,
            " GLOB '[0-9]*' AND s.",
            $column,
            " NOT GLOB '*[^0-9]*' ",
            "THEN CAST(s.",
            $column,
            " AS INTEGER) END"
        )
    };
}

pub(crate) use whole;

/// A v2 whole-number column, `$default` when it does not read as one.
macro_rules! whole_or {
    ($column:literal, $default:literal) => {
        concat!("COALESCE(", whole!($column), ", ", $default, ")")
    };
}

pub(crate) use whole_or;

/// A v2 whole-number column kept at `$min` or above, so a v3 CHECK holds.
macro_rules! whole_min {
    ($column:literal, $min:literal) => {
        concat!("MAX(", $min, ", ", whole_or!($column, $min), ")")
    };
}

pub(crate) use whole_min;

/// A v2 flag column as 0 or 1: 1 when it reads as a non-zero number.
macro_rules! flag {
    ($column:literal) => {
        concat!("CASE WHEN ", whole!($column), " != 0 THEN 1 ELSE 0 END")
    };
}

pub(crate) use flag;

/// A v2 REAL column: the value when it is a number, NULL otherwise.
macro_rules! real {
    ($column:literal) => {
        concat!(
            "CASE WHEN typeof(s.",
            $column,
            ") IN ('integer', 'real') THEN s.",
            $column,
            " END"
        )
    };
}

pub(crate) use real;

/// A v2 text column with a fixed set of values: each `$from` maps to its v3
/// `$to`; anything else, NULL included, takes `$default`.
macro_rules! set_or {
    ($column:literal, $default:literal, $($from:literal => $to:literal),+ $(,)?) => {
        concat!(
            "CASE s.",
            $column,
            $(" WHEN '", $from, "' THEN '", $to, "'",)+
            " ELSE '",
            $default,
            "' END"
        )
    };
}

pub(crate) use set_or;

/// Requester and dismissal rows hang off their request.
const ON_REQUEST: &[Parent] = &[Parent::required(
    "request_history",
    &[("musicbrainz_id_lower", "musicbrainz_id_lower")],
)];

/// v2's terminal request states.
const FINISHED: &str = "s.status IN ('imported', 'incomplete', 'failed', 'cancelled', 'rejected')";

/// Requester and dismissal rows of a finished request.
const OF_FINISHED: &str = "s.musicbrainz_id_lower IN (SELECT musicbrainz_id_lower \
     FROM v2.request_history WHERE status IN \
     ('imported', 'incomplete', 'failed', 'cancelled', 'rejected'))";

/// Finished requests.
pub const REQUESTS: TableSection = TableSection {
    name: "request",
    source: Source::Table {
        table: "request_history",
        filter: FINISHED,
        requires: &[],
    },
    target: Target::Table("request_history"),
    columns: &[
        col("musicbrainz_id_lower"),
        col("musicbrainz_id"),
        col("artist_name"),
        col("album_title"),
        col("artist_mbid"),
        Column::new("year").from_sql(whole!("year")),
        col("cover_url"),
        Column::new("requested_at").from_sql(concat!(
            "COALESCE(",
            millis!("requested_at"),
            ", ",
            millis!("completed_at"),
            ", ",
            now_millis!(),
            ")"
        )),
        Column::new("completed_at").from_sql(millis!("completed_at")),
        // A live status would send a finished request back to work, so anything
        // else reads as failed.
        Column::new("status").from_sql_of(
            "status",
            set_or!(
                "status",
                "failed",
                "imported" => "imported",
                "incomplete" => "incomplete",
                "failed" => "failed",
                "cancelled" => "cancelled",
                "rejected" => "rejected",
            ),
        ),
        Column::new("monitor_artist").from_sql_of("monitor_artist", flag!("monitor_artist")),
        Column::new("auto_download_artist")
            .from_sql_of("auto_download_artist", flag!("auto_download_artist")),
        col("user_id"),
        col("requested_by_name"),
        Column::new("reviewed_by_id").from_sql(live_user!("reviewed_by_id")),
        col("reviewed_by_name"),
        Column::new("reviewed_at").from_sql(millis!("reviewed_at")),
        col("download_task_id"),
        col("release_mbid"),
        Column::new("request_kind").from_sql_of(
            "request_kind",
            set_or!("request_kind", "album", "album" => "album", "track" => "track"),
        ),
        col("track_title"),
        Column::new("duration_seconds").from_sql(whole!("duration_seconds")),
        col("track_release_group_mbid"),
        Column::new("dispatch_authorized")
            .from_sql_of("dispatch_authorized", flag!("dispatch_authorized")),
        Column::new("generation").from_sql_of("generation", whole_or!("generation", 1)),
    ],
    key: &["musicbrainz_id_lower"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "requests still waiting or downloading; request them again in v3",
};

/// Who asked for each finished request.
pub const REQUEST_REQUESTERS: TableSection = TableSection {
    name: "request_requester",
    source: Source::Table {
        table: "request_history_requesters",
        filter: OF_FINISHED,
        requires: &[],
    },
    target: Target::Table("request_history_requesters"),
    columns: &[
        col("user_id"),
        col("musicbrainz_id_lower"),
        Column::new("requested_at").from_sql(concat!("COALESCE(", millis!("requested_at"), ", 0)")),
        col("requested_by_name"),
    ],
    key: &["user_id", "musicbrainz_id_lower"],
    unique: &[],
    parents: ON_REQUEST,
    user_column: Some("user_id"),
    left_behind: "requesters of unfinished requests; request again in v3",
};

/// Requests a user cleared from their list.
pub const REQUEST_DISMISSALS: TableSection = TableSection {
    name: "request_dismissal",
    source: Source::Table {
        table: "request_history_dismissals",
        filter: OF_FINISHED,
        requires: &[],
    },
    target: Target::Table("request_history_dismissals"),
    columns: &[
        col("user_id"),
        col("musicbrainz_id_lower"),
        Column::new("dismissed_at").from_sql(concat!(
            "COALESCE(",
            millis!("dismissed_at"),
            ", ",
            now_millis!(),
            ")"
        )),
    ],
    key: &["user_id", "musicbrainz_id_lower"],
    unique: &[],
    parents: ON_REQUEST,
    user_column: Some("user_id"),
    left_behind: "dismissals of unfinished requests; nothing to do",
};

/// Wanted watches, every state: live ones keep checking in v3.
pub const WANTED_WATCHES: TableSection = TableSection {
    name: "wanted_watch",
    source: Source::Table {
        table: "wanted_watches",
        filter: "",
        requires: &[],
    },
    target: Target::Table("wanted_watches"),
    columns: &[
        col("release_group_mbid_lower"),
        col("release_group_mbid"),
        col("user_id"),
        col("artist_name"),
        col("album_title"),
        col("artist_mbid"),
        Column::new("year").from_sql(whole!("year")),
        col("cover_url"),
        Column::new("kind").from_sql_of(
            "kind",
            set_or!("kind", "missing", "missing" => "missing", "partial" => "partial"),
        ),
        // Only watching watches are checked, so an unknown state reads as stopped.
        Column::new("state").from_sql_of(
            "state",
            set_or!(
                "state",
                "stopped",
                "watching" => "watching",
                "dormant" => "watching",
                "stopped" => "stopped",
                "fulfilled" => "fulfilled",
            ),
        ),
        Column::new("created_at").from_sql(concat!(
            "COALESCE(",
            millis!("created_at"),
            ", ",
            now_millis!(),
            ")"
        )),
        col("first_release_date"),
        Column::new("check_count").from_sql_of("check_count", whole_or!("check_count", 0)),
        Column::new("quiet_streak").from_sql_of("quiet_streak", whole_or!("quiet_streak", 0)),
        Column::new("last_checked_at").from_sql(millis!("last_checked_at")),
        Column::new("next_check_at").from_sql(concat!(
            "CASE WHEN s.state = 'dormant' THEN ",
            now_millis!(),
            " ELSE COALESCE(",
            millis!("next_check_at"),
            ", ",
            now_millis!(),
            ") END"
        )),
        col("last_outcome"),
        Column::new("new_candidate_count")
            .from_sql_of("new_candidate_count", whole_or!("new_candidate_count", 0)),
    ],
    key: &["release_group_mbid_lower"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "wanted watches of deleted users; nothing to do, those accounts were deleted in \
                  v2",
};

/// Candidates a watch already saw, so they are not announced twice.
pub const WANTED_SEEN_CANDIDATES: TableSection = TableSection {
    name: "wanted_seen_candidate",
    source: Source::Table {
        table: "wanted_seen_candidates",
        filter: "s.release_group_mbid_lower IN (SELECT release_group_mbid_lower \
                 FROM v2.wanted_watches WHERE user_id IN (SELECT id FROM v2.auth_users))",
        requires: &[],
    },
    target: Target::Table("wanted_seen_candidates"),
    columns: &[
        col("release_group_mbid_lower"),
        col("source"),
        col("identity"),
        Column::new("first_seen_at").from_sql_of(
            "first_seen_at",
            concat!(
                "COALESCE(",
                millis!("first_seen_at"),
                ", ",
                now_millis!(),
                ")"
            ),
        ),
    ],
    key: &["release_group_mbid_lower", "source", "identity"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "seen candidates of watches that stayed behind; nothing to do",
};

/// Per-user quota overrides.
pub const QUOTAS: TableSection = TableSection {
    name: "quota",
    source: Source::Table {
        table: "user_quotas",
        filter: "",
        requires: &[],
    },
    target: Target::Table("user_quotas"),
    columns: &[
        col("user_id"),
        Column::new("request_quota_count")
            .from_sql_of("request_quota_count", whole!("request_quota_count")),
        Column::new("request_quota_days")
            .from_sql_of("request_quota_days", whole!("request_quota_days")),
        Column::new("storage_quota_gb").from_sql_of("storage_quota_gb", whole!("storage_quota_gb")),
    ],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "quotas of deleted users; nothing to do, those accounts were deleted in v2",
};
