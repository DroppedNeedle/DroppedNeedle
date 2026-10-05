//! Jellyfin wire shapes the adapter depends on.
//!
//! Field names are Jellyfin's PascalCase. Unknown fields are ignored and
//! genuinely optional ones default; identity does not: an item without an
//! `Id` (or the older `ItemId` some search hints carry), a session without
//! an `Id`, or a login answer without the user id or token fails decoding,
//! and the adapter reports that as an upstream error instead of rendering
//! an item nobody can open.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A list of items (`/Items`, `/Artists`, `/Playlists/{id}/Items`,
/// `/Items/{id}/Similar`, instant mixes).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ItemPage {
    /// Page items.
    #[serde(default)]
    pub items: Vec<Item>,
    /// Matching records upstream.
    #[serde(default)]
    pub total_record_count: Option<i64>,
}

/// `/Search/Hints` answer.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SearchHints {
    /// Hints, any item type.
    #[serde(default)]
    pub search_hints: Vec<Item>,
}

/// A list of named entries (`/MusicGenres`): the name is the identity.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NamedPage {
    /// Entries.
    #[serde(default)]
    pub items: Vec<Named>,
}

/// One named entry.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Named {
    /// Display name.
    pub name: String,
}

/// One Jellyfin item: album, artist, track, or playlist.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawItem")]
pub struct Item {
    /// Item id.
    pub id: String,
    /// Display name.
    pub name: Option<String>,
    /// `MusicAlbum`, `MusicArtist`, `Audio`, `Playlist`, ...
    pub kind: Option<String>,
    /// Album title (tracks).
    pub album: Option<String>,
    /// Album id (tracks).
    pub album_id: Option<String>,
    /// Parent id.
    pub parent_id: Option<String>,
    /// Album artist name.
    pub album_artist: Option<String>,
    /// Linked artists.
    pub artist_items: Vec<NamedRef>,
    /// Artist names.
    pub artists: Vec<String>,
    /// Release year.
    pub production_year: Option<i64>,
    /// Child count (album tracks, playlist entries).
    pub child_count: Option<i64>,
    /// Album count (artists).
    pub album_count: Option<i64>,
    /// Track number.
    pub index_number: Option<i64>,
    /// Disc number.
    pub parent_index_number: Option<i64>,
    /// Length in 100ns ticks.
    pub run_time_ticks: Option<i64>,
    /// External ids (`MusicBrainzAlbum`, `MusicBrainzReleaseGroup`, ...).
    pub provider_ids: HashMap<String, Option<String>>,
    /// Image tags by kind.
    pub image_tags: HashMap<String, String>,
    /// The calling user's play data.
    pub user_data: Option<UserData>,
}

impl Item {
    /// One provider id, when set and non-empty.
    pub fn provider(&self, key: &str) -> Option<String> {
        self.provider_ids
            .get(key)
            .cloned()
            .flatten()
            .filter(|value| !value.is_empty())
    }

    /// The primary image tag.
    pub fn primary_tag(&self) -> Option<&str> {
        self.image_tags.get("Primary").map(String::as_str)
    }

    /// The user's play count, zero when unknown.
    pub fn play_count(&self) -> i64 {
        self.user_data
            .as_ref()
            .and_then(|data| data.play_count)
            .unwrap_or(0)
    }
}

/// The item as it arrives: `Id`, or the older `ItemId` on search hints.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawItem {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    item_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "Type")]
    kind: Option<String>,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    album_id: Option<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    album_artist: Option<String>,
    #[serde(default)]
    artist_items: Vec<NamedRef>,
    #[serde(default)]
    artists: Vec<String>,
    #[serde(default)]
    production_year: Option<i64>,
    #[serde(default)]
    child_count: Option<i64>,
    #[serde(default)]
    album_count: Option<i64>,
    #[serde(default)]
    index_number: Option<i64>,
    #[serde(default)]
    parent_index_number: Option<i64>,
    #[serde(default)]
    run_time_ticks: Option<i64>,
    #[serde(default)]
    provider_ids: HashMap<String, Option<String>>,
    #[serde(default)]
    image_tags: HashMap<String, String>,
    #[serde(default)]
    user_data: Option<UserData>,
}

impl TryFrom<RawItem> for Item {
    type Error = String;

    fn try_from(raw: RawItem) -> Result<Self, Self::Error> {
        let id = raw
            .id
            .filter(|id| !id.is_empty())
            .or(raw.item_id.filter(|id| !id.is_empty()))
            .ok_or_else(|| "item without an Id".to_owned())?;
        Ok(Self {
            id,
            name: raw.name,
            kind: raw.kind,
            album: raw.album,
            album_id: raw.album_id,
            parent_id: raw.parent_id,
            album_artist: raw.album_artist,
            artist_items: raw.artist_items,
            artists: raw.artists,
            production_year: raw.production_year,
            child_count: raw.child_count,
            album_count: raw.album_count,
            index_number: raw.index_number,
            parent_index_number: raw.parent_index_number,
            run_time_ticks: raw.run_time_ticks,
            provider_ids: raw.provider_ids,
            image_tags: raw.image_tags,
            user_data: raw.user_data,
        })
    }
}

/// An id and a name (`ArtistItems`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NamedRef {
    /// Linked id.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
}

/// The calling user's data on an item.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserData {
    /// Plays.
    #[serde(default)]
    pub play_count: Option<i64>,
}

/// `/System/Info`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfo {
    /// Server name.
    #[serde(default)]
    pub server_name: Option<String>,
    /// Server version.
    #[serde(default)]
    pub version: Option<String>,
}

/// `/Audio/{id}/Lyrics` (`LyricDto`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Lyrics {
    /// Lines in order.
    #[serde(default)]
    pub lyrics: Vec<LyricLine>,
}

/// One lyric line; `Start` is in 100ns ticks.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LyricLine {
    /// Line text.
    #[serde(default)]
    pub text: Option<String>,
    /// Start offset in ticks, when synced.
    #[serde(default)]
    pub start: Option<i64>,
}

/// One `/Sessions` entry.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Session {
    /// Session id.
    pub id: String,
    /// Listening user.
    #[serde(default)]
    pub user_name: Option<String>,
    /// Device label.
    #[serde(default)]
    pub device_name: Option<String>,
    /// Client label.
    #[serde(default)]
    pub client: Option<String>,
    /// What is playing, when anything is.
    #[serde(default)]
    pub now_playing_item: Option<Item>,
    /// Position and pause state.
    #[serde(default)]
    pub play_state: Option<PlayState>,
}

/// Playback state of a session.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlayState {
    /// Position in ticks.
    #[serde(default)]
    pub position_ticks: Option<i64>,
    /// True while paused.
    #[serde(default)]
    pub is_paused: Option<bool>,
}

/// `/Items/Filters` (the legacy query filters).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct QueryFilters {
    /// Release years.
    #[serde(default)]
    pub years: Vec<i64>,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Studios.
    #[serde(default)]
    pub studios: Vec<String>,
}

/// `POST /Users/AuthenticateByName` answer.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticationResult {
    /// The signed-in user.
    pub user: AuthenticatedUser,
    /// User-scoped access token.
    pub access_token: String,
}

/// The user in an authentication answer.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticatedUser {
    /// Jellyfin user id.
    pub id: String,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
}

/// Body for `/Sessions/Playing`, `/Progress`, and `/Stopped`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackReport<'a> {
    /// Item being played.
    pub item_id: &'a str,
    /// Playback session id; empty because v3 never opens one.
    pub play_session_id: &'a str,
    /// Whether the client can seek.
    pub can_seek: bool,
    /// Position in ticks, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position_ticks: Option<i64>,
    /// Pause state (progress reports only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_paused: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An item with no id must not decode into an empty-id album.
    #[test]
    fn items_without_an_id_fail_to_decode() {
        let missing = serde_json::from_str::<ItemPage>(r#"{"Items":[{"Name":"Ghost"}]}"#);
        assert!(missing.is_err());
        let hint = serde_json::from_str::<SearchHints>(
            r#"{"SearchHints":[{"ItemId":"jf-1","Name":"Old hint"}]}"#,
        )
        .expect("ItemId still identifies a hint");
        assert_eq!(hint.search_hints[0].id, "jf-1");
    }
}
