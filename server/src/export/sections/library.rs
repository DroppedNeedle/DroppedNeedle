//! The library catalog and the curator's identity decisions.
//!
//! v2 and v3 keep the catalog in the same tables with the same columns,
//! so rows travel verbatim and keep their ids. That is what keeps every
//! Subsonic and Jellyfin id stable (both derive from these ids), and what
//! lets favorites, history, playlists and queues find their tracks again.
//! v3's first scan then matches the carried rows by root and path instead
//! of minting new ids.
//!
//! Two things change on the way:
//!
//! - album grouping keys are rewritten in v3's form (see
//!   [`crate::export::library_keys`]), so a file v3 re-reads stays in its
//!   album;
//! - tracks v2 grouped on its own are marked `legacy_import`, which tells
//!   v3's scan to keep them in their v2 album while the file still names
//!   it. Tracks a curator placed (`manual`, locked) stay locked.
//!
//! Identity decisions keep their `decision_source`, so a manual match
//! stays protected from automatic passes. References to v2's
//! identification attempts stay behind (that history is not carried), and
//! a reference to a user v2 no longer has is emptied, as v2's own
//! `ON DELETE SET NULL` would have done.

use super::{Column, Parent, Source, TableSection, Target, col};

/// The value of a user column, or NULL when v2 no longer has that user.
macro_rules! live_user {
    ($column:literal) => {
        concat!(
            "CASE WHEN s.\"",
            $column,
            "\" IN (SELECT id FROM v2.auth_users) THEN s.\"",
            $column,
            "\" END"
        )
    };
}
pub(crate) use live_user;

/// A plain v2 table with no extra filter.
const fn table(name: &'static str) -> Source {
    Source::Table {
        table: name,
        filter: "",
        requires: &[],
    }
}

const ON_ARTIST: Parent = Parent::required("local_artists", &[("local_artist_id", "id")]);
const ON_ALBUM: Parent = Parent::required("local_albums", &[("local_album_id", "id")]);
const ON_TRACK: Parent = Parent::required("local_tracks", &[("local_track_id", "id")]);

/// Artists.
pub const ARTISTS: TableSection = TableSection {
    name: "library_artist",
    source: table("local_artists"),
    target: Target::Table("local_artists"),
    columns: &[
        col("id"),
        col("display_name"),
        col("sort_name"),
        col("folded_name"),
        col("normalized_name"),
        col("kind"),
        col("retired_into_artist_id"),
        col("created_at"),
        col("updated_at"),
        col("row_revision"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "",
};

/// Albums. The grouping key is rewritten after the copy.
pub const ALBUMS: TableSection = TableSection {
    name: "library_album",
    source: table("local_albums"),
    target: Target::Table("local_albums"),
    columns: &[
        col("id"),
        col("root_id"),
        col("grouping_key"),
        col("title"),
        col("title_folded"),
        col("album_artist_name"),
        col("album_artist_name_folded"),
        col("tag_album_title"),
        col("tag_album_artist_name"),
        col("album_artist_id"),
        col("album_artist_sort_name"),
        col("year"),
        col("original_release_date"),
        col("primary_genre"),
        col("is_compilation"),
        col("grouping_source"),
        col("grouping_locked"),
        col("retired_into_album_id"),
        col("created_at"),
        col("updated_at"),
        col("row_revision"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[Parent::required(
        "local_artists",
        &[("album_artist_id", "id")],
    )],
    user_column: None,
    left_behind: "",
};

/// Album artist credits.
pub const ALBUM_ARTISTS: TableSection = TableSection {
    name: "library_album_artist",
    source: table("local_album_artists"),
    target: Target::Table("local_album_artists"),
    columns: &[
        col("local_album_id"),
        col("position"),
        col("local_artist_id"),
        col("role"),
        col("credited_name"),
        col("join_phrase"),
        col("row_revision"),
    ],
    key: &["local_album_id", "position"],
    unique: &[],
    parents: &[ON_ALBUM, ON_ARTIST],
    user_column: None,
    left_behind: "",
};

/// Tracks, keyed by id and placed by root and relative path. Tracks v2
/// grouped on its own are marked `legacy_import`.
pub const TRACKS: TableSection = TableSection {
    name: "library_track",
    source: table("local_tracks"),
    target: Target::Table("local_tracks"),
    columns: &[
        col("id"),
        col("local_album_id"),
        col("root_id"),
        col("file_path"),
        col("relative_path"),
        col("path_hash"),
        col("file_size_bytes"),
        col("file_mtime_ns"),
        col("stat_revision"),
        col("stat_revision_kind"),
        col("tag_revision"),
        col("tags_read_at"),
        col("metadata_incomplete"),
        col("title"),
        col("title_folded"),
        col("artist_name"),
        col("artist_name_folded"),
        col("album_title"),
        col("album_title_folded"),
        col("album_artist_name"),
        col("album_artist_name_folded"),
        col("tag_album_title"),
        col("tag_album_artist_name"),
        col("disc_number"),
        col("track_number"),
        col("year"),
        col("genre"),
        col("genre_folded"),
        col("release_type"),
        col("title_sort"),
        col("artist_sort"),
        col("album_sort"),
        col("album_artist_sort"),
        col("disc_subtitle"),
        col("is_compilation"),
        col("embedded_release_group_mbid"),
        col("embedded_release_mbid"),
        col("embedded_recording_mbid"),
        col("embedded_release_track_mbid"),
        col("embedded_artist_mbid"),
        col("embedded_album_artist_mbid"),
        col("duration_seconds"),
        col("file_format"),
        col("bit_rate"),
        col("sample_rate"),
        col("bit_depth"),
        col("channels"),
        col("replaygain_track_gain"),
        col("replaygain_album_gain"),
        col("replaygain_track_peak"),
        col("replaygain_album_peak"),
        col("availability"),
        col("missing_since"),
        col("excluded_at"),
        col("ingest_source"),
        col("download_task_id"),
        col("source_path"),
        col("imported_at"),
        Column::new("membership_source").from_sql(
            "CASE s.membership_source WHEN 'automatic' THEN 'legacy_import' \
             ELSE s.membership_source END",
        ),
        col("membership_locked"),
        col("desired_policy_revision"),
        col("applied_policy_revision"),
        col("applied_policy"),
        col("manual_excluded"),
        col("row_revision"),
        col("title_provenance"),
        col("album_title_provenance"),
        col("album_artist_provenance"),
    ],
    key: &["id"],
    unique: &[&["root_id", "relative_path"]],
    parents: &[ON_ALBUM],
    user_column: None,
    left_behind: "",
};

/// Track artist credits.
pub const TRACK_ARTISTS: TableSection = TableSection {
    name: "library_track_artist",
    source: table("local_track_artists"),
    target: Target::Table("local_track_artists"),
    columns: &[
        col("local_track_id"),
        col("position"),
        col("local_artist_id"),
        col("role"),
        col("credited_name"),
        col("join_phrase"),
        col("row_revision"),
    ],
    key: &["local_track_id", "position"],
    unique: &[],
    parents: &[ON_TRACK, ON_ARTIST],
    user_column: None,
    left_behind: "",
};

/// Track genres, including ones fetched from providers.
pub const TRACK_GENRES: TableSection = TableSection {
    name: "library_track_genre",
    source: table("local_track_genres"),
    target: Target::Table("local_track_genres"),
    columns: &[
        col("local_track_id"),
        col("position"),
        col("name"),
        col("folded_name"),
        col("source"),
        col("genre_mbid"),
        col("weight"),
        col("source_document_revision"),
    ],
    key: &["local_track_id", "position"],
    unique: &[&["local_track_id", "folded_name"]],
    parents: &[ON_TRACK],
    user_column: None,
    left_behind: "",
};

/// Stand-ins v2 keeps for playlist entries whose track left the library.
pub const TOMBSTONES: TableSection = TableSection {
    name: "library_tombstone",
    source: table("library_reference_tombstones"),
    target: Target::Table("library_reference_tombstones"),
    columns: &[
        col("id"),
        col("source_kind"),
        col("source_key"),
        col("legacy_file_id"),
        col("title"),
        col("artist_name"),
        col("album_name"),
        col("source_type"),
        col("created_at"),
        col("row_revision"),
    ],
    key: &["id"],
    unique: &[&["source_kind", "source_key"]],
    parents: &[],
    user_column: None,
    left_behind: "",
};

/// Links from artists, albums and tracks to pages on other services.
pub const SOURCE_LINKS: TableSection = TableSection {
    name: "library_source_link",
    source: table("local_entity_source_links"),
    target: Target::Table("local_entity_source_links"),
    columns: &[
        col("id"),
        col("local_artist_id"),
        col("local_album_id"),
        col("local_track_id"),
        col("provider"),
        col("external_entity_type"),
        col("external_id"),
        col("canonical_url"),
        col("decision_source"),
        Column::new("selected_by_user_id").from_sql(live_user!("selected_by_user_id")),
        col("verified_at"),
        col("created_at"),
        col("updated_at"),
        col("row_revision"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[
        Parent::optional("local_artists", &[("local_artist_id", "id")]),
        Parent::optional("local_albums", &[("local_album_id", "id")]),
        Parent::optional("local_tracks", &[("local_track_id", "id")]),
    ],
    user_column: None,
    left_behind: "",
};

/// Retired artist ids that keep resolving to the artist that took over.
pub const ARTIST_ALIASES: TableSection = TableSection {
    name: "library_artist_alias",
    source: table("local_artist_aliases"),
    target: Target::Table("local_artist_aliases"),
    columns: &[
        col("alias"),
        col("local_artist_id"),
        col("kind"),
        col("created_at"),
    ],
    key: &["alias"],
    unique: &[],
    parents: &[ON_ARTIST],
    user_column: None,
    left_behind: "",
};

/// Retired album ids that keep resolving to the album that took over.
pub const ALBUM_ALIASES: TableSection = TableSection {
    name: "library_album_alias",
    source: table("local_album_aliases"),
    target: Target::Table("local_album_aliases"),
    columns: &[
        col("alias"),
        col("local_album_id"),
        col("kind"),
        col("created_at"),
    ],
    key: &["alias"],
    unique: &[],
    parents: &[ON_ALBUM],
    user_column: None,
    left_behind: "",
};

/// MusicBrainz identities of artists.
pub const ARTIST_IDENTITIES: TableSection = TableSection {
    name: "artist_identity",
    source: table("local_artist_external_identities"),
    target: Target::Table("local_artist_external_identities"),
    columns: &[
        col("local_artist_id"),
        col("provider"),
        col("provider_artist_id"),
        col("decision_source"),
        Column::new("selected_by_user_id").from_sql(live_user!("selected_by_user_id")),
        col("selected_at"),
        col("row_revision"),
        col("provider_source_mode"),
        col("provider_source_id"),
        col("provider_source_generation"),
    ],
    key: &["local_artist_id", "provider"],
    unique: &[&["provider", "provider_artist_id"]],
    parents: &[ON_ARTIST],
    user_column: None,
    left_behind: "",
};

/// MusicBrainz identities of albums (release group and release).
pub const ALBUM_IDENTITIES: TableSection = TableSection {
    name: "album_identity",
    source: table("local_album_external_identities"),
    target: Target::Table("local_album_external_identities"),
    columns: &[
        col("local_album_id"),
        col("provider"),
        col("release_group_mbid"),
        col("release_mbid"),
        col("decision_source"),
        col("matcher_version"),
        Column::new("selected_by_user_id").from_sql(live_user!("selected_by_user_id")),
        col("selected_at"),
        col("row_revision"),
        col("provider_source_mode"),
        col("provider_source_id"),
        col("provider_source_generation"),
        col("provider_base_url"),
    ],
    key: &["local_album_id", "provider"],
    unique: &[],
    parents: &[ON_ALBUM],
    user_column: None,
    left_behind: "",
};

/// MusicBrainz identities of tracks (recording and release track).
pub const TRACK_IDENTITIES: TableSection = TableSection {
    name: "track_identity",
    source: table("local_track_external_identities"),
    target: Target::Table("local_track_external_identities"),
    columns: &[
        col("local_track_id"),
        col("provider"),
        col("recording_mbid"),
        col("release_mbid"),
        col("release_track_mbid"),
        col("medium_position"),
        col("release_track_position"),
        col("decision_source"),
        col("selected_at"),
        col("row_revision"),
        col("provider_source_mode"),
        col("provider_source_id"),
        col("provider_source_generation"),
        col("provider_base_url"),
    ],
    key: &["local_track_id", "provider"],
    unique: &[],
    parents: &[ON_TRACK],
    user_column: None,
    left_behind: "",
};

/// Review states that record a curator's decision rather than a question.
const DECIDED_REVIEW: &str = "s.state IN ('keep_tagged', 'excluded', 'resolved')";

/// v2's decided identification reviews, kept as v2 wrote them.
pub const REVIEW_DECISIONS: TableSection = TableSection {
    name: "review_decision",
    source: Source::Table {
        table: "library_identification_reviews",
        filter: DECIDED_REVIEW,
        requires: &[],
    },
    target: Target::Table("library_identification_reviews"),
    columns: &[
        col("id"),
        col("local_album_id"),
        col("local_track_id"),
        col("state"),
        col("reason_code"),
        col("input_revision"),
        col("decision_revision"),
        Column::new("decided_by_user_id").from_sql(live_user!("decided_by_user_id")),
        col("created_at"),
        col("updated_at"),
        col("decided_at"),
        col("row_revision"),
        col("edition_uncertain"),
        col("ranked_edition_keys_json"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[
        Parent::optional("local_albums", &[("local_album_id", "id")]),
        Parent::optional("local_tracks", &[("local_track_id", "id")]),
    ],
    user_column: None,
    left_behind: "open review questions; v3 asks again (see the identify queue)",
};

/// Albums a curator told v2 to keep as tagged (or excluded from
/// identification), as v3's own record of that decision: a rejected
/// review, meaning "keep the album's tags, seal nothing".
pub const KEPT_TAGGED: TableSection = TableSection {
    name: "identify_decision",
    source: Source::Table {
        table: "library_identification_reviews",
        filter: "s.local_album_id IS NOT NULL AND s.state IN ('keep_tagged', 'excluded')",
        requires: &[],
    },
    target: Target::Table("library_identify_reviews"),
    columns: &[
        Column::new("id").from_sql("'v2-' || s.id"),
        col("local_album_id"),
        Column::new("reason_code")
            .from_sql("CASE s.state WHEN 'excluded' THEN 'EXCLUDED' ELSE 'KEEP_TAGGED' END"),
        Column::new("candidates_json").from_sql("'[]'"),
        Column::new("state").from_sql("'rejected'"),
        Column::new("resolved_by_user_id").from_sql(live_user!("decided_by_user_id")),
        Column::new("created_ms").from_sql("CAST(s.created_at * 1000 AS INTEGER)"),
        Column::new("updated_ms")
            .from_sql("CAST(COALESCE(s.decided_at, s.updated_at) * 1000 AS INTEGER)"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[ON_ALBUM],
    user_column: None,
    left_behind: "",
};

/// Albums v2 left waiting on a review question go back on v3's identify
/// queue at the backlog priority, so the question is asked again with
/// v3's candidates. One job per album.
pub const REQUEUED_REVIEWS: TableSection = TableSection {
    name: "identify_requeue",
    source: Source::Table {
        table: "library_identification_reviews",
        filter: "s.local_album_id IS NOT NULL \
                 AND s.state IN ('needs_review', 'edition_to_confirm')",
        requires: &[],
    },
    target: Target::Table("library_identify_jobs"),
    columns: &[
        Column::new("id").from_sql("'v2-review-' || s.id"),
        col("local_album_id"),
        Column::new("kind").from_sql("'historical'"),
        // The historical-backlog priority v3's identify queue uses.
        Column::new("priority").from_sql("40"),
        Column::new("state").from_sql("'queued'"),
        Column::new("attempts").from_sql("0"),
        Column::new("not_before_ms").from_sql("0"),
        Column::new("input_revision").from_sql("'v2-review'"),
        Column::new("idempotency_key").from_sql("s.local_album_id || ':v2-review'"),
        Column::new("created_ms").from_sql("CAST(s.created_at * 1000 AS INTEGER)"),
        Column::new("updated_ms").from_sql("CAST(s.updated_at * 1000 AS INTEGER)"),
    ],
    key: &["id"],
    unique: &[&["idempotency_key"]],
    parents: &[ON_ALBUM],
    user_column: None,
    left_behind: "",
};

/// Edition pins: the release a curator chose for an album.
pub const ALBUM_PINS: TableSection = TableSection {
    name: "album_pin",
    source: table("library_album_release_pins"),
    target: Target::Table("library_album_release_pins"),
    columns: &[
        col("local_album_id"),
        col("release_group_mbid"),
        col("release_mbid"),
        col("set_by_user_id"),
        col("set_at"),
    ],
    key: &["local_album_id"],
    unique: &[],
    parents: &[ON_ALBUM],
    user_column: None,
    left_behind: "",
};

/// The same pins as identification reads them: one release per release
/// group. When two albums of one group were pinned differently, the first
/// pin counts.
pub const IDENTIFY_PINS: TableSection = TableSection {
    name: "identify_pin",
    source: table("library_album_release_pins"),
    target: Target::Table("album_release_pins"),
    columns: &[
        col("release_group_mbid"),
        col("release_mbid"),
        col("set_by_user_id"),
        col("set_at"),
    ],
    key: &["release_group_mbid"],
    // Several albums of one group share the key; the rule makes the first
    // landing pin win instead of the insert failing on the second.
    unique: &[&["release_group_mbid"]],
    parents: &[],
    user_column: None,
    left_behind: "",
};

/// Candidate pairs of artists that may be one artist.
pub const MERGE_CANDIDATES: TableSection = TableSection {
    name: "artist_merge_candidate",
    source: table("local_artist_merge_candidates"),
    target: Target::Table("local_artist_merge_candidates"),
    columns: &[
        col("id"),
        col("left_artist_id"),
        col("right_artist_id"),
        col("reason_code"),
        col("state"),
        col("created_at"),
        col("updated_at"),
        col("row_revision"),
    ],
    key: &["id"],
    unique: &[&["left_artist_id", "right_artist_id", "reason_code"]],
    parents: &[
        Parent::required("local_artists", &[("left_artist_id", "id")]),
        Parent::required("local_artists", &[("right_artist_id", "id")]),
    ],
    user_column: None,
    left_behind: "",
};

/// Pairs of artists a curator marked as different people.
pub const ARTIST_DISMISSALS: TableSection = TableSection {
    name: "artist_dismissal",
    source: table("library_artist_reconciliation_dismissals"),
    target: Target::Table("library_artist_reconciliation_dismissals"),
    columns: &[
        col("left_artist_id"),
        col("right_artist_id"),
        col("left_artist_revision"),
        col("right_artist_revision"),
        col("dismissed_by_user_id"),
        col("reason_code"),
        col("created_at"),
        col("updated_at"),
        col("row_revision"),
    ],
    key: &["left_artist_id", "right_artist_id"],
    unique: &[],
    parents: &[
        Parent::required("local_artists", &[("left_artist_id", "id")]),
        Parent::required("local_artists", &[("right_artist_id", "id")]),
    ],
    user_column: Some("dismissed_by_user_id"),
    left_behind: "decisions of deleted users",
};
