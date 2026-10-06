//! Admin user import from Jellyfin and Plex.
//!
//! Pre-provisions DroppedNeedle accounts from the shared media server: each
//! import creates an `auth_users` row (role forced to `user`) plus a
//! pre-linked `auth_providers` row with NULL `provider_data` (a login
//! identity, not a credential store). The first SSO login matches the
//! pre-seeded `provider_uid`, so no admin-set password is needed.
//!
//! The join key must equal exactly what the live login produces: the
//! Jellyfin user id from `GET /Users`, the Plex account uuid.
//!
//! Ported from v2's user import service and its `/admin/import/*` routes.

use std::collections::HashMap;

use super::super::federated::users::{
    CREATE_RETRIES, PROVIDER_PLEX, ProviderBinding, username_base,
};
use super::error::UsersError;
use super::models::{
    ImportCandidateListResponse, ImportCandidateView, ImportUsersResponse, UserRecord,
};
use super::roles::Role;
use super::services::{self, MAX_DISPLAY_NAME_LEN, store_internal, user_response as render_user};
use super::stores::{DirectoryUser, StoreError, UserDirectory};
use super::{UsersDeps, clock_now};

/// GET /admin/import/{jellyfin,plex}: every importable account with its
/// already-imported flag.
pub async fn list_import_candidates(
    deps: &UsersDeps,
    directory: &dyn UserDirectory,
) -> Result<ImportCandidateListResponse, UsersError> {
    let users = directory
        .list_users()
        .await
        .map_err(|error| UsersError::unavailable(&error, deps.ids.as_ref()))?;
    let mut candidates = Vec::with_capacity(users.len());
    for user in &users {
        let binding = deps
            .users
            .get_provider_binding(directory.provider(), &user.provider_uid)
            .await
            .map_err(|error| store_internal(deps, error))?;
        candidates.push(ImportCandidateView {
            provider: directory.provider().to_owned(),
            provider_uid: user.provider_uid.clone(),
            display_name: user.display_name.clone(),
            avatar_url: user.avatar_url.clone(),
            email: user.email.clone(),
            already_imported: binding.is_some(),
        });
    }
    Ok(ImportCandidateListResponse { candidates })
}

/// POST /admin/import: import a batch of uids from one directory. The
/// catalog is re-read server-side; client display names are never trusted.
pub async fn import_users(
    deps: &UsersDeps,
    directory: &dyn UserDirectory,
    provider_uids: &[String],
) -> Result<ImportUsersResponse, UsersError> {
    let users = directory
        .list_users()
        .await
        .map_err(|error| UsersError::unavailable(&error, deps.ids.as_ref()))?;
    let catalog: HashMap<&str, &DirectoryUser> = users
        .iter()
        .map(|user| (user.provider_uid.as_str(), user))
        .collect();

    let mut imported = Vec::new();
    let mut conflicts = Vec::new();
    let mut skipped = Vec::new();
    for uid in provider_uids {
        let Some(candidate) = catalog.get(uid.as_str()) else {
            skipped.push(uid.clone());
            continue;
        };
        match import_one(deps, directory.provider(), candidate).await {
            Ok(ImportOutcome::Imported(user)) => {
                imported.push(render_user(deps, &user).await?);
            }
            Ok(ImportOutcome::Conflict) => conflicts.push(uid.clone()),
            Ok(ImportOutcome::Skipped) => skipped.push(uid.clone()),
            Err(ImportFault::UsernameExhausted) => {
                // Un-de-dupable username: the whole batch fails as 409 (v2
                // maps its RegistrationError the same way).
                return Err(UsersError::Conflict {
                    message: "Could not import users".to_owned(),
                });
            }
            Err(ImportFault::Store(cause)) => {
                // One bad uid must not abort the batch (v2 parity).
                tracing::warn!(
                    uid = %uid.chars().take(8).collect::<String>(),
                    %cause,
                    "user import skipped one uid",
                );
                skipped.push(uid.clone());
            }
        }
    }
    tracing::info!(
        provider = directory.provider(),
        imported = imported.len(),
        conflicts = conflicts.len(),
        skipped = skipped.len(),
        "user import finished",
    );
    Ok(ImportUsersResponse {
        total_imported: imported.len() as u64,
        imported,
        conflicts,
        skipped,
    })
}

/// What one uid imported as.
enum ImportOutcome {
    Imported(UserRecord),
    /// The directory email belongs to an existing account. Nothing is
    /// linked: a media-server profile email is not proof of ownership, so
    /// the admin decides.
    Conflict,
    Skipped,
}

/// What one uid failed with. Store faults skip the uid; an exhausted
/// username derivation fails the batch.
enum ImportFault {
    Store(StoreError),
    UsernameExhausted,
}

/// Import one catalog entry: skip when already bound, link on email
/// collision, else create plus pre-link.
async fn import_one(
    deps: &UsersDeps,
    provider: &str,
    candidate: &DirectoryUser,
) -> Result<ImportOutcome, ImportFault> {
    let uid = candidate.provider_uid.as_str();

    // Idempotency: already linked means skip, never duplicate.
    let bound = deps
        .users
        .get_provider_binding(provider, uid)
        .await
        .map_err(ImportFault::Store)?;
    if bound.is_some() {
        return Ok(ImportOutcome::Skipped);
    }

    // Email collision: report it and import nothing. A directory email is
    // not proof the person owns the existing account.
    let email = match services::normalize_email(candidate.email.as_deref()) {
        Ok(email) => email,
        Err(_) => {
            tracing::warn!(
                uid = %uid.chars().take(8).collect::<String>(),
                "user import skipped a uid with an invalid email",
            );
            return Ok(ImportOutcome::Skipped);
        }
    };
    if let Some(email) = email.as_deref() {
        let existing = deps
            .users
            .get_by_email(email)
            .await
            .map_err(ImportFault::Store)?;
        if existing.is_some() {
            return Ok(ImportOutcome::Conflict);
        }
    }

    // New account. The Jellyfin avatar is an unguarded constructed URL, so
    // only Plex thumbs persist (v2 parity); role is forced, never requested.
    let avatar_url = if provider == PROVIDER_PLEX {
        candidate.avatar_url.clone()
    } else {
        None
    };
    let mut created: Option<UserRecord> = None;
    for _ in 0..CREATE_RETRIES {
        let (username, username_display) =
            derive_unique_username(deps, email.as_deref(), &candidate.display_name)
                .await
                .map_err(ImportFault::Store)?;
        let name: String = candidate
            .display_name
            .trim()
            .chars()
            .take(MAX_DISPLAY_NAME_LEN)
            .collect();
        let user = UserRecord {
            id: deps.ids.new_id(),
            username: Some(username),
            username_display: Some(username_display.clone()),
            display_name: if name.is_empty() {
                username_display
            } else {
                name
            },
            email: email.clone(),
            avatar_url: avatar_url.clone(),
            role: Role::User,
            created_at: clock_now(deps),
            last_login_at: None,
        };
        match deps.users.insert(user.clone()).await {
            Ok(()) => {
                created = Some(user);
                break;
            }
            Err(StoreError::Conflict) => continue,
            Err(error) => return Err(ImportFault::Store(error)),
        }
    }
    let Some(user) = created else {
        return Err(ImportFault::UsernameExhausted);
    };

    // Pre-linked binding: NULL provider_data (a login identity, not a
    // credential store; the first SSO login seals the real tokens). When
    // this fails the user row stays and the next import links it by email.
    match deps
        .users
        .insert_provider_binding(ProviderBinding {
            id: deps.ids.new_id(),
            user_id: user.id.clone(),
            provider: provider.to_owned(),
            provider_uid: uid.to_owned(),
        })
        .await
    {
        Ok(()) => Ok(ImportOutcome::Imported(user)),
        Err(StoreError::Conflict) => Ok(ImportOutcome::Skipped),
        Err(error) => Err(ImportFault::Store(error)),
    }
}

/// Derive a unique `(username, username_display)` pair through the shared
/// login-flow helper (email local-part, else display name, else `user`),
/// de-duped with a numeric suffix.
async fn derive_unique_username(
    deps: &UsersDeps,
    email: Option<&str>,
    display_name: &str,
) -> Result<(String, String), StoreError> {
    let base = username_base(email, display_name);
    let mut n = 1u32;
    loop {
        let (username, display) = if n == 1 {
            (base.to_lowercase(), base.clone())
        } else {
            (
                format!("{}-{n}", base.to_lowercase()),
                format!("{base}-{n}"),
            )
        };
        if deps.users.get_by_username(&username).await?.is_none() {
            return Ok((username, display));
        }
        n += 1;
    }
}
