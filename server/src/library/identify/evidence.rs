//! Between identify's records and the matcher's inputs and outputs.

use std::collections::HashMap;

use super::models::{CandidateEvidence, EvidenceClass, LocalAlbumFacts, TrackEvidence};
use crate::library::matching::decide::{ACCEPT_ALBUM, ACCEPT_TRACK, REVIEW_CEILING};
use crate::library::matching::score::LIBRARY_EXCLUDED;
use crate::library::matching::{EditionHints, LocalAlbum, LocalTrack, Release, ReleaseMatch};

/// The matcher's view of an album: its facts plus the AcoustID
/// recordings heard per track.
pub fn local_album(
    facts: &LocalAlbumFacts,
    fingerprints: &HashMap<String, Vec<String>>,
) -> LocalAlbum {
    LocalAlbum {
        title: facts.title.clone(),
        artist: facts.album_artist_name.clone(),
        year: facts.year,
        is_compilation: facts.is_compilation,
        tracks: facts
            .tracks
            .iter()
            .map(|track| LocalTrack {
                id: track.local_track_id.clone(),
                title: track.title.clone(),
                artist: track.artist_name.clone(),
                track_number: track.track_number,
                disc_number: track.disc_number.max(1),
                duration_secs: track
                    .duration_exact
                    .or(track.duration_secs.map(|seconds| seconds as f64)),
                recording_mbid: non_blank(track.recording_mbid.as_deref()),
                release_track_mbid: non_blank(track.release_track_mbid.as_deref()),
                release_mbid: non_blank(track.release_mbid.as_deref()),
                fingerprint_recordings: fingerprints
                    .get(&track.local_track_id)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect(),
        hints: hints(facts),
    }
}

/// The edition hints most files agree on.
fn hints(facts: &LocalAlbumFacts) -> EditionHints {
    fn common(values: impl Iterator<Item = Option<String>>) -> Option<String> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for value in values.flatten() {
            match counts.iter_mut().find(|(seen, _)| *seen == value) {
                Some((_, count)) => *count += 1,
                None => counts.push((value, 1)),
            }
        }
        counts
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(value, _)| value)
    }
    let tracks = &facts.tracks;
    EditionHints {
        media: common(tracks.iter().map(|track| non_blank(track.media.as_deref()))),
        barcode: common(
            tracks
                .iter()
                .map(|track| track.barcode.as_deref().and_then(EditionHints::barcode_key)),
        ),
        catalog_number: common(tracks.iter().map(|track| {
            track
                .catalog_number
                .as_deref()
                .and_then(EditionHints::catalog_key)
        })),
        country: common(tracks.iter().map(|track| {
            non_blank(track.release_country.as_deref())
                .filter(|code| code.len() == 2)
                .map(|code| code.to_ascii_uppercase())
        })),
        total_discs: common(tracks.iter().map(|track| {
            track
                .total_discs
                .filter(|total| *total >= 2 && *total >= track.disc_number)
                .map(|total| total.to_string())
        }))
        .and_then(|total| total.parse().ok()),
    }
}

fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// One candidate as a curator and the sealing code see it. Paired tracks
/// within the track threshold are supported and carry the release's
/// recording and release-track ids; a vetoed track is contradictory.
pub fn candidate_evidence(
    local: &LocalAlbum,
    release: &Release,
    matched: &ReleaseMatch,
) -> CandidateEvidence {
    let track_evidence = local
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            if let Some(conflict) = matched.conflicts.iter().find(|c| c.local == index) {
                return TrackEvidence {
                    local_track_id: track.id.clone(),
                    classification: EvidenceClass::Contradictory,
                    evidence_kinds: vec![conflict.kind.code().to_owned()],
                    recording_mbid: None,
                    release_track_mbid: None,
                };
            }
            match matched.pair_for(index) {
                Some(pair) => {
                    let candidate = &release.tracks[pair.track];
                    let close = pair.distance <= ACCEPT_TRACK;
                    TrackEvidence {
                        local_track_id: track.id.clone(),
                        classification: if close {
                            EvidenceClass::Supported
                        } else {
                            EvidenceClass::Unknown
                        },
                        evidence_kinds: vec![if close {
                            pair.support.code().to_owned()
                        } else {
                            "distant_pair".to_owned()
                        }],
                        recording_mbid: Some(candidate.recording_id.clone()),
                        release_track_mbid: Some(candidate.id.clone()),
                    }
                }
                None => TrackEvidence {
                    local_track_id: track.id.clone(),
                    classification: EvidenceClass::Unknown,
                    evidence_kinds: vec!["unmatched".to_owned()],
                    recording_mbid: None,
                    release_track_mbid: None,
                },
            }
        })
        .collect();
    let distance = matched.library_distance();
    let reason_code = if !matched.conflicts.is_empty() {
        "CONFLICTING_TRACK_EVIDENCE"
    } else if matched.names_agree
        && distance <= ACCEPT_ALBUM
        && matched.worst_track() <= ACCEPT_TRACK
    {
        "CLOSE_MATCH"
    } else if distance <= REVIEW_CEILING {
        "WEAK_MATCH"
    } else {
        "DISTANT_MATCH"
    };
    CandidateEvidence {
        candidate_key: format!("{}:{}", release.release_group_id, release.id),
        release_group_mbid: release.release_group_id.clone(),
        release_mbid: Some(release.id.clone()),
        album_title: release.title.clone(),
        album_artist_name: release.artist_text(),
        track_evidence,
        score: (1.0 - distance).clamp(0.0, 1.0),
        reason_code: reason_code.to_owned(),
        distance,
        penalties: matched.distance.shares_excluding(LIBRARY_EXCLUDED),
    }
}
