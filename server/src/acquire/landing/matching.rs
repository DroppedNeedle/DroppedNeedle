//! Match landed files to the requested release.
//!
//! The library's matching engine does the scoring; this module feeds it
//! the way Lidarr's import does for a grabbed album: candidates restricted
//! to the requested release group (the pinned edition first, then the
//! release the files' tags name, then the group's editions from a
//! search), and the closest conflict-free edition wins. Then it plans the
//! files: what imports, what is already in the library, what is held
//! because its tags name another track, and what the release does not
//! account for at all.

use std::collections::{HashMap, HashSet};

use super::ports::{LandingLibrary, OwnedTracks};
use super::probe::{LandedFile, Landing};
use super::specs::Target;
use crate::library::identify::sources::ReleaseHit;
use crate::library::matching::decide::{ACCEPT_TRACK, EDITION_COHORT};
use crate::library::matching::match_release;
use crate::library::matching::score::TrackPair;
use crate::library::matching::strings::{fold, string_dist};
use crate::library::matching::{LocalAlbum, LocalTrack, Release, ReleaseMatch, ReleaseTrack};

/// Editions of the requested group fetched after a search.
const PER_GROUP: usize = 3;

/// How the lookup went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchState {
    /// A conflict-free release was scored.
    Matched,
    /// MusicBrainz knows no release of the requested group to compare.
    NoCandidate,
    /// Every candidate is vetoed by the files' own MusicBrainz ids.
    Contradictory,
    /// MusicBrainz could not answer; try again later.
    Unavailable(String),
}

/// The chosen release and how the files scored against it.
#[derive(Debug, Clone)]
pub struct FoundRelease {
    pub release: Release,
    pub scored: ReleaseMatch,
}

/// The matching outcome the specs read.
#[derive(Debug, Clone)]
pub struct MatchSummary {
    pub state: MatchState,
    pub found: Option<FoundRelease>,
    /// Releases considered and their library distance, for the record.
    pub considered: Vec<(String, f64)>,
    /// Tracks of the group the library already holds.
    pub owned: OwnedTracks,
}

impl MatchSummary {
    /// The scored release, when one was found.
    pub fn best(&self) -> Option<&FoundRelease> {
        self.found.as_ref()
    }

    /// Whether the library already holds this release track.
    pub fn owns(&self, track: &ReleaseTrack) -> bool {
        self.owned
            .release_tracks
            .contains(&track.id.to_ascii_lowercase())
            || self
                .owned
                .recordings
                .contains(&track.recording_id.to_ascii_lowercase())
    }
}

impl FoundRelease {
    /// For a track download, the pair that is the requested track: by
    /// recording MBID, else (on the release acquisition resolved the track
    /// to) by its position there, else by title, else the only pair there
    /// is.
    pub fn requested_pair(&self, target: &Target) -> Option<&TrackPair> {
        let track_of = |pair: &&TrackPair| &self.release.tracks[pair.track];
        let by_position = || {
            let (release, wanted) = target.album_positions.as_ref()?;
            if !self.release.answers_to(release) {
                return None;
            }
            self.scored.pairs.iter().find(|pair| {
                let track = track_of(pair);
                wanted
                    .iter()
                    .any(|at| at.disc == track.disc && at.track == track.position)
            })
        };
        if let Some(recording) = target.recording_mbid.as_deref() {
            return self
                .scored
                .pairs
                .iter()
                .find(|pair| track_of(pair).recording_id.eq_ignore_ascii_case(recording))
                .or_else(by_position);
        }
        if let Some(pair) = by_position() {
            return Some(pair);
        }
        if let Some(title) = target.track_title.as_deref() {
            return self
                .scored
                .pairs
                .iter()
                .filter(|pair| string_dist(&track_of(pair).title, title) <= 0.2)
                .min_by(|a, b| a.distance.total_cmp(&b.distance));
        }
        match self.scored.pairs.as_slice() {
            [only] => Some(only),
            _ => None,
        }
    }
}

/// The files as the matching engine sees them. Untagged files fall back
/// to the file name ("03 - Title.flac") and the disc folder ("CD2"), and
/// an untagged album takes the requested names, as v2's folder import did.
pub fn local_album(files: &[LandedFile], target: &Target) -> LocalAlbum {
    let tracks = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let (name_disc, number, name_title) = file_name_parts(file);
            let tag = &file.tag;
            LocalTrack {
                id: index.to_string(),
                title: if tag.title.trim().is_empty() {
                    name_title
                } else {
                    tag.title.trim().to_owned()
                },
                artist: tag.artist.trim().to_owned(),
                track_number: if tag.track_number > 0 {
                    tag.track_number
                } else {
                    number.unwrap_or(0)
                },
                disc_number: if tag.disc_number > 0 {
                    tag.disc_number
                } else {
                    name_disc.or_else(|| disc_folder(file)).unwrap_or(1)
                },
                duration_secs: file.header.duration_seconds,
                recording_mbid: clean(tag.musicbrainz_recording_id.as_deref()),
                release_track_mbid: clean(tag.musicbrainz_release_track_id.as_deref()),
                release_mbid: clean(tag.musicbrainz_release_id.as_deref()),
                fingerprint_recordings: Vec::new(),
            }
        })
        .collect();
    let album_tag = most_common(files.iter().map(|file| file.tag.album.trim()));
    let artist_tag = most_common(files.iter().map(|file| {
        file.tag
            .album_artist
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(file.tag.artist.trim())
    }));
    LocalAlbum {
        title: album_tag.unwrap_or_else(|| target.album_title.clone()),
        artist: artist_tag.unwrap_or_else(|| target.artist_name.clone()),
        year: files.iter().find_map(|file| file.tag.year).or(target.year),
        is_compilation: files.iter().any(|file| file.tag.compilation),
        tracks,
        ..LocalAlbum::default()
    }
}

/// Gather the candidates and score them. MusicBrainz failures come back
/// as [`MatchState::Unavailable`] so the landing waits instead of
/// guessing.
pub async fn find(
    library: &dyn LandingLibrary,
    target: &Target,
    landing: &Landing,
) -> MatchSummary {
    let local = local_album(&landing.audio, target);
    let owned = if target.release_group_mbid.is_empty() {
        OwnedTracks::default()
    } else {
        library.owned(&target.release_group_mbid).await
    };
    let releases = match recall(library, target, &local).await {
        Ok(releases) => releases,
        Err(detail) => {
            return MatchSummary {
                state: MatchState::Unavailable(detail),
                found: None,
                considered: Vec::new(),
                owned,
            };
        }
    };
    let mut summary = choose(&local, releases, target);
    summary.owned = owned;
    summary
}

/// Candidate releases of the requested group. A pinned edition is the
/// only candidate: the files are judged against its tracklist and nothing
/// else (owner rule, the pin is honoured end to end). Without a pin: the
/// tagged release, then the group's editions from a search.
async fn recall(
    library: &dyn LandingLibrary,
    target: &Target,
    local: &LocalAlbum,
) -> Result<Vec<Release>, String> {
    let mut releases: Vec<Release> = Vec::new();
    let error = |error: crate::library::identify::sources::SourceError| error.0;
    if let Some(pinned) = target.release_mbid.as_deref() {
        fetch(library.release(pinned).await.map_err(error)?, &mut releases);
        return Ok(releases);
    }
    if let Some(tagged) = local.tagged_release()
        && !releases.iter().any(|known| known.answers_to(&tagged))
    {
        fetch(
            library.release(&tagged).await.map_err(error)?,
            &mut releases,
        );
    }
    let in_group = |release: &Release| {
        target.release_group_mbid.is_empty()
            || release
                .release_group_id
                .eq_ignore_ascii_case(&target.release_group_mbid)
    };
    releases.retain(in_group);
    if !target.album_title.trim().is_empty() {
        let artist = if is_various(&target.artist_name) {
            ""
        } else {
            target.artist_name.as_str()
        };
        let hits = library
            .search(&target.album_title, artist)
            .await
            .map_err(error)?;
        for id in pick_hits(&hits, target, local.tracks.len(), &releases) {
            fetch(library.release(&id).await.map_err(error)?, &mut releases);
        }
        releases.retain(in_group);
    }
    Ok(releases)
}

/// Keep a fetched release unless it is already in the list.
fn fetch(found: Option<Release>, releases: &mut Vec<Release>) {
    if let Some(release) = found
        && !releases.iter().any(|known| known.id == release.id)
    {
        releases.push(release);
    }
}

/// Group editions worth a tracklist fetch: best search score first, then
/// the closest track count.
fn pick_hits(
    hits: &[ReleaseHit],
    target: &Target,
    local_tracks: usize,
    known: &[Release],
) -> Vec<String> {
    let mut ranked: Vec<&ReleaseHit> = hits
        .iter()
        .filter(|hit| {
            target.release_group_mbid.is_empty()
                || hit
                    .release_group_id
                    .eq_ignore_ascii_case(&target.release_group_mbid)
        })
        .filter(|hit| !known.iter().any(|release| release.answers_to(&hit.id)))
        .collect();
    ranked.sort_by(|a, b| {
        b.score.cmp(&a.score).then_with(|| {
            let gap = |hit: &ReleaseHit| {
                hit.track_count
                    .map_or(usize::MAX, |count| (count as usize).abs_diff(local_tracks))
            };
            gap(a).cmp(&gap(b))
        })
    });
    ranked
        .into_iter()
        .take(PER_GROUP)
        .map(|hit| hit.id.clone())
        .collect()
}

/// Score every candidate and pick the edition: the closest conflict-free
/// release by library distance, and among editions within
/// [`EDITION_COHORT`] of it the pinned one, then the one the tags name,
/// then the closest full tracklist (the matching engine's edition rule).
pub fn choose(local: &LocalAlbum, releases: Vec<Release>, target: &Target) -> MatchSummary {
    if releases.is_empty() {
        return MatchSummary {
            state: MatchState::NoCandidate,
            found: None,
            considered: Vec::new(),
            owned: OwnedTracks::default(),
        };
    }
    let aliases = HashMap::new();
    let scored: Vec<ReleaseMatch> = releases
        .iter()
        .map(|release| match_release(local, release, &aliases))
        .collect();
    let considered = releases
        .iter()
        .zip(&scored)
        .map(|(release, scored)| (release.id.clone(), scored.library_distance()))
        .collect();
    let eligible: Vec<usize> = (0..releases.len())
        .filter(|index| scored[*index].conflicts.is_empty())
        .collect();
    let Some(best) = eligible
        .iter()
        .map(|index| scored[*index].library_distance())
        .min_by(f64::total_cmp)
    else {
        return MatchSummary {
            state: MatchState::Contradictory,
            found: None,
            considered,
            owned: OwnedTracks::default(),
        };
    };
    let tagged = local.tagged_release();
    let rank = |index: usize| {
        let release = &releases[index];
        let pinned = target
            .release_mbid
            .as_deref()
            .is_some_and(|pin| release.answers_to(pin));
        let named = tagged.as_deref().is_some_and(|id| release.answers_to(id));
        (!pinned, !named)
    };
    let Some(chosen) = eligible
        .iter()
        .copied()
        .filter(|index| scored[*index].library_distance() <= best + EDITION_COHORT)
        .min_by(|a, b| {
            rank(*a)
                .cmp(&rank(*b))
                .then_with(|| {
                    scored[*a]
                        .album_distance()
                        .total_cmp(&scored[*b].album_distance())
                })
                .then_with(|| releases[*a].id.cmp(&releases[*b].id))
        })
    else {
        return MatchSummary {
            state: MatchState::Contradictory,
            found: None,
            considered,
            owned: OwnedTracks::default(),
        };
    };
    let mut releases = releases;
    let mut scored = scored;
    MatchSummary {
        state: MatchState::Matched,
        found: Some(FoundRelease {
            release: releases.swap_remove(chosen),
            scored: scored.swap_remove(chosen),
        }),
        considered,
        owned: OwnedTracks::default(),
    }
}

/// What happens to each landed file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilePlan {
    /// (landed file, release track) pairs to publish.
    pub import: Vec<(usize, usize)>,
    /// Landed files whose release track the library already holds.
    pub owned: Vec<usize>,
    /// (landed file, release track) pairs too far apart: their tags name
    /// another track. Held for a person.
    pub held: Vec<(usize, usize)>,
    /// Landed files the release does not account for; left out.
    pub extra: Vec<usize>,
    /// Every requested track is now in the library (imported or held
    /// before).
    pub complete: bool,
}

/// Plan the files against the chosen release.
pub fn plan(summary: &MatchSummary, target: &Target, files: usize) -> FilePlan {
    let Some(found) = summary.best() else {
        return FilePlan {
            extra: (0..files).collect(),
            ..FilePlan::default()
        };
    };
    let mut out = FilePlan::default();
    let requested = if target.is_track {
        found.requested_pair(target).map(|pair| pair.local)
    } else {
        None
    };
    let mut paired = HashSet::new();
    for pair in &found.scored.pairs {
        paired.insert(pair.local);
        if target.is_track && Some(pair.local) != requested {
            out.extra.push(pair.local);
            continue;
        }
        let track = &found.release.tracks[pair.track];
        if summary.owns(track) {
            out.owned.push(pair.local);
        } else if pair.distance > ACCEPT_TRACK {
            out.held.push((pair.local, pair.track));
        } else {
            out.import.push((pair.local, pair.track));
        }
    }
    out.extra
        .extend((0..files).filter(|index| !paired.contains(index)));
    out.extra.sort_unstable();
    let covered: HashSet<usize> = found
        .scored
        .pairs
        .iter()
        .filter(|pair| {
            out.import.contains(&(pair.local, pair.track)) || out.owned.contains(&pair.local)
        })
        .map(|pair| pair.track)
        .collect();
    out.complete = if target.is_track {
        requested.is_some_and(|local| {
            out.import.iter().any(|(file, _)| *file == local) || out.owned.contains(&local)
        })
    } else {
        found
            .release
            .tracks
            .iter()
            .enumerate()
            .all(|(index, track)| covered.contains(&index) || summary.owns(track))
    };
    out
}

/// "03 - Title.flac" -> (None, 3, "Title"); "203 Title.flac" -> disc 2,
/// track 3; a name without a number keeps its whole stem as the title.
fn file_name_parts(file: &LandedFile) -> (Option<u32>, Option<u32>, String) {
    let stem = file
        .path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    number_and_title(&stem)
}

/// The leading number of a file name as `(disc, track)` plus the title
/// after it. Three digits are disc then track, as v2's naming wrote them
/// ("101 Title" is disc 1, track 1); a leading zero disc means none.
fn number_and_title(stem: &str) -> (Option<u32>, Option<u32>, String) {
    let trimmed = stem.trim_start();
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 3 {
        return (None, None, stem.trim().to_owned());
    }
    let rest = trimmed[digits.len()..]
        .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, '.' | '-' | '_' | ')'))
        .trim()
        .to_owned();
    if digits.len() == 3 {
        let disc = digits[..1].parse().ok().filter(|disc| *disc > 0);
        let track = digits[1..].parse().ok().filter(|track| *track > 0);
        // "100" names no track 0: read it as one plain number.
        if track.is_some() {
            return (disc, track, rest);
        }
    }
    (None, digits.parse().ok(), rest)
}

/// A disc number from the containing folder ("CD2", "Disc 3").
fn disc_folder(file: &LandedFile) -> Option<u32> {
    disc_in_folder(&file.path.parent()?.file_name()?.to_string_lossy())
}

fn disc_in_folder(folder: &str) -> Option<u32> {
    let words = super::specs::words(&folder.to_lowercase());
    for (index, word) in words.iter().enumerate() {
        for prefix in ["cd", "disc", "disk"] {
            if let Some(number) = word.strip_prefix(prefix)
                && !number.is_empty()
            {
                return number.parse().ok();
            }
            if word == prefix {
                return words.get(index + 1).and_then(|next| next.parse().ok());
            }
        }
    }
    None
}

/// The `(disc, track)` a client file name gives, from its leading number
/// and a disc folder: `Album\CD2\03 - Title.flac` is `(2, 3)`. `None`
/// when the name carries no track number.
pub fn position_in_name(filename: &str) -> Option<(u32, u32)> {
    let normalized = filename.replace('\\', "/");
    let path = std::path::Path::new(&normalized);
    let (name_disc, track, _) = number_and_title(&path.file_stem()?.to_string_lossy());
    let disc = name_disc
        .or_else(|| {
            path.parent()
                .and_then(std::path::Path::file_name)
                .and_then(|name| disc_in_folder(&name.to_string_lossy()))
        })
        .unwrap_or(1);
    Some((disc, track?))
}

fn clean(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

/// The most common non-empty value.
fn most_common<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for value in values.filter(|value| !value.is_empty()) {
        match counts.iter_mut().find(|(seen, _)| *seen == value) {
            Some((_, count)) => *count += 1,
            None => counts.push((value, 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(value, _)| value.to_owned())
}

fn is_various(name: &str) -> bool {
    matches!(fold(name).as_str(), "variousartists" | "various" | "va")
}

#[cfg(test)]
mod tests {
    use super::position_in_name;

    #[test]
    fn positions_come_from_names_and_disc_folders() {
        assert_eq!(
            position_in_name(r"@@peer\Album\CD2\03 - Title.flac"),
            Some((2, 3))
        );
        assert_eq!(position_in_name("Album/07. Song.mp3"), Some((1, 7)));
        assert_eq!(position_in_name("Album/cover.jpg"), None);
        assert_eq!(position_in_name("Album/101 Title.flac"), Some((1, 1)));
        assert_eq!(position_in_name("Album/214 - Title.flac"), Some((2, 14)));
    }
}
