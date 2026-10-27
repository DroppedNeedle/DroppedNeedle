//! What converting an album to one edition would do, worked out before
//! anything changes.
//!
//! On the album's current edition, a file whose stored identity names a
//! track of it keeps that track first (a person's match by hand before any
//! other), so the plan shows what automatic management writes from. The
//! rest are lined up against the edition's tracklist with the same
//! placement a choice writes ([`super::choice::place`]). Other editions
//! are lined up by that placement alone. Each file keeps its place on an
//! edition track, is a second copy of a
//! song the edition already has a file for, is another version of a track
//! no file took (same title and artist, but the matcher would not place
//! it), or is an extra the edition has no track for. Edition tracks no
//! file took are missing, another version or not; "get missing tracks"
//! fetches exactly those. Choosing never deletes a file: duplicates, other
//! versions and extras stay in the album, unplaced, for the person to keep
//! or remove.

use std::collections::HashSet;

use super::choice::place;
use super::models::ReidentificationCandidate;
use super::reasons::{Reason, TRACK_ID_CORRECTED, TRACK_ID_ELSEWHERE};
use crate::identify::models::{LocalAlbumFacts, LocalTrackFacts};
use crate::matching::model::{Release, credit_text};
use crate::matching::score::{ConflictKind, Support};
use crate::matching::strings::fold;

/// What happens to one file when the album takes the edition. Choosing
/// never deletes a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = EditionFileFate)]
pub enum FileFate {
    /// The file sits on its own track of the edition.
    Keep,
    /// The edition has this song, but another file already holds its track.
    Duplicate,
    /// Another version of an edition track no file holds: the same title
    /// and artist, but not placed on it (a different length or recording).
    OtherVersion,
    /// The edition has no track for this file.
    Extra,
}

impl FileFate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Duplicate => "duplicate",
            Self::OtherVersion => "other_version",
            Self::Extra => "extra",
        }
    }
}

/// One track of the edition and the file that holds it, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanTrack {
    pub disc: u32,
    pub position: u32,
    pub title: String,
    pub recording_mbid: String,
    pub release_track_mbid: String,
    pub length_ms: Option<u64>,
    /// The file on this track; `None` means the track is missing.
    pub local_track_id: Option<String>,
}

/// One of the album's files and what the edition makes of it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanFile {
    pub local_track_id: String,
    pub title: String,
    pub disc: u32,
    pub track: u32,
    /// The file's length, when the scan read it.
    pub length_ms: Option<u64>,
    pub fate: FileFate,
    /// The edition track (disc, position) the file keeps, duplicates or is
    /// another version of.
    pub edition_track: Option<(u32, u32)>,
    /// Why the file is where it is, when its tags need a word: a track id
    /// from another edition, set aside or in the way.
    pub note: Option<Reason>,
}

/// What MusicBrainz says about the planned edition.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EditionFacts {
    pub title: String,
    pub disambiguation: Option<String>,
    pub date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    /// One format per medium that names one, in medium order.
    pub formats: Vec<String>,
    pub disc_count: u32,
    pub track_count: u32,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
    pub barcode: Option<String>,
}

impl EditionFacts {
    fn of(release: &Release) -> Self {
        let discs = release
            .tracks
            .iter()
            .map(|track| track.disc)
            .max()
            .unwrap_or(0);
        Self {
            title: release.title.clone(),
            disambiguation: release.disambiguation.clone(),
            date: release.date.clone(),
            country: release.country.clone(),
            status: release.status.clone(),
            formats: release
                .media
                .iter()
                .filter_map(|medium| medium.format.clone())
                .collect(),
            disc_count: u32::try_from(release.media.len())
                .unwrap_or(u32::MAX)
                .max(discs),
            track_count: u32::try_from(release.tracks.len()).unwrap_or(u32::MAX),
            label: release.labels.first().cloned(),
            catalog_number: release.catalog_numbers.first().cloned(),
            barcode: release.barcode.clone(),
        }
    }
}

/// A file of the album whose stored identity names a release track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlacement {
    pub local_track_id: String,
    pub release_mbid: Option<String>,
    pub release_track_mbid: String,
    /// A person matched the file to this track by hand, and the match
    /// stands.
    pub by_hand: bool,
    /// When that match was made, in milliseconds; set only with `by_hand`.
    pub matched_at: Option<i64>,
}

/// The plan as a person reads it: the plan, the album identity revision
/// it was read at, the files a person matched by hand, and those of them
/// the app has written since the match (their "Undo match" waits).
#[derive(Debug, Clone, PartialEq)]
pub struct PlanReading {
    pub plan: EditionPlan,
    pub identity_revision: i64,
    pub matched_by_hand: Vec<String>,
    pub match_written: Vec<String>,
}

/// One file on one edition track: the index into `release.tracks`, the
/// (disc, position) it keeps, and whether a person matched it by hand.
pub(super) struct Seat {
    pub track: usize,
    pub local_track_id: String,
    pub spot: (u32, u32),
    pub by_hand: bool,
}

/// Seat the album's indexed files on `release`: the stored placements
/// first (by hand before the rest, then by file id), each skipped when the
/// release has no such track or another file took it; then the matcher
/// places the files left on the tracks still free.
pub(super) fn seat(
    release: &Release,
    candidate: &ReidentificationCandidate,
    indexed: &[String],
    stored: &[StoredPlacement],
) -> Vec<Seat> {
    let mut seats: Vec<Seat> = Vec::new();
    let mut ordered: Vec<&StoredPlacement> = stored
        .iter()
        .filter(|row| indexed.contains(&row.local_track_id))
        .collect();
    ordered.sort_by(|a, b| {
        b.by_hand
            .cmp(&a.by_hand)
            .then_with(|| a.local_track_id.cmp(&b.local_track_id))
    });
    for row in ordered {
        let Some(index) = release
            .tracks
            .iter()
            .position(|track| track.id.eq_ignore_ascii_case(&row.release_track_mbid))
        else {
            continue;
        };
        if seats
            .iter()
            .any(|seat| seat.track == index || seat.local_track_id == row.local_track_id)
        {
            continue;
        }
        let track = &release.tracks[index];
        seats.push(Seat {
            track: index,
            local_track_id: row.local_track_id.clone(),
            spot: (track.disc, track.position),
            by_hand: row.by_hand,
        });
    }
    let free: Vec<String> = indexed
        .iter()
        .filter(|file| !seats.iter().any(|seat| seat.local_track_id == **file))
        .cloned()
        .collect();
    let (placed, _) = place(&free, candidate);
    for placement in placed {
        let Some(index) = release
            .tracks
            .iter()
            .position(|track| track.id.eq_ignore_ascii_case(&placement.release_track_mbid))
        else {
            continue;
        };
        if seats.iter().any(|seat| seat.track == index) {
            continue;
        }
        seats.push(Seat {
            track: index,
            local_track_id: placement.local_track_id,
            spot: (placement.disc, placement.position),
            by_hand: false,
        });
    }
    seats
}

/// The whole plan for one album and one edition.
#[derive(Debug, Clone, PartialEq)]
pub struct EditionPlan {
    pub local_album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: String,
    pub title: String,
    pub artist_name: String,
    /// The edition is already the album's edition.
    pub is_current: bool,
    /// The edition belongs to the album's current release group (false
    /// for an album with none).
    pub same_album: bool,
    pub edition: EditionFacts,
    pub tracks: Vec<PlanTrack>,
    pub files: Vec<PlanFile>,
}

impl EditionPlan {
    /// Edition tracks no file holds.
    pub fn missing(&self) -> impl Iterator<Item = &PlanTrack> {
        self.tracks
            .iter()
            .filter(|track| track.local_track_id.is_none())
    }

    /// Files that keep a place on the edition.
    pub fn kept(&self) -> impl Iterator<Item = &PlanFile> {
        self.files.iter().filter(|file| file.fate == FileFate::Keep)
    }
}

/// Line the album's indexed files up against `release`, the files in
/// `stored` first (given only for the album's current edition). `current`
/// is the album's release and release group. Returns the plan and the
/// files on it a person matched by hand.
pub fn build(
    facts: &LocalAlbumFacts,
    release: &Release,
    candidate: &ReidentificationCandidate,
    indexed: &[String],
    stored: &[StoredPlacement],
    current: (Option<&str>, Option<&str>),
) -> (EditionPlan, Vec<String>) {
    let (current_release, current_group) = current;
    let seats = seat(release, candidate, indexed, stored);
    let mut tracks: Vec<PlanTrack> = release
        .tracks
        .iter()
        .map(|track| PlanTrack {
            disc: track.disc,
            position: track.position,
            title: track.title.clone(),
            recording_mbid: track.recording_id.to_ascii_lowercase(),
            release_track_mbid: track.id.to_ascii_lowercase(),
            length_ms: track.length_ms,
            local_track_id: None,
        })
        .collect();
    let mut kept: HashSet<&str> = HashSet::new();
    for seat in &seats {
        tracks[seat.track].local_track_id = Some(seat.local_track_id.clone());
        kept.insert(seat.local_track_id.as_str());
    }
    let recording_of = |track_id: &str| {
        candidate
            .evidence
            .track_evidence
            .iter()
            .find(|evidence| evidence.local_track_id == track_id)
            .and_then(|evidence| evidence.recording_mbid.clone())
            .or_else(|| {
                facts
                    .tracks
                    .iter()
                    .find(|file| file.local_track_id == track_id)
                    .and_then(|file| file.recording_mbid.clone())
            })
    };
    let mut files: Vec<PlanFile> = indexed
        .iter()
        .map(|track_id| {
            let local = facts
                .tracks
                .iter()
                .find(|file| file.local_track_id == *track_id);
            let (fate, edition_track) = if kept.contains(track_id.as_str()) {
                let spot = seats
                    .iter()
                    .find(|seat| seat.local_track_id == *track_id)
                    .map(|seat| seat.spot);
                (FileFate::Keep, spot)
            } else {
                let same_song = recording_of(track_id).and_then(|recording| {
                    // A second copy only when another file holds that track.
                    tracks.iter().find(|track| {
                        track.local_track_id.is_some()
                            && track.recording_mbid.eq_ignore_ascii_case(&recording)
                    })
                });
                match same_song {
                    Some(track) => (FileFate::Duplicate, Some((track.disc, track.position))),
                    None => match local.and_then(|file| other_version_of(file, release, &tracks)) {
                        Some(spot) => (FileFate::OtherVersion, Some(spot)),
                        None => (FileFate::Extra, None),
                    },
                }
            };
            let note = note_for(candidate, track_id, fate);
            PlanFile {
                local_track_id: track_id.clone(),
                title: local.map(|file| file.title.clone()).unwrap_or_default(),
                disc: local.map_or(1, |file| file.disc_number.max(1)),
                track: local.map_or(0, |file| file.track_number),
                length_ms: local.and_then(length_ms),
                fate,
                edition_track,
                note,
            }
        })
        .collect();
    // Another version pairs one file with one track: a track two files
    // could be another version of pairs with neither.
    for file in 0..files.len() {
        if files[file].fate != FileFate::OtherVersion {
            continue;
        }
        let spot = files[file].edition_track;
        let shared = files
            .iter()
            .filter(|other| other.fate == FileFate::OtherVersion && other.edition_track == spot)
            .count()
            > 1;
        if shared {
            for other in files
                .iter_mut()
                .filter(|other| other.fate == FileFate::OtherVersion && other.edition_track == spot)
            {
                other.fate = FileFate::Extra;
                other.edition_track = None;
            }
        }
    }
    let artist_name = release
        .artists
        .iter()
        .map(|artist| format!("{}{}", artist.name, artist.join))
        .collect::<String>();
    let by_hand = seats
        .iter()
        .filter(|seat| seat.by_hand)
        .map(|seat| seat.local_track_id.clone())
        .collect();
    let plan = EditionPlan {
        local_album_id: facts.local_album_id.clone(),
        release_group_mbid: release.release_group_id.to_ascii_lowercase(),
        release_mbid: release.id.to_ascii_lowercase(),
        title: release.title.clone(),
        artist_name,
        is_current: current_release.is_some_and(|current| release.answers_to(current)),
        same_album: current_group
            .is_some_and(|group| group.eq_ignore_ascii_case(&release.release_group_id)),
        edition: EditionFacts::of(release),
        tracks,
        files,
    };
    (plan, by_hand)
}

/// The note for a file whose track id belongs to another edition: set
/// aside when the file is kept by its recording, in the way when it is not.
fn note_for(
    candidate: &ReidentificationCandidate,
    track_id: &str,
    fate: FileFate,
) -> Option<Reason> {
    let kinds = &candidate
        .evidence
        .track_evidence
        .iter()
        .find(|evidence| evidence.local_track_id == track_id)?
        .evidence_kinds;
    let has = |code: &str| kinds.iter().any(|kind| kind == code);
    if fate == FileFate::Keep {
        has(Support::TrackIdElsewhere.code()).then_some(TRACK_ID_CORRECTED)
    } else {
        has(ConflictKind::ReleaseTrack.code()).then_some(TRACK_ID_ELSEWHERE)
    }
}

/// The missing edition track `file` is another version of: the one track
/// no file holds whose title and artist fold to the file's. `None` when no
/// track or more than one fits. [`build`] then drops a pairing another
/// file shares.
fn other_version_of(
    file: &LocalTrackFacts,
    release: &Release,
    tracks: &[PlanTrack],
) -> Option<(u32, u32)> {
    let title = fold(&file.title);
    let artist = fold(&file.artist_name);
    if title.is_empty() {
        return None;
    }
    let mut fits = tracks.iter().filter(|track| {
        track.local_track_id.is_none()
            && fold(&track.title) == title
            && fold(&track_artist(release, track)) == artist
    });
    match (fits.next(), fits.next()) {
        (Some(track), None) => Some((track.disc, track.position)),
        _ => None,
    }
}

/// The track's credited artist, or the release's when the track names none.
fn track_artist(release: &Release, track: &PlanTrack) -> String {
    release
        .tracks
        .iter()
        .find(|candidate| candidate.id.eq_ignore_ascii_case(&track.release_track_mbid))
        .map(|candidate| credit_text(&candidate.artists))
        .filter(|credit| !credit.is_empty())
        .unwrap_or_else(|| release.artist_text())
}

/// The file's length in milliseconds, from the most exact length known.
fn length_ms(file: &LocalTrackFacts) -> Option<u64> {
    match file.duration_exact {
        Some(seconds) if seconds.is_finite() && seconds >= 0.0 => {
            Some((seconds * 1000.0).round() as u64)
        }
        _ => file
            .duration_secs
            .map(|seconds| seconds.saturating_mul(1000)),
    }
}
