//! From scored releases to one verdict.
//!
//! Thresholds, and where they come from:
//!
//! - Accept at a library distance of at most [`ACCEPT_ALBUM`] (0.20) with
//!   no paired track worse than [`ACCEPT_TRACK`] (0.40): Lidarr's
//!   `CloseAlbumMatch` and `CloseTrackMatch`. The same 0.40 was v2's pair
//!   ceiling.
//! - Show a curator anything up to [`REVIEW_CEILING`] (0.35), v2's album
//!   distance ceiling; past it the album has no plausible candidate.
//! - The best release group must beat every other group by
//!   [`GROUP_MARGIN`] (0.05), v2's margin floor. Editions of one group
//!   are not rivals: they go through edition choice instead. Within the
//!   margin, a group still wins when the files make up one of its
//!   releases completely and closely (see below) and that release's full
//!   distance, missing tracks counted, beats every other group's by more
//!   than the margin: a one-file "Night Shift" goes to the single, not to
//!   the ten-track album it also fits. When groups the files complete
//!   closely are within the margin of each other on that distance, a
//!   group with an Official release beats groups whose every recalled
//!   release has another status (a promotional copy filed as its own
//!   group); a release with no status is never counted as another status.
//! - Edition choice looks at the group's editions within
//!   [`EDITION_COHORT`] (0.10, v2's consensus epsilon) of the best and
//!   prefers the release the tags name, then one whose barcode or catalog
//!   number the tags carry, then the closest full tracklist (missing tracks
//!   count here), then the fewest contradicted tag hints (media, country,
//!   disc count), then the closest exact track lengths, then the saved
//!   edition preferences (v2's order without them: Official status,
//!   earliest date, worldwide country), then MBID.
//! - A lone candidate (no other release group plausible) needs v2's
//!   quorum: two tracks paired within 0.40, or a recording or
//!   release-track MBID on any file. A complete close fit also passes:
//!   no missing or unmatched tracks, a non-blank album title and artist
//!   that pass the name gate, and every pair within [`COMPLETE_TRACK`]
//!   (0.10) with titles that agree in full. On an untagged pair 0.10
//!   allows a length up to about 19 s off with the track number right; a
//!   wrong track number alone is 0.17. The titles are compared with their
//!   bracketed parts kept and no discounts, also within 0.10, so "Song"
//!   never completes "Song (Acoustic)" or "Song (Instrumental)", while a
//!   remix named on both sides still does. Without either there is too
//!   little to go on and the verdict is insufficient evidence.
//! - A release that fails the album title or artist gate (see `score`)
//!   goes to review however close its tracks are.
//! - Files the accepted release leaves unpaired send it to review past
//!   one (two from 21 files on). There is no outside source for that
//!   count: beets lets no unpaired file through automatically (`max_rec`
//!   caps `unmatched_tracks` at medium), and Lidarr's weight table counts
//!   them in the distance. When every track of the release is paired, the extra
//!   files are bonus files the edition lacks, and one per
//!   [`BONUS_SHARE`] (4) paired files is allowed: two bonus files on an
//!   eight-track album pass, two extra files around a two-track single do
//!   not. Each one still counts in the distance.
//! - Live releases, and compilations the tags do not call compilations,
//!   need confirmation (v2): unless every file carries its release-track
//!   id, only the release group is pinned. A track id set aside as
//!   another edition's (see `score`) does not name its track. Untagged
//!   files still confirm one when the group won clearly (no rival group
//!   within the margin) and the fit is very close: a complete close fit
//!   (above), a full distance within [`CONFIRM_ALBUM`] (0.05), and every
//!   file's length known and within [`CONFIRM_LENGTH_SECS`] (3 s) of its
//!   track. Length is the real guard here: 0.05 still lets a different
//!   year through on an album of six tracks or more. Two releases of one
//!   live group can be different nights of the same songs, so when
//!   another release of the group pairs any file with another recording,
//!   at least one file it pairs must have a known length more than 3 s
//!   off its track there, or the edition stays unconfirmed.
//!
//! - When the files name a release group (see `identify::evidence`), a
//!   tie within the margin goes to that group when it has a candidate
//!   in the tie, before the rules above are asked. The tie still counts
//!   as contested, so it never confirms a live release or compilation by
//!   itself. A release outside that group is only ever a guess: it comes
//!   back as [`Verdict::OutsideNamedGroup`], never identified.
//!
//! - A file whose audio names another track of the chosen release than
//!   the one its title clearly fits (see `ReleaseMatch::disputed`) pairs
//!   by its description, and the release comes back as
//!   [`Verdict::AudioDisagrees`], a guess, never identified: AcoustID
//!   holds user-submitted mistakes, and only a person can tell which side
//!   is wrong. The one case the files settle: the file's measured length
//!   is its title's track's and not the heard track's (see
//!   `ReleaseMatch::overruled`). Its fingerprint is set aside without
//!   holding the release.
//!
//! "Library distance" leaves missing tracks out, as Lidarr does for files
//! already on disk: holding part of an album is normal.

use std::cmp::Ordering;

use super::model::{EditionHints, LocalAlbum, Release};
use super::score::{ReleaseMatch, TrackPair};
use super::strings::basic_dist;
use crate::edition_prefs::{EditionFacts, Preferences};

pub const ACCEPT_ALBUM: f64 = 0.20;
pub const ACCEPT_TRACK: f64 = 0.40;
pub const REVIEW_CEILING: f64 = 0.35;
pub const GROUP_MARGIN: f64 = 0.05;
pub const EDITION_COHORT: f64 = 0.10;
/// Every pair of a complete close fit is at most this far off.
pub const COMPLETE_TRACK: f64 = 0.10;
/// The full distance under which untagged files confirm a live release or
/// an unflagged compilation.
pub const CONFIRM_ALBUM: f64 = 0.05;
/// How far each file's length may be off its track for that confirmation.
pub const CONFIRM_LENGTH_SECS: f64 = 3.0;
/// A release the files complete may leave one unpaired file per this
/// many paired ones.
pub const BONUS_SHARE: usize = 4;
/// A tagged release this close needs no search (beets' "strong" match).
pub const STRONG: f64 = 0.04;
/// Weak tags past this distance call for fingerprints (Lidarr).
pub const FINGERPRINT_ABOVE: f64 = 0.15;

/// Why an album goes to a curator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewReason {
    /// Two release groups are about equally close.
    Ambiguous,
    /// The best release is plausible but not close enough to accept.
    WeakMatch,
}

impl ReviewReason {
    pub fn code(self) -> &'static str {
        match self {
            ReviewReason::Ambiguous => "AMBIGUOUS_CANDIDATES",
            ReviewReason::WeakMatch => "WEAK_MATCH",
        }
    }
}

/// The verdict; indexes point into the scored releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    NoCandidate,
    Identified(usize),
    /// The release group is clear; the exact edition needs confirmation.
    EditionUncertain(usize),
    Review(ReviewReason),
    /// Nothing plausible, and the files' own ids rule candidates out.
    Contradictory,
    /// Nothing plausible.
    Insufficient,
    /// The best release is outside the release group the files name: a
    /// guess to confirm, never confirmed by itself.
    OutsideNamedGroup(usize),
    /// The best release, with some file's audio naming another of its
    /// tracks than the one the file's title clearly fits: a guess to
    /// confirm, never confirmed by itself.
    AudioDisagrees(usize),
}

/// Edition hints: the release the tags name, and the saved edition
/// preferences for the last tie-break. A person's choice never reaches the
/// matcher: a chosen album is not identified automatically.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditionPrefs<'a> {
    pub tagged: Option<&'a str>,
    /// `None` falls back to v2's order: Official, earliest, worldwide.
    pub preferences: Option<&'a Preferences>,
    /// The release group the files name, lowercase.
    pub named_group: Option<&'a str>,
}

pub fn eligible(matched: &ReleaseMatch) -> bool {
    matched.conflicts.is_empty() && matched.library_distance() <= REVIEW_CEILING
}

pub fn decide(
    local: &LocalAlbum,
    releases: &[Release],
    matches: &[ReleaseMatch],
    prefs: EditionPrefs<'_>,
) -> Verdict {
    let verdict = decide_group(local, releases, matches, prefs);
    match (verdict, prefs.named_group) {
        (Verdict::Identified(index) | Verdict::EditionUncertain(index), Some(named))
            if !same_group(&releases[index], named) =>
        {
            Verdict::OutsideNamedGroup(index)
        }
        (Verdict::Identified(index) | Verdict::EditionUncertain(index), _)
            if !matches[index].disputed.is_empty() =>
        {
            Verdict::AudioDisagrees(index)
        }
        _ => verdict,
    }
}

/// True when `named` has an eligible release inside the tie band: within
/// the margin of the closest eligible release.
pub fn named_in_tie(matches: &[ReleaseMatch], releases: &[Release], named: &str) -> bool {
    let best = matches
        .iter()
        .filter(|matched| eligible(matched))
        .map(ReleaseMatch::library_distance)
        .fold(f64::INFINITY, f64::min);
    releases.iter().zip(matches).any(|(release, matched)| {
        eligible(matched)
            && same_group(release, named)
            && matched.library_distance() < best + GROUP_MARGIN
    })
}

fn decide_group(
    local: &LocalAlbum,
    releases: &[Release],
    matches: &[ReleaseMatch],
    prefs: EditionPrefs<'_>,
) -> Verdict {
    if releases.is_empty() {
        return Verdict::NoCandidate;
    }
    let candidates: Vec<usize> = (0..releases.len())
        .filter(|index| eligible(&matches[*index]))
        .collect();
    let Some(&best) = candidates
        .iter()
        .min_by(|a, b| candidate_order(**a, **b, local, releases, matches, prefs))
    else {
        return if matches.iter().any(|matched| !matched.conflicts.is_empty()) {
            Verdict::Contradictory
        } else {
            Verdict::Insufficient
        };
    };
    let best_distance = matches[best].library_distance();
    let in_group =
        |index: usize, group: &str| releases[index].release_group_id.eq_ignore_ascii_case(group);
    let mut group = releases[best].release_group_id.as_str();
    let rival = candidates
        .iter()
        .filter(|index| !in_group(**index, group))
        .map(|index| matches[*index].library_distance())
        .fold(f64::INFINITY, f64::min);
    let contested = rival - best_distance < GROUP_MARGIN;
    if contested {
        let named = prefs
            .named_group
            .filter(|named| named_in_tie(matches, releases, named));
        let decided = match named {
            Some(named) => Some(named),
            None => completed_group(&candidates, best_distance, local, releases, matches, prefs),
        };
        match decided {
            Some(completed) => group = completed,
            None => return Verdict::Review(ReviewReason::Ambiguous),
        }
    }
    let group_best = candidates
        .iter()
        .filter(|index| in_group(**index, group))
        .map(|index| matches[*index].library_distance())
        .fold(f64::INFINITY, f64::min);
    let Some(&chosen) = candidates
        .iter()
        .filter(|index| in_group(**index, group))
        .filter(|index| matches[**index].library_distance() <= group_best + EDITION_COHORT)
        .min_by(|a, b| edition_order(**a, **b, local, releases, matches, prefs))
    else {
        return Verdict::Insufficient;
    };
    let matched = &matches[chosen];
    if rival.is_infinite()
        && !quorum(local, matched)
        && !complete_fit(local, &releases[chosen], matched)
    {
        return Verdict::Insufficient;
    }
    if matched.pairs.is_empty()
        || !matched.names_agree
        || matched.library_distance() > ACCEPT_ALBUM
        || matched.worst_track() > ACCEPT_TRACK
        || matched.unmatched.len() > unmatched_limit(local, matched)
    {
        return Verdict::Review(ReviewReason::WeakMatch);
    }
    if needs_type_confirmation(local, &releases[chosen])
        && !every_file_names_its_track(local, &releases[chosen], matched)
        && (contested || !very_close_fit(local, releases, matches, &candidates, chosen))
    {
        return Verdict::EditionUncertain(chosen);
    }
    Verdict::Identified(chosen)
}

/// Files the accepted release may leave unpaired (see the module notes).
fn unmatched_limit(local: &LocalAlbum, matched: &ReleaseMatch) -> usize {
    let base = if local.tracks.len() <= 20 { 1 } else { 2 };
    if matched.missing.is_empty() {
        base.max(matched.pairs.len() / BONUS_SHARE)
    } else {
        base
    }
}

/// Lidarr's rule for when fingerprints are worth taking: nothing close,
/// local files left over, or a bad pair. Files whose tags already carry
/// recording MBIDs have nothing to gain.
pub fn should_fingerprint(local: &LocalAlbum, matches: &[ReleaseMatch]) -> bool {
    if local
        .tracks
        .iter()
        .all(|track| track.recording_mbid.is_some())
    {
        return false;
    }
    let best = matches
        .iter()
        .filter(|matched| matched.conflicts.is_empty())
        .min_by(|a, b| a.library_distance().total_cmp(&b.library_distance()));
    match best {
        None => true,
        Some(best) => {
            best.library_distance() > FINGERPRINT_ABOVE
                || !best.unmatched.is_empty()
                || best.worst_track() > ACCEPT_TRACK
        }
    }
}

/// Candidate order: the closest library distance, then edition order,
/// which counts missing tracks. The best guess follows it too.
pub fn candidate_order(
    a: usize,
    b: usize,
    local: &LocalAlbum,
    releases: &[Release],
    matches: &[ReleaseMatch],
    prefs: EditionPrefs<'_>,
) -> Ordering {
    matches[a]
        .library_distance()
        .total_cmp(&matches[b].library_distance())
        .then_with(|| edition_order(a, b, local, releases, matches, prefs))
}

/// The files make up the whole release closely: nothing missing or left
/// over, a real album title and artist that pass the name gate, and every
/// pair within [`COMPLETE_TRACK`] with titles that agree in full,
/// bracketed parts included.
fn complete_fit(local: &LocalAlbum, release: &Release, matched: &ReleaseMatch) -> bool {
    !local.title.trim().is_empty()
        && !local.artist.trim().is_empty()
        && !matched.pairs.is_empty()
        && matched.missing.is_empty()
        && matched.unmatched.is_empty()
        && matched.names_agree
        && matched.pairs.iter().all(|pair| {
            pair.distance <= COMPLETE_TRACK
                && local.tracks.get(pair.local).is_some_and(|file| {
                    release.tracks.get(pair.track).is_some_and(|track| {
                        basic_dist(&file.title, &track.title) <= COMPLETE_TRACK
                    })
                })
        })
}

/// The files fit the chosen release almost exactly (see the module
/// notes), and every other release of its group that hears a file as
/// another recording is ruled out by a track length.
fn very_close_fit(
    local: &LocalAlbum,
    releases: &[Release],
    matches: &[ReleaseMatch],
    candidates: &[usize],
    chosen: usize,
) -> bool {
    let (release, matched) = (&releases[chosen], &matches[chosen]);
    let lengths_close = matched
        .pairs
        .iter()
        .all(|pair| length_off(local, release, pair).is_some_and(|gap| gap <= CONFIRM_LENGTH_SECS));
    if !lengths_close
        || matched.album_distance() > CONFIRM_ALBUM
        || !complete_fit(local, release, matched)
    {
        return false;
    }
    let recording = |release: &Release, matched: &ReleaseMatch, file: usize| {
        matched
            .pair_for(file)
            .and_then(|pair| release.tracks.get(pair.track))
            .map(|track| track.recording_id.to_ascii_lowercase())
    };
    candidates
        .iter()
        .copied()
        .filter(|index| {
            *index != chosen && same_group(&releases[*index], &release.release_group_id)
        })
        .filter(|index| {
            let (other, other_match) = (&releases[*index], &matches[*index]);
            other_match.pairs.iter().any(|pair| {
                recording(other, other_match, pair.local) != recording(release, matched, pair.local)
            })
        })
        .all(|index| {
            matches[index]
                .pairs
                .iter()
                .filter_map(|pair| length_off(local, &releases[index], pair))
                .any(|gap| gap > CONFIRM_LENGTH_SECS)
        })
}

/// How many seconds a paired file is off its track; `None` when either
/// length is unknown.
fn length_off(local: &LocalAlbum, release: &Release, pair: &TrackPair) -> Option<f64> {
    let file = local.tracks.get(pair.local)?.duration_secs?;
    let track = release.tracks.get(pair.track)?.length_ms?;
    Some((file - track as f64 / 1000.0).abs())
}

/// For release groups tied within the margin: the group of the release
/// the files complete closely, when its full distance beats every other
/// group's releases by more than the margin. Among groups the files
/// complete about equally well, the one group with an Official release
/// wins when every recalled release of the others has another status.
fn completed_group<'a>(
    candidates: &[usize],
    best_distance: f64,
    local: &LocalAlbum,
    releases: &'a [Release],
    matches: &[ReleaseMatch],
    prefs: EditionPrefs<'_>,
) -> Option<&'a str> {
    let complete: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|index| matches[*index].library_distance() < best_distance + GROUP_MARGIN)
        .filter(|index| complete_fit(local, &releases[*index], &matches[*index]))
        .collect();
    let completed = complete.iter().copied().min_by(|a, b| {
        matches[*a]
            .album_distance()
            .total_cmp(&matches[*b].album_distance())
            .then_with(|| edition_order(*a, *b, local, releases, matches, prefs))
    })?;
    let group = releases[completed].release_group_id.as_str();
    let distance = matches[completed].album_distance();
    let close = |index: &usize| matches[*index].album_distance() - distance <= GROUP_MARGIN;
    let rivals: Vec<&str> = candidates
        .iter()
        .filter(|index| !same_group(&releases[**index], group) && close(index))
        .map(|index| releases[*index].release_group_id.as_str())
        .collect();
    if rivals.is_empty() {
        return Some(group);
    }
    // Every group in the tie must be complete closely within the margin.
    let mut tied: Vec<&str> = vec![group];
    for rival in rivals {
        if !tied.iter().any(|seen| seen.eq_ignore_ascii_case(rival)) {
            tied.push(rival);
        }
    }
    let completes = |group: &str| {
        complete
            .iter()
            .any(|index| same_group(&releases[*index], group) && close(index))
    };
    if !tied.iter().all(|group| completes(group)) {
        return None;
    }
    let with_official: Vec<&str> = tied
        .iter()
        .copied()
        .filter(|group| releases.iter().any(|r| same_group(r, group) && official(r)))
        .collect();
    let [winner] = with_official[..] else {
        return None;
    };
    tied.iter()
        .filter(|group| !group.eq_ignore_ascii_case(winner))
        .all(|group| {
            releases
                .iter()
                .filter(|release| same_group(release, group))
                .all(other_status)
        })
        .then_some(winner)
}

fn same_group(release: &Release, group: &str) -> bool {
    release.release_group_id.eq_ignore_ascii_case(group)
}

/// A status that is stated and is not Official (Promotion, Bootleg, ...).
fn other_status(release: &Release) -> bool {
    release
        .status
        .as_deref()
        .is_some_and(|status| !status.trim().is_empty())
        && !official(release)
}

/// v2's lone-candidate quorum (`_lone_eligible_supported`).
fn quorum(local: &LocalAlbum, matched: &ReleaseMatch) -> bool {
    let close = matched
        .pairs
        .iter()
        .filter(|pair| pair.distance <= ACCEPT_TRACK)
        .count();
    close >= 2
        || local
            .tracks
            .iter()
            .any(|track| track.recording_mbid.is_some() || track.release_track_mbid.is_some())
}

fn needs_type_confirmation(local: &LocalAlbum, release: &Release) -> bool {
    release.secondary_types.iter().any(|kind| {
        kind.eq_ignore_ascii_case("live")
            || (kind.eq_ignore_ascii_case("compilation") && !local.is_compilation)
    })
}

fn every_file_names_its_track(
    local: &LocalAlbum,
    release: &Release,
    matched: &ReleaseMatch,
) -> bool {
    local.tracks.iter().enumerate().all(|(index, track)| {
        let Some(pair) = matched.pair_for(index) else {
            return false;
        };
        track
            .release_track_mbid
            .as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case(&release.tracks[pair.track].id))
    })
}

/// Edition order among releases of one group (see the module notes).
fn edition_order(
    a: usize,
    b: usize,
    local: &LocalAlbum,
    releases: &[Release],
    matches: &[ReleaseMatch],
    prefs: EditionPrefs<'_>,
) -> Ordering {
    let (left, right) = (&releases[a], &releases[b]);
    let named = |release: &Release, mbid: Option<&str>| -> bool {
        mbid.is_some_and(|mbid| release.answers_to(mbid))
    };
    let preferred = match prefs.preferences {
        Some(preferences) => preferences
            .key(&release_facts(left))
            .cmp(&preferences.key(&release_facts(right))),
        None => official(right)
            .cmp(&official(left))
            .then_with(|| date_key(left.date.as_deref()).cmp(&date_key(right.date.as_deref())))
            .then_with(|| worldwide(right).cmp(&worldwide(left))),
    };
    named(right, prefs.tagged)
        .cmp(&named(left, prefs.tagged))
        .then_with(|| same_id(right, &local.hints).cmp(&same_id(left, &local.hints)))
        .then_with(|| {
            rounded(matches[a].album_distance()).cmp(&rounded(matches[b].album_distance()))
        })
        .then_with(|| hint_misses(left, &local.hints).cmp(&hint_misses(right, &local.hints)))
        .then_with(|| {
            length_gap(local, left, &matches[a]).cmp(&length_gap(local, right, &matches[b]))
        })
        .then(preferred)
        .then_with(|| left.id.cmp(&right.id))
}

/// The tags' barcode or catalog number names this release: as strong as
/// a release id for telling pressings apart.
fn same_id(release: &Release, hints: &EditionHints) -> bool {
    barcode_agrees(release, hints) == Some(true) || catalog_agrees(release, hints) == Some(true)
}

/// Whether the release's barcode is the one the hints hold; `None` when
/// either side has none.
fn barcode_agrees(release: &Release, hints: &EditionHints) -> Option<bool> {
    let wanted = hints.barcode.as_deref()?;
    let own = release
        .barcode
        .as_deref()
        .and_then(EditionHints::barcode_key)?;
    Some(own == wanted)
}

/// Whether one of the release's catalog numbers is the one the hints
/// hold; `None` when either side has none.
fn catalog_agrees(release: &Release, hints: &EditionHints) -> Option<bool> {
    let wanted = hints.catalog_number.as_deref()?;
    let own: Vec<String> = release
        .catalog_numbers
        .iter()
        .filter_map(|raw| EditionHints::catalog_key(raw))
        .collect();
    (!own.is_empty()).then(|| own.iter().any(|key| key == wanted))
}

/// How a release's barcode and catalog numbers stand to the ones the files
/// carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdStance {
    /// It states one the files carry and contradicts neither.
    Names,
    /// It states another value for one the files carry and agrees with
    /// neither.
    Contradicts,
    /// It is silent on both, or agrees on one and differs on the other.
    Neither,
}

/// How `release` stands to the barcode and catalog number in `ids`.
pub fn id_stance(release: &Release, ids: &EditionHints) -> IdStance {
    let answers = [barcode_agrees(release, ids), catalog_agrees(release, ids)];
    let agrees = answers.contains(&Some(true));
    let differs = answers.contains(&Some(false));
    match (agrees, differs) {
        (true, false) => IdStance::Names,
        (false, true) => IdStance::Contradicts,
        _ => IdStance::Neither,
    }
}

/// Soft tag hints the release contradicts: media format, country, and the
/// number of discs (which tells a partial two-disc set from one disc).
pub fn hint_misses(release: &Release, hints: &EditionHints) -> usize {
    let media = hints.media.as_deref().is_some_and(|media| {
        let media = media.to_lowercase();
        !release.media.iter().any(|medium| {
            medium.format.as_deref().is_some_and(|format| {
                let format = format.to_lowercase();
                format.contains(&media) || media.contains(&format)
            })
        })
    });
    let country = hints.country.as_deref().is_some_and(|country| {
        !release
            .country
            .as_deref()
            .is_some_and(|own| own.eq_ignore_ascii_case(country))
    });
    let discs = hints
        .total_discs
        .is_some_and(|total| !release.media.is_empty() && release.media.len() != total as usize);
    usize::from(media) + usize::from(country) + usize::from(discs)
}

/// Mean gap between the files' exact lengths and the release's track
/// lengths, in tenths of a second, with no grace: what tells a remaster or
/// a digital edition from the CD when the tracklists agree.
fn length_gap(local: &LocalAlbum, release: &Release, matched: &ReleaseMatch) -> u64 {
    let gaps: Vec<f64> = matched
        .pairs
        .iter()
        .filter_map(|pair| {
            let file = local.tracks.get(pair.local)?.duration_secs?;
            let track = release.tracks.get(pair.track)?.length_ms? as f64 / 1000.0;
            Some((file - track).abs())
        })
        .collect();
    if gaps.is_empty() {
        return u64::MAX;
    }
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    (mean * 10.0).round() as u64
}

/// What the edition preferences read on one release.
fn release_facts(release: &Release) -> EditionFacts<'_> {
    EditionFacts {
        status: release.status.as_deref(),
        formats: release
            .media
            .iter()
            .filter_map(|medium| medium.format.as_deref())
            .collect(),
        country: release.country.as_deref(),
        date: release.date.as_deref(),
        text: vec![release.title.as_str()],
        types: release.secondary_types.iter().map(String::as_str).collect(),
    }
}

/// Distances equal to six places tie, so float noise never picks an
/// edition.
fn rounded(distance: f64) -> i64 {
    (distance * 1_000_000.0).round() as i64
}

fn official(release: &Release) -> bool {
    release
        .status
        .as_deref()
        .is_some_and(|status| status.eq_ignore_ascii_case("official"))
}

fn worldwide(release: &Release) -> bool {
    release.country.as_deref() == Some("XW")
}

/// v2's mixed-precision date order: earlier first, a more precise date
/// before a vaguer one of the same prefix, no date last.
fn date_key(date: Option<&str>) -> (u32, u32, u32) {
    const UNKNOWN: u32 = 100;
    let Some(date) = date.map(str::trim) else {
        return (10_000, UNKNOWN, UNKNOWN);
    };
    let mut parts = date.split('-');
    let number = |part: Option<&str>, width: usize| -> Option<u32> {
        let part = part?;
        (part.len() == width && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let Some(year) = number(parts.next(), 4) else {
        return (10_000, UNKNOWN, UNKNOWN);
    };
    let month = number(parts.next(), 2).unwrap_or(UNKNOWN);
    let day = number(parts.next(), 2).unwrap_or(UNKNOWN);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::matching::model::ReleaseMedium;
    use crate::matching::model::{CreditedArtist, LocalTrack, ReleaseTrack};
    use crate::matching::score::{ConflictKind, Support, match_release};

    const TITLES: [&str; 8] = [
        "Lamplight",
        "Blue Hour",
        "Night Shift",
        "Last Train Home",
        "Harbour Lights",
        "Slow Fade",
        "Paper Moon",
        "Morning Line",
    ];

    fn release(group: &str, title: &str, count: usize) -> Release {
        Release {
            id: format!("{group}-release"),
            release_group_id: group.to_owned(),
            title: title.to_owned(),
            artists: vec![CreditedArtist {
                id: "artist".to_owned(),
                name: "The Lanterns".to_owned(),
                sort_name: None,
                join: String::new(),
            }],
            tracks: (0..count)
                .map(|index| ReleaseTrack {
                    genres: Vec::new(),
                    id: format!("{group}-track-{index}"),
                    recording_id: format!("recording-{index}"),
                    title: TITLES[index].to_owned(),
                    artists: Vec::new(),
                    disc: 1,
                    position: index as u32 + 1,
                    absolute_position: index as u32 + 1,
                    length_ms: Some(200_000),
                    isrcs: Vec::new(),
                })
                .collect(),
            ..Release::default()
        }
    }

    /// Files titled like the release, at 200 s, plus `garbage` ones
    /// (wrong title, length a minute and a half off), with recording
    /// MBIDs when `ids` is set.
    fn local(count: usize, garbage: usize, ids: bool) -> LocalAlbum {
        LocalAlbum {
            title: "Night Shift".to_owned(),
            artist: "The Lanterns".to_owned(),
            tracks: (0..count + garbage)
                .map(|index| LocalTrack {
                    id: format!("t{index}"),
                    title: if index < count {
                        TITLES[index].to_owned()
                    } else {
                        "zzzz".to_owned()
                    },
                    track_number: index as u32 + 1,
                    disc_number: 1,
                    duration_secs: Some(if index < count { 200.0 } else { 290.0 }),
                    recording_mbid: ids.then(|| format!("recording-{index}")),
                    ..LocalTrack::default()
                })
                .collect(),
            ..LocalAlbum::default()
        }
    }

    fn verdict(local: &LocalAlbum, releases: &[Release]) -> Verdict {
        let matches: Vec<_> = releases
            .iter()
            .map(|release| match_release(local, release, &HashMap::new()))
            .collect();
        decide(local, releases, &matches, EditionPrefs::default())
    }

    #[test]
    fn thresholds_and_gates() {
        let night = |count| release("group-a", "Night Shift", count);
        let weak = Verdict::Review(ReviewReason::WeakMatch);
        let cases: Vec<(&str, LocalAlbum, Vec<Release>, Verdict)> = vec![
            // One file that makes up the whole release closely is enough;
            // one file of a longer release is not.
            (
                "lone candidate, one plain track, complete",
                local(1, 0, false),
                vec![night(1)],
                Verdict::Identified(0),
            ),
            (
                "lone candidate, one plain track of two",
                local(1, 0, false),
                vec![night(2)],
                Verdict::Insufficient,
            ),
            (
                "lone candidate, one tagged track",
                local(1, 0, true),
                vec![night(1)],
                Verdict::Identified(0),
            ),
            (
                "lone candidate, two plain tracks",
                local(2, 0, false),
                vec![night(2)],
                Verdict::Identified(0),
            ),
            (
                "one unmatched file is allowed",
                local(3, 1, false),
                vec![night(4)],
                Verdict::Identified(0),
            ),
            (
                "two unmatched files are not",
                local(2, 2, false),
                vec![night(4)],
                weak,
            ),
            (
                "two bonus files on a complete eight-track release are allowed",
                local(8, 2, false),
                vec![night(8)],
                Verdict::Identified(0),
            ),
            (
                "two extra files around a complete two-track single are not",
                local(2, 2, false),
                vec![night(2)],
                weak,
            ),
            (
                "two groups within the margin",
                local(2, 0, false),
                vec![night(2), release("group-b", "Night Shift", 2)],
                Verdict::Review(ReviewReason::Ambiguous),
            ),
            (
                "edition words do not fail the title gate",
                local(2, 0, false),
                vec![release("group-a", "Night Shift (Deluxe Edition)", 2)],
                Verdict::Identified(0),
            ),
            (
                "another title fails the gate",
                local(4, 0, false),
                vec![release("group-a", "Night Shift Sessions", 4)],
                weak,
            ),
            (
                "embedded ids prove a retitled album",
                local(4, 0, true),
                vec![release("group-a", "Night Shift Sessions", 4)],
                Verdict::Identified(0),
            ),
        ];
        for (name, local, releases, expected) in cases {
            assert_eq!(verdict(&local, &releases), expected, "{name}");
        }
    }

    /// One untagged "Night Shift" fits the single and the first track of
    /// the ten-track album equally well; it completes only the single.
    #[test]
    fn a_lone_file_goes_to_the_single_it_completes() {
        let mut album = release("album", "Night Shift", 1);
        album.tracks = (0..10)
            .map(|index| ReleaseTrack {
                id: format!("album-track-{index}"),
                recording_id: format!("album-recording-{index}"),
                title: if index == 0 {
                    "Night Shift".to_owned()
                } else {
                    format!("Album Track {index}")
                },
                position: index + 1,
                absolute_position: index + 1,
                ..album.tracks[0].clone()
            })
            .collect();
        let mut single = release("single", "Night Shift", 1);
        single.tracks[0].title = "Night Shift".to_owned();
        let mut file = local(1, 0, false);
        file.tracks[0].title = "Night Shift".to_owned();

        assert_eq!(
            verdict(&file, &[album.clone(), single.clone()]),
            Verdict::Identified(1)
        );

        // A single with a second track the files lack is no better a fit.
        let mut two_sided = single;
        two_sided.tracks.push(ReleaseTrack {
            id: "single-track-1".to_owned(),
            recording_id: "single-recording-1".to_owned(),
            title: "Blue Hour".to_owned(),
            position: 2,
            absolute_position: 2,
            ..two_sided.tracks[0].clone()
        });
        assert_eq!(
            verdict(&file, &[album, two_sided]),
            Verdict::Review(ReviewReason::Ambiguous)
        );
    }

    /// A one-track release of `group` titled `title`, its track `song`.
    fn one_track(group: &str, title: &str, song: &str, status: Option<&str>) -> Release {
        let mut single = release(group, title, 1);
        single.tracks[0].title = song.to_owned();
        single.status = status.map(str::to_owned);
        single
    }

    /// One untagged file titled `song`, on an album titled `album`.
    fn one_file(album: &str, song: &str) -> LocalAlbum {
        let mut file = local(1, 0, false);
        file.title = album.to_owned();
        file.tracks[0].title = song.to_owned();
        file
    }

    #[test]
    fn what_a_complete_fit_needs() {
        let city = "Midnight In The Quiet City";
        let acoustic = "Midnight In The Quiet City (Acoustic)";
        let mut album = release("album", city, 1);
        album.tracks = (0..10)
            .map(|index| ReleaseTrack {
                id: format!("album-track-{index}"),
                recording_id: format!("album-recording-{index}"),
                title: if index == 0 {
                    city.to_owned()
                } else {
                    format!("Album Track {index}")
                },
                position: index + 1,
                absolute_position: index + 1,
                ..album.tracks[0].clone()
            })
            .collect();
        let official = Some("Official");
        let promotion = Some("Promotion");
        let cases: Vec<(&str, LocalAlbum, Vec<Release>, Verdict)> = vec![
            (
                "another version of the song is not the song",
                one_file("Sunrise", "Sunrise"),
                vec![one_track(
                    "g",
                    "Sunrise (Instrumental)",
                    "Sunrise (Instrumental)",
                    official,
                )],
                Verdict::Insufficient,
            ),
            (
                "nor does it break a tie with the album",
                one_file(city, city),
                vec![
                    album.clone(),
                    one_track("acoustic", acoustic, acoustic, official),
                ],
                Verdict::Review(ReviewReason::Ambiguous),
            ),
            (
                "a blank album title completes nothing",
                one_file("", "Night Shift"),
                vec![one_track("g", "Night Shift", "Night Shift", official)],
                Verdict::Insufficient,
            ),
            (
                "the Official single beats a promotional group",
                one_file("Night Shift", "Night Shift"),
                vec![
                    one_track("promo", "Night Shift - Single", "Night Shift", promotion),
                    one_track("single", "Night Shift", "Night Shift", official),
                ],
                Verdict::Identified(1),
            ),
            (
                "a group with no status is not promotional",
                one_file("Night Shift", "Night Shift"),
                vec![
                    one_track("unknown", "Night Shift", "Night Shift", None),
                    one_track("single", "Night Shift", "Night Shift", official),
                ],
                Verdict::Review(ReviewReason::Ambiguous),
            ),
        ];
        let wrong: Vec<String> = cases
            .into_iter()
            .filter_map(|(name, local, releases, expected)| {
                let got = verdict(&local, &releases);
                (got != expected).then(|| format!("{name}: expected {expected:?}, got {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Release `id` of `group`: the four recordings, its own track ids.
    fn edition(group: &str, id: &str, title: &str) -> Release {
        let mut release = release(group, title, 4);
        release.id = id.to_owned();
        for (index, track) in release.tracks.iter_mut().enumerate() {
            track.id = format!("{id}-track-{index}");
        }
        release
    }

    /// Four files naming `release`, each with its recording and track.
    fn named(release: &str) -> LocalAlbum {
        let mut album = local(4, 0, true);
        for (index, track) in album.tracks.iter_mut().enumerate() {
            track.release_mbid = Some(release.to_owned());
            track.release_track_mbid = Some(format!("{release}-track-{index}"));
        }
        album
    }

    fn scored(local: &LocalAlbum, releases: &[Release]) -> (Verdict, Vec<ReleaseMatch>) {
        let matches: Vec<_> = releases
            .iter()
            .map(|release| match_release(local, release, &HashMap::new()))
            .collect();
        let verdict = decide(local, releases, &matches, EditionPrefs::default());
        (verdict, matches)
    }

    /// The file's release id outranks its track id, as in Picard: a file
    /// whose release id names R and whose recording is on R pairs on R as
    /// if it had no track id when its track id belongs to another edition
    /// (R2). Never stronger than that: not proof for the name gate, and
    /// not naming its track for a release that needs confirmation.
    #[test]
    fn a_track_id_from_another_edition_is_set_aside() {
        let r = || edition("g", "r", "Night Shift");
        let r2 = || edition("g", "r2", "Night Shift");
        let stray = |mut album: LocalAlbum| {
            album.tracks[1].release_track_mbid = Some("r2-track-1".to_owned());
            album
        };

        // The rule, against the same album with file 2's track id removed.
        let mut clean = named("r");
        clean.tracks[1].release_track_mbid = None;
        let (verdict, matches) = scored(&stray(named("r")), &[r(), r2()]);
        let (clean_verdict, clean_matches) = scored(&clean, &[r(), r2()]);
        assert_eq!(verdict, Verdict::Identified(0));
        assert_eq!(clean_verdict, Verdict::Identified(0));
        assert!(matches[0].conflicts.is_empty());
        assert_eq!(matches[0].distance, clean_matches[0].distance);
        let pair = matches[0]
            .pair_for(1)
            .map(|pair| (pair.track, pair.support));
        assert_eq!(pair, Some((1, Support::TrackIdElsewhere)));
        let mut expected = clean_matches[0].pairs.clone();
        for pair in expected.iter_mut().filter(|pair| pair.local == 1) {
            pair.support = Support::TrackIdElsewhere;
        }
        assert_eq!(matches[0].pairs, expected);

        let mut no_recording = stray(named("r"));
        no_recording.tracks[1].recording_mbid = None;
        let mut not_on_r = stray(named("r"));
        not_on_r.tracks[1].recording_mbid = Some("recording-9".to_owned());
        let mut names_r2 = stray(named("r"));
        names_r2.tracks[1].release_mbid = Some("r2".to_owned());
        let mut live = r();
        live.secondary_types = vec!["Live".to_owned()];
        // A fit too loose to confirm the live release by itself.
        let mut loose_length = stray(named("r"));
        loose_length.tracks[0].duration_secs = Some(215.0);
        let mut retitled = r();
        retitled.title = "Night Shift Sessions".to_owned();
        let mut merged = r();
        merged.old_ids = vec!["r-old".to_owned()];
        let mut via_old_id = named("r");
        for (index, track) in via_old_id.tracks.iter_mut().enumerate() {
            track.release_mbid = Some("r-old".to_owned());
            track.release_track_mbid = Some(format!("r2-track-{index}"));
        }
        let mut all_r2 = named("r");
        for (index, track) in all_r2.tracks.iter_mut().enumerate() {
            track.release_track_mbid = Some(format!("r2-track-{index}"));
        }
        let cases: Vec<(&str, LocalAlbum, Vec<Release>, Verdict)> = vec![
            (
                "no recording id: the track id still vetoes",
                no_recording.clone(),
                vec![r()],
                Verdict::Contradictory,
            ),
            (
                "a recording not on R vetoes",
                not_on_r.clone(),
                vec![r()],
                Verdict::Contradictory,
            ),
            (
                "file 2 names R2: its track id is ignored on R",
                names_r2.clone(),
                vec![r()],
                Verdict::Identified(0),
            ),
            (
                "a loosely fitting live release still needs every track named",
                loose_length,
                vec![live],
                Verdict::EditionUncertain(0),
            ),
            (
                "clean ids prove a retitled album",
                named("r"),
                vec![retitled.clone()],
                Verdict::Identified(0),
            ),
            (
                "a set-aside id proves nothing",
                stray(named("r")),
                vec![retitled],
                Verdict::Review(ReviewReason::WeakMatch),
            ),
            (
                "every file names R through a merged id",
                via_old_id,
                vec![merged],
                Verdict::Identified(0),
            ),
            (
                "every track id is R2's, R2 scored too",
                all_r2,
                vec![r(), r2()],
                Verdict::Identified(0),
            ),
        ];
        for (name, local, releases, expected) in cases {
            assert_eq!(scored(&local, &releases).0, expected, "{name}");
        }

        let (_, matches) = scored(&no_recording, &[r()]);
        assert_eq!(matches[0].conflicts[0].kind, ConflictKind::ReleaseTrack);
        let (_, matches) = scored(&not_on_r, &[r()]);
        assert_eq!(matches[0].conflicts[0].kind, ConflictKind::Recording);
        let (_, matches) = scored(&names_r2, &[r()]);
        let support = matches[0].pair_for(1).map(|pair| pair.support);
        assert_eq!(support, Some(Support::EmbeddedId));

        // Two set-aside files on one recording: one takes the track.
        let mut twice = stray(named("r"));
        twice.tracks[2].recording_mbid = Some("recording-1".to_owned());
        twice.tracks[2].title = "Blue Hour".to_owned();
        twice.tracks[2].release_track_mbid = Some("r2-track-1".to_owned());
        let (_, matches) = scored(&twice, &[r()]);
        assert!(matches[0].conflicts.is_empty());
        assert_eq!(matches[0].unmatched.len(), 1);
        assert!([1, 2].contains(&matches[0].unmatched[0]));
        assert_eq!(matches[0].missing, vec![2]);

        // A recording on R twice: the file's disc and position pick it.
        let mut two_discs = r();
        two_discs.tracks[2].recording_id = "recording-1".to_owned();
        two_discs.tracks[2].title = "Blue Hour".to_owned();
        for (index, track) in two_discs.tracks.iter_mut().enumerate() {
            track.disc = index as u32 / 2 + 1;
            track.position = index as u32 % 2 + 1;
        }
        two_discs.media = (1..=2)
            .map(|position| ReleaseMedium {
                position,
                format: None,
                title: None,
                track_count: 2,
            })
            .collect();
        let mut late = stray(named("r"));
        late.tracks.truncate(2);
        late.tracks[1].disc_number = 2;
        late.tracks[1].track_number = 1;
        let (_, matches) = scored(&late, &[two_discs]);
        assert_eq!(matches[0].pair_for(1).map(|pair| pair.track), Some(2));
    }

    /// Untagged files confirm a live release or an unflagged compilation
    /// only when they fit it almost exactly, and never when another
    /// edition of the group could be another recording of the same songs.
    #[test]
    fn a_live_album_or_compilation_confirms_on_a_very_close_fit() {
        let typed = |kind: &str, id: &str| {
            let mut release = edition("g", id, "Night Shift");
            release.secondary_types = vec![kind.to_owned()];
            release
        };
        let other_night = |offset_ms: Option<i64>| {
            let mut release = typed("Live", "night-2");
            for (index, track) in release.tracks.iter_mut().enumerate() {
                track.recording_id = format!("night-2-recording-{index}");
                track.length_ms = offset_ms
                    .and_then(|offset| track.length_ms.map(|ms| (ms as i64 + offset) as u64));
            }
            release
        };
        let mut five_seconds_off = local(4, 0, false);
        five_seconds_off.tracks[2].duration_secs = Some(205.0);
        let mut live_titles = local(4, 0, false);
        for track in &mut live_titles.tracks {
            track.title = format!("{} (Live)", track.title);
        }
        let cases: Vec<(&str, LocalAlbum, Vec<Release>, Verdict)> = vec![
            (
                "a live album fits closely",
                local(4, 0, false),
                vec![typed("Live", "night-1")],
                Verdict::Identified(0),
            ),
            (
                "an unflagged compilation fits closely",
                local(4, 0, false),
                vec![typed("Compilation", "best-of")],
                Verdict::Identified(0),
            ),
            (
                "titles that differ by (Live) are not nearly exact",
                live_titles,
                vec![typed("Live", "night-1")],
                Verdict::EditionUncertain(0),
            ),
            (
                "another night at the same lengths",
                local(4, 0, false),
                vec![typed("Live", "night-1"), other_night(Some(0))],
                Verdict::EditionUncertain(0),
            ),
            (
                "another night one second off",
                local(4, 0, false),
                vec![typed("Live", "night-1"), other_night(Some(1_000))],
                Verdict::EditionUncertain(0),
            ),
            (
                "another night with no lengths",
                local(4, 0, false),
                vec![typed("Live", "night-1"), other_night(None)],
                Verdict::EditionUncertain(0),
            ),
            (
                "a track five seconds off is not nearly exact",
                five_seconds_off,
                vec![typed("Live", "night-1")],
                Verdict::EditionUncertain(0),
            ),
            (
                "another night five seconds off",
                local(4, 0, false),
                vec![typed("Live", "night-1"), other_night(Some(5_000))],
                Verdict::Identified(0),
            ),
        ];
        let wrong: Vec<String> = cases
            .into_iter()
            .filter_map(|(name, local, releases, expected)| {
                let got = verdict(&local, &releases);
                (got != expected).then(|| format!("{name}: expected {expected:?}, got {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    fn named_verdict(local: &LocalAlbum, releases: &[Release], named: &str) -> Verdict {
        let matches: Vec<_> = releases
            .iter()
            .map(|release| match_release(local, release, &HashMap::new()))
            .collect();
        let prefs = EditionPrefs {
            named_group: Some(named),
            ..EditionPrefs::default()
        };
        decide(local, releases, &matches, prefs)
    }

    /// The files' tags name album G. A tie goes to G; a release of
    /// another group is only ever a guess; a live release of G won in a
    /// tie stays unconfirmed.
    #[test]
    fn the_group_the_files_name() {
        let album = |first_ms: u64| {
            let mut album = release("album", "Night Shift", 1);
            album.tracks = (0..10)
                .map(|index| ReleaseTrack {
                    id: format!("album-track-{index}"),
                    recording_id: format!("album-recording-{index}"),
                    title: if index == 0 {
                        "Night Shift".to_owned()
                    } else {
                        format!("Album Track {index}")
                    },
                    position: index + 1,
                    absolute_position: index + 1,
                    length_ms: Some(if index == 0 { first_ms } else { 200_000 }),
                    ..album.tracks[0].clone()
                })
                .collect();
            album
        };
        let single = one_track("single", "Night Shift", "Night Shift", Some("Official"));
        let file = one_file("Night Shift", "Night Shift");
        let mut live = edition("g", "night-1", "Night Shift");
        live.secondary_types = vec!["Live".to_owned()];
        let studio = edition("s", "studio", "Night Shift");
        let cases: Vec<(&str, LocalAlbum, Vec<Release>, &str, Verdict)> = vec![
            (
                "a tie goes to the named group",
                file.clone(),
                vec![album(200_000), single.clone()],
                "album",
                Verdict::Identified(0),
            ),
            (
                "a clearly better release of another group is a guess",
                file.clone(),
                vec![album(290_000), single.clone()],
                "album",
                Verdict::OutsideNamedGroup(1),
            ),
            (
                "so is a lone release of another group",
                file,
                vec![single],
                "album",
                Verdict::OutsideNamedGroup(0),
            ),
            (
                "a live release won in a tie stays unconfirmed",
                local(4, 0, false),
                vec![live, studio],
                "g",
                Verdict::EditionUncertain(0),
            ),
        ];
        let wrong: Vec<String> = cases
            .into_iter()
            .filter_map(|(name, local, releases, named, expected)| {
                let got = named_verdict(&local, &releases, named);
                (got != expected).then(|| format!("{name}: expected {expected:?}, got {got:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Files naming single S that carry album A's track ids: the release
    /// id decides, and the name gate sends them to review when the album
    /// title tag follows A.
    #[test]
    fn a_single_with_the_album_track_ids() {
        let album = edition("ga", "a", "Night Shift");
        let mut single = edition("gs", "s", "Blue Hour");
        single.tracks.truncate(2);
        for (index, track) in single.tracks.iter_mut().enumerate() {
            track.recording_id = format!("recording-{}", index + 1);
            track.title = TITLES[index + 1].to_owned();
        }
        let files = |title: &str| LocalAlbum {
            title: title.to_owned(),
            artist: "The Lanterns".to_owned(),
            tracks: (0..2)
                .map(|index| LocalTrack {
                    id: format!("t{index}"),
                    title: TITLES[index + 1].to_owned(),
                    track_number: index as u32 + 1,
                    disc_number: 1,
                    duration_secs: Some(200.0),
                    recording_mbid: Some(format!("recording-{}", index + 1)),
                    release_mbid: Some("s".to_owned()),
                    release_track_mbid: Some(format!("a-track-{}", index + 1)),
                    ..LocalTrack::default()
                })
                .collect(),
            ..LocalAlbum::default()
        };
        let releases = [album, single];
        assert_eq!(
            scored(&files("Night Shift"), &releases).0,
            Verdict::Review(ReviewReason::WeakMatch)
        );
        assert_eq!(
            scored(&files("Blue Hour"), &releases).0,
            Verdict::Identified(1)
        );
    }

    /// "[none]", Picard's and MusicBrainz's catalog number for a release
    /// without one, names no release: files saying it do not pick the
    /// edition that says it over a closer one.
    #[test]
    fn a_none_catalog_number_names_no_edition() {
        let mut far = edition("g", "far", "Night Shift");
        far.catalog_numbers = vec!["[none]".to_owned()];
        far.tracks[0].length_ms = Some(230_000);
        let near = edition("g", "near", "Night Shift");
        let mut files = local(4, 0, false);
        files.hints.catalog_number = EditionHints::catalog_key("[none]");
        assert_eq!(scored(&files, &[far, near]).0, Verdict::Identified(1));
    }
}
