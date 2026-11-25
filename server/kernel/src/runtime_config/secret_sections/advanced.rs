//! Advanced tuning: Discover, AudioDB, images and retention.

use super::*;

// --- advanced_settings (closed export allowlist) ---------------------------
// Kept: the Discover tuning, the AudioDB switches and key,
// image serving, and the retention and prune cadences. Cache lifetimes,
// precache pacing, HTTP limits and the browser cache lifetimes are fixed
// in code; a v2 export that carries them has them reported as dropped.
// The AudioDB key is encrypted at rest now (v2 stored it plaintext).

/// Advanced tuning: Discover, AudioDB, images and retention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct AdvancedSettings {
    /// MusicBrainz lookups one Last.fm album batch may spend.
    pub discover_queue_lastfm_mbid_max_lookups: i64,
    /// Whether Discover blends Last.fm and ListenBrainz for a user who
    /// links both, instead of following only their primary service.
    pub discover_blend_listening_services: bool,
    /// AudioDB master switch.
    pub audiodb_enabled: bool,
    /// AudioDB name-search fallback.
    pub audiodb_name_search_fallback: bool,
    /// Direct remote images.
    pub direct_remote_images_enabled: bool,
    /// Prefer local cover art.
    pub prefer_local_cover_art: bool,
    /// AudioDB key (encrypted at rest; v2 stored it plaintext). A missing
    /// field reads as empty (then the read path falls back to the "123"
    /// default); only an explicitly stored value decrypts.
    #[serde(default)]
    #[schema(value_type = String)]
    pub audiodb_api_key: Secret,
    /// Request-history retention days.
    pub request_history_retention_days: i64,
    /// Ignored-releases retention days.
    pub ignored_releases_retention_days: i64,
    /// Store-prune cadence hours.
    pub store_prune_interval_hours: i64,
}

impl Default for AdvancedSettings {
    fn default() -> Self {
        Self {
            discover_queue_lastfm_mbid_max_lookups: 10,
            discover_blend_listening_services: true,
            audiodb_enabled: true,
            audiodb_name_search_fallback: false,
            direct_remote_images_enabled: true,
            prefer_local_cover_art: true,
            audiodb_api_key: Secret::default(),
            request_history_retention_days: 180,
            ignored_releases_retention_days: 365,
            store_prune_interval_hours: 6,
        }
    }
}

impl Section for AdvancedSettings {
    const KEY: &'static str = "advanced_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        let key = Self::KEY;
        let ints: &[(&str, i64, i64, i64)] = &[
            (
                "discover_queue_lastfm_mbid_max_lookups",
                self.discover_queue_lastfm_mbid_max_lookups,
                1,
                50,
            ),
            (
                "request_history_retention_days",
                self.request_history_retention_days,
                30,
                3650,
            ),
            (
                "ignored_releases_retention_days",
                self.ignored_releases_retention_days,
                30,
                3650,
            ),
            (
                "store_prune_interval_hours",
                self.store_prune_interval_hours,
                1,
                168,
            ),
        ];
        for (field, value, min, max) in ints {
            check_range(key, field, *value, *min, *max)?;
        }
        Ok(())
    }

    fn normalize(&mut self) {
        if self.audiodb_api_key.expose().trim().is_empty() {
            self.audiodb_api_key = Secret::new("123");
        }
    }
}

impl SecretSection for AdvancedSettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.audiodb_api_key,
            strip: false,
        }]
    }
}
