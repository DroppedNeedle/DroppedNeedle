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
//!   are not rivals: they go through edition choice instead.
//! - Edition choice looks at the group's editions within
//!   [`EDITION_COHORT`] (0.10, v2's consensus epsilon) of the best and
//!   prefers the release the tags name, then the
//!   closest full tracklist (missing tracks count here), then v2's order:
//!   Official status, earliest date, worldwide country, MBID.
//! - A lone candidate (no other release group plausible) needs v2's
//!   quorum: two tracks paired within 0.40, or a recording or
//!   release-track MBID on any file. Without it there is too little to
//!   go on and the verdict is insufficient evidence.
//! - A release that fails the album title or artist gate (see `score`)
//!   goes to review however close its tracks are.
//! - Live releases, and compilations the tags do not call compilations,
//!   need confirmation (v2): unless every file carries its release-track
//!   id, only the release group is pinned.
//!
//! "Library distance" leaves missing tracks out, as Lidarr does for files
//! already on disk: holding part of an album is normal.

use std::cmp::Ordering;

use super::model::{LocalAlbum, Release};
use super::score::ReleaseMatch;

pub const ACCEPT_ALBUM: f64 = 0.20;
pub const ACCEPT_TRACK: f64 = 0.40;
pub const REVIEW_CEILING: f64 = 0.35;
pub const GROUP_MARGIN: f64 = 0.05;
pub const EDITION_COHORT: f64 = 0.10;
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
}

/// Edition hints: the release the tags name. A person's choice never
/// reaches the matcher: a chosen album is not identified automatically.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditionPrefs<'a> {
    pub tagged: Option<&'a str>,
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
    if releases.is_empty() {
        return Verdict::NoCandidate;
    }
    let candidates: Vec<usize> = (0..releases.len())
        .filter(|index| eligible(&matches[*index]))
        .collect();
    let Some(&best) = candidates.iter().min_by(|a, b| {
        matches[**a]
            .library_distance()
            .total_cmp(&matches[**b].library_distance())
            .then_with(|| edition_order(**a, **b, releases, matches, prefs))
    }) else {
        return if matches.iter().any(|matched| !matched.conflicts.is_empty()) {
            Verdict::Contradictory
        } else {
            Verdict::Insufficient
        };
    };
    let best_distance = matches[best].library_distance();
    let group = releases[best].release_group_id.as_str();
    let rival = candidates
        .iter()
        .filter(|index| {
            !releases[**index]
                .release_group_id
                .eq_ignore_ascii_case(group)
        })
        .map(|index| matches[*index].library_distance())
        .fold(f64::INFINITY, f64::min);
    let Some(&chosen) = candidates
        .iter()
        .filter(|index| {
            releases[**index]
                .release_group_id
                .eq_ignore_ascii_case(group)
        })
        .filter(|index| matches[**index].library_distance() <= best_distance + EDITION_COHORT)
        .min_by(|a, b| edition_order(**a, **b, releases, matches, prefs))
    else {
        return Verdict::Insufficient;
    };
    if rival - best_distance < GROUP_MARGIN {
        return Verdict::Review(ReviewReason::Ambiguous);
    }
    let matched = &matches[chosen];
    if rival.is_infinite() && !quorum(local, matched) {
        return Verdict::Insufficient;
    }
    let unmatched_limit = if local.tracks.len() <= 20 { 1 } else { 2 };
    if matched.pairs.is_empty()
        || !matched.names_agree
        || matched.library_distance() > ACCEPT_ALBUM
        || matched.worst_track() > ACCEPT_TRACK
        || matched.unmatched.len() > unmatched_limit
    {
        return Verdict::Review(ReviewReason::WeakMatch);
    }
    if needs_type_confirmation(local, &releases[chosen])
        && !every_file_names_its_track(local, &releases[chosen], matched)
    {
        return Verdict::EditionUncertain(chosen);
    }
    Verdict::Identified(chosen)
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
    releases: &[Release],
    matches: &[ReleaseMatch],
    prefs: EditionPrefs<'_>,
) -> Ordering {
    let (left, right) = (&releases[a], &releases[b]);
    let named = |release: &Release, mbid: Option<&str>| -> bool {
        mbid.is_some_and(|mbid| release.answers_to(mbid))
    };
    named(right, prefs.tagged)
        .cmp(&named(left, prefs.tagged))
        .then_with(|| {
            rounded(matches[a].album_distance()).cmp(&rounded(matches[b].album_distance()))
        })
        .then_with(|| official(right).cmp(&official(left)))
        .then_with(|| date_key(left.date.as_deref()).cmp(&date_key(right.date.as_deref())))
        .then_with(|| worldwide(right).cmp(&worldwide(left)))
        .then_with(|| left.id.cmp(&right.id))
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
    use crate::library::matching::model::{CreditedArtist, LocalTrack, ReleaseTrack};
    use crate::library::matching::score::match_release;

    const TITLES: [&str; 4] = ["Lamplight", "Blue Hour", "Night Shift", "Last Train Home"];

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
                    id: format!("{group}-track-{index}"),
                    recording_id: format!("recording-{index}"),
                    title: TITLES[index].to_owned(),
                    artists: Vec::new(),
                    disc: 1,
                    position: index as u32 + 1,
                    absolute_position: index as u32 + 1,
                    length_ms: Some(200_000),
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
            (
                "lone candidate, one plain track",
                local(1, 0, false),
                vec![night(1)],
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
}
