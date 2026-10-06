//! Explicit re-identification: an administrator asks for another look at
//! one album, sees every candidate the matcher scored, and chooses.
//!
//! Unlike a queued identification, nothing seals on its own. The worker
//! recalls candidates (every release the usual recall finds, or only the
//! release the administrator named), scores them with the matching
//! engine, and parks the job as `ready` with the candidates on its
//! snapshot. The administrator then settles it through
//! [`super::decisions::select_candidate`]. A MusicBrainz outage defers the
//! job for two minutes, at most five times, before it fails with a code
//! that can be resumed.

use super::models::{CandidateTrack, Evaluation, ReidentificationCandidate};
use crate::library::identify::models::{EvidenceClass, LocalAlbumFacts, RecallResult};
use crate::library::identify::service::Ranking;
use crate::library::matching::model::VARIOUS_ARTISTS_MBID;
use crate::library::matching::score::NAME_GATE;
use crate::library::matching::strings::{string_dist, strip_edition_words};
use crate::library::matching::{Release, ReleaseMatch};

/// Seconds a deferred re-identification waits before trying again.
pub const RETRY_SECS: f64 = 120.0;
/// Deferrals before an outage fails the job (resumable).
pub const MAX_ATTEMPTS: i64 = 5;
/// Reason on candidates of a release an administrator named: they always
/// need confirmation.
const MANUAL_EXACT: &str = "MANUAL_EXACT_RELEASE_REQUEST";

/// The evaluation an administrator reviews: every scored candidate, the
/// automatic verdict, and which candidate (if any) would have sealed on
/// its own. `exact_request` marks a release the administrator named.
pub fn evaluation(
    facts: &LocalAlbumFacts,
    recall: &RecallResult,
    ranking: &Ranking,
    exact_request: bool,
) -> Evaluation {
    let mut candidates: Vec<ReidentificationCandidate> = ranking
        .order
        .iter()
        .zip(&ranking.candidates)
        .enumerate()
        .filter_map(|(rank, (&index, evidence))| {
            let release = recall.releases.get(index)?;
            let matched = ranking.matches.get(index)?;
            let automatic_safe = !exact_request && rank == 0 && ranking.lead_identified;
            let mut evidence = evidence.clone();
            if automatic_safe {
                evidence.reason_code = "SUPPORTED".to_owned();
            } else if exact_request && evidence.contradictory_count() == 0 {
                evidence.reason_code = MANUAL_EXACT.to_owned();
            }
            Some(candidate(facts, release, matched, evidence, automatic_safe))
        })
        .collect();
    // v2's order: what would seal on its own, then the best score.
    candidates.sort_by(|a, b| {
        b.automatic_safe
            .cmp(&a.automatic_safe)
            .then_with(|| b.evidence.score.total_cmp(&a.evidence.score))
            .then_with(|| a.candidate_key.cmp(&b.candidate_key))
    });
    let (outcome, reason_code) = if exact_request && !candidates.is_empty() {
        ("contradictory".to_owned(), MANUAL_EXACT.to_owned())
    } else {
        (snake(&ranking.outcome), ranking.reason_code.clone())
    };
    Evaluation {
        outcome,
        reason_code,
        candidates,
        selected_candidate_key: None,
        custom_manifest_id: None,
    }
}

fn candidate(
    facts: &LocalAlbumFacts,
    release: &Release,
    matched: &ReleaseMatch,
    evidence: crate::library::identify::models::CandidateEvidence,
    automatic_safe: bool,
) -> ReidentificationCandidate {
    let tracks = facts
        .tracks
        .iter()
        .enumerate()
        .map(|(index, local)| {
            let placed = matched
                .pair_for(index)
                .and_then(|pair| release.tracks.get(pair.track));
            CandidateTrack {
                local_track_id: local.local_track_id.clone(),
                title: placed.map(|track| track.title.clone()),
                disc_number: placed.map(|track| track.disc),
                position: placed.map(|track| track.position),
            }
        })
        .collect();
    let various = facts.is_compilation || release.is_various_artists();
    ReidentificationCandidate {
        candidate_key: evidence.candidate_key.clone(),
        automatic_safe,
        artist_mbid: release
            .artists
            .first()
            .map(|artist| artist.id.clone())
            .filter(|id| !id.eq_ignore_ascii_case(VARIOUS_ARTISTS_MBID)),
        release_type: release.primary_type.clone(),
        release_date: release.date.clone(),
        local_album_title: facts.title.clone(),
        local_album_artist_name: facts.album_artist_name.clone(),
        album_title_classification: name_class(
            &strip_edition_words(&facts.title),
            &strip_edition_words(&release.title),
        ),
        album_artist_classification: if various {
            EvidenceClass::Unknown
        } else {
            name_class(&facts.album_artist_name, &release.artist_text())
        },
        tracks,
        unmatched_expected_tracks: matched
            .missing
            .iter()
            .filter_map(|index| release.tracks.get(*index))
            .map(|track| track.title.clone())
            .collect(),
        evidence,
    }
}

/// v2's album name gate as a verdict: within the gate supports the
/// candidate, beyond it contradicts, and a blank local name says nothing.
pub fn name_class(local: &str, release: &str) -> EvidenceClass {
    if local.trim().is_empty() {
        EvidenceClass::Unknown
    } else if string_dist(local, release) <= NAME_GATE {
        EvidenceClass::Supported
    } else {
        EvidenceClass::Contradictory
    }
}

fn snake<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|json| json.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The evaluation recorded when MusicBrainz stayed down past the retry
/// budget. The job fails with a code an administrator can resume.
pub fn unavailable() -> Evaluation {
    Evaluation {
        outcome: "provider_deferred".to_owned(),
        reason_code: super::control::RESUMABLE_FAILURE.to_owned(),
        ..Evaluation::default()
    }
}
