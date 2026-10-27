//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract the caller, call the host or
//! service, render. Status mapping lives in the `From<PluginError>` impl
//! for [`ApiError`]. Bodies parse through the kernel's [`ValidJson`].
//!
//! Blocking host mutations (install, update, uninstall) run on the
//! blocking pool; dispatches and ticks stay on the async runtime.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    body::Body,
    extract::{FromRef, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use droppedneedle_auth::{Admin, Principal, User};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::ConfigStore;

use super::capabilities::routes::ROUTE_BODY_MAX_BYTES;
use super::error::{PluginError, SourceFault};
use super::host::{PluginHost, UninstallError, UpdateError};
use super::install::{
    ArchiveFetcher, InstallError, StagedPlugin, TRUST_WARNING, parse_source, resolve, stage,
};
use super::manifest::{Capability, RouteAuth};
use super::models::{
    PluginInfo, PluginInstallInfo, PluginInstallPreview, PluginInstallRequest, PluginListResponse,
    PluginRefKind, PluginRuntimeInfo, PluginSettingFieldInfo, PluginSourceUpdateRequest,
    PluginSourceUpdateResponse, PluginSourcesResponse, PluginUpdateRequest, StatusMessage,
};
use super::runtime::PluginRouteBody;
use super::ticks::PluginTickLoops;

/// Plugin-route dependencies.
#[derive(Clone)]
pub struct PluginsDeps {
    /// The plugin host.
    pub host: Arc<PluginHost>,
    /// Config store for masked settings reads.
    pub config: Arc<ConfigStore>,
    /// Where installs reach GitHub.
    pub github: super::install::GitHubHosts,
    /// Install archive fetcher.
    pub fetcher: Arc<dyn ArchiveFetcher>,
    /// Tick-loop rebuilds, synced after every mutation.
    pub tick_sync: Arc<PluginTickLoops>,
    /// `/ext/` rate limiter.
    pub ext_limiter: Arc<ExtRateLimiter>,
    /// Wall time, stamped on install records.
    pub clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
}

fn map_install(error: InstallError) -> PluginError {
    match error {
        InstallError::Io(reason) => PluginError::internal(reason),
        InstallError::Denied => PluginError::Source(SourceFault::Refused),
        InstallError::RateLimited { retry_after } => {
            PluginError::Source(SourceFault::RateLimited { retry_after })
        }
        InstallError::Download => PluginError::Source(SourceFault::Unreachable),
        other => PluginError::InvalidInput {
            message: other.to_string(),
        },
    }
}

/// One plugin as the admin UI sees it, secrets masked.
fn plugin_info(
    host: &PluginHost,
    config: &ConfigStore,
    name: &str,
) -> Result<PluginInfo, PluginError> {
    let plugin = host.get(name).ok_or(PluginError::NotFound)?;
    let manifest = &plugin.manifest;
    let stored = config
        .get_plugin_masked(name, &manifest.secret_keys())
        .map_err(|error| PluginError::internal(&error))?;
    let mut values = HashMap::new();
    for field in &manifest.settings {
        values.insert(
            field.key.clone(),
            stored.settings.get(&field.key).cloned().unwrap_or_default(),
        );
    }
    let mut sources = Vec::new();
    let mut targets = Vec::new();
    for cap in &manifest.capability_configs {
        if !cap.source.is_empty() && !sources.contains(&cap.source) {
            sources.push(cap.source.clone());
        }
        if !cap.target_source.is_empty() && !targets.contains(&cap.target_source) {
            targets.push(cap.target_source.clone());
        }
    }
    let runtime = plugin.runtime_status().map(|status| PluginRuntimeInfo {
        state: status.state,
        restarts: status.restarts,
        last_error: status.last_error,
        implemented_capabilities: status.implemented,
        dropped_events: host
            .dropped_event_counts()
            .get(&manifest.name)
            .copied()
            .unwrap_or(0),
        health: plugin.health(),
    });
    let install = host.install_record(name).map(|record| PluginInstallInfo {
        repository_url: record.repository_url,
        repository: record.repository,
        ref_kind: record.ref_kind,
        reference: record.reference,
        commit: record.commit,
        installed_at: record.installed_at,
    });
    Ok(PluginInfo {
        name: manifest.name.clone(),
        display_name: manifest.display_name.clone(),
        version: manifest.version.clone(),
        enabled: plugin.enabled,
        capabilities: manifest.capabilities.clone(),
        active_capabilities: plugin.active_capabilities.clone(),
        description: manifest.description.clone(),
        author: manifest.author.clone(),
        homepage: manifest.homepage.clone(),
        error: plugin.error.clone(),
        settings_fields: manifest
            .settings
            .iter()
            .map(|field| PluginSettingFieldInfo {
                key: field.key.clone(),
                label: field.label.clone(),
                help: field.help.clone(),
                secret: field.secret,
            })
            .collect(),
        settings_values: values,
        ui_entry: manifest.ui_entry.clone(),
        ui_pages: manifest.ui_pages.clone(),
        ui_external_url: manifest.ui_external_url.clone(),
        sources,
        targets,
        permissions: manifest.permissions(),
        runtime,
        install,
    })
}

// ---------------------------------------------------------------------------
// Admin plugin routes
// ---------------------------------------------------------------------------

/// List every discovered plugin, secrets masked.
#[utoipa::path(
    get,
    tag = "plugins",
    path = "/api/v3/plugins",
    responses(
        (status = 200, description = "Plugin listing", body = PluginListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn list_plugins(
    State(deps): State<PluginsDeps>,
    _admin: Admin,
) -> Result<Json<PluginListResponse>, ApiError> {
    let mut plugins = Vec::new();
    for plugin in deps.host.list_plugins() {
        plugins.push(plugin_info(
            deps.host.as_ref(),
            deps.config.as_ref(),
            &plugin.manifest.name,
        )?);
    }
    Ok(Json(PluginListResponse { plugins }))
}

/// Resolve and download one install request without installing it.
async fn stage_request(
    deps: &PluginsDeps,
    url: &str,
    reference: Option<&str>,
) -> Result<StagedPlugin, PluginError> {
    let source = parse_source(url, reference).map_err(map_install)?;
    let resolved = resolve(deps.fetcher.as_ref(), &deps.github, source)
        .await
        .map_err(map_install)?;
    stage(deps.fetcher.as_ref(), &deps.github, resolved)
        .await
        .map_err(map_install)
}

/// Write a staged plugin to disk off the async workers, then re-sync ticks.
async fn install_staged(deps: &PluginsDeps, staged: StagedPlugin) -> Result<String, PluginError> {
    let host = Arc::clone(&deps.host);
    let installed_at = deps.clock.now_unix();
    let name = tokio::task::spawn_blocking(move || host.install_staged(&staged, installed_at))
        .await
        .map_err(|error| PluginError::internal(&error))?
        .map_err(map_install)?;
    deps.tick_sync.sync_host(&deps.host).await;
    Ok(name)
}

/// Show what installing a repository would bring in: the manifest, the
/// permissions it asks for, the exact commit, and the trust warning.
/// Downloads and validates; writes nothing.
#[utoipa::path(
    post,
    tag = "plugins",
    path = "/api/v3/plugins/install/preview",
    request_body = PluginInstallRequest,
    responses(
        (status = 200, description = "What would be installed", body = PluginInstallPreview),
        (status = 400, description = "Not a usable plugin repository"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 502, description = "GitHub refused the request or could not be reached"),
        (status = 503, description = "GitHub is rate limiting this server"),
    )
)]
pub async fn preview_plugin_install(
    State(deps): State<PluginsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<PluginInstallRequest>,
) -> Result<Json<PluginInstallPreview>, ApiError> {
    let staged = stage_request(&deps, &body.repository_url, body.reference.as_deref()).await?;
    let manifest = &staged.manifest;
    let installed_version = deps
        .host
        .get(&manifest.name)
        .filter(|plugin| plugin.error.is_none() || !plugin.manifest.version.is_empty())
        .map(|plugin| plugin.manifest.version);
    Ok(Json(PluginInstallPreview {
        name: manifest.name.clone(),
        display_name: manifest.display_name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        author: manifest.author.clone(),
        homepage: manifest.homepage.clone(),
        capabilities: manifest.capabilities.clone(),
        permissions: manifest.permissions(),
        repository: format!(
            "{}/{}",
            staged.resolved.source.owner, staged.resolved.source.repo
        ),
        ref_kind: staged.resolved.ref_kind,
        reference: staged.resolved.reference.clone(),
        commit: staged.resolved.commit.clone(),
        installed_version,
        warning: TRUST_WARNING.to_owned(),
    }))
}

/// Install a plugin from a public GitHub repository, pinned to an exact
/// commit. The code is stored, never run: the plugin arrives disabled and
/// an admin must enable it. Reinstalling over an installed plugin replaces
/// its code and keeps its settings.
#[utoipa::path(
    post,
    tag = "plugins",
    path = "/api/v3/plugins/install",
    request_body = PluginInstallRequest,
    responses(
        (status = 201, description = "Installed plugin", body = PluginInfo),
        (status = 400, description = "Not a usable plugin repository"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 409, description = "The repository moved since the preview"),
        (status = 502, description = "GitHub refused the request or could not be reached"),
        (status = 503, description = "GitHub is rate limiting this server"),
    )
)]
pub async fn install_plugin(
    State(deps): State<PluginsDeps>,
    _admin: Admin,
    ValidJson(body): ValidJson<PluginInstallRequest>,
) -> Result<(StatusCode, Json<PluginInfo>), ApiError> {
    let staged = stage_request(&deps, &body.repository_url, body.reference.as_deref()).await?;
    if let Some(expected) = body
        .commit
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        && !expected.eq_ignore_ascii_case(&staged.resolved.commit)
    {
        return Err(ApiError::conflict(
            "The repository changed since the preview. Preview it again to see what would be installed",
        ));
    }
    let name = install_staged(&deps, staged).await?;
    let info = plugin_info(deps.host.as_ref(), deps.config.as_ref(), &name)?;
    Ok((StatusCode::CREATED, Json(info)))
}

/// Update one installed plugin from the GitHub repository it came from.
/// A plugin installed from releases moves to the latest release; a pinned
/// one moves only when a new ref is given. Settings and data stay.
#[utoipa::path(
    post,
    tag = "plugins",
    path = "/api/v3/plugins/{name}/update",
    request_body = PluginSourceUpdateRequest,
    params(("name" = String, Path, description = "Plugin name")),
    responses(
        (status = 200, description = "Update outcome", body = PluginSourceUpdateResponse),
        (status = 400, description = "No GitHub source, or not a usable plugin"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown plugin"),
        (status = 409, description = "The repository moved since the preview"),
        (status = 502, description = "GitHub refused the request or could not be reached"),
        (status = 503, description = "GitHub is rate limiting this server"),
    )
)]
pub async fn update_plugin_from_source(
    State(deps): State<PluginsDeps>,
    _admin: Admin,
    Path(name): Path<String>,
    ValidJson(body): ValidJson<PluginSourceUpdateRequest>,
) -> Result<Json<PluginSourceUpdateResponse>, ApiError> {
    deps.host.get(&name).ok_or(PluginError::NotFound)?;
    let record = deps
        .host
        .install_record(&name)
        .ok_or_else(|| map_install(InstallError::NoSource))?;
    let asked = body
        .reference
        .as_deref()
        .map(str::trim)
        .filter(|reference| !reference.is_empty());
    let reference = match (asked, record.ref_kind) {
        (Some(reference), _) => Some(reference.to_owned()),
        // Releases move to the newest release.
        (None, PluginRefKind::Release) => None,
        // Branches move to their tip.
        (None, PluginRefKind::Branch) if record.reference == "HEAD" => None,
        (None, PluginRefKind::Branch) => Some(record.reference.clone()),
        // Tags and commits stay pinned.
        (None, PluginRefKind::Tag | PluginRefKind::Commit) => Some(record.commit.clone()),
    };
    let url = format!("https://github.com/{}", record.repository);
    let staged = stage_request(&deps, &url, reference.as_deref()).await?;
    if staged.manifest.name != name {
        return Err(ApiError::invalid_input(format!(
            "That repository now holds a plugin named '{}', not '{name}'",
            staged.manifest.name
        )));
    }
    if let Some(expected) = body
        .commit
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        && !expected.eq_ignore_ascii_case(&staged.resolved.commit)
    {
        return Err(ApiError::conflict(
            "The repository changed since the preview. Preview it again to see what would be installed",
        ));
    }
    let updated = staged.resolved.commit != record.commit;
    if updated {
        install_staged(&deps, staged).await?;
    }
    let plugin = plugin_info(deps.host.as_ref(), deps.config.as_ref(), &name)?;
    Ok(Json(PluginSourceUpdateResponse { updated, plugin }))
}

/// Save one plugin's enable switch plus its settings.
#[utoipa::path(
    put,
    tag = "plugins",
    path = "/api/v3/plugins/{name}",
    request_body = PluginUpdateRequest,
    params(("name" = String, Path, description = "Plugin name")),
    responses(
        (status = 200, description = "Updated plugin", body = PluginInfo),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown plugin"),
    )
)]
pub async fn update_plugin(
    State(deps): State<PluginsDeps>,
    _admin: Admin,
    Path(name): Path<String>,
    ValidJson(body): ValidJson<PluginUpdateRequest>,
) -> Result<Json<PluginInfo>, ApiError> {
    let host = Arc::clone(&deps.host);
    let saved = tokio::task::spawn_blocking(move || {
        host.update_settings(&name, body.enabled, body.settings)
    })
    .await
    .map_err(|error| PluginError::internal(&error))?;
    match saved {
        Ok(plugin) => {
            deps.tick_sync.sync_host(&deps.host).await;
            Ok(Json(plugin_info(
                deps.host.as_ref(),
                deps.config.as_ref(),
                &plugin.manifest.name,
            )?))
        }
        Err(UpdateError::NotFound) => Err(ApiError::not_found()),
        Err(UpdateError::StoreFailed(reason)) => Err(ApiError::internal(reason)),
        Err(UpdateError::Busy(reason)) => {
            Err(ApiError::unavailable(Unavailable::ServerBusy, reason))
        }
    }
}

/// Remove one plugin's folder. Its saved settings stay in config, so a
/// reinstall picks them back up.
#[utoipa::path(
    delete,
    tag = "plugins",
    path = "/api/v3/plugins/{name}",
    params(("name" = String, Path, description = "Plugin name")),
    responses(
        (status = 200, description = "Uninstall receipt", body = StatusMessage),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown plugin"),
    )
)]
pub async fn uninstall_plugin(
    State(deps): State<PluginsDeps>,
    _admin: Admin,
    Path(name): Path<String>,
) -> Result<Json<StatusMessage>, ApiError> {
    let host = Arc::clone(&deps.host);
    let removed = tokio::task::spawn_blocking({
        let name = name.clone();
        move || host.uninstall(&name)
    })
    .await
    .map_err(|error| PluginError::internal(&error))?;
    match removed {
        Ok(()) => {
            deps.tick_sync.sync_host(&deps.host).await;
            Ok(Json(StatusMessage {
                status: "ok".to_owned(),
                message: format!("Removed {name}"),
            }))
        }
        Err(UninstallError::NotFound) => Err(ApiError::not_found()),
    }
}

/// List enabled plugin acquisition sources. Any authenticated user may
/// read this; only admins manage plugins.
#[utoipa::path(
    get,
    tag = "plugins",
    path = "/api/v3/plugins/sources",
    responses(
        (status = 200, description = "Plugin sources", body = PluginSourcesResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_plugin_sources(
    State(deps): State<PluginsDeps>,
    _user: User,
) -> Json<PluginSourcesResponse> {
    Json(PluginSourcesResponse {
        sources: deps.host.plugin_sources(),
    })
}

// ---------------------------------------------------------------------------
// Guarded plugin HTTP under /ext/
// ---------------------------------------------------------------------------

/// One pacing bucket: (plugin, caller, method, subpath).
type ExtRateKey = (String, String, String, String);

/// Per-route request pacing: one sliding minute window per key, capped so
/// the map cannot grow without bound.
#[derive(Debug, Default)]
pub struct ExtRateLimiter {
    hits: Mutex<HashMap<ExtRateKey, Vec<Instant>>>,
}

impl ExtRateLimiter {
    /// Empty limiter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a call; `None` when allowed, else Retry-After seconds.
    pub fn check(
        &self,
        plugin: &str,
        principal: &str,
        method: &str,
        subpath: &str,
        limit_per_minute: i64,
    ) -> Option<u64> {
        const WINDOW: Duration = Duration::from_secs(60);
        const CAP: usize = 10_000;
        let now = Instant::now();
        let mut guard = self.hits.lock().ok()?;
        let key = (
            plugin.to_owned(),
            principal.to_owned(),
            method.to_owned(),
            subpath.to_owned(),
        );
        if !guard.contains_key(&key) && guard.len() >= CAP {
            guard.retain(|_, hits| hits.iter().any(|hit| now.duration_since(*hit) < WINDOW));
            while guard.len() >= CAP {
                if let Some(first) = guard.keys().next().cloned() {
                    guard.remove(&first);
                } else {
                    break;
                }
            }
        }
        let hits = guard.entry(key).or_default();
        hits.retain(|hit| now.duration_since(*hit) < WINDOW);
        let limit = limit_per_minute.max(1) as usize;
        if hits.len() >= limit {
            let retry_after = hits
                .first()
                .and_then(|oldest| oldest.checked_add(WINDOW))
                .and_then(|reset| reset.checked_duration_since(now))
                .map(|wait| wait.as_secs() + 1)
                .unwrap_or(1)
                .max(1);
            return Some(retry_after);
        }
        hits.push(now);
        None
    }
}

async fn serve_ext(
    deps: &PluginsDeps,
    caller: &Principal,
    plugin_name: &str,
    subpath: &str,
    method: &str,
    request: Request,
) -> Result<Response, PluginError> {
    if plugin_name.is_empty()
        || subpath.is_empty()
        || subpath.starts_with('/')
        || subpath.split('/').any(|part| part == "..")
    {
        return Err(PluginError::InvalidInput {
            message: "Invalid plugin route".to_owned(),
        });
    }
    let plugin = match deps.host.get(plugin_name) {
        Some(plugin) if plugin.serves(Capability::Publisher) => plugin,
        _ => return Err(PluginError::NotFound),
    };
    let spec = plugin
        .manifest
        .routes
        .iter()
        .find(|route| route.path == subpath && route.method.as_str() == method);
    let Some(spec) = spec else {
        return Err(PluginError::NotFound);
    };
    if spec.auth == RouteAuth::Admin {
        Admin::try_from(caller.clone()).map_err(|refused| PluginError::Forbidden {
            message: refused.to_string(),
        })?;
    }
    if let Some(retry_after) = deps.ext_limiter.check(
        plugin_name,
        caller.user_id(),
        method,
        subpath,
        spec.rate_limit_per_minute,
    ) {
        return Err(PluginError::RateLimited { retry_after });
    }
    let (parts, body) = request.into_parts();
    if let Some(declared) = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<usize>().ok())
        && declared > ROUTE_BODY_MAX_BYTES
    {
        return Err(PluginError::PayloadTooLarge);
    }
    let raw = axum::body::to_bytes(body, ROUTE_BODY_MAX_BYTES + 1)
        .await
        .map_err(|_| PluginError::PayloadTooLarge)?;
    let route_body = if raw.is_empty() {
        PluginRouteBody::Empty
    } else if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&raw) {
        PluginRouteBody::Json(json)
    } else if let Ok(text) = std::str::from_utf8(&raw) {
        PluginRouteBody::Text(text.to_owned())
    } else {
        PluginRouteBody::Empty
    };
    let mut query = HashMap::new();
    if let Some(raw_query) = parts.uri.query() {
        for pair in raw_query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (key, value),
                None => (pair, ""),
            };
            query.insert(
                percent_decode(key).unwrap_or_else(|| key.to_owned()),
                percent_decode(value).unwrap_or_else(|| value.to_owned()),
            );
        }
    }
    let result = deps
        .host
        .handle_plugin_route(plugin_name, method, subpath, &query, &route_body)
        .await;
    let status =
        StatusCode::from_u16(result.status.max(100) as u16).unwrap_or(StatusCode::BAD_GATEWAY);
    if status == StatusCode::BAD_GATEWAY {
        return Err(PluginError::RouteFailed);
    }
    if status == StatusCode::NOT_FOUND {
        return Err(PluginError::NotFound);
    }
    Ok((status, Json(result.body)).into_response())
}

/// Decode one percent-encoded query part. Malformed escapes fail the part
/// (the caller keeps the raw text) instead of failing the request.
fn percent_decode(part: &str) -> Option<String> {
    let bytes = part.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            index += 3;
            continue;
        }
        if bytes[index] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8(out).ok()
}

/// Serve one declared plugin GET route.
#[utoipa::path(
    get,
    tag = "plugins",
    path = "/api/v3/plugins/ext/{name}/{subpath}",
    params(
        ("name" = String, Path, description = "Plugin name"),
        ("subpath" = String, Path, description = "Declared route subpath"),
    ),
    responses(
        (status = 200, description = "Plugin answer"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown plugin or route"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn plugin_ext_get(
    State(deps): State<PluginsDeps>,
    User(caller): User,
    Path((name, subpath)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    Ok(serve_ext(&deps, &caller, &name, &subpath, "GET", request).await?)
}

/// Serve one declared plugin POST route.
#[utoipa::path(
    post,
    tag = "plugins",
    path = "/api/v3/plugins/ext/{name}/{subpath}",
    params(
        ("name" = String, Path, description = "Plugin name"),
        ("subpath" = String, Path, description = "Declared route subpath"),
    ),
    responses(
        (status = 200, description = "Plugin answer"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown plugin or route"),
        (status = 413, description = "Request body too large"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn plugin_ext_post(
    State(deps): State<PluginsDeps>,
    User(caller): User,
    Path((name, subpath)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    Ok(serve_ext(&deps, &caller, &name, &subpath, "POST", request).await?)
}

/// Serve one declared plugin DELETE route.
#[utoipa::path(
    delete,
    tag = "plugins",
    path = "/api/v3/plugins/ext/{name}/{subpath}",
    params(
        ("name" = String, Path, description = "Plugin name"),
        ("subpath" = String, Path, description = "Declared route subpath"),
    ),
    responses(
        (status = 200, description = "Plugin answer"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown plugin or route"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn plugin_ext_delete(
    State(deps): State<PluginsDeps>,
    User(caller): User,
    Path((name, subpath)): Path<(String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    Ok(serve_ext(&deps, &caller, &name, &subpath, "DELETE", request).await?)
}

// ---------------------------------------------------------------------------
// Plugin panel page
// ---------------------------------------------------------------------------

/// The fixed document around a plugin's panel script.
const PANEL_PAGE_HEAD: &[u8] = b"<!doctype html><html><head><meta charset=\"utf-8\">\
<style>body{font:13px/1.5 system-ui,sans-serif;margin:0;padding:12px;color:#111;background:#fff}\
#panel-root{min-height:40px}</style></head><body><div id=\"panel-root\"></div><script>";
const PANEL_PAGE_TAIL: &[u8] = b"</script></body></html>";

/// What the panel page may do. It is plugin code shown inside the admin UI,
/// so the page is sealed: `sandbox allow-scripts` gives it an origin of its
/// own with no cookies or storage, even when the address is opened in its
/// own tab, and only the app may frame it, along with whatever an admin
/// allowed to frame the app. Inline script and style run, pictures come from
/// `data:` and `blob:` only, and nothing else loads or connects. Never add
/// `allow-same-origin`: the script would then run as the app, with the
/// admin's session.
fn panel_page_policy(config: &ConfigStore) -> String {
    format!(
        "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
         img-src data: blob:; connect-src 'none'; frame-src 'none'; object-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors {}; sandbox allow-scripts",
        droppedneedle_kernel::runtime_config::sections::framed_page_ancestors(&config.live())
    )
}

/// The panel script with every `</script` (any case) broken up, so its text
/// cannot close the element it is inlined into. `<\/script` reads the same
/// inside a JavaScript string or regular expression. Not covered: a script
/// holding `<!--` and then `<script` makes the parser run past the closing
/// tag, which only breaks that plugin's own panel. Every other byte passes
/// through unchanged.
fn inline_panel_script(script: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(script.len() + 16);
    let mut rest = script;
    while let Some((&byte, tail)) = rest.split_first() {
        out.push(byte);
        if byte == b'<'
            && tail.first() == Some(&b'/')
            && tail
                .get(1..7)
                .is_some_and(|name| name.eq_ignore_ascii_case(b"script"))
        {
            out.push(b'\\');
        }
        rest = tail;
    }
    out
}

/// Format one timestamp as an HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`).
fn http_date(when: SystemTime) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = when
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let time = secs % 86_400;
    // Civil date from days since the epoch (Howard Hinnant's algorithm).
    let era_days = days + 719_468;
    let era = era_days.div_euclid(146_097);
    let day_of_era = era_days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u64;
    let month = (if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }) as usize;
    let full_year = if month <= 2 { year + 1 } else { year };
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[(days.rem_euclid(7)) as usize],
        day,
        MONTHS[month - 1],
        full_year,
        time / 3_600,
        (time % 3_600) / 60,
        time % 60
    )
}

/// Serve one enabled plugin's settings panel as a page for the admin UI to
/// frame: a fixed wrapper with the plugin's panel script inlined. Admin-only.
/// The entry path resolves inside the plugin's own directory; anything
/// escaping it reads as missing.
#[utoipa::path(
    get,
    tag = "plugins",
    path = "/api/v3/plugins/{name}/ui/panel",
    params(("name" = String, Path, description = "Plugin name")),
    responses(
        (status = 200, description = "Panel page", content_type = "text/html"),
        (status = 304, description = "Not modified"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
        (status = 404, description = "Unknown plugin or panel"),
    )
)]
pub async fn plugin_panel_page(
    State(deps): State<PluginsDeps>,
    _admin: Admin,
    Path(name): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
        || name.contains('\0')
    {
        return Err(ApiError::not_found());
    }
    let plugin = match deps.host.get(&name) {
        Some(plugin) if plugin.enabled && plugin.runtime.is_some() => plugin,
        _ => return Err(ApiError::not_found()),
    };
    let entry = plugin.manifest.ui_entry.clone();
    if entry.is_empty() {
        return Err(ApiError::not_found());
    }
    let normalized = entry.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.starts_with("http://")
        || normalized.starts_with("https://")
        || normalized.split('/').any(|part| part == "..")
        || normalized.contains('\0')
    {
        return Err(ApiError::not_found());
    }
    if plugin.directory.is_empty() {
        return Err(ApiError::not_found());
    }
    let if_none_match = request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    serve_panel_file(
        &plugin.directory,
        &normalized,
        &name,
        &if_none_match,
        &panel_page_policy(&deps.config),
    )
    .await
    .map_err(ApiError::from)
}

async fn serve_panel_file(
    directory: &str,
    entry: &str,
    plugin_name: &str,
    if_none_match: &str,
    policy: &str,
) -> Result<Response, PluginError> {
    let base = tokio::fs::canonicalize(directory)
        .await
        .map_err(|_| PluginError::NotFound)?;
    let mut joined = base.clone();
    for part in entry.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(PluginError::NotFound);
        }
        joined.push(part);
    }
    let target = tokio::fs::canonicalize(&joined)
        .await
        .map_err(|_| PluginError::NotFound)?;
    if target != base && !target.starts_with(&base) {
        return Err(PluginError::NotFound);
    }
    let metadata = tokio::fs::metadata(&target)
        .await
        .map_err(|_| PluginError::NotFound)?;
    if !metadata.is_file() {
        return Err(PluginError::NotFound);
    }
    // The manifest entry is static and the plugin dir is admin-managed, so
    // the check-then-read below races with nothing untrusted.
    let mtime = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    // The build's version is part of the tag: a release that changes the
    // wrapper must not answer 304 for a page cached from the old one.
    let etag = format!(
        "\"{plugin_name}-{}-{:x}-{:x}\"",
        env!("CARGO_PKG_VERSION"),
        metadata.len(),
        mtime
    );
    // The policy is on the 304 too: a cached page takes its headers from
    // whichever answer the browser saw last.
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header("x-content-type-options", "nosniff")
        .header("content-security-policy", policy)
        .header("cache-control", "private, max-age=60, must-revalidate")
        .header("etag", etag.clone())
        .header(
            "last-modified",
            http_date(metadata.modified().unwrap_or(UNIX_EPOCH)),
        );
    let candidates: Vec<&str> = if_none_match
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    if candidates.iter().any(|token| {
        *token == "*" || *token == etag || token.strip_prefix("W/") == Some(etag.as_str())
    }) {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .map_err(|error| PluginError::internal(&error));
    }
    let bytes = tokio::fs::read(&target)
        .await
        .map_err(|_| PluginError::NotFound)?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(PluginError::NotFound);
    }
    let mut page = PANEL_PAGE_HEAD.to_vec();
    page.extend(inline_panel_script(&bytes));
    page.extend_from_slice(PANEL_PAGE_TAIL);
    builder
        .status(StatusCode::OK)
        .body(Body::from(page))
        .map_err(|error| PluginError::internal(&error))
}

// ---------------------------------------------------------------------------
// Routers
// ---------------------------------------------------------------------------

/// Every plugin route. Installing and managing plugins takes an admin;
/// listing sources and calling a plugin's own routes takes any user. A
/// plugin route's subpath may hold slashes.
pub fn plugin_routes<S>() -> Routes<S>
where
    S: Clone + Send + Sync + 'static,
    PluginsDeps: FromRef<S>,
{
    Routes::new()
        .admin(routes!(list_plugins))
        .admin(routes!(install_plugin))
        .admin(routes!(preview_plugin_install))
        .admin(routes!(update_plugin_from_source))
        .user(routes!(list_plugin_sources))
        .admin(routes!(update_plugin, uninstall_plugin))
        .admin(routes!(plugin_panel_page))
        .user_at(routes!(plugin_ext_get, plugin_ext_post, plugin_ext_delete))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_decoding_tolerates_bad_escapes() {
        assert_eq!(percent_decode("a+b"), Some("a b".to_owned()));
        assert_eq!(percent_decode("a%20b"), Some("a b".to_owned()));
        assert_eq!(percent_decode("a%zz"), None);
        assert_eq!(percent_decode("a%2"), None);
    }

    #[test]
    fn limiter_opens_after_the_first_call_over_budget() {
        let limiter = ExtRateLimiter::new();
        assert_eq!(limiter.check("p", "u", "GET", "s", 1), None);
        assert!(limiter.check("p", "u", "GET", "s", 1).is_some());
        assert_eq!(limiter.check("p", "u", "GET", "other", 1), None);
    }

    #[test]
    fn http_date_matches_the_reference_point() {
        // 2024-01-01T00:00:00Z is a Monday.
        let when = UNIX_EPOCH + Duration::from_secs(1_704_067_200);
        assert_eq!(http_date(when), "Mon, 01 Jan 2024 00:00:00 GMT");
        assert_eq!(http_date(UNIX_EPOCH), "Thu, 01 Jan 1970 00:00:00 GMT");
    }
}
