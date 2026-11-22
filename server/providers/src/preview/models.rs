//! Deezer wire shapes for the preview legs, the artist picture and the barcode
//! lookup.
//!
//! Tolerant of unknown fields. A track's, an album's and an artist's id is the
//! identity of the answer and is required: one without it fails decoding. A
//! track whose preview is missing, null, empty or not a web address is
//! absence for that track only. An error answer is an error object inside a
//! 200; the classifier in `preview.rs` reads it before these shapes decode.

use serde::Deserialize;

use crate::web_address::{self, WebAddress};

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
    /// 30-second MP3 URL. It is signed and expires, so it is resolved just in
    /// time. Anything but an http or https address reads as none.
    #[serde(default, deserialize_with = "web_address::or_none")]
    pub preview: Option<WebAddress>,
    /// The track's artist.
    #[serde(default)]
    pub artist: Option<Artist>,
}

/// One track by id, `GET /track/{id}`, which a song link is read through.
///
/// Verified against api.deezer.com/track/13791930 on 2026-11-19: tests/fixtures/deezer/track.json.
/// An id Deezer does not hold is the error object, code 800: tests/fixtures/deezer/track_not_found.json.
#[derive(Debug, Deserialize)]
pub struct SongLookup {
    /// Deezer track id, compared with the id asked for.
    pub id: i64,
    /// Full title.
    #[serde(default)]
    pub title: String,
    /// Short title, without the version in brackets.
    #[serde(default)]
    pub title_short: String,
    /// International Standard Recording Code.
    #[serde(default)]
    pub isrc: Option<String>,
    /// Length in seconds.
    #[serde(default)]
    pub duration: Option<i64>,
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

/// One album by barcode, `GET /album/upc:{barcode}`. The id is the answer; the
/// barcode is compared with the one asked for, so both fields are required.
///
/// Verified against api.deezer.com/album/upc:{barcode} on 2026-11-21 (the
/// leading-zero form on 2026-11-22): In Rainbows (12 digits) is
/// tests/fixtures/deezer/album_upc_12.json and Mellon Collie (13 digits) is
/// album_upc_13.json. The 14-digit form, the leading-zero GTIN-13 form and an
/// unknown barcode answer the error object, code 800: album_upc_14.json,
/// album_upc_13_leading_zero.json and album_upc_unknown.json.
#[derive(Debug, Deserialize)]
pub struct AlbumByUpc {
    /// Deezer album id. Decoding requires it.
    pub id: u64,
    /// The album's barcode as Deezer holds it, digits as text.
    pub upc: String,
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
