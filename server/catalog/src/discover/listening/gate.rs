//! What may be offered as one artist's album: a release group whose
//! primary type is Album or EP, that is not a live set, demo or
//! compilation, and that credits the artist. Every read of an artist's
//! albums answers through it, so no surface offers a single as an album
//! or files another artist's album under this one. A fact that is not
//! known passes its test: rows kept before the facts were read, and
//! ListenBrainz's rows, which carry no secondary types.

use super::rows::{AlbumRow, RankedAlbum};

/// Secondary types that keep a release group off every album shelf.
const LEFT_OUT: [&str; 3] = ["Live", "Demo", "Compilation"];

/// Whether `album` may be offered as an album by `artist_mbid`.
pub fn may_offer(album: &AlbumRow, artist_mbid: &str) -> bool {
    let facts = &album.facts;
    let album_or_ep = facts.primary_type.as_deref().is_none_or(|primary| {
        primary.eq_ignore_ascii_case("album") || primary.eq_ignore_ascii_case("ep")
    });
    let left_out = facts.secondary_types.as_ref().is_some_and(|types| {
        types.iter().any(|kind| {
            LEFT_OUT
                .iter()
                .any(|out| kind.trim().eq_ignore_ascii_case(out))
        })
    });
    let artist = artist_mbid.trim();
    let credited = facts
        .credited_artist_mbids
        .as_ref()
        .is_none_or(|ids| ids.iter().any(|id| id.trim().eq_ignore_ascii_case(artist)));
    album_or_ep && !left_out && credited
}

/// The rows of `rows` that may be offered as albums by `artist_mbid`, in
/// their order. A row that fails is left out, never relabelled.
pub fn offered(rows: Vec<RankedAlbum>, artist_mbid: &str) -> Vec<RankedAlbum> {
    rows.into_iter()
        .filter(|row| may_offer(&row.album, artist_mbid))
        .collect()
}
