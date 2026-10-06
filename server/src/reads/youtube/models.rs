//! Wire shapes for the YouTube link routes. Field names match v2 so the web
//! client's album bar, track buttons and YouTube library page read them as
//! before.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

/// A saved album entry. It holds a full-album video, track videos, or both:
/// an entry created by track generation alone has no `video_id`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct YouTubeLink {
    /// Album id (a release-group MBID, or `manual-...` for hand-made links).
    pub album_id: String,
    /// Full-album video id, when one is saved.
    pub video_id: Option<String>,
    /// Album title.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Embed URL for the full-album video, when one is saved.
    pub embed_url: Option<String>,
    /// Cover image URL, when known.
    pub cover_url: Option<String>,
    /// When the link was saved (ISO-8601, UTC).
    pub created_at: String,
    /// True when a person pasted the video rather than search finding it.
    pub is_manual: bool,
    /// How many track videos the album has.
    pub track_count: i64,
}

/// A saved video for one track.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct YouTubeTrackLink {
    /// Album id the track belongs to.
    pub album_id: String,
    /// Album title.
    pub album_name: String,
    /// Disc number, 1 for single-disc albums.
    pub disc_number: i64,
    /// Track position on its disc.
    pub track_number: i64,
    /// Track title.
    pub track_name: String,
    /// Video id.
    pub video_id: String,
    /// Artist name.
    pub artist_name: String,
    /// Embed URL for the video.
    pub embed_url: String,
    /// When the link was saved (ISO-8601, UTC).
    pub created_at: String,
}

/// Today's YouTube search budget.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct YouTubeQuotaStatus {
    /// Searches spent today.
    pub used: i64,
    /// Daily budget.
    pub limit: i64,
    /// Searches left today, never negative.
    pub remaining: i64,
    /// UTC date the numbers belong to, YYYY-MM-DD.
    pub date: String,
}

/// Body for `POST /youtube/generate`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct YouTubeLinkGenerateRequest {
    /// Artist name to search for.
    pub artist_name: String,
    /// Album title to search for.
    pub album_name: String,
    /// Album id the link is saved under.
    pub album_id: String,
    /// Cover image URL to show with the link.
    #[serde(default)]
    pub cover_url: Option<String>,
}

/// Answer of `POST /youtube/generate`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YouTubeLinkResponse {
    /// The saved (or already saved) album link.
    pub link: YouTubeLink,
    /// Search budget after the call.
    pub quota: YouTubeQuotaStatus,
}

/// Body for `POST /youtube/manual`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct YouTubeManualLinkRequest {
    /// Album title.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// A YouTube URL or a bare 11-character video id.
    pub youtube_url: String,
    /// Cover image URL.
    #[serde(default)]
    pub cover_url: Option<String>,
    /// Album id to save under. Left out, a `manual-...` id is made up.
    #[serde(default)]
    pub album_id: Option<String>,
}

/// Body for `PUT /youtube/link/{album_id}`. Fields left out keep their
/// saved value; `cover_url: null` clears the cover.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct YouTubeLinkUpdateRequest {
    /// New YouTube URL or video id.
    #[serde(default)]
    pub youtube_url: Option<String>,
    /// New album title.
    #[serde(default)]
    pub album_name: Option<String>,
    /// New artist name.
    #[serde(default)]
    pub artist_name: Option<String>,
    /// New cover URL; `null` clears it, leaving it out keeps it.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>)]
    pub cover_url: Option<Option<String>>,
}

/// Body for `POST /youtube/generate-track`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct YouTubeTrackLinkGenerateRequest {
    /// Album id the track belongs to.
    pub album_id: String,
    /// Album title.
    pub album_name: String,
    /// Artist name to search for.
    pub artist_name: String,
    /// Track title to search for.
    pub track_name: String,
    /// Track position on its disc.
    pub track_number: i64,
    /// Disc number, 1 when left out.
    #[serde(default = "first_disc")]
    pub disc_number: i64,
    /// Cover image URL for the album entry.
    #[serde(default)]
    pub cover_url: Option<String>,
}

/// Answer of `POST /youtube/generate-track`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YouTubeTrackLinkResponse {
    /// The saved (or already saved) track link.
    pub track_link: YouTubeTrackLink,
    /// Search budget after the call.
    pub quota: YouTubeQuotaStatus,
}

/// One track in a batch generation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct YouTubeTrackInput {
    /// Track title to search for.
    pub track_name: String,
    /// Track position on its disc.
    pub track_number: i64,
    /// Disc number, 1 when left out.
    #[serde(default = "first_disc")]
    pub disc_number: i64,
}

/// Body for `POST /youtube/generate-tracks`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct YouTubeTrackLinkBatchGenerateRequest {
    /// Album id the tracks belong to.
    pub album_id: String,
    /// Album title.
    pub album_name: String,
    /// Artist name to search for.
    pub artist_name: String,
    /// Tracks to find videos for.
    pub tracks: Vec<YouTubeTrackInput>,
    /// Cover image URL for the album entry.
    #[serde(default)]
    pub cover_url: Option<String>,
}

/// A track the batch could not find a video for.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct YouTubeTrackLinkFailure {
    /// Disc number.
    pub disc_number: i64,
    /// Track position on its disc.
    pub track_number: i64,
    /// Track title.
    pub track_name: String,
    /// Why, in a sentence for the user.
    pub reason: String,
}

/// Answer of `POST /youtube/generate-tracks`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YouTubeTrackLinkBatchResponse {
    /// Saved links, including tracks that already had one.
    pub track_links: Vec<YouTubeTrackLink>,
    /// Tracks without a video.
    pub failed: Vec<YouTubeTrackLinkFailure>,
    /// Search budget after the call.
    pub quota: YouTubeQuotaStatus,
}

fn first_disc() -> i64 {
    1
}

/// Tell "sent as null" (`Some(None)`) apart from "left out" (`None`).
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
