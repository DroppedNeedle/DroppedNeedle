//! Listening and discovery preferences: release-type filters, scrobble
//! targets, the primary music source, home page rows, the Last.fm switch,
//! and lyrics.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Section, check_range};
use crate::runtime_config::error::ConfigError;

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

// --- home_settings ----------------------------------------------------------

/// Home page cache lifetimes and the two trending rows (v2
/// `HomeSettings`). The ranges are v2's, enforced on save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct HomeSettings {
    /// Trending rows cache lifetime, seconds (300-86400).
    pub cache_ttl_trending: i64,
    /// Personal rows cache lifetime, seconds (60-3600).
    pub cache_ttl_personal: i64,
    /// Show the "What's hot" row.
    pub show_whats_hot: bool,
    /// Show the "Globally trending" row.
    pub show_globally_trending: bool,
}

impl Default for HomeSettings {
    fn default() -> Self {
        Self {
            cache_ttl_trending: 3600,
            cache_ttl_personal: 300,
            show_whats_hot: true,
            show_globally_trending: true,
        }
    }
}

impl Section for HomeSettings {
    const KEY: &'static str = "home_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "cache_ttl_trending",
            self.cache_ttl_trending,
            300,
            86400,
        )?;
        check_range(
            Self::KEY,
            "cache_ttl_personal",
            self.cache_ttl_personal,
            60,
            3600,
        )
    }
}

// --- scrobble_settings / primary_music_source -----------------------------

/// Scrobble targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct ScrobbleSettings {
    /// Scrobble to Last.fm (per-user credentials).
    pub scrobble_to_lastfm: bool,
    /// Scrobble to ListenBrainz.
    pub scrobble_to_listenbrainz: bool,
}

impl Section for ScrobbleSettings {
    const KEY: &'static str = "scrobble_settings";
}

/// Which service backs scrobble-targeted discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MusicSource {
    /// ListenBrainz.
    #[default]
    Listenbrainz,
    /// Last.fm.
    #[serde(rename = "lastfm")]
    Lastfm,
}

/// Primary music source selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct PrimaryMusicSource {
    /// Active source.
    pub source: MusicSource,
}

impl Section for PrimaryMusicSource {
    const KEY: &'static str = "primary_music_source";
}

// --- lyrics_settings (read-path lyrics provider) ---------------------------
// The master switch for live LRCLIB lyrics on the library read path. This is
// the read-path provider toggle, not the library-management write block
// (`LyricsManagementSettings`): with this off, lyrics reads stay on the
// empty memory port and the server makes no lyrics network calls.

/// Lyrics settings: master switch for the live LRCLIB read path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct LyricsSettings {
    /// Master switch for live LRCLIB lyrics fan-out.
    pub enabled: bool,
}

impl Section for LyricsSettings {
    const KEY: &'static str = "lyrics_settings";
}
