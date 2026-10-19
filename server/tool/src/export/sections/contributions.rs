//! Open MusicBrainz contributions: albums a curator was adding to
//! MusicBrainz when v2 stopped, and the return links MusicBrainz will
//! still call.
//!
//! v3 keeps contributions in the same tables v2 did, so rows travel
//! verbatim. Only open contributions move: a linked one already left its
//! result in the album's identity, and a cancelled or out-of-date one has
//! nothing left to finish. v2 allowed one open contribution per album, so
//! the album is the unique rule; an album that already has a contribution
//! in v3 keeps that one.
//!
//! Verification jobs do not travel. v3 queues a verification for every
//! verifying contribution when it starts, and the code of the last
//! verification that failed moves onto the contribution.

use super::library::live_user;
use super::{Column, Parent, Source, TableSection, Target, col};

/// Contribution states that are still in progress.
const OPEN: &str = "s.state NOT IN ('linked', 'cancelled', 'stale')";

/// The code of the contribution's last verification that ended without a
/// link, which v3 keeps on the contribution for its review reason.
const LAST_FAILURE: &str = "(SELECT j.last_failure_code \
     FROM v2.library_contribution_verification_jobs j WHERE j.contribution_id = s.id \
     AND j.state IN ('needs_review', 'failed', 'cancelled') \
     ORDER BY j.updated_at DESC, j.id DESC LIMIT 1)";

const ON_DRAFT: Parent =
    Parent::required("library_contribution_drafts", &[("contribution_id", "id")]);

/// Contributions still in progress.
pub const DRAFTS: TableSection = TableSection {
    name: "contribution_draft",
    source: Source::Table {
        table: "library_contribution_drafts",
        filter: OPEN,
        requires: &["library_contribution_verification_jobs"],
    },
    target: Target::Table("library_contribution_drafts"),
    columns: &[
        col("id"),
        col("local_album_id"),
        Column::new("created_by_user_id").from_sql(live_user!("created_by_user_id")),
        Column::new("updated_by_user_id").from_sql(live_user!("updated_by_user_id")),
        col("state"),
        col("album_row_revision"),
        col("input_revision"),
        col("local_snapshot_json"),
        col("resolved_draft_json"),
        col("source_selection_json"),
        col("provider_snapshot_expires_at"),
        col("duplicate_result_json"),
        col("duplicate_checked_at"),
        col("duplicate_input_revision"),
        col("result_release_mbid"),
        col("result_source"),
        col("result_received_at"),
        col("seed_snapshot_json"),
        col("seed_hash"),
        col("seeded_at"),
        col("terminal_at"),
        col("created_at"),
        col("updated_at"),
        col("row_revision"),
        Column::new("last_verification_failure").from_sql(LAST_FAILURE),
    ],
    key: &["id"],
    unique: &[&["local_album_id"]],
    parents: &[Parent::required(
        "local_albums",
        &[("local_album_id", "id")],
    )],
    user_column: None,
    left_behind: "finished MusicBrainz contributions (linked, cancelled or out of date). \
                  Nothing to do: a linked album keeps its MusicBrainz match; start a new \
                  contribution in v3 for one you still want to add",
};

/// Return links MusicBrainz calls when a curator saves the release. Only
/// unused, unexpired ones still work; v3 answers v2's callback address.
pub const CALLBACK_TOKENS: TableSection = TableSection {
    name: "contribution_callback_token",
    source: Source::Table {
        table: "library_contribution_callback_tokens",
        filter: "s.consumed_at IS NULL AND s.expires_at >= CAST(strftime('%s', 'now') AS REAL) \
                 AND s.contribution_id IN (SELECT d.id FROM v2.library_contribution_drafts d \
                 WHERE d.state NOT IN ('linked', 'cancelled', 'stale'))",
        requires: &["library_contribution_drafts"],
    },
    target: Target::Table("library_contribution_callback_tokens"),
    columns: &[
        col("token_hash"),
        col("contribution_id"),
        col("requested_by_user_id"),
        col("expires_at"),
        col("consumed_at"),
        col("created_at"),
    ],
    key: &["token_hash"],
    unique: &[],
    parents: &[ON_DRAFT],
    user_column: Some("requested_by_user_id"),
    left_behind: "used or expired MusicBrainz return links, or links of deleted users. If \
                  MusicBrainz did not report back, enter the new release's MBID on the \
                  album's contribution in v3",
};
