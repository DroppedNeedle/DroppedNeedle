//! Library Management state: original-file baselines with their stored
//! bytes, per-track management state, custom editions, album exclusions
//! and field overrides.
//!
//! Baselines are the one thing here nothing can rebuild: the tags and
//! place a file had before DroppedNeedle first changed it. Each travels
//! three ways at once:
//!
//! - v2's own baseline row and its blob ledger rows, verbatim;
//! - the stored bytes of every blob a baseline names (the tag snapshot
//!   and any sidecar or artwork it kept), into v3's content-addressed
//!   blob table, hash checked on the way out of v2;
//! - a v3 baseline translated from the tag snapshot (the importer's
//!   baseline step), which is what v3's "restore original" reads.
//!
//! Management state keeps `baseline_id`, so v3 can tell a track whose v2
//! baseline did not make it across and refuse to manage it rather than
//! record the already-changed file as its original.

use super::library::live_user;
use super::{Column, FileSet, Parent, Source, TableSection, Target, col};

/// Blobs a carried baseline names: its tag snapshot, plus every blob its
/// ancillary snapshot lists (sidecars and external art).
const BASELINE_BLOB: &str = "s.sha256 IN (SELECT b.semantic_snapshot_blob_sha256 \
     FROM v2.library_management_baselines b) \
     OR s.sha256 IN (SELECT json_extract(a.value, '$.blob_sha256') \
     FROM v2.library_management_baselines b, json_each(CASE \
     WHEN json_valid(b.ancillary_snapshot_json) THEN b.ancillary_snapshot_json \
     ELSE '[]' END) a)";

const ON_ALBUM: Parent = Parent::required("local_albums", &[("local_album_id", "id")]);
const ON_TRACK: Parent = Parent::required("local_tracks", &[("local_track_id", "id")]);

/// v2's ledger rows for the blobs baselines name.
pub const BLOBS: TableSection = TableSection {
    name: "management_blob",
    source: Source::Table {
        table: "library_management_blobs",
        filter: BASELINE_BLOB,
        requires: &["library_management_baselines"],
    },
    target: Target::Table("library_management_blobs"),
    columns: &[
        col("sha256"),
        col("kind"),
        col("byte_length"),
        col("relative_path"),
        col("media_metadata_json"),
        col("created_at"),
        col("row_revision"),
    ],
    key: &["sha256"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "stored files that only the undo of single v2 changes used; nothing to do",
};

/// The stored bytes of those blobs, read from v2's blob folder and
/// checked against their hash.
pub const BLOB_BYTES: TableSection = TableSection {
    name: "management_blob_bytes",
    source: Source::Files(FileSet::ManagementBlobs),
    target: Target::Table("library_publish_blobs"),
    columns: &[col("sha256"), col("bytes")],
    key: &["sha256"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "",
};

/// v2's original-file baselines, as v2 wrote them.
pub const BASELINES: TableSection = TableSection {
    name: "management_baseline",
    source: Source::Table {
        table: "library_management_baselines",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_management_baselines"),
    columns: &[
        col("id"),
        col("local_track_id"),
        col("original_root_id"),
        col("original_relative_path"),
        col("format"),
        col("adapter_version"),
        col("semantic_snapshot_blob_sha256"),
        col("image_snapshot_json"),
        col("ancillary_snapshot_json"),
        col("file_mtime_ns"),
        col("file_mode"),
        col("stat_revision"),
        col("tag_revision"),
        col("identity_revision"),
        col("created_at"),
        col("restore_status"),
        col("last_verified_at"),
        col("catalog_document_json"),
        col("catalog_document_hash"),
        col("row_revision"),
    ],
    key: &["id"],
    unique: &[&["local_track_id"]],
    parents: &[
        ON_TRACK,
        Parent::required(
            "library_management_blobs",
            &[("semantic_snapshot_blob_sha256", "sha256")],
        ),
    ],
    user_column: None,
    left_behind: "",
};

/// What v2 last did to each managed track. The job reference stays
/// behind with v2's job history.
pub const TRACK_STATE: TableSection = TableSection {
    name: "track_management_state",
    source: Source::Table {
        table: "library_track_management_state",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_track_management_state"),
    columns: &[
        col("local_track_id"),
        col("baseline_id"),
        col("applied_profile_id"),
        col("applied_profile_revision"),
        col("applied_projection_hash"),
        col("applied_naming_script_revision"),
        col("applied_override_revision"),
        col("managed_root_id"),
        col("managed_path_revision"),
        col("last_managed_at"),
        col("last_outcome"),
        col("last_reason_code"),
        col("row_revision"),
    ],
    key: &["local_track_id"],
    unique: &[],
    parents: &[
        ON_TRACK,
        Parent::optional("library_management_baselines", &[("baseline_id", "id")]),
    ],
    user_column: None,
    left_behind: "",
};

/// Custom editions a curator sealed for an album.
pub const EDITION_MANIFESTS: TableSection = TableSection {
    name: "custom_edition",
    source: Source::Table {
        table: "library_custom_edition_manifests",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_custom_edition_manifests"),
    columns: &[
        col("id"),
        col("local_album_id"),
        col("version"),
        col("release_group_mbid"),
        col("album_title"),
        col("album_artist_name"),
        col("artist_mbid"),
        col("album_metadata_json"),
        col("source_album_revision"),
        col("source_identity_revision"),
        col("input_revision"),
        col("content_hash"),
        col("selected_candidate_key"),
        col("sealed_by_user_id"),
        col("sealed_at"),
    ],
    key: &["id"],
    unique: &[&["local_album_id", "version"]],
    parents: &[ON_ALBUM],
    user_column: Some("sealed_by_user_id"),
    left_behind: "custom editions sealed by deleted users; seal them again in v3 if you need them",
};

/// The tracks of each custom edition.
pub const EDITION_TRACKS: TableSection = TableSection {
    name: "custom_edition_track",
    source: Source::Table {
        table: "library_custom_edition_tracks",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_custom_edition_tracks"),
    columns: &[
        col("manifest_id"),
        col("ordinal"),
        col("local_track_id"),
        col("source_track_revision"),
        col("source_identity_revision"),
        col("stat_revision"),
        col("tag_revision"),
        col("title"),
        col("artist_name"),
        col("album_title"),
        col("album_artist_name"),
        col("disc_number"),
        col("track_number"),
        col("recording_mbid"),
        col("artist_mbid"),
        col("album_artist_mbid"),
        col("metadata_json"),
        col("file_format"),
        col("duration_seconds"),
    ],
    key: &["manifest_id", "ordinal"],
    unique: &[&["manifest_id", "local_track_id"]],
    parents: &[
        Parent::required("library_custom_edition_manifests", &[("manifest_id", "id")]),
        ON_TRACK,
    ],
    user_column: None,
    left_behind: "",
};

/// Which custom edition is active for an album.
pub const EDITION_ACTIVE: TableSection = TableSection {
    name: "custom_edition_active",
    source: Source::Table {
        table: "library_custom_edition_active",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_custom_edition_active"),
    columns: &[
        col("local_album_id"),
        col("manifest_id"),
        col("activated_at"),
        col("row_revision"),
    ],
    key: &["local_album_id"],
    unique: &[&["manifest_id"]],
    parents: &[
        ON_ALBUM,
        Parent::required("library_custom_edition_manifests", &[("manifest_id", "id")]),
    ],
    user_column: None,
    left_behind: "",
};

/// Albums a curator took out of Library Management.
pub const EXCLUSIONS: TableSection = TableSection {
    name: "management_exclusion",
    source: Source::Table {
        table: "library_management_exclusions",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_management_exclusions"),
    columns: &[
        col("local_album_id"),
        col("reason"),
        col("excluded_by_user_id"),
        col("excluded_at"),
        col("row_revision"),
    ],
    key: &["local_album_id"],
    unique: &[],
    parents: &[ON_ALBUM],
    user_column: Some("excluded_by_user_id"),
    left_behind: "exclusions set by deleted users; exclude those albums again in v3",
};

/// Field overrides (replace, preserve or clear one field of an album or
/// track).
pub const OVERRIDES: TableSection = TableSection {
    name: "management_override",
    source: Source::Table {
        table: "library_management_overrides",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_management_overrides"),
    columns: &[
        col("id"),
        col("subject_kind"),
        col("local_album_id"),
        col("local_track_id"),
        col("field_name"),
        col("value_json"),
        col("mode"),
        Column::new("actor_user_id").from_sql(live_user!("actor_user_id")),
        col("reason"),
        col("subject_revision"),
        col("created_at"),
        col("updated_at"),
        col("row_revision"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[
        Parent::optional("local_albums", &[("local_album_id", "id")]),
        Parent::optional("local_tracks", &[("local_track_id", "id")]),
    ],
    user_column: None,
    left_behind: "",
};
