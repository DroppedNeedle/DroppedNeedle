//! Cover Art Archive wire shapes, read by the listing client in `coverart.rs`.
//!
//! Tolerant of unknown fields. A listing without its `images` list fails
//! decoding, and so does an image without its numeric `id`, which the
//! candidate id is built from. The other fields are optional. The flags
//! default to the less trusting answer: an image is not approved, front or
//! back until the archive says so.

use serde::Deserialize;

/// Thumbnail URLs keyed by pixel width.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct CaaThumbnails {
    /// 1200px thumbnail, when the archive generated one.
    #[serde(rename = "1200", default)]
    pub size_1200: Option<String>,
    /// 500px thumbnail, when the archive generated one.
    #[serde(rename = "500", default)]
    pub size_500: Option<String>,
    /// 250px thumbnail, when the archive generated one.
    #[serde(rename = "250", default)]
    pub size_250: Option<String>,
}

/// One image in a listing.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CaaImage {
    /// Whether MusicBrainz editors approved this image.
    #[serde(default)]
    pub approved: bool,
    /// Whether this image serves as the back cover.
    #[serde(default)]
    pub back: bool,
    /// Editor comment.
    #[serde(default)]
    pub comment: String,
    /// Whether this image serves as the front cover.
    #[serde(default)]
    pub front: bool,
    /// Numeric image id: required identity.
    pub id: u64,
    /// Original file URL, http or https. Empty when the answer has none.
    #[serde(default)]
    pub image: String,
    /// Thumbnails by width.
    #[serde(default)]
    pub thumbnails: CaaThumbnails,
    /// Type labels (`Front`, `Back`, `Spine`, `Booklet`; one image may carry
    /// several).
    #[serde(default)]
    pub types: Vec<String>,
}

/// The image listing of one release or release group.
///
/// Verified against coverartarchive.org/release/{mbid} on 2026-10-20: tests/fixtures/coverartarchive/release_found.json.
/// Verified against coverartarchive.org/release-group/{mbid} on 2026-10-20: tests/fixtures/coverartarchive/release_group_found.json.
/// A release the archive does not hold answers 404 with an HTML page and no listing, recorded on 2026-10-20: tests/fixtures/coverartarchive/release_not_found.html.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CaaResponse {
    /// Listed images, in archive order. Required: a page without it fails decoding.
    pub images: Vec<CaaImage>,
}
