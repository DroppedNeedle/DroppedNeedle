//! Plex wire shapes the adapter reads.
//!
//! Every answer arrives inside `MediaContainer`. Unknown fields are ignored
//! and optional fields default. Plex serializes some counts and ids as
//! strings and others as numbers, so both decode. A metadata row is a
//! mixed bag (albums, artists, tracks, playlists, history rows), so its
//! `ratingKey` is optional at decode time and required where a view needs
//! it: [`Metadata::rating_key`] fails with a contract error instead of
//! yielding an item nobody can open.

use serde::{Deserialize, Deserializer};

use super::adapter::AdapterError;

/// The JSON envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    /// The container.
    #[serde(rename = "MediaContainer")]
    pub container: Container,
}

/// `MediaContainer`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Container {
    /// Matching records upstream.
    #[serde(rename = "totalSize", deserialize_with = "lenient_int")]
    pub total_size: Option<i64>,
    /// Metadata rows.
    #[serde(rename = "Metadata")]
    pub metadata: Vec<Metadata>,
    /// Directory rows (sections, genres, moods).
    #[serde(rename = "Directory")]
    pub directory: Vec<Directory>,
    /// Hubs (search, discovery).
    #[serde(rename = "Hub")]
    pub hub: Vec<Hub>,
    /// Server name (`/`).
    #[serde(rename = "friendlyName")]
    pub friendly_name: Option<String>,
    /// Server version (`/`).
    pub version: Option<String>,
    /// Server machine id (`/identity`).
    #[serde(rename = "machineIdentifier")]
    pub machine_identifier: Option<String>,
}

/// One plex.tv `/resources` device. Client devices carry no token, so
/// every field is optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Resource {
    /// Device id; a server's matches its machine id.
    pub client_identifier: Option<String>,
    /// Comma-separated roles (`server`, `player`, ...).
    pub provides: Option<String>,
    /// The account's token for this server.
    pub access_token: Option<String>,
}

/// One `Directory` row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Directory {
    /// Section key.
    #[serde(deserialize_with = "lenient_string")]
    pub key: Option<String>,
    /// Title.
    pub title: Option<String>,
    /// Section type (`artist` for music).
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

/// One hub.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Hub {
    /// Hub type (`album`, `track`, `artist`, ...).
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Hub title.
    pub title: Option<String>,
    /// Rows.
    #[serde(rename = "Metadata")]
    pub metadata: Vec<Metadata>,
}

/// One metadata row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Metadata {
    /// Rating key: the item id.
    #[serde(rename = "ratingKey", deserialize_with = "lenient_string")]
    pub rating_key: Option<String>,
    /// Row type (`album`, `artist`, `track`, `playlist`).
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Title.
    pub title: Option<String>,
    /// Parent title (album artist for albums, album for tracks).
    #[serde(rename = "parentTitle")]
    pub parent_title: Option<String>,
    /// Grandparent title (artist for tracks).
    #[serde(rename = "grandparentTitle")]
    pub grandparent_title: Option<String>,
    /// Parent rating key.
    #[serde(rename = "parentRatingKey", deserialize_with = "lenient_string")]
    pub parent_rating_key: Option<String>,
    /// Track number.
    #[serde(deserialize_with = "lenient_int")]
    pub index: Option<i64>,
    /// Disc number.
    #[serde(rename = "parentIndex", deserialize_with = "lenient_int")]
    pub parent_index: Option<i64>,
    /// Release year.
    #[serde(deserialize_with = "lenient_int")]
    pub year: Option<i64>,
    /// Length in milliseconds.
    #[serde(deserialize_with = "lenient_int")]
    pub duration: Option<i64>,
    /// Child count (album tracks, playlist entries).
    #[serde(rename = "leafCount", deserialize_with = "lenient_int")]
    pub leaf_count: Option<i64>,
    /// True for a smart (rule-based) playlist.
    #[serde(deserialize_with = "lenient_bool")]
    pub smart: Option<bool>,
    /// Plays.
    #[serde(rename = "viewCount", deserialize_with = "lenient_int")]
    pub view_count: Option<i64>,
    /// Added, unix seconds.
    #[serde(rename = "addedAt", deserialize_with = "lenient_int")]
    pub added_at: Option<i64>,
    /// Last played, unix seconds.
    #[serde(rename = "lastViewedAt", deserialize_with = "lenient_int")]
    pub last_viewed_at: Option<i64>,
    /// History row play time, unix seconds.
    #[serde(rename = "viewedAt", deserialize_with = "lenient_int")]
    pub viewed_at: Option<i64>,
    /// Session position in milliseconds.
    #[serde(rename = "viewOffset", deserialize_with = "lenient_int")]
    pub view_offset: Option<i64>,
    /// User rating.
    #[serde(rename = "userRating")]
    pub user_rating: Option<f64>,
    /// Thumbnail path.
    pub thumb: Option<String>,
    /// Playlist composite path.
    pub composite: Option<String>,
    /// Genre tags.
    #[serde(rename = "Genre")]
    pub genre: Vec<Tag>,
    /// External ids (`mbid://...`).
    #[serde(rename = "Guid")]
    pub guid: Vec<Guid>,
    /// Media versions.
    #[serde(rename = "Media")]
    pub media: Vec<Media>,
    /// Session listener.
    #[serde(rename = "User")]
    pub user: Option<Titled>,
    /// Session player.
    #[serde(rename = "Player")]
    pub player: Option<Player>,
    /// Session.
    #[serde(rename = "Session")]
    pub session: Option<SessionRef>,
}

impl Metadata {
    /// The rating key a view needs. A row without one breaks the contract.
    pub fn rating_key(&self) -> Result<&str, AdapterError> {
        self.rating_key
            .as_deref()
            .filter(|key| !key.is_empty())
            .ok_or_else(|| AdapterError::Api("Plex returned a row without a ratingKey".to_owned()))
    }

    /// The first `mbid://` guid.
    pub fn mbid(&self) -> Option<String> {
        self.guid
            .iter()
            .filter_map(|guid| guid.id.as_deref())
            .find_map(|id| id.strip_prefix("mbid://").map(str::to_owned))
    }

    /// True when the row has a thumbnail.
    pub fn has_thumb(&self) -> bool {
        self.thumb.as_deref().is_some_and(|thumb| !thumb.is_empty())
    }

    /// The first media part key: the stream gateway's Plex key.
    pub fn part_key(&self) -> Option<String> {
        self.media
            .first()?
            .part
            .first()?
            .key
            .clone()
            .filter(|key| !key.is_empty())
    }
}

/// A `{tag}` row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Tag {
    /// Tag text.
    pub tag: Option<String>,
}

/// A `{id}` guid row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Guid {
    /// Guid, e.g. `mbid://...`.
    pub id: Option<String>,
}

/// One media version.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Media {
    /// Parts.
    #[serde(rename = "Part")]
    pub part: Vec<Part>,
}

/// One media part.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Part {
    /// Part key (`/library/parts/...`).
    pub key: Option<String>,
}

/// A `{title}` row.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Titled {
    /// Title.
    pub title: Option<String>,
}

/// A session player.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Player {
    /// Player name.
    pub title: Option<String>,
    /// `playing`, `paused`, or `buffering`.
    pub state: Option<String>,
}

/// A session reference.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SessionRef {
    /// Session id.
    #[serde(deserialize_with = "lenient_string")]
    pub id: Option<String>,
}

/// A number or a numeric string as an integer; anything else is absent.
fn lenient_int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        Number(i64),
        Float(f64),
        Text(String),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Loose>::deserialize(deserializer)? {
        Some(Loose::Number(number)) => Some(number),
        Some(Loose::Float(number)) if number.is_finite() => Some(number as i64),
        Some(Loose::Text(text)) => text.trim().parse().ok(),
        _ => None,
    })
}

/// A boolean, `0`/`1`, or `"0"`/`"1"`; anything else is absent.
fn lenient_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        Bool(bool),
        Number(i64),
        Text(String),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Loose>::deserialize(deserializer)? {
        Some(Loose::Bool(value)) => Some(value),
        Some(Loose::Number(number)) => Some(number != 0),
        Some(Loose::Text(text)) => match text.trim() {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

/// A string or a number as a string.
fn lenient_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        Text(String),
        Number(i64),
        Other(serde::de::IgnoredAny),
    }
    Ok(match Option::<Loose>::deserialize(deserializer)? {
        Some(Loose::Text(text)) => Some(text),
        Some(Loose::Number(number)) => Some(number.to_string()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plex mixes string and number spellings; both decode, and a row
    /// with no rating key cannot become a view.
    #[test]
    fn counts_decode_either_way_and_views_need_a_rating_key() {
        let envelope: Envelope = serde_json::from_str(
            r#"{"MediaContainer":{"totalSize":"3","Metadata":[
                {"ratingKey":42,"leafCount":"7","year":2024},
                {"title":"Ghost"}
            ]}}"#,
        )
        .expect("decodes");
        let rows = envelope.container.metadata;
        assert_eq!(envelope.container.total_size, Some(3));
        assert_eq!(rows[0].rating_key(), Ok("42"));
        assert_eq!(rows[0].leaf_count, Some(7));
        assert!(rows[1].rating_key().is_err());
    }
}
