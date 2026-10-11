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

/// Which reviews a listing returns. Empty lists and `None` filter
/// nothing.
#[derive(Debug, Clone, Default)]
pub struct ReviewFilter {
    /// One state, or every state when `None`.
    pub state: Option<ReviewState>,
    pub album_id: Option<String>,
    pub reason_code: Option<String>,
    /// Matches the album title or artist, case-insensitive.
    pub search: Option<String>,
    /// Only these reviews (a bulk selection by hand).
    pub review_ids: Vec<String>,
    /// Albums with a file in one of these library folders.
    pub root_ids: Vec<String>,
    /// Albums with a file under this folder policy (`automatic`,
    /// `local_metadata` or `excluded`).
    pub policy: Option<String>,
    /// Albums with an identification job in this state.
    pub job_state: Option<String>,
    /// Leave out albums whose identification is queued or running.
    pub exclude_active_jobs: bool,
    /// Filed or changed inside these bounds (Unix milliseconds).
    pub created_from: Option<i64>,
    pub created_to: Option<i64>,
    pub updated_from: Option<i64>,
    pub updated_to: Option<i64>,
    /// Oldest change first instead of newest.
    pub oldest_first: bool,
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
    let json_list = |values: &[String]| {
        (!values.is_empty()).then(|| serde_json::Value::from(values.to_vec()).to_string())
    };
    let review_ids = json_list(&filter.review_ids);
    let root_ids = json_list(&filter.root_ids);
    let state = filter.state.map(state_name);
    let clause = "FROM library_identify_reviews r \
         JOIN local_albums a ON a.id = r.local_album_id \
         WHERE a.retired_into_album_id IS NULL \
         AND (:state IS NULL OR r.state = :state) \
         AND (:album IS NULL OR r.local_album_id = :album) \
         AND (:reason IS NULL OR r.reason_code = :reason) \
         AND (:search IS NULL OR lower(a.title) LIKE :search \
              OR lower(COALESCE(a.album_artist_name, '')) LIKE :search) \
         AND (:ids IS NULL OR r.id IN (SELECT value FROM json_each(:ids))) \
         AND (:roots IS NULL OR EXISTS (SELECT 1 FROM local_tracks t \
              WHERE t.local_album_id = r.local_album_id \
              AND t.root_id IN (SELECT value FROM json_each(:roots)))) \
         AND (:policy IS NULL OR EXISTS (SELECT 1 FROM local_tracks t \
              WHERE t.local_album_id = r.local_album_id AND t.applied_policy = :policy)) \
         AND (:job IS NULL OR EXISTS (SELECT 1 FROM library_identify_jobs j \
              WHERE j.local_album_id = r.local_album_id AND j.state = :job)) \
         AND (:no_active = 0 OR NOT EXISTS (SELECT 1 FROM library_identify_jobs j \
              WHERE j.local_album_id = r.local_album_id \
              AND j.state IN ('queued', 'running', 'deferred'))) \
         AND (:created_from IS NULL OR r.created_ms >= :created_from) \
         AND (:created_to IS NULL OR r.created_ms <= :created_to) \
         AND (:updated_from IS NULL OR r.updated_ms >= :updated_from) \
         AND (:updated_to IS NULL OR r.updated_ms <= :updated_to)";
    let params = rusqlite::named_params! {
        ":state": state,
        ":album": filter.album_id,
        ":reason": filter.reason_code,
        ":search": search,
        ":ids": review_ids,
        ":roots": root_ids,
        ":policy": filter.policy,
        ":job": filter.job_state,
        ":no_active": filter.exclude_active_jobs,
        ":created_from": filter.created_from,
        ":created_to": filter.created_to,
        ":updated_from": filter.updated_from,
        ":updated_to": filter.updated_to,
    };
    let total: i64 = conn.query_row(&format!("SELECT COUNT(*) {clause}"), params, |row| {
        row.get(0)
    })?;
    let order = if filter.oldest_first { "ASC" } else { "DESC" };
    let mut statement = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} {clause} ORDER BY r.updated_ms {order}, r.id \
         LIMIT {} OFFSET {}",
        filter.limit, filter.offset
    ))?;
    let rows = statement
        .query_map(params, map_row)?
        .filter_map(Result::transpose)
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, u64::try_from(total).unwrap_or(0)))
}

/// How many reviews a batch changed, and why the rest were left alone.
#[derive(Debug, Clone, Default)]
pub struct Tally {
    pub done: u32,
    /// Each reason once, with how many reviews it held back.
    pub skipped: Vec<(reasons::Reason, u32)>,
}

impl Tally {
    pub fn skip(&mut self, reason: reasons::Reason) {
        match self
            .skipped
            .iter_mut()
            .find(|(seen, _)| seen.code == reason.code)
        {
            Some((_, count)) => *count += 1,
            None => self.skipped.push((reason, 1)),
        }
    }
}

/// What a bulk action does to each selected review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkAction {
    /// Keep the album as tagged.
    KeepTagged,
    /// Take the album's files out of the library.
    Exclude,
    /// Use the chosen release, or each review's best one.
    Accept,
}

impl BulkAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::KeepTagged => "keep_tagged",
            Self::Exclude => "exclude",
            Self::Accept => "accept",
        }
    }
}

/// One review a bulk action will change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkItem {
    pub review_id: String,
    /// The release an accept uses.
    pub candidate_key: Option<String>,
}

/// A bulk action's preview.
#[derive(Debug, Clone)]
pub struct BulkPlan {
    /// Reviews the selection matched.
    pub matched: u64,
    pub items: Vec<BulkItem>,
    pub skipped: Vec<(reasons::Reason, u32)>,
    /// Names exactly this action over exactly these reviews.
    pub token: String,
}

/// Work out which selected reviews a bulk action changes. Only pending
/// reviews change; an accept needs the chosen release on the review, or
/// any release when none was chosen (the best one is used).
pub fn plan_bulk(
    rows: &[ReviewRow],
    matched: u64,
    action: BulkAction,
    candidate_key: Option<&str>,
) -> BulkPlan {
    use sha2::Digest as _;

    let mut tally = Tally::default();
    let mut items = Vec::new();
    for row in rows {
        let review = &row.review;
        if review.state != ReviewState::Pending {
            tally.skip(reasons::REVIEW_SETTLED);
            continue;
        }
        let candidate = if action == BulkAction::Accept {
            let chosen = match candidate_key {
                Some(key) => review
                    .candidates
                    .iter()
                    .find(|candidate| candidate.candidate_key == key),
                None => review
                    .candidates
                    .iter()
                    .max_by(|a, b| a.score.total_cmp(&b.score)),
            };
            match chosen {
                Some(candidate) => Some(candidate.candidate_key.clone()),
                None => {
                    tally.skip(if candidate_key.is_some() {
                        reasons::REVIEW_CANDIDATE_MISSING
                    } else {
                        reasons::REVIEW_NO_CANDIDATE
                    });
                    continue;
                }
            }
        } else {
            None
        };
        items.push(BulkItem {
            review_id: review.id.clone(),
            candidate_key: candidate,
        });
    }
    let mut hasher = sha2::Sha256::new();
    hasher.update(action.name());
    for item in &items {
        hasher.update(b"\n");
        hasher.update(&item.review_id);
        hasher.update(b"=");
        hasher.update(item.candidate_key.as_deref().unwrap_or_default());
    }
    let token = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    BulkPlan {
        matched,
        items,
        skipped: tally.skipped,
        token,
    }
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
