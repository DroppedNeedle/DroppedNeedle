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
//! candidate ([`heard_on_candidates`]). It never moves a file off a track
//! that clearly fits it (see [`ReleaseMatch::disputed`]): AcoustID holds
//! user-submitted mistakes, often a neighbouring track's recording. When
//! the file's measured length is that track's and not the heard one's,
//! the length settles it (see [`ReleaseMatch::overruled`]).
//!
//! The album title and artist also pass v2's 0.20 gate: a release whose
//! title (edition words aside) or artist is further off than that never
//! identifies on its own, unless the files' own ids prove every track (for
//! the artist, only when one credit contains the other, so a tribute band
//! never passes for the band it covers).

use std::collections::HashMap;

use super::assign::assign;
use super::decide::COMPLETE_TRACK;
use super::distance::Distance;
use super::model::{LocalAlbum, LocalTrack, Release, ReleaseTrack, credit_text};
use super::strings::{
    artist_dist, artist_subset, basic_dist, string_dist, strip_edition_words, undecorated,
};
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
/// Two lengths within this many seconds can be the same master; further
/// apart they are different audio. The download landing uses the same
/// margin.
pub const SAME_MASTER_SECS: f64 = 3.0;
/// The evidence kind on a file whose fingerprint its length overruled.
pub const FINGERPRINT_OVERRULED: &str = "acoustid_overruled";

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
    /// Files whose audio names another track of this release than the one
    /// their title clearly fits (within [`COMPLETE_TRACK`] by `basic_dist`,
    /// in full or with bracketed parts and a "feat." tail left out) with
    /// its number or length also fitting. Their fingerprint is set aside
    /// here, so they pair by description, and the release never confirms
    /// by itself.
    pub disputed: Vec<usize>,
    /// Files that would be disputed, but whose measured length (from the
    /// audio header, never a length tag) is within [`SAME_MASTER_SECS`] of
    /// the clearly fitting track they pair with and further than that off
    /// every track their audio names. Their fingerprint is set aside too,
    /// and the release may confirm as if they had none.
    pub overruled: Vec<usize>,
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
    let unheard: Vec<usize> = local
        .tracks
        .iter()
        .zip(&recordings)
        .enumerate()
        .filter(|(_, (track, recording))| recording.is_none() && audio_disputes(track, release))
        .map(|(row, _)| row)
        .collect();
    let mut pair_costs: HashMap<(usize, usize), (f64, Support)> = HashMap::new();
    for (row, track) in local.tracks.iter().enumerate() {
        let heard: &[String] = if unheard.contains(&row) {
            &[]
        } else {
            &track.fingerprint_recordings
        };
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
                heard,
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
    let (overruled, disputed): (Vec<usize>, Vec<usize>) = unheard.into_iter().partition(|row| {
        let paired = pairs
            .iter()
            .find(|pair| pair.local == *row)
            .map(|pair| &release.tracks[pair.track]);
        length_overrules(&local.tracks[*row], release, paired)
    });

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
        disputed,
        overruled,
    }
}

/// The file's description clearly fits a track of `release` its audio
/// does not name, and no track the audio names fits it as well: a title
/// at least as close, and the file's number wherever that track has it.
/// "Interlude 1" and "Interlude 2" both pass the title gate, so the
/// heard one must also match as closely as the other.
fn audio_disputes(local: &LocalTrack, release: &Release) -> bool {
    let (named, other): (Vec<&ReleaseTrack>, Vec<&ReleaseTrack>) = release
        .tracks
        .iter()
        .partition(|candidate| hears(local, candidate));
    let fitting: Vec<&ReleaseTrack> = other
        .into_iter()
        .filter(|candidate| clearly_fits(local, candidate))
        .collect();
    let fits_as_well = |heard: &ReleaseTrack| {
        clearly_fits(local, heard)
            && fitting.iter().all(|rival| {
                title_dist(&local.title, &heard.title) <= title_dist(&local.title, &rival.title)
                    && (index_fits(local, rival) != Some(true)
                        || index_fits(local, heard) == Some(true))
            })
    };
    !named.is_empty() && !fitting.is_empty() && !named.iter().any(|heard| fits_as_well(heard))
}

/// Whether the file's audio names `candidate`'s recording.
fn hears(local: &LocalTrack, candidate: &ReleaseTrack) -> bool {
    local
        .fingerprint_recordings
        .iter()
        .any(|id| id.eq_ignore_ascii_case(&candidate.recording_id))
}

/// A file its audio disputes, settled by its measured length: within
/// [`SAME_MASTER_SECS`] of the unheard track it pairs with, which clearly
/// fits it, and further than that off every track its audio names. An
/// unknown length on either side settles nothing.
fn length_overrules(local: &LocalTrack, release: &Release, paired: Option<&ReleaseTrack>) -> bool {
    let (Some(seconds), Some(own)) = (local.duration_secs, paired) else {
        return false;
    };
    let near = |track: &ReleaseTrack| {
        track
            .length_ms
            .map(|length_ms| (seconds - length_ms as f64 / 1000.0).abs() <= SAME_MASTER_SECS)
    };
    let mut heard = release
        .tracks
        .iter()
        .filter(|candidate| hears(local, candidate))
        .peekable();
    !hears(local, own)
        && clearly_fits(local, own)
        && near(own) == Some(true)
        && heard.peek().is_some()
        && heard.all(|candidate| near(candidate) == Some(false))
}

/// The file's title matches the track's within [`COMPLETE_TRACK`], in full
/// or without the decorations either side adds ("(Remaster)", "(LP
/// Version)", "feat. X"), and its number or length fits too. A prefix
/// such as "0101 Cluster One" is not a decoration: such a file stays for
/// its audio to settle.
fn clearly_fits(local: &LocalTrack, candidate: &ReleaseTrack) -> bool {
    let length_fits = match (local.duration_secs, candidate.length_ms) {
        (Some(seconds), Some(length_ms)) => {
            (seconds - length_ms as f64 / 1000.0).abs() <= LENGTH_GRACE_SECS
        }
        _ => false,
    };
    title_fits(&local.title, &candidate.title)
        && (index_fits(local, candidate) == Some(true) || length_fits)
}

fn title_fits(file: &str, track: &str) -> bool {
    let (bare_file, bare_track) = (undecorated(file), undecorated(track));
    basic_dist(file, track) <= COMPLETE_TRACK
        || (!bare_file.is_empty()
            && !bare_track.is_empty()
            && basic_dist(&bare_file, &bare_track) <= COMPLETE_TRACK)
}

/// Whether the file's track number is the track's; `None` when the file
/// has none.
fn index_fits(local: &LocalTrack, candidate: &ReleaseTrack) -> Option<bool> {
    (local.track_number > 0).then(|| {
        let on_medium =
            local.disc_number.max(1) == candidate.disc && local.track_number == candidate.position;
        on_medium || local.track_number == candidate.absolute_position
    })
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
/// `heard` is the file's fingerprint recordings, or none when set aside.
fn track_distance(
    local: &LocalTrack,
    recording: Option<&str>,
    heard: &[String],
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
    } else if !heard.is_empty() {
        // Every heard recording sits on some candidate: one that is not
        // this track's is a rival's, and counts against the pair.
        let heard = heard
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
    if let Some(fits) = index_fits(local, candidate) {
        distance.add_bool("track_index", !fits);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn release_of(tracks: &[(&str, u64)]) -> Release {
        Release {
            id: "release".to_owned(),
            release_group_id: "group".to_owned(),
            title: "Night Shift".to_owned(),
            tracks: tracks
                .iter()
                .zip(1..)
                .map(|((title, seconds), position)| ReleaseTrack {
                    id: format!("track-{position}"),
                    recording_id: format!("recording-{position}"),
                    title: (*title).to_owned(),
                    disc: 1,
                    position,
                    absolute_position: position,
                    length_ms: Some(seconds * 1000),
                    ..ReleaseTrack::default()
                })
                .collect(),
            ..Release::default()
        }
    }

    /// One file against `release`: its title, number, measured length and
    /// the track position AcoustID heard.
    fn one_file(
        release: &Release,
        title: &str,
        number: u32,
        seconds: f64,
        heard: u32,
    ) -> ReleaseMatch {
        let local = LocalAlbum {
            title: "Night Shift".to_owned(),
            tracks: vec![LocalTrack {
                id: "t1".to_owned(),
                title: title.to_owned(),
                track_number: number,
                disc_number: 1,
                duration_secs: Some(seconds),
                fingerprint_recordings: vec![format!("recording-{heard}")],
                ..LocalTrack::default()
            }],
            ..LocalAlbum::default()
        };
        match_release(&local, release, &HashMap::new())
    }

    /// "(Remaster)", "(LP Version)" and "feat." do not stop a title from
    /// clearly fitting its plain track, so audio naming a neighbour of the
    /// same length never moves the file. A track whose title fits better,
    /// such as the remix, still wins over the plain one.
    #[test]
    fn a_decorated_title_still_holds_its_track() {
        let release = release_of(&[
            ("Night Drive", 200),
            ("Night Drive (Remix)", 200),
            ("Daybreak", 200),
        ]);
        for title in [
            "Night Drive (Remaster)",
            "Night Drive (LP Version)",
            "Night Drive (feat. Ana Lee)",
        ] {
            let matched = one_file(&release, title, 1, 200.0, 3);
            assert_eq!(matched.disputed, vec![0], "{title}");
            assert_eq!(matched.pair_for(0).map(|pair| pair.track), Some(0));
        }
        let remix = one_file(&release, "Night Drive (Remix)", 0, 200.0, 1);
        assert_eq!(remix.disputed, vec![0]);
        assert_eq!(remix.pair_for(0).map(|pair| pair.track), Some(1));
    }

    /// A file whose measured length is its own track's and more than
    /// [`SAME_MASTER_SECS`] off the track AcoustID heard overrules the
    /// fingerprint: it stays on its track without holding the release.
    /// When the heard track is as long, the dispute stands.
    #[test]
    fn the_measured_length_overrules_a_fingerprint() {
        let release = release_of(&[("Night Drive", 200), ("Daybreak", 240)]);
        let matched = one_file(&release, "Night Drive", 1, 200.5, 2);
        assert!(matched.disputed.is_empty());
        assert_eq!(matched.overruled, vec![0]);
        assert_eq!(matched.pair_for(0).map(|pair| pair.track), Some(0));

        let close = release_of(&[("Night Drive", 200), ("Daybreak", 202)]);
        let matched = one_file(&close, "Night Drive", 1, 200.5, 2);
        assert_eq!(matched.disputed, vec![0]);
        assert_eq!(matched.pair_for(0).map(|pair| pair.track), Some(0));
    }

    /// Two tracks whose titles differ by one digit both pass the title
    /// gate: audio naming the other one still never moves the file off
    /// the track its title and number match exactly.
    #[test]
    fn audio_never_moves_a_file_to_a_near_namesake() {
        let track = |position: u32, title: &str| ReleaseTrack {
            id: format!("track-{position}"),
            recording_id: format!("recording-{position}"),
            title: title.to_owned(),
            disc: 1,
            position,
            absolute_position: position,
            length_ms: Some(90_000),
            ..ReleaseTrack::default()
        };
        let release = Release {
            id: "release".to_owned(),
            release_group_id: "group".to_owned(),
            title: "Night Shift".to_owned(),
            tracks: vec![track(1, "Interlude 1"), track(2, "Interlude 2")],
            ..Release::default()
        };
        let local = LocalAlbum {
            title: "Night Shift".to_owned(),
            tracks: vec![LocalTrack {
                id: "t1".to_owned(),
                title: "Interlude 1".to_owned(),
                track_number: 1,
                disc_number: 1,
                duration_secs: Some(90.0),
                fingerprint_recordings: vec!["recording-2".to_owned()],
                ..LocalTrack::default()
            }],
            ..LocalAlbum::default()
        };
        let matched = match_release(&local, &release, &HashMap::new());
        assert_eq!(matched.disputed, vec![0]);
        assert_eq!(matched.pair_for(0).map(|pair| pair.track), Some(0));
    }
}
