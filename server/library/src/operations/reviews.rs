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
use super::decisions::{CatalogAction, record_action};
use super::models::OperationError;
use super::reasons;
use crate::catalog_revision::bump_catalog;
use crate::identify::models::{ReviewItem, ReviewState};
use crate::identify::sqlite::{bump_identification_stream, map_review};

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
    /// Only reviews that list at least one release.
    pub with_candidates: bool,
    pub order: ReviewOrder,
    pub limit: u32,
    pub offset: u32,
}

/// How a listing is sorted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReviewOrder {
    /// Most recently changed first.
    #[default]
    Newest,
    /// Least recently changed first.
    Oldest,
    /// Albums with the most files first.
    MostTracks,
}

/// One review with the album it is about.
#[derive(Debug, Clone)]
pub struct ReviewRow {
    pub review: ReviewItem,
    pub album_title: String,
    pub artist_name: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// The album's match was chosen by a person.
    pub manual_identity: bool,
    /// The album's files as (track id, title), for the evidence list.
    pub tracks: Vec<(String, String)>,
}

/// What a person does with a review besides choosing or keeping tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    Dismiss,
    Detach,
    Exclude,
    Restore,
}

/// A review row's columns; the last says whether a person's choice, made
/// now or carried over from v2, holds the album.
fn row_columns() -> String {
    format!(
        "r.id, r.local_album_id, r.reason_code, r.candidates_json, r.state, \
         r.resolved_by_user_id, r.selected_candidate_key, COALESCE(a.title, ''), \
         COALESCE(a.album_artist_name, ''), r.created_ms, r.updated_ms, \
         EXISTS (SELECT 1 FROM local_album_external_identities e \
                 WHERE e.local_album_id = r.local_album_id AND e.decision_source IN {})",
        crate::identity::sql::PERSONS
    )
}

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
        manual_identity: row.get(11)?,
        tracks: Vec::new(),
    }))
}

/// Fill in each row's file titles.
fn with_tracks(conn: &Connection, rows: &mut [ReviewRow]) -> rusqlite::Result<()> {
    let mut statement = conn.prepare_cached(
        "SELECT id, title FROM local_tracks WHERE local_album_id = ?1 \
         ORDER BY disc_number, track_number, id",
    )?;
    for row in rows {
        row.tracks = statement
            .query_map([&row.review.local_album_id], |track| {
                Ok((track.get(0)?, track.get(1)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(())
}

/// Reviews matching a filter (its reason left out), counted by reason.
pub type ReasonCounts = Vec<(String, u64)>;

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

/// One page of reviews, the total that match, and how many match each
/// reason (ignoring the reason filter, so every bucket stays visible).
pub fn list(
    conn: &Connection,
    filter: &ReviewFilter,
) -> rusqlite::Result<(Vec<ReviewRow>, u64, ReasonCounts)> {
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
         AND (:updated_to IS NULL OR r.updated_ms <= :updated_to) \
         AND (:with_candidates = 0 OR json_array_length(r.candidates_json) > 0)";
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
        ":with_candidates": filter.with_candidates,
    };
    let total: i64 = conn.query_row(&format!("SELECT COUNT(*) {clause}"), params, |row| {
        row.get(0)
    })?;
    let order = match filter.order {
        ReviewOrder::Newest => "r.updated_ms DESC",
        ReviewOrder::Oldest => "r.updated_ms ASC",
        ReviewOrder::MostTracks => {
            "(SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id = r.local_album_id) DESC, \
             r.updated_ms DESC"
        }
    };
    let mut statement = conn.prepare(&format!(
        "SELECT {} {clause} ORDER BY {order}, r.id LIMIT {} OFFSET {}",
        row_columns(),
        filter.limit,
        filter.offset
    ))?;
    let mut rows = statement
        .query_map(params, map_row)?
        .filter_map(Result::transpose)
        .collect::<rusqlite::Result<Vec<_>>>()?;
    with_tracks(conn, &mut rows)?;
    // The reason buckets ignore the reason filter; the clause allows a
    // NULL reason, so binding one gives every reason's count.
    let bucket_params = rusqlite::named_params! {
        ":state": state,
        ":album": filter.album_id,
        ":reason": Option::<String>::None,
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
        ":with_candidates": filter.with_candidates,
    };
    let mut buckets = conn.prepare(&format!(
        "SELECT r.reason_code, COUNT(*) {clause} GROUP BY r.reason_code ORDER BY COUNT(*) DESC"
    ))?;
    let counts = buckets
        .query_map(bucket_params, |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .map(|row| row.map(|(code, count)| (code, u64::try_from(count).unwrap_or(0))))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, u64::try_from(total).unwrap_or(0), counts))
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
        // Accepting never overwrites a person's pin, and the editions a
        // merge dropped are only taken back when one is named.
        if action == BulkAction::Accept && row.manual_identity {
            tally.skip(reasons::REVIEW_ALBUM_PINNED);
            continue;
        }
        if action == BulkAction::Accept
            && candidate_key.is_none()
            && review.reason_code.starts_with("EDITION_")
        {
            tally.skip(reasons::REVIEW_MERGE_NEEDS_CHOICE);
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
            "SELECT {} FROM library_identify_reviews r \
             LEFT JOIN local_albums a ON a.id = r.local_album_id WHERE r.id = ?1",
            row_columns()
        ),
        [review_id],
        map_row,
    )
    .optional()?
    .flatten()
    .map(|row| {
        let mut rows = [row];
        with_tracks(conn, &mut rows)?;
        let [row] = rows;
        Ok(row)
    })
    .transpose()
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
            exclude(tx, review_id, &album_id, actor, now)?;
            ReviewState::Excluded
        }
        ReviewAction::Restore => {
            restore(tx, review_id, &album_id, actor, now)?;
            ReviewState::Dismissed
        }
    };
    if !mark(tx, review_id, needed, next, actor, now)? {
        return Err(OperationError::Conflict(reasons::REVIEW_SETTLED));
    }
    Ok(())
}

/// Move one review from `from` to `to`. False when it was no longer in
/// `from` (someone else settled it first).
pub fn mark(
    tx: &Transaction<'_>,
    review_id: &str,
    from: &str,
    to: ReviewState,
    actor: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE library_identify_reviews SET state = ?3, resolved_by_user_id = ?4, \
         updated_ms = ?5 WHERE id = ?1 AND state = ?2",
        params![
            review_id,
            from,
            state_name(to),
            actor,
            crate::clock::ms(now)
        ],
    )?;
    if changed == 1 {
        bump_identification_stream(tx)?;
    }
    Ok(changed == 1)
}

/// Take an album's files out of the library. Every file of the album is
/// marked, including ones missing right now, so a file that comes back
/// stays out; the files this changed are recorded so a restore puts back
/// exactly those. Excluded files stay on disk.
fn exclude(
    tx: &Transaction<'_>,
    review_id: &str,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let changed: Vec<String> = tx
        .prepare(
            "UPDATE local_tracks SET manual_excluded = 1, \
             availability = CASE WHEN availability = 'indexed' THEN 'excluded' \
                            ELSE availability END, \
             excluded_at = ?2, row_revision = row_revision + 1 \
             WHERE local_album_id = ?1 AND manual_excluded = 0 RETURNING id",
        )?
        .query_map(params![album_id, now], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for track_id in &changed {
        tx.execute(
            "INSERT OR IGNORE INTO library_review_exclusions (review_id, local_track_id) \
             VALUES (?1, ?2)",
            params![review_id, track_id],
        )?;
    }
    audit(tx, album_id, actor, true, changed.len(), now)
}

/// Put back the files a review's exclude took out. A review imported from
/// v2 has no record of them, so every file excluded by hand comes back.
/// Files the folder policy excludes stay excluded.
fn restore(
    tx: &Transaction<'_>,
    review_id: &str,
    album_id: &str,
    actor: &str,
    now: f64,
) -> Result<(), OperationError> {
    let recorded: i64 = tx.query_row(
        "SELECT COUNT(*) FROM library_review_exclusions WHERE review_id = ?1",
        [review_id],
        |row| row.get(0),
    )?;
    // Both parameters are always bound; the scope uses the one it needs.
    let scope = if recorded > 0 {
        "id IN (SELECT local_track_id FROM library_review_exclusions WHERE review_id = ?1) \
         AND ?2 IS NOT NULL"
    } else {
        "local_album_id = ?2 AND ?1 IS NOT NULL"
    };
    let changed = tx.execute(
        &format!(
            "UPDATE local_tracks SET manual_excluded = 0, \
             availability = CASE WHEN availability = 'excluded' AND applied_policy <> 'excluded' \
                            THEN 'indexed' ELSE availability END, \
             excluded_at = CASE WHEN applied_policy = 'excluded' THEN excluded_at ELSE NULL END, \
             row_revision = row_revision + 1 \
             WHERE manual_excluded = 1 AND {scope}"
        ),
        params![review_id, album_id],
    )?;
    tx.execute(
        "DELETE FROM library_review_exclusions WHERE review_id = ?1",
        [review_id],
    )?;
    audit(tx, album_id, actor, false, changed, now)
}

fn audit(
    tx: &Transaction<'_>,
    album_id: &str,
    actor: &str,
    exclude: bool,
    tracks: usize,
    now: f64,
) -> Result<(), OperationError> {
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
            after: serde_json::json!({ "excluded": exclude, "tracks": tracks }),
            reason,
        },
        now,
    )?;
    bump_catalog(tx)?;
    Ok(())
}
