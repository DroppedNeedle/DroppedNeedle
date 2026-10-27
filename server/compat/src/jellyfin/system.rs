//! System info, login and user identity routes.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use droppedneedle_auth::compat_auth::jellyfin::{
    JellyfinRequest, SessionFacts, authenticate_by_name, extract_client, extract_device,
    extract_version, login_echo_json, server_id,
};
use droppedneedle_auth::session::client_ip::ClientIp;
use uuid::Uuid;

use super::builders::{self};
use super::models::{AuthenticateRequest, PublicSystemInfo, SystemInfo};
use super::router::*;

// ===== System / identity =====

pub(super) async fn system_info_public(
    State(state): State<JellyfinState>,
    request: Request<Body>,
) -> Response {
    let settings = state.settings.jellyfin();
    json(
        StatusCode::OK,
        &PublicSystemInfo {
            local_address: local_address(&state, &request),
            server_name: settings.server_name.clone(),
            version: settings.server_version.clone(),
            product_name: "Jellyfin Server".to_owned(),
            operating_system: String::new(),
            id: server_id(),
            startup_wizard_completed: true,
        },
    )
}

pub(super) async fn system_info(
    State(state): State<JellyfinState>,
    _caller: Authed,
    request: Request<Body>,
) -> Response {
    let settings = state.settings.jellyfin();
    json(
        StatusCode::OK,
        &SystemInfo {
            local_address: local_address(&state, &request),
            server_name: settings.server_name.clone(),
            version: settings.server_version.clone(),
            product_name: "Jellyfin Server".to_owned(),
            operating_system: String::new(),
            id: server_id(),
            startup_wizard_completed: true,
            has_pending_restart: false,
            is_shutting_down: false,
            supports_library_monitor: true,
            web_socket_port_number: state.listener.port,
            completed_installations: Vec::new(),
            // Nothing here can restart the app or open a browser, and this
            // API offers no update.
            can_self_restart: false,
            can_launch_web_browser: false,
            program_data_path: String::new(),
            web_path: String::new(),
            items_by_name_path: String::new(),
            cache_path: String::new(),
            log_path: String::new(),
            internal_metadata_path: String::new(),
            cast_receiver_applications: Vec::new(),
            has_update_available: false,
            encoder_location: if settings.ffmpeg_available {
                "System"
            } else {
                "NotFound"
            }
            .to_owned(),
            system_architecture: system_architecture().to_owned(),
            operating_system_display_name: operating_system_display_name().to_owned(),
            transcoding_temp_path: String::new(),
        },
    )
}

/// The operating system's name as Jellyfin shows it.
fn operating_system_display_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
}

/// The build's architecture in Jellyfin's spelling. The four spellings
/// Jellyfin uses are mapped; any other architecture is sent under its Rust
/// name.
fn system_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "X64",
        "aarch64" => "Arm64",
        "x86" => "X86",
        "arm" => "Arm",
        other => other,
    }
}

/// QuickConnect is unsupported: the literal `false` (v2 `_handle` lambda).
pub(super) async fn quick_connect() -> Response {
    raw_json(StatusCode::OK, "false")
}

/// Signed in only, as on a real server. Nothing to end: the token is the
/// app password itself, so signing out one device never revokes it for the
/// others.
pub(super) async fn sessions_logout(_caller: Authed) -> Response {
    no_content()
}

/// The users a sign-in screen may list: none, as every user here is hidden
/// (`IsHidden`).
pub(super) async fn users_public() -> Response {
    raw_json(StatusCode::OK, "[]")
}

// ===== Auth / user =====

/// `AuthenticateByName`: the app password echoes verbatim as `AccessToken`
/// with a fresh `SessionInfo` and the full non-null user object
/// (`compat_auth` renders it; bad credentials → 401, never 403).
pub(super) async fn authenticate(
    State(state): State<JellyfinState>,
    request: Request<Body>,
) -> Response {
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
    let application_version = extract_version(&probe);
    // The address the limits layer resolved for this request through the
    // trusted-proxy rule: the caller's own, sent back only to the caller.
    let remote_end_point = parts
        .extensions
        .get::<ClientIp>()
        .map(|client| client.0.to_string())
        .unwrap_or_default();
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
        signed_in_at: builders::iso_o(Some(unix_now(&state))),
        application_version,
        remote_end_point,
    };
    let policy = state.settings.jellyfin().policy_facts(&user.role);
    raw_json(
        StatusCode::OK,
        &login_echo_json(&user, &login.pw, &facts, policy),
    )
}

/// Now, as unix seconds, by the platform clock.
fn unix_now(state: &JellyfinState) -> f64 {
    state.passwords.clock().now_ms() as f64 / 1000.0
}

pub(super) async fn users_me(State(state): State<JellyfinState>, authed: Authed) -> Response {
    json(
        StatusCode::OK,
        &authed.principal.user_dto(
            &server_id(),
            unix_now(&state),
            state
                .settings
                .jellyfin()
                .policy_facts(&authed.principal.role),
        ),
    )
}

/// The `{user_id}` path id is ignored; the caller is always returned (v2
/// `_user_dto(u)` parity).
pub(super) async fn user_by_id(
    state: State<JellyfinState>,
    Path(_user_id): Path<String>,
    authed: Authed,
) -> Response {
    users_me(state, authed).await
}
