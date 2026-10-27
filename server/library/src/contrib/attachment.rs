//! Whether a verified release fits the album a contribution describes:
//! the library matcher's pairing, distances and gates, run over the
//! contribution's own view of the album's files.

use std::collections::HashMap;

use futures_util::future::BoxFuture;

use super::models::{ContributionRecord, MusicBrainzVerifiedRelease, ReleaseTrackDraft};
use super::seams::{
    AttachmentCandidate, AttachmentDecision, AttachmentEvidence, AttachmentOutcome, TrackEvidence,
};
use crate::matching::decide::{ACCEPT_ALBUM, ACCEPT_TRACK};
use crate::matching::{
    CreditedArtist, LocalAlbum, LocalTrack, Release, ReleaseMedium, ReleaseTrack, match_release,
};

/// Attachment evidence over the library matcher: the album's files, as
/// the contribution describes them (draft titles where the curator edited,
/// snapshot positions and lengths, the files' recording MBIDs), are scored
/// against the verified release's tracklist with the same pairing,
/// distances and title, artist and length gates identification uses.
///
/// v2 decided attachments with its evidence engine minus the lone-release
/// quorum (the curator named this exact release), so this does the same:
/// accept only a clean fit, and send everything else to review with a
/// catalogued reason. An accepted decision carries the file-to-track
/// pairing, which the link writes as track identities.
pub struct MatchingAttachmentEvidence;

impl AttachmentEvidence for MatchingAttachmentEvidence {
    fn matcher_version(&self) -> String {
        "v3-matching-1".to_owned()
    }

    fn decide_attachment<'a>(
        &'a self,
        contribution: &'a ContributionRecord,
        verified: &'a MusicBrainzVerifiedRelease,
        recording_mbids: &'a HashMap<String, Option<String>>,
        _relative_paths: &'a HashMap<String, String>,
    ) -> BoxFuture<'a, AttachmentDecision> {
        let decision = attachment_decision(contribution, verified, recording_mbids);
        Box::pin(async move { decision })
    }
}

fn attachment_decision(
    contribution: &ContributionRecord,
    verified: &MusicBrainzVerifiedRelease,
    recording_mbids: &HashMap<String, Option<String>>,
) -> AttachmentDecision {
    let candidate = AttachmentCandidate {
        release_group_mbid: verified.release_group_mbid.clone(),
        release_mbid: Some(verified.release_mbid.clone()),
        artist_mbid: verified.artist_mbid.clone(),
    };
    let review = |code: &str, candidate: AttachmentCandidate| AttachmentDecision {
        outcome: AttachmentOutcome::NeedsReview,
        reason_code: Some(code.to_owned()),
        selected_candidate_key: None,
        candidates: vec![candidate],
        tracks: Vec::new(),
    };
    if verified.release_mbid.trim().is_empty() {
        return review("VERIFIED_MBID_MISSING", candidate);
    }
    let local = local_album(contribution, recording_mbids);
    let release = matching_release(verified);
    let matched = match_release(&local, &release, &HashMap::new());
    if !matched.conflicts.is_empty() {
        return review("ATTACHMENT_CONTRADICTION", candidate);
    }
    if !matched.names_agree {
        return review("ATTACHMENT_NAME_MISMATCH", candidate);
    }
    let unmatched_limit = if local.tracks.len() <= 20 { 1 } else { 2 };
    if matched.pairs.is_empty()
        || matched.library_distance() > ACCEPT_ALBUM
        || matched.worst_track() > ACCEPT_TRACK
        || matched.unmatched.len() > unmatched_limit
    {
        return review("ATTACHMENT_WEAK_FIT", candidate);
    }
    let tracks = matched
        .pairs
        .iter()
        .filter_map(|pair| {
            let file = local.tracks.get(pair.local)?;
            let track = release.tracks.get(pair.track)?;
            (!track.recording_id.is_empty()).then(|| TrackEvidence {
                local_track_id: file.id.clone(),
                recording_mbid: track.recording_id.clone(),
                release_track_mbid: Some(track.id.clone()).filter(|id| !id.is_empty()),
                medium_position: Some(i64::from(track.disc)),
                track_position: Some(i64::from(track.position)),
            })
        })
        .collect();
    AttachmentDecision {
        outcome: AttachmentOutcome::Identified,
        reason_code: None,
        selected_candidate_key: Some(candidate.key()),
        candidates: vec![candidate],
        tracks,
    }
}

/// The album as the contribution describes it (v2 `_attachment_evidence`):
/// draft values where present, the snapshot otherwise.
fn local_album(
    contribution: &ContributionRecord,
    recording_mbids: &HashMap<String, Option<String>>,
) -> LocalAlbum {
    let draft_tracks: HashMap<&str, &ReleaseTrackDraft> = contribution
        .draft
        .media
        .iter()
        .flat_map(|medium| medium.tracks.iter())
        .map(|track| (track.local_track_id.as_str(), track))
        .collect();
    let text = |value: Option<&String>, fallback: &str| {
        value
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or(fallback)
            .to_owned()
    };
    let snapshot = &contribution.local_snapshot;
    let tracks = snapshot
        .media
        .iter()
        .flat_map(|medium| medium.tracks.iter())
        .map(|track| {
            let draft = draft_tracks.get(track.local_track_id.as_str());
            LocalTrack {
                id: track.local_track_id.clone(),
                title: text(draft.and_then(|d| d.title.value.as_ref()), &track.title),
                artist: text(
                    draft.and_then(|d| d.artist_name.value.as_ref()),
                    track.artist_name.as_deref().unwrap_or(""),
                ),
                track_number: u32::try_from(track.track_number).unwrap_or(0),
                disc_number: u32::try_from(track.disc_number.max(1)).unwrap_or(1),
                duration_secs: track.duration_seconds.filter(|_| track.duration_reliable),
                recording_mbid: recording_mbids
                    .get(&track.local_track_id)
                    .cloned()
                    .flatten(),
                ..LocalTrack::default()
            }
        })
        .collect();
    LocalAlbum {
        title: text(contribution.draft.title.value.as_ref(), &snapshot.title),
        artist: text(
            contribution.draft.artist_credit.value.as_ref(),
            &snapshot.album_artist_name,
        ),
        year: snapshot.year,
        is_compilation: snapshot.is_compilation,
        tracks,
        ..LocalAlbum::default()
    }
}

/// The verified release in the matcher's shape.
fn matching_release(verified: &MusicBrainzVerifiedRelease) -> Release {
    let mut media: Vec<ReleaseMedium> = Vec::new();
    for track in &verified.tracks {
        let disc = u32::try_from(track.disc_number.max(1)).unwrap_or(1);
        match media.iter_mut().find(|medium| medium.position == disc) {
            Some(medium) => medium.track_count += 1,
            None => media.push(ReleaseMedium {
                position: disc,
                track_count: 1,
                ..ReleaseMedium::default()
            }),
        }
    }
    Release {
        id: verified.release_mbid.clone(),
        release_group_id: verified.release_group_mbid.clone(),
        title: verified.title.clone(),
        artists: vec![CreditedArtist {
            id: verified.artist_mbid.clone().unwrap_or_default(),
            name: verified.artist_name.clone(),
            sort_name: None,
            join: String::new(),
        }],
        date: verified.date.clone(),
        country: verified.country.clone(),
        status: verified.status.clone(),
        barcode: verified.barcode.clone(),
        media,
        tracks: verified
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| ReleaseTrack {
                id: track.release_track_mbid.clone().unwrap_or_default(),
                recording_id: track.recording_mbid.clone().unwrap_or_default(),
                title: track.title.clone(),
                artists: Vec::new(),
                disc: u32::try_from(track.disc_number.max(1)).unwrap_or(1),
                position: u32::try_from(track.position.max(0)).unwrap_or(0),
                absolute_position: u32::try_from(index + 1).unwrap_or(u32::MAX),
                length_ms: track
                    .duration_seconds
                    .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                    .map(|seconds| (seconds * 1000.0).round() as u64),
                isrcs: Vec::new(),
                genres: Vec::new(),
            })
            .collect(),
        ..Release::default()
    }
}
