//! The request ledger, wanted watches and per-user quotas.
//!
//! Every request carries. One v2 had not finished lands as one v3 still has
//! to do: waiting for approval, or approved with no download yet, which v3
//! starts with a fresh search (the import's request step settles the ones
//! the library already holds). Nothing of v2's download progress comes with
//! it. Requesters and dismissals follow their request.
//! Request counts per user come from the requester rows, so carrying them
//! keeps quota usage where it was.
//!
//! v3 keeps these times as integer milliseconds where v2 had ISO text or
//! REAL seconds, and has no dormant watch: one carries over watching and
//! due.

use super::library::live_user;
use super::{Column, Parent, Source, TableSection, Target, ValueRule, col};

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

/// A v2 `cover_url` moved onto v3's routes, the same rule as migration
/// 0144. A root-relative path's `/api/` segment is found behind any base
/// path v2's web UI put in front, which is dropped (v3 adds its own). The
/// cover and media server image paths move to v3's routes for the same
/// ids, any other v2 `/api/` path becomes no cover, and a full address or
/// a v3 path stays.
pub(crate) const V3_COVER: &str = "CASE WHEN substr(s.cover_url, 1, 1) = '/' \
     AND substr(s.cover_url, 1, 2) != '//' AND instr(s.cover_url, '/api/') > 0 \
     AND (instr(s.cover_url, '?') = 0 OR instr(s.cover_url, '/api/') < instr(s.cover_url, '?')) \
     AND substr(s.cover_url, instr(s.cover_url, '/api/'), 8) != '/api/v3/' \
     THEN CASE \
     WHEN substr(s.cover_url, instr(s.cover_url, '/api/'), 15) = '/api/v1/covers/' \
     THEN '/api/v3/covers/' || substr(s.cover_url, instr(s.cover_url, '/api/') + 15) \
     WHEN substr(s.cover_url, instr(s.cover_url, '/api/'), 12) = '/api/covers/' \
     THEN '/api/v3/covers/' || substr(s.cover_url, instr(s.cover_url, '/api/') + 12) \
     WHEN substr(s.cover_url, instr(s.cover_url, '/api/'), 23) = '/api/v1/jellyfin/image/' \
     THEN '/api/v3/remotes/jellyfin/images/' || substr(s.cover_url, instr(s.cover_url, '/api/') + 23) \
     WHEN substr(s.cover_url, instr(s.cover_url, '/api/'), 24) = '/api/v1/navidrome/cover/' \
     THEN '/api/v3/remotes/navidrome/images/' || substr(s.cover_url, instr(s.cover_url, '/api/') + 24) \
     WHEN substr(s.cover_url, instr(s.cover_url, '/api/'), 19) = '/api/v1/plex/thumb/' \
     THEN '/api/v3/remotes/plex/images/' || substr(s.cover_url, instr(s.cover_url, '/api/') + 19) \
     END ELSE s.cover_url END";

/// Requester and dismissal rows hang off their request.
const ON_REQUEST: &[Parent] = &[Parent::required(
    "request_history",
    &[("musicbrainz_id_lower", "musicbrainz_id_lower")],
)];

// v2's request states, as v2's `request_history.py` and `request_service.py`
// list them (`_TERMINAL_STATUSES`, `_TASK_ACTIVE_STATUSES`,
// `_ACTIVE_REQUEST_STATUSES`, `_CANCELLING_STATUS`): finished, still to do,
// or being cancelled.

/// v2's finished states, which v3 keeps as they are.
macro_rules! finished {
    () => {
        "('imported', 'incomplete', 'failed', 'cancelled', 'rejected')"
    };
}

/// v2's states of a request still to do: waiting for approval, about to
/// start, queued or downloading.
macro_rules! unfinished {
    () => {
        "('awaiting_approval', 'pending', 'queued', 'downloading')"
    };
}

/// A request v2 had approved and was getting: v3 starts it again by
/// itself. One with no owner, or whose owner was deleted, cannot, as its
/// download needs one, so it waits for an admin, who becomes its owner on
/// approval.
macro_rules! starts_again {
    () => {
        "s.status IN ('pending', 'queued', 'downloading') \
         AND s.user_id IN (SELECT id FROM v2.auth_users)"
    };
}

/// Every request, in the state v3 reads.
pub const REQUESTS: TableSection = TableSection {
    name: "request",
    source: Source::Table {
        table: "request_history",
        filter: "",
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
        Column::new("cover_url").from_sql_of("cover_url", V3_COVER),
        Column::new("requested_at").from_sql(concat!(
            "COALESCE(",
            millis!("requested_at"),
            ", ",
            millis!("completed_at"),
            ", ",
            now_millis!(),
            ")"
        )),
        // A request being cancelled is cancelled as the import finishes it.
        Column::new("completed_at").from_sql(concat!(
            "CASE WHEN s.status IN ",
            unfinished!(),
            " THEN NULL WHEN s.status = 'cancelling' THEN COALESCE(",
            millis!("completed_at"),
            ", ",
            now_millis!(),
            ") ELSE ",
            millis!("completed_at"),
            " END"
        )),
        // A request under way starts again with no download, so v3 searches
        // afresh. A state v2 never wrote would read as live work, so it
        // reads as failed.
        Column::new("status").from_sql_of(
            "status",
            concat!(
                "CASE WHEN ",
                starts_again!(),
                " THEN 'pending' WHEN s.status IN ",
                unfinished!(),
                " THEN 'awaiting_approval' WHEN s.status = 'cancelling' THEN 'cancelled' \
                 WHEN s.status IN ",
                finished!(),
                " THEN s.status ELSE 'failed' END"
            ),
        ),
        Column::new("monitor_artist").from_sql_of("monitor_artist", flag!("monitor_artist")),
        Column::new("auto_download_artist")
            .from_sql_of("auto_download_artist", flag!("auto_download_artist")),
        // v2 keeps a deleted owner's id on their requests. An unfinished one
        // lands with no owner, waiting for an admin; the user filter reads
        // this value, so the row and the asks others joined it with carry.
        Column::new("user_id").from_sql_of(
            "user_id",
            concat!(
                "CASE WHEN s.status IN ",
                unfinished!(),
                " THEN ",
                live_user!("user_id"),
                " ELSE s.user_id END"
            ),
        ),
        col("requested_by_name"),
        Column::new("reviewed_by_id").from_sql(live_user!("reviewed_by_id")),
        col("reviewed_by_name"),
        Column::new("reviewed_at").from_sql(millis!("reviewed_at")),
        // Only finished downloads carry, so only a finished request keeps one.
        Column::new("download_task_id").from_sql_of(
            "download_task_id",
            concat!(
                "CASE WHEN s.status IN ",
                finished!(),
                " THEN s.download_task_id END"
            ),
        ),
        col("release_mbid"),
        Column::new("request_kind").from_sql_of(
            "request_kind",
            set_or!("request_kind", "album", "album" => "album", "track" => "track"),
        ),
        col("track_title"),
        Column::new("duration_seconds").from_sql(whole!("duration_seconds")),
        col("track_release_group_mbid"),
        // v3's startup recovery starts an approved request with no download.
        // One waiting for approval is not approved yet, and a cancelled one
        // is not any more, as v3's own cancel leaves it.
        Column::new("dispatch_authorized").from_sql_of(
            "dispatch_authorized",
            concat!(
                "CASE WHEN ",
                starts_again!(),
                " THEN 1 WHEN s.status IN ",
                unfinished!(),
                " OR s.status = 'cancelling' THEN 0 ELSE ",
                flag!("dispatch_authorized"),
                " END"
            ),
        ),
        Column::new("generation").from_sql_of("generation", whole_or!("generation", 1)),
    ],
    key: &["musicbrainz_id_lower"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "finished requests whose owner was deleted in v2",
};

/// Who asked for each request.
pub const REQUEST_REQUESTERS: TableSection = TableSection {
    name: "request_requester",
    source: Source::Table {
        table: "request_history_requesters",
        filter: "",
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
    left_behind: "requesters that are deleted users; nothing to do, those accounts were deleted \
                  in v2",
};

/// Requests a user cleared from their list.
pub const REQUEST_DISMISSALS: TableSection = TableSection {
    name: "request_dismissal",
    source: Source::Table {
        table: "request_history_dismissals",
        filter: "",
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
    left_behind: "dismissals of deleted users; nothing to do, those accounts were deleted in v2",
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
        Column::new("cover_url").from_sql_of("cover_url", V3_COVER),
        Column::new("kind").ruled(ValueRule::OneOf {
            kept: &["missing", "partial"],
            prefix: None,
            renamed: &[],
            default: "missing",
        }),
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
        // v2's spellings of what a check found, as v3 reads them; anything
        // else says nothing.
        Column::new("last_outcome").from_sql_of(
            "last_outcome",
            "CASE s.last_outcome WHEN 'no_results' THEN 'quiet' \
             WHEN 'seen_only' THEN 'seen' WHEN 'new_manual' THEN 'found' \
             WHEN 'auto_dispatched' THEN 'dispatched' WHEN 'satisfied' THEN 'in_library' \
             WHEN 'error' THEN 'error' WHEN 'imported' THEN 'imported' ELSE NULL END",
        ),
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
