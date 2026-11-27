//! A pick's reason, pure: the one line shown on it, built from its stored
//! paths and nothing else.
//!
//! A new release says so first, naming its artist and, for an artist close
//! to theirs, the one of theirs it is closest to. Then a band link, naming
//! the person's artist it runs through (and the member, for a band sharing
//! one). Then the person's artists, then a gap in an artist they collect
//! (how many of the artist's albums the library holds), then a service's
//! pick. A line names up to three of their artists, strongest first. A
//! pick whose line would name an artist that is no longer one of their
//! seeds has no reason, and is not offered. No score, share or rank is ever
//! part of a line.

use std::collections::HashSet;

use super::candidate::{BandLinkRole, Candidate, Path, PathKind, PathSource};
use crate::discover::models::{PickReason, PickReasonArtist, PickReasonKind};
use droppedneedle_library::sound::Closeness;

/// The most of the person's artists a line names.
const NAMED_MAX: usize = 3;

/// The strongest seed paths of `candidate`, strongest first, each with its
/// artist and link.
pub(crate) fn seed_paths(candidate: &Candidate) -> Vec<&Path> {
    let mut paths: Vec<&Path> = candidate
        .paths
        .iter()
        .filter(|path| path.kind == PathKind::Seed && path.artist.is_some() && path.link.is_some())
        .collect();
    paths.sort_by(|a, b| b.support().total_cmp(&a.support()));
    paths
}

/// The line of a track found by sound: how close it sounds to the
/// person's own track `title` by `artist`. A sound find is always a
/// library track, and the line names the track it was found from.
pub(crate) fn sound_line(closeness: Closeness, title: &str, artist: &str) -> String {
    let how = match closeness {
        Closeness::VeryClose => "Sounds very close to",
        Closeness::Close => "Sounds close to",
        Closeness::Further => "Sounds a little like",
    };
    format!("{how} {title} by {artist}.")
}

/// The line of a new release by `artist`: one of the person's, or close to
/// their `near`.
pub(crate) fn fresh_line(artist: &str, near: Option<&str>) -> String {
    match near {
        Some(near) => format!("New from {artist}, close to {near}."),
        None => format!("New from {artist}."),
    }
}

/// The line of a gap in an artist the library holds `owned` albums of.
pub(crate) fn gap_line(owned: i64) -> String {
    match owned {
        1 => "You have one of their albums, not this one.".to_owned(),
        owned => format!("You have {owned} of their albums, not this one."),
    }
}

/// The strongest band-link path of `candidate`, with its artist and band
/// link.
pub(crate) fn fact_path(candidate: &Candidate) -> Option<&Path> {
    let mut strongest: Option<&Path> = None;
    for path in candidate
        .paths
        .iter()
        .filter(|path| path.kind == PathKind::Fact && path.artist.is_some() && path.fact.is_some())
    {
        if strongest.is_none_or(|best| path.support() > best.support()) {
            strongest = Some(path);
        }
    }
    strongest
}

/// The line of a band link from the person's `seed` to `artist`, the
/// pick's, in `role`, through `via` when one member.
pub(crate) fn band_line(artist: &str, seed: &str, role: BandLinkRole, via: Option<&str>) -> String {
    match (role, via) {
        (BandLinkRole::Member, _) => format!("{artist} of {seed}."),
        (BandLinkRole::Band, _) => format!("With {seed}."),
        (BandLinkRole::OtherName, _) => format!("{seed} under another name."),
        (BandLinkRole::SharedMember, Some(via)) => format!("With {via} of {seed}."),
        (BandLinkRole::SharedMember, None) => format!("With members of {seed}."),
    }
}

/// The line naming `names` (one to three).
fn close_to(names: &[&str], closest_new: bool) -> String {
    match names {
        [one] if closest_new => format!("Closest artist to {one} that isn't in your history."),
        [one] => format!("Close to {one}."),
        [one, two] => format!("Close to both {one} and {two}."),
        [one, two, three, ..] => format!("Near {one}, {two} and {three}."),
        [] => String::new(),
    }
}

/// Why `candidate` is offered, read from its paths; `None` when no path
/// gives a line, or one the line would name no longer holds: its artist is
/// not among `seeds`, the person's seeds now (lowercased).
pub fn reason(candidate: &Candidate, seeds: &HashSet<String>) -> Option<PickReason> {
    let paths = seed_paths(candidate);
    if candidate
        .paths
        .iter()
        .any(|path| path.kind == PathKind::Fresh)
    {
        return fresh_reason(candidate, &paths, seeds);
    }
    if let Some(found) = fact_path(candidate).and_then(|path| fact_reason(candidate, path, seeds)) {
        return Some(found);
    }
    if !paths.is_empty() {
        let named: Vec<&Path> = paths.iter().take(NAMED_MAX).copied().collect();
        let mut artists = Vec::with_capacity(named.len());
        let (mut plays, mut owned) = (0, 0);
        for path in &named {
            let artist = path.artist.as_ref()?;
            if artist.name.trim().is_empty() || !seeds.contains(&artist.mbid) {
                return None;
            }
            plays += artist.plays;
            owned += artist.albums_owned;
            artists.push(PickReasonArtist {
                artist_mbid: artist.mbid.clone(),
                name: artist.name.trim().to_owned(),
            });
        }
        let names: Vec<&str> = artists.iter().map(|artist| artist.name.as_str()).collect();
        let closest_new =
            paths.len() == 1 && named[0].link.as_ref().is_some_and(|link| link.closest_new);
        let mut line = close_to(&names, closest_new);
        // Seeds from the shelves alone: what they own is the reason.
        match (plays, owned) {
            (0, 1) => line.push_str(" You own one of their albums."),
            (0, owned) if owned > 1 => {
                line.push_str(&format!(" You own {owned} of their albums."));
            }
            _ => {}
        }
        return Some(PickReason {
            kind: PickReasonKind::YourArtists,
            line,
            artists,
        });
    }
    // A gap: the artist's albums the library holds are the reason.
    if let Some(artist) = candidate
        .paths
        .iter()
        .find(|path| path.kind == PathKind::Library)
        .and_then(|path| path.artist.as_ref())
        .filter(|artist| artist.albums_owned > 0 && !artist.name.trim().is_empty())
    {
        return Some(PickReason {
            kind: PickReasonKind::YourArtists,
            line: gap_line(artist.albums_owned),
            artists: vec![PickReasonArtist {
                artist_mbid: artist.mbid.clone(),
                name: artist.name.trim().to_owned(),
            }],
        });
    }
    candidate
        .paths
        .iter()
        .find(|path| path.kind == PathKind::Source)
        .and_then(|path| match path.source {
            PathSource::Listenbrainz => Some(PickReason {
                kind: PickReasonKind::Source,
                line: "From ListenBrainz's weekly picks for you.".to_owned(),
                artists: Vec::new(),
            }),
            _ => None,
        })
}

/// The line of a band link `path`, whose artist must still be one of
/// `seeds`.
fn fact_reason(candidate: &Candidate, path: &Path, seeds: &HashSet<String>) -> Option<PickReason> {
    let seed = path.artist.as_ref()?;
    let fact = path.fact.as_ref()?;
    // The linked artist by its own name: the album may credit another.
    let artist = fact
        .artist_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(candidate.artist_name.trim());
    let name = seed.name.trim();
    if artist.is_empty() || name.is_empty() || !seeds.contains(&seed.mbid) {
        return None;
    }
    let via = fact
        .via
        .as_ref()
        .map(|via| via.name.trim())
        .filter(|via| !via.is_empty());
    Some(PickReason {
        kind: PickReasonKind::Fact,
        line: band_line(artist, name, fact.role, via),
        artists: vec![PickReasonArtist {
            artist_mbid: seed.mbid.clone(),
            name: name.to_owned(),
        }],
    })
}

/// The line of a new release: by an artist close to theirs, naming the
/// seed it is closest to, which must still be one of `seeds`; else by
/// their own artist.
fn fresh_reason(
    candidate: &Candidate,
    paths: &[&Path],
    seeds: &HashSet<String>,
) -> Option<PickReason> {
    let name = candidate.artist_name.trim();
    if name.is_empty() {
        return None;
    }
    let (line, artist) = match paths.first() {
        Some(path) => {
            let near = path.artist.as_ref()?;
            if near.name.trim().is_empty() || !seeds.contains(&near.mbid) {
                return None;
            }
            (
                fresh_line(name, Some(near.name.trim())),
                PickReasonArtist {
                    artist_mbid: near.mbid.clone(),
                    name: near.name.trim().to_owned(),
                },
            )
        }
        None => (
            fresh_line(name, None),
            PickReasonArtist {
                artist_mbid: candidate.artist_mbid.clone(),
                name: name.to_owned(),
            },
        ),
    };
    Some(PickReason {
        kind: PickReasonKind::YourArtists,
        line,
        artists: vec![artist],
    })
}
