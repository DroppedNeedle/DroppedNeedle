//! The request ledger, wanted watches and per-user quotas.
//!
//! Only finished requests carry: one still waiting for approval or a
//! download would need v3 to pick the work back up, so it stays behind
//! and the user asks again. Requesters and dismissals follow their request.
//! Request counts per user come from the requester rows, so carrying them
//! keeps quota usage where it was.

use super::{Source, TableSection, Target, col};

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
        col("year"),
        col("cover_url"),
        col("requested_at"),
        col("completed_at"),
        col("status"),
        col("monitor_artist"),
        col("auto_download_artist"),
        col("user_id"),
        col("requested_by_name"),
        col("reviewed_by_id"),
        col("reviewed_by_name"),
        col("reviewed_at"),
        col("download_task_id"),
        col("release_mbid"),
        col("request_kind"),
        col("track_title"),
        col("duration_seconds"),
        col("track_release_group_mbid"),
        col("dispatch_authorized"),
        col("generation"),
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
        col("requested_at"),
        col("requested_by_name"),
    ],
    key: &["user_id", "musicbrainz_id_lower"],
    unique: &[],
    parents: &[],
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
        col("dismissed_at"),
    ],
    key: &["user_id", "musicbrainz_id_lower"],
    unique: &[],
    parents: &[],
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
        col("year"),
        col("cover_url"),
        col("kind"),
        col("state"),
        col("created_at"),
        col("first_release_date"),
        col("check_count"),
        col("quiet_streak"),
        col("last_checked_at"),
        col("next_check_at"),
        col("last_outcome"),
        col("new_candidate_count"),
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
        col("first_seen_at"),
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
        col("request_quota_count"),
        col("request_quota_days"),
        col("storage_quota_gb"),
    ],
    key: &["user_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "quotas of deleted users; nothing to do, those accounts were deleted in v2",
};
