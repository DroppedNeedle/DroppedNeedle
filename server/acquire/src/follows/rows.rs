//! Plain row writes for the offline importer, inside its transaction. They
//! write rows and nothing else: no lane turn, no second connection, no wake-up,
//! ask or request row. What to write, and which fields win a merge, is the
//! importer's plan.

use rusqlite::{Transaction, params};

use super::sql;
use crate::requests::ledger::ApprovalState;

/// One followed artist as it is stored. Times are milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowRow {
    /// Owning user.
    pub user_id: String,
    /// Artist MBID in its original case.
    pub artist_mbid: String,
    /// Display name.
    pub artist_name: String,
    /// Auto-download intent.
    pub auto_download: bool,
    /// When the follow began.
    pub followed_at: i64,
    /// Last change.
    pub updated_at: i64,
}

/// The fields of an approval a merge may change. Times are milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalFields {
    /// Review state.
    pub state: ApprovalState,
    /// When it was asked for.
    pub requested_at: i64,
    /// Reviewer, while that user exists.
    pub reviewed_by_id: Option<String>,
    /// Reviewer's display name, which outlives the reviewer.
    pub reviewed_by_name: Option<String>,
    /// When it was reviewed.
    pub reviewed_at: Option<i64>,
    /// Bulk-approval batch.
    pub batch_id: Option<String>,
    /// Origin label.
    pub source: Option<String>,
}

/// Add one follow.
pub fn insert_follow_in(tx: &Transaction<'_>, row: &FollowRow) -> rusqlite::Result<()> {
    tx.execute(
        sql::INSERT_FOLLOW_ROW.text(),
        params![
            row.user_id,
            row.artist_mbid,
            row.artist_mbid.to_lowercase(),
            row.artist_name,
            row.auto_download,
            row.followed_at,
            row.updated_at,
        ],
    )?;
    Ok(())
}

/// Write a follow's merged values, found by user and lowercase MBID.
pub fn merge_follow_in(
    tx: &Transaction<'_>,
    user_id: &str,
    artist_mbid_lower: &str,
    auto_download: bool,
    followed_at: i64,
    updated_at: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::MERGE_FOLLOW_ROW.text(),
        params![
            user_id,
            artist_mbid_lower,
            auto_download,
            followed_at,
            updated_at,
        ],
    )?;
    Ok(())
}

/// Add one approval.
pub fn insert_approval_in(
    tx: &Transaction<'_>,
    user_id: &str,
    artist_mbid: &str,
    artist_name: &str,
    fields: &ApprovalFields,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::INSERT_APPROVAL_ROW.text(),
        params![
            user_id,
            artist_mbid,
            artist_mbid.to_lowercase(),
            artist_name,
            fields.state,
            fields.requested_at,
            fields.reviewed_by_id,
            fields.reviewed_by_name,
            fields.reviewed_at,
            fields.batch_id,
            fields.source,
        ],
    )?;
    Ok(())
}

/// Write an approval's merged values, found by user and lowercase MBID.
pub fn merge_approval_in(
    tx: &Transaction<'_>,
    user_id: &str,
    artist_mbid_lower: &str,
    fields: &ApprovalFields,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::MERGE_APPROVAL_ROW.text(),
        params![
            user_id,
            artist_mbid_lower,
            fields.state,
            fields.requested_at,
            fields.reviewed_by_id,
            fields.reviewed_by_name,
            fields.reviewed_at,
            fields.batch_id,
            fields.source,
        ],
    )?;
    Ok(())
}
