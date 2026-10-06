//! Identity product rules from v2: who may overwrite whom, when an
//! automatic identity retracts, and the proof gates for merging artists.
//! Matching files to releases lives in `crate::library::matching`.
//!
//! Every function here is pure: the same inputs always give the same
//! verdict, so the tests pin behavior without any store or provider.

use super::models::{ArtistCredit, DecisionSource, IdentificationOutcome};

/// Verdict for the name-anchored retirement path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstitutionVerdict {
    /// Every precondition holds; the retirement may proceed.
    Allowed,
    /// Proof is missing or stale; park the case, never guess.
    Waiting,
    /// Proof conflicts; a curator must decide.
    NeedsReview,
    /// Hard refusal with the exact why.
    Refused(SubstitutionRefusal),
}

/// Exact refusal reasons for album-level substitution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstitutionRefusal {
    /// No accepted release MBID and no durable proof row: name-only.
    NameOnly,
    /// Credits name several provider artists or none at all.
    CompositeOrAmbiguousCredits,
    /// The album already carries a direct identity naming someone else.
    ConflictingDirectIdentity,
}

/// Inputs to the name-anchored retirement gate.
#[derive(Debug, Clone, Default)]
pub struct SubstitutionCase {
    /// Accepted MusicBrainz release MBID on the source album, if any.
    pub accepted_release_mbid: Option<String>,
    /// Durable (revision-valid) proof rows for the source artist.
    pub durable_proof_mbids: Vec<String>,
    /// The provider artist MBID the retirement would converge on.
    pub expected_artist_mbid: String,
    /// Every provider credit resolves to exactly one artist.
    pub credits_unambiguous: bool,
    /// Direct provider artist identity on the source artist, if any.
    pub direct_identity_mbid: Option<String>,
    /// A folded-name match triggered this attempt. With proof missing,
    /// a bare case waits for proof; a name-anchored one refuses outright.
    pub name_evidence_present: bool,
}

/// Album-level provider proof gates every name-anchored retirement; a
/// name match alone never merges artists.
///
/// Preconditions, all required:
/// 1. An accepted MusicBrainz release MBID on the source album, or a
///    durable revision-valid credit proof row agreeing on the expected MBID.
/// 2. No contradictory proof: a current proof row naming anyone else vetoes.
/// 3. No composite or ambiguous credits.
/// 4. Never name-only: a name match without proof refuses, it never passes.
///
/// Missing proof with no name claim waits for proof to arrive; a
/// revision-valid proof row naming anyone else sends the case to a
/// curator. A direct identity naming a different artist refuses.
pub fn evaluate_substitution(case: &SubstitutionCase) -> SubstitutionVerdict {
    if !case.credits_unambiguous {
        return SubstitutionVerdict::Refused(SubstitutionRefusal::CompositeOrAmbiguousCredits);
    }
    if let Some(direct) = case.direct_identity_mbid.as_deref()
        && !mbid_eq(direct, &case.expected_artist_mbid)
    {
        return SubstitutionVerdict::Refused(SubstitutionRefusal::ConflictingDirectIdentity);
    }
    let expected = case.expected_artist_mbid.as_str();
    if case
        .durable_proof_mbids
        .iter()
        .any(|mbid| !mbid_eq(mbid, expected))
    {
        return SubstitutionVerdict::NeedsReview;
    }
    if !case.durable_proof_mbids.is_empty() {
        return SubstitutionVerdict::Allowed;
    }
    if case.accepted_release_mbid.is_some() {
        return SubstitutionVerdict::Allowed;
    }
    if case.name_evidence_present {
        return SubstitutionVerdict::Refused(SubstitutionRefusal::NameOnly);
    }
    SubstitutionVerdict::Waiting
}

/// Provider-proof rule for automatic reconciliation: folded-name or
/// spelling similarity alone never merges. At least one durable proof
/// signal (accepted release identity, revision-valid proof row, or a
/// provider redirect lookup) must agree.
#[derive(Debug, Clone, Default)]
pub struct ReconciliationProof {
    pub name_similarity: f64,
    pub accepted_release_identity: bool,
    pub durable_proof_rows: usize,
    pub redirect_lookup_proved: bool,
    pub contradictory_proof: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationVerdict {
    Merge,
    Waiting,
    NeedsReview,
    RefusedNameOnly,
}

pub fn evaluate_reconciliation(proof: &ReconciliationProof) -> ReconciliationVerdict {
    if proof.contradictory_proof {
        return ReconciliationVerdict::NeedsReview;
    }
    if proof.accepted_release_identity
        || proof.durable_proof_rows > 0
        || proof.redirect_lookup_proved
    {
        return ReconciliationVerdict::Merge;
    }
    if proof.name_similarity <= 0.0 {
        return ReconciliationVerdict::Waiting;
    }
    ReconciliationVerdict::RefusedNameOnly
}

/// Overwrite protection: an automatic pass may only touch automatic rows.
/// Manual and legacy-import rows survive rescans, moves, and re-id; when a
/// new attempt disagrees with one, the right move is a review, not a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwriteVerdict {
    /// Safe to write: no row yet, or the row is automatic.
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
        Some(source) if source.automatic_may_overwrite() => OverwriteVerdict::MayWrite,
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

/// Automatic identities retract when contradiction arrives; protected ones
/// never do. Returns true when the stored identity must be cleared.
pub fn retracts_on_contradiction(
    current: Option<DecisionSource>,
    outcome: IdentificationOutcome,
) -> bool {
    matches!(outcome, IdentificationOutcome::Contradictory)
        && matches!(current, Some(DecisionSource::Automatic))
}

/// Exact track contributors create appearances, never owned artists.
/// A credit whose provider MBID already owns a local artist stays owned;
/// every other exact credit becomes an appearance on the track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreditRole {
    OwnedArtist { local_artist_id: String },
    Appearance,
}

pub fn classify_credit(
    credit: &ArtistCredit,
    owned_by_mbid: &std::collections::HashMap<String, String>,
) -> CreditRole {
    match owned_by_mbid.get(&credit.artist_mbid.to_lowercase()) {
        Some(local_id) => CreditRole::OwnedArtist {
            local_artist_id: local_id.clone(),
        },
        None => CreditRole::Appearance,
    }
}

/// A casefold MBID comparison: MBIDs are ASCII, so this is exact.
fn mbid_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}
