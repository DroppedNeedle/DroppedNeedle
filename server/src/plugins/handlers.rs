//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract the caller, call the host or
//! service, render. Status mapping lives in
//! [`PluginError`](super::error::PluginError). Bodies parse through
//! [`ValidJson`], which keeps malformed input inside the shared error
//! envelope instead of Axum's default plain-text 400.
//!
//! Blocking host mutations (install, update, uninstall) run on the
//! blocking pool; dispatches and ticks stay on the async runtime.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    body::Body,
    extract::{FromRequest, FromRequestParts, Path, Request, State},
    http::{StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;

use crate::auth::session::middleware::CurrentSession;
use crate::auth::users::roles::Role;
use crate::ids::IdGenerator;
use crate::runtime_config::ConfigStore;

use super::error::PluginError;
use super::host::{
    ArchiveFetcher, ArchiveUnpacker, InstallError, PluginHost, ROUTE_BODY_MAX_BYTES,
    UninstallError, UpdateError,
};
use super::models::{
    ConnectionStatus, ListenBrainzConnectRequest, PluginInfo, PluginInstallRequest,
    PluginListResponse, PluginSettingFieldInfo, PluginSourcesResponse, PluginUpdateRequest,
    ScrobblePreferences, ScrobblePreferencesUpdate, StatusMessage,
};
use super::runtime::PluginRouteBody;
use super::scrobble::{
    ConnectError, MixStateReader, PrefsError, ScrobbleDeps, ScrobblePrefsPatch,
    connect_listenbrainz, disconnect_listenbrainz, get_prefs, update_prefs,
};
use super::ticks::TickLoopSync;

/// Role lookups for route gating. The users store owns the rows; this seam
/// keeps plugins independent of that store. Production wires it over the
/// user store; tests use a fixed map.
pub trait UserRoles: Send + Sync {
    /// One user's role, or `None` when the account is gone (stale session).
    fn role_of(&self, user_id: &str) -> Option<Role>;
}

/// Plugin-route dependencies.
#[derive(Clone)]
pub struct PluginsDeps {
    /// The plugin host.
    pub host: Arc<PluginHost>,
    /// Config store for masked settings reads.
    pub config: Arc<ConfigStore>,
    /// Role lookups for admin gating.
    pub roles: Arc<dyn UserRoles>,
    /// Error ids.
    pub ids: Arc<dyn IdGenerator>,
    /// Install archive fetcher.
    pub fetcher: Arc<dyn ArchiveFetcher>,
    /// Install archive unpacker.
    pub unpacker: Arc<dyn ArchiveUnpacker>,
    /// Tick-loop rebuilds, synced after every mutation.
    pub tick_sync: Arc<dyn TickLoopSync>,
    /// `/ext/` rate limiter.
    pub ext_limiter: Arc<ExtRateLimiter>,
}

/// Scrobble-settings route dependencies.
pub struct ScrobbleHttpDeps {
    /// Scrobble settings service dependencies.
    pub deps: ScrobbleDeps,
    /// Role lookups (admins read approved-by-role mix state).
    pub roles: Arc<dyn UserRoles>,
    /// Standing-grant state reader.
    pub mix_state: Arc<dyn MixStateReader>,
    /// Error ids.
    pub ids: Arc<dyn IdGenerator>,
}

impl Clone for ScrobbleHttpDeps {
    fn clone(&self) -> Self {
        Self {
            deps: ScrobbleDeps {
                prefs: Arc::clone(&self.deps.prefs),
                links: Arc::clone(&self.deps.links),
                verifier: Arc::clone(&self.deps.verifier),
                mix_hook: Arc::clone(&self.deps.mix_hook),
                cache_hook: Arc::clone(&self.deps.cache_hook),
            },
            roles: Arc::clone(&self.roles),
            mix_state: Arc::clone(&self.mix_state),
            ids: Arc::clone(&self.ids),
        }
    }
}

/// The authenticated caller: user id plus a live role lookup.
#[derive(Debug, Clone)]
pub struct RouteCaller {
    /// Owning user id.
    pub user_id: String,
    /// Account role.
    pub role: Role,
}

impl RouteCaller {
    /// Resolve the caller from the stashed session plus a role lookup. A
    /// missing session or a gone account is 401.
    pub fn require(parts: &Parts, roles: &dyn UserRoles) -> Result<Self, PluginError> {
        let session =
            parts
                .extensions
                .get::<CurrentSession>()
                .ok_or(PluginError::Unauthorized {
                    message: "Authentication required".to_owned(),
                })?;
        let role = roles
            .role_of(&session.user_id)
            .ok_or(PluginError::Unauthorized {
                message: "Authentication required".to_owned(),
            })?;
        Ok(Self {
            user_id: session.user_id.clone(),
            role,
        })
    }
}

/// Any authenticated user.
pub struct AuthenticatedUser(pub RouteCaller);

impl FromRequestParts<PluginsDeps> for AuthenticatedUser {
    type Rejection = PluginError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &PluginsDeps,
    ) -> Result<Self, Self::Rejection> {
        RouteCaller::require(parts, state.roles.as_ref()).map(AuthenticatedUser)
    }
}

/// An admin. Non-admin is 403.
pub struct AdminUser(pub RouteCaller);

impl FromRequestParts<PluginsDeps> for AdminUser {
    type Rejection = PluginError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &PluginsDeps,
    ) -> Result<Self, Self::Rejection> {
        let caller = RouteCaller::require(parts, state.roles.as_ref())?;
        if caller.role == Role::Admin {
            Ok(AdminUser(caller))
        } else {
            Err(PluginError::Forbidden {
                message: "Admin access required".to_owned(),
            })
        }
    }
}

/// Any authenticated user on the scrobble routes.
pub struct ScrobbleUser(pub RouteCaller);

impl FromRequestParts<ScrobbleHttpDeps> for ScrobbleUser {
    type Rejection = PluginError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ScrobbleHttpDeps,
    ) -> Result<Self, Self::Rejection> {
        RouteCaller::require(parts, state.roles.as_ref()).map(ScrobbleUser)
    }
}

/// JSON body extractor that renders failures in the shared envelope.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = PluginError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| PluginError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
            })
    }
}

fn map_install(error: InstallError, ids: &dyn IdGenerator) -> PluginError {
    match error {
        InstallError::Io(reason) => PluginError::internal(&reason, ids),
        other => PluginError::InvalidInput {
            message: other.to_string(),
        },
    }
}

/// One plugin as the admin UI sees it, secrets masked.
fn plugin_info(
    host: &PluginHost,
    config: &ConfigStore,
    ids: &dyn IdGenerator,
    name: &str,
) -> Result<PluginInfo, PluginError> {
    let plugin = host.get(name).ok_or(PluginError::NotFound)?;
    let manifest = &plugin.manifest;
    let stored = config
        .get_plugin_masked(name, &manifest.secret_keys())
        .map_err(|error| PluginError::internal(&error, ids))?;
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
    })
}

// ---------------------------------------------------------------------------
// Admin plugin routes
// ---------------------------------------------------------------------------

/// List every discovered plugin, secrets masked.
#[utoipa::path(
    get,
    path = "/api/v3/plugins",
    responses(
        (status = 200, description = "Plugin listing", body = PluginListResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
    )
)]
pub async fn list_plugins(
    State(deps): State<PluginsDeps>,
    _admin: AdminUser,
) -> Result<Json<PluginListResponse>, PluginError> {
    let mut plugins = Vec::new();
    for plugin in deps.host.list_plugins() {
        plugins.push(plugin_info(
            deps.host.as_ref(),
            deps.config.as_ref(),
            deps.ids.as_ref(),
            &plugin.manifest.name,
        )?);
    }
    Ok(Json(PluginListResponse { plugins }))
}

/// Install a plugin from a public GitHub repository. The code is stored,
/// never executed: the plugin arrives disabled and an admin must enable
/// it, exactly like a hand-copied folder.
#[utoipa::path(
    post,
    path = "/api/v3/plugins/install",
    request_body = PluginInstallRequest,
    responses(
        (status = 201, description = "Installed plugin", body = PluginInfo),
        (status = 400, description = "Not a usable plugin repository"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
    )
)]
pub async fn install_plugin(
    State(deps): State<PluginsDeps>,
    _admin: AdminUser,
    ValidJson(body): ValidJson<PluginInstallRequest>,
) -> Result<(StatusCode, Json<PluginInfo>), PluginError> {
    let archive = PluginHost::fetch_plugin_archive(&body.repository_url, deps.fetcher.as_ref())
        .await
        .map_err(|error| map_install(error, deps.ids.as_ref()))?;
    let host = Arc::clone(&deps.host);
    let unpacker = Arc::clone(&deps.unpacker);
    let name =
        tokio::task::spawn_blocking(move || host.install_archive(&archive, unpacker.as_ref()))
            .await
            .map_err(|error| PluginError::internal(&error, deps.ids.as_ref()))?
            .map_err(|error| map_install(error, deps.ids.as_ref()))?;
    deps.tick_sync.sync_host(&deps.host).await;
    let info = plugin_info(
        deps.host.as_ref(),
        deps.config.as_ref(),
        deps.ids.as_ref(),
        &name,
    )?;
    Ok((StatusCode::CREATED, Json(info)))
}

/// Save one plugin's enable switch plus its settings.
#[utoipa::path(
    put,
    path = "/api/v3/plugins/{name}",
    request_body = PluginUpdateRequest,
    params(("name" = String, Path, description = "Plugin name")),
    responses(
        (status = 200, description = "Updated plugin", body = PluginInfo),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
        (status = 404, description = "Unknown plugin"),
    )
)]
pub async fn update_plugin(
    State(deps): State<PluginsDeps>,
    _admin: AdminUser,
    Path(name): Path<String>,
    ValidJson(body): ValidJson<PluginUpdateRequest>,
) -> Result<Json<PluginInfo>, PluginError> {
    let host = Arc::clone(&deps.host);
    let saved = tokio::task::spawn_blocking(move || {
        host.update_settings(&name, body.enabled, body.settings)
    })
    .await
    .map_err(|error| PluginError::internal(&error, deps.ids.as_ref()))?;
    match saved {
        Ok(plugin) => {
            deps.tick_sync.sync_host(&deps.host).await;
            Ok(Json(plugin_info(
                deps.host.as_ref(),
                deps.config.as_ref(),
                deps.ids.as_ref(),
                &plugin.manifest.name,
            )?))
        }
        Err(UpdateError::NotFound) => Err(PluginError::NotFound),
        Err(UpdateError::StoreFailed(reason)) => {
            Err(PluginError::internal(&reason, deps.ids.as_ref()))
        }
    }
}

/// Remove one plugin's folder. Its saved settings stay in config, so a
/// reinstall picks them back up.
#[utoipa::path(
    delete,
    path = "/api/v3/plugins/{name}",
    params(("name" = String, Path, description = "Plugin name")),
    responses(
        (status = 200, description = "Uninstall receipt", body = StatusMessage),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
        (status = 404, description = "Unknown plugin"),
    )
)]
pub async fn uninstall_plugin(
    State(deps): State<PluginsDeps>,
    _admin: AdminUser,
    Path(name): Path<String>,
) -> Result<Json<StatusMessage>, PluginError> {
    let host = Arc::clone(&deps.host);
    let removed = tokio::task::spawn_blocking({
        let name = name.clone();
        move || host.uninstall(&name)
    })
    .await
    .map_err(|error| PluginError::internal(&error, deps.ids.as_ref()))?;
    match removed {
        Ok(()) => {
            deps.tick_sync.sync_host(&deps.host).await;
            Ok(Json(StatusMessage {
                status: "ok".to_owned(),
                message: format!("Removed {name}"),
            }))
        }
        Err(UninstallError::NotFound) => Err(PluginError::NotFound),
    }
}

/// List enabled plugin acquisition sources. Any authenticated user may
/// read this; only admins manage plugins.
#[utoipa::path(
    get,
    path = "/api/v3/plugins/sources",
    responses(
        (status = 200, description = "Plugin sources", body = PluginSourcesResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_plugin_sources(
    State(deps): State<PluginsDeps>,
    _user: AuthenticatedUser,
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
    caller: &RouteCaller,
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
        Some(plugin) if plugin.enabled && plugin.module.is_some() => plugin,
        _ => return Err(PluginError::NotFound),
    };
    let spec = plugin
        .manifest
        .routes
        .iter()
        .find(|route| route.path == subpath && route.method.to_ascii_uppercase() == method);
    let Some(spec) = spec else {
        return Err(PluginError::NotFound);
    };
    if spec.auth == "admin" && caller.role != Role::Admin {
        return Err(PluginError::Forbidden {
            message: "Admin access required".to_owned(),
        });
    }
    if let Some(retry_after) = deps.ext_limiter.check(
        plugin_name,
        &caller.user_id,
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
    path = "/api/v3/plugins/ext/{name}/{subpath}",
    params(
        ("name" = String, Path, description = "Plugin name"),
        ("subpath" = String, Path, description = "Declared route subpath"),
    ),
    responses(
        (status = 200, description = "Plugin answer"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
        (status = 404, description = "Unknown plugin or route"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn plugin_ext_get(
    State(deps): State<PluginsDeps>,
    caller: AuthenticatedUser,
    Path((name, subpath)): Path<(String, String)>,
    request: Request,
) -> Result<Response, PluginError> {
    serve_ext(&deps, &caller.0, &name, &subpath, "GET", request).await
}

/// Serve one declared plugin POST route.
#[utoipa::path(
    post,
    path = "/api/v3/plugins/ext/{name}/{subpath}",
    params(
        ("name" = String, Path, description = "Plugin name"),
        ("subpath" = String, Path, description = "Declared route subpath"),
    ),
    responses(
        (status = 200, description = "Plugin answer"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
        (status = 404, description = "Unknown plugin or route"),
        (status = 413, description = "Request body too large"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn plugin_ext_post(
    State(deps): State<PluginsDeps>,
    caller: AuthenticatedUser,
    Path((name, subpath)): Path<(String, String)>,
    request: Request,
) -> Result<Response, PluginError> {
    serve_ext(&deps, &caller.0, &name, &subpath, "POST", request).await
}

/// Serve one declared plugin DELETE route.
#[utoipa::path(
    delete,
    path = "/api/v3/plugins/ext/{name}/{subpath}",
    params(
        ("name" = String, Path, description = "Plugin name"),
        ("subpath" = String, Path, description = "Declared route subpath"),
    ),
    responses(
        (status = 200, description = "Plugin answer"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
        (status = 404, description = "Unknown plugin or route"),
        (status = 429, description = "Too many requests"),
    )
)]
pub async fn plugin_ext_delete(
    State(deps): State<PluginsDeps>,
    caller: AuthenticatedUser,
    Path((name, subpath)): Path<(String, String)>,
    request: Request,
) -> Result<Response, PluginError> {
    serve_ext(&deps, &caller.0, &name, &subpath, "DELETE", request).await
}

// ---------------------------------------------------------------------------
// Plugin panel bundle
// ---------------------------------------------------------------------------

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

/// Serve one enabled plugin's panel bundle. Admin-only: panel code runs in
/// the admin UI. The entry path resolves inside the plugin's own directory;
/// anything escaping it reads as missing.
#[utoipa::path(
    get,
    path = "/api/v3/plugins/{name}/ui/panel.js",
    params(("name" = String, Path, description = "Plugin name")),
    responses(
        (status = 200, description = "Panel bundle"),
        (status = 304, description = "Not modified"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin access required"),
        (status = 404, description = "Unknown plugin or panel"),
    )
)]
pub async fn plugin_panel_js(
    State(deps): State<PluginsDeps>,
    _admin: AdminUser,
    Path(name): Path<String>,
    request: Request,
) -> Result<Response, PluginError> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
        || name.contains('\0')
    {
        return Err(PluginError::NotFound);
    }
    let plugin = match deps.host.get(&name) {
        Some(plugin) if plugin.enabled && plugin.module.is_some() => plugin,
        _ => return Err(PluginError::NotFound),
    };
    let entry = plugin.manifest.ui_entry.clone();
    if entry.is_empty() {
        return Err(PluginError::NotFound);
    }
    let normalized = entry.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.starts_with("http://")
        || normalized.starts_with("https://")
        || normalized.split('/').any(|part| part == "..")
        || normalized.contains('\0')
    {
        return Err(PluginError::NotFound);
    }
    if plugin.directory.is_empty() {
        return Err(PluginError::NotFound);
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
        deps.ids.as_ref(),
    )
    .await
}

async fn serve_panel_file(
    directory: &str,
    entry: &str,
    plugin_name: &str,
    if_none_match: &str,
    ids: &dyn IdGenerator,
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
    let etag = format!("\"{plugin_name}-{:x}-{:x}\"", metadata.len(), mtime);
    let mut builder = Response::builder()
        .header("x-content-type-options", "nosniff")
        .header("content-security-policy", "sandbox")
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
            .map_err(|error| PluginError::internal(&error, ids));
    }
    let bytes = tokio::fs::read(&target)
        .await
        .map_err(|_| PluginError::NotFound)?;
    builder = builder
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/javascript");
    builder
        .body(Body::from(bytes))
        .map_err(|error| PluginError::internal(&error, ids))
}

// ---------------------------------------------------------------------------
// Scrobble-settings routes
// ---------------------------------------------------------------------------

async fn prefs_response(deps: &ScrobbleHttpDeps, user_id: &str, role: Role) -> ScrobblePreferences {
    let prefs = get_prefs(&deps.deps, user_id).await;
    ScrobblePreferences {
        scrobble_to_lastfm: prefs.scrobble_to_lastfm,
        scrobble_to_listenbrainz: prefs.scrobble_to_listenbrainz,
        navidrome_handles_external_scrobbles: prefs.navidrome_handles_external_scrobbles,
        primary_music_source: prefs.primary_music_source,
        now_playing_visibility: prefs.now_playing_visibility,
        auto_request_personal_mix: prefs.auto_request_personal_mix,
        auto_request_state: deps.mix_state.auto_request_state(
            user_id,
            role.as_str(),
            prefs.auto_request_personal_mix,
        ),
    }
}

/// Read the caller's scrobble preferences.
#[utoipa::path(
    get,
    path = "/api/v3/me/scrobble-preferences",
    responses(
        (status = 200, description = "Scrobble preferences", body = ScrobblePreferences),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_scrobble_preferences(
    State(deps): State<ScrobbleHttpDeps>,
    user: ScrobbleUser,
) -> Json<ScrobblePreferences> {
    Json(prefs_response(&deps, &user.0.user_id, user.0.role).await)
}

/// Update the caller's scrobble preferences. Absent fields keep their
/// stored values.
#[utoipa::path(
    put,
    path = "/api/v3/me/scrobble-preferences",
    request_body = ScrobblePreferencesUpdate,
    responses(
        (status = 200, description = "Scrobble preferences", body = ScrobblePreferences),
        (status = 400, description = "Unknown enum value"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn update_scrobble_preferences(
    State(deps): State<ScrobbleHttpDeps>,
    user: ScrobbleUser,
    ValidJson(body): ValidJson<ScrobblePreferencesUpdate>,
) -> Result<Json<ScrobblePreferences>, PluginError> {
    let patch = ScrobblePrefsPatch {
        scrobble_to_lastfm: body.scrobble_to_lastfm,
        scrobble_to_listenbrainz: body.scrobble_to_listenbrainz,
        navidrome_handles_external_scrobbles: body.navidrome_handles_external_scrobbles,
        primary_music_source: body.primary_music_source,
        now_playing_visibility: body.now_playing_visibility,
        auto_request_personal_mix: body.auto_request_personal_mix,
    };
    match update_prefs(&deps.deps, &user.0.user_id, user.0.role.as_str(), &patch).await {
        Ok(_) => Ok(Json(
            prefs_response(&deps, &user.0.user_id, user.0.role).await,
        )),
        Err(PrefsError::InvalidValue(message)) => Err(PluginError::InvalidInput { message }),
    }
}

/// Link the caller's ListenBrainz account. The credential verifies first;
/// only a verified pair stores.
#[utoipa::path(
    put,
    path = "/api/v3/me/connections/listenbrainz",
    request_body = ListenBrainzConnectRequest,
    responses(
        (status = 200, description = "Link status", body = ConnectionStatus),
        (status = 400, description = "Username required or credential rejected"),
        (status = 401, description = "Not authenticated"),
        (status = 429, description = "ListenBrainz is rate-limiting"),
    )
)]
pub async fn connect_listenbrainz_route(
    State(deps): State<ScrobbleHttpDeps>,
    user: ScrobbleUser,
    ValidJson(body): ValidJson<ListenBrainzConnectRequest>,
) -> Result<Json<ConnectionStatus>, PluginError> {
    match connect_listenbrainz(
        &deps.deps,
        &user.0.user_id,
        &body.username,
        &body.user_token,
    )
    .await
    {
        Ok(link) => Ok(Json(ConnectionStatus {
            service: "listenbrainz".to_owned(),
            enabled: true,
            username: link.username,
        })),
        Err(ConnectError::UsernameRequired) => Err(PluginError::InvalidInput {
            message: "A ListenBrainz username is required".to_owned(),
        }),
        Err(ConnectError::RateLimited) => Err(PluginError::RateLimited { retry_after: 60 }),
        Err(ConnectError::Rejected(message)) => Err(PluginError::InvalidInput { message }),
        Err(ConnectError::StoreFailed(reason)) => {
            Err(PluginError::internal(&reason, deps.ids.as_ref()))
        }
    }
}

/// Read the caller's ListenBrainz link status. Unlinked reads as missing.
#[utoipa::path(
    get,
    path = "/api/v3/me/connections/listenbrainz",
    responses(
        (status = 200, description = "Link status", body = ConnectionStatus),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No link"),
    )
)]
pub async fn listenbrainz_status(
    State(deps): State<ScrobbleHttpDeps>,
    user: ScrobbleUser,
) -> Result<Json<ConnectionStatus>, PluginError> {
    match deps.deps.links.status(&user.0.user_id).await {
        Some(link) => Ok(Json(ConnectionStatus {
            service: "listenbrainz".to_owned(),
            enabled: true,
            username: link.username,
        })),
        None => Err(PluginError::NotFound),
    }
}

/// Remove the caller's ListenBrainz link.
#[utoipa::path(
    delete,
    path = "/api/v3/me/connections/listenbrainz",
    responses(
        (status = 200, description = "Disconnect receipt", body = StatusMessage),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "No link"),
    )
)]
pub async fn disconnect_listenbrainz_route(
    State(deps): State<ScrobbleHttpDeps>,
    user: ScrobbleUser,
) -> Result<Json<StatusMessage>, PluginError> {
    if disconnect_listenbrainz(&deps.deps, &user.0.user_id).await {
        Ok(Json(StatusMessage {
            status: "ok".to_owned(),
            message: "Disconnected listenbrainz".to_owned(),
        }))
    } else {
        Err(PluginError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// Routers
// ---------------------------------------------------------------------------

/// Every plugin route. Mount under `/api/v3` behind the
/// session gate; see `mod.rs` for the wiring note.
pub fn plugins_router(deps: PluginsDeps) -> axum::Router {
    use axum::routing::{get, post, put};

    axum::Router::new()
        .route("/plugins", get(list_plugins))
        .route("/plugins/install", post(install_plugin))
        .route("/plugins/sources", get(list_plugin_sources))
        .route(
            "/plugins/{name}",
            put(update_plugin).delete(uninstall_plugin),
        )
        .route("/plugins/{name}/ui/panel.js", get(plugin_panel_js))
        .route(
            "/plugins/ext/{name}/{*subpath}",
            get(plugin_ext_get)
                .post(plugin_ext_post)
                .delete(plugin_ext_delete),
        )
        .with_state(deps)
}

/// Every scrobble-settings route. Mount under `/api/v3`
/// behind the session gate.
pub fn scrobble_router(deps: ScrobbleHttpDeps) -> axum::Router {
    use axum::routing::get;

    axum::Router::new()
        .route(
            "/me/scrobble-preferences",
            get(get_scrobble_preferences).put(update_scrobble_preferences),
        )
        .route(
            "/me/connections/listenbrainz",
            get(listenbrainz_status)
                .put(connect_listenbrainz_route)
                .delete(disconnect_listenbrainz_route),
        )
        .with_state(deps)
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
