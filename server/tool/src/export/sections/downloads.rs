//! The download ledger and the bad-source blocklist.
//!
//! Only finished downloads carry: v3 starts with an empty queue. Task ids
//! stay as they were, so the library carry can link files back to the
//! download that landed them, and per-user storage usage keeps counting
//! the completed ones.

use super::requests::{millis, now_millis};
use super::{Column, Parent, Source, TableSection, Target, col};

/// Finished downloads.
pub const DOWNLOAD_TASKS: TableSection = TableSection {
    name: "download_task",
    source: Source::Table {
        table: "download_tasks",
        filter: "s.status IN ('completed', 'partial', 'failed', 'cancelled')",
        requires: &[],
    },
    target: Target::Table("download_tasks"),
    columns: &[
        col("id"),
        col("user_id"),
        col("request_history_mbid"),
        col("download_type"),
        col("release_group_mbid"),
        col("release_mbid"),
        col("release_track_mbid"),
        col("recording_mbid"),
        col("artist_mbid"),
        col("artist_name"),
        col("album_title"),
        col("track_title"),
        col("track_number"),
        col("disc_number"),
        col("year"),
        col("track_count"),
        col("track_duration_seconds"),
        col("download_client"),
        col("source"),
        col("origin"),
        col("source_username"),
        col("source_directory"),
        col("search_query"),
        col("search_job_id"),
        col("candidate_index"),
        col("status"),
        col("preflight_score"),
        col("progress_percent"),
        col("total_size_bytes"),
        col("downloaded_bytes"),
        col("files_total"),
        col("files_completed"),
        col("files_failed"),
        col("quality_format"),
        col("quality_bitrate"),
        col("quality_sample_rate"),
        col("quality_bit_depth"),
        col("advertised_queue_depth"),
        col("queue_position_start"),
        col("queue_position_end"),
        col("remote_queued"),
        Column::new("preferred_quality_fallback_at").from_sql_of(
            "preferred_quality_fallback_at",
            millis!("preferred_quality_fallback_at"),
        ),
        col("quality_pool_key"),
        col("attempt_number"),
        col("attempt_total"),
        col("has_next_source"),
        col("quality_snapshot_json"),
        col("quality_preference_step"),
        col("quality_certainty"),
        col("quality_provenance"),
        col("manual_quality_override"),
        col("staging_path"),
        col("final_path"),
        col("error_message"),
        col("retry_count"),
        Column::new("last_polled_at").from_sql_of("last_polled_at", millis!("last_polled_at")),
        Column::new("created_at").from_sql_of(
            "created_at",
            concat!("COALESCE(", millis!("created_at"), ", ", now_millis!(), ")"),
        ),
        Column::new("started_at").from_sql_of("started_at", millis!("started_at")),
        Column::new("completed_at").from_sql_of("completed_at", millis!("completed_at")),
        Column::new("cancelled_at").from_sql_of("cancelled_at", millis!("cancelled_at")),
        Column::new("updated_at").from_sql_of(
            "updated_at",
            concat!("COALESCE(", millis!("updated_at"), ", ", now_millis!(), ")"),
        ),
        Column::new("wrong_product_verdict_at").from_sql_of(
            "wrong_product_verdict_at",
            millis!("wrong_product_verdict_at"),
        ),
        col("wrong_product_detail"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "downloads that had not finished; let them finish in v2 before you export, or \
                  request them again in v3",
};

/// Settled attempts of carried downloads. An attempt still cleaning up
/// (or flagged for attention) points at a v2 download folder that v3's
/// cleanup would act on, so only finished and preserved ones carry.
pub const DOWNLOAD_ATTEMPTS: TableSection = TableSection {
    name: "download_attempt",
    source: Source::Table {
        table: "download_attempts",
        filter: "s.state IN ('complete', 'preserved') AND s.task_id IN (SELECT t.id \
                 FROM v2.download_tasks t WHERE t.status IN \
                 ('completed', 'partial', 'failed', 'cancelled') \
                 AND t.user_id IN (SELECT id FROM v2.auth_users))",
        requires: &["download_tasks"],
    },
    target: Target::Table("download_attempts"),
    columns: &[
        col("id"),
        col("task_id"),
        col("source"),
        col("candidate_index"),
        col("job_name"),
        col("handle_json"),
        col("remote_storage"),
        col("mount_root"),
        col("workspace_path"),
        col("materialized_paths_json"),
        col("materialized_fingerprints_json"),
        col("publisher_bundle_ids_json"),
        col("legacy_reconciled"),
        col("state"),
        col("disposition"),
        col("cleanup_failures"),
        Column::new("next_retry_at").from_sql_of(
            "next_retry_at",
            concat!("COALESCE(", millis!("next_retry_at"), ", 0)"),
        ),
        col("error_code"),
        Column::new("created_at").from_sql_of(
            "created_at",
            concat!("COALESCE(", millis!("created_at"), ", ", now_millis!(), ")"),
        ),
        Column::new("updated_at").from_sql_of(
            "updated_at",
            concat!("COALESCE(", millis!("updated_at"), ", ", now_millis!(), ")"),
        ),
        Column::new("completed_at").from_sql_of("completed_at", millis!("completed_at")),
        col("row_revision"),
    ],
    key: &["id"],
    unique: &[],
    parents: &[Parent::required("download_tasks", &[("task_id", "id")])],
    user_column: None,
    left_behind: "attempts of unfinished downloads, or still cleaning up their folder; let v2 \
                  finish them before you export, or request them again in v3",
};

/// Sources v2 learned not to download from again. v3 numbers the rows
/// itself; the source, identity and release group name each one.
pub const QUARANTINE: TableSection = TableSection {
    name: "quarantine",
    source: Source::Table {
        table: "download_quarantine",
        filter: "",
        requires: &[],
    },
    target: Target::Table("download_quarantine"),
    columns: &[
        col("source"),
        col("identity"),
        col("release_group_mbid"),
        col("reason"),
        Column::new("quarantined_at").from_sql_of(
            "quarantined_at",
            concat!(
                "COALESCE(",
                millis!("quarantined_at"),
                ", ",
                now_millis!(),
                ")"
            ),
        ),
    ],
    key: &["source", "identity", "release_group_mbid"],
    unique: &[],
    parents: &[],
    user_column: None,
    left_behind: "",
};
