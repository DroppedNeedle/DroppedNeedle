//! Rank Soulseek search results by folder against the tracklist being
//! fetched, before anything downloads.
//!
//! Results are grouped by peer and folder (a `CD1`/`Disc 2` subfolder
//! counts as its album folder). Each folder's files are paired with the
//! release's tracks from what the file names say: the leading track
//! number, the title words, and the advertised length. Folders are then
//! ordered the same way for album and single-track downloads:
//!
//! 1. completeness: how many of the release's tracks the folder holds;
//! 2. durations: how many paired files have a length off the release's;
//! 3. quality: the worst file's place in the quality recipe;
//! 4. speed: a free upload slot, a short queue, a fast uploader.
//!
//! This is soularr's folder check and Lidarr's import matching moved in
//! front of the download, with v2's pairing rules (a hard length miss
//! never pairs, a title the name contradicts needs the number and the
//! length to agree). For a single track, a folder holding little of the
//! album is a lone-track share: it only ranks after every album folder.

use std::collections::HashMap;

use super::policy::{DownloadPolicy, NOT_IMPORTABLE_EXTENSIONS};
use super::repository::SearchResult;
use crate::acquire::downloads::manifest::{ExpectedTrack, TrackPosition};
use crate::library::matching::strings::{fold, string_dist};

/// Audio containers a folder is judged on (WMA is not supported).
const AUDIO_EXTENSIONS: [&str; 12] = [
    "flac", "mp3", "m4a", "aac", "ogg", "opus", "wav", "aif", "aiff", "alac", "ape", "wv",
];

/// Words that say nothing about which song a file is.
const FILLER_WORDS: [&str; 7] = [
    "track", "tracks", "audio", "unknown", "untitled", "song", "songs",
];

/// Paired lengths further apart than this count against a folder.
const CLOSE_LENGTH_SECONDS: f64 = 3.0;

/// What the ranking is asked for.
#[derive(Debug, Clone, Copy)]
pub struct RankRequest<'a> {
    /// The release's tracks. Empty when no release is known: folders are
    /// then judged on their audio file count.
    pub tracklist: &'a [ExpectedTrack],
    /// Positions to fetch. For a single track these must all be present;
    /// for an album, empty means the whole folder, otherwise the folder
    /// gives whichever of them it holds (the tracks a short landing missed).
    pub wanted: &'a [TrackPosition],
    /// A single-track download: lone-track shares rank last.
    pub single_track: bool,
}

/// One ranked folder.
#[derive(Debug, Clone)]
pub struct FolderPick {
    /// Peer.
    pub username: String,
    /// Folder path on the peer.
    pub folder: String,
    /// Files to enqueue from it.
    pub files: Vec<SearchResult>,
    /// Release tracks the folder holds (audio files when no tracklist).
    pub coverage: usize,
    /// Tracks on the release (0 when unknown).
    pub total: usize,
    /// Paired files whose length is unknown or off the release's.
    pub duration_misses: usize,
    /// Worst recipe rank of the files to fetch (`usize::MAX` when one
    /// is outside the recipe).
    pub quality: usize,
    /// For a single track: the folder holds little of the album.
    pub lone_track: bool,
    has_free_slot: bool,
    queue_length: i64,
    upload_speed: i64,
}

/// Rank folders best first. Folders that cannot serve the wanted
/// positions are left out.
pub fn rank_folders(
    hits: &[SearchResult],
    request: RankRequest<'_>,
    policy: &DownloadPolicy,
) -> Vec<FolderPick> {
    let mut groups: HashMap<(String, String), Vec<&SearchResult>> = HashMap::new();
    for hit in hits {
        let folder = folder_of(&hit.filename).0;
        groups
            .entry((hit.username.clone(), folder))
            .or_default()
            .push(hit);
    }
    let multi_disc = request.tracklist.iter().any(|track| track.disc_number > 1);
    let mut picks: Vec<FolderPick> = groups
        .into_iter()
        .filter_map(|((username, folder), files)| {
            judge(username, folder, &files, request, multi_disc, policy)
        })
        .collect();
    picks.sort_by(|a, b| {
        a.lone_track
            .cmp(&b.lone_track)
            .then(b.coverage.cmp(&a.coverage))
            .then(a.duration_misses.cmp(&b.duration_misses))
            .then(a.quality.cmp(&b.quality))
            .then(b.has_free_slot.cmp(&a.has_free_slot))
            .then(a.queue_length.cmp(&b.queue_length))
            .then(b.upload_speed.cmp(&a.upload_speed))
            .then(a.username.cmp(&b.username))
            .then(a.folder.cmp(&b.folder))
    });
    picks
}

/// Score one folder, or `None` when it cannot serve the request.
fn judge(
    username: String,
    folder: String,
    files: &[&SearchResult],
    request: RankRequest<'_>,
    multi_disc: bool,
    policy: &DownloadPolicy,
) -> Option<FolderPick> {
    let audio: Vec<FileFacts<'_>> = files
        .iter()
        .filter(|hit| is_audio(&hit.extension))
        .map(|hit| FileFacts::read(hit, multi_disc))
        .collect();
    if audio.is_empty() {
        return None;
    }
    let (coverage, total, duration_misses, assigned) = if request.tracklist.is_empty() {
        (audio.len(), 0, 0, HashMap::new())
    } else {
        let assigned = assign(&audio, request.tracklist);
        let misses = assigned
            .iter()
            .filter(|(track, file)| {
                let expected = request.tracklist[**track].duration_seconds;
                match (audio[**file].hit.duration, expected) {
                    (Some(have), Some(want)) => (have - want).abs() > CLOSE_LENGTH_SECONDS,
                    _ => true,
                }
            })
            .count();
        (assigned.len(), request.tracklist.len(), misses, assigned)
    };
    let wanted_files = request.wanted.iter().map(|position| {
        let track = request.tracklist.iter().position(|track| {
            track.disc_number == i64::from(position.disc)
                && track.track_number == i64::from(position.track)
        })?;
        assigned.get(&track).map(|file| audio[*file].hit.clone())
    });
    let mut coverage = coverage;
    let to_fetch: Vec<SearchResult> = if request.wanted.is_empty() {
        files.iter().map(|hit| (*hit).clone()).collect()
    } else if request.single_track {
        wanted_files.collect::<Option<_>>()?
    } else {
        // An album's failover after a short landing: fetch whichever of the
        // missing tracks this folder holds, and rank on how many it holds.
        let held: Vec<SearchResult> = wanted_files.flatten().collect();
        coverage = held.len();
        held
    };
    if to_fetch.is_empty() {
        return None;
    }
    let quality = to_fetch
        .iter()
        .filter(|hit| is_audio(&hit.extension))
        .map(|hit| {
            policy
                .recipe_rank(&hit.extension, hit.bitrate, hit.bit_depth, hit.sample_rate)
                .unwrap_or(usize::MAX)
        })
        .max()
        .unwrap_or(usize::MAX);
    let lone_track = request.single_track && total > 1 && coverage < (total.div_ceil(2)).max(2);
    let first = files.first()?;
    Some(FolderPick {
        username,
        folder,
        files: to_fetch,
        coverage,
        total,
        duration_misses,
        quality,
        lone_track,
        has_free_slot: first.has_free_slot,
        queue_length: first.queue_length,
        upload_speed: first.upload_speed,
    })
}

fn is_audio(extension: &str) -> bool {
    let extension = extension.to_ascii_lowercase();
    AUDIO_EXTENSIONS.contains(&extension.as_str())
        && !NOT_IMPORTABLE_EXTENSIONS.contains(&extension.as_str())
}

/// The album folder of a remote path (a disc subfolder collapses into its
/// parent) and the disc number the subfolder names.
pub fn folder_of(path: &str) -> (String, Option<u32>) {
    let parts: Vec<&str> = path.split(['/', '\\']).collect();
    let dirs = &parts[..parts.len().saturating_sub(1)];
    match dirs.split_last() {
        Some((last, parents)) if !parents.is_empty() => match disc_of(last) {
            Some(disc) => (parents.join("\\"), Some(disc)),
            None => (dirs.join("\\"), None),
        },
        _ => (dirs.join("\\"), None),
    }
}

/// `CD2`, `Disc 2`, `disk-2` -> 2.
fn disc_of(name: &str) -> Option<u32> {
    let lower = name.trim().to_ascii_lowercase();
    let rest = ["disc", "disk", "cd"]
        .iter()
        .find_map(|word| lower.strip_prefix(word))?;
    let digits: String = rest
        .trim_start_matches([' ', '-', '_', '.'])
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().filter(|disc| *disc > 0)
}

/// What a file name says about the track it holds.
struct FileFacts<'a> {
    hit: &'a SearchResult,
    disc: Option<u32>,
    number: Option<u32>,
    /// The name without its number, folded; empty when it names nothing.
    signal: String,
    /// The same words before folding, for the edit distance.
    raw: String,
}

impl<'a> FileFacts<'a> {
    fn read(hit: &'a SearchResult, multi_disc: bool) -> Self {
        let (_, folder_disc) = folder_of(&hit.filename);
        let name = hit.filename.rsplit(['/', '\\']).next().unwrap_or("");
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        let (disc, number, rest) = leading_number(stem, multi_disc);
        let raw = rest
            .trim_start_matches([' ', '.', '-', '_', ')', ']'])
            .trim()
            .to_owned();
        let meaningful = raw
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .any(|word| {
                !word.chars().all(|ch| ch.is_ascii_digit())
                    && !FILLER_WORDS.contains(&word.to_lowercase().as_str())
            });
        Self {
            hit,
            disc: disc.or(folder_disc),
            number,
            signal: if meaningful {
                fold(&raw)
            } else {
                String::new()
            },
            raw,
        }
    }
}

/// Leading `03`, `1-03` (disc 1, track 3) or, on a multi-disc release,
/// `103`. Four digits read as a year, never a number.
fn leading_number(stem: &str, multi_disc: bool) -> (Option<u32>, Option<u32>, &str) {
    let trimmed = stem.trim_start();
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return (None, None, trimmed);
    }
    let (head, rest) = trimmed.split_at(digits);
    let Ok(value) = head.parse::<u32>() else {
        return (None, None, trimmed);
    };
    // `1-03`: a disc, a dash, then the track.
    if let Some(after) = rest.strip_prefix('-') {
        let track_digits = after.chars().take_while(char::is_ascii_digit).count();
        if (1..=3).contains(&track_digits)
            && let Ok(track) = after[..track_digits].parse::<u32>()
        {
            return (Some(value), Some(track), &after[track_digits..]);
        }
    }
    // The number must stand alone (`10cc` is a name).
    if rest.chars().next().is_some_and(char::is_alphanumeric) {
        return (None, None, trimmed);
    }
    if multi_disc && value > 100 && value % 100 > 0 {
        return (Some(value / 100), Some(value % 100), rest);
    }
    (None, Some(value), rest)
}

/// How a file's name compares with a track title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitleFit {
    Strong,
    Loose,
    Unknown,
    Conflict,
}

fn title_fit(file: &FileFacts<'_>, title: Option<&str>) -> TitleFit {
    let Some(title) = title else {
        return TitleFit::Unknown;
    };
    let wanted = fold(title);
    if wanted.is_empty() || file.signal.is_empty() {
        return TitleFit::Unknown;
    }
    if file.signal == wanted || string_dist(&file.raw, title) <= 0.15 {
        return TitleFit::Strong;
    }
    let contains = (wanted.chars().count() >= 4 && file.signal.contains(&wanted))
        || (file.signal.chars().count() >= 4 && wanted.contains(&file.signal));
    if contains {
        TitleFit::Loose
    } else {
        TitleFit::Conflict
    }
}

/// Whether two lengths agree: `Some(false)` is a hard miss (more than
/// 15 s or 10% apart), `None` when either is unknown.
fn lengths_agree(have: Option<f64>, want: Option<f64>) -> Option<bool> {
    let (have, want) = (have?, want?);
    if have <= 0.0 || want <= 0.0 {
        return None;
    }
    Some((have - want).abs() <= (want * 0.10).max(15.0))
}

/// How strongly one file is one track; `0` is not at all.
fn pair_strength(
    file: &FileFacts<'_>,
    track: &ExpectedTrack,
    absolute: usize,
    multi_disc: bool,
) -> u8 {
    let length = lengths_agree(file.hit.duration, track.duration_seconds);
    if length == Some(false) {
        return 0;
    }
    let disc_ok = file
        .disc
        .is_none_or(|disc| i64::from(disc) == track.disc_number);
    let number = file.number.map(|number| {
        let number = i64::from(number);
        (number == track.track_number && disc_ok)
            || (multi_disc && file.disc.is_none() && usize::try_from(number) == Ok(absolute))
    });
    match (number, title_fit(file, track.title.as_deref())) {
        (Some(true), TitleFit::Strong | TitleFit::Loose) => 4,
        (Some(true), TitleFit::Unknown) | (None, TitleFit::Strong) => 3,
        (None, TitleFit::Loose) => 2,
        (Some(true), TitleFit::Conflict) if length == Some(true) => 2,
        (Some(false), TitleFit::Strong) => 1,
        (Some(false), TitleFit::Loose) if length == Some(true) => 1,
        _ => 0,
    }
}

/// Pair files with tracks one to one, strongest pairs first. Answers
/// track index to file index.
fn assign(files: &[FileFacts<'_>], tracklist: &[ExpectedTrack]) -> HashMap<usize, usize> {
    let multi_disc = tracklist.iter().any(|track| track.disc_number > 1);
    let mut pairs: Vec<(u8, usize, usize)> = Vec::new();
    for (track_index, track) in tracklist.iter().enumerate() {
        for (file_index, file) in files.iter().enumerate() {
            let strength = pair_strength(file, track, track_index + 1, multi_disc);
            if strength > 0 {
                pairs.push((strength, track_index, file_index));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut by_track = HashMap::new();
    let mut used = vec![false; files.len()];
    for (_, track, file) in pairs {
        if by_track.contains_key(&track) || used[file] {
            continue;
        }
        used[file] = true;
        by_track.insert(track, file);
    }
    by_track
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(user: &str, path: &str, duration: f64, free: bool) -> SearchResult {
        SearchResult {
            username: user.to_owned(),
            filename: path.to_owned(),
            parent_directory: String::new(),
            size: 1,
            extension: "flac".to_owned(),
            bitrate: None,
            bit_depth: Some(16),
            sample_rate: Some(44_100),
            duration: Some(duration),
            has_free_slot: free,
            upload_speed: 1,
            queue_length: 0,
        }
    }

    fn track(number: i64, title: &str, seconds: f64) -> ExpectedTrack {
        ExpectedTrack {
            track_number: number,
            disc_number: 1,
            duration_seconds: Some(seconds),
            recording_mbid: None,
            title: Some(title.to_owned()),
            release_track_mbid: None,
        }
    }

    #[test]
    fn album_folder_beats_lone_track_and_only_the_wanted_file_is_fetched() {
        let tracklist = vec![
            track(1, "Safe From Harm", 331.0),
            track(2, "One Love", 288.0),
            track(3, "Blue Lines", 261.0),
            track(4, "Be Thankful", 249.0),
        ];
        let hits = vec![
            hit(
                "lone",
                "Singles\\Massive Attack - One Love.flac",
                288.0,
                true,
            ),
            hit(
                "album",
                "Music\\Blue Lines\\01 Safe From Harm.flac",
                331.0,
                false,
            ),
            hit("album", "Music\\Blue Lines\\02 One Love.flac", 289.0, false),
            hit(
                "album",
                "Music\\Blue Lines\\03 Blue Lines.flac",
                261.0,
                false,
            ),
            hit(
                "album",
                "Music\\Blue Lines\\04 Be Thankful.flac",
                249.0,
                false,
            ),
            // A wrong-length "02" never pairs with track 2.
            hit("short", "x\\Blue Lines\\02 One Love.flac", 120.0, true),
        ];
        let wanted = [TrackPosition { disc: 1, track: 2 }];
        let picks = rank_folders(
            &hits,
            RankRequest {
                tracklist: &tracklist,
                wanted: &wanted,
                single_track: true,
            },
            &DownloadPolicy::default(),
        );
        let users: Vec<&str> = picks.iter().map(|pick| pick.username.as_str()).collect();
        assert_eq!(users, ["album", "lone"]);
        assert!(!picks[0].lone_track && picks[1].lone_track);
        assert_eq!(picks[0].coverage, 4);
        let names: Vec<&str> = picks[0].files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["Music\\Blue Lines\\02 One Love.flac"]);
    }

    #[test]
    fn names_and_disc_folders_read() {
        assert_eq!(
            folder_of("a\\Album\\CD2\\01.flac"),
            ("a\\Album".to_owned(), Some(2))
        );
        assert_eq!(
            leading_number("1-03 Title", true),
            (Some(1), Some(3), " Title")
        );
        assert_eq!(leading_number("203 Title", true).0, Some(2));
        assert_eq!(leading_number("10cc - Song", false).1, None);
        assert_eq!(leading_number("1999 Mix", false).1, None);
    }
}
