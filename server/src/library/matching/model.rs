//! What the matcher compares: local files on one side, a MusicBrainz
//! release with its full tracklist on the other.
//!
//! [`Release`] is also the stored release document: identification keeps
//! it so the publisher can tag files from the release later without
//! asking MusicBrainz again.

use serde::{Deserialize, Serialize};

/// One local file, as the tags (and the fingerprint, when taken) say.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// 0 when unknown.
    pub track_number: u32,
    /// 1 when the tags say nothing.
    pub disc_number: u32,
    pub duration_secs: Option<f64>,
    pub recording_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub release_mbid: Option<String>,
    /// AcoustID recordings for this file's audio (empty: not printed).
    pub fingerprint_recordings: Vec<String>,
}

/// One local album: the files the library grouped together.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LocalAlbum {
    pub title: String,
    pub artist: String,
    pub year: Option<i32>,
    pub is_compilation: bool,
    pub tracks: Vec<LocalTrack>,
}

impl LocalAlbum {
    /// The release MBID most files agree on, when at least half carry it.
    pub fn tagged_release(&self) -> Option<String> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        for track in &self.tracks {
            let Some(mbid) = track.release_mbid.as_deref() else {
                continue;
            };
            let mbid = mbid.trim().to_ascii_lowercase();
            match counts.iter_mut().find(|(seen, _)| *seen == mbid) {
                Some((_, count)) => *count += 1,
                None => counts.push((mbid, 1)),
            }
        }
        let (mbid, count) = counts.into_iter().max_by_key(|(_, count)| *count)?;
        (count * 2 >= self.tracks.len().max(1)).then_some(mbid)
    }

    /// The highest disc number the files claim (1 when none do).
    pub fn max_disc(&self) -> usize {
        self.tracks
            .iter()
            .map(|track| track.disc_number.max(1) as usize)
            .max()
            .unwrap_or(1)
    }
}

/// One credited artist on a release or track.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CreditedArtist {
    pub id: String,
    /// The name as credited here.
    pub name: String,
    pub sort_name: Option<String>,
    /// Text joining this artist to the next (" & ", "; ", "").
    pub join: String,
}

/// Credit display text: credited names with their join phrases.
pub fn credit_text(artists: &[CreditedArtist]) -> String {
    artists
        .iter()
        .map(|artist| format!("{}{}", artist.name, artist.join))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// One medium of a release.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReleaseMedium {
    pub position: u32,
    pub format: Option<String>,
    pub title: Option<String>,
    pub track_count: u32,
}

/// One track of a release, in release order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReleaseTrack {
    /// Release-track MBID.
    pub id: String,
    pub recording_id: String,
    pub title: String,
    pub artists: Vec<CreditedArtist>,
    /// Medium position.
    pub disc: u32,
    /// Position on the medium.
    pub position: u32,
    /// Position counted across the whole release.
    pub absolute_position: u32,
    pub length_ms: Option<u64>,
}

/// A release with everything matching and tagging need.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Release {
    pub id: String,
    pub release_group_id: String,
    pub title: String,
    pub artists: Vec<CreditedArtist>,
    pub date: Option<String>,
    /// The release group's first release date.
    pub original_date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    pub barcode: Option<String>,
    pub asin: Option<String>,
    pub primary_type: Option<String>,
    pub secondary_types: Vec<String>,
    pub labels: Vec<String>,
    pub catalog_numbers: Vec<String>,
    pub media: Vec<ReleaseMedium>,
    pub tracks: Vec<ReleaseTrack>,
    /// Retired MBIDs MusicBrainz redirected to this release.
    #[serde(default)]
    pub old_ids: Vec<String>,
}

impl Release {
    pub fn artist_text(&self) -> String {
        credit_text(&self.artists)
    }

    /// True when `mbid` names this release, directly or as a merged id.
    pub fn answers_to(&self, mbid: &str) -> bool {
        self.id.eq_ignore_ascii_case(mbid)
            || self
                .old_ids
                .iter()
                .any(|old| old.eq_ignore_ascii_case(mbid))
    }

    pub fn is_various_artists(&self) -> bool {
        self.artists
            .iter()
            .any(|artist| artist.id.eq_ignore_ascii_case(VARIOUS_ARTISTS_MBID))
    }

    pub fn year(&self) -> Option<i32> {
        year_of(self.date.as_deref())
    }

    pub fn original_year(&self) -> Option<i32> {
        year_of(self.original_date.as_deref())
    }
}

/// MusicBrainz's "Various Artists" special artist.
pub const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

fn year_of(date: Option<&str>) -> Option<i32> {
    let year = date?.get(..4)?;
    year.bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| year.parse().ok())
        .flatten()
}
