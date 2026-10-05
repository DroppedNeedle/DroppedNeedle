//! Wire shapes for the plugin and scrobble-settings routes.
//!
//! Secret-marked plugin settings follow the house mask-sentinel rule:
//! reads return the mask when a value exists, and a save that sends the
//! mask back keeps the stored value. ListenBrainz links never carry the
//! token: the status shape holds the username only.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One settings field a plugin declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginSettingFieldInfo {
    /// Setting key.
    pub key: String,
    /// Display label.
    pub label: String,
    /// Help text.
    #[serde(default)]
    pub help: String,
    /// Whether the value is secret (masked on read, encrypted at rest).
    #[serde(default)]
    pub secret: bool,
}

/// One plugin as the admin UI sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginInfo {
    /// Manifest name.
    pub name: String,
    /// Display name.
    pub display_name: String,
    /// Plugin version.
    pub version: String,
    /// Whether the plugin is enabled and loaded.
    pub enabled: bool,
    /// Declared capability ids.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Declared capabilities the module actually implements.
    #[serde(default)]
    pub active_capabilities: Vec<String>,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Author string.
    #[serde(default)]
    pub author: String,
    /// Homepage URL.
    #[serde(default)]
    pub homepage: String,
    /// Load failure, when present.
    #[serde(default)]
    pub error: Option<String>,
    /// Declared settings fields.
    #[serde(default)]
    pub settings_fields: Vec<PluginSettingFieldInfo>,
    /// Current settings values (secrets masked).
    #[serde(default)]
    pub settings_values: HashMap<String, String>,
    /// Panel bundle path, when the plugin ships UI.
    #[serde(default)]
    pub ui_entry: String,
    /// Panel page ids.
    #[serde(default)]
    pub ui_pages: Vec<String>,
    /// External panel URL.
    #[serde(default)]
    pub ui_external_url: String,
    /// Declared download-client sources.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Declared indexer targets.
    #[serde(default)]
    pub targets: Vec<String>,
}

/// Plugin listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginListResponse {
    /// Every discovered plugin.
    pub plugins: Vec<PluginInfo>,
}

/// Enable switch plus settings save. A secret field sent back as the mask
/// keeps its stored value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginUpdateRequest {
    /// New enable switch.
    pub enabled: bool,
    /// New settings values.
    #[serde(default)]
    pub settings: HashMap<String, String>,
}

/// Install request: one public GitHub repository URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginInstallRequest {
    /// Repository URL, e.g. `https://github.com/owner/repo`.
    pub repository_url: String,
}

/// One enabled plugin acquisition source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginSource {
    /// Source key (`plugin:<name>` or `usenet`).
    pub key: String,
    /// Owning plugin name.
    #[serde(default)]
    pub plugin: String,
    /// Display name.
    #[serde(default)]
    pub display_name: String,
    /// Whether the plugin serves downloads for this source.
    #[serde(default)]
    pub has_client: bool,
    /// Whether the plugin serves searches for this source.
    #[serde(default)]
    pub has_indexer: bool,
    /// Indexer target source.
    #[serde(default)]
    pub target_source: String,
    /// Whether the source is ready to use.
    #[serde(default)]
    pub configured: bool,
    /// `ok`, `degraded`, or `unknown`.
    #[serde(default)]
    pub health: String,
}

/// Plugin source listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginSourcesResponse {
    /// Enabled acquisition sources.
    pub sources: Vec<PluginSource>,
}

/// One linked external account. Never carries the secret, only the name.
/// The schema rename keeps this distinct from the remotes
/// `ConnectionStatus` in the one OpenAPI document; the wire shape is
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = PluginConnectionStatus)]
pub struct ConnectionStatus {
    /// Service name (`listenbrainz` here).
    pub service: String,
    /// Whether the link is active.
    #[serde(default)]
    pub enabled: bool,
    /// Linked username.
    #[serde(default)]
    pub username: String,
}

/// Per-user scrobble preferences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScrobblePreferences {
    /// Forward plays to Last.fm.
    #[serde(default)]
    pub scrobble_to_lastfm: bool,
    /// Forward plays to ListenBrainz.
    #[serde(default)]
    pub scrobble_to_listenbrainz: bool,
    /// Navidrome owns external forwarding for Navidrome plays.
    #[serde(default = "default_navidrome_delegation")]
    pub navidrome_handles_external_scrobbles: bool,
    /// `listenbrainz` or `lastfm`.
    #[serde(default = "default_primary_source")]
    pub primary_music_source: String,
    /// `full`, `track_hidden`, or `offline`.
    #[serde(default = "default_visibility")]
    pub now_playing_visibility: String,
    /// Standing intent to auto-request the personal mix.
    #[serde(default)]
    pub auto_request_personal_mix: bool,
    /// Standing-grant state for auto-request.
    #[serde(default = "default_mix_state")]
    pub auto_request_state: String,
}

fn default_navidrome_delegation() -> bool {
    true
}

fn default_primary_source() -> String {
    "listenbrainz".to_owned()
}

fn default_visibility() -> String {
    "full".to_owned()
}

fn default_mix_state() -> String {
    "none".to_owned()
}

/// Partial prefs update: absent fields keep their stored values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(default)]
pub struct ScrobblePreferencesUpdate {
    /// New Last.fm forwarding switch.
    pub scrobble_to_lastfm: Option<bool>,
    /// New ListenBrainz forwarding switch.
    pub scrobble_to_listenbrainz: Option<bool>,
    /// New Navidrome delegation switch.
    pub navidrome_handles_external_scrobbles: Option<bool>,
    /// New primary source.
    pub primary_music_source: Option<String>,
    /// New presence visibility.
    pub now_playing_visibility: Option<String>,
    /// New personal-mix auto-request intent.
    pub auto_request_personal_mix: Option<bool>,
}

/// ListenBrainz connect request. The username is required: it drives every
/// per-user read, and without it a "connected" account yields silently
/// empty discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListenBrainzConnectRequest {
    /// User token.
    pub user_token: String,
    /// ListenBrainz username.
    #[serde(default)]
    pub username: String,
}

/// Plain status answer for uninstalls and disconnects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StatusMessage {
    /// `ok`.
    pub status: String,
    /// Human detail.
    pub message: String,
}
