//! Score one release against one local album.
//!
//! Every local track is compared with every release track (title,
//! position, length, and the MusicBrainz or AcoustID recording when
//! known), the optimal pairing is found, and the album distance adds the
//! album-level penalties plus one entry per paired, missing, and
//! unmatched track. Embedded ids are proof: a local recording MBID that
//! the release does not carry, or a release-track MBID that disagrees
//! with the release the tags name, vetoes the release outright.

use std::collections::HashMap;

use super::assign::assign;
use super::distance::Distance;
use super::model::{LocalAlbum, LocalTrack, Release, ReleaseTrack, credit_text};

/// A pair worse than this is better left unmatched (v2's dummy cost).
pub const UNMATCHED_PAIR_COST: f64 = 0.65;
/// Length differences inside this many seconds cost nothing...
pub const LENGTH_GRACE_SECS: f64 = 10.0;
/// ...and cost everything past this many more (beets and Lidarr).
pub const LENGTH_MAX_SECS: f64 = 30.0;

/// What ties a local track to its release track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// The embedded recording or release-track MBID.
    EmbeddedId,
    /// The audio's AcoustID recordings include the release track's.
    Fingerprint,
    /// Title, position, and length only.
    Description,
}

impl Support {
    pub fn code(self) -> &'static str {
        match self {
            Support::EmbeddedId => "embedded_id",
            Support::Fingerprint => "acoustid",
            Support::Description => "title_position_length",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackPair {
    /// Index into the local tracks.
    pub local: usize,
    /// Index into the release tracks.
    pub track: usize,
    pub distance: f64,
    pub support: Support,
}

/// Why a release is vetoed for one local track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// The file's recording MBID is not on this release.
    Recording,
    /// The file names this release but a track id it does not carry.
    ReleaseTrack,
}

impl ConflictKind {
    pub fn code(self) -> &'static str {
        match self {
            ConflictKind::Recording => "recording_mbid_conflict",
            ConflictKind::ReleaseTrack => "release_track_mbid_conflict",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub local: usize,
    pub kind: ConflictKind,
}

/// One release scored against one local album.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseMatch {
    pub distance: Distance,
    pub pairs: Vec<TrackPair>,
    /// Local tracks left without a release track.
    pub unmatched: Vec<usize>,
    /// Release tracks no local track took.
    pub missing: Vec<usize>,
    pub conflicts: Vec<Conflict>,
}

impl ReleaseMatch {
    /// The full distance: what ranks editions against each other.
    pub fn album_distance(&self) -> f64 {
        self.distance.normalized()
    }

    /// The distance for files already in the library, where holding only
    /// part of an album is normal: missing tracks do not count.
    pub fn library_distance(&self) -> f64 {
        self.distance.normalized_excluding(&["missing_tracks"])
    }

    /// The worst paired track's distance.
    pub fn worst_track(&self) -> f64 {
        self.pairs
            .iter()
            .map(|pair| pair.distance)
            .fold(0.0, f64::max)
    }

    pub fn pair_for(&self, local: usize) -> Option<&TrackPair> {
        self.pairs.iter().find(|pair| pair.local == local)
    }
}

/// Score `release` against `local`. `aliases` maps retired recording
/// MBIDs to the ones MusicBrainz merged them into.
pub fn match_release(
    local: &LocalAlbum,
    release: &Release,
    aliases: &HashMap<String, String>,
) -> ReleaseMatch {
    let various = local.is_compilation || release.is_various_artists();
    let recordings: Vec<Option<String>> = local
        .tracks
        .iter()
        .map(|track| canonical_recording(track, aliases))
        .collect();
    let claims_release = |track: &LocalTrack| {
        track
            .release_mbid
            .as_deref()
            .is_some_and(|mbid| release.answers_to(mbid))
    };
    let mut pair_costs: HashMap<(usize, usize), (f64, Support)> = HashMap::new();
    for (row, track) in local.tracks.iter().enumerate() {
        for (column, candidate) in release.tracks.iter().enumerate() {
            if let Some(scored) = track_distance(
                track,
                recordings[row].as_deref(),
                claims_release(track),
                candidate,
                various,
            ) {
                pair_costs.insert((row, column), scored);
            }
        }
    }
    let assignment = assign(
        local.tracks.len(),
        release.tracks.len(),
        UNMATCHED_PAIR_COST,
        |row, column| pair_costs.get(&(row, column)).map(|(cost, _)| *cost),
    );

    let mut pairs = Vec::new();
    let mut unmatched = Vec::new();
    let mut taken = vec![false; release.tracks.len()];
    for (row, column) in assignment.into_iter().enumerate() {
        match column.and_then(|column| pair_costs.get(&(row, column)).map(|cost| (column, cost))) {
            Some((column, (cost, support))) => {
                taken[column] = true;
                pairs.push(TrackPair {
                    local: row,
                    track: column,
                    distance: *cost,
                    support: *support,
                });
            }
            None => unmatched.push(row),
        }
    }
    let missing: Vec<usize> = (0..release.tracks.len())
        .filter(|index| !taken[*index])
        .collect();

    let mut conflicts = Vec::new();
    for (row, track) in local.tracks.iter().enumerate() {
        if let Some(recording) = recordings[row].as_deref()
            && !release
                .tracks
                .iter()
                .any(|candidate| candidate.recording_id.eq_ignore_ascii_case(recording))
        {
            conflicts.push(Conflict {
                local: row,
                kind: ConflictKind::Recording,
            });
        } else if let Some(release_track) = track.release_track_mbid.as_deref()
            && claims_release(track)
            && !release
                .tracks
                .iter()
                .any(|candidate| candidate.id.eq_ignore_ascii_case(release_track))
        {
            conflicts.push(Conflict {
                local: row,
                kind: ConflictKind::ReleaseTrack,
            });
        }
    }

    let mut distance = Distance::default();
    if !various && !local.artist.trim().is_empty() {
        distance.add_string("artist", &local.artist, &release.artist_text());
    }
    if !local.title.trim().is_empty() {
        distance.add_string("album", &local.title, &release.title);
    }
    if let Some(year) = local.year
        && release.year().is_some()
    {
        let matches = Some(year) == release.year() || Some(year) == release.original_year();
        distance.add_bool("year", !matches);
    }
    if !release.media.is_empty() {
        distance.add_bool("media_count", local.disc_count() != release.media.len());
    }
    if let Some(tagged) = local.tagged_release() {
        distance.add_bool("album_id", !release.answers_to(&tagged));
    }
    for pair in &pairs {
        distance.add("tracks", pair.distance);
    }
    for _ in missing.iter().take(local.tracks.len()) {
        distance.add("missing_tracks", 1.0);
    }
    for _ in &unmatched {
        distance.add("unmatched_tracks", 1.0);
    }
    ReleaseMatch {
        distance,
        pairs,
        unmatched,
        missing,
        conflicts,
    }
}

fn canonical_recording(track: &LocalTrack, aliases: &HashMap<String, String>) -> Option<String> {
    let mbid = track.recording_mbid.as_deref()?.trim().to_ascii_lowercase();
    if mbid.is_empty() {
        return None;
    }
    Some(aliases.get(&mbid).cloned().unwrap_or(mbid))
}

/// One pair's normalized distance, or `None` when ids rule it out.
fn track_distance(
    local: &LocalTrack,
    recording: Option<&str>,
    claims_release: bool,
    candidate: &ReleaseTrack,
    various: bool,
) -> Option<(f64, Support)> {
    let mut distance = Distance::default();
    let mut support = Support::Description;
    if let Some(release_track) = local.release_track_mbid.as_deref()
        && claims_release
    {
        if !release_track.eq_ignore_ascii_case(&candidate.id) {
            return None;
        }
        support = Support::EmbeddedId;
    }
    if let Some(recording) = recording {
        if !recording.eq_ignore_ascii_case(&candidate.recording_id) {
            return None;
        }
        distance.add_bool("recording_id", false);
        support = Support::EmbeddedId;
    } else if !local.fingerprint_recordings.is_empty() {
        let heard = local
            .fingerprint_recordings
            .iter()
            .any(|id| id.eq_ignore_ascii_case(&candidate.recording_id));
        distance.add_bool("recording_id", !heard);
        if heard && support == Support::Description {
            support = Support::Fingerprint;
        }
    }
    distance.add_string("track_title", &local.title, &candidate.title);
    if various && !local.artist.trim().is_empty() {
        distance.add_string(
            "track_artist",
            &local.artist,
            &credit_text(&candidate.artists),
        );
    }
    if local.track_number > 0 {
        let on_medium =
            local.disc_number.max(1) == candidate.disc && local.track_number == candidate.position;
        let absolute = local.track_number == candidate.absolute_position;
        distance.add_bool("track_index", !(on_medium || absolute));
    }
    if let (Some(seconds), Some(length_ms)) = (local.duration_secs, candidate.length_ms) {
        let difference = (seconds - length_ms as f64 / 1000.0).abs() - LENGTH_GRACE_SECS;
        distance.add_ratio("track_length", difference.max(0.0), LENGTH_MAX_SECS);
    }
    Some((distance.normalized(), support))
}
