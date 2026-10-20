//! Spotify accounts and Web API wire shapes, read by `spotify_client.rs`.
//!
//! Tolerant of unknown fields. Identity is required: a token answer
//! without its `access_token`, a profile without its `id`, a playlist
//! without its `id` or its owner's `id`, and a playlist entry's track
//! without its `id` key fail decoding, and so does the page they are in.
//! A local file's track carries `"id": null`, which decodes and is skipped.
//! A page without its `items` list fails too, so a changed answer never
//! reads as an empty playlist. Other fields default to empty or no value.
//!
//! Each shape is checked against the answer the in-repo Spotify mock
//! records (`server/testkit/src/imports.rs`, named per shape below). No live
//! probe date is on record: a live call needs a user's OAuth grant.

use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};

/// A token grant, `POST /api/token` on the accounts host, for both the code
/// exchange and the refresh.
///
/// Checked against the Spotify mock's `spotify_token`.
#[derive(Deserialize)]
pub struct TokenAnswer {
    /// Fresh access token.
    pub access_token: String,
    /// Rotated refresh token; a refresh grant often carries none.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Lifetime in seconds.
    #[serde(default)]
    pub expires_in: Option<i64>,
}

/// The current user, `GET /me`.
///
/// Checked against the Spotify mock's `spotify_me`.
#[derive(Debug, Deserialize)]
pub struct Profile {
    /// Spotify user id, which the owner filter compares.
    pub id: String,
    /// Display name, null for some accounts.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// One page of the caller's playlists, `GET /me/playlists`.
///
/// Checked against the Spotify mock's `spotify_playlists`.
#[derive(Debug, Deserialize)]
pub struct PlaylistPage {
    /// The playlists on this page.
    pub items: Vec<SimplifiedPlaylist>,
    /// Playlists across every page.
    pub total: i64,
}

/// One playlist in the caller's list.
#[derive(Debug, Deserialize)]
pub struct SimplifiedPlaylist {
    /// Spotify playlist id.
    pub id: String,
    /// Playlist name.
    #[serde(default)]
    pub name: String,
    /// Description, null when unset.
    #[serde(default)]
    pub description: Option<String>,
    /// Cover images, null when none.
    #[serde(default)]
    pub images: Option<Vec<Image>>,
    /// Who owns it.
    pub owner: Owner,
    /// Track count on the current shape (`{"href", "total"}`).
    #[serde(default)]
    pub items: Option<TrackCount>,
    /// Track count on the older shape, null on the current one.
    #[serde(default)]
    pub tracks: Option<TrackCount>,
}

/// A playlist's owner.
#[derive(Debug, Deserialize)]
pub struct Owner {
    /// Spotify user id.
    pub id: String,
    /// Display name, null for some accounts.
    #[serde(default)]
    pub display_name: Option<String>,
}

/// The `items` or `tracks` member of a listed playlist. Only an object
/// with a `total` carries a count; any other value (`items` doubles as a
/// list elsewhere) carries none.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum TrackCount {
    /// An object with the count.
    Counted {
        /// Tracks on the playlist.
        total: i64,
    },
    /// Anything else.
    Other(IgnoredAny),
}

/// One cover image.
#[derive(Debug, Deserialize)]
pub struct Image {
    /// Image address.
    pub url: String,
    /// Width in pixels, null when Spotify does not know it.
    #[serde(default)]
    pub width: Option<i64>,
}

/// One playlist's metadata, `GET /playlists/{id}?fields=id,name,images,tracks.total`.
///
/// Checked against the Spotify mock's `spotify_playlist`.
#[derive(Debug, Deserialize)]
pub struct PlaylistInfo {
    /// Spotify playlist id. The import does not read it; decoding still
    /// requires it.
    #[allow(dead_code)]
    pub id: String,
    /// Cover images, null when none.
    #[serde(default)]
    pub images: Option<Vec<Image>>,
}

/// One page of a playlist's entries, `GET /playlists/{id}/items`.
///
/// Checked against the Spotify mock's `spotify_items`.
#[derive(Debug, Deserialize)]
pub struct EntryPage {
    /// The entries on this page.
    pub items: Vec<PlaylistEntry>,
    /// Address of the next page; null on the last.
    #[serde(default)]
    pub next: Option<String>,
}

/// One playlist entry. The track sits under `item` on the current shape
/// and under `track` on the older one; both are read, `track` first.
#[derive(Debug, Deserialize)]
pub struct PlaylistEntry {
    /// A local file, which has no Spotify track to import.
    #[serde(default)]
    pub is_local: bool,
    /// The track, older key. Null for a track Spotify removed.
    #[serde(default)]
    pub track: Option<EntryTrack>,
    /// The track, current key.
    #[serde(default)]
    pub item: Option<EntryTrack>,
}

/// The track (or episode) of one entry.
#[derive(Debug, Deserialize)]
pub struct EntryTrack {
    /// Spotify track id. The key is required; a local file sends null.
    #[serde(deserialize_with = "nullable")]
    pub id: Option<String>,
    /// `track` or `episode`; read as `track` when absent.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Track title.
    #[serde(default)]
    pub name: String,
    /// Credited artists.
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    /// The album it is on.
    #[serde(default)]
    pub album: Option<AlbumRef>,
    /// Outside ids (ISRC).
    #[serde(default)]
    pub external_ids: Option<ExternalIds>,
    /// Position on its disc.
    #[serde(default)]
    pub track_number: Option<i64>,
    /// Disc number.
    #[serde(default)]
    pub disc_number: Option<i64>,
    /// Length in milliseconds.
    #[serde(default)]
    pub duration_ms: Option<i64>,
}

/// One credited artist.
#[derive(Debug, Deserialize)]
pub struct ArtistRef {
    /// Artist name.
    #[serde(default)]
    pub name: Option<String>,
}

/// The album a track is on.
#[derive(Debug, Deserialize)]
pub struct AlbumRef {
    /// Spotify album id, null for a local file.
    #[serde(default)]
    pub id: Option<String>,
    /// Album title.
    #[serde(default)]
    pub name: Option<String>,
    /// Cover images.
    #[serde(default)]
    pub images: Option<Vec<Image>>,
}

/// A track's outside ids.
#[derive(Debug, Deserialize)]
pub struct ExternalIds {
    /// International Standard Recording Code.
    #[serde(default)]
    pub isrc: Option<String>,
}

/// A value that may be null but whose key must be present.
fn nullable<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}
