//! Domain logic: validation, minting, verification, and orchestration.
//!
//! Services compose stores and return [`UsersError`] directly; handlers stay
//! thin. Nothing here touches HTTP types.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use std::sync::Arc;

use super::super::federated::password_import::HashScheme;
use super::super::session::tokens;
use super::error::UsersError;
use super::models::{
    AdminAppPasswordView, AdminUserListResponse, AppPasswordAuth, AppPasswordCreate,
    AppPasswordCreatedResponse, AppPasswordListResponse, AppPasswordRecord, AppPasswordView,
    DeviceSessionMint, DeviceSessionResponse, LastFmConnection, LastFmSessionResponse,
    LastFmStatusResponse, LastFmTokenResponse, LocalCredential, PasswordReset, ProfileResponse,
    RecoveryCode, RecoveryCodeResponse, SessionListResponse, SessionView, UserRecord, UserResponse,
};
use super::roles::{AuthContext, Role, SessionKind};
use super::stores::{LastFmError, RoleChange, StoreError, UserDeletion};
use super::{UsersDeps, clock_now};

// ---------------------------------------------------------------------------
// Validation and crypto helpers
// ---------------------------------------------------------------------------

/// Minimum password length, kept from v2.
pub const MIN_PASSWORD_LEN: usize = 12;
/// Maximum password size. v2 capped at 72 UTF-8 bytes (a bcrypt ceiling);
/// v3 hashes with Argon2id, so the cap only bounds abuse.
pub const MAX_PASSWORD_BYTES: usize = 512;
/// Companion device label cap, kept from v2.
pub const MAX_DEVICE_LABEL_LEN: usize = 80;
/// Display name cap.
pub const MAX_DISPLAY_NAME_LEN: usize = 64;
/// Avatar image cap: 5 MiB decoded, kept from v2.
pub const MAX_AVATAR_BYTES: usize = 5 * 1024 * 1024;
/// App-password name cap.
pub const MAX_APP_PASSWORD_NAME_LEN: usize = 128;
/// Live app passwords per user, kept from v2.
pub const MAX_ACTIVE_APP_PASSWORDS: u64 = 25;
/// Recovery code lifetime: 15 minutes, kept from v2.
pub const RECOVERY_TTL_SECS: i64 = 15 * 60;
/// Recovery code alphabet (no ambiguous chars), kept from v2.
pub const RECOVERY_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// Recovery code length, kept from v2.
pub const RECOVERY_LEN: usize = 20;
/// Allowed avatar image types, kept from v2.
pub const ALLOWED_AVATAR_TYPES: &[&str] = &["image/jpeg", "image/png", "image/webp", "image/gif"];
/// Upper bound for secrets offered for verification, kept from v2.
pub const MAX_AUTH_VALUE_LEN: usize = 1024;
/// Prefix marking companion rows in the shared token table, kept from v2.
pub const COMPANION_LABEL_PREFIX: &str = "DroppedNeedle companion \u{b7} ";

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Fill a buffer from the OS generator. Failure is a 500, never a fallback.
fn random_bytes<const N: usize>(deps: &UsersDeps) -> Result<[u8; N], UsersError> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|cause| {
        UsersError::internal(
            &format_args!("random generator failed: {cause}"),
            deps.ids.as_ref(),
        )
    })?;
    Ok(buf)
}

/// A fresh opaque secret from the session token mint (32 random bytes,
/// urlsafe base64, v2 format kept byte for byte).
fn fresh_secret(deps: &UsersDeps) -> Result<String, UsersError> {
    tokens::mint_token().map_err(|cause| UsersError::internal(&cause, deps.ids.as_ref()))
}

/// Validate a username: 3-32 chars of letters, digits, `.`, `_`, `-`.
/// Returns (lowercased, display casing).
pub fn validate_username(raw: &str) -> Result<(String, String), UsersError> {
    let candidate = raw.trim();
    let ok_len = (3..=32).contains(&candidate.len());
    let ok_chars = candidate
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if !ok_len || !ok_chars {
        return Err(UsersError::InvalidInput {
            message: "Invalid username".to_owned(),
        });
    }
    Ok((candidate.to_lowercase(), candidate.to_owned()))
}

/// Normalize an optional email: blank/None clears to None.
pub fn normalize_email(raw: Option<&str>) -> Result<Option<String>, UsersError> {
    let Some(value) = raw else {
        return Ok(None);
    };
    let trimmed = value.trim().to_lowercase();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() < 5 || !trimmed.contains('@') {
        return Err(UsersError::InvalidInput {
            message: "Invalid email address".to_owned(),
        });
    }
    Ok(Some(trimmed))
}

/// Validate a display name: 1-64 chars after trimming.
pub fn validate_display_name(raw: &str) -> Result<String, UsersError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(UsersError::InvalidInput {
            message: "Display name cannot be empty".to_owned(),
        });
    }
    if name.chars().count() > MAX_DISPLAY_NAME_LEN {
        return Err(UsersError::InvalidInput {
            message: "Display name is too long".to_owned(),
        });
    }
    Ok(name.to_owned())
}

/// Validate a password: at least 12 chars, at most 512 UTF-8 bytes.
pub fn validate_password(raw: &str) -> Result<(), UsersError> {
    if raw.chars().count() < MIN_PASSWORD_LEN {
        return Err(UsersError::InvalidInput {
            message: "Password must be at least 12 characters".to_owned(),
        });
    }
    if raw.len() > MAX_PASSWORD_BYTES {
        return Err(UsersError::InvalidInput {
            message: "Password is too long".to_owned(),
        });
    }
    Ok(())
}

/// Validate a companion device label: collapse whitespace, 1-80 chars.
pub fn validate_device_label(raw: &str) -> Result<String, UsersError> {
    let label = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() || label.chars().count() > MAX_DEVICE_LABEL_LEN {
        return Err(UsersError::InvalidInput {
            message: "Invalid device name".to_owned(),
        });
    }
    Ok(label)
}

/// Run the breach screen. A corpus hit is a 400; the screen itself fails
/// open, so only hits surface here.
async fn screen_password(deps: &UsersDeps, password: &str) -> Result<(), UsersError> {
    let policy = deps.security.hibp();
    if deps.screen.screen(password, &policy).await {
        return Err(UsersError::InvalidInput {
            message: "This password has appeared in a known data breach. Please choose a different password."
                .to_owned(),
        });
    }
    Ok(())
}

/// Hash off the IO loop: Argon2id is slow by design. A hashing failure
/// fails the request (fixed 500); no sentinel hash is ever persisted.
async fn hash_password(deps: &UsersDeps, password: &str) -> Result<String, UsersError> {
    let hasher = Arc::clone(&deps.passwords);
    let owned = password.to_owned();
    tokio::task::spawn_blocking(move || hasher.try_hash_argon2id(&owned))
        .await
        .map_err(|cause| {
            UsersError::internal(
                &format_args!("password hash panicked: {cause}"),
                deps.ids.as_ref(),
            )
        })?
        .map_err(|cause| UsersError::internal(&cause, deps.ids.as_ref()))
}

/// Verify off the IO loop, for the same reason. Unknown scheme tags fail
/// closed (dummy work, then false).
async fn check_password(deps: &UsersDeps, password: &str, scheme: &str, hash: &str) -> bool {
    let hasher = Arc::clone(&deps.passwords);
    let (password, scheme, hash) = (password.to_owned(), scheme.to_owned(), hash.to_owned());
    tokio::task::spawn_blocking(move || match HashScheme::from_tag(&scheme) {
        Some(HashScheme::Bcrypt) => hasher.verify_bcrypt(&password, &hash),
        Some(HashScheme::Argon2id) => hasher.verify_argon2id(&password, &hash),
        None => {
            hasher.dummy_verify();
            false
        }
    })
    .await
    .unwrap_or(false)
}

/// Scheme tag written on every new hash row.
fn native_scheme() -> &'static str {
    HashScheme::Argon2id.as_tag()
}

/// Map a store failure to a 500. Callers map `Conflict` themselves first.
pub(crate) fn store_internal(deps: &UsersDeps, error: StoreError) -> UsersError {
    match error {
        StoreError::Conflict => UsersError::Conflict {
            message: "Conflicting state".to_owned(),
        },
        StoreError::Internal(cause) => UsersError::internal(&cause, deps.ids.as_ref()),
    }
}

/// Load the caller's own user row. A stale principal (row gone) is a 401:
///
/// the session outlived the account.
async fn own_user(deps: &UsersDeps, ctx: &AuthContext) -> Result<UserRecord, UsersError> {
    deps.users
        .get_by_id(&ctx.user_id)
        .await
        .map_err(|error| store_internal(deps, error))?
        .ok_or_else(|| UsersError::Unauthorized {
            message: "Authentication required".to_owned(),
        })
}

/// Render a user row plus its providers into the public shape.
pub(crate) async fn user_response(
    deps: &UsersDeps,
    user: &UserRecord,
) -> Result<UserResponse, UsersError> {
    let providers = deps
        .users
        .provider_names(&user.id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(UserResponse {
        id: user.id.clone(),
        username: user.username.clone(),
        username_display: user.username_display.clone(),
        display_name: user.display_name.clone(),
        email: user.email.clone(),
        avatar_url: user.avatar_url.clone(),
        role: user.role,
        providers,
        created_at: user.created_at,
        last_login_at: user.last_login_at,
    })
}

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

/// GET /me: own profile.
pub async fn get_profile(
    deps: &UsersDeps,
    ctx: &AuthContext,
) -> Result<ProfileResponse, UsersError> {
    let user = own_user(deps, ctx).await?;
    Ok(ProfileResponse {
        user: user_response(deps, &user).await?,
    })
}

/// PATCH /me: change display name.
pub async fn update_display_name(
    deps: &UsersDeps,
    ctx: &AuthContext,
    display_name: &str,
) -> Result<UserResponse, UsersError> {
    let name = validate_display_name(display_name)?;
    deps.users
        .update_profile(&ctx.user_id, Some(&name), None)
        .await
        .map_err(|error| store_internal(deps, error))?;
    user_response(deps, &own_user(deps, ctx).await?).await
}

/// PUT /me/username: rename self. Taken names are a 409.
pub async fn update_username(
    deps: &UsersDeps,
    ctx: &AuthContext,
    username: &str,
) -> Result<UserResponse, UsersError> {
    let (lower, display) = validate_username(username)?;
    deps.users
        .update_username(&ctx.user_id, &lower, &display)
        .await
        .map_err(|error| match error {
            StoreError::Conflict => UsersError::Conflict {
                message: "Username already taken".to_owned(),
            },
            StoreError::Internal(cause) => UsersError::internal(&cause, deps.ids.as_ref()),
        })?;
    user_response(deps, &own_user(deps, ctx).await?).await
}

/// PUT /me/email: set or clear the email. Taken addresses are a 409.
pub async fn update_email(
    deps: &UsersDeps,
    ctx: &AuthContext,
    email: Option<&str>,
) -> Result<UserResponse, UsersError> {
    let normalized = normalize_email(email)?;
    if let Some(candidate) = normalized.as_deref() {
        let existing = deps
            .users
            .get_by_email(candidate)
            .await
            .map_err(|error| store_internal(deps, error))?;
        if existing.as_ref().is_some_and(|row| row.id != ctx.user_id) {
            return Err(UsersError::Conflict {
                message: "Email already in use".to_owned(),
            });
        }
    }
    deps.users
        .update_email(&ctx.user_id, normalized.as_deref())
        .await
        .map_err(|error| match error {
            StoreError::Conflict => UsersError::Conflict {
                message: "Email already in use".to_owned(),
            },
            StoreError::Internal(cause) => UsersError::internal(&cause, deps.ids.as_ref()),
        })?;
    user_response(deps, &own_user(deps, ctx).await?).await
}

/// POST /me/password: change password, proving the current one.
pub async fn change_password(
    deps: &UsersDeps,
    ctx: &AuthContext,
    current_password: &str,
    new_password: &str,
) -> Result<UserResponse, UsersError> {
    let local = deps
        .users
        .local_credential(&ctx.user_id)
        .await
        .map_err(|error| store_internal(deps, error))?
        .ok_or_else(|| UsersError::Conflict {
            message: "No local password is set for this account".to_owned(),
        })?;
    if !check_password(deps, current_password, &local.scheme, &local.hash).await {
        return Err(UsersError::Unauthorized {
            message: "Current password is incorrect".to_owned(),
        });
    }
    validate_password(new_password)?;
    screen_password(deps, new_password).await?;
    let hash = hash_password(deps, new_password).await?;
    // One write: the new hash lands and every other session of the account
    // dies with the old password. The session making the change stays.
    let replaced = deps
        .users
        .change_local_hash(
            &ctx.user_id,
            &local.hash,
            native_scheme(),
            &hash,
            &ctx.session_id,
        )
        .await
        .map_err(|error| store_internal(deps, error))?;
    if !replaced {
        return Err(UsersError::Conflict {
            message: "Your password changed while this request was running. Try again.".to_owned(),
        });
    }
    user_response(deps, &own_user(deps, ctx).await?).await
}

/// POST /me/local-password: set the first local password on an account
/// without one (SSO-created accounts). Needs a standard session.
pub async fn set_local_password(
    deps: &UsersDeps,
    ctx: &AuthContext,
    new_password: &str,
) -> Result<UserResponse, UsersError> {
    ctx.require_standard_session()?;
    let existing = deps
        .users
        .local_credential(&ctx.user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    if existing.is_some() {
        return Err(UsersError::Conflict {
            message: "A local password already exists; use change password instead".to_owned(),
        });
    }
    let user = own_user(deps, ctx).await?;
    if user.username.is_none() {
        return Err(UsersError::InvalidInput {
            message: "Choose a username first".to_owned(),
        });
    }
    validate_password(new_password)?;
    screen_password(deps, new_password).await?;
    let hash = hash_password(deps, new_password).await?;
    deps.users
        .insert_local_credential(LocalCredential {
            id: deps.ids.new_id(),
            user_id: ctx.user_id.clone(),
            scheme: native_scheme().to_owned(),
            hash,
        })
        .await
        .map_err(|error| match error {
            StoreError::Conflict => UsersError::Conflict {
                message: "Could not set a local password".to_owned(),
            },
            StoreError::Internal(cause) => UsersError::internal(&cause, deps.ids.as_ref()),
        })?;
    user_response(deps, &own_user(deps, ctx).await?).await
}

/// Served avatar path for one user. The version query busts image caches
/// after a re-upload; the route ignores it when serving.
pub fn avatar_url(user_id: &str, version: &str) -> String {
    format!("/api/v3/users/{user_id}/avatar?v={version}")
}

/// POST /me/avatar: store a new avatar from a JSON base64 upload.
pub async fn upload_avatar(
    deps: &UsersDeps,
    ctx: &AuthContext,
    content_type: &str,
    image_base64: &str,
) -> Result<UserResponse, UsersError> {
    if !ALLOWED_AVATAR_TYPES.contains(&content_type) {
        return Err(UsersError::InvalidInput {
            message: "Invalid image type. Allowed: JPEG, PNG, WebP, GIF".to_owned(),
        });
    }
    let bytes = B64
        .decode(image_base64.trim())
        .map_err(|_| UsersError::InvalidInput {
            message: "Invalid image data".to_owned(),
        })?;
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(UsersError::InvalidInput {
            message: "Image too large. Maximum size is 5 MB".to_owned(),
        });
    }
    deps.avatars
        .save(&ctx.user_id, content_type, &bytes)
        .await
        .map_err(|error| store_internal(deps, error))?;
    let nonce = hex_encode(&random_bytes::<3>(deps)?);
    let version = format!("{}-{nonce}", clock_now(deps));
    let url = avatar_url(&ctx.user_id, &version);
    deps.users
        .update_profile(&ctx.user_id, None, Some(&url))
        .await
        .map_err(|error| store_internal(deps, error))?;
    user_response(deps, &own_user(deps, ctx).await?).await
}

/// GET /users/{id}/avatar: self-or-admin read; admins need every avatar so
/// the admin user list renders.
pub async fn get_avatar(
    deps: &UsersDeps,
    ctx: &AuthContext,
    user_id: &str,
) -> Result<(Vec<u8>, String), UsersError> {
    if ctx.user_id != user_id && !ctx.role.is_admin() {
        return Err(UsersError::Forbidden {
            message: "Forbidden".to_owned(),
        });
    }
    deps.avatars
        .load(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?
        .ok_or(UsersError::NotFound)
}

// ---------------------------------------------------------------------------
// Sessions (session-list UI backend)
// ---------------------------------------------------------------------------

/// Strip the companion marker prefix back to the device label for display.
pub fn companion_label(user_agent: &str) -> &str {
    user_agent
        .strip_prefix(COMPANION_LABEL_PREFIX)
        .unwrap_or(user_agent)
}

/// GET /auth/sessions: own sessions, current first, then newest first.
pub async fn list_sessions(
    deps: &UsersDeps,
    ctx: &AuthContext,
) -> Result<SessionListResponse, UsersError> {
    let mut rows = deps
        .sessions
        .list_for_user(&ctx.user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let mut views: Vec<SessionView> = rows
        .iter()
        .map(|row| SessionView {
            id: row.id.clone(),
            kind: match row.kind {
                SessionKind::Standard => "standard".to_owned(),
                SessionKind::Companion => "companion".to_owned(),
            },
            label: companion_label(&row.label).to_owned(),
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            expires_at: row.expires_at,
            current: row.id == ctx.session_id,
        })
        .collect();
    views.sort_by(|a, b| b.current.cmp(&a.current));
    Ok(SessionListResponse { sessions: views })
}

/// POST /auth/device-sessions: mint a named companion token. Reminting a
/// label atomically revokes the prior same-label token. Companions cannot
/// mint (403), so token trees stay one level deep.
pub async fn mint_device_session(
    deps: &UsersDeps,
    ctx: &AuthContext,
    body: &DeviceSessionMint,
) -> Result<DeviceSessionResponse, UsersError> {
    ctx.require_standard_session()?;
    let label = validate_device_label(&body.label)?;
    own_user(deps, ctx).await?;
    let token = fresh_secret(deps)?;
    let now = clock_now(deps);
    let row = deps
        .sessions
        .replace_companion(
            &deps.ids.new_id(),
            &ctx.user_id,
            &tokens::hash_token(&token),
            &label,
            now,
            tokens::expires_at(now),
        )
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(DeviceSessionResponse {
        id: row.id,
        label,
        token,
        expires_at: row.expires_at,
    })
}

/// DELETE /auth/sessions/{id}: revoke one own session. Foreign or unknown
/// ids are 404, never 403: no cross-user oracle.
pub async fn revoke_session(
    deps: &UsersDeps,
    ctx: &AuthContext,
    session_id: &str,
) -> Result<(), UsersError> {
    let revoked = deps
        .sessions
        .revoke_scoped(&ctx.user_id, session_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    if revoked {
        Ok(())
    } else {
        Err(UsersError::NotFound)
    }
}

/// POST /auth/logout-all: revoke every own session.
pub async fn logout_all(deps: &UsersDeps, ctx: &AuthContext) -> Result<(), UsersError> {
    deps.sessions
        .revoke_all_for_user(&ctx.user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(())
}

/// DELETE /admin/users/{id}/sessions: revoke every session of one user.
pub async fn admin_revoke_user_sessions(deps: &UsersDeps, user_id: &str) -> Result<(), UsersError> {
    let user = deps
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    if user.is_none() {
        return Err(UsersError::NotFound);
    }
    deps.sessions
        .revoke_all_for_user(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Admin users
// ---------------------------------------------------------------------------

/// Clamp admin list pagination to the v2 page shape.
pub fn clamp_admin_page(limit: Option<u64>, offset: Option<u64>) -> (u64, u64) {
    let limit = limit.unwrap_or(100).clamp(1, 100);
    let offset = offset.unwrap_or(0);
    (limit, offset)
}

/// GET /admin/users: one page plus the total.
pub async fn admin_list_users(
    deps: &UsersDeps,
    limit: u64,
    offset: u64,
) -> Result<AdminUserListResponse, UsersError> {
    let (users, total) = deps
        .users
        .list(limit, offset)
        .await
        .map_err(|error| store_internal(deps, error))?;
    let mut views = Vec::with_capacity(users.len());
    for user in &users {
        views.push(user_response(deps, user).await?);
    }
    Ok(AdminUserListResponse {
        users: views,
        total,
    })
}

/// GET /admin/users/{id}: one user.
pub async fn admin_get_user(deps: &UsersDeps, user_id: &str) -> Result<UserResponse, UsersError> {
    let user = deps
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?
        .ok_or(UsersError::NotFound)?;
    user_response(deps, &user).await
}

/// POST /admin/users: create a local account. Conflicts stay vague on
/// purpose (no username/email oracle for admins either).
pub async fn admin_create_user(
    deps: &UsersDeps,
    username: &str,
    password: &str,
    display_name: Option<&str>,
    email: Option<&str>,
    role: Role,
) -> Result<UserResponse, UsersError> {
    let (lower, display) = validate_username(username)?;
    validate_password(password)?;
    screen_password(deps, password).await?;
    let normalized_email = normalize_email(email)?;
    if deps
        .users
        .get_by_username(&lower)
        .await
        .map_err(|error| store_internal(deps, error))?
        .is_some()
    {
        return Err(vague_create_conflict());
    }
    if let Some(candidate) = normalized_email.as_deref()
        && deps
            .users
            .get_by_email(candidate)
            .await
            .map_err(|error| store_internal(deps, error))?
            .is_some()
    {
        return Err(vague_create_conflict());
    }
    let name = display_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&display)
        .to_owned();
    if name.chars().count() > MAX_DISPLAY_NAME_LEN {
        return Err(UsersError::InvalidInput {
            message: "Display name is too long".to_owned(),
        });
    }
    let hash = hash_password(deps, password).await?;
    let user_id = deps.ids.new_id();
    let now = clock_now(deps);
    let user = UserRecord {
        id: user_id.clone(),
        username: Some(lower),
        username_display: Some(display),
        display_name: name,
        email: normalized_email,
        avatar_url: None,
        role,
        created_at: now,
        last_login_at: None,
    };
    // One transaction for the account and its password: a crash between
    // two writes would leave a passwordless account (fatal for first setup).
    deps.users
        .insert_with_local_credential(
            user.clone(),
            LocalCredential {
                id: deps.ids.new_id(),
                user_id,
                scheme: native_scheme().to_owned(),
                hash,
            },
        )
        .await
        .map_err(|error| create_insert_error(deps, error))?;
    user_response(deps, &user).await
}

/// Creation-write faults: conflicts stay vague (no username/email oracle),
/// store faults are fixed 500s with a log line. Never confused: a 500
/// reported as 409 would hide an outage behind "try another name".
fn create_insert_error(deps: &UsersDeps, error: StoreError) -> UsersError {
    match error {
        StoreError::Conflict => vague_create_conflict(),
        StoreError::Internal(cause) => UsersError::internal(&cause, deps.ids.as_ref()),
    }
}

fn vague_create_conflict() -> UsersError {
    UsersError::Conflict {
        message: "Could not create user".to_owned(),
    }
}

/// PUT /admin/users/{id}/role: change a role. Self-demotion is 403; removing
/// the last admin is 409.
pub async fn admin_set_role(
    deps: &UsersDeps,
    ctx: &AuthContext,
    user_id: &str,
    role: Role,
) -> Result<UserResponse, UsersError> {
    if ctx.user_id == user_id && !role.is_admin() {
        return Err(UsersError::Forbidden {
            message: "Cannot remove your own admin privileges".to_owned(),
        });
    }
    // The last-admin guard runs inside the role write, so two admins
    // demoting each other at once cannot both pass it.
    match deps
        .users
        .set_role(user_id, role)
        .await
        .map_err(|error| store_internal(deps, error))?
    {
        RoleChange::Changed => {}
        RoleChange::NotFound => return Err(UsersError::NotFound),
        RoleChange::LastAdmin => {
            return Err(UsersError::Conflict {
                message: "Cannot remove the last admin account".to_owned(),
            });
        }
    }
    let updated = deps
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?
        .ok_or(UsersError::NotFound)?;
    user_response(deps, &updated).await
}

/// DELETE /admin/users/{id}: delete an account. Self-deletion is 403;
/// deleting the last admin, or an account library history still names, is
/// 409.
pub async fn admin_delete_user(
    deps: &UsersDeps,
    ctx: &AuthContext,
    user_id: &str,
) -> Result<(), UsersError> {
    if ctx.user_id == user_id {
        return Err(UsersError::Forbidden {
            message: "Cannot delete your own account".to_owned(),
        });
    }
    match deps
        .users
        .delete(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?
    {
        UserDeletion::Deleted => Ok(()),
        UserDeletion::NotFound => Err(UsersError::NotFound),
        UserDeletion::LastAdmin => Err(UsersError::Conflict {
            message: "Cannot delete the last admin account".to_owned(),
        }),
        UserDeletion::Referenced(records) => Err(UsersError::Conflict {
            message: format!(
                "This account is recorded on {} and cannot be deleted. Change its role instead.",
                records.join(", ")
            ),
        }),
    }
}

// ---------------------------------------------------------------------------
// Password recovery (no live mail; codes travel out of band)
// ---------------------------------------------------------------------------

/// Mint a recovery code: 20 alphabet chars, SHA-256 at rest, 15-minute TTL,
/// single active code per user.
fn mint_recovery_code(deps: &UsersDeps) -> Result<(String, String), UsersError> {
    let bytes = random_bytes::<RECOVERY_LEN>(deps)?;
    let canonical: String = bytes
        .iter()
        .map(|byte| RECOVERY_ALPHABET[(usize::from(*byte)) % RECOVERY_ALPHABET.len()] as char)
        .collect();
    let display = canonical
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join("-");
    Ok((display, tokens::hash_token(&canonical)))
}

/// Canonicalize a submitted code: ignore dashes, spaces, and case.
pub fn canonicalize_recovery_code(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect::<String>()
        .to_uppercase()
}

/// POST /admin/users/{id}/recovery-code: mint a code for a local account.
/// The code returns once; delivery is out of band (shown to the admin).
pub async fn admin_mint_recovery_code(
    deps: &UsersDeps,
    user_id: &str,
) -> Result<RecoveryCodeResponse, UsersError> {
    let user = deps
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?
        .ok_or(UsersError::NotFound)?;
    let local = deps
        .users
        .local_credential(&user.id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    if local.is_none() {
        return Err(UsersError::Conflict {
            message: "Local password recovery is not available for this account".to_owned(),
        });
    }
    let (display, code_hash) = mint_recovery_code(deps)?;
    let now = clock_now(deps);
    deps.recovery
        .store(RecoveryCode {
            user_id: user.id,
            code_hash,
            created_at: now,
            expires_at: now + RECOVERY_TTL_SECS,
        })
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(RecoveryCodeResponse {
        recovery_code: display,
        expires_at: now + RECOVERY_TTL_SECS,
    })
}

/// POST /auth/password-recovery/reset (public): consume a code to set a new
/// password. Failures share one message: no username/code oracle.
pub async fn reset_password(deps: &UsersDeps, body: &PasswordReset) -> Result<(), UsersError> {
    validate_password(&body.new_password)?;
    let canonical = canonicalize_recovery_code(&body.recovery_code);
    let now = clock_now(deps);
    let found = deps
        .recovery
        .find_live_by_hash(&tokens::hash_token(&canonical), now)
        .await
        .map_err(|error| store_internal(deps, error))?;
    let invalid = || UsersError::Unauthorized {
        message: "Invalid or expired recovery code".to_owned(),
    };
    let Some(code) = found else {
        return Err(invalid());
    };
    let owner = deps
        .users
        .get_by_id(&code.user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    let matches = owner.as_ref().and_then(|row| row.username.as_deref())
        == Some(body.username.trim().to_lowercase().as_str());
    if !matches {
        return Err(invalid());
    }
    let local = deps
        .users
        .local_credential(&code.user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    let Some(local) = local else {
        return Err(invalid());
    };
    // Only a caller holding a live code reaches the breach screen, so the
    // public reset route cannot be used to probe the HIBP service.
    screen_password(deps, &body.new_password).await?;
    let hash = hash_password(deps, &body.new_password).await?;
    // One atomic write: the new hash lands, every prior session dies, and
    // the code is consumed. A reset that left sessions live would hand the
    // account to whoever holds an old token.
    let replaced = deps
        .users
        .complete_recovery_reset(&code.user_id, &local.hash, native_scheme(), &hash)
        .await
        .map_err(|error| store_internal(deps, error))?;
    if !replaced {
        return Err(invalid());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-user Last.fm (v3 has no admin-global pair)
// ---------------------------------------------------------------------------

/// Last.fm master-switch guard. Reads live on every call.
fn require_lastfm_enabled(deps: &UsersDeps) -> Result<(), UsersError> {
    if deps.lastfm_switch.enabled() {
        Ok(())
    } else {
        Err(UsersError::Conflict {
            message: "Last.fm is disabled".to_owned(),
        })
    }
}

/// Map a Last.fm client failure. Transport faults are fixed-body 502s;
/// unapproved tokens are a 409 (user action pending, not an outage);
/// rejections are a 400 naming no secret material.
fn lastfm_error(deps: &UsersDeps, error: LastFmError) -> UsersError {
    match error {
        LastFmError::Transport => UsersError::upstream(&"last.fm unreachable", deps.ids.as_ref()),
        LastFmError::TokenNotAuthorized => UsersError::Conflict {
            message: "Last.fm access hasn't been approved yet. Approve it, then try again."
                .to_owned(),
        },
        LastFmError::Configuration => UsersError::InvalidInput {
            message: "Last.fm rejected the request. Check the stored credentials and try again."
                .to_owned(),
        },
    }
}

/// GET /me/connections/lastfm: link status. Never carries secrets.
pub async fn lastfm_status(
    deps: &UsersDeps,
    user_id: &str,
) -> Result<LastFmStatusResponse, UsersError> {
    let link = deps
        .lastfm
        .get(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    match link {
        None => Ok(LastFmStatusResponse {
            configured: false,
            linked: false,
            username: None,
        }),
        Some(link) => Ok(LastFmStatusResponse {
            configured: link.configured,
            linked: link.username.is_some() && link.session_key_encrypted.is_some(),
            username: link.username,
        }),
    }
}

/// PUT /me/connections/lastfm: store the user's own API credentials.
/// The `lastfm****` mask sentinel keeps the stored value per field; storing
/// a new key unlinks the old session.
pub async fn lastfm_set_credentials(
    deps: &UsersDeps,
    user_id: &str,
    api_key: &str,
    shared_secret: &str,
) -> Result<LastFmConnection, UsersError> {
    require_lastfm_enabled(deps)?;
    let mask = crate::runtime_config::mask::LASTFM_SECRET_MASK;
    let current = deps
        .lastfm
        .get(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    // Resolves one field to its stored ciphertext plus whether the plaintext
    // changed. The comparison decrypts first: ciphertext never compares
    // equal (fresh nonce per seal), so comparing it would unlink on every
    // resubmit. An undecryptable stored value counts as changed.
    let resolve =
        |raw: &str, stored: Option<String>, what: &str| -> Result<(String, bool), UsersError> {
            if raw == mask {
                let cipher = stored.ok_or_else(|| UsersError::InvalidInput {
                    message: format!("Last.fm {what} is required"),
                })?;
                return Ok((cipher, false));
            }
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(UsersError::InvalidInput {
                    message: format!("Last.fm {what} is required"),
                });
            }
            let unchanged = stored
                .as_deref()
                .and_then(|cipher| deps.crypto.decrypt(cipher).ok())
                .as_deref()
                == Some(trimmed);
            let cipher = deps
                .crypto
                .encrypt(trimmed)
                .map_err(|cause| UsersError::internal(&cause, deps.ids.as_ref()))?;
            Ok((cipher, !unchanged))
        };
    let (key_cipher, key_changed) = resolve(
        api_key,
        current
            .as_ref()
            .and_then(|link| link.api_key_encrypted.clone()),
        "API key",
    )?;
    let (secret_cipher, secret_changed) = resolve(
        shared_secret,
        current
            .as_ref()
            .and_then(|link| link.shared_secret_encrypted.clone()),
        "shared secret",
    )?;
    let link = LastFmConnection {
        configured: true,
        api_key_encrypted: Some(key_cipher),
        shared_secret_encrypted: Some(secret_cipher),
        username: if key_changed || secret_changed {
            None
        } else {
            current.as_ref().and_then(|link| link.username.clone())
        },
        session_key_encrypted: if key_changed || secret_changed {
            None
        } else {
            current
                .as_ref()
                .and_then(|link| link.session_key_encrypted.clone())
        },
    };
    deps.lastfm
        .upsert(user_id, link.clone())
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(link)
}

/// Decrypt one user's stored key pair. Decrypt failure is a 500: the row is
/// corrupt or the install was re-keyed.
fn lastfm_keypair(
    deps: &UsersDeps,
    link: &LastFmConnection,
) -> Result<(String, String), UsersError> {
    let (Some(key_cipher), Some(secret_cipher)) = (
        link.api_key_encrypted.as_deref(),
        link.shared_secret_encrypted.as_deref(),
    ) else {
        return Err(UsersError::Conflict {
            message: "Set your Last.fm API credentials first".to_owned(),
        });
    };
    let key = deps
        .crypto
        .decrypt(key_cipher)
        .map_err(|cause| UsersError::internal(&cause, deps.ids.as_ref()))?;
    let secret = deps
        .crypto
        .decrypt(secret_cipher)
        .map_err(|cause| UsersError::internal(&cause, deps.ids.as_ref()))?;
    Ok((key, secret))
}

/// POST /me/connections/lastfm/token: fetch a sign-in token + approval URL.
pub async fn lastfm_request_token(
    deps: &UsersDeps,
    user_id: &str,
) -> Result<LastFmTokenResponse, UsersError> {
    require_lastfm_enabled(deps)?;
    let link = deps
        .lastfm
        .get(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    let Some(link) = link.filter(|link| link.configured) else {
        return Err(UsersError::Conflict {
            message: "Set your Last.fm API credentials first".to_owned(),
        });
    };
    let (api_key, _secret) = lastfm_keypair(deps, &link)?;
    let (token, auth_url) = deps
        .lastfm_client
        .request_token(&api_key)
        .await
        .map_err(|error| lastfm_error(deps, error))?;
    Ok(LastFmTokenResponse { token, auth_url })
}

/// POST /me/connections/lastfm/session: exchange an approved token.
pub async fn lastfm_exchange_session(
    deps: &UsersDeps,
    user_id: &str,
    token: &str,
) -> Result<LastFmSessionResponse, UsersError> {
    require_lastfm_enabled(deps)?;
    let link = deps
        .lastfm
        .get(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    let Some(link) = link.filter(|link| link.configured) else {
        return Err(UsersError::Conflict {
            message: "Set your Last.fm API credentials first".to_owned(),
        });
    };
    let (api_key, shared_secret) = lastfm_keypair(deps, &link)?;
    let (username, session_key) = deps
        .lastfm_client
        .exchange_session(&api_key, &shared_secret, token)
        .await
        .map_err(|error| lastfm_error(deps, error))?;
    let session_cipher = deps
        .crypto
        .encrypt(&session_key)
        .map_err(|cause| UsersError::internal(&cause, deps.ids.as_ref()))?;
    deps.lastfm
        .upsert(
            user_id,
            LastFmConnection {
                username: Some(username.clone()),
                session_key_encrypted: Some(session_cipher),
                ..link
            },
        )
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(LastFmSessionResponse {
        username,
        linked: true,
    })
}

/// DELETE /me/connections/lastfm: unlink (drops credentials and session).
/// Idempotent: unlinking twice still succeeds.
pub async fn lastfm_unlink(deps: &UsersDeps, user_id: &str) -> Result<(), UsersError> {
    deps.lastfm
        .delete(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(())
}

/// Wiring point for scrobble forwarding (not called yet): the linked session for one
/// user, or None when disabled, unlinked, or undecryptable. Scrobbling is
/// optional enrichment, so every miss degrades to None with a log line.
pub async fn lastfm_scrobble_session(deps: &UsersDeps, user_id: &str) -> Option<(String, String)> {
    if !deps.lastfm_switch.enabled() {
        return None;
    }
    let link = deps.lastfm.get(user_id).await.ok()??;
    let (username, session_cipher) = match (link.username, link.session_key_encrypted) {
        (Some(username), Some(cipher)) => (username, cipher),
        _ => return None,
    };
    match deps.crypto.decrypt(&session_cipher) {
        Ok(session_key) => Some((username, session_key)),
        Err(cause) => {
            tracing::error!(user_id = %user_id.chars().take(8).collect::<String>(), %cause, "last.fm session undecryptable");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// App passwords (+ the compat verification contract)
// ---------------------------------------------------------------------------

fn app_password_view(row: &AppPasswordRecord) -> AppPasswordView {
    AppPasswordView {
        id: row.id.clone(),
        name: row.name.clone(),
        created_at: row.created_at,
        last_used_at: row.last_used_at,
        last_client: row.last_client.clone(),
    }
}

/// GET /me/app-passwords: own live app passwords, oldest first.
pub async fn list_app_passwords(
    deps: &UsersDeps,
    user_id: &str,
) -> Result<AppPasswordListResponse, UsersError> {
    let rows = deps
        .app_passwords
        .list_active_by_user(user_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    Ok(AppPasswordListResponse {
        app_passwords: rows.iter().map(app_password_view).collect(),
    })
}

/// POST /me/app-passwords: create one. The secret returns once, never again.
/// Needs a standard session: a device token must not mint a permanent
/// credential.
pub async fn create_app_password(
    deps: &UsersDeps,
    ctx: &AuthContext,
    body: &AppPasswordCreate,
) -> Result<AppPasswordCreatedResponse, UsersError> {
    ctx.require_standard_session()?;
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("App password");
    if name.chars().count() > MAX_APP_PASSWORD_NAME_LEN {
        return Err(UsersError::InvalidInput {
            message: "App-password name is too long".to_owned(),
        });
    }
    let secret = fresh_secret(deps)?;
    let encrypted = deps
        .crypto
        .encrypt(&secret)
        .map_err(|cause| UsersError::internal(&cause, deps.ids.as_ref()))?;
    let row = AppPasswordRecord {
        id: deps.ids.new_id(),
        user_id: ctx.user_id.clone(),
        name: name.to_owned(),
        secret_sha256: tokens::hash_token(&secret),
        secret_encrypted: encrypted,
        created_at: clock_now(deps),
        last_used_at: None,
        last_client: None,
    };
    // The cap is checked inside the insert transaction, so parallel
    // creates cannot overshoot it.
    let inserted = deps
        .app_passwords
        .insert_capped(row.clone(), MAX_ACTIVE_APP_PASSWORDS)
        .await
        .map_err(|error| store_internal(deps, error))?;
    if !inserted {
        return Err(UsersError::Conflict {
            message: format!(
                "App-password limit reached ({MAX_ACTIVE_APP_PASSWORDS}). Revoke one before creating another."
            ),
        });
    }
    Ok(AppPasswordCreatedResponse {
        id: row.id,
        name: row.name,
        secret,
        created_at: row.created_at,
    })
}

/// DELETE /me/app-passwords/{id}: revoke one own app password. Foreign or
/// unknown ids are 404: no cross-user oracle.
pub async fn revoke_app_password(
    deps: &UsersDeps,
    user_id: &str,
    app_password_id: &str,
) -> Result<(), UsersError> {
    let row = deps
        .app_passwords
        .get_by_id(app_password_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    match row {
        Some(row) if row.user_id == user_id => {
            deps.app_passwords
                .revoke(app_password_id)
                .await
                .map_err(|error| store_internal(deps, error))?;
            Ok(())
        }
        _ => Err(UsersError::NotFound),
    }
}

/// GET /admin/app-passwords: every live app password with its owner.
/// Ownerless rows (mid-cascade) are skipped, never shown as ghosts.
pub async fn admin_list_app_passwords(
    deps: &UsersDeps,
) -> Result<Vec<AdminAppPasswordView>, UsersError> {
    let rows = deps
        .app_passwords
        .list_all_active()
        .await
        .map_err(|error| store_internal(deps, error))?;
    let owner_ids: Vec<String> = rows.iter().map(|row| row.user_id.clone()).collect();
    let owners = deps
        .users
        .get_by_ids(&owner_ids)
        .await
        .map_err(|error| store_internal(deps, error))?;
    let mut views = Vec::new();
    for row in &rows {
        let Some(owner) = owners.iter().find(|user| user.id == row.user_id) else {
            continue;
        };
        let owner_username = owner
            .username_display
            .clone()
            .or_else(|| owner.username.clone())
            .unwrap_or_else(|| owner.display_name.clone());
        views.push(AdminAppPasswordView {
            id: row.id.clone(),
            user_id: row.user_id.clone(),
            owner_username,
            owner_display_name: owner.display_name.clone(),
            name: row.name.clone(),
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            last_client: row.last_client.clone(),
        });
    }
    Ok(views)
}

/// DELETE /admin/app-passwords/{id}: revoke any user's app password.
/// Unknown or already-revoked ids are 404.
pub async fn admin_revoke_app_password(
    deps: &UsersDeps,
    app_password_id: &str,
) -> Result<(), UsersError> {
    let revoked = deps
        .app_passwords
        .revoke(app_password_id)
        .await
        .map_err(|error| store_internal(deps, error))?;
    if revoked {
        Ok(())
    } else {
        Err(UsersError::NotFound)
    }
}

/// Compat verification contract (the compat APIs call this; native paths never do).
///
/// SHA-256(secret) resolves the live row, the stored digest compares in
/// constant time, and a successful verification stamps last use. Returns
/// None on every miss and never raises: unknown secrets, revoked rows,
/// missing owners, and store faults all verify as None (faults log).
/// Key-rotation safe: verification never decrypts.
pub async fn verify_app_password(
    deps: &UsersDeps,
    secret: &str,
    last_client: Option<&str>,
) -> Option<AppPasswordAuth> {
    if secret.is_empty() || secret.len() > MAX_AUTH_VALUE_LEN {
        return None;
    }
    let digest = tokens::hash_token(secret);
    let row = deps
        .app_passwords
        .get_active_by_sha256(&digest)
        .await
        .ok()??;
    if !tokens::constant_time_eq(&row.secret_sha256, &digest) {
        return None;
    }
    let owner = deps.users.get_by_id(&row.user_id).await.ok()??;
    if let Err(cause) = deps
        .app_passwords
        .touch(&digest, clock_now(deps), last_client)
        .await
    {
        tracing::warn!(%cause, "app-password touch failed");
    }
    Some(AppPasswordAuth {
        user_id: owner.id,
        app_password_id: row.id,
    })
}
