//! Identity product rules: when an automatic identity retracts.
//! Matching files to releases lives in `crate::matching`.
//!
//! Every function here is pure: the same inputs always give the same
//! verdict, so the tests pin behavior without any store or provider.

use super::models::{DecisionSource, IdentificationOutcome};

/// Revisable identities retract when contradiction arrives; protected ones
/// never do. Returns true when the stored identity must be cleared.
pub fn retracts_on_contradiction(
    current: Option<DecisionSource>,
    outcome: IdentificationOutcome,
) -> bool {
    matches!(outcome, IdentificationOutcome::Contradictory)
        && current.is_some_and(DecisionSource::is_revisable)
}
