//! Deezer wire shapes for the preview legs and the artist picture.
//!
//! Tolerant of unknown fields. A track's, an album's and an artist's id is the
//! identity of the answer and is required: one without it fails decoding. A
//! track whose preview is missing, null or empty is absence for that track
//! only. An error answer is an error object inside a 200; the classifier in
//! `preview.rs` reads it before these shapes decode.

use serde::Deserialize;

/// The track search, `GET /search`. The artist query and the field-scoped track
/// query both answer with it.
///
/// Verified against api.deezer.com/search on 2026-10-20: tests/fixtures/deezer/search_artist.json.
/// A search with no hits is an empty `data`: tests/fixtures/deezer/search_track_no_hits.json.
#[derive(Debug, Deserialize)]
pub struct TrackSearch {
    /// The hits, best first.
    pub data: Vec<Track>,
}

/// One track on the wire.
#[derive(Debug, Deserialize)]
pub struct Track {
    /// Deezer track id. The client does not read it; decoding still requires it.
    #[allow(dead_code)]
    pub id: i64,
    /// Full title.
    #[serde(default)]
    pub title: String,
    /// Short title, shown in preference to `title` when it is set.
    #[serde(default)]
    pub title_short: String,
    /// Position within the album.
    #[serde(default)]
    pub track_position: Option<i64>,
    /// 30-second MP3 URL. It is signed and expires, so it is resolved just in time.
    #[serde(default)]
    pub preview: Option<String>,
    /// The track's artist.
    #[serde(default)]
    pub artist: Option<Artist>,
}

/// An artist stub on a track or an album.
#[derive(Debug, Deserialize)]
pub struct Artist {
    /// Artist name. Matching compares it with the requested artist.
    #[serde(default)]
    pub name: String,
}

/// The album search, `GET /search/album`.
///
/// Verified against api.deezer.com/search/album on 2026-10-20: tests/fixtures/deezer/search_album.json.
#[derive(Debug, Deserialize)]
pub struct AlbumSearch {
    /// The hits, best first.
    pub data: Vec<Album>,
}

/// One album hit. Its id is the key the track list is fetched by, so an album
/// without one fails decoding.
#[derive(Debug, Deserialize)]
pub struct Album {
    /// Deezer album id.
    pub id: i64,
    /// The album's artist.
    #[serde(default)]
    pub artist: Option<Artist>,
}

/// One album's track list, `GET /album/{id}/tracks`.
///
/// Verified against api.deezer.com/album/{id}/tracks on 2026-10-20: tests/fixtures/deezer/album_tracks.json.
#[derive(Debug, Deserialize)]
pub struct AlbumTracks {
    /// The tracks, in album order.
    pub data: Vec<Track>,
}

/// One artist, `GET /artist/{id}`. Its picture is read by exact id.
///
/// Verified against api.deezer.com/artist/415 on 2026-10-20: tests/fixtures/deezer/artist.json.
/// The picture URLs point at cdn-images.dzcdn.net (the 1000 px one answered
/// 200 image/jpeg with no redirect). `picture` itself is
/// `api.deezer.com/artist/{id}/image`, which answers a 302 to the same CDN.
/// An unknown id gets the error object, code 800:
/// tests/fixtures/deezer/artist_not_found.json.
#[derive(Debug, Deserialize)]
pub struct DeezerArtist {
    /// Deezer artist id.
    pub id: i64,
    /// The 1000 by 1000 picture URL.
    #[serde(default)]
    pub picture_xl: Option<String>,
}

/// The error object Deezer answers with inside a 200. An unknown album id gets
/// it from `/album/{id}/tracks`: tests/fixtures/deezer/album_not_found.json, 2026-10-20.
#[derive(Debug, Deserialize)]
pub struct DeezerFault {
    /// The error, present when the answer is one.
    #[serde(default)]
    pub error: Option<DeezerFaultCode>,
}

/// Deezer's error object. Its code is what the classifier reads, so an object
/// without one fails decoding.
#[derive(Debug, Deserialize)]
pub struct DeezerFaultCode {
    /// Deezer's error number. 800 is "no data": the id names nothing.
    pub code: i64,
}
