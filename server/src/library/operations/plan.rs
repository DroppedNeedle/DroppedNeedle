//! What converting an album to one edition would do, worked out before
//! anything changes.
//!
//! The plan lines the album's files up against the edition's tracklist
//! with the same placement a choice writes ([`super::choice::place`]):
//! each file keeps its place on an edition track, is a second copy of a
//! song the edition already has a file for, or is an extra the edition
//! has no track for. Edition tracks no file took are missing; "get missing
//! tracks" fetches exactly those. Choosing never deletes a file: duplicates
//! and extras stay in the album, unplaced, for the person to keep or
//! remove.

use std::collections::HashSet;

use super::choice::place;
use super::models::ReidentificationCandidate;
use crate::library::identify::models::LocalAlbumFacts;
use crate::library::matching::model::Release;

/// What happens to one file when the album takes the edition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFate {
    /// The file sits on its own track of the edition.
    Keep,
    /// The edition has this song, but another file already holds its track.
    Duplicate,
    /// The edition has no track for this file.
    Extra,
}

impl FileFate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Duplicate => "duplicate",
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
    pub fate: FileFate,
    /// The edition track (disc, position) the file keeps, or the one it
    /// duplicates.
    pub edition_track: Option<(u32, u32)>,
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

/// Line the album's indexed files up against `release`.
pub fn build(
    facts: &LocalAlbumFacts,
    release: &Release,
    candidate: &ReidentificationCandidate,
    indexed: &[String],
    current_release: Option<&str>,
) -> EditionPlan {
    let (placed, _) = place(indexed, candidate);
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
    for placement in &placed {
        if let Some(track) = tracks.iter_mut().find(|track| {
            track
                .release_track_mbid
                .eq_ignore_ascii_case(&placement.release_track_mbid)
        }) {
            track.local_track_id = Some(placement.local_track_id.clone());
            kept.insert(placement.local_track_id.as_str());
        }
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
    let files = indexed
        .iter()
        .map(|track_id| {
            let local = facts
                .tracks
                .iter()
                .find(|file| file.local_track_id == *track_id);
            let (fate, edition_track) = if kept.contains(track_id.as_str()) {
                let spot = placed
                    .iter()
                    .find(|placement| placement.local_track_id == *track_id)
                    .map(|placement| (placement.disc, placement.position));
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
                    None => (FileFate::Extra, None),
                }
            };
            PlanFile {
                local_track_id: track_id.clone(),
                title: local.map(|file| file.title.clone()).unwrap_or_default(),
                disc: local.map_or(1, |file| file.disc_number.max(1)),
                track: local.map_or(0, |file| file.track_number),
                fate,
                edition_track,
            }
        })
        .collect();
    let artist_name = release
        .artists
        .iter()
        .map(|artist| format!("{}{}", artist.name, artist.join))
        .collect::<String>();
    EditionPlan {
        local_album_id: facts.local_album_id.clone(),
        release_group_mbid: release.release_group_id.to_ascii_lowercase(),
        release_mbid: release.id.to_ascii_lowercase(),
        title: release.title.clone(),
        artist_name,
        is_current: current_release.is_some_and(|current| release.answers_to(current)),
        tracks,
        files,
    }
}
