//! Listening and discovery preferences: release-type filters, scrobble
//! targets, the primary music source, the Last.fm switch, and lyrics.

use serde::{Deserialize, Serialize};

use super::Section;

// --- user_preferences -----------------------------------------------------

/// Release-type filters for discovery and search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

// --- scrobble_settings / primary_music_source -----------------------------

/// Scrobble targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PrimaryMusicSource {
    /// Active source.
    pub source: MusicSource,
}

impl Section for PrimaryMusicSource {
    const KEY: &'static str = "primary_music_source";
}

// --- lastfm_settings (per-user credentials only) --------------------------
// The admin-global credential pair is deleted; the section keeps only the
// master switch. The v2 importer decrypts sealed lastfm secrets, then
// drops them, and the per-user store behind LASTFM_SECRET_MASK holds the
// credentials.

/// Last.fm settings: master switch only (per-user credentials live in
/// the per-user store, not here).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LastFmSettings {
    /// Master switch for Last.fm fan-out.
    pub enabled: bool,
}

impl Section for LastFmSettings {
    const KEY: &'static str = "lastfm_settings";
}

// --- lyrics_settings (read-path lyrics provider) ---------------------------
// The master switch for live LRCLIB lyrics on the library read path. This is
// the read-path provider toggle, not the library-management write block
// (`LyricsManagementSettings`): with this off, lyrics reads stay on the
// empty memory port and the server makes no lyrics network calls.

/// Lyrics settings: master switch for the live LRCLIB read path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LyricsSettings {
    /// Master switch for live LRCLIB lyrics fan-out.
    pub enabled: bool,
}

impl Section for LyricsSettings {
    const KEY: &'static str = "lyrics_settings";
}
