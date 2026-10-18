//! Review operations: ambiguous cases wait on a curator, never on a guess.

use super::models::{CandidateEvidence, ReviewItem, ReviewState};
use super::sqlite::SqliteIdentifyStore;
use super::stores::StoreError;

/// File one review for an ambiguous album. Returns the stored item.
pub fn file_review(
    store: &SqliteIdentifyStore,
    id: &str,
    local_album_id: &str,
    reason_code: &str,
    candidates: Vec<CandidateEvidence>,
) -> Result<ReviewItem, StoreError> {
    let review = ReviewItem {
        id: id.to_owned(),
        local_album_id: local_album_id.to_owned(),
        reason_code: reason_code.to_owned(),
        candidates,
        state: ReviewState::Pending,
        resolved_by_user_id: None,
        selected_candidate_key: None,
    };
    store.file(review.clone())?;
    Ok(review)
}

/// Reject a pending review: the candidates are wrong, keep the album
/// tagged as-is. Rejecting a settled review is a no-op returning false.
pub fn reject_review(
    store: &SqliteIdentifyStore,
    review_id: &str,
    by_user_id: &str,
) -> Result<bool, StoreError> {
    let Some(review) = store.get(review_id)? else {
        return Ok(false);
    };
    if review.state != ReviewState::Pending {
        return Ok(false);
    }
    store.set_state(review_id, ReviewState::Rejected, Some(by_user_id), None)
}
