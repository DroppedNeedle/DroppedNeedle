//! Listening and discovery preferences: release-type filters, the primary
//! music source and lyrics.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::Section;

// --- user_preferences -----------------------------------------------------

/// Release-type filters for discovery and search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct UserPreferences {
    /// Primary release types (album, ep, single, ...).
    pub primary_types: Vec<String>,
    /// Secondary release types (studio, live, ...).
    pub secondary_types: Vec<String>,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            primary_types: ["album", "ep", "single"]
                .iter()
                .map(ToString::to_string)
                .collect(),
            secondary_types: ["studio"].iter().map(ToString::to_string).collect(),
        }
    }
}

impl Section for UserPreferences {
    const KEY: &'static str = "user_preferences";
}

// --- primary_music_source -------------------------------------------------

/// A listening-data service: the primary source setting, and the one the
/// discover shelves, charts, radio and enrichment answers name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ListenSource {
    /// ListenBrainz.
    #[default]
    Listenbrainz,
    /// Last.fm.
    Lastfm,
}

impl ListenSource {
    /// Wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Listenbrainz => "listenbrainz",
            Self::Lastfm => "lastfm",
        }
    }
}

/// Primary music source selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct PrimaryMusicSource {
    /// Active source.
    pub source: ListenSource,
}

impl Section for PrimaryMusicSource {
    const KEY: &'static str = "primary_music_source";
}

// --- lyrics_settings (read-path lyrics provider) ---------------------------
// The switch for the online LRCLIB lookup on the library read path, on by
// default: a missing section or a stored `{}` reads on, and a stored
// `"enabled": false` stays off. This is the read-path toggle, not the
// library-management write block (`LyricsManagementSettings`): with it off,
// lyrics reads stay on the empty memory port and the server makes no lyrics
// network calls.

/// Lyrics settings: the switch for the online LRCLIB lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct LyricsSettings {
    /// Look up lyrics on LRCLIB when the server has none for a track.
    pub enabled: bool,
}

impl Default for LyricsSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Section for LyricsSettings {
    const KEY: &'static str = "lyrics_settings";
}
