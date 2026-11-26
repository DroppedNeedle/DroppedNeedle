//! The fresh releases source: the albums and EPs on the person's list of
//! new releases from their artists ([`new_releases`]) that the pool may
//! hold and the album gate offers ([`gate::may_offer`]: no live set, demo,
//! compilation or the rest), as `fresh` candidates. Each has a `fresh` path; one by an artist
//! close to theirs carries that artist's seed paths from this build's
//! ranking too, and one whose artist the ranking does not hold is left
//! out, since its line would name nobody. The list is read from kept rows
//! only: nothing is asked of a provider.

use std::collections::HashMap;

use super::super::candidate::{Candidate, CandidateKind, Path, PathArtist, PathKind, PathSource};
use super::super::rank::{self, AlbumGates, RankedArtist, Seed};
use crate::discover::listening::{AlbumFacts, AlbumRow, RankedAlbum, gate};
use crate::discover::new_releases::Listed;
use crate::metadata::models::ReleaseType;

/// The candidates `listed` gives through `gates`. `ranked` is this build's
/// ranking, `seeds` the person's seeds.
pub(crate) fn candidates(
    listed: &Listed,
    ranked: &[RankedArtist],
    seeds: &[Seed],
    gates: &AlbumGates<'_>,
) -> Vec<Candidate> {
    let ranked: HashMap<&str, &RankedArtist> = ranked
        .iter()
        .map(|artist| (artist.mbid.as_str(), artist))
        .collect();
    let seeds: HashMap<&str, &Seed> = seeds
        .iter()
        .map(|seed| (seed.mbid.as_str(), seed))
        .collect();
    listed
        .releases
        .iter()
        .filter_map(|row| {
            let album = &row.album;
            if album.in_library
                || !matches!(
                    album.release_type,
                    Some(ReleaseType::Album | ReleaseType::Ep)
                )
            {
                return None;
            }
            let group = album.musicbrainz_id.as_deref()?;
            let artist = album.artist_id.as_deref()?;
            if gates.artists.refuses(artist) || !gates.admits(group, artist) {
                return None;
            }
            let name = album.artist_name.clone().unwrap_or_default();
            // A release by an artist close to theirs carries the seeds that
            // point there; one by their own artist names that artist.
            let (seed_paths, support, group_no, group_support) = if row.reason.is_some() {
                let near = ranked.get(artist)?;
                (
                    near.paths.clone(),
                    near.support,
                    near.group,
                    near.group_support,
                )
            } else {
                let seed = seeds.get(artist);
                (
                    Vec::new(),
                    seed.map_or(0.0, |seed| seed.weight),
                    seed.and_then(|seed| seed.group),
                    0.0,
                )
            };
            let own = seeds.get(artist).map(|seed| PathArtist {
                mbid: seed.mbid.clone(),
                name: seed.name.clone(),
                plays: seed.plays,
                weight: seed.weight,
                albums_owned: seed.albums_owned,
            });
            // What the follow poll found came from the MusicBrainz source.
            let (source, fetched_at) = if listed.followed.contains(group) {
                (PathSource::Musicbrainz, None)
            } else {
                (PathSource::Listenbrainz, listed.read_at)
            };
            let fresh = Path {
                kind: PathKind::Fresh,
                source,
                fetched_at,
                artist: own.or_else(|| {
                    Some(PathArtist {
                        mbid: artist.to_owned(),
                        name: name.clone(),
                        plays: 0,
                        weight: 0.0,
                        albums_owned: 0,
                    })
                }),
                link: None,
                known_credit: None,
                fact: None,
            };
            let mut paths = vec![fresh];
            paths.extend(seed_paths);
            let shown = RankedAlbum {
                album: AlbumRow {
                    release_group_mbid: group.to_owned(),
                    title: album.title.clone(),
                    artist_name: name.clone(),
                    artist_mbid: Some(artist.to_owned()),
                    facts: AlbumFacts {
                        primary_type: album.release_type.map(|kind| {
                            match kind {
                                ReleaseType::Ep => "EP",
                                _ => "Album",
                            }
                            .to_owned()
                        }),
                        secondary_types: listed.secondary.get(group).cloned(),
                        first_release_date: album.release_date.clone(),
                        ..AlbumFacts::default()
                    },
                },
                listen_count: 0,
            };
            if !gate::may_offer(&shown.album, artist) {
                return None;
            }
            Some(rank::album_candidate(
                &shown,
                (artist, &name, None),
                CandidateKind::Fresh,
                (&paths, support, group_no, group_support),
                gates.artists,
            ))
        })
        .collect()
}
