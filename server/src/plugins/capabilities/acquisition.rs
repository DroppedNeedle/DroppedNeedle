//! `indexer` and `download_client`: plugins as acquisition sources.
//!
//! One plugin, one source key: `plugin:<manifest-name>`. A plugin that
//! declares both capabilities is a complete source. An indexer-only plugin
//! feeds the source its `target_source` names: another plugin's key, or
//! `usenet` to add releases (with an NZB URL) to the SABnzbd pipeline.
//!
//! This module only speaks to plugins: typed calls, time budgets, and
//! "absence, not failure" fallbacks (a failing search is no results, a
//! failing health check is an error status). Ranking and the download
//! worker live in `acquire`, which applies the same quality, size, term
//! and quarantine rules to plugin results as to every other source.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::host::{LoadedPlugin, PluginHost};
use super::super::protocol::methods;
use super::super::runtime::CallError;
use super::{call, decode};

/// Search budget the plugin is told about.
const SEARCH_BUDGET: Duration = Duration::from_secs(30);
/// Extra time the host waits past the search budget.
const SEARCH_GRACE: Duration = Duration::from_secs(5);
/// Budget for starting a download (some clients fetch files here).
const ENQUEUE_TIMEOUT: Duration = Duration::from_secs(120);
/// Budget for status, inspect, discard and abort.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
/// Budget for a health check.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(10);
/// Results kept per plugin search.
const RESULTS_PER_SEARCH: usize = 200;

/// One exact file in a files-mode result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginFileRef {
    /// Peer or account name, when the source has one.
    pub username: String,
    /// File name as the source knows it.
    pub filename: String,
    /// Size in bytes, 0 when unknown.
    pub size: i64,
}

/// One search result from a plugin indexer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSearchResult {
    /// Release title.
    pub title: String,
    /// Total size in bytes.
    pub size_bytes: i64,
    /// The plugin's own confidence, 0 to 1 (clamped by the host).
    pub score: f64,
    /// `lossless`, `mp3_320`, `mp3_256`, `mp3_192`, `low`, or empty.
    pub quality_tier: String,
    /// Exact files (files mode); empty means the client fetches a folder.
    pub files: Vec<PluginFileRef>,
    /// Opaque token handed back to the client at enqueue.
    pub payload: String,
    /// NZB URL, for indexers that feed `usenet`.
    pub nzb_url: String,
    /// Usenet post date (unix seconds), for indexers that feed `usenet`.
    pub usenet_date: Option<f64>,
}

/// Health of one plugin source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginHealth {
    /// `ok` or `error`.
    pub status: String,
    /// Detail for the admin.
    #[serde(default)]
    pub message: String,
    /// Whether the plugin has the settings it needs.
    #[serde(default = "default_true")]
    pub configured: bool,
}

fn default_true() -> bool {
    true
}

/// Correlation handle for one plugin download.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginTaskHandle {
    /// Source key, forced by the host.
    pub source: String,
    /// Peer or account name.
    pub username: String,
    /// Files the client fetches.
    pub filenames: Vec<String>,
    /// Client job name.
    pub job_name: String,
    /// Client-side id, when it has one.
    pub nzo_id: String,
    /// The payload from the search result, handed back on every call.
    pub plugin_token: String,
}

/// Progress of one plugin download.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginTaskStatus {
    /// `queued`, `downloading`, `completed` or `failed`.
    pub status: String,
    /// Files in the job.
    pub files_total: i64,
    /// Files finished.
    pub files_completed: i64,
    /// Bytes downloaded so far.
    pub bytes_downloaded: i64,
    /// Failure detail.
    pub error: Option<String>,
    /// Files that finished successfully.
    pub succeeded_filenames: Vec<String>,
    /// Bytes are moving right now.
    pub has_active_transfer: bool,
    /// Position in a remote queue.
    pub queue_position_start: Option<i64>,
    /// Latest position in a remote queue.
    pub queue_position_end: Option<i64>,
}

/// Files a finished plugin download produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginMaterialization {
    /// `active`, `completed`, `failed` or `missing`.
    pub state: String,
    /// Folder holding the files.
    pub workspace_path: String,
    /// Exact file paths.
    pub file_paths: Vec<String>,
    /// The client's storage is reachable.
    pub mount_healthy: Option<bool>,
}

/// What the worker asks a plugin client to download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PluginEnqueue {
    /// Download task id.
    pub task_id: String,
    /// Source key.
    pub source: String,
    /// Exact files for files mode; empty for folder mode.
    pub files: Vec<PluginFileRef>,
    /// The search result's payload.
    pub payload: String,
    /// Job name the worker picked (stable per task and attempt).
    pub job_name: String,
    /// `album` or `track`.
    pub download_type: String,
}

/// The source key a plugin's indexer results belong to.
pub fn indexer_target(plugin: &LoadedPlugin) -> String {
    plugin
        .manifest
        .capability_configs
        .iter()
        .find(|config| config.id == "indexer" && !config.target_source.is_empty())
        .map(|config| config.target_source.clone())
        .unwrap_or_else(|| format!("plugin:{}", plugin.manifest.name))
}

impl PluginHost {
    /// The enabled plugin whose `download_client` serves `source_key`.
    pub fn download_client(&self, source_key: &str) -> Option<LoadedPlugin> {
        let name = source_key.strip_prefix("plugin:")?;
        self.get(name)
            .filter(|plugin| plugin.serves("download_client"))
    }

    /// Enabled plugin source keys with a download client, sorted.
    pub fn download_source_keys(&self) -> Vec<String> {
        self.serving("download_client")
            .iter()
            .map(|plugin| format!("plugin:{}", plugin.manifest.name))
            .collect()
    }

    /// Enabled indexers feeding one source key, in name order.
    pub fn indexers_for(&self, source_key: &str) -> Vec<LoadedPlugin> {
        self.serving("indexer")
            .into_iter()
            .filter(|plugin| indexer_target(plugin) == source_key)
            .collect()
    }

    /// Album search across every indexer feeding `source_key`. A failing
    /// indexer drops only its own results.
    pub async fn search_album(
        &self,
        source_key: &str,
        artist_name: &str,
        album_title: &str,
        year: Option<i64>,
        track_count: Option<i64>,
    ) -> Vec<PluginSearchResult> {
        let params = json!({
            "artist_name": artist_name,
            "album_title": album_title,
            "year": year,
            "track_count": track_count,
            "timeout": SEARCH_BUDGET.as_secs_f64(),
        });
        self.search(source_key, methods::SEARCH_ALBUM, params).await
    }

    /// Track search across every indexer feeding `source_key`.
    pub async fn search_track(
        &self,
        source_key: &str,
        artist_name: &str,
        track_title: &str,
        album_title: Option<&str>,
    ) -> Vec<PluginSearchResult> {
        let params = json!({
            "artist_name": artist_name,
            "track_title": track_title,
            "album_title": album_title,
            "duration_seconds": Value::Null,
            "timeout": SEARCH_BUDGET.as_secs_f64(),
        });
        self.search(source_key, methods::SEARCH_TRACK, params).await
    }

    async fn search(
        &self,
        source_key: &str,
        method: &str,
        params: Value,
    ) -> Vec<PluginSearchResult> {
        let indexers = self.indexers_for(source_key);
        let answers = futures_util::future::join_all(indexers.iter().map(|plugin| {
            let params = params.clone();
            async move {
                let value = call(plugin, method, params, SEARCH_BUDGET + SEARCH_GRACE)
                    .await
                    .ok()?;
                decode::<Vec<PluginSearchResult>>(plugin, method, value).ok()
            }
        }))
        .await;
        answers
            .into_iter()
            .flatten()
            .flat_map(|results| results.into_iter().take(RESULTS_PER_SEARCH))
            .collect()
    }

    /// Health of one plugin (any capability). Errors read as an error
    /// status, never a failure.
    pub async fn plugin_health(&self, plugin: &LoadedPlugin) -> PluginHealth {
        match call(plugin, methods::HEALTH, Value::Null, HEALTH_TIMEOUT).await {
            Ok(value) => {
                decode::<PluginHealth>(plugin, methods::HEALTH, value).unwrap_or(PluginHealth {
                    status: "error".to_owned(),
                    message: "health answer was malformed".to_owned(),
                    configured: false,
                })
            }
            Err(CallError::Unsupported(_)) => PluginHealth {
                status: "ok".to_owned(),
                message: String::new(),
                configured: true,
            },
            Err(error) => PluginHealth {
                status: "error".to_owned(),
                message: error.to_string(),
                configured: false,
            },
        }
    }

    /// Start one download on the client serving `request.source`.
    pub async fn enqueue_download(
        &self,
        request: &PluginEnqueue,
    ) -> Result<PluginTaskHandle, CallError> {
        let plugin = self
            .download_client(&request.source)
            .ok_or_else(|| CallError::NotRunning(format!("{} is not enabled", request.source)))?;
        let params = serde_json::to_value(request)
            .map_err(|error| CallError::Malformed(error.to_string()))?;
        let value = call(&plugin, methods::ENQUEUE, params, ENQUEUE_TIMEOUT).await?;
        let mut handle: PluginTaskHandle = decode(&plugin, methods::ENQUEUE, value)?;
        // The source key is the host's, whatever the plugin says; the token
        // is the search payload unless the plugin chose its own.
        handle.source = request.source.clone();
        if handle.plugin_token.is_empty() {
            handle.plugin_token = request.payload.clone();
        }
        if handle.job_name.is_empty() {
            handle.job_name = request.job_name.clone();
        }
        Ok(handle)
    }

    /// Progress of one download.
    pub async fn download_status(
        &self,
        handle: &PluginTaskHandle,
    ) -> Result<PluginTaskStatus, CallError> {
        let (plugin, value) = self.client_call(handle, methods::STATUS).await?;
        decode(&plugin, methods::STATUS, value)
    }

    /// Files one download produced.
    pub async fn download_inspect(
        &self,
        handle: &PluginTaskHandle,
    ) -> Result<PluginMaterialization, CallError> {
        let (plugin, value) = self.client_call(handle, methods::INSPECT).await?;
        decode(&plugin, methods::INSPECT, value)
    }

    /// Forget one finished download on the client side.
    pub async fn download_discard(&self, handle: &PluginTaskHandle) -> Result<bool, CallError> {
        let (_, value) = self.client_call(handle, methods::DISCARD).await?;
        Ok(value.as_bool().unwrap_or(false))
    }

    /// Stop one running download.
    pub async fn download_abort(&self, handle: &PluginTaskHandle) -> Result<bool, CallError> {
        let (_, value) = self.client_call(handle, methods::ABORT).await?;
        Ok(value.as_bool().unwrap_or(false))
    }

    async fn client_call(
        &self,
        handle: &PluginTaskHandle,
        method: &str,
    ) -> Result<(LoadedPlugin, Value), CallError> {
        let plugin = self
            .download_client(&handle.source)
            .ok_or_else(|| CallError::NotRunning(format!("{} is not enabled", handle.source)))?;
        let value = call(&plugin, method, json!({ "handle": handle }), CLIENT_TIMEOUT).await?;
        Ok((plugin, value))
    }
}
