//! The verdict on one album, from what the matcher made of its files
//! against MusicBrainz. Pure: the service gathers the facts, this decides.
//!
//! Ported from v2's `IdentityRepairService._classify` (identity repair)
//! and `_classify_management_readiness` (identity preparation), on top of
//! v3's matching engine: a candidate's per-file evidence is the matcher's,
//! and "the matcher would accept it" is the candidate's `automatic_safe`.

use std::collections::{HashMap, HashSet};

use super::models::{EditionSummary, FindingPayload, Mapping, Verdict};
use crate::library::identify::models::EvidenceClass;
use crate::library::operations::models::ReidentificationCandidate;

/// The album's identity as the check found it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityState {
    pub release_group_mbid: Option<String>,
    pub release_mbid: Option<String>,
    /// Someone chose it: a recheck reports, never removes.
    pub manual: bool,
}

/// One indexed file's current identity row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackState {
    pub local_track_id: String,
    pub recording_mbid: Option<String>,
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub medium_position: Option<i64>,
    pub release_track_position: Option<i64>,
}

fn same_group(identity: &IdentityState, candidate: &ReidentificationCandidate) -> bool {
    identity
        .release_group_mbid
        .as_deref()
        .is_none_or(|group| group.eq_ignore_ascii_case(&candidate.evidence.release_group_mbid))
}

/// Identity repair: does the album's linked release still hold up?
/// `candidate` is the linked release scored against the files (`None`
/// when MusicBrainz has no such release), `release_track_count` its size.
pub fn existing_match(
    identity: &IdentityState,
    candidate: Option<&ReidentificationCandidate>,
    release_track_count: usize,
    local_count: usize,
) -> Verdict {
    if identity.manual {
        return Verdict::new("manual_identity", "MANUAL_IDENTITY_REPORT_ONLY");
    }
    let Some(candidate) = candidate else {
        return Verdict::new("unverifiable", "EVIDENCE_UNAVAILABLE");
    };
    let evidence = &candidate.evidence;
    let supported = evidence.supported_count();
    let contradictory = evidence.contradictory_count();
    // Only a whole-album copy is judged: a partial copy may be another
    // edition, which is a question for a person, not a detach.
    let complete = local_count > 0
        && release_track_count == local_count
        && evidence.track_evidence.len() == local_count;
    if complete && (supported == 0 || contradictory > 0) {
        let reason = if supported == 0 {
            "ZERO_SUPPORT"
        } else {
            "HARD_CONTRADICTION"
        };
        return Verdict::new("safe_detach", reason).eligible();
    }
    if !same_group(identity, candidate) {
        return Verdict::new("needs_review", "SELECTED_RELEASE_CONFLICT");
    }
    if candidate.automatic_safe {
        Verdict::new("valid", "CURRENT_IDENTITY_PASSES")
    } else {
        Verdict::new("needs_review", "NON_TERMINAL_SAFETY_CONCERN")
    }
}

/// Where every file lands on the candidate's release: each file
/// supported, with a recording, a release track and a position, no two on
/// one track, and no file contradicting the release. `None` otherwise.
pub fn full_mapping(candidate: &ReidentificationCandidate) -> Option<Vec<Mapping>> {
    let evidence = &candidate.evidence;
    if candidate.tracks.is_empty() || evidence.contradictory_count() > 0 {
        return None;
    }
    let mut seen = HashSet::new();
    let mut mapping = Vec::with_capacity(candidate.tracks.len());
    for spot in &candidate.tracks {
        let file = evidence
            .track_evidence
            .iter()
            .find(|track| track.local_track_id == spot.local_track_id)
            .filter(|track| track.classification == EvidenceClass::Supported)?;
        let placed = Mapping {
            local_track_id: spot.local_track_id.clone(),
            recording_mbid: file.recording_mbid.clone()?,
            release_track_mbid: file.release_track_mbid.clone()?,
            medium_position: spot.disc_number.filter(|disc| *disc > 0)?,
            release_track_position: spot.position.filter(|position| *position > 0)?,
        };
        if !seen.insert(placed.release_track_mbid.to_ascii_lowercase()) {
            return None;
        }
        mapping.push(placed);
    }
    Some(mapping)
}

fn same_recording(current: &str, proposed: &str, aliases: &HashMap<String, String>) -> bool {
    let canonical = |id: &str| {
        let id = id.to_ascii_lowercase();
        aliases
            .get(&id)
            .map(|to| to.to_ascii_lowercase())
            .unwrap_or(id)
    };
    canonical(current) == canonical(proposed)
}

/// Identity preparation for an album with an exact release: are its files
/// placed on that release, and can the missing placements be filled in
/// safely? `aliases` maps merged recording ids to their survivors.
pub fn readiness_exact(
    identity: &IdentityState,
    candidate: Option<&ReidentificationCandidate>,
    tracks: &[TrackState],
    aliases: &HashMap<String, String>,
) -> Verdict {
    let Some(candidate) = candidate else {
        return Verdict::new("needs_review", "SELECTED_RELEASE_UNAVAILABLE");
    };
    if !same_group(identity, candidate) {
        return Verdict::new("needs_review", "SELECTED_RELEASE_CONFLICT");
    }
    let Some(mapping) = full_mapping(candidate) else {
        return Verdict::new("needs_review", "EXACT_RELEASE_MAPPING_INCOMPLETE");
    };
    if mapping.len() != tracks.len() {
        return Verdict::new("needs_review", "EXACT_RELEASE_MAPPING_INCOMPLETE");
    }
    // The matcher must accept the release on its own, unless a person
    // chose it: then every file fitting is enough.
    if !candidate.automatic_safe && !identity.manual {
        return Verdict::new("needs_review", "NON_TERMINAL_SAFETY_CONCERN");
    }
    let release = identity.release_mbid.clone().unwrap_or_default();
    let mut unchanged = true;
    for row in tracks {
        let Some(placed) = mapping
            .iter()
            .find(|placed| placed.local_track_id == row.local_track_id)
        else {
            return Verdict::new("needs_review", "EXACT_RELEASE_MAPPING_INCOMPLETE");
        };
        let conflict = row
            .release_mbid
            .as_deref()
            .is_some_and(|value| !value.eq_ignore_ascii_case(&release))
            || row
                .release_track_mbid
                .as_deref()
                .is_some_and(|value| !value.eq_ignore_ascii_case(&placed.release_track_mbid))
            || row
                .recording_mbid
                .as_deref()
                .is_some_and(|value| !same_recording(value, &placed.recording_mbid, aliases));
        if conflict {
            return Verdict::new("needs_review", "CONFLICTING_TRACK_EVIDENCE");
        }
        unchanged &= row.recording_mbid.is_some()
            && row.release_mbid.is_some()
            && row.release_track_mbid.is_some()
            && row.medium_position == Some(i64::from(placed.medium_position))
            && row.release_track_position == Some(i64::from(placed.release_track_position));
    }
    if unchanged {
        return Verdict::new("ready", "EXACT_RELEASE_MAPPINGS_VERIFIED");
    }
    Verdict::new("mapping_ready", "EXACT_RELEASE_MAPPING_SUPPORTED")
        .eligible()
        .with(FindingPayload {
            release_mbid: Some(release),
            mapping,
            ..FindingPayload::default()
        })
}

/// Identity preparation for an album without an exact release: suggest
/// the best edition that places every file, inside the album's release
/// group when it has one. `candidates` come best first.
pub fn readiness_suggest(
    identity: &IdentityState,
    candidates: &[ReidentificationCandidate],
    summary_for: impl Fn(&ReidentificationCandidate) -> EditionSummary,
) -> Verdict {
    let suggestible: Vec<&ReidentificationCandidate> = candidates
        .iter()
        .filter(|candidate| candidate.evidence.release_mbid.is_some())
        .filter(|candidate| same_group(identity, candidate))
        .filter(|candidate| full_mapping(candidate).is_some())
        .collect();
    let Some(winner) = suggestible.first() else {
        return Verdict::new("exact_release_required", "EXACT_EDITION_NOT_ACCEPTED");
    };
    let mut summary = summary_for(winner);
    summary.competing_count = suggestible.len() as u32;
    Verdict::new("exact_release_suggested", "EXACT_EDITION_SUGGESTED")
        .eligible()
        .with(FindingPayload {
            summary: Some(summary),
            candidate: Some((*winner).clone()),
            ..FindingPayload::default()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::identify::models::{CandidateEvidence, TrackEvidence};
    use crate::library::operations::models::CandidateTrack;

    fn candidate(classes: &[EvidenceClass], safe: bool) -> ReidentificationCandidate {
        let evidence = CandidateEvidence {
            candidate_key: "rg:rel".to_owned(),
            release_group_mbid: "rg".to_owned(),
            release_mbid: Some("rel".to_owned()),
            album_title: "Album".to_owned(),
            album_artist_name: "Artist".to_owned(),
            track_evidence: classes
                .iter()
                .enumerate()
                .map(|(index, class)| TrackEvidence {
                    local_track_id: format!("t{index}"),
                    classification: *class,
                    evidence_kinds: Vec::new(),
                    recording_mbid: Some(format!("rec{index}")),
                    release_track_mbid: Some(format!("rt{index}")),
                })
                .collect(),
            score: 0.9,
            reason_code: String::new(),
            distance: 0.1,
            penalties: Vec::new(),
        };
        ReidentificationCandidate {
            candidate_key: "rg:rel".to_owned(),
            automatic_safe: safe,
            evidence,
            artist_mbid: None,
            release_type: None,
            release_date: None,
            local_album_title: "Album".to_owned(),
            local_album_artist_name: "Artist".to_owned(),
            album_title_classification: EvidenceClass::Supported,
            album_artist_classification: EvidenceClass::Supported,
            tracks: (0..classes.len())
                .map(|index| CandidateTrack {
                    local_track_id: format!("t{index}"),
                    title: None,
                    disc_number: Some(1),
                    position: Some(index as u32 + 1),
                })
                .collect(),
            unmatched_expected_tracks: Vec::new(),
        }
    }

    fn automatic() -> IdentityState {
        IdentityState {
            release_group_mbid: Some("rg".to_owned()),
            release_mbid: Some("rel".to_owned()),
            manual: false,
        }
    }

    #[test]
    fn verdicts_follow_v2_rules() {
        use EvidenceClass::*;
        let id = automatic();
        // A whole copy with no supported file, or one contradicting file,
        // is safe to detach; a partial copy never is.
        let none = candidate(&[Unknown, Unknown], false);
        assert_eq!(
            existing_match(&id, Some(&none), 2, 2).finding_code,
            "safe_detach"
        );
        assert_eq!(
            existing_match(&id, Some(&none), 3, 2).finding_code,
            "needs_review"
        );
        let veto = candidate(&[Supported, Contradictory], false);
        assert_eq!(
            existing_match(&id, Some(&veto), 2, 2).reason_code,
            "HARD_CONTRADICTION"
        );
        let good = candidate(&[Supported, Supported], true);
        assert_eq!(existing_match(&id, Some(&good), 2, 2).finding_code, "valid");
        let chosen = IdentityState {
            manual: true,
            ..id.clone()
        };
        assert_eq!(
            existing_match(&chosen, Some(&none), 2, 2).finding_code,
            "manual_identity"
        );

        // Readiness: unplaced files get placed; placed files verify; a file
        // placed elsewhere is a conflict.
        let blank: Vec<TrackState> = (0..2)
            .map(|index| TrackState {
                local_track_id: format!("t{index}"),
                ..TrackState::default()
            })
            .collect();
        let aliases = HashMap::new();
        let verdict = readiness_exact(&id, Some(&good), &blank, &aliases);
        assert_eq!(verdict.finding_code, "mapping_ready");
        assert_eq!(verdict.payload.mapping.len(), 2);
        let placed: Vec<TrackState> = verdict
            .payload
            .mapping
            .iter()
            .map(|m| TrackState {
                local_track_id: m.local_track_id.clone(),
                recording_mbid: Some(m.recording_mbid.clone()),
                release_mbid: Some("rel".to_owned()),
                release_track_mbid: Some(m.release_track_mbid.clone()),
                medium_position: Some(i64::from(m.medium_position)),
                release_track_position: Some(i64::from(m.release_track_position)),
            })
            .collect();
        assert_eq!(
            readiness_exact(&id, Some(&good), &placed, &aliases).finding_code,
            "ready"
        );
        let mut moved = placed.clone();
        moved[0].release_track_mbid = Some("elsewhere".to_owned());
        assert_eq!(
            readiness_exact(&id, Some(&good), &moved, &aliases).reason_code,
            "CONFLICTING_TRACK_EVIDENCE"
        );

        // Suggestions stay inside the album's release group.
        let other_group = IdentityState {
            release_group_mbid: Some("other".to_owned()),
            release_mbid: None,
            manual: true,
        };
        let suggest = |id: &IdentityState| {
            readiness_suggest(id, std::slice::from_ref(&good), |_| {
                EditionSummary::default()
            })
        };
        assert_eq!(suggest(&other_group).finding_code, "exact_release_required");
        let no_identity = IdentityState::default();
        assert_eq!(
            suggest(&no_identity).finding_code,
            "exact_release_suggested"
        );
    }
}
