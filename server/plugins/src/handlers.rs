//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract the caller, call the host or
//! service, render. Status mapping lives in the `From<PluginError>` impl
//! for [`ApiError`]. Bodies parse through the kernel's [`ValidJson`].
//!
//! Blocking host mutations (install, update, uninstall) run on the
//! blocking pool; dispatches and ticks stay on the async runtime.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{FromRef, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};

use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::ConfigStore;

use super::error::{PluginError, SourceFault};
use super::ext::ExtCall;
pub use super::ext::ExtRateLimiter;
use super::host::{PluginHost, UninstallError, UpdateError};
use super::install::{
    ArchiveFetcher, InstallError, StagedPlugin, TRUST_WARNING, parse_source, resolve, stage,
};
use super::manifest::RouteMethod;
use super::models::{
    PluginInfo, PluginInstallInfo, PluginInstallPreview, PluginInstallRequest, PluginListResponse,
    PluginRuntimeInfo, PluginSettingFieldInfo, PluginSourceUpdateRequest,
    PluginSourceUpdateResponse, PluginSourcesResponse, PluginUpdateRequest, StatusMessage,
};
use super::ticks::PluginTickLoops;
use super::{ext, panel};

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
        InstallError::Unreadable => PluginError::Source(SourceFault::Unreadable),
        refused @ (InstallError::NoReleasesNow | InstallError::RepositoryReplaced) => {
            PluginError::Conflict {
                message: refused.to_string(),
            }
        }
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
        (status = 502, description = "GitHub refused the request, could not be reached, or sent an unreadable answer"),
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
        (status = 502, description = "GitHub refused the request, could not be reached, or sent an unreadable answer"),
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
/// A plugin installed from releases moves to the latest release, a branch
/// to its tip; a pinned one moves only when a new ref is given. An update
/// never switches the kind of install by itself, and never installs from a
/// repository GitHub knows by another id. Settings and data stay.
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
        (status = 409, description = "The repository moved since the preview, was replaced, or has no releases now"),
        (status = 502, description = "GitHub refused the request, could not be reached, or sent an unreadable answer"),
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
    let source = record
        .update_source(body.reference.as_deref())
        .map_err(map_install)?;
    let resolved = resolve(deps.fetcher.as_ref(), &deps.github, source)
        .await
        .map_err(map_install)?;
    record.same_repository(&resolved).map_err(map_install)?;
    let repository_id = resolved.repository_id;
    let staged = stage(deps.fetcher.as_ref(), &deps.github, resolved)
        .await
        .map_err(map_install)?;
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
    } else if record.repository_id.is_none() {
        // One small file, read and written in place like the record read above.
        deps.host
            .keep_repository_id(&name, repository_id)
            .map_err(map_install)?;
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
    ValidQuery(query): ValidQuery<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let call = ExtCall {
        caller: &caller,
        plugin: &name,
        subpath: &subpath,
        method: RouteMethod::Get,
        query,
        headers: &headers,
        body,
    };
    let answer = ext::serve(&deps.host, &deps.ext_limiter, call).await?;
    Ok((answer.status, Json(answer.body)).into_response())
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
    ValidQuery(query): ValidQuery<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let call = ExtCall {
        caller: &caller,
        plugin: &name,
        subpath: &subpath,
        method: RouteMethod::Post,
        query,
        headers: &headers,
        body,
    };
    let answer = ext::serve(&deps.host, &deps.ext_limiter, call).await?;
    Ok((answer.status, Json(answer.body)).into_response())
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
    ValidQuery(query): ValidQuery<HashMap<String, String>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let call = ExtCall {
        caller: &caller,
        plugin: &name,
        subpath: &subpath,
        method: RouteMethod::Delete,
        query,
        headers: &headers,
        body,
    };
    let answer = ext::serve(&deps.host, &deps.ext_limiter, call).await?;
    Ok((answer.status, Json(answer.body)).into_response())
}

// ---------------------------------------------------------------------------
// Plugin panel page
// ---------------------------------------------------------------------------

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
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let answer = panel::serve(&deps.host, &deps.config, &name, if_none_match).await?;
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, answer.policy)
        .header(
            header::CACHE_CONTROL,
            "private, max-age=60, must-revalidate",
        )
        .header(header::ETAG, answer.etag)
        .header(header::LAST_MODIFIED, answer.last_modified);
    let rendered = match answer.page {
        Some(page) => builder.status(StatusCode::OK).body(Body::from(page)),
        None => builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()),
    };
    Ok(rendered.map_err(|error| PluginError::internal(&error))?)
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
