//! Match landed files to the requested release.
//!
//! The library's matching engine does the scoring; this module feeds it
//! the way Lidarr's import does for a grabbed album: candidates restricted
//! to the requested release group (the pinned edition first, then the
//! release the files' tags name, then the group's editions from its
//! listing, or from a title search when the group cannot be listed), and
//! the closest conflict-free edition wins. Recording ids on
//! the files that no candidate carries are looked up first, in case
//! MusicBrainz merged them. Then it plans the files: what imports, what is
//! already in the library, what is held because its tags name another
//! track, and what the release does not account for at all.

use std::collections::{HashMap, HashSet};

use super::ports::{LandingLibrary, OwnedTracks};
use super::probe::{LandedFile, Landing};
use super::specs::{Target, album_version};
use droppedneedle_library::identify::providers::GROUP_FETCH;
use droppedneedle_library::identify::sources::ReleaseHit;
use droppedneedle_library::matching::decide::{ACCEPT_TRACK, EDITION_COHORT};
use droppedneedle_library::matching::score::{NAME_GATE, TrackPair};
use droppedneedle_library::matching::strings::{
    artist_subset, fold, string_dist, strip_edition_words,
};
use droppedneedle_library::matching::{
    LocalAlbum, LocalTrack, Release, ReleaseMatch, ReleaseTrack, usable_mbid,
};
use droppedneedle_library::matching::{match_release, unknown_recordings};
use droppedneedle_providers::RequestPriority;

/// The landing is background work: nobody waits on it.
const PRIORITY: RequestPriority = RequestPriority::BackgroundSync;

/// Editions of the requested group fetched after a title search, used
/// when the group's own listing is empty.
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
    /// For a single the person chose to take from its album: files tagged
    /// as a track of another group's release, by file index, with the
    /// recording their tags name. Matched without their ids.
    pub album_copies: HashMap<usize, String>,
    /// For a song request: files of another edition of the album, by file
    /// index, with the release their tags name ([`set_aside_editions`]).
    pub set_aside: HashMap<usize, String>,
    /// For a preferred edition: the release the files' tags name, when
    /// MusicBrainz places it in another release group.
    pub other_album: Option<String>,
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
/// an untagged album takes the requested names.
/// Those names prove nothing: a download's untagged files import only with
/// other evidence of their artist (`specs::artist_proof`).
pub fn local_album(files: &[LandedFile], target: &Target) -> LocalAlbum {
    let mut dropped = 0;
    let tracks: Vec<LocalTrack> = files
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
                duration_measured: file.header.duration_measured,
                recording_mbid: usable_mbid(tag.musicbrainz_recording_id.as_deref(), &mut dropped),
                release_track_mbid: usable_mbid(
                    tag.musicbrainz_release_track_id.as_deref(),
                    &mut dropped,
                ),
                release_mbid: usable_mbid(tag.musicbrainz_release_id.as_deref(), &mut dropped),
                fingerprint_recordings: Vec::new(),
                // Only library identification lets an ISRC overrule ids.
                isrcs: Vec::new(),
            }
        })
        .collect();
    if dropped > 0 {
        tracing::debug!(
            dropped,
            "malformed MusicBrainz ids in tags left out of matching"
        );
    }
    let album_name = most_common(files.iter().map(album_tag));
    let artist_tag = most_common(files.iter().map(|file| {
        file.tag
            .album_artist
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(file.tag.artist.trim())
    }));
    LocalAlbum {
        title: album_name.unwrap_or_else(|| target.album_title.clone()),
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
    let mut local = local_album(&landing.audio, target);
    let owned = if target.release_group_mbid.is_empty() {
        OwnedTracks::default()
    } else {
        library
            .owned(&target.release_group_mbid, target.planned_album_id())
            .await
    };
    let recalled = async {
        let Recalled {
            releases,
            other_album,
        } = recall(library, target, &local).await?;
        let album_copies = take_album_copies(library, target, &mut local, &releases).await?;
        let set_aside = set_aside_editions(target, &landing.audio, &mut local, &releases);
        let aliases = merged_recordings(library, &local, &releases, PRIORITY).await?;
        Ok::<_, String>((releases, aliases, album_copies, set_aside, other_album))
    };
    let (releases, aliases, album_copies, set_aside, other_album) = match recalled.await {
        Ok(recalled) => recalled,
        Err(detail) => {
            return MatchSummary {
                state: MatchState::Unavailable(detail),
                found: None,
                considered: Vec::new(),
                owned,
                album_copies: HashMap::new(),
                set_aside: HashMap::new(),
                other_album: None,
            };
        }
    };
    let mut summary = choose(&local, releases, &aliases, target);
    summary.owned = owned;
    summary.album_copies = album_copies;
    summary.set_aside = set_aside;
    summary.other_album = other_album;
    summary
}

/// For a song request with a pinned edition: the files that are the song
/// from another edition of its album ([`song_from_another_edition`]).
/// Their release ids are set aside, so they pair with the pinned release
/// like untagged files, and are filed and tagged as it. Answers each
/// one's release id. Nothing is set aside while the pin did not load.
fn set_aside_editions(
    target: &Target,
    files: &[LandedFile],
    local: &mut LocalAlbum,
    releases: &[Release],
) -> HashMap<usize, String> {
    let mut set_aside = HashMap::new();
    let (Some(_), Some(pinned)) = (target.release_mbid.as_deref(), releases.first()) else {
        return set_aside;
    };
    for (index, (file, track)) in files.iter().zip(local.tracks.iter_mut()).enumerate() {
        if let Some(release) = song_from_another_edition(target, file, pinned) {
            track.release_mbid = None;
            track.release_track_mbid = None;
            set_aside.insert(index, release);
        }
    }
    set_aside
}

/// A song request's file from another edition of `release`'s album, as
/// its own tags say: tagged with the album's group and another release,
/// carrying its own recording id, in a format whose tags the import
/// writes. An upgrade replaces a library file and keeps to the album's
/// edition. Answers the release the file's tags name. The landing and a
/// held file's import both read it, so the album's match is left alone
/// either way.
pub fn song_from_another_edition(
    target: &Target,
    file: &LandedFile,
    release: &Release,
) -> Option<String> {
    if !target.is_track || target.origin == crate::dispatch::DispatchOrigin::Upgrade.as_str() {
        return None;
    }
    let tag = &file.tag;
    let same_group = tag
        .musicbrainz_release_group_id
        .as_deref()
        .is_some_and(|tagged| {
            tagged
                .trim()
                .eq_ignore_ascii_case(&release.release_group_id)
        });
    let has_recording = usable_mbid(tag.musicbrainz_recording_id.as_deref(), &mut 0).is_some();
    let tagged = usable_mbid(tag.musicbrainz_release_id.as_deref(), &mut 0)?;
    (same_group
        && has_recording
        && droppedneedle_library::tags::save::writable(file.format)
        && !release.answers_to(&tagged))
    .then_some(tagged)
}

/// For a single the person chose to take from its album: files tagged as
/// a track of the release their tags name, when that release belongs to
/// another group and the track's title agrees with the requested song's,
/// are that album's copy of the song. A recording MusicBrainz has since
/// retired is found by the file's release track id. Their MusicBrainz ids
/// are set aside so they pair with the single by title and length, and the
/// album track's recordings are answered by file index.
async fn take_album_copies(
    library: &dyn LandingLibrary,
    target: &Target,
    local: &mut LocalAlbum,
    releases: &[Release],
) -> Result<HashMap<usize, String>, String> {
    let mut copies = HashMap::new();
    if !target.is_track || !target.album_version || target.release_group_mbid.is_empty() {
        return Ok(copies);
    }
    let Some(tagged) = local.tagged_release() else {
        return Ok(copies);
    };
    if releases.iter().any(|release| release.answers_to(&tagged)) {
        return Ok(copies);
    }
    let Some(album) = library
        .release(&tagged, PRIORITY)
        .await
        .map_err(|error| error.0)?
    else {
        return Ok(copies);
    };
    let requested = target.track_title.as_deref().or_else(|| {
        let recording = target.recording_mbid.as_deref()?;
        releases
            .iter()
            .flat_map(|release| &release.tracks)
            .find(|track| track.recording_id.eq_ignore_ascii_case(recording))
            .map(|track| track.title.as_str())
    });
    let Some(requested) = requested else {
        return Ok(copies);
    };
    if album
        .release_group_id
        .eq_ignore_ascii_case(&target.release_group_mbid)
    {
        return Ok(copies);
    }
    for (index, track) in local.tracks.iter_mut().enumerate() {
        let on_album = track
            .release_mbid
            .as_deref()
            .is_some_and(|id| album.answers_to(id));
        let tagged = |id: Option<&str>, on: &str| id.is_some_and(|id| on.eq_ignore_ascii_case(id));
        let recording = album
            .tracks
            .iter()
            .find(|on| tagged(track.recording_mbid.as_deref(), &on.recording_id))
            .or_else(|| {
                album
                    .tracks
                    .iter()
                    .find(|on| tagged(track.release_track_mbid.as_deref(), &on.id))
            })
            .filter(|on| same_song_title(&on.title, requested))
            .map(|on| on.recording_id.to_ascii_lowercase());
        let (true, Some(recording)) = (on_album, recording) else {
            continue;
        };
        track.recording_mbid = None;
        track.release_track_mbid = None;
        track.release_mbid = None;
        copies.insert(index, recording);
    }
    Ok(copies)
}

/// Whether two titles name one song, whatever edition words they carry.
fn same_song_title(a: &str, b: &str) -> bool {
    string_dist(&strip_edition_words(a), &strip_edition_words(b)) <= NAME_GATE
}

/// Whether a single's file that looks like its album's copy
/// ([`album_version`]) is shown to be one by more than its album tag: its
/// own MusicBrainz release group or release names another group, or
/// MusicBrainz lists an album of that title by the requested artist with
/// a track of the file's title. A lookup that fails shows nothing.
pub async fn album_copy_proven(
    library: &dyn LandingLibrary,
    target: &Target,
    landing: &Landing,
    matched: &MatchSummary,
) -> bool {
    let group = target.release_group_mbid.as_str();
    if !target.is_track || target.album_version || group.is_empty() {
        return false;
    }
    let Some(found) = matched.best() else {
        return false;
    };
    let (Some(album), Some(pair)) = (
        album_version(target, landing, found),
        found.requested_pair(target),
    ) else {
        return false;
    };
    let Some(file) = landing.audio.get(pair.local) else {
        return false;
    };
    let other_group =
        |id: &str| usable_mbid(Some(id), &mut 0).is_some_and(|id| !id.eq_ignore_ascii_case(group));
    let tag = &file.tag;
    if tag
        .musicbrainz_release_group_id
        .as_deref()
        .is_some_and(other_group)
    {
        return true;
    }
    if let Some(tagged) = tag
        .musicbrainz_release_id
        .as_deref()
        .and_then(|id| usable_mbid(Some(id), &mut 0))
        .filter(|id| !found.release.answers_to(id))
    {
        match library.release(&tagged, PRIORITY).await {
            Ok(Some(release)) if other_group(&release.release_group_id) => return true,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    error = error.0,
                    "tagged release unreadable; not proof of an album copy"
                );
            }
        }
    }
    let various = is_various(&target.artist_name);
    let artist = if various {
        ""
    } else {
        target.artist_name.as_str()
    };
    let hits = match library.search(&album, artist, PRIORITY).await {
        Ok(hits) => hits,
        Err(error) => {
            tracing::debug!(
                error = error.0,
                "album search failed; not proof of an album copy"
            );
            return false;
        }
    };
    let Some(hit) = hits
        .iter()
        .filter(|hit| other_group(&hit.release_group_id) && same_song_title(&hit.title, &album))
        .max_by_key(|hit| hit.score)
    else {
        return false;
    };
    let release = match library.release(&hit.id, PRIORITY).await {
        Ok(Some(release)) => release,
        Ok(None) => return false,
        Err(error) => {
            tracing::debug!(
                error = error.0,
                "album release unreadable; not proof of an album copy"
            );
            return false;
        }
    };
    let credit = release.artist_text();
    let same_artist = various
        || string_dist(&credit, &target.artist_name) <= NAME_GATE
        || artist_subset(&target.artist_name, &credit);
    let title = match tag.title.trim() {
        "" => file_name_parts(file).2,
        tagged => tagged.to_owned(),
    };
    same_artist
        && release
            .tracks
            .iter()
            .any(|track| same_song_title(&track.title, &title))
}

/// Whether the person chose the single's album copy for this file and
/// AcoustID heard the recording its own tags name.
pub fn heard_own_album_recording(summary: &MatchSummary, file: usize, heard: &[String]) -> bool {
    summary.album_copies.get(&file).is_some_and(|own| {
        heard
            .iter()
            .any(|recording| recording.eq_ignore_ascii_case(own))
    })
}

/// Album tags taggers write when they know none, read as no album tag.
const PLACEHOLDER_ALBUMS: [&str; 2] = ["unknown album", "unknown"];

/// The file's album tag, empty when it is blank or a placeholder.
pub fn album_tag(file: &LandedFile) -> &str {
    let album = file.tag.album.trim();
    if PLACEHOLDER_ALBUMS
        .iter()
        .any(|placeholder| album.eq_ignore_ascii_case(placeholder))
    {
        ""
    } else {
        album
    }
}

/// The ids MusicBrainz merged the files' recording MBIDs into, for every
/// one no candidate carries: a single retired id left unresolved vetoes
/// the release the files are. MusicBrainz is asked at `priority`.
pub async fn merged_recordings(
    library: &dyn LandingLibrary,
    local: &LocalAlbum,
    releases: &[Release],
    priority: RequestPriority,
) -> Result<HashMap<String, String>, String> {
    let mut aliases = HashMap::new();
    for mbid in unknown_recordings(local, releases) {
        if let Some(canonical) = library
            .canonical_recording(&mbid, priority)
            .await
            .map_err(|error| error.0)?
            && canonical != mbid
        {
            aliases.insert(mbid, canonical);
        }
    }
    Ok(aliases)
}

/// What [`recall`] found: the candidates, and for a preferred edition the
/// tagged release MusicBrainz places in another group.
struct Recalled {
    releases: Vec<Release>,
    other_album: Option<String>,
}

/// Candidate releases of the requested group. A pinned edition is the
/// only candidate: the files are judged against its tracklist and nothing
/// else. A preferred edition comes first, and its own group counts as the
/// requested one (MusicBrainz may have merged the group asked for). Then
/// the release the files' tags name: when MusicBrainz places it in the
/// group the two are the candidates, and when it places it in another
/// group only the preferred one is, with that release recorded. Files
/// the tags do not settle that make up the preferred edition whole have
/// it as their only candidate. Otherwise: the tagged release, then the
/// group's editions from its listing (closest track count first), else
/// from a title search.
async fn recall(
    library: &dyn LandingLibrary,
    target: &Target,
    local: &LocalAlbum,
) -> Result<Recalled, String> {
    let mut releases: Vec<Release> = Vec::new();
    let error = |error: droppedneedle_library::identify::sources::SourceError| error.0;
    let mut preferred_group: Option<String> = None;
    let in_group = |release: &Release, preferred_group: &Option<String>| {
        target.release_group_mbid.is_empty()
            || release
                .release_group_id
                .eq_ignore_ascii_case(&target.release_group_mbid)
            || preferred_group
                .as_deref()
                .is_some_and(|group| release.release_group_id.eq_ignore_ascii_case(group))
    };
    if let Some(pinned) = target.release_mbid.as_deref() {
        fetch(
            library.release(pinned, PRIORITY).await.map_err(error)?,
            &mut releases,
        );
        if !target.release_preferred {
            return Ok(Recalled {
                releases,
                other_album: None,
            });
        }
        preferred_group = releases
            .first()
            .map(|preferred| preferred.release_group_id.clone());
        let only =
            |releases: Vec<Release>, other_album: Option<String>| -> Result<Recalled, String> {
                Ok(Recalled {
                    releases,
                    other_album,
                })
            };
        if let Some(tagged) = local.tagged_release() {
            if releases.iter().any(|known| known.answers_to(&tagged)) {
                return only(releases, None);
            }
            // A preferred release MusicBrainz did not know is not asked
            // for twice.
            if !tagged.eq_ignore_ascii_case(pinned) {
                match library.release(&tagged, PRIORITY).await.map_err(error)? {
                    Some(found) if in_group(&found, &preferred_group) => {
                        fetch(Some(found), &mut releases);
                        return only(releases, None);
                    }
                    Some(_) => return only(releases, Some(tagged)),
                    // A release MusicBrainz does not know is no evidence.
                    None => {}
                }
            }
        }
        if releases
            .first()
            .is_some_and(|preferred| makes_up_whole(local, preferred))
        {
            return only(releases, None);
        }
    } else if let Some(tagged) = local.tagged_release() {
        fetch(
            library.release(&tagged, PRIORITY).await.map_err(error)?,
            &mut releases,
        );
    }
    releases.retain(|release| in_group(release, &preferred_group));
    let mut hits = Vec::new();
    if !target.release_group_mbid.is_empty() {
        hits = library
            .group_releases(&target.release_group_mbid, PRIORITY)
            .await
            .map_err(error)?;
    }
    let listed = !hits.is_empty();
    if !listed && !target.album_title.trim().is_empty() {
        let artist = if is_various(&target.artist_name) {
            ""
        } else {
            target.artist_name.as_str()
        };
        hits = library
            .search(&target.album_title, artist, PRIORITY)
            .await
            .map_err(error)?;
    }
    for id in pick_hits(&hits, target, local.tracks.len(), &releases, listed) {
        fetch(
            library.release(&id, PRIORITY).await.map_err(error)?,
            &mut releases,
        );
    }
    releases.retain(|release| in_group(release, &preferred_group));
    Ok(Recalled {
        releases,
        other_album: None,
    })
}

/// Whether the files make up `release` whole: every file pairs with one
/// of its tracks, none of its tracks is left over and no file's ids
/// contradict it.
fn makes_up_whole(local: &LocalAlbum, release: &Release) -> bool {
    let scored = match_release(local, release, &HashMap::new());
    scored.unmatched.is_empty() && scored.missing.is_empty() && scored.conflicts.is_empty()
}

/// Keep a fetched release unless it is already in the list.
fn fetch(found: Option<Release>, releases: &mut Vec<Release>) {
    if let Some(release) = found
        && !releases.iter().any(|known| known.id == release.id)
    {
        releases.push(release);
    }
}

/// Group editions worth a tracklist fetch. From the group's listing
/// (`listed`), the closest track count first, then the best score; from a
/// title search, the best score first.
fn pick_hits(
    hits: &[ReleaseHit],
    target: &Target,
    local_tracks: usize,
    known: &[Release],
    listed: bool,
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
    let gap = |hit: &ReleaseHit| {
        hit.track_count
            .map_or(usize::MAX, |count| (count as usize).abs_diff(local_tracks))
    };
    ranked.sort_by(|a, b| {
        let score = b.score.cmp(&a.score);
        if listed {
            gap(a).cmp(&gap(b)).then(score)
        } else {
            score.then_with(|| gap(a).cmp(&gap(b)))
        }
    });
    ranked
        .into_iter()
        .take(if listed { GROUP_FETCH } else { PER_GROUP })
        .map(|hit| hit.id.clone())
        .collect()
}

/// Score every candidate and pick the edition: the closest conflict-free
/// release by library distance, and among editions within
/// [`EDITION_COHORT`] of it the pinned one, then the one the tags name,
/// then the closest full tracklist (the matching engine's edition rule).
/// A preferred edition gives way to the one the tags name and to one the
/// files make up whole.
/// `aliases` maps retired recording MBIDs to the ones they were merged
/// into.
pub fn choose(
    local: &LocalAlbum,
    releases: Vec<Release>,
    aliases: &HashMap<String, String>,
    target: &Target,
) -> MatchSummary {
    if releases.is_empty() {
        return MatchSummary {
            state: MatchState::NoCandidate,
            found: None,
            considered: Vec::new(),
            owned: OwnedTracks::default(),
            album_copies: HashMap::new(),
            set_aside: HashMap::new(),
            other_album: None,
        };
    }
    let scored: Vec<ReleaseMatch> = releases
        .iter()
        .map(|release| match_release(local, release, aliases))
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
            album_copies: HashMap::new(),
            set_aside: HashMap::new(),
            other_album: None,
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
        if target.release_preferred {
            // The tags' release first, then an edition the files make up
            // whole, then the preferred one.
            let complete = scored[index].missing.is_empty();
            (!named, !complete, !pinned)
        } else {
            (!pinned, !named, false)
        }
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
            album_copies: HashMap::new(),
            set_aside: HashMap::new(),
            other_album: None,
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
        album_copies: HashMap::new(),
        set_aside: HashMap::new(),
        other_album: None,
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
    plan_holding(summary, target, files, &HashSet::new())
}

/// [`plan`], holding the paired files in `hold` as well, wherever they
/// would have gone.
pub fn plan_holding(
    summary: &MatchSummary,
    target: &Target,
    files: usize,
    hold: &HashSet<usize>,
) -> FilePlan {
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
        // A file too far from its track is held even when the library
        // owns that track: it may be another song, and an upgrade must
        // never replace the owned copy with it.
        if pair.distance > ACCEPT_TRACK || hold.contains(&pair.local) {
            out.held.push((pair.local, pair.track));
        } else if summary.owns(track) {
            out.owned.push(pair.local);
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
pub(super) fn file_name_parts(file: &LandedFile) -> (Option<u32>, Option<u32>, String) {
    let stem = file
        .path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    number_and_title(&stem)
}

/// The leading number of a file name as `(disc, track)` plus the title
/// after it. Three digits are disc then track, as older naming wrote
/// them ("101 Title" is disc 1, track 1); a leading zero disc means none.
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

pub(crate) fn is_various(name: &str) -> bool {
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
