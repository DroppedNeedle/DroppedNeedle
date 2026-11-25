//! A pick's reason, pure: the one line shown on it, built from its stored
//! paths and nothing else.
//!
//! A new release says so first, naming its artist and, for an artist close
//! to theirs, the one of theirs it is closest to. Then the person's
//! artists, then a service's pick. A line names up to three of their
//! artists, strongest first. A pick whose line would name
//! an artist that is no longer one of their seeds has no reason, and is not
//! offered. No score, share or rank is ever part of a line.

use std::collections::HashSet;

use super::candidate::{Candidate, Path, PathKind, PathSource};
use crate::discover::models::{PickReason, PickReasonArtist, PickReasonKind};

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

/// The line of a new release by `artist`: one of the person's, or close to
/// their `near`.
pub(crate) fn fresh_line(artist: &str, near: Option<&str>) -> String {
    match near {
        Some(near) => format!("New from {artist}, close to {near}."),
        None => format!("New from {artist}."),
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
