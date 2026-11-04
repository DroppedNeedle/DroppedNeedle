//! How many tracks an album's edition has beside the ones the library
//! holds: the one rule the album cards, the discography rows and the
//! player apps' `releaseSongCount` read.
//!
//! The edition's count is the matched MusicBrainz release's tracklist while
//! the metadata cache holds it, else the track totals the files' tags give
//! (see [`tagged_song_count`](super::player::tagged_song_count)). A guess
//! the files' tags contradict (`NAMED_GROUP_DIFFERS`) never lends its
//! release: its tags count. Nothing here asks MusicBrainz.

use std::collections::HashMap;

use droppedneedle_kernel::db::statement::ListLen;
use rusqlite::types::Value;

use super::sql;
use super::store::{LocalCatalog, rows, unsigned};
use super::stores::StoreError;
use crate::metadata::Catalog;

/// Most ids bound into one `IN (...)` list.
const MAX_BINDS: usize = 500;

/// What the library holds of one album, and how many tracks its edition
/// has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlbumTracks {
    /// The album's indexed tracks.
    pub held: u64,
    /// The edition's tracks; `None` when neither the release nor the tags
    /// tell.
    pub expected: Option<u64>,
}

/// [`AlbumTracks`] for each of `album_ids` that names an album, as the
/// module describes. `releases` is the metadata catalog whose cache holds
/// the releases; a release it does not hold counts from the tags.
pub async fn album_tracks(
    local: &LocalCatalog,
    releases: &Catalog,
    album_ids: &[String],
) -> Result<HashMap<String, AlbumTracks>, StoreError> {
    let mut found: Vec<(String, u64, Option<String>)> = Vec::new();
    for chunk in album_ids.chunks(MAX_BINDS) {
        let values: Vec<Value> = chunk.iter().map(|id| Value::Text(id.clone())).collect();
        found.extend(
            local
                .read("library.albums.edition_tracks", move |conn| {
                    rows(
                        conn,
                        &sql::album_edition_tracks(ListLen::new(values.len())),
                        &values,
                        |row| Ok((row.get(0)?, unsigned(row.get(1)?), row.get(2)?)),
                    )
                })
                .await?,
        );
    }
    let mut wanted: Vec<String> = found
        .iter()
        .filter_map(|(_, _, release)| release.clone())
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    let held = if wanted.is_empty() {
        HashMap::new()
    } else {
        releases.held_release_track_counts(&wanted).await
    };
    let from_release = |release: &Option<String>| {
        release
            .as_ref()
            .and_then(|mbid| held.get(mbid))
            .map(|count| u64::from(*count))
    };
    let untold: Vec<String> = found
        .iter()
        .filter(|(_, _, release)| from_release(release).is_none())
        .map(|(id, _, _)| id.clone())
        .collect();
    let tagged = if untold.is_empty() {
        HashMap::new()
    } else {
        local.tagged_song_counts(&untold).await?
    };
    Ok(found
        .into_iter()
        .map(|(id, held, release)| {
            let expected = from_release(&release)
                .or_else(|| tagged.get(&id).map(|count| u64::from(*count)))
                .filter(|count| *count > 0);
            (id, AlbumTracks { held, expected })
        })
        .collect())
}

/// What the library holds of one release group over all its owning
/// albums: their tracks add up (an album split over folders) to the
/// largest edition count among them at most, since a second copy holds
/// the same tracks again. `None` for no owners.
#[must_use]
pub fn group_tracks(owners: &[AlbumTracks]) -> Option<AlbumTracks> {
    let held = owners
        .iter()
        .filter(|album| album.held > 0)
        .map(|album| album.held)
        .reduce(u64::saturating_add)?;
    let expected = owners.iter().filter_map(|album| album.expected).max();
    Some(AlbumTracks {
        held: expected.map_or(held, |expected| held.min(expected)),
        expected,
    })
}

#[cfg(test)]
mod tests {
    use super::{AlbumTracks, group_tracks};

    fn album(held: u64, expected: Option<u64>) -> AlbumTracks {
        AlbumTracks { held, expected }
    }

    /// Copies of one group add up to the edition's count at most; a group
    /// no copy holds a track of is not held.
    #[test]
    fn a_group_adds_its_copies_up_to_the_edition() {
        assert_eq!(
            group_tracks(&[album(5, Some(11)), album(6, None)]),
            Some(album(11, Some(11)))
        );
        assert_eq!(
            group_tracks(&[album(11, Some(11)), album(5, Some(11))]),
            Some(album(11, Some(11)))
        );
        assert_eq!(group_tracks(&[album(3, None)]), Some(album(3, None)));
        assert_eq!(group_tracks(&[album(0, Some(4))]), None);
        assert_eq!(group_tracks(&[]), None);
    }
}
