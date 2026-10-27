//! The verdict on one album, from what the matcher made of its files
//! against MusicBrainz. Pure: the service gathers the facts, this decides.
//!
//! Ported from v2's `IdentityRepairService._classify` (identity repair)
//! and `_classify_management_readiness` (identity preparation), on top of
//! v3's matching engine: a candidate's per-file evidence is the matcher's,
//! and "the matcher would accept it" is the candidate's `automatic_safe`.

use std::collections::{HashMap, HashSet};

use super::models::{EditionSummary, FindingCode, FindingPayload, Mapping, Verdict};
use crate::identify::models::EvidenceClass;
use crate::operations::models::ReidentificationCandidate;

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
        return Verdict::new(FindingCode::ManualIdentity, "MANUAL_IDENTITY_REPORT_ONLY");
    }
    let Some(candidate) = candidate else {
        return Verdict::new(FindingCode::Unverifiable, "EVIDENCE_UNAVAILABLE");
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
        return Verdict::new(FindingCode::SafeDetach, reason).eligible();
    }
    if !same_group(identity, candidate) {
        return Verdict::new(FindingCode::NeedsReview, "SELECTED_RELEASE_CONFLICT");
    }
    if candidate.automatic_safe {
        Verdict::new(FindingCode::Valid, "CURRENT_IDENTITY_PASSES")
    } else {
        Verdict::new(FindingCode::NeedsReview, "NON_TERMINAL_SAFETY_CONCERN")
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
        return Verdict::new(FindingCode::NeedsReview, "SELECTED_RELEASE_UNAVAILABLE");
    };
    if !same_group(identity, candidate) {
        return Verdict::new(FindingCode::NeedsReview, "SELECTED_RELEASE_CONFLICT");
    }
    let Some(mapping) = full_mapping(candidate) else {
        return Verdict::new(FindingCode::NeedsReview, "EXACT_RELEASE_MAPPING_INCOMPLETE");
    };
    if mapping.len() != tracks.len() {
        return Verdict::new(FindingCode::NeedsReview, "EXACT_RELEASE_MAPPING_INCOMPLETE");
    }
    // The matcher must accept the release on its own, unless a person
    // chose it: then every file fitting is enough.
    if !candidate.automatic_safe && !identity.manual {
        return Verdict::new(FindingCode::NeedsReview, "NON_TERMINAL_SAFETY_CONCERN");
    }
    let release = identity.release_mbid.clone().unwrap_or_default();
    let mut unchanged = true;
    for row in tracks {
        let Some(placed) = mapping
            .iter()
            .find(|placed| placed.local_track_id == row.local_track_id)
        else {
            return Verdict::new(FindingCode::NeedsReview, "EXACT_RELEASE_MAPPING_INCOMPLETE");
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
            return Verdict::new(FindingCode::NeedsReview, "CONFLICTING_TRACK_EVIDENCE");
        }
        unchanged &= row.recording_mbid.is_some()
            && row.release_mbid.is_some()
            && row.release_track_mbid.is_some()
            && row.medium_position == Some(i64::from(placed.medium_position))
            && row.release_track_position == Some(i64::from(placed.release_track_position));
    }
    if unchanged {
        return Verdict::new(FindingCode::Ready, "EXACT_RELEASE_MAPPINGS_VERIFIED");
    }
    Verdict::new(FindingCode::MappingReady, "EXACT_RELEASE_MAPPING_SUPPORTED")
        .eligible()
        .with(FindingPayload {
            release_mbid: Some(release),
            mapping,
            ..FindingPayload::default()
        })
}

/// Top score an edition needs to be accepted without a person (v2's).
pub const MIN_AUTO_ACCEPT_SCORE: f64 = 0.95;
/// Lead over the next edition an automatic acceptance needs (v2's).
pub const AUTO_ACCEPT_MARGIN: f64 = 0.05;

/// Per-candidate reasons that count as fully supported: the matcher's
/// lead (`SUPPORTED`) and any edition within its acceptance thresholds
/// whose names agree (`CLOSE_MATCH`). v2 accepted `SUPPORTED` and
/// `SUPPORTED_EMBEDDED_IDS`.
const QUALIFYING_REASONS: [&str; 2] = ["SUPPORTED", "CLOSE_MATCH"];

/// v2's automatic acceptance gate over the suggestible editions, best
/// first: the best scores at least 0.95 and is alone, or beats the next
/// by 0.05 without tying any other on score, status, date and country.
/// As in v2, every suggestible edition must also carry a fully supported
/// reason, and the best must be one the matcher would accept on its own.
/// Returns `AUTO_ACCEPT` or the reason the suggestion waits for a person.
pub fn auto_gate(ranked: &[(&ReidentificationCandidate, EditionSummary)]) -> &'static str {
    let Some((top, top_summary)) = ranked.first() else {
        return "EMPTY";
    };
    let score = top.evidence.score;
    if score < MIN_AUTO_ACCEPT_SCORE {
        return "BELOW_MIN_SCORE";
    }
    let key = |candidate: &ReidentificationCandidate, summary: &EditionSummary| {
        (
            candidate.evidence.score.to_bits(),
            summary.status.clone(),
            summary.date.clone(),
            summary.country.clone(),
        )
    };
    let top_key = key(top, top_summary);
    if ranked[1..]
        .iter()
        .any(|(other, summary)| key(other, summary) == top_key)
    {
        return "TIE";
    }
    if let Some((next, _)) = ranked.get(1)
        && score - next.evidence.score < AUTO_ACCEPT_MARGIN
    {
        return "MARGIN_TOO_NARROW";
    }
    let qualifies = ranked.iter().all(|(candidate, _)| {
        QUALIFYING_REASONS.contains(&candidate.evidence.reason_code.as_str())
    });
    if !qualifies || !top.automatic_safe {
        return "NON_QUALIFYING_REASON";
    }
    "AUTO_ACCEPT"
}

/// Identity preparation for an album without an exact release: suggest
/// the best edition that places every file, inside the album's release
/// group when it has one. `candidates` come best first. With
/// `auto_accept_enabled` the suggestion goes through the automatic
/// acceptance gate; an album someone chose is never accepted
/// automatically.
pub fn readiness_suggest(
    identity: &IdentityState,
    candidates: &[ReidentificationCandidate],
    auto_accept_enabled: bool,
    summary_for: impl Fn(&ReidentificationCandidate) -> EditionSummary,
) -> Verdict {
    let ranked: Vec<(&ReidentificationCandidate, EditionSummary)> = candidates
        .iter()
        .filter(|candidate| candidate.evidence.release_mbid.is_some())
        .filter(|candidate| same_group(identity, candidate))
        .filter(|candidate| full_mapping(candidate).is_some())
        .map(|candidate| (candidate, summary_for(candidate)))
        .collect();
    let Some((winner, summary)) = ranked.first() else {
        return Verdict::new(
            FindingCode::ExactReleaseRequired,
            "EXACT_EDITION_NOT_ACCEPTED",
        );
    };
    let mut summary = summary.clone();
    summary.competing_count = ranked.len() as u32;
    let gate = (auto_accept_enabled && !identity.manual).then(|| auto_gate(&ranked));
    summary.auto_gate = gate.map(str::to_owned);
    Verdict::new(
        FindingCode::ExactReleaseSuggested,
        "EXACT_EDITION_SUGGESTED",
    )
    .eligible()
    .with(FindingPayload {
        summary: Some(summary),
        candidate: Some((*winner).clone()),
        auto_accept: gate == Some("AUTO_ACCEPT"),
        ..FindingPayload::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identify::models::{CandidateEvidence, TrackEvidence};
    use crate::operations::models::CandidateTrack;

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
            FindingCode::SafeDetach
        );
        assert_eq!(
            existing_match(&id, Some(&none), 3, 2).finding_code,
            FindingCode::NeedsReview
        );
        let veto = candidate(&[Supported, Contradictory], false);
        assert_eq!(
            existing_match(&id, Some(&veto), 2, 2).reason_code,
            "HARD_CONTRADICTION"
        );
        let good = candidate(&[Supported, Supported], true);
        assert_eq!(
            existing_match(&id, Some(&good), 2, 2).finding_code,
            FindingCode::Valid
        );
        let chosen = IdentityState {
            manual: true,
            ..id.clone()
        };
        assert_eq!(
            existing_match(&chosen, Some(&none), 2, 2).finding_code,
            FindingCode::ManualIdentity
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
        assert_eq!(verdict.finding_code, FindingCode::MappingReady);
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
            FindingCode::Ready
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
            readiness_suggest(id, std::slice::from_ref(&good), true, |_| {
                EditionSummary::default()
            })
        };
        // The automatic gate: a lone safe edition at 0.95 or more passes;
        // a close runner-up, a tie, or an album someone chose never does.
        let mut best = good.clone();
        best.evidence.score = 0.97;
        best.evidence.reason_code = "SUPPORTED".to_owned();
        let blank = EditionSummary::default;
        assert_eq!(auto_gate(&[(&best, blank())]), "AUTO_ACCEPT");
        let mut close = best.clone();
        close.automatic_safe = false;
        close.evidence.score = 0.95;
        close.evidence.reason_code = "CLOSE_MATCH".to_owned();
        assert_eq!(
            auto_gate(&[(&best, blank()), (&close, blank())]),
            "MARGIN_TOO_NARROW"
        );
        assert_eq!(auto_gate(&[(&best, blank()), (&best, blank())]), "TIE");
        // Every suggestible edition must be fully supported, not only the lead.
        let mut weak = close.clone();
        weak.evidence.score = 0.5;
        weak.evidence.reason_code = "WEAK_MATCH".to_owned();
        assert_eq!(
            auto_gate(&[(&best, blank()), (&weak, blank())]),
            "NON_QUALIFYING_REASON"
        );
        let chosen = IdentityState {
            manual: true,
            ..IdentityState::default()
        };
        let held = readiness_suggest(&chosen, std::slice::from_ref(&best), true, |_| blank());
        assert!(!held.payload.auto_accept);
        let open = readiness_suggest(
            &IdentityState::default(),
            std::slice::from_ref(&best),
            true,
            |_| blank(),
        );
        assert!(open.payload.auto_accept);
        assert_eq!(
            suggest(&other_group).finding_code,
            FindingCode::ExactReleaseRequired
        );
        let no_identity = IdentityState::default();
        assert_eq!(
            suggest(&no_identity).finding_code,
            FindingCode::ExactReleaseSuggested
        );
    }
}
