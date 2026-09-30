//! Identity product rules, ported verbatim from v2.
//!
//! Every function here is pure: the same inputs always give the same
//! verdict, so the briefs pin behavior without any store or provider.

use super::models::{
    ArtistCredit, CandidateEvidence, DecisionSource, EvidenceClass, IdentificationOutcome,
    ReleasePin,
};

/// F-IDENT-01 option-B verdict for the name-anchored retirement path.
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

/// Inputs to the F-IDENT-01 option-B gate.
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

/// F-IDENT-01 option B (owner 2026-08-20): album-level provider proof
/// gates every name-anchored retirement.
///
/// Preconditions, all required:
/// 1. An accepted MusicBrainz release MBID on the source album, OR a
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

/// Release pins are hint-only: they steer edition search and display, and
/// must never count as identity evidence. The pin crosses into candidate
/// recall only as a ranking hint, never into evidence scoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditionHint {
    pub release_group_mbid: String,
    pub preferred_release_mbid: String,
}

impl EditionHint {
    pub fn from_pin(pin: &ReleasePin) -> Self {
        Self {
            release_group_mbid: pin.release_group_mbid.clone(),
            preferred_release_mbid: pin.release_mbid.clone(),
        }
    }
}

/// Rank candidates with the pin as a tie-break only: the pinned edition
/// floats to the top of its own release group, but a pin never promotes a
/// candidate from another group and never changes any score.
pub fn rank_with_hint(
    mut candidates: Vec<CandidateEvidence>,
    hint: Option<&EditionHint>,
) -> Vec<CandidateEvidence> {
    let Some(hint) = hint else {
        return candidates;
    };
    candidates.sort_by(|a, b| {
        let a_pinned = a.release_mbid.as_deref() == Some(hint.preferred_release_mbid.as_str())
            && a.release_group_mbid
                .eq_ignore_ascii_case(&hint.release_group_mbid);
        let b_pinned = b.release_mbid.as_deref() == Some(hint.preferred_release_mbid.as_str())
            && b.release_group_mbid
                .eq_ignore_ascii_case(&hint.release_group_mbid);
        b_pinned
            .cmp(&a_pinned)
            .then_with(|| b.score.total_cmp(&a.score))
    });
    candidates
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

/// Score one candidate against local facts. Provider proof (embedded
/// recording MBIDs, AcoustID support, release MBIDs) decides; folded
/// title similarity only breaks ties and can never identify alone.
pub fn classify_track(
    local_recording_mbid: Option<&str>,
    local_release_mbid: Option<&str>,
    candidate_recording_mbid: Option<&str>,
    candidate_release_mbid: Option<&str>,
    fingerprint_support_mbid: Option<&str>,
) -> EvidenceClass {
    if let (Some(local), Some(candidate)) = (local_recording_mbid, candidate_recording_mbid) {
        if mbid_eq(local, candidate) {
            return EvidenceClass::Supported;
        }
        return EvidenceClass::Contradictory;
    }
    if let (Some(local), Some(candidate)) = (local_release_mbid, candidate_release_mbid)
        && !mbid_eq(local, candidate)
    {
        return EvidenceClass::Contradictory;
    }
    if let (Some(support), Some(candidate)) = (fingerprint_support_mbid, candidate_recording_mbid)
        && mbid_eq(support, candidate)
    {
        return EvidenceClass::Supported;
    }
    EvidenceClass::Unknown
}
