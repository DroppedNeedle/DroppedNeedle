//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract, call the service, render.
//! Status mapping lives here, in the one `From<UsersError> for ApiError`:
//! the services return the domain [`UsersError`] and know nothing of HTTP.

use axum::{
    Json,
    body::{Body, Bytes, to_bytes},
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use utoipa::{IntoParams, ToSchema};

use super::super::federated::users::{PROVIDER_JELLYFIN, PROVIDER_PLEX};
use super::UsersDeps;
use super::error::UsersError;
use super::import::{import_users, list_import_candidates};
use super::models::{
    AdminUserCreate, AdminUserListResponse, AppPasswordCreate, AppPasswordCreatedResponse,
    DeviceSessionMint, DeviceSessionResponse, DisplayNameUpdate, EmailUpdate,
    ImportCandidateListResponse, ImportUsersRequest, ImportUsersResponse, LastFmConfiguredResponse,
    LastFmCredentialsSet, LastFmSessionExchange, LastFmSessionResponse, LastFmStatusResponse,
    LastFmTokenResponse, LocalPasswordSet, PasswordChange, PasswordReset, ProfileResponse,
    RecoveryCodeResponse, RoleUpdate, SessionListResponse, UserResponse, UsernameUpdate,
};
use super::roles::{Admin, Role, User};
use super::services;
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};

/// Avatar upload body cap: 8 MiB of JSON holds the 5 MiB image plus base64
/// overhead with headroom; the decoded 5 MiB rule still applies after.
const AVATAR_BODY_CAP: usize = 8 * 1024 * 1024;

impl From<UsersError> for ApiError {
    fn from(error: UsersError) -> Self {
        match error {
            UsersError::Unauthorized { message } => ApiError::unauthorized(message),
            UsersError::Forbidden { message } => ApiError::forbidden(message),
            UsersError::NotFound => ApiError::not_found(),
            UsersError::InvalidInput { message } => ApiError::invalid_input(message),
            UsersError::Conflict { message } => ApiError::conflict(message),
            UsersError::TooLarge => ApiError::too_large(),
            UsersError::Unavailable { cause } => {
                ApiError::fault(StatusCode::SERVICE_UNAVAILABLE, cause)
            }
            UsersError::Upstream { cause } => ApiError::fault(StatusCode::BAD_GATEWAY, cause),
            UsersError::Busy => ApiError::unavailable(Unavailable::ServerBusy, "users store busy"),
            UsersError::Internal { cause } => ApiError::internal(cause),
        }
    }
}

/// Profile of the caller.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/me",
    responses((status = 200, description = "Own profile", body = ProfileResponse))
)]
pub async fn get_profile(
    State(deps): State<UsersDeps>,
    User(caller): User,
) -> Result<Json<ProfileResponse>, ApiError> {
    services::get_profile(&deps, &caller)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Change the caller's display name.
#[utoipa::path(
    patch,
    tag = "users",
    path = "/api/v3/me",
    request_body = DisplayNameUpdate,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn patch_profile(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<DisplayNameUpdate>,
) -> Result<Json<UserResponse>, ApiError> {
    services::update_display_name(&deps, &caller, &body.display_name)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Rename the caller.
#[utoipa::path(
    put,
    tag = "users",
    path = "/api/v3/me/username",
    request_body = UsernameUpdate,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn put_username(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<UsernameUpdate>,
) -> Result<Json<UserResponse>, ApiError> {
    services::update_username(&deps, &caller, &body.username)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Set or clear the caller's email.
#[utoipa::path(
    put,
    tag = "users",
    path = "/api/v3/me/email",
    request_body = EmailUpdate,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn put_email(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<EmailUpdate>,
) -> Result<Json<UserResponse>, ApiError> {
    services::update_email(&deps, &caller, body.email.as_deref())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Change the caller's password.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/me/password",
    request_body = PasswordChange,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn post_password(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<PasswordChange>,
) -> Result<Json<UserResponse>, ApiError> {
    services::change_password(&deps, &caller, &body.current_password, &body.new_password)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Set the first local password on an account without one.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/me/local-password",
    request_body = LocalPasswordSet,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn post_local_password(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<LocalPasswordSet>,
) -> Result<Json<UserResponse>, ApiError> {
    services::set_local_password(&deps, &caller, &body.new_password)
        .await
        .map(Json)
        .map_err(ApiError::from)
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
    tag = "users",
    path = "/api/v3/me/avatar",
    request_body = AvatarUploadBody,
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn post_avatar(
    State(deps): State<UsersDeps>,
    User(caller): User,
    body: Body,
) -> Result<Json<UserResponse>, ApiError> {
    let bytes: Bytes = to_bytes(body, AVATAR_BODY_CAP)
        .await
        .map_err(|_| UsersError::TooLarge)?;
    let parsed: AvatarUploadBody =
        serde_json::from_slice(&bytes).map_err(|cause| UsersError::InvalidInput {
            message: format!("Invalid request body: {cause}"),
        })?;
    services::upload_avatar(&deps, &caller, &parsed.content_type, &parsed.image_base64)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Serve one user's avatar. Self-or-admin; the `v` query is ignored.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/users/{id}/avatar",
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Avatar bytes"))
)]
pub async fn get_avatar(
    State(deps): State<UsersDeps>,
    User(caller): User,
    Path(user_id): Path<String>,
) -> Result<Response, ApiError> {
    let (bytes, content_type) = services::get_avatar(&deps, &caller, &user_id).await?;
    let mime = content_type
        .parse()
        .map_err(|_| UsersError::internal(&"stored avatar type is not a header value"))?;
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
    tag = "users",
    path = "/api/v3/auth/sessions",
    responses((status = 200, description = "Own sessions", body = SessionListResponse))
)]
pub async fn list_sessions(
    State(deps): State<UsersDeps>,
    User(caller): User,
) -> Result<Json<SessionListResponse>, ApiError> {
    services::list_sessions(&deps, &caller)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Mint a named companion Bearer token. Standard sessions only.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/auth/device-sessions",
    request_body = DeviceSessionMint,
    responses((status = 201, description = "Minted token (raw value shown once)", body = DeviceSessionResponse))
)]
pub async fn mint_device_session(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<DeviceSessionMint>,
) -> Result<(StatusCode, Json<DeviceSessionResponse>), ApiError> {
    services::mint_device_session(&deps, &caller, &body)
        .await
        .map(|minted| (StatusCode::CREATED, Json(minted)))
        .map_err(ApiError::from)
}

/// Revoke one own session.
#[utoipa::path(
    delete,
    tag = "users",
    path = "/api/v3/auth/sessions/{id}",
    params(("id" = String, Path, description = "Session id")),
    responses((status = 204, description = "Revoked"))
)]
pub async fn revoke_session(
    State(deps): State<UsersDeps>,
    User(caller): User,
    Path(session_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    services::revoke_session(&deps, &caller, &session_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Revoke every own session.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/auth/logout-all",
    responses((status = 204, description = "All own sessions revoked"))
)]
pub async fn logout_all(
    State(deps): State<UsersDeps>,
    User(caller): User,
) -> Result<StatusCode, ApiError> {
    services::logout_all(&deps, &caller)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Reset a password with a recovery code. Public: a locked-out user holds
/// no session.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/auth/password-recovery/reset",
    request_body = PasswordReset,
    responses((status = 204, description = "Password reset"))
)]
pub async fn reset_password(
    State(deps): State<UsersDeps>,
    ValidJson(body): ValidJson<PasswordReset>,
) -> Result<StatusCode, ApiError> {
    services::reset_password(&deps, &body)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Caller's app passwords.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/me/app-passwords",
    responses((status = 200, description = "Own app passwords", body = super::models::AppPasswordListResponse))
)]
pub async fn list_app_passwords(
    State(deps): State<UsersDeps>,
    User(caller): User,
) -> Result<Json<super::models::AppPasswordListResponse>, ApiError> {
    services::list_app_passwords(&deps, caller.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Create an app password. The secret returns once.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/me/app-passwords",
    request_body = AppPasswordCreate,
    responses((status = 201, description = "Created (secret shown once)", body = AppPasswordCreatedResponse))
)]
pub async fn create_app_password(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<AppPasswordCreate>,
) -> Result<(StatusCode, Json<AppPasswordCreatedResponse>), ApiError> {
    services::create_app_password(&deps, &caller, &body)
        .await
        .map(|created| (StatusCode::CREATED, Json(created)))
        .map_err(ApiError::from)
}

/// Revoke one own app password.
#[utoipa::path(
    delete,
    tag = "users",
    path = "/api/v3/me/app-passwords/{id}",
    params(("id" = String, Path, description = "App-password id")),
    responses((status = 204, description = "Revoked"))
)]
pub async fn revoke_app_password(
    State(deps): State<UsersDeps>,
    User(caller): User,
    Path(app_password_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    services::revoke_app_password(&deps, caller.user_id(), &app_password_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Per-user Last.fm link status.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/me/connections/lastfm",
    responses((status = 200, description = "Link status", body = LastFmStatusResponse))
)]
pub async fn lastfm_status(
    State(deps): State<UsersDeps>,
    User(caller): User,
) -> Result<Json<LastFmStatusResponse>, ApiError> {
    services::lastfm_status(&deps, caller.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Store the caller's own Last.fm API credentials.
#[utoipa::path(
    put,
    tag = "users",
    path = "/api/v3/me/connections/lastfm",
    request_body = LastFmCredentialsSet,
    responses((status = 200, description = "Stored", body = LastFmConfiguredResponse))
)]
pub async fn lastfm_set_credentials(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<LastFmCredentialsSet>,
) -> Result<Json<LastFmConfiguredResponse>, ApiError> {
    services::lastfm_set_credentials(&deps, caller.user_id(), &body.api_key, &body.shared_secret)
        .await
        .map(|link| {
            Json(LastFmConfiguredResponse {
                configured: link.configured,
                linked: link.username.is_some() && link.session_key_encrypted.is_some(),
            })
        })
        .map_err(ApiError::from)
}

/// Fetch a Last.fm sign-in token plus approval URL.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/me/connections/lastfm/token",
    responses((status = 200, description = "Token and approval URL", body = LastFmTokenResponse))
)]
pub async fn lastfm_token(
    State(deps): State<UsersDeps>,
    User(caller): User,
) -> Result<Json<LastFmTokenResponse>, ApiError> {
    services::lastfm_request_token(&deps, caller.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Exchange an approved token for a linked session.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/me/connections/lastfm/session",
    request_body = LastFmSessionExchange,
    responses((status = 200, description = "Linked session", body = LastFmSessionResponse))
)]
pub async fn lastfm_session(
    State(deps): State<UsersDeps>,
    User(caller): User,
    ValidJson(body): ValidJson<LastFmSessionExchange>,
) -> Result<Json<LastFmSessionResponse>, ApiError> {
    services::lastfm_exchange_session(&deps, caller.user_id(), &body.token)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Unlink Last.fm (drops credentials and session).
#[utoipa::path(
    delete,
    tag = "users",
    path = "/api/v3/me/connections/lastfm",
    responses((status = 204, description = "Unlinked"))
)]
pub async fn lastfm_unlink(
    State(deps): State<UsersDeps>,
    User(caller): User,
) -> Result<StatusCode, ApiError> {
    services::lastfm_unlink(&deps, caller.user_id())
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Paging for the admin user list. A value that is not a whole number is
/// ignored, the same as leaving it out.
#[derive(Debug, serde::Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AdminUsersQuery {
    /// Rows per page, 1 to 100 (default 100).
    #[serde(default, deserialize_with = "lenient_u64")]
    #[param(value_type = Option<u64>)]
    pub limit: Option<u64>,
    /// Rows to skip (default 0).
    #[serde(default, deserialize_with = "lenient_u64")]
    #[param(value_type = Option<u64>)]
    pub offset: Option<u64>,
}

fn lenient_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<String> = serde::Deserialize::deserialize(deserializer)?;
    Ok(value.and_then(|value| value.parse::<u64>().ok()))
}

/// Admin user listing, paged.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/admin/users",
    params(AdminUsersQuery),
    responses((status = 200, description = "User page", body = AdminUserListResponse))
)]
pub async fn admin_list_users(
    State(deps): State<UsersDeps>,
    admin: Admin,
    ValidQuery(query): ValidQuery<AdminUsersQuery>,
) -> Result<Json<AdminUserListResponse>, ApiError> {
    let (limit, offset) = services::clamp_admin_page(query.limit, query.offset);
    services::admin_list_users(&deps, &admin, limit, offset)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Create a local account.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/admin/users",
    request_body = AdminUserCreate,
    responses((status = 201, description = "Created account", body = UserResponse))
)]
pub async fn admin_create_user(
    State(deps): State<UsersDeps>,
    admin: Admin,
    ValidJson(body): ValidJson<AdminUserCreate>,
) -> Result<(StatusCode, Json<UserResponse>), ApiError> {
    services::admin_create_user(
        &deps,
        &admin,
        &body.username,
        &body.password,
        body.display_name.as_deref(),
        body.email.as_deref(),
        body.role.unwrap_or(Role::User),
    )
    .await
    .map(|user| (StatusCode::CREATED, Json(user)))
    .map_err(ApiError::from)
}

/// Fetch one user.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/admin/users/{id}",
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "The account", body = UserResponse))
)]
pub async fn admin_get_user(
    State(deps): State<UsersDeps>,
    admin: Admin,
    Path(user_id): Path<String>,
) -> Result<Json<UserResponse>, ApiError> {
    services::admin_get_user(&deps, &admin, &user_id)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Change one user's role.
#[utoipa::path(
    put,
    tag = "users",
    path = "/api/v3/admin/users/{id}/role",
    request_body = RoleUpdate,
    params(("id" = String, Path, description = "User id")),
    responses((status = 200, description = "Updated account", body = UserResponse))
)]
pub async fn admin_set_role(
    State(deps): State<UsersDeps>,
    admin: Admin,
    Path(user_id): Path<String>,
    ValidJson(body): ValidJson<RoleUpdate>,
) -> Result<Json<UserResponse>, ApiError> {
    services::admin_set_role(&deps, &admin, &user_id, body.role)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Delete one user.
#[utoipa::path(
    delete,
    tag = "users",
    path = "/api/v3/admin/users/{id}",
    params(("id" = String, Path, description = "User id")),
    responses((status = 204, description = "Deleted"))
)]
pub async fn admin_delete_user(
    State(deps): State<UsersDeps>,
    admin: Admin,
    Path(user_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    services::admin_delete_user(&deps, &admin, &user_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Revoke every session of one user.
#[utoipa::path(
    delete,
    tag = "users",
    path = "/api/v3/admin/users/{id}/sessions",
    params(("id" = String, Path, description = "User id")),
    responses((status = 204, description = "Sessions revoked"))
)]
pub async fn admin_revoke_sessions(
    State(deps): State<UsersDeps>,
    admin: Admin,
    Path(user_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    services::admin_revoke_user_sessions(&deps, &admin, &user_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// Mint a recovery code for one user. The code returns once.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/admin/users/{id}/recovery-code",
    params(("id" = String, Path, description = "User id")),
    responses((status = 201, description = "Minted code (shown once)", body = RecoveryCodeResponse))
)]
pub async fn admin_mint_recovery_code(
    State(deps): State<UsersDeps>,
    admin: Admin,
    Path(user_id): Path<String>,
) -> Result<(StatusCode, Json<RecoveryCodeResponse>), ApiError> {
    services::admin_mint_recovery_code(&deps, &admin, &user_id)
        .await
        .map(|minted| (StatusCode::CREATED, Json(minted)))
        .map_err(ApiError::from)
}

/// Every live app password across users, with owners.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/admin/app-passwords",
    responses((status = 200, description = "All live app passwords", body = super::models::AdminAppPasswordListResponse))
)]
pub async fn admin_list_app_passwords(
    State(deps): State<UsersDeps>,
    admin: Admin,
) -> Result<Json<super::models::AdminAppPasswordListResponse>, ApiError> {
    services::admin_list_app_passwords(&deps, &admin)
        .await
        .map(|rows| {
            Json(super::models::AdminAppPasswordListResponse {
                app_passwords: rows,
            })
        })
        .map_err(ApiError::from)
}

/// Revoke any user's app password.
#[utoipa::path(
    delete,
    tag = "users",
    path = "/api/v3/admin/app-passwords/{id}",
    params(("id" = String, Path, description = "App-password id")),
    responses((status = 204, description = "Revoked"))
)]
pub async fn admin_revoke_app_password(
    State(deps): State<UsersDeps>,
    admin: Admin,
    Path(app_password_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    services::admin_revoke_app_password(&deps, &admin, &app_password_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(ApiError::from)
}

/// List Jellyfin accounts available for import. 503 when Jellyfin is off,
/// has no API key, or cannot be reached.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/admin/import/jellyfin",
    responses(
        (status = 200, description = "Importable accounts", body = ImportCandidateListResponse),
        (status = 503, description = "Jellyfin is not set up or unreachable")
    )
)]
pub async fn admin_import_list_jellyfin(
    State(deps): State<UsersDeps>,
    admin: Admin,
) -> Result<Json<ImportCandidateListResponse>, ApiError> {
    list_import_candidates(&deps, &admin, deps.jellyfin_directory.as_ref())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// List Plex accounts (Plex Home users and friends) available for import.
/// 503 when no Plex token is set up or plex.tv cannot be reached.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/admin/import/plex",
    responses(
        (status = 200, description = "Importable accounts", body = ImportCandidateListResponse),
        (status = 503, description = "Plex is not set up or unreachable")
    )
)]
pub async fn admin_import_list_plex(
    State(deps): State<UsersDeps>,
    admin: Admin,
) -> Result<Json<ImportCandidateListResponse>, ApiError> {
    list_import_candidates(&deps, &admin, deps.plex_directory.as_ref())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Import a batch of accounts from one provider. The catalog is re-read
/// server-side; unknown, already-bound, and failed uids land in `skipped`.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/admin/import",
    request_body = ImportUsersRequest,
    responses((status = 200, description = "Finished batch", body = ImportUsersResponse))
)]
pub async fn admin_import_users(
    State(deps): State<UsersDeps>,
    admin: Admin,
    ValidJson(body): ValidJson<ImportUsersRequest>,
) -> Result<Json<ImportUsersResponse>, ApiError> {
    let directory = match body.provider.as_str() {
        PROVIDER_JELLYFIN => deps.jellyfin_directory.clone(),
        PROVIDER_PLEX => deps.plex_directory.clone(),
        _ => {
            return Err(UsersError::InvalidInput {
                message: "Unsupported import provider".to_owned(),
            }
            .into());
        }
    };
    import_users(&deps, &admin, directory.as_ref(), &body.provider_uids)
        .await
        .map(Json)
        .map_err(ApiError::from)
}
