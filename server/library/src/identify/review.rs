//! Review operations: ambiguous cases wait on a curator, never on a guess.

use super::models::ReviewState;
use super::sqlite::SqliteIdentifyStore;
use super::stores::StoreError;

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
