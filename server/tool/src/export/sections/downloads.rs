//! The download ledger and the bad-source blocklist.
//!
//! Only finished downloads carry: v3 starts with an empty queue. Task ids
//! stay as they were, so the library carry can link files back to the
//! download that landed them, and per-user storage usage keeps counting
//! the completed ones.

use super::requests::{flag, millis, now_millis, real, set_or, whole, whole_min, whole_or};
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
        Column::new("track_number").from_sql_of("track_number", whole!("track_number")),
        Column::new("disc_number").from_sql_of("disc_number", whole!("disc_number")),
        Column::new("year").from_sql_of("year", whole!("year")),
        Column::new("track_count").from_sql_of("track_count", whole!("track_count")),
        Column::new("track_duration_seconds")
            .from_sql_of("track_duration_seconds", real!("track_duration_seconds")),
        col("download_client"),
        col("source"),
        col("origin"),
        col("source_username"),
        col("source_directory"),
        col("search_query"),
        col("search_job_id"),
        Column::new("candidate_index").from_sql_of("candidate_index", whole!("candidate_index")),
        // A live status would put a finished download back to work, so anything
        // else reads as failed.
        Column::new("status").from_sql_of(
            "status",
            set_or!(
                "status",
                "failed",
                "completed" => "completed",
                "partial" => "partial",
                "failed" => "failed",
                "cancelled" => "cancelled",
            ),
        ),
        Column::new("preflight_score").from_sql_of("preflight_score", real!("preflight_score")),
        Column::new("progress_percent")
            .from_sql_of("progress_percent", whole_or!("progress_percent", 0)),
        Column::new("total_size_bytes").from_sql_of("total_size_bytes", whole!("total_size_bytes")),
        Column::new("downloaded_bytes")
            .from_sql_of("downloaded_bytes", whole_or!("downloaded_bytes", 0)),
        Column::new("files_total").from_sql_of("files_total", whole_or!("files_total", 0)),
        Column::new("files_completed")
            .from_sql_of("files_completed", whole_or!("files_completed", 0)),
        Column::new("files_failed").from_sql_of("files_failed", whole_or!("files_failed", 0)),
        col("quality_format"),
        Column::new("quality_bitrate").from_sql_of("quality_bitrate", whole!("quality_bitrate")),
        Column::new("quality_sample_rate")
            .from_sql_of("quality_sample_rate", whole!("quality_sample_rate")),
        Column::new("quality_bit_depth")
            .from_sql_of("quality_bit_depth", whole!("quality_bit_depth")),
        Column::new("advertised_queue_depth")
            .from_sql_of("advertised_queue_depth", whole!("advertised_queue_depth")),
        Column::new("queue_position_start")
            .from_sql_of("queue_position_start", whole!("queue_position_start")),
        Column::new("queue_position_end")
            .from_sql_of("queue_position_end", whole!("queue_position_end")),
        Column::new("remote_queued").from_sql_of("remote_queued", flag!("remote_queued")),
        Column::new("preferred_quality_fallback_at").from_sql_of(
            "preferred_quality_fallback_at",
            millis!("preferred_quality_fallback_at"),
        ),
        col("quality_pool_key"),
        Column::new("attempt_number").from_sql_of("attempt_number", whole_or!("attempt_number", 0)),
        Column::new("attempt_total").from_sql_of("attempt_total", whole_or!("attempt_total", 0)),
        Column::new("has_next_source").from_sql_of("has_next_source", flag!("has_next_source")),
        col("quality_snapshot_json"),
        Column::new("quality_preference_step")
            .from_sql_of("quality_preference_step", whole!("quality_preference_step")),
        col("quality_certainty"),
        col("quality_provenance"),
        Column::new("manual_quality_override")
            .from_sql_of("manual_quality_override", flag!("manual_quality_override")),
        col("staging_path"),
        col("final_path"),
        col("error_message"),
        Column::new("retry_count").from_sql_of("retry_count", whole_or!("retry_count", 0)),
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
        // The v2 check already holds these values; an unknown source reads as
        // soulseek, the v3 default for a source.
        Column::new("source").from_sql_of(
            "source",
            "CASE WHEN s.source IN ('soulseek', 'usenet') OR s.source LIKE 'plugin:%' \
             THEN s.source ELSE 'soulseek' END",
        ),
        Column::new("candidate_index")
            .from_sql_of("candidate_index", whole_min!("candidate_index", 0)),
        col("job_name"),
        col("handle_json"),
        col("remote_storage"),
        col("mount_root"),
        col("workspace_path"),
        col("materialized_paths_json"),
        col("materialized_fingerprints_json"),
        col("publisher_bundle_ids_json"),
        Column::new("legacy_reconciled")
            .from_sql_of("legacy_reconciled", flag!("legacy_reconciled")),
        // Cleanup retries the other states, so an unknown one reads as preserved,
        // which cleanup never sweeps.
        Column::new("state").from_sql_of(
            "state",
            set_or!(
                "state",
                "preserved",
                "complete" => "complete",
                "preserved" => "preserved",
            ),
        ),
        // A discard aborts a cancelled transfer, so an unknown value reads as undecided.
        Column::new("disposition").from_sql_of(
            "disposition",
            set_or!(
                "disposition",
                "undecided",
                "undecided" => "undecided",
                "discard" => "discard",
                "preserve" => "preserve",
            ),
        ),
        Column::new("cleanup_failures")
            .from_sql_of("cleanup_failures", whole_min!("cleanup_failures", 0)),
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
        Column::new("row_revision").from_sql_of("row_revision", whole_min!("row_revision", 1)),
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
        // Every reason blocks the source for the same 7 days, so an unknown one
        // only labels the row; it reads as a failed download.
        Column::new("reason").from_sql_of(
            "reason",
            set_or!(
                "reason",
                "download_failed",
                "verify_failed" => "verify_failed",
                "corrupt" => "corrupt",
                "fingerprint_mismatch" => "fingerprint_mismatch",
                "duration_mismatch" => "duration_mismatch",
                "download_failed" => "download_failed",
                "manual" => "manual",
            ),
        ),
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
