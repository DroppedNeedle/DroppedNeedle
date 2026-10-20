//! iTunes Search API wire shapes, read by the album search in `itunes.rs` and
//! by the song searches in `preview.rs`.
//!
//! Tolerant of unknown fields. A hit's identity is required: an album hit
//! without its `collectionId`, or a song hit without its `trackId`, fails
//! decoding, and so does the page it is in. A page without its `results` list
//! fails too, so a changed answer never reads as a search with no hits. The
//! other fields default to an empty string or to no value, which the callers
//! read as a hit to skip.

use serde::Deserialize;

/// An album search, `GET /search?entity=album`.
///
/// Verified against itunes.apple.com/search on 2026-10-20: tests/fixtures/itunes/search_album.json.
/// A search with no hits is an empty list: tests/fixtures/itunes/search_no_results.json.
#[derive(Debug, Deserialize)]
pub struct AlbumSearch {
    /// The hits, best first. An empty list is a search with no hits.
    pub results: Vec<AlbumHit>,
}

/// One album hit.
#[derive(Debug, Deserialize)]
pub struct AlbumHit {
    /// Store collection id. The client does not read it; decoding still requires it.
    #[allow(dead_code)]
    #[serde(rename = "collectionId")]
    pub collection_id: i64,
    /// Result kind. Only `collection` rows are albums.
    #[serde(default, rename = "wrapperType")]
    pub wrapper_type: String,
    /// Album title.
    #[serde(default, rename = "collectionName")]
    pub collection_name: String,
    /// Album artist.
    #[serde(default, rename = "artistName")]
    pub artist_name: String,
    /// Store page for the album.
    #[serde(default, rename = "collectionViewUrl")]
    pub collection_view_url: String,
}

/// A song search, `GET /search?entity=song`, for a track preview or an album's
/// tracks.
///
/// Verified against itunes.apple.com/search on 2026-10-20: tests/fixtures/itunes/search_track.json.
/// Verified against itunes.apple.com/search on 2026-10-20: tests/fixtures/itunes/search_album_tracks.json.
#[derive(Debug, Deserialize)]
pub struct SongSearch {
    /// The hits, best first. An empty list is a search with no hits.
    pub results: Vec<SongHit>,
}

/// One song hit.
#[derive(Debug, Deserialize)]
pub struct SongHit {
    /// Store track id. The client does not read it; decoding still requires it.
    #[allow(dead_code)]
    #[serde(rename = "trackId")]
    pub track_id: i64,
    /// Song artist.
    #[serde(default, rename = "artistName")]
    pub artist_name: String,
    /// Song title.
    #[serde(default, rename = "trackName")]
    pub track_name: String,
    /// Album title.
    #[serde(default, rename = "collectionName")]
    pub collection_name: String,
    /// 30-second AAC preview URL.
    #[serde(default, rename = "previewUrl")]
    pub preview_url: String,
    /// Position within the album.
    #[serde(default, rename = "trackNumber")]
    pub track_number: Option<i64>,
}
