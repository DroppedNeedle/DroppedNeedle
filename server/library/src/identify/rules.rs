//! Identity product rules from v2: who may overwrite whom, and when an
//! automatic identity retracts.
//! Matching files to releases lives in `crate::matching`.
//!
//! Every function here is pure: the same inputs always give the same
//! verdict, so the tests pin behavior without any store or provider.

use super::models::{DecisionSource, IdentificationOutcome};

/// Overwrite protection: an automatic pass may only touch revisable rows.
/// Manual and legacy-import rows survive rescans, moves, and re-id; when a
/// new attempt disagrees with one, the right move is a review, not a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwriteVerdict {
    /// Safe to write: no row yet, or the row is revisable.
    MayWrite,
    /// The row is curator-protected; file a review instead.
    ProtectedFileReview,
    /// The row is curator-protected and the new evidence agrees with it;
    /// a settled question, not new information. No review needed.
    QuietReconfirm,
}

pub fn evaluate_overwrite(
    current: Option<DecisionSource>,
    new_release_group_mbid: Option<&str>,
    current_release_group_mbid: Option<&str>,
    _contradictory: bool,
) -> OverwriteVerdict {
    match current {
        None => OverwriteVerdict::MayWrite,
        Some(source) if source.is_revisable() => OverwriteVerdict::MayWrite,
        Some(_) => {
            // Agreement is computed from the two release-group MBIDs:
            // a protected row the new evidence agrees with quietly
            // reconfirms, and only a genuine disagreement files a
            // review. The legacy contradiction flag plays no part.
            let agrees = match (new_release_group_mbid, current_release_group_mbid) {
                (Some(next), Some(have)) => next.eq_ignore_ascii_case(have),
                _ => false,
            };
            if agrees {
                OverwriteVerdict::QuietReconfirm
            } else {
                OverwriteVerdict::ProtectedFileReview
            }
        }
    }
}

/// Revisable identities retract when contradiction arrives; protected ones
/// never do. Returns true when the stored identity must be cleared.
pub fn retracts_on_contradiction(
    current: Option<DecisionSource>,
    outcome: IdentificationOutcome,
) -> bool {
    matches!(outcome, IdentificationOutcome::Contradictory)
        && current.is_some_and(DecisionSource::is_revisable)
}
