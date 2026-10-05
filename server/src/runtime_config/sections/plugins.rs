//! Plugin state and the internal bookkeeping section.

use serde::{Deserialize, Serialize};

use super::Section;

// --- plugins --------------------------------------------------------------
// Secret-flagged values are encrypted at rest (v2 stored them plaintext).
// Which keys are secret comes from each plugin manifest at runtime, so the
// section itself is a plain map and `ConfigStore` offers secret-aware
// plugin helpers that take the manifest's secret-key set.

/// One plugin's persisted state. Disabled by default: dropping a folder
/// in must never run code.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PluginConfig {
    /// Enabled switch.
    pub enabled: bool,
    /// Settings map; secret-flagged values are ciphertext at rest.
    pub settings: std::collections::HashMap<String, String>,
}

impl std::fmt::Debug for PluginConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The map can hold decrypted secrets in the get_plugin_raw form,
        // and this struct cannot tell secret keys from plain ones without
        // the plugin manifest, so every value redacts; only keys print.
        let redacted: std::collections::BTreeMap<&str, &str> = self
            .settings
            .keys()
            .map(|key| (key.as_str(), "[redacted]"))
            .collect();
        f.debug_struct("PluginConfig")
            .field("enabled", &self.enabled)
            .field("settings", &redacted)
            .finish()
    }
}

/// The `plugins` section: per-plugin state by plugin name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Plugins(pub std::collections::HashMap<String, PluginConfig>);

impl Section for Plugins {
    const KEY: &'static str = "plugins";
}

// --- _internal ------------------------------------------------------------
// Kept: plex_client_id, droppedneedle_device_id,
// brainzmash_consent_admin. Dropped as recomputed/derived: audiodb sweep
// cursor + completion, release_type_policy_revision,
// musicbrainz_settings_revision, official_source_selected.

/// Typed subset of the `_internal` section. Only the kept keys exist;
/// anything else in the file is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InternalState {
    /// Stable Plex client id.
    pub plex_client_id: Option<String>,
    /// Stable Jellyfin device id.
    pub droppedneedle_device_id: Option<String>,
    /// Admin id that consented to BrainzMash.
    pub brainzmash_consent_admin: Option<String>,
}

impl Section for InternalState {
    const KEY: &'static str = "_internal";
}
