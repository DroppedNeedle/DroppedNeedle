//! Which Deezer album fills a task, and at which bitrate. Pure: no I/O.
//!
//! A Deezer tracklist is paired one to one with the release the task
//! fetches, in this order:
//!
//! 1. by ISRC: the Deezer track's ISRC is one of the target track's, the
//!    lengths agree within 10 s (or one is unknown), and the position or the
//!    title agrees (every same-position pair is made before any by title);
//! 2. by position: same disc and position, lengths within 3 s, the title
//!    fits;
//! 3. with a length missing: same disc and position, the title fits.
//!
//! The pairs then give a verdict for a pinned edition, an edition the task
//! only prefers, or one song taken from its album ([`fit`]), and a
//! [`Score`] that orders the albums that fit. The barcode lookup that finds
//! a candidate is the caller's; a barcode hit still has to fit.
//!
//! The bitrate is chosen per add from the account's flags and the quality
//! settings ([`choose_bitrate`]), and stepped down when Deezer has no file
//! at the one asked for ([`step_down`]).

use std::cmp::Ordering;

use super::client::Bitrate;
use super::models::{Track, Tracklist};
use crate::downloads::plan::{ExpectedTrack, TrackPosition};
use crate::landing::quality::{RecipeFit, recipe_fit};
use crate::target::SearchTarget;
use crate::usenet::policy::{QualityTier, UsenetPolicy};
use droppedneedle_library::matching::strings::{fold, isrc_key, string_dist};

/// The title distance under which a name agrees, the bound slskd's folder
/// ranking uses for a strong title fit (`slskd/folders.rs`, `title_fit`).
const TITLE_BOUND: f64 = 0.15;

/// Lengths of a position pair agree within this, slskd's
/// `CLOSE_LENGTH_SECONDS` (`slskd/folders.rs`).
const POSITION_LENGTH_SECONDS: f64 = 3.0;

/// Lengths of an ISRC pair agree within this: the ISRC already says it is
/// the recording, so the length only catches a wrong cut.
const ISRC_LENGTH_SECONDS: f64 = 10.0;

/// Lengths this close count toward the score.
const EXACT_LENGTH_SECONDS: f64 = 1.0;

/// Version words that make a track another recording than the one asked
/// for: a title fits only when both titles name each as often, or the
/// target's album title names it. The first three match only as the word
/// itself (or its plural), the rest as a word's start.
const WHOLE_VERSION_WORDS: [&str; 3] = ["live", "edit", "demo"];
const STEM_VERSION_WORDS: [&str; 4] = ["remix", "acoustic", "instrumental", "karaoke"];

/// How the task judges an edition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A person, the library or a copy chose this edition: only it will do.
    Pin,
    /// No person chose the edition: another edition of the album may serve.
    Preferred,
    /// One song, taken from an album that serves under the preferred rule.
    Song,
}

impl Mode {
    /// The mode a target asks for.
    #[must_use]
    pub fn of(target: &SearchTarget) -> Mode {
        if target.track.is_some() {
            Mode::Song
        } else if target.release_preferred {
            Mode::Preferred
        } else {
            Mode::Pin
        }
    }
}

/// Why a tracklist does not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Misfit {
    /// The target has no tracklist to judge against.
    NoTargetTracks,
    /// A target track found no Deezer track.
    TargetTrackUnpaired,
    /// Deezer has tracks the target does not, and they may not be taken.
    ExtraTracks,
    /// A track the task needs is not served where Deemix runs.
    Unreadable,
    /// The disc counts differ and the pairing leaned on positions.
    DiscCount,
    /// Most of the target's ISRCs disagree with Deezer's.
    IsrcMismatch,
    /// No pair had a length or an ISRC to go on.
    NothingToGoOn,
    /// A song task's wanted track found no Deezer track.
    WantedUnpaired,
}

/// How well a fitting tracklist fits. Higher [`Score::ordering`] is better.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// Pairs made.
    pub pairs: usize,
    /// Pairs made by ISRC.
    pub isrc_pairs: usize,
    /// Pairs whose lengths agree within 1 s.
    pub close_pairs: usize,
    /// Target tracks whose ISRCs the Deezer track at their position
    /// disagrees with.
    pub isrc_mismatches: usize,
    /// Deezer marks the album explicit.
    pub explicit: bool,
    /// How far Deezer's album title is from the target's.
    pub title_distance: f64,
}

impl Score {
    /// Every pair was made by ISRC.
    #[must_use]
    pub fn every_pair_by_isrc(&self) -> bool {
        self.pairs > 0 && self.isrc_pairs == self.pairs
    }

    /// Order of preference: ISRC pairs, pairs within 1 s, fewest ISRC
    /// mismatches, explicit over clean, then the closer album title.
    #[must_use]
    pub fn ordering(&self, other: &Score) -> Ordering {
        self.isrc_pairs
            .cmp(&other.isrc_pairs)
            .then(self.close_pairs.cmp(&other.close_pairs))
            .then(other.isrc_mismatches.cmp(&self.isrc_mismatches))
            .then(self.explicit.cmp(&other.explicit))
            .then(other.title_distance.total_cmp(&self.title_distance))
    }
}

/// How a pair was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairKind {
    Isrc,
    Length,
    Position,
}

/// One target track and the Deezer track it pairs with, by index.
#[derive(Debug, Clone, Copy)]
struct Pair {
    target: usize,
    deezer: usize,
    kind: PairKind,
}

/// Whether `album` fits `target` under the target's mode, and how well.
pub fn fit(target: &SearchTarget, album: &Tracklist) -> Result<Score, Misfit> {
    let mode = Mode::of(target);
    let expected = &target.tracklist;
    if expected.is_empty() {
        return Err(Misfit::NoTargetTracks);
    }
    let pairs = pair(&target.album_title, expected, &album.tracks);
    if pairs.len() < expected.len() {
        return Err(Misfit::TargetTrackUnpaired);
    }
    let deezer_of = |target_index: usize| {
        pairs
            .iter()
            .find(|pair| pair.target == target_index)
            .map(|pair| &album.tracks[pair.deezer])
    };
    if mode == Mode::Song {
        let wanted_paired = !target.wanted.is_empty()
            && target
                .wanted
                .iter()
                .all(|wanted| expected.iter().any(|track| at(track, *wanted)));
        if !wanted_paired {
            return Err(Misfit::WantedUnpaired);
        }
    }
    let needed_unreadable = expected.iter().enumerate().any(|(index, track)| {
        let needed =
            target.wanted.is_empty() || target.wanted.iter().any(|wanted| at(track, *wanted));
        needed && deezer_of(index).is_some_and(|deezer| !deezer.readable)
    });
    if needed_unreadable {
        return Err(Misfit::Unreadable);
    }
    let mismatches = isrc_mismatches(expected, &album.tracks);
    let has_extras = pairs.len() < album.tracks.len();
    match mode {
        Mode::Pin => {
            if has_extras {
                return Err(Misfit::ExtraTracks);
            }
            let by_isrc_alone = pairs.iter().all(|pair| pair.kind == PairKind::Isrc);
            if !by_isrc_alone
                && disc_count(expected.iter().map(|track| track.disc_number))
                    != disc_count(
                        album
                            .tracks
                            .iter()
                            .map(|track| i64::from(track.disk_number)),
                    )
            {
                return Err(Misfit::DiscCount);
            }
            let with_isrcs = expected
                .iter()
                .filter(|track| !isrc_keys(track).is_empty())
                .count();
            if with_isrcs * 2 >= expected.len() && mismatches * 2 > with_isrcs {
                return Err(Misfit::IsrcMismatch);
            }
            if pairs.iter().all(|pair| pair.kind == PairKind::Position) {
                return Err(Misfit::NothingToGoOn);
            }
        }
        Mode::Preferred | Mode::Song => {
            if has_extras && !extras_allowed(target, album, &pairs) {
                return Err(Misfit::ExtraTracks);
            }
        }
    }
    let close_pairs = pairs
        .iter()
        .filter(|pair| {
            length_gap(&expected[pair.target], &album.tracks[pair.deezer])
                .is_some_and(|gap| gap <= EXACT_LENGTH_SECONDS)
        })
        .count();
    Ok(Score {
        pairs: pairs.len(),
        isrc_pairs: pairs
            .iter()
            .filter(|pair| pair.kind == PairKind::Isrc)
            .count(),
        close_pairs,
        isrc_mismatches: mismatches,
        explicit: album.explicit_lyrics == Some(true),
        title_distance: string_dist(&album.title, &target.album_title),
    })
}

/// The Deezer track the pairing gives the target track at `position`, when
/// it pairs.
pub fn paired_track<'a>(
    target: &SearchTarget,
    album: &'a Tracklist,
    position: TrackPosition,
) -> Option<&'a Track> {
    let index = target
        .tracklist
        .iter()
        .position(|track| at(track, position))?;
    pair(&target.album_title, &target.tracklist, &album.tracks)
        .into_iter()
        .find(|pair| pair.target == index)
        .map(|pair| &album.tracks[pair.deezer])
}

/// The best fitting album among `albums`, with its score. On a tie the
/// earlier album wins, so the caller's order (a barcode hit first) holds.
pub fn best_fit<'a>(
    target: &SearchTarget,
    albums: &'a [Tracklist],
) -> Option<(&'a Tracklist, Score)> {
    let mut best: Option<(&Tracklist, Score)> = None;
    for album in albums {
        let Ok(score) = fit(target, album) else {
            continue;
        };
        let better = best
            .as_ref()
            .is_none_or(|(_, held)| score.ordering(held) == Ordering::Greater);
        if better {
            best = Some((album, score));
        }
    }
    best
}

/// Pairs one to one: ISRC pairs at the same position for every target
/// first, then ISRC pairs whose title fits, then position pairs with
/// lengths, then position pairs where a length is missing. An ISRC pair at
/// the same position skips the title check: the ISRC names the recording.
fn pair(album_title: &str, expected: &[ExpectedTrack], tracks: &[Track]) -> Vec<Pair> {
    let target_keys: Vec<Vec<String>> = expected.iter().map(isrc_keys).collect();
    let deezer_keys: Vec<Option<String>> = tracks
        .iter()
        .map(|track| track.isrc.as_deref().and_then(isrc_key))
        .collect();
    let mut pairs: Vec<Pair> = Vec::new();
    let mut taken = vec![false; tracks.len()];
    for at_position in [true, false] {
        for (target, track) in expected.iter().enumerate() {
            if pairs.iter().any(|pair| pair.target == target) {
                continue;
            }
            let chosen = tracks.iter().enumerate().find(|(index, deezer)| {
                let same_recording = deezer_keys[*index]
                    .as_ref()
                    .is_some_and(|key| target_keys[target].contains(key));
                let placed = if at_position {
                    same_position(track, deezer)
                } else {
                    title_fits(track.title.as_deref(), album_title, deezer)
                };
                !taken[*index]
                    && same_recording
                    && length_gap(track, deezer).is_none_or(|gap| gap <= ISRC_LENGTH_SECONDS)
                    && placed
            });
            if let Some((deezer, _)) = chosen {
                taken[deezer] = true;
                pairs.push(Pair {
                    target,
                    deezer,
                    kind: PairKind::Isrc,
                });
            }
        }
    }
    for kind in [PairKind::Length, PairKind::Position] {
        for (target, track) in expected.iter().enumerate() {
            if pairs.iter().any(|pair| pair.target == target) {
                continue;
            }
            let chosen = tracks.iter().enumerate().find(|(index, deezer)| {
                let lengths = match (kind, length_gap(track, deezer)) {
                    (PairKind::Length, Some(gap)) => gap <= POSITION_LENGTH_SECONDS,
                    (PairKind::Position, None) => true,
                    _ => false,
                };
                !taken[*index]
                    && lengths
                    && same_position(track, deezer)
                    && title_fits(track.title.as_deref(), album_title, deezer)
            });
            if let Some((deezer, _)) = chosen {
                taken[deezer] = true;
                pairs.push(Pair {
                    target,
                    deezer,
                    kind,
                });
            }
        }
    }
    pairs
}

/// Under a preference, Deezer's extra tracks are a bonus-track edition of
/// the same album only when its title holds the target's, the artist
/// agrees, every target track sits at its own place, and every extra comes
/// after them. A compilation fails the title or the artist.
fn extras_allowed(target: &SearchTarget, album: &Tracklist, pairs: &[Pair]) -> bool {
    let wanted_title = fold(&target.album_title);
    if wanted_title.is_empty() || !fold(&album.title).contains(&wanted_title) {
        return false;
    }
    if !names_agree(&album.artist.name, &target.artist) {
        return false;
    }
    let in_place = pairs
        .iter()
        .all(|pair| same_position(&target.tracklist[pair.target], &album.tracks[pair.deezer]));
    if !in_place {
        return false;
    }
    let place = |track: &Track| (track.disk_number, track.track_position);
    let Some(last_paired) = pairs
        .iter()
        .map(|pair| place(&album.tracks[pair.deezer]))
        .max()
    else {
        return false;
    };
    album
        .tracks
        .iter()
        .enumerate()
        .filter(|(index, _)| !pairs.iter().any(|pair| pair.deezer == *index))
        .all(|(_, track)| place(track) > last_paired)
}

/// Target tracks with ISRCs where the Deezer track at their position
/// carries an ISRC that is not one of them.
fn isrc_mismatches(expected: &[ExpectedTrack], tracks: &[Track]) -> usize {
    expected
        .iter()
        .filter(|track| {
            let keys = isrc_keys(track);
            !keys.is_empty()
                && tracks
                    .iter()
                    .find(|deezer| same_position(track, deezer))
                    .and_then(|deezer| deezer.isrc.as_deref())
                    .and_then(isrc_key)
                    .is_some_and(|key| !keys.contains(&key))
        })
        .count()
}

/// A target track's ISRCs as matching compares them.
fn isrc_keys(track: &ExpectedTrack) -> Vec<String> {
    track
        .isrcs
        .iter()
        .filter_map(|isrc| isrc_key(isrc))
        .collect()
}

/// Whether a Deezer track's title agrees with the target's. Deezer splits
/// a version out into `title_version` or leaves it in `title_short`, while
/// MusicBrainz keeps it in the title. So the folded `title_short` or the
/// full title must be within the title bound, and the full title must name
/// each version word (live, remix and the like) as often as the target's
/// title does, unless the target's album title names that word.
fn title_fits(target: Option<&str>, album_title: &str, track: &Track) -> bool {
    let Some(target) = target else {
        return false;
    };
    let wanted = fold(target);
    if wanted.is_empty() {
        return false;
    }
    let full = if track.title_version.trim().is_empty() {
        track.title_short.clone()
    } else {
        format!("{} {}", track.title_short, track.title_version)
    };
    let close = |name: &str| {
        let folded = fold(name);
        !folded.is_empty() && (folded == wanted || string_dist(name, target) <= TITLE_BOUND)
    };
    (close(&track.title_short) || close(&full)) && versions_agree(target, album_title, &full)
}

/// Whether `deezer` names each version word as often as `target`, leaving
/// out the words `album_title` names ("Live at Leeds").
fn versions_agree(target: &str, album_title: &str, deezer: &str) -> bool {
    let album = version_words(album_title);
    let wanted = version_words(target);
    let seen = version_words(deezer);
    let count = |words: &[&str], term: &str| words.iter().filter(|word| **word == term).count();
    WHOLE_VERSION_WORDS
        .iter()
        .chain(STEM_VERSION_WORDS.iter())
        .all(|term| album.contains(term) || count(&wanted, term) == count(&seen, term))
}

/// The version words in `text`, one entry each time it names one.
fn version_words(text: &str) -> Vec<&'static str> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .filter_map(|word| version_word(&word.to_lowercase()))
        .collect()
}

/// The version word `word` is, if any.
fn version_word(word: &str) -> Option<&'static str> {
    WHOLE_VERSION_WORDS
        .iter()
        .find(|term| word == **term || word.strip_suffix('s') == Some(**term))
        .or_else(|| {
            STEM_VERSION_WORDS
                .iter()
                .find(|term| word.starts_with(**term))
        })
        .copied()
}

fn names_agree(left: &str, right: &str) -> bool {
    let folded = fold(left);
    !folded.is_empty() && (folded == fold(right) || string_dist(left, right) <= TITLE_BOUND)
}

fn same_position(track: &ExpectedTrack, deezer: &Track) -> bool {
    track.disc_number == i64::from(deezer.disk_number)
        && track.track_number == i64::from(deezer.track_position)
}

fn at(track: &ExpectedTrack, position: TrackPosition) -> bool {
    track.disc_number == i64::from(position.disc) && track.track_number == i64::from(position.track)
}

/// Seconds between the two lengths, when both are known.
fn length_gap(track: &ExpectedTrack, deezer: &Track) -> Option<f64> {
    let wanted = track.duration_seconds.filter(|seconds| *seconds > 0.0)?;
    let seen = deezer.duration.filter(|seconds| *seconds > 0)?;
    Some((wanted - f64::from(seen)).abs())
}

fn disc_count(discs: impl Iterator<Item = i64>) -> usize {
    let mut seen: Vec<i64> = discs.collect();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// What Deemix says the signed-in account may download. `None` means not
/// known yet, never "cannot".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccountFlags {
    pub can_stream_lossless: Option<bool>,
    pub can_stream_hq: Option<bool>,
}

impl AccountFlags {
    fn streams(self, bitrate: Bitrate) -> bool {
        match bitrate {
            Bitrate::Flac => self.can_stream_lossless != Some(false),
            Bitrate::Mp3At320 => self.can_stream_hq != Some(false),
            Bitrate::Mp3At128 => true,
        }
    }
}

/// Best first.
const BITRATES: [Bitrate; 3] = [Bitrate::Flac, Bitrate::Mp3At320, Bitrate::Mp3At128];

fn tier(bitrate: Bitrate) -> QualityTier {
    match bitrate {
        Bitrate::Flac => QualityTier::Lossless,
        Bitrate::Mp3At320 => QualityTier::Mp3_320,
        Bitrate::Mp3At128 => QualityTier::Low,
    }
}

/// The recipe step a bitrate's files take: Deezer's FLAC is 16-bit 44.1 kHz.
fn recipe_step(policy: &UsenetPolicy, bitrate: Bitrate) -> Option<usize> {
    let fit = match bitrate {
        Bitrate::Flac => recipe_fit(&policy.quality_recipe, "flac", None, Some(16), Some(44_100)),
        Bitrate::Mp3At320 => recipe_fit(&policy.quality_recipe, "mp3", Some(320), None, None),
        Bitrate::Mp3At128 => recipe_fit(&policy.quality_recipe, "mp3", Some(128), None, None),
    };
    match fit {
        RecipeFit::Step(step) => Some(step),
        RecipeFit::Unshown | RecipeFit::Outside => None,
    }
}

/// What an add is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AddFor {
    /// A person picked the album behind the confirm: the band and the
    /// recipe do not apply.
    pub waived: bool,
    /// The add upgrades a copy held below the cutoff: only a tier at or
    /// above `quality_cutoff` beats it.
    pub upgrade: bool,
}

/// The bitrate to add among `candidates`. An upgrade keeps only tiers at
/// or above the cutoff. Then a waived pick takes the best the account can
/// stream; any other add takes, of those the account streams and the band
/// and recipe allow, the earliest recipe step, the higher tier on a tie.
fn best_of(
    candidates: impl Iterator<Item = Bitrate>,
    account: AccountFlags,
    policy: &UsenetPolicy,
    add: AddFor,
) -> Option<Bitrate> {
    let mut usable = candidates
        .filter(|bitrate| account.streams(*bitrate))
        .filter(|bitrate| !add.upgrade || tier(*bitrate) >= policy.quality_cutoff);
    if add.waived {
        return usable.next();
    }
    usable
        .filter(|bitrate| policy.accepts_tier(tier(*bitrate)))
        .filter_map(|bitrate| recipe_step(policy, bitrate).map(|step| (step, bitrate)))
        .min_by_key(|(step, bitrate)| (*step, std::cmp::Reverse(tier(*bitrate))))
        .map(|(_, bitrate)| bitrate)
}

/// The bitrate to add, or `None` when the account gives nothing the
/// settings allow.
#[must_use]
pub fn choose_bitrate(
    account: AccountFlags,
    policy: &UsenetPolicy,
    add: AddFor,
) -> Option<Bitrate> {
    best_of(BITRATES.into_iter(), account, policy, add)
}

/// The next bitrate below `current` to add after Deezer answered that the
/// album has no file at `current`, under the same rules as
/// [`choose_bitrate`]; `None` when none is left.
#[must_use]
pub fn step_down(
    current: Bitrate,
    account: AccountFlags,
    policy: &UsenetPolicy,
    add: AddFor,
) -> Option<Bitrate> {
    let below = BITRATES
        .into_iter()
        .filter(move |bitrate| tier(*bitrate) < tier(current));
    best_of(below, account, policy, add)
}

#[cfg(test)]
mod tests {
    use droppedneedle_kernel::runtime_config::sections::recipe_for_range;

    use super::*;
    use crate::deemix::models::{Artist, TracklistAnswer};
    use crate::target::TrackWant;

    fn recorded(json: &str) -> Tracklist {
        serde_json::from_str::<TracklistAnswer>(json)
            .expect("the recording decodes")
            .into_tracklist()
            .expect("the recording reads")
            .expect("the recording has tracks")
    }

    fn in_rainbows() -> Tracklist {
        recorded(include_str!(
            "../../../tests/fixtures/deemix/tracklist_album.json"
        ))
    }

    fn mellon_collie() -> Tracklist {
        recorded(include_str!(
            "../../../tests/fixtures/deemix/tracklist_album_multidisc.json"
        ))
    }

    /// The release as MusicBrainz would give it for the Deezer album:
    /// lengths `drift` seconds off, ISRCs when `isrcs` is set.
    fn release_of(album: &Tracklist, drift: f64, isrcs: bool) -> Vec<ExpectedTrack> {
        album
            .tracks
            .iter()
            .map(|track| ExpectedTrack {
                track_number: i64::from(track.track_position),
                disc_number: i64::from(track.disk_number),
                duration_seconds: track.duration.map(|seconds| f64::from(seconds) + drift),
                recording_mbid: None,
                title: Some(track.title_short.clone()),
                release_track_mbid: None,
                isrcs: if isrcs {
                    track.isrc.iter().cloned().collect()
                } else {
                    Vec::new()
                },
            })
            .collect()
    }

    fn target(
        album: &str,
        artist: &str,
        tracklist: Vec<ExpectedTrack>,
        mode: Mode,
    ) -> SearchTarget {
        SearchTarget {
            artist: artist.to_owned(),
            album_title: album.to_owned(),
            year: None,
            tracklist,
            barcode: None,
            wanted: Vec::new(),
            track: None,
            release_preferred: mode == Mode::Preferred,
        }
    }

    fn song(mut target: SearchTarget, disc: u32, track: u32) -> SearchTarget {
        target.release_preferred = false;
        target.wanted = vec![TrackPosition { disc, track }];
        target.track = Some(TrackWant {
            artist: target.artist.clone(),
            title: String::new(),
            duration_seconds: None,
            no_album: None,
        });
        target
    }

    /// A Deezer track: disc, position, length, ISRC, title and version.
    type TrackRow<'a> = (u32, u32, Option<u32>, Option<&'a str>, &'a str, &'a str);

    fn built(title: &str, artist: &str, tracks: &[TrackRow<'_>]) -> Tracklist {
        Tracklist {
            id: 1,
            title: title.to_owned(),
            artist: Artist {
                name: artist.to_owned(),
            },
            explicit_lyrics: Some(false),
            tracks: tracks
                .iter()
                .enumerate()
                .map(
                    |(index, (disc, position, duration, isrc, title, version))| Track {
                        id: 100 + u64::try_from(index).unwrap_or_default(),
                        disk_number: *disc,
                        track_position: *position,
                        duration: *duration,
                        isrc: isrc.map(str::to_owned),
                        title_short: (*title).to_owned(),
                        title_version: (*version).to_owned(),
                        readable: true,
                    },
                )
                .collect(),
        }
    }

    fn expected(
        disc: i64,
        number: i64,
        duration: Option<f64>,
        isrc: Option<&str>,
        title: &str,
    ) -> ExpectedTrack {
        ExpectedTrack {
            track_number: number,
            disc_number: disc,
            duration_seconds: duration,
            recording_mbid: None,
            title: Some(title.to_owned()),
            release_track_mbid: None,
            isrcs: isrc.iter().map(|isrc| (*isrc).to_owned()).collect(),
        }
    }

    /// The rows that do not give what they should, for one assert that
    /// lists them all.
    fn wrong_rows<T: PartialEq + std::fmt::Debug>(
        rows: impl IntoIterator<Item = (String, T, T)>,
    ) -> String {
        rows.into_iter()
            .filter(|(_, got, want)| got != want)
            .map(|(case, got, want)| format!("{case}: got {got:?}, want {want:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `album` with the track at `index` retitled, its ISRC dropped so
    /// only the title can pair it.
    fn retitled(album: &Tracklist, index: usize, short: &str, version: &str) -> Tracklist {
        let mut album = album.clone();
        album.tracks[index].isrc = None;
        album.tracks[index].title_short = short.to_owned();
        album.tracks[index].title_version = version.to_owned();
        album
    }

    /// The In Rainbows release without ISRCs, the track at `index` titled
    /// `title` as MusicBrainz would.
    fn rainbows_titled(rainbows: &Tracklist, index: usize, title: &str) -> SearchTarget {
        let mut release = release_of(rainbows, 0.0, false);
        release[index].title = Some(title.to_owned());
        target("In Rainbows", "Radiohead", release, Mode::Pin)
    }

    /// One tracklist against one target a row: whether it fits under the
    /// target's mode, and for a fit how many pairs were by ISRC.
    #[test]
    fn a_tracklist_fits_the_edition_only_by_its_rules() {
        let rainbows = in_rainbows();
        let collie = mellon_collie();
        let rainbows_release = |drift, isrcs, mode| {
            target(
                "In Rainbows",
                "Radiohead",
                release_of(&rainbows, drift, isrcs),
                mode,
            )
        };
        let nine_of_ten = |mode| {
            let mut release = release_of(&rainbows, 0.4, true);
            release.pop();
            target("In Rainbows", "Radiohead", release, mode)
        };

        // The CD release: titles without Deezer's "(Remastered 2012)", its
        // 28 tracks on one disc numbered through, lengths a few seconds off.
        let cd_twin = {
            let mut release = release_of(&collie, -4.0, true);
            for (index, track) in release.iter_mut().enumerate() {
                track.disc_number = 1;
                track.track_number = i64::try_from(index + 1).unwrap_or_default();
            }
            target(
                "Mellon Collie and the Infinite Sadness",
                "The Smashing Pumpkins",
                release,
                Mode::Pin,
            )
        };

        // The same two discs as MusicBrainz spells them, with no ISRCs.
        let collie_as_musicbrainz = {
            let mut release = release_of(&collie, 1.0, false);
            let spelled = [
                (8, "Love (feat. James Iha)"),
                (9, "Cupid de Locke"),
                (16, "Thirty\u{2010}Three"),
                (19, "Tales of a Scorched Earth"),
                (22, "X.Y.U."),
                (25, "Lily (My One and Only)"),
            ];
            for (index, title) in spelled {
                release[index].title = Some(title.to_owned());
            }
            target(
                "Mellon Collie and the Infinite Sadness",
                "The Smashing Pumpkins",
                release,
                Mode::Pin,
            )
        };

        let mut unreadable = rainbows.clone();
        unreadable.tracks[4].readable = false;

        let with_bonus = |album: &Tracklist, position: u32| {
            let mut album = album.clone();
            let mut bonus = album.tracks[9].clone();
            bonus.disk_number = 1;
            bonus.track_position = position;
            bonus.isrc = None;
            bonus.title_short = "Someone Else".to_owned();
            album.tracks.push(bonus);
            album
        };
        let compilation = {
            let mut album = with_bonus(&rainbows, 11);
            album.title = "Alternative Classics".to_owned();
            album.artist.name = "Various Artists".to_owned();
            album
        };
        let other_artist = {
            let mut album = rainbows.clone();
            album.artist.name = "Various Artists".to_owned();
            album
        };
        let bonus_first = {
            let mut album = rainbows.clone();
            for track in &mut album.tracks {
                track.track_position += 1;
            }
            with_bonus(&album, 1)
        };
        // The album on disc 2 with a bonus disc before it: every track at
        // its own place, the extra before them.
        let on_disc_two = {
            let mut release = release_of(&rainbows, 0.0, true);
            for track in &mut release {
                track.disc_number = 2;
            }
            target("In Rainbows", "Radiohead", release, Mode::Preferred)
        };
        let bonus_disc_first = {
            let mut album = rainbows.clone();
            for track in &mut album.tracks {
                track.disk_number = 2;
            }
            with_bonus(&album, 1)
        };
        // The first two tracks swapped and a bonus at the end.
        let reordered = {
            let mut album = with_bonus(&rainbows, 11);
            album.tracks[0].track_position = 2;
            album.tracks[1].track_position = 1;
            album
        };

        let live = retitled(&rainbows, 2, "Nude", "(Live From The Basement)");
        let remix = retitled(&rainbows, 6, "Reckoner", "(Remix)");
        let live_in_short = retitled(&rainbows, 0, "15 Step (Live)", "");
        let one_live = retitled(&rainbows, 0, "One", "(Live)");
        let four_tet = retitled(&rainbows, 6, "Reckoner", "(Four Tet Remix)");
        let radio_edit = retitled(&rainbows, 2, "Nude", "(Radio Edit)");
        let live_forever_live = retitled(&rainbows, 0, "Live Forever", "(Live)");
        let live_forever = retitled(&rainbows, 0, "Live Forever", "(Remastered)");
        let isrc_live = {
            let mut album = rainbows.clone();
            album.tracks[2].title_version = "(Live)".to_owned();
            album
        };
        let live_album = {
            let mut album = rainbows.clone();
            album.title = "In Rainbows Live".to_owned();
            for track in &mut album.tracks {
                track.isrc = None;
                track.title_version = "(Live)".to_owned();
            }
            album
        };

        // Two discs asked for, one on Deezer: the first two tracks pair by
        // position, the third only by its ISRC.
        let discs_target = target(
            "Album",
            "Artist",
            vec![
                expected(1, 1, Some(200.0), None, "One"),
                expected(1, 2, Some(210.0), None, "Two"),
                expected(2, 1, Some(220.0), Some("GBAAA0000003"), "Three"),
            ],
            Mode::Pin,
        );
        let one_disc = built(
            "Album",
            "Artist",
            &[
                (1, 1, Some(200), None, "One", ""),
                (1, 2, Some(210), None, "Two", ""),
                (1, 3, Some(220), Some("GBAAA0000003"), "Three", ""),
            ],
        );

        // One recording at two places. The track at the second place holds
        // the ISRC; the first carries another, so only its title and length
        // pair it. Taking the second for the first by title would leave the
        // second unpaired.
        let shared_isrc_target = target(
            "Album",
            "Artist",
            vec![
                expected(1, 1, Some(200.0), Some("GBAAA0000001"), "Intro"),
                expected(1, 2, Some(200.0), Some("GBAAA0000001"), "Intro"),
            ],
            Mode::Pin,
        );
        let shared_isrc = built(
            "Album",
            "Artist",
            &[
                (1, 1, Some(200), Some("GBAAA0000009"), "Intro", ""),
                (1, 2, Some(200), Some("GBAAA0000001"), "Intro", ""),
            ],
        );

        // Deezer's ISRCs at the first `count` positions are another
        // recording's.
        let mismatched = |count: usize| {
            let mut album = rainbows.clone();
            for track in album.tracks.iter_mut().take(count) {
                track.isrc = Some(format!("USXXX{:07}", track.id % 10_000_000));
            }
            album
        };
        let mismatched_all = mismatched(10);
        let mismatched_half = mismatched(5);
        let mismatched_four = mismatched(4);
        let isrcs_on_four = {
            let mut release = release_of(&rainbows, 0.0, true);
            for track in release.iter_mut().skip(4) {
                track.isrcs.clear();
            }
            target("In Rainbows", "Radiohead", release, Mode::Pin)
        };

        let lengthless = {
            let mut release = release_of(&rainbows, 0.0, false);
            for track in &mut release {
                track.duration_seconds = None;
            }
            release
        };

        type Row<'a> = (&'a str, SearchTarget, &'a Tracklist, Result<usize, Misfit>);
        let rows: Vec<Row<'_>> = vec![
            (
                "exact pin by ISRC",
                rainbows_release(1.4, true, Mode::Pin),
                &rainbows,
                Ok(10),
            ),
            (
                "ISRC pair 9 s off",
                rainbows_release(9.0, true, Mode::Pin),
                &rainbows,
                Ok(10),
            ),
            (
                "ISRC pair 11 s off",
                rainbows_release(11.0, true, Mode::Pin),
                &rainbows,
                Err(Misfit::TargetTrackUnpaired),
            ),
            (
                "pin by position",
                rainbows_release(2.5, false, Mode::Pin),
                &rainbows,
                Ok(0),
            ),
            (
                "position pin with a length 4 s off",
                rainbows_release(4.0, false, Mode::Pin),
                &rainbows,
                Err(Misfit::TargetTrackUnpaired),
            ),
            ("digital twin of a CD pin", cd_twin, &collie, Ok(28)),
            (
                "MusicBrainz's spelling of the titles",
                collie_as_musicbrainz,
                &collie,
                Ok(0),
            ),
            (
                "same-position ISRC pairs come first",
                shared_isrc_target,
                &shared_isrc,
                Ok(1),
            ),
            (
                "bonus track under a pin",
                nine_of_ten(Mode::Pin),
                &rainbows,
                Err(Misfit::ExtraTracks),
            ),
            (
                "bonus track under a preference",
                nine_of_ten(Mode::Preferred),
                &rainbows,
                Ok(9),
            ),
            (
                "bonus-track edition by another artist",
                nine_of_ten(Mode::Preferred),
                &other_artist,
                Err(Misfit::ExtraTracks),
            ),
            (
                "compilation under a preference",
                rainbows_release(0.0, true, Mode::Preferred),
                &compilation,
                Err(Misfit::ExtraTracks),
            ),
            (
                "bonus track before the album under a preference",
                rainbows_release(0.0, true, Mode::Preferred),
                &bonus_first,
                Err(Misfit::ExtraTracks),
            ),
            (
                "bonus disc before the album under a preference",
                on_disc_two,
                &bonus_disc_first,
                Err(Misfit::ExtraTracks),
            ),
            (
                "tracks out of place with a bonus under a preference",
                rainbows_release(0.0, true, Mode::Preferred),
                &reordered,
                Err(Misfit::ExtraTracks),
            ),
            (
                "live version",
                rainbows_release(0.0, false, Mode::Pin),
                &live,
                Err(Misfit::TargetTrackUnpaired),
            ),
            (
                "live version left in the short title",
                rainbows_release(0.0, false, Mode::Pin),
                &live_in_short,
                Err(Misfit::TargetTrackUnpaired),
            ),
            (
                "live version asked for",
                rainbows_titled(&rainbows, 2, "Nude (Live)"),
                &live,
                Ok(0),
            ),
            (
                "short live title asked for",
                rainbows_titled(&rainbows, 0, "One (Live)"),
                &one_live,
                Ok(0),
            ),
            (
                "remix version",
                rainbows_release(0.0, false, Mode::Pin),
                &remix,
                Err(Misfit::TargetTrackUnpaired),
            ),
            (
                "remix asked for, studio track",
                rainbows_titled(&rainbows, 6, "Reckoner (Remix)"),
                &rainbows,
                Err(Misfit::TargetTrackUnpaired),
            ),
            (
                "remix asked for",
                rainbows_titled(&rainbows, 6, "Reckoner (Four Tet Remix)"),
                &four_tet,
                Ok(0),
            ),
            (
                "radio edit asked for",
                rainbows_titled(&rainbows, 2, "Nude (Radio Edit)"),
                &radio_edit,
                Ok(0),
            ),
            (
                "a version word in the song's own name",
                rainbows_titled(&rainbows, 0, "Live Forever"),
                &live_forever,
                Ok(0),
            ),
            (
                "a live take of a song named for it",
                rainbows_titled(&rainbows, 0, "Live Forever"),
                &live_forever_live,
                Err(Misfit::TargetTrackUnpaired),
            ),
            (
                "live album named live",
                target(
                    "In Rainbows Live",
                    "Radiohead",
                    release_of(&rainbows, 0.0, false),
                    Mode::Pin,
                ),
                &live_album,
                Ok(0),
            ),
            (
                "ISRC pair at its place with a version",
                rainbows_release(0.0, true, Mode::Pin),
                &isrc_live,
                Ok(10),
            ),
            (
                "unreadable needed track",
                rainbows_release(0.0, true, Mode::Pin),
                &unreadable,
                Err(Misfit::Unreadable),
            ),
            (
                "unreadable wanted song",
                song(rainbows_release(0.0, true, Mode::Pin), 1, 5),
                &unreadable,
                Err(Misfit::Unreadable),
            ),
            (
                "song beside an unreadable track",
                song(rainbows_release(0.0, true, Mode::Pin), 1, 7),
                &unreadable,
                Ok(10),
            ),
            (
                "song from a bonus-track edition",
                song(nine_of_ten(Mode::Pin), 1, 3),
                &rainbows,
                Ok(9),
            ),
            // Target resolution takes the wanted positions from the same
            // release, so it never makes this shape; the row guards it.
            (
                "song whose track the release lacks",
                song(nine_of_ten(Mode::Pin), 1, 10),
                &rainbows,
                Err(Misfit::WantedUnpaired),
            ),
            (
                "disc-count difference",
                discs_target,
                &one_disc,
                Err(Misfit::DiscCount),
            ),
            (
                "mostly mismatched ISRCs under a pin",
                rainbows_release(0.0, true, Mode::Pin),
                &mismatched_all,
                Err(Misfit::IsrcMismatch),
            ),
            (
                "half the ISRCs mismatched under a pin",
                rainbows_release(0.0, true, Mode::Pin),
                &mismatched_half,
                Ok(5),
            ),
            (
                "ISRCs on fewer than half the tracks, all mismatched",
                isrcs_on_four,
                &mismatched_four,
                Ok(0),
            ),
            (
                "mostly mismatched ISRCs under a preference",
                rainbows_release(0.0, true, Mode::Preferred),
                &mismatched_all,
                Ok(0),
            ),
            (
                "no lengths and no ISRCs under a pin",
                target("In Rainbows", "Radiohead", lengthless.clone(), Mode::Pin),
                &rainbows,
                Err(Misfit::NothingToGoOn),
            ),
            (
                "no lengths and no ISRCs under a preference",
                target("In Rainbows", "Radiohead", lengthless, Mode::Preferred),
                &rainbows,
                Ok(0),
            ),
        ];
        let wrong = wrong_rows(rows.into_iter().map(|(case, target, album, want)| {
            let got = fit(&target, album).map(|score| score.isrc_pairs);
            (case.to_owned(), got, want)
        }));
        assert!(wrong.is_empty(), "{wrong}");

        let mismatch_score = fit(
            &rainbows_release(0.0, true, Mode::Preferred),
            &mismatched_all,
        );
        assert_eq!(
            mismatch_score.map(|score| score.isrc_mismatches),
            Ok(10),
            "a preference counts the mismatches it lets through"
        );
    }

    /// The best of several fitting albums: ISRC pairs first, then close
    /// lengths, fewer ISRC mismatches, explicit over clean, the closer title.
    #[test]
    fn the_best_fit_follows_the_score_order() {
        let rainbows = in_rainbows();
        let release = target(
            "In Rainbows",
            "Radiohead",
            release_of(&rainbows, 0.0, true),
            Mode::Preferred,
        );
        let mut explicit = rainbows.clone();
        explicit.id = 2;
        explicit.explicit_lyrics = Some(true);
        let mut no_isrcs = explicit.clone();
        no_isrcs.id = 3;
        for track in &mut no_isrcs.tracks {
            track.isrc = None;
        }
        let mut farther_title = rainbows.clone();
        farther_title.id = 4;
        farther_title.title = "In Rainbows (Deluxe)".to_owned();
        let mut off_lengths = explicit.clone();
        off_lengths.id = 5;
        for track in &mut off_lengths.tracks {
            track.duration = track.duration.map(|seconds| seconds + 2);
        }
        let mut twin = rainbows.clone();
        twin.id = 6;
        let mut clean_no_isrcs = no_isrcs.clone();
        clean_no_isrcs.id = 7;
        clean_no_isrcs.explicit_lyrics = Some(false);
        let mut explicit_mismatched = explicit.clone();
        explicit_mismatched.id = 8;
        for track in &mut explicit_mismatched.tracks {
            track.isrc = Some(format!("USXXX{:07}", track.id % 10_000_000));
        }

        let rows: Vec<(&str, Vec<Tracklist>, u64)> = vec![
            (
                "explicit beats clean",
                vec![rainbows.clone(), explicit.clone()],
                2,
            ),
            (
                "explicit beats clean in either order",
                vec![explicit.clone(), rainbows.clone()],
                2,
            ),
            (
                "ISRC pairs beat explicit",
                vec![no_isrcs.clone(), rainbows.clone()],
                14_880_659,
            ),
            (
                "close lengths beat explicit",
                vec![off_lengths, rainbows.clone()],
                14_880_659,
            ),
            (
                "fewer ISRC mismatches beat explicit",
                vec![explicit_mismatched, clean_no_isrcs],
                7,
            ),
            (
                "the closer title wins",
                vec![farther_title, rainbows.clone()],
                14_880_659,
            ),
            (
                "a tie keeps the first",
                vec![rainbows.clone(), twin],
                14_880_659,
            ),
        ];
        let wrong = wrong_rows(rows.into_iter().map(|(case, albums, want)| {
            let best = best_fit(&release, &albums).map(|(album, _)| album.id);
            (case.to_owned(), best, Some(want))
        }));
        assert!(wrong.is_empty(), "{wrong}");
        assert!(
            best_fit(&release, &[mellon_collie()]).is_none(),
            "nothing that fits is no pick"
        );
    }

    fn policy(min: &str, max: &str, order: &[&str]) -> UsenetPolicy {
        let order: Vec<String> = order.iter().map(|tier| (*tier).to_owned()).collect();
        UsenetPolicy {
            quality_min: QualityTier::parse(min).expect("a tier"),
            quality_max: QualityTier::parse(max).expect("a tier"),
            quality_recipe: recipe_for_range(min, max, &order),
            ..UsenetPolicy::default()
        }
    }

    fn flags(lossless: Option<bool>, hq: Option<bool>) -> AccountFlags {
        AccountFlags {
            can_stream_lossless: lossless,
            can_stream_hq: hq,
        }
    }

    const PLAIN: AddFor = AddFor {
        waived: false,
        upgrade: false,
    };
    const WAIVED: AddFor = AddFor {
        waived: true,
        upgrade: false,
    };
    const UPGRADE: AddFor = AddFor {
        waived: false,
        upgrade: true,
    };
    const WAIVED_UPGRADE: AddFor = AddFor {
        waived: true,
        upgrade: true,
    };

    /// The bitrate an add asks for, one account and set of settings a row,
    /// and the step down after Deezer has no file at the first.
    #[test]
    fn the_bitrate_follows_the_account_and_the_settings() {
        use Bitrate::{Flac, Mp3At128, Mp3At320};
        let default = policy("mp3_320", "lossless", &[]);
        let mp3_first = policy("mp3_320", "lossless", &["mp3_320", "lossless"]);
        let anything = policy("low", "lossless", &[]);
        let lossless_only = policy("lossless", "lossless", &[]);
        let cutoff_320 = UsenetPolicy {
            quality_cutoff: QualityTier::Mp3_320,
            ..policy("low", "lossless", &[])
        };
        let paid = flags(Some(true), Some(true));
        let hq_only = flags(Some(false), Some(true));
        let free = flags(Some(false), Some(false));
        let unknown = flags(None, None);

        type Row<'a> = (
            &'a str,
            AccountFlags,
            &'a UsenetPolicy,
            AddFor,
            Option<Bitrate>,
        );
        let rows: &[Row<'_>] = &[
            ("paid", paid, &default, PLAIN, Some(Flac)),
            (
                "paid, MP3 first in the recipe",
                paid,
                &mp3_first,
                PLAIN,
                Some(Mp3At320),
            ),
            ("hq only", hq_only, &default, PLAIN, Some(Mp3At320)),
            (
                "hq only, lossless only",
                hq_only,
                &lossless_only,
                PLAIN,
                None,
            ),
            ("free", free, &default, PLAIN, None),
            ("free, any quality", free, &anything, PLAIN, Some(Mp3At128)),
            ("unknown flags", unknown, &default, PLAIN, Some(Flac)),
            (
                "unknown lossless, no hq",
                flags(None, Some(false)),
                &default,
                PLAIN,
                Some(Flac),
            ),
            ("waived, free", free, &lossless_only, WAIVED, Some(Mp3At128)),
            (
                "waived, hq only",
                hq_only,
                &lossless_only,
                WAIVED,
                Some(Mp3At320),
            ),
            (
                "waived, unknown flags",
                unknown,
                &default,
                WAIVED,
                Some(Flac),
            ),
            ("upgrade, paid", paid, &default, UPGRADE, Some(Flac)),
            (
                "upgrade below a lossless cutoff, hq only",
                hq_only,
                &default,
                UPGRADE,
                None,
            ),
            (
                "upgrade, MP3 first, lossless cutoff",
                paid,
                &mp3_first,
                UPGRADE,
                Some(Flac),
            ),
            (
                "upgrade, hq only, cutoff 320",
                hq_only,
                &cutoff_320,
                UPGRADE,
                Some(Mp3At320),
            ),
            (
                "upgrade, free, cutoff 320",
                free,
                &cutoff_320,
                UPGRADE,
                None,
            ),
            (
                "waived upgrade, free, cutoff 320",
                free,
                &cutoff_320,
                WAIVED_UPGRADE,
                None,
            ),
        ];
        let wrong = wrong_rows(rows.iter().map(|(case, account, settings, add, want)| {
            (
                (*case).to_owned(),
                choose_bitrate(*account, settings, *add),
                *want,
            )
        }));
        assert!(wrong.is_empty(), "{wrong}");

        type Step<'a> = (
            &'a str,
            Bitrate,
            AccountFlags,
            &'a UsenetPolicy,
            AddFor,
            Option<Bitrate>,
        );
        let steps: &[Step<'_>] = &[
            (
                "no FLAC: MP3 320",
                Flac,
                paid,
                &default,
                PLAIN,
                Some(Mp3At320),
            ),
            (
                "no MP3 320 under the default",
                Mp3At320,
                paid,
                &default,
                PLAIN,
                None,
            ),
            (
                "no FLAC, lossless only",
                Flac,
                paid,
                &lossless_only,
                PLAIN,
                None,
            ),
            (
                "no FLAC, no hq: MP3 128",
                Flac,
                flags(Some(true), Some(false)),
                &anything,
                PLAIN,
                Some(Mp3At128),
            ),
            (
                "no MP3 320, any quality",
                Mp3At320,
                paid,
                &anything,
                PLAIN,
                Some(Mp3At128),
            ),
            (
                "waived, no FLAC",
                Flac,
                paid,
                &lossless_only,
                WAIVED,
                Some(Mp3At320),
            ),
            ("nothing below 128", Mp3At128, paid, &anything, WAIVED, None),
            (
                "upgrade, no FLAC, lossless cutoff",
                Flac,
                paid,
                &default,
                UPGRADE,
                None,
            ),
            (
                "upgrade, no FLAC, cutoff 320",
                Flac,
                paid,
                &cutoff_320,
                UPGRADE,
                Some(Mp3At320),
            ),
            (
                "upgrade, no MP3 320, cutoff 320",
                Mp3At320,
                paid,
                &cutoff_320,
                UPGRADE,
                None,
            ),
        ];
        let wrong = wrong_rows(steps.iter().map(
            |(case, current, account, settings, add, want)| {
                (
                    (*case).to_owned(),
                    step_down(*current, *account, settings, *add),
                    *want,
                )
            },
        ));
        assert!(wrong.is_empty(), "{wrong}");
    }
}
