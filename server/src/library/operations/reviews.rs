//! Identification reviews a person settles from the review list.
//!
//! Most albums never need one: identification always picks a best guess
//! and lists unsure albums as unconfirmed. Reviews are the cases that wait
//! on a person: an undone automatic edition with nothing to go back to,
//! editions dropped when albums were combined, and repair findings.
//!
//! Choosing a candidate and keeping the album as tagged live with the
//! identify service. This module adds the rest of what v2 offered:
//! dismiss (close without a decision), detach (take the match off and keep
//! the tags), exclude (take the files out of the library) and restore
//! (put them back). Each runs inside the caller's transaction.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::choice;
use super::decisions::{CatalogAction, bump_catalog, record_action};
use super::models::OperationError;
use super::reasons;
use crate::library::identify::models::{ReviewItem, ReviewState};
use crate::library::identify::sqlite::{bump_identification_stream, map_review};

/// The match-state reason a detached album shows.
const DETACHED: &str = "DETACHED_BY_PERSON";

/// Which reviews a listing returns.
#[derive(Debug, Clone, Default)]
pub struct ReviewFilter {
    /// One state, or every state when `None`.
    pub state: Option<ReviewState>,
    pub album_id: Option<String>,
    pub reason_code: Option<String>,
    /// Matches the album title or artist, case-insensitive.
    pub search: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

/// One review with the album it is about.
#[derive(Debug, Clone)]
pub struct ReviewRow {
    pub review: ReviewItem,
    pub album_title: String,
    pub artist_name: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// What a person does with a review besides choosing or keeping tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    Dismiss,
    Detach,
    Exclude,
    Restore,
}

const ROW_COLUMNS: &str = "r.id, r.local_album_id, r.reason_code, r.candidates_json, r.state, \
     r.resolved_by_user_id, r.selected_candidate_key, COALESCE(a.title, ''), \
     COALESCE(a.album_artist_name, ''), r.created_ms, r.updated_ms";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<ReviewRow>> {
    let Some(review) = map_review(row)? else {
        return Ok(None);
    };
    Ok(Some(ReviewRow {
        review,
        album_title: row.get(7)?,
        artist_name: row.get(8)?,
        created_ms: row.get(9)?,
        updated_ms: row.get(10)?,
    }))
}

/// State name as stored.
pub fn state_name(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Pending => "pending",
        ReviewState::Approved => "approved",
        ReviewState::Rejected => "rejected",
        ReviewState::Dismissed => "dismissed",
        ReviewState::Excluded => "excluded",
    }
}

/// Parse a state filter: `None` or `all` lists every state.
pub fn parse_state(raw: Option<&str>) -> Result<Option<ReviewState>, OperationError> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("all") => Ok(None),
        Some("pending") => Ok(Some(ReviewState::Pending)),
        Some("approved") => Ok(Some(ReviewState::Approved)),
        Some("rejected") => Ok(Some(ReviewState::Rejected)),
        Some("dismissed") => Ok(Some(ReviewState::Dismissed)),
        Some("excluded") => Ok(Some(ReviewState::Excluded)),
        Some(_) => Err(OperationError::Invalid(reasons::REVIEW_STATE_INVALID)),
    }
}

/// One page of reviews, newest change first, and the total that match.
pub fn list(conn: &Connection, filter: &ReviewFilter) -> rusqlite::Result<(Vec<ReviewRow>, u64)> {
    let search = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.to_lowercase()));
    let state = filter.state.map(state_name);
    let clause = "FROM library_identify_reviews r \
         JOIN local_albums a ON a.id = r.local_album_id \
         WHERE a.retired_into_album_id IS NULL \
         AND (?1 IS NULL OR r.state = ?1) \
         AND (?2 IS NULL OR r.local_album_id = ?2) \
         AND (?3 IS NULL OR r.reason_code = ?3) \
         AND (?4 IS NULL OR lower(a.title) LIKE ?4 OR lower(COALESCE(a.album_artist_name, '')) LIKE ?4)";
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) {clause}"),
        params![state, filter.album_id, filter.reason_code, search],
        |row| row.get(0),
    )?;
    let mut statement = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} {clause} ORDER BY r.updated_ms DESC, r.id LIMIT ?5 OFFSET ?6"
    ))?;
    let rows = statement
        .query_map(
            params![
                state,
                filter.album_id,
                filter.reason_code,
                search,
                filter.limit,
                filter.offset
            ],
            map_row,
        )?
        .filter_map(Result::transpose)
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, u64::try_from(total).unwrap_or(0)))
}

/// One review by id.
pub fn get(conn: &Connection, review_id: &str) -> rusqlite::Result<Option<ReviewRow>> {
    conn.query_row(
        &format!(
            "SELECT {ROW_COLUMNS} FROM library_identify_reviews r \
             LEFT JOIN local_albums a ON a.id = r.local_album_id WHERE r.id = ?1"
        ),
        [review_id],
        map_row,
    )
    .optional()
    .map(Option::flatten)
}

/// Settle a review with `action`. Dismiss, detach and exclude need a
/// pending review; restore needs an excluded one.
pub fn settle(
    tx: &Transaction<'_>,
    review_id: &str,
    action: ReviewAction,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let (album_id, state): (String, String) = tx
        .query_row(
            "SELECT local_album_id, state FROM library_identify_reviews WHERE id = ?1",
            [review_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(OperationError::NotFound(reasons::REVIEW_NOT_FOUND))?;
    let needed = if action == ReviewAction::Restore {
        "excluded"
    } else {
        "pending"
    };
    if state != needed {
        return Err(OperationError::Conflict(
            if action == ReviewAction::Restore {
                reasons::REVIEW_NOT_EXCLUDED
            } else {
                reasons::REVIEW_SETTLED
            },
        ));
    }
    let next = match action {
        ReviewAction::Dismiss => ReviewState::Dismissed,
        ReviewAction::Detach => {
            choice::detach_by_person(tx, &album_id, DETACHED, actor, now)?;
            ReviewState::Rejected
        }
        ReviewAction::Exclude => {
            set_excluded(tx, &album_id, true, actor, now)?;
            ReviewState::Excluded
        }
        ReviewAction::Restore => {
            set_excluded(tx, &album_id, false, actor, now)?;
            ReviewState::Dismissed
        }
    };
    mark(tx, review_id, next, actor)?;
    Ok(())
}

/// Record a new state on one review.
pub fn mark(
    tx: &Transaction<'_>,
    review_id: &str,
    state: ReviewState,
    actor: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE library_identify_reviews SET state = ?2, resolved_by_user_id = ?3, \
         updated_ms = ?4 WHERE id = ?1",
        params![
            review_id,
            state_name(state),
            actor,
            crate::library::clock::now_ms() as i64
        ],
    )?;
    bump_identification_stream(tx)
}

/// Take an album's files out of the library (or put them back). Excluded
/// files stay on disk; scans keep them out until they are restored. A
/// restore leaves files the library policy excludes as excluded.
fn set_excluded(
    tx: &Transaction<'_>,
    album_id: &str,
    exclude: bool,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let changed = if exclude {
        tx.execute(
            "UPDATE local_tracks SET manual_excluded = 1, availability = 'excluded', \
             excluded_at = ?2, row_revision = row_revision + 1 \
             WHERE local_album_id = ?1 AND availability = 'indexed'",
            params![album_id, now],
        )?
    } else {
        tx.execute(
            "UPDATE local_tracks SET manual_excluded = 0, \
             availability = CASE WHEN applied_policy = 'excluded' THEN 'excluded' ELSE 'indexed' END, \
             excluded_at = CASE WHEN applied_policy = 'excluded' THEN excluded_at ELSE NULL END, \
             row_revision = row_revision + 1 \
             WHERE local_album_id = ?1 AND manual_excluded = 1",
            params![album_id],
        )?
    };
    let (kind, reason) = if exclude {
        ("review_exclude", "EXCLUDED_FROM_REVIEW")
    } else {
        ("review_restore", "RESTORED_FROM_REVIEW")
    };
    record_action(
        tx,
        CatalogAction {
            actor,
            kind,
            album_id,
            job_id: None,
            before: serde_json::json!({ "excluded": !exclude }),
            after: serde_json::json!({ "excluded": exclude, "tracks": changed }),
            reason,
        },
        now,
    )?;
    bump_catalog(tx)?;
    Ok(())
}
