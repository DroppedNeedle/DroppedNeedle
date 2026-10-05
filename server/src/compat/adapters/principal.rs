//! Compat principal: app-password verification plus a profile lookup.
//!
//! The password stores authenticate to a user id; the Subsonic handlers
//! also need the username, display name, and admin flag. [`CompatProfiles`]
//! is that lookup, implemented for [`UsersDeps`] in production and scripted
//! by tests.

use crate::auth::compat_auth::subsonic::{
    SubsonicDenied, SubsonicParams, SubsonicPasswordStore, authenticate,
};
use crate::auth::users::UsersDeps;
use crate::compat::subsonic::auth::{Credentials, Principal};
use crate::compat::subsonic::error::SubsonicError;

/// Profile facts the compat handlers read off a principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatProfile {
    /// Owning user id.
    pub user_id: String,
    /// Login username (lowercased lookup form).
    pub username: String,
    /// Preferred username casing.
    pub username_display: String,
    /// Display name.
    pub display_name: String,
    /// Admin role.
    pub is_admin: bool,
}

/// User-id → profile lookup behind the compat principals.
pub trait CompatProfiles: Clone + Send + Sync {
    /// Profile for a user id, or `None` when the account is gone.
    fn profile(&self, user_id: &str) -> impl Future<Output = Option<CompatProfile>> + Send;
}

impl CompatProfiles for UsersDeps {
    async fn profile(&self, user_id: &str) -> Option<CompatProfile> {
        let row = self.users.get_by_id(user_id).await.ok().flatten()?;
        let username = row.username.clone().unwrap_or_default();
        Some(CompatProfile {
            user_id: row.id,
            username_display: row
                .username_display
                .clone()
                .unwrap_or_else(|| username.clone()),
            username,
            display_name: row.display_name,
            is_admin: row.role.as_str() == "admin",
        })
    }
}

/// Authenticated compat caller.
#[derive(Debug, Clone)]
pub struct CompatPrincipal {
    /// Profile facts.
    pub profile: CompatProfile,
}

impl Principal for CompatPrincipal {
    fn user_id(&self) -> &str {
        &self.profile.user_id
    }

    fn username(&self) -> &str {
        &self.profile.username
    }

    fn username_display(&self) -> &str {
        &self.profile.username_display
    }

    fn display_name(&self) -> &str {
        &self.profile.display_name
    }

    fn is_admin(&self) -> bool {
        self.profile.is_admin
    }
}

/// Subsonic secret verifier over the app-password store. The
/// classified [`Credentials`] map back onto [`SubsonicParams`] so the
/// `compat_auth` [`authenticate`] runs the exact contract (schemes, precedence,
/// caps); denials keep their codes, and a vanished account fails as code
/// 40 like an unknown credential.
#[derive(Debug, Clone)]
pub struct SubsonicVerifier<P, U> {
    passwords: P,
    profiles: U,
}

impl<P, U> SubsonicVerifier<P, U> {
    /// Wrap a password store and a profile lookup.
    pub fn new(passwords: P, profiles: U) -> Self {
        Self {
            passwords,
            profiles,
        }
    }
}

impl<P: SubsonicPasswordStore, U: CompatProfiles> crate::compat::subsonic::Verifier
    for SubsonicVerifier<P, U>
{
    type Principal = CompatPrincipal;

    async fn verify(&self, credentials: &Credentials) -> Result<CompatPrincipal, SubsonicError> {
        let params = match credentials {
            Credentials::Token {
                username,
                token,
                salt,
                client,
            } => {
                let mut entries = vec![
                    ("u", username.as_str()),
                    ("t", token.as_str()),
                    ("s", salt.as_str()),
                ];
                if let Some(client) = client {
                    entries.push(("c", client.as_str()));
                }
                SubsonicParams::new(entries)
            }
            Credentials::Password {
                username,
                password,
                client,
            } => {
                let mut entries = vec![("u", username.as_str()), ("p", password.as_str())];
                if let Some(client) = client {
                    entries.push(("c", client.as_str()));
                }
                SubsonicParams::new(entries)
            }
            Credentials::ApiKey { key } => SubsonicParams::new(vec![("apiKey", key.as_str())]),
        };
        let denied = |denied: SubsonicDenied| SubsonicError::code_only(denied.code);
        let principal = authenticate(&self.passwords, &params)
            .await
            .map_err(denied)?;
        let profile = self
            .profiles
            .profile(&principal.user_id)
            .await
            .ok_or_else(|| {
                SubsonicError::code_only(crate::auth::compat_auth::subsonic::WRONG_CREDENTIALS)
            })?;
        Ok(CompatPrincipal { profile })
    }
}
