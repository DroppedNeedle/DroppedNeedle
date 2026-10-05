//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract, call the service, render.
//! Status mapping lives here, in [`UsersHttpError`]: the services return
//! the domain [`UsersError`] and know nothing of HTTP. Bodies
//! parse through [`ValidJson`], which keeps malformed input inside the
//! shared error envelope instead of Axum's default plain-text 400.

use axum::{
    Json,
    body::{Body, Bytes, to_bytes},
    extract::{FromRequest, Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use utoipa::ToSchema;

use super::super::federated::users::{PROVIDER_JELLYFIN, PROVIDER_PLEX};
use super::UsersDeps;
use super::error::UsersError;
use super::import::{
    DisabledJellyfinDirectory, DisabledPlexDirectory, import_users, list_import_candidates,
};
use super::models::{
    AdminUserCreate, AdminUserListResponse, AppPasswordCreate, AppPasswordCreatedResponse,
    DeviceSessionMint, DeviceSessionResponse, DisplayNameUpdate, EmailUpdate,
    ImportCandidateListResponse, ImportUsersRequest, ImportUsersResponse, LastFmConfiguredResponse,
    LastFmCredentialsSet, LastFmSessionExchange, LastFmSessionResponse, LastFmStatusResponse,
    LastFmTokenResponse, LocalPasswordSet, PasswordChange, PasswordReset, ProfileResponse,
    RecoveryCodeResponse, RoleUpdate, SessionListResponse, UserResponse, UsernameUpdate,
};
use super::roles::{CurrentAdmin, CurrentUser, Role};
use super::services;
use crate::error::{
    CONFLICT, FIXED_INTERNAL_MESSAGE, FIXED_TOO_LARGE_MESSAGE, FIXED_UPSTREAM_MESSAGE, FORBIDDEN,
    INTERNAL_ERROR, INVALID_INPUT, NOT_FOUND, NOT_FOUND_MESSAGE, PAYLOAD_TOO_LARGE, UPSTREAM_ERROR,
    envelope_response, fault_response, unauthorized_response,
};

/// Avatar upload body cap: 8 MiB of JSON holds the 5 MiB image plus base64
/// overhead with headroom; the decoded 5 MiB rule still applies after.
const AVATAR_BODY_CAP: usize = 8 * 1024 * 1024;

/// A users-domain failure on its way to the wire: the one place users
/// errors get an HTTP status and envelope.
#[derive(Debug)]
pub struct UsersHttpError(pub UsersError);

impl From<UsersError> for UsersHttpError {
    fn from(error: UsersError) -> Self {
        Self(error)
    }
}

impl IntoResponse for UsersHttpError {
    fn into_response(self) -> Response {
        match self.0 {
            UsersError::Unauthorized { message } => unauthorized_response(message),
            UsersError::Forbidden { message } => {
                envelope_response(StatusCode::FORBIDDEN, FORBIDDEN, message, None)
            }
            UsersError::NotFound => {
                envelope_response(StatusCode::NOT_FOUND, NOT_FOUND, NOT_FOUND_MESSAGE, None)
            }
            UsersError::InvalidInput { message } => {
                envelope_response(StatusCode::BAD_REQUEST, INVALID_INPUT, message, None)
            }
            UsersError::Conflict { message } => {
                envelope_response(StatusCode::CONFLICT, CONFLICT, message, None)
            }
            UsersError::TooLarge => envelope_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                PAYLOAD_TOO_LARGE,
                FIXED_TOO_LARGE_MESSAGE,
                None,
            ),
            UsersError::Unavailable { error_id } => fault_response(
                StatusCode::SERVICE_UNAVAILABLE,
                UPSTREAM_ERROR,
                FIXED_UPSTREAM_MESSAGE,
                &error_id,
            ),
            UsersError::Upstream { error_id } => fault_response(
                StatusCode::BAD_GATEWAY,
                UPSTREAM_ERROR,
                FIXED_UPSTREAM_MESSAGE,
                &error_id,
            ),
            UsersError::Internal { error_id } => fault_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                INTERNAL_ERROR,
                FIXED_INTERNAL_MESSAGE,
                &error_id,
            ),
        }
    }
}

/// JSON body extractor that renders failures in the shared envelope.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = UsersHttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| {
                UsersHttpError(UsersError::InvalidInput {
                    message: format!("Invalid request body: {cause}"),
                })
            })
    }
}

/// Profile of the caller.
#[utoipa::path(
    get,
    path = "/api/v3/me",
    responses((status = 200, description = "Own profile", body = ProfileResponse))
)]
pub async fn get_profile(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
) -> Result<Json<ProfileResponse>, UsersHttpError> {
    services::get_profile(&deps, &ctx)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Change the caller's display name.
#[utoipa::path(
    patch,
    path = "/api/v3/me",
    request_body = DisplayNameUpdate,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn patch_profile(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<DisplayNameUpdate>,
) -> Result<Json<UserResponse>, UsersHttpError> {
    services::update_display_name(&deps, &ctx, &body.display_name)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Rename the caller.
#[utoipa::path(
    put,
    path = "/api/v3/me/username",
    request_body = UsernameUpdate,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn put_username(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<UsernameUpdate>,
) -> Result<Json<UserResponse>, UsersHttpError> {
    services::update_username(&deps, &ctx, &body.username)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Set or clear the caller's email.
#[utoipa::path(
    put,
    path = "/api/v3/me/email",
    request_body = EmailUpdate,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn put_email(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<EmailUpdate>,
) -> Result<Json<UserResponse>, UsersHttpError> {
    services::update_email(&deps, &ctx, body.email.as_deref())
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Change the caller's password.
#[utoipa::path(
    post,
    path = "/api/v3/me/password",
    request_body = PasswordChange,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn post_password(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<PasswordChange>,
) -> Result<Json<UserResponse>, UsersHttpError> {
    services::change_password(&deps, &ctx, &body.current_password, &body.new_password)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Set the first local password on an account without one.
#[utoipa::path(
    post,
    path = "/api/v3/me/local-password",
    request_body = LocalPasswordSet,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn post_local_password(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<LocalPasswordSet>,
) -> Result<Json<UserResponse>, UsersHttpError> {
    services::set_local_password(&deps, &ctx, &body.new_password)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Avatar upload payload (parsed manually so the body cap applies).
#[derive(Debug, serde::Deserialize, ToSchema)]
struct AvatarUploadBody {
    /// Image content type.
    content_type: String,
    /// Base64 image bytes.
    image_base64: String,
}

/// Upload a new avatar from base64 bytes.
#[utoipa::path(
    post,
    path = "/api/v3/me/avatar",
    request_body = AvatarUploadBody,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn post_avatar(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    body: Body,
) -> Result<Json<UserResponse>, UsersHttpError> {
    let bytes: Bytes = to_bytes(body, AVATAR_BODY_CAP)
        .await
        .map_err(|_| UsersError::TooLarge)?;
    let parsed: AvatarUploadBody =
        serde_json::from_slice(&bytes).map_err(|cause| UsersError::InvalidInput {
            message: format!("Invalid request body: {cause}"),
        })?;
    services::upload_avatar(&deps, &ctx, &parsed.content_type, &parsed.image_base64)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Serve one user's avatar. Self-or-admin; the `v` query is ignored.
#[utoipa::path(
    get,
    path = "/api/v3/users/{id}/avatar",
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Avatar bytes"))
)]
pub async fn get_avatar(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    Path(user_id): Path<String>,
) -> Result<Response, UsersHttpError> {
    let (bytes, content_type) = services::get_avatar(&deps, &ctx, &user_id).await?;
    let mime = content_type.parse().map_err(|_| {
        UsersError::internal(
            &"stored avatar type is not a header value",
            deps.ids.as_ref(),
        )
    })?;
    Ok((
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("private, max-age=3600"),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// List the caller's sessions for the session-list UI.
#[utoipa::path(
    get,
    path = "/api/v3/auth/sessions",
    responses((status = 200, description = "Own sessions", body = SessionListResponse))
)]
pub async fn list_sessions(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
) -> Result<Json<SessionListResponse>, UsersHttpError> {
    services::list_sessions(&deps, &ctx)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Mint a named companion Bearer token. Standard sessions only.
#[utoipa::path(
    post,
    path = "/api/v3/auth/device-sessions",
    request_body = DeviceSessionMint,
    responses((status = 201, description = "Minted token (raw value shown once)", body = DeviceSessionResponse))
)]
pub async fn mint_device_session(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<DeviceSessionMint>,
) -> Result<(StatusCode, Json<DeviceSessionResponse>), UsersHttpError> {
    services::mint_device_session(&deps, &ctx, &body)
        .await
        .map(|minted| (StatusCode::CREATED, Json(minted)))
        .map_err(UsersHttpError::from)
}

/// Revoke one own session.
#[utoipa::path(
    delete,
    path = "/api/v3/auth/sessions/{id}",
    params(("id" = String, Path, description = "Session id")),
    responses((status = 204, description = "Revoked"))
)]
pub async fn revoke_session(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    Path(session_id): Path<String>,
) -> Result<StatusCode, UsersHttpError> {
    services::revoke_session(&deps, &ctx, &session_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(UsersHttpError::from)
}

/// Revoke every own session.
#[utoipa::path(
    post,
    path = "/api/v3/auth/logout-all",
    responses((status = 204, description = "All own sessions revoked"))
)]
pub async fn logout_all(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
) -> Result<StatusCode, UsersHttpError> {
    services::logout_all(&deps, &ctx)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(UsersHttpError::from)
}

/// Reset a password with a recovery code. Public: a locked-out user holds
/// no session.
#[utoipa::path(
    post,
    path = "/api/v3/auth/password-recovery/reset",
    request_body = PasswordReset,
    responses((status = 204, description = "Password reset"))
)]
pub async fn reset_password(
    State(deps): State<UsersDeps>,
    ValidJson(body): ValidJson<PasswordReset>,
) -> Result<StatusCode, UsersHttpError> {
    services::reset_password(&deps, &body)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(UsersHttpError::from)
}

/// Caller's app passwords.
#[utoipa::path(
    get,
    path = "/api/v3/me/app-passwords",
    responses((status = 200, description = "Own app passwords", body = super::models::AppPasswordListResponse))
)]
pub async fn list_app_passwords(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
) -> Result<Json<super::models::AppPasswordListResponse>, UsersHttpError> {
    services::list_app_passwords(&deps, &ctx.user_id)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Create an app password. The secret returns once.
#[utoipa::path(
    post,
    path = "/api/v3/me/app-passwords",
    request_body = AppPasswordCreate,
    responses((status = 201, description = "Created (secret shown once)", body = AppPasswordCreatedResponse))
)]
pub async fn create_app_password(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<AppPasswordCreate>,
) -> Result<(StatusCode, Json<AppPasswordCreatedResponse>), UsersHttpError> {
    services::create_app_password(&deps, &ctx.user_id, &body)
        .await
        .map(|created| (StatusCode::CREATED, Json(created)))
        .map_err(UsersHttpError::from)
}

/// Revoke one own app password.
#[utoipa::path(
    delete,
    path = "/api/v3/me/app-passwords/{id}",
    params(("id" = String, Path, description = "App-password id")),
    responses((status = 204, description = "Revoked"))
)]
pub async fn revoke_app_password(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    Path(app_password_id): Path<String>,
) -> Result<StatusCode, UsersHttpError> {
    services::revoke_app_password(&deps, &ctx.user_id, &app_password_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(UsersHttpError::from)
}

/// Per-user Last.fm link status.
#[utoipa::path(
    get,
    path = "/api/v3/me/connections/lastfm",
    responses((status = 200, description = "Link status", body = LastFmStatusResponse))
)]
pub async fn lastfm_status(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
) -> Result<Json<LastFmStatusResponse>, UsersHttpError> {
    services::lastfm_status(&deps, &ctx.user_id)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Store the caller's own Last.fm API credentials.
#[utoipa::path(
    put,
    path = "/api/v3/me/connections/lastfm",
    request_body = LastFmCredentialsSet,
    responses((status = 200, description = "Stored", body = LastFmConfiguredResponse))
)]
pub async fn lastfm_set_credentials(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<LastFmCredentialsSet>,
) -> Result<Json<LastFmConfiguredResponse>, UsersHttpError> {
    services::lastfm_set_credentials(&deps, &ctx.user_id, &body.api_key, &body.shared_secret)
        .await
        .map(|link| {
            Json(LastFmConfiguredResponse {
                configured: link.configured,
                linked: link.username.is_some() && link.session_key_encrypted.is_some(),
            })
        })
        .map_err(UsersHttpError::from)
}

/// Fetch a Last.fm sign-in token plus approval URL.
#[utoipa::path(
    post,
    path = "/api/v3/me/connections/lastfm/token",
    responses((status = 200, description = "Token and approval URL", body = LastFmTokenResponse))
)]
pub async fn lastfm_token(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
) -> Result<Json<LastFmTokenResponse>, UsersHttpError> {
    services::lastfm_request_token(&deps, &ctx.user_id)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Exchange an approved token for a linked session.
#[utoipa::path(
    post,
    path = "/api/v3/me/connections/lastfm/session",
    request_body = LastFmSessionExchange,
    responses((status = 200, description = "Linked session", body = LastFmSessionResponse))
)]
pub async fn lastfm_session(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
    ValidJson(body): ValidJson<LastFmSessionExchange>,
) -> Result<Json<LastFmSessionResponse>, UsersHttpError> {
    services::lastfm_exchange_session(&deps, &ctx.user_id, &body.token)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Unlink Last.fm (drops credentials and session).
#[utoipa::path(
    delete,
    path = "/api/v3/me/connections/lastfm",
    responses((status = 204, description = "Unlinked"))
)]
pub async fn lastfm_unlink(
    State(deps): State<UsersDeps>,
    CurrentUser(ctx): CurrentUser,
) -> Result<StatusCode, UsersHttpError> {
    services::lastfm_unlink(&deps, &ctx.user_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(UsersHttpError::from)
}

/// Admin user listing, paged.
#[utoipa::path(
    get,
    path = "/api/v3/admin/users",
    responses((status = 200, description = "User page", body = AdminUserListResponse))
)]
pub async fn admin_list_users(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<AdminUserListResponse>, UsersHttpError> {
    let limit = query
        .get("limit")
        .and_then(|value| value.parse::<u64>().ok());
    let offset = query
        .get("offset")
        .and_then(|value| value.parse::<u64>().ok());
    let (limit, offset) = services::clamp_admin_page(limit, offset);
    services::admin_list_users(&deps, limit, offset)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Create a local account.
#[utoipa::path(
    post,
    path = "/api/v3/admin/users",
    request_body = AdminUserCreate,
    responses((status = 201, description = "Created account", body = UserResponse))
)]
pub async fn admin_create_user(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
    ValidJson(body): ValidJson<AdminUserCreate>,
) -> Result<(StatusCode, Json<UserResponse>), UsersHttpError> {
    services::admin_create_user(
        &deps,
        &body.username,
        &body.password,
        body.display_name.as_deref(),
        body.email.as_deref(),
        body.role.unwrap_or(Role::User),
    )
    .await
    .map(|user| (StatusCode::CREATED, Json(user)))
    .map_err(UsersHttpError::from)
}

/// Fetch one user.
#[utoipa::path(
    get,
    path = "/api/v3/admin/users/{id}",
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "The account", body = UserResponse))
)]
pub async fn admin_get_user(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
    Path(user_id): Path<String>,
) -> Result<Json<UserResponse>, UsersHttpError> {
    services::admin_get_user(&deps, &user_id)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Change one user's role.
#[utoipa::path(
    put,
    path = "/api/v3/admin/users/{id}/role",
    request_body = RoleUpdate,
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn admin_set_role(
    State(deps): State<UsersDeps>,
    CurrentAdmin(ctx): CurrentAdmin,
    Path(user_id): Path<String>,
    ValidJson(body): ValidJson<RoleUpdate>,
) -> Result<Json<UserResponse>, UsersHttpError> {
    services::admin_set_role(&deps, &ctx, &user_id, body.role)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Delete one user.
#[utoipa::path(
    delete,
    path = "/api/v3/admin/users/{id}",
    params(("id" = String, Path, description = "User id")),
    responses((status = 204, description = "Deleted"))
)]
pub async fn admin_delete_user(
    State(deps): State<UsersDeps>,
    CurrentAdmin(ctx): CurrentAdmin,
    Path(user_id): Path<String>,
) -> Result<StatusCode, UsersHttpError> {
    services::admin_delete_user(&deps, &ctx, &user_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(UsersHttpError::from)
}

/// Revoke every session of one user.
#[utoipa::path(
    delete,
    path = "/api/v3/admin/users/{id}/sessions",
    params(("id" = String, Path, description = "User id")),
    responses((status = 204, description = "Sessions revoked"))
)]
pub async fn admin_revoke_sessions(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
    Path(user_id): Path<String>,
) -> Result<StatusCode, UsersHttpError> {
    services::admin_revoke_user_sessions(&deps, &user_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(UsersHttpError::from)
}

/// Mint a recovery code for one user. The code returns once.
#[utoipa::path(
    post,
    path = "/api/v3/admin/users/{id}/recovery-code",
    params(("id" = String, Path, description = "User id")),
    responses((status = 201, description = "Minted code (shown once)", body = RecoveryCodeResponse))
)]
pub async fn admin_mint_recovery_code(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
    Path(user_id): Path<String>,
) -> Result<(StatusCode, Json<RecoveryCodeResponse>), UsersHttpError> {
    services::admin_mint_recovery_code(&deps, &user_id)
        .await
        .map(|minted| (StatusCode::CREATED, Json(minted)))
        .map_err(UsersHttpError::from)
}

/// Every live app password across users, with owners.
#[utoipa::path(
    get,
    path = "/api/v3/admin/app-passwords",
    responses((status = 200, description = "All live app passwords", body = super::models::AdminAppPasswordListResponse))
)]
pub async fn admin_list_app_passwords(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
) -> Result<Json<super::models::AdminAppPasswordListResponse>, UsersHttpError> {
    services::admin_list_app_passwords(&deps)
        .await
        .map(|rows| {
            Json(super::models::AdminAppPasswordListResponse {
                app_passwords: rows,
            })
        })
        .map_err(UsersHttpError::from)
}

/// Revoke any user's app password.
#[utoipa::path(
    delete,
    path = "/api/v3/admin/app-passwords/{id}",
    params(("id" = String, Path, description = "App-password id")),
    responses((status = 204, description = "Revoked"))
)]
pub async fn admin_revoke_app_password(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
    Path(app_password_id): Path<String>,
) -> Result<StatusCode, UsersHttpError> {
    services::admin_revoke_app_password(&deps, &app_password_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(UsersHttpError::from)
}

/// List Jellyfin accounts available for import. 503 until a live Jellyfin
/// client exists (same posture as the login flows).
#[utoipa::path(
    get,
    path = "/api/v3/admin/import/jellyfin",
    responses((status = 200, description = "Importable accounts", body = ImportCandidateListResponse))
)]
pub async fn admin_import_list_jellyfin(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
) -> Result<Json<ImportCandidateListResponse>, UsersHttpError> {
    list_import_candidates(&deps, &DisabledJellyfinDirectory)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// List Plex accounts available for import. 503 until a live Plex client
/// exists (same posture as the login flows).
#[utoipa::path(
    get,
    path = "/api/v3/admin/import/plex",
    responses((status = 200, description = "Importable accounts", body = ImportCandidateListResponse))
)]
pub async fn admin_import_list_plex(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
) -> Result<Json<ImportCandidateListResponse>, UsersHttpError> {
    list_import_candidates(&deps, &DisabledPlexDirectory)
        .await
        .map(Json)
        .map_err(UsersHttpError::from)
}

/// Import a batch of accounts from one provider. The catalog is re-read
/// server-side; unknown, already-bound, and failed uids land in `skipped`.
#[utoipa::path(
    post,
    path = "/api/v3/admin/import",
    request_body = ImportUsersRequest,
    responses((status = 200, description = "Finished batch", body = ImportUsersResponse))
)]
pub async fn admin_import_users(
    State(deps): State<UsersDeps>,
    CurrentAdmin(_): CurrentAdmin,
    ValidJson(body): ValidJson<ImportUsersRequest>,
) -> Result<Json<ImportUsersResponse>, UsersHttpError> {
    // Disabled directories until the live clients land: every provider
    // branch 503s below; only the provider name validates here.
    let directory = match body.provider.as_str() {
        PROVIDER_JELLYFIN => {
            import_users(&deps, &DisabledJellyfinDirectory, &body.provider_uids).await
        }
        PROVIDER_PLEX => import_users(&deps, &DisabledPlexDirectory, &body.provider_uids).await,
        _ => {
            return Err(UsersError::InvalidInput {
                message: "Unsupported import provider".to_owned(),
            }
            .into());
        }
    };
    directory.map(Json).map_err(UsersHttpError::from)
}

/// Curator probe body: proves the extractor admitted the caller.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, serde::Serialize, ToSchema)]
pub struct CuratorProbeResponse {
    /// Always true when this response is returned.
    pub curator: bool,
}

/// Curator probe for tests: no production router mounts it; tests mount it
/// to pin `CurrentCurator` behavior per route class.
#[cfg(any(test, feature = "test-support"))]
pub async fn curator_probe(
    super::roles::CurrentCurator(_): super::roles::CurrentCurator,
) -> Json<CuratorProbeResponse> {
    Json(CuratorProbeResponse { curator: true })
}
