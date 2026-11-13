//! Listening and discovery preferences: release-type filters, the primary
//! music source, the ListenBrainz reads switch and lyrics.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::Section;
use crate::runtime_config::Live;

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

// --- listening source -----------------------------------------------------

/// A listening-data service: the one the discover shelves, charts, radio and
/// enrichment answers name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ListenSource {
    /// ListenBrainz.
    #[default]
    Listenbrainz,
    /// Last.fm.
    Lastfm,
}

crate::spelled!(ListenSource {
    Listenbrainz => "listenbrainz",
    Lastfm => "lastfm",
});

// --- listenbrainz_settings ------------------------------------------------
// The switch for every ListenBrainz read the app makes (recommendations,
// popularity, search counts, genres, Discover, the Weekly Mix and Wrapped),
// on by default: a missing section or a stored `{}` reads on. v2 and earlier
// v3 builds stored `enabled`, `username` and `user_token` here for an
// instance account nothing used; those fields are ignored, so an old stored
// `"enabled": false` does not turn reads off. Each person's own scrobbling
// goes through their own link and is not affected.

/// ListenBrainz settings: the switch for the app's ListenBrainz reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ListenBrainzSettings {
    /// Read recommendations, popularity and genres from ListenBrainz.
    pub reads_enabled: bool,
}

impl Default for ListenBrainzSettings {
    fn default() -> Self {
        Self {
            reads_enabled: true,
        }
    }
}

impl Section for ListenBrainzSettings {
    const KEY: &'static str = "listenbrainz_settings";
}

/// Whether the app may read ListenBrainz now. A section that does not read
/// is off, never its default: the reads wait until it is saved again.
pub fn listenbrainz_reads_on(settings: &Live<ListenBrainzSettings>) -> bool {
    settings
        .read()
        .is_some_and(|settings| settings.reads_enabled)
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
