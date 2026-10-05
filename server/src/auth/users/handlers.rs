//! Thin Axum handlers and route assembly.
//!
//! Every handler answers one route: extract, call the service, render.
//! Status mapping lives in [`UsersError`](super::error::UsersError). Bodies
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
    DisabledJellyfinDirectory, DisabledPlexDirectory, ImportError, import_users,
    list_import_candidates,
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

/// Avatar upload body cap: 8 MiB of JSON holds the 5 MiB image plus base64
/// overhead with headroom; the decoded 5 MiB rule still applies after.
const AVATAR_BODY_CAP: usize = 8 * 1024 * 1024;

/// JSON body extractor that renders failures in the shared envelope.
pub struct ValidJson<T>(pub T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for ValidJson<T> {
    type Rejection = UsersError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(req, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|cause| UsersError::InvalidInput {
                message: format!("Invalid request body: {cause}"),
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
) -> Result<Json<ProfileResponse>, UsersError> {
    services::get_profile(&deps, &ctx).await.map(Json)
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
) -> Result<Json<UserResponse>, UsersError> {
    services::update_display_name(&deps, &ctx, &body.display_name)
        .await
        .map(Json)
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
) -> Result<Json<UserResponse>, UsersError> {
    services::update_username(&deps, &ctx, &body.username)
        .await
        .map(Json)
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
) -> Result<Json<UserResponse>, UsersError> {
    services::update_email(&deps, &ctx, body.email.as_deref())
        .await
        .map(Json)
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
) -> Result<Json<UserResponse>, UsersError> {
    services::change_password(&deps, &ctx, &body.current_password, &body.new_password)
        .await
        .map(Json)
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
) -> Result<Json<UserResponse>, UsersError> {
    services::set_local_password(&deps, &ctx, &body.new_password)
        .await
        .map(Json)
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
) -> Result<Json<UserResponse>, UsersError> {
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
) -> Result<Response, UsersError> {
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
) -> Result<Json<SessionListResponse>, UsersError> {
    services::list_sessions(&deps, &ctx).await.map(Json)
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
) -> Result<(StatusCode, Json<DeviceSessionResponse>), UsersError> {
    services::mint_device_session(&deps, &ctx, &body)
        .await
        .map(|minted| (StatusCode::CREATED, Json(minted)))
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
) -> Result<StatusCode, UsersError> {
    services::revoke_session(&deps, &ctx, &session_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
) -> Result<StatusCode, UsersError> {
    services::logout_all(&deps, &ctx)
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
) -> Result<StatusCode, UsersError> {
    services::reset_password(&deps, &body)
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
) -> Result<Json<super::models::AppPasswordListResponse>, UsersError> {
    services::list_app_passwords(&deps, &ctx.user_id)
        .await
        .map(Json)
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
) -> Result<(StatusCode, Json<AppPasswordCreatedResponse>), UsersError> {
    services::create_app_password(&deps, &ctx.user_id, &body)
        .await
        .map(|created| (StatusCode::CREATED, Json(created)))
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
) -> Result<StatusCode, UsersError> {
    services::revoke_app_password(&deps, &ctx.user_id, &app_password_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
) -> Result<Json<LastFmStatusResponse>, UsersError> {
    services::lastfm_status(&deps, &ctx.user_id).await.map(Json)
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
) -> Result<Json<LastFmConfiguredResponse>, UsersError> {
    services::lastfm_set_credentials(&deps, &ctx.user_id, &body.api_key, &body.shared_secret)
        .await
        .map(|link| {
            Json(LastFmConfiguredResponse {
                configured: link.configured,
                linked: link.username.is_some() && link.session_key_encrypted.is_some(),
            })
        })
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
) -> Result<Json<LastFmTokenResponse>, UsersError> {
    services::lastfm_request_token(&deps, &ctx.user_id)
        .await
        .map(Json)
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
) -> Result<Json<LastFmSessionResponse>, UsersError> {
    services::lastfm_exchange_session(&deps, &ctx.user_id, &body.token)
        .await
        .map(Json)
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
) -> Result<StatusCode, UsersError> {
    services::lastfm_unlink(&deps, &ctx.user_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
) -> Result<Json<AdminUserListResponse>, UsersError> {
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
) -> Result<(StatusCode, Json<UserResponse>), UsersError> {
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
) -> Result<Json<UserResponse>, UsersError> {
    services::admin_get_user(&deps, &user_id).await.map(Json)
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
) -> Result<Json<UserResponse>, UsersError> {
    services::admin_set_role(&deps, &ctx, &user_id, body.role)
        .await
        .map(Json)
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
) -> Result<StatusCode, UsersError> {
    services::admin_delete_user(&deps, &ctx, &user_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
) -> Result<StatusCode, UsersError> {
    services::admin_revoke_user_sessions(&deps, &user_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
) -> Result<(StatusCode, Json<RecoveryCodeResponse>), UsersError> {
    services::admin_mint_recovery_code(&deps, &user_id)
        .await
        .map(|minted| (StatusCode::CREATED, Json(minted)))
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
) -> Result<Json<super::models::AdminAppPasswordListResponse>, UsersError> {
    services::admin_list_app_passwords(&deps).await.map(|rows| {
        Json(super::models::AdminAppPasswordListResponse {
            app_passwords: rows,
        })
    })
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
) -> Result<StatusCode, UsersError> {
    services::admin_revoke_app_password(&deps, &app_password_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
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
) -> Result<Json<ImportCandidateListResponse>, ImportError> {
    list_import_candidates(&deps, &DisabledJellyfinDirectory)
        .await
        .map(Json)
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
) -> Result<Json<ImportCandidateListResponse>, ImportError> {
    list_import_candidates(&deps, &DisabledPlexDirectory)
        .await
        .map(Json)
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
) -> Result<Json<ImportUsersResponse>, ImportError> {
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
    directory.map(Json)
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
