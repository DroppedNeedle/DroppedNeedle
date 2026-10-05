//! System info, login and user identity routes.

use crate::auth::compat_auth::jellyfin::{
    JellyfinPasswordStore, JellyfinRequest, SessionFacts, authenticate_by_name, extract_client,
    extract_device, login_echo_json, server_id,
};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use uuid::Uuid;

use super::builders::{self};
use super::models::{AuthenticateRequest, PublicSystemInfo, SystemInfo};
use super::router::*;
use super::seams::{IdMap, LibraryRead, PlaybackSessions, StreamEngine};

// ===== System / identity =====

pub(super) async fn system_info_public<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let settings = state.settings.jellyfin();
    json(
        StatusCode::OK,
        &PublicSystemInfo {
            local_address: local_address(&state, request.headers()),
            server_name: settings.server_name.clone(),
            version: settings.server_version.clone(),
            product_name: "Jellyfin Server".to_owned(),
            operating_system: String::new(),
            id: server_id(),
            startup_wizard_completed: true,
        },
    )
}

pub(super) async fn system_info<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let query = request.uri().query().map(str::to_owned);
    if authed(&state.passwords, request.headers(), query.as_deref())
        .await
        .is_err()
    {
        return error(StatusCode::UNAUTHORIZED);
    }
    let settings = state.settings.jellyfin();
    json(
        StatusCode::OK,
        &SystemInfo {
            local_address: local_address(&state, request.headers()),
            server_name: settings.server_name.clone(),
            version: settings.server_version.clone(),
            product_name: "Jellyfin Server".to_owned(),
            operating_system: String::new(),
            id: server_id(),
            startup_wizard_completed: true,
            has_pending_restart: false,
            is_shutting_down: false,
            supports_library_monitor: true,
        },
    )
}

/// QuickConnect is unsupported: the literal `false` (v2 `_handle` lambda).
pub(super) async fn quick_connect<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    raw_json(StatusCode::OK, "false")
}

pub(super) async fn sessions_logout<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    no_content()
}

// ===== Auth / user =====

/// `AuthenticateByName`: the app password echoes verbatim as `AccessToken`
/// with a fresh `SessionInfo` and the full non-null user object
/// (`compat_auth` renders it; bad credentials → 401, never 403).
pub(super) async fn authenticate<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return error(StatusCode::BAD_REQUEST),
    };
    if bytes.is_empty() {
        return error(StatusCode::BAD_REQUEST);
    }
    let login: AuthenticateRequest = match serde_json::from_slice(&bytes) {
        Ok(login) => login,
        Err(_) => return error(StatusCode::BAD_REQUEST),
    };
    let probe = JellyfinRequest {
        authorization: header(&parts.headers, "authorization"),
        emby_authorization: header(&parts.headers, "x-emby-authorization"),
        emby_token: None,
        mediabrowser_token: None,
        query_apikey: None,
        query_api_key: None,
    };
    let client = extract_client(&probe);
    let (device_name, device_id) = extract_device(&probe);
    let user = match authenticate_by_name(
        &state.passwords,
        &login.username,
        &login.pw,
        client.as_deref(),
    )
    .await
    {
        Ok(user) => user,
        Err(denied) => {
            return error(
                StatusCode::from_u16(denied.status()).unwrap_or(StatusCode::UNAUTHORIZED),
            );
        }
    };
    let facts = SessionFacts {
        id: Uuid::new_v4().simple().to_string(),
        client,
        device_name,
        device_id,
        last_activity: builders::utc_now_iso(),
    };
    raw_json(StatusCode::OK, &login_echo_json(&user, &login.pw, &facts))
}

pub(super) async fn users_me<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    if let Some(denied) = gate(&state) {
        return denied;
    }
    let query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    match authed(&state.passwords, &headers, query.as_deref()).await {
        Ok(authed) => json(StatusCode::OK, &authed.principal.user_dto(&server_id())),
        Err(denied) => denied,
    }
}

/// The `{user_id}` path id is ignored; the caller is always returned (v2
/// `_user_dto(u)` parity).
pub(super) async fn user_by_id<S, L, E, P, I>(
    State(state): State<JellyfinState<S, L, E, P, I>>,
    Path(_user_id): Path<String>,
    request: Request<Body>,
) -> Response
where
    S: JellyfinPasswordStore,
    L: LibraryRead,
    E: StreamEngine,
    P: PlaybackSessions,
    I: IdMap,
{
    users_me(State(state), request).await
}
