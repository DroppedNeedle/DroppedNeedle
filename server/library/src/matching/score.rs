//! Score one release against one local album.
//!
//! Every local track is compared with every release track (title,
//! position, length, and the MusicBrainz or AcoustID recording when
//! known), the optimal pairing is found, and the album distance adds the
//! album-level penalties plus one entry per paired, missing, and
//! unmatched track. Embedded ids are proof: a local recording MBID that
//! the release does not carry vetoes the release outright. So does a
//! release-track MBID the release the file names does not carry, unless
//! the file's recording MBID places it on that release: then the file's
//! release id outranks its track id, as in Picard, and the track id is set
//! aside as a tagging mistake (the file pairs as it would with no track
//! id, see [`Support::TrackIdElsewhere`]). A length more than 40 s off
//! vetoes a pair when nothing but the description ties it (v2's duration
//! conflict). A fingerprint helps a pair whose recording AcoustID heard,
//! and counts against one only when what it heard sits on a rival
//! candidate ([`heard_on_candidates`]).
//!
//! The album title and artist also pass v2's 0.20 gate: a release whose
//! title (edition words aside) or artist is further off than that never
//! identifies on its own, unless the files' own ids prove every track (for
//! the artist, only when one credit contains the other, so a tribute band
//! never passes for the band it covers).

use std::collections::HashMap;

use super::assign::assign;
use super::distance::Distance;
use super::model::{LocalAlbum, LocalTrack, Release, ReleaseTrack, credit_text};
use super::strings::{artist_dist, artist_subset, string_dist, strip_edition_words};
use droppedneedle_providers::musicbrainz::is_valid_mbid;

/// A pair worse than this is better left unmatched (v2's dummy cost).
pub const UNMATCHED_PAIR_COST: f64 = 0.65;
/// Penalties left out of the library distance (and its shares): holding
/// part of an album is normal for files already in the library.
pub const LIBRARY_EXCLUDED: &[&str] = &["missing_tracks"];
/// v2's album title and artist gate.
pub const NAME_GATE: f64 = 0.20;
/// Length differences inside this many seconds cost nothing...
pub const LENGTH_GRACE_SECS: f64 = 10.0;
/// ...and cost everything past this many more (beets and Lidarr).
pub const LENGTH_MAX_SECS: f64 = 30.0;

/// What ties a local track to its release track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// The embedded recording or release-track MBID.
    EmbeddedId,
    /// The recording MBID, on a file whose release id names this release
    /// and whose track id belongs to another edition. Weaker than
    /// `EmbeddedId`: never proof for the name gate or for credits.
    TrackIdElsewhere,
    /// The audio's AcoustID recordings include the release track's.
    Fingerprint,
    /// Title, position, and length only.
    Description,
}

impl Support {
    pub fn code(self) -> &'static str {
        match self {
            Support::EmbeddedId => "embedded_id",
            Support::TrackIdElsewhere => "track_id_elsewhere",
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
    /// The album title and artist pass the name gate (see the module).
    pub names_agree: bool,
}

impl ReleaseMatch {
    /// The full distance: what ranks editions against each other.
    pub fn album_distance(&self) -> f64 {
        self.distance.normalized()
    }

    /// The distance for files already in the library, where holding only
    /// part of an album is normal: missing tracks do not count.
    pub fn library_distance(&self) -> f64 {
        self.distance.normalized_excluding(LIBRARY_EXCLUDED)
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
    let on_release = |recording: &str| {
        release
            .tracks
            .iter()
            .any(|candidate| candidate.recording_id.eq_ignore_ascii_case(recording))
    };
    // A track id the release the file names does not carry: a veto, unless
    // the file's recording is on the release, which sets the id aside.
    let foreign_track_id = |track: &LocalTrack| {
        track.release_track_mbid.as_deref().is_some_and(|id| {
            claims_release(track)
                && !release
                    .tracks
                    .iter()
                    .any(|candidate| candidate.id.eq_ignore_ascii_case(id))
        })
    };
    let set_aside: Vec<bool> = local
        .tracks
        .iter()
        .zip(&recordings)
        .map(|(track, recording)| {
            foreign_track_id(track) && recording.as_deref().is_some_and(on_release)
        })
        .collect();
    let mut pair_costs: HashMap<(usize, usize), (f64, Support)> = HashMap::new();
    for (row, track) in local.tracks.iter().enumerate() {
        for (column, candidate) in release.tracks.iter().enumerate() {
            let track_id = if set_aside[row] {
                TrackId::SetAside
            } else if claims_release(track) {
                TrackId::Binding
            } else {
                TrackId::Ignored
            };
            if let Some(scored) = track_distance(
                track,
                recordings[row].as_deref(),
                track_id,
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
            && !on_release(recording)
        {
            conflicts.push(Conflict {
                local: row,
                kind: ConflictKind::Recording,
            });
        } else if !set_aside[row] && foreign_track_id(track) {
            conflicts.push(Conflict {
                local: row,
                kind: ConflictKind::ReleaseTrack,
            });
        }
    }

    let mut distance = Distance::default();
    if !various && !local.artist.trim().is_empty() {
        distance.add("artist", artist_dist(&local.artist, &release.artist_text()));
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
    // The catalog keeps no disc total, so a partial set of a multi-disc
    // release cannot be told from a single disc: only a disc number the
    // release does not have counts against it.
    if !release.media.is_empty() {
        distance.add_bool("media_count", local.max_disc() > release.media.len());
    }
    if let Some(tagged) = local.tagged_release() {
        distance.add_bool("album_id", !release.answers_to(&tagged));
    }
    for pair in &pairs {
        distance.add("tracks", pair.distance);
    }
    for _ in &missing {
        distance.add("missing_tracks", 1.0);
    }
    for _ in &unmatched {
        distance.add("unmatched_tracks", 1.0);
    }
    let names_agree = names_agree(local, release, various, &pairs);
    ReleaseMatch {
        distance,
        pairs,
        unmatched,
        missing,
        conflicts,
        names_agree,
    }
}

/// v2's title and artist gate, with its proof escapes.
fn names_agree(local: &LocalAlbum, release: &Release, various: bool, pairs: &[TrackPair]) -> bool {
    let proven = !local.tracks.is_empty()
        && (0..local.tracks.len()).all(|index| {
            pairs
                .iter()
                .any(|pair| pair.local == index && pair.support == Support::EmbeddedId)
        });
    let title_ok = local.title.trim().is_empty()
        || string_dist(
            &strip_edition_words(&local.title),
            &strip_edition_words(&release.title),
        ) <= NAME_GATE
        || proven;
    let release_artist = release.artist_text();
    let artist_ok = various
        || local.artist.trim().is_empty()
        || artist_dist(&local.artist, &release_artist) <= NAME_GATE
        || (proven && artist_subset(&local.artist, &release_artist));
    title_ok && artist_ok
}

/// Most recording MBIDs looked up for merges per album: one per file of
/// the largest albums.
pub const MERGE_LOOKUPS: usize = 100;

/// The files' recording MBIDs that no candidate in `releases` carries,
/// lowercased, each once, in file order, at most [`MERGE_LOOKUPS`].
/// MusicBrainz may have merged them into a recording a candidate does
/// carry; until that is looked up each one vetoes every candidate. A tag
/// that is not a MusicBrainz id is left out: MusicBrainz would refuse it,
/// and it stays a plain conflict.
pub fn unknown_recordings(local: &LocalAlbum, releases: &[Release]) -> Vec<String> {
    if releases.is_empty() {
        return Vec::new();
    }
    let mut unknown: Vec<String> = Vec::new();
    for track in &local.tracks {
        let Some(mbid) = canonical_recording(track, &HashMap::new()) else {
            continue;
        };
        let carried = releases.iter().any(|release| {
            release
                .tracks
                .iter()
                .any(|candidate| candidate.recording_id.eq_ignore_ascii_case(&mbid))
        });
        if !carried && is_valid_mbid(&mbid) && !unknown.contains(&mbid) {
            unknown.push(mbid);
        }
    }
    unknown.truncate(MERGE_LOOKUPS);
    unknown
}

/// The AcoustID recordings heard per file (`heard`, keyed by local track
/// id) that some release in `releases` carries. A fingerprint confirms a
/// pair when it names the pair's recording, and counts against a pair only
/// when it names a recording a rival track carries. A recording no
/// candidate has says nothing: AcoustID links many reissues and remasters
/// to no recording, or to an older one.
pub fn heard_on_candidates(
    heard: &HashMap<String, Vec<String>>,
    releases: &[Release],
) -> HashMap<String, Vec<String>> {
    let carried = |recording: &str| {
        releases.iter().any(|release| {
            release
                .tracks
                .iter()
                .any(|track| track.recording_id.eq_ignore_ascii_case(recording))
        })
    };
    heard
        .iter()
        .filter_map(|(file, recordings)| {
            let placed: Vec<String> = recordings
                .iter()
                .filter(|recording| carried(recording))
                .cloned()
                .collect();
            (!placed.is_empty()).then(|| (file.clone(), placed))
        })
        .collect()
}

fn canonical_recording(track: &LocalTrack, aliases: &HashMap<String, String>) -> Option<String> {
    let mbid = track.recording_mbid.as_deref()?.trim().to_ascii_lowercase();
    if mbid.is_empty() {
        return None;
    }
    Some(aliases.get(&mbid).cloned().unwrap_or(mbid))
}

/// How a file's release-track id counts against one release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackId {
    /// The file names this release: its track id picks the track.
    Binding,
    /// The file names this release, but its track id belongs to another
    /// edition while its recording is on this one: the id is left out.
    SetAside,
    /// The file names another release (or none): its track id says
    /// nothing here.
    Ignored,
}

/// What the movements a release names after its work's title cost, as a
/// share of their plain distance: the weight beets gives a bracketed
/// suffix.
const MOVEMENT_WEIGHT: f64 = 0.3;

/// A file's title against a release track's. MusicBrainz writes a
/// multi-part piece as "Work: I. Movement / II. Movement" ("Close to the
/// Edge: I. The Solid Time of Change / ..."), where a file often names the
/// work alone: the part before the colon is compared too, and the
/// movements cost [`MOVEMENT_WEIGHT`] of what they would.
fn title_dist(file: &str, track: &str) -> f64 {
    let full = string_dist(file, track);
    match track.split_once(':') {
        Some((work, _)) if !file.contains(':') && !work.trim().is_empty() => {
            let work_only = string_dist(file, work);
            full.min(work_only + MOVEMENT_WEIGHT * (full - work_only).max(0.0))
        }
        _ => full,
    }
}

/// One pair's normalized distance, or `None` when ids rule it out.
fn track_distance(
    local: &LocalTrack,
    recording: Option<&str>,
    track_id: TrackId,
    candidate: &ReleaseTrack,
    various: bool,
) -> Option<(f64, Support)> {
    let mut distance = Distance::default();
    let mut support = Support::Description;
    if let Some(release_track) = local.release_track_mbid.as_deref()
        && track_id == TrackId::Binding
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
        support = if track_id == TrackId::SetAside {
            Support::TrackIdElsewhere
        } else {
            Support::EmbeddedId
        };
    } else if !local.fingerprint_recordings.is_empty() {
        // Every heard recording sits on some candidate: one that is not
        // this track's is a rival's, and counts against the pair.
        let heard = local
            .fingerprint_recordings
            .iter()
            .any(|id| id.eq_ignore_ascii_case(&candidate.recording_id));
        distance.add_bool("recording_id", !heard);
        if heard && support == Support::Description {
            support = Support::Fingerprint;
        }
    }
    distance.add("track_title", title_dist(&local.title, &candidate.title));
    if various && !local.artist.trim().is_empty() {
        distance.add(
            "track_artist",
            artist_dist(&local.artist, &credit_text(&candidate.artists)),
        );
    }
    if local.track_number > 0 {
        let on_medium =
            local.disc_number.max(1) == candidate.disc && local.track_number == candidate.position;
        let absolute = local.track_number == candidate.absolute_position;
        distance.add_bool("track_index", !(on_medium || absolute));
    }
    if let (Some(seconds), Some(length_ms)) = (local.duration_secs, candidate.length_ms) {
        let gap = (seconds - length_ms as f64 / 1000.0).abs();
        if support == Support::Description && gap > LENGTH_GRACE_SECS + LENGTH_MAX_SECS {
            return None;
        }
        distance.add_ratio(
            "track_length",
            (gap - LENGTH_GRACE_SECS).max(0.0),
            LENGTH_MAX_SECS,
        );
    }
    Some((distance.normalized(), support))
}
