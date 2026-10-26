//! Between identify's records and the matcher's inputs and outputs.

use std::collections::HashMap;

use super::models::{
    AlbumIdentity, CandidateEvidence, EvidenceClass, LocalAlbumFacts, TrackEvidence,
};
use crate::matching::decide::{ACCEPT_ALBUM, ACCEPT_TRACK, REVIEW_CEILING};
use crate::matching::score::{FINGERPRINT_OVERRULED, LIBRARY_EXCLUDED};
use crate::matching::{EditionHints, LocalAlbum, LocalTrack, Release, ReleaseMatch};
use droppedneedle_providers::musicbrainz::is_valid_mbid;

/// The release group the files name, lowercase: the one every indexed
/// file's tag names, else the group of the album's revisable match when
/// it has no release yet. A file without a valid group id, or files
/// naming two groups, name none. A person's match is never read here: an
/// automatic pass does not look at an album a person decided.
pub fn named_group(facts: &LocalAlbumFacts, current: Option<&AlbumIdentity>) -> Option<String> {
    let tagged: Option<Vec<String>> = facts
        .tracks
        .iter()
        .map(|track| valid_group(track.release_group_mbid.as_deref()))
        .collect();
    if let Some(tagged) = tagged
        && let Some(first) = tagged.first()
        && tagged.iter().all(|group| group == first)
    {
        return Some(first.clone());
    }
    current
        .filter(|row| {
            row.decision_source.is_revisable() && non_blank(row.release_mbid.as_deref()).is_none()
        })
        .and_then(|row| valid_group(row.release_group_mbid.as_deref()))
}

fn valid_group(value: Option<&str>) -> Option<String> {
    let group = value?.trim().to_ascii_lowercase();
    is_valid_mbid(&group).then_some(group)
}

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
    let files: Vec<EditionHints> = facts
        .tracks
        .iter()
        .map(|track| {
            EditionHints::of_file(
                track.media.as_deref(),
                track.barcode.as_deref(),
                track.catalog_number.as_deref(),
                track.release_country.as_deref(),
                track.total_discs,
                track.disc_number,
            )
        })
        .collect();
    EditionHints {
        media: common(files.iter().map(|file| file.media.clone())),
        barcode: common(files.iter().map(|file| file.barcode.clone())),
        catalog_number: common(files.iter().map(|file| file.catalog_number.clone())),
        country: common(files.iter().map(|file| file.country.clone())),
        total_discs: common(
            files
                .iter()
                .map(|file| file.total_discs.map(|total| total.to_string())),
        )
        .and_then(|total| total.parse().ok()),
    }
}

/// The barcode and catalog number keys more than half of the album's
/// indexed files carry; the other hints are left empty. A value on half
/// the files or fewer names no edition: it can be a stray copy.
pub fn edition_ids(facts: &LocalAlbumFacts) -> EditionHints {
    edition_ids_of(
        facts
            .tracks
            .iter()
            .map(|track| (track.barcode.as_deref(), track.catalog_number.as_deref())),
    )
}

/// [`edition_ids`] over each indexed file's raw barcode and catalog number.
pub fn edition_ids_of<'a>(
    files: impl IntoIterator<Item = (Option<&'a str>, Option<&'a str>)>,
) -> EditionHints {
    let files: Vec<(Option<String>, Option<String>)> = files
        .into_iter()
        .map(|(barcode, catalog)| {
            (
                barcode.and_then(EditionHints::barcode_key),
                catalog.and_then(EditionHints::catalog_key),
            )
        })
        .collect();
    let majority = |values: Vec<&String>| -> Option<String> {
        let mut counts: Vec<(&String, usize)> = Vec::new();
        for value in values {
            match counts.iter_mut().find(|(seen, _)| *seen == value) {
                Some((_, count)) => *count += 1,
                None => counts.push((value, 1)),
            }
        }
        counts
            .into_iter()
            .find(|(_, count)| count * 2 > files.len())
            .map(|(value, _)| value.clone())
    };
    EditionHints {
        barcode: majority(files.iter().filter_map(|file| file.0.as_ref()).collect()),
        catalog_number: majority(files.iter().filter_map(|file| file.1.as_ref()).collect()),
        ..EditionHints::default()
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
/// recording and release-track ids; a vetoed track is contradictory. A
/// file whose length overruled its fingerprint says so.
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
                    let mut evidence_kinds = vec![if close {
                        pair.support.code().to_owned()
                    } else {
                        "distant_pair".to_owned()
                    }];
                    if matched.overruled.contains(&index) {
                        evidence_kinds.push(FINGERPRINT_OVERRULED.to_owned());
                    }
                    TrackEvidence {
                        local_track_id: track.id.clone(),
                        classification: if close {
                            EvidenceClass::Supported
                        } else {
                            EvidenceClass::Unknown
                        },
                        evidence_kinds,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identify::models::{DecisionSource, LocalTrackFacts};

    const G: &str = "11111111-1111-1111-1111-111111111111";
    const H: &str = "22222222-2222-2222-2222-222222222222";

    fn facts(groups: &[Option<&str>]) -> LocalAlbumFacts {
        LocalAlbumFacts {
            tracks: groups
                .iter()
                .map(|group| LocalTrackFacts {
                    release_group_mbid: group.map(str::to_owned),
                    ..LocalTrackFacts::default()
                })
                .collect(),
            ..LocalAlbumFacts::default()
        }
    }

    fn row(group: &str, release: Option<&str>, source: DecisionSource) -> AlbumIdentity {
        AlbumIdentity {
            local_album_id: "album".to_owned(),
            provider: "musicbrainz".to_owned(),
            release_group_mbid: Some(group.to_owned()),
            release_mbid: release.map(str::to_owned),
            decision_source: source,
            row_revision: 1,
        }
    }

    #[test]
    fn the_group_the_files_name() {
        let upper = G.to_ascii_uppercase();
        let auto = DecisionSource::Automatic;
        let cases: Vec<(&str, LocalAlbumFacts, Option<AlbumIdentity>, Option<&str>)> = vec![
            (
                "every file names G",
                facts(&[Some(G), Some(&upper)]),
                None,
                Some(G),
            ),
            ("files name G and H", facts(&[Some(G), Some(H)]), None, None),
            (
                "one file names nothing",
                facts(&[Some(G), None]),
                None,
                None,
            ),
            (
                "one file is blank",
                facts(&[Some(G), Some(" ")]),
                None,
                None,
            ),
            (
                "one file is malformed",
                facts(&[Some(G), Some("G")]),
                None,
                None,
            ),
            (
                "a revisable group-only match",
                facts(&[None]),
                Some(row(H, None, DecisionSource::Embedded)),
                Some(H),
            ),
            (
                "an exact match names none",
                facts(&[None]),
                Some(row(H, Some("release"), auto)),
                None,
            ),
            (
                "a person's group-only match names none",
                facts(&[None]),
                Some(row(H, None, DecisionSource::Manual)),
                None,
            ),
            (
                "a malformed group-only match names none",
                facts(&[None]),
                Some(row("not-an-id", None, auto)),
                None,
            ),
            (
                "the tags come before the match",
                facts(&[Some(G)]),
                Some(row(H, None, auto)),
                Some(G),
            ),
        ];
        let wrong: Vec<String> = cases
            .into_iter()
            .filter_map(|(name, facts, current, expected)| {
                let got = named_group(&facts, current.as_ref());
                (got.as_deref() != expected).then(|| format!("{name}: got {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }
}
