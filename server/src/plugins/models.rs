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
    /// Declared capabilities the server sends calls for while the plugin is
    /// enabled (those active for its `api_version`).
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
    /// What the plugin asks to do, in plain words.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// The running process, while enabled.
    #[serde(default)]
    pub runtime: Option<PluginRuntimeInfo>,
    /// Where the plugin was installed from, when it came from GitHub.
    #[serde(default)]
    pub install: Option<PluginInstallInfo>,
}

/// Health of one enabled plugin's process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginRuntimeInfo {
    /// `starting`, `running`, `restarting`, `stopped` or `failed`.
    pub state: super::runtime::RuntimeState,
    /// Restarts since it was enabled.
    pub restarts: u64,
    /// Last start failure, crash or protocol problem.
    #[serde(default)]
    pub last_error: Option<String>,
    /// Capabilities the plugin said it implements.
    #[serde(default)]
    pub implemented_capabilities: Vec<String>,
    /// Events skipped because the plugin was still busy with the last one.
    #[serde(default)]
    pub dropped_events: u64,
}

/// The pinned GitHub source of one installed plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginInstallInfo {
    /// The URL the admin gave.
    pub repository_url: String,
    /// `owner/repo`.
    pub repository: String,
    /// `release`, `tag`, `branch` or `commit`.
    pub ref_kind: String,
    /// The release tag, tag, branch or commit asked for.
    pub reference: String,
    /// The exact commit installed.
    pub commit: String,
    /// Unix seconds.
    pub installed_at: i64,
}

/// What an install would bring in, shown before anything is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginInstallPreview {
    /// Manifest name.
    pub name: String,
    /// Display name.
    pub display_name: String,
    /// Plugin version.
    pub version: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Author string.
    #[serde(default)]
    pub author: String,
    /// Homepage URL.
    #[serde(default)]
    pub homepage: String,
    /// Declared capabilities.
    pub capabilities: Vec<String>,
    /// What the plugin asks to do, in plain words.
    pub permissions: Vec<String>,
    /// `owner/repo`.
    pub repository: String,
    /// `release`, `tag`, `branch` or `commit`.
    pub ref_kind: String,
    /// The release tag, tag, branch or commit.
    pub reference: String,
    /// The exact commit that will be installed. Send it back to install
    /// exactly this code.
    pub commit: String,
    /// Version already installed under the same name, when any.
    #[serde(default)]
    pub installed_version: Option<String>,
    /// The plain trust warning to show the admin.
    pub warning: String,
}

/// Update an installed plugin from its GitHub source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginSourceUpdateRequest {
    /// A release tag, tag, branch or commit to move to. Without one, a
    /// plugin installed from releases moves to the latest release, one
    /// installed from a branch moves to the branch tip, and one pinned to a
    /// tag or commit stays.
    #[serde(default)]
    pub reference: Option<String>,
    /// The exact commit the admin expects (from a preview). When set and
    /// the source now resolves elsewhere, the update stops with 409.
    #[serde(default)]
    pub commit: Option<String>,
}

/// Outcome of an update from GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginSourceUpdateResponse {
    /// False when the plugin was already at that commit.
    pub updated: bool,
    /// The plugin after the update.
    pub plugin: PluginInfo,
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

/// Install request: one public GitHub repository URL, optionally pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PluginInstallRequest {
    /// Repository URL, e.g. `https://github.com/owner/repo` (latest
    /// release), `.../releases/tag/v1.0.0`, `.../tree/<ref>` or
    /// `.../commit/<sha>`.
    pub repository_url: String,
    /// A release tag, tag, branch or commit; overrides any in the URL.
    #[serde(default)]
    pub reference: Option<String>,
    /// The exact commit from a preview. When set, exactly that commit
    /// installs, whatever the ref points at by now.
    #[serde(default)]
    pub commit: Option<String>,
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
    /// `ok` (running), `degraded` (restarting), `error` (cannot start) or
    /// `unknown` (starting or stopped).
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
