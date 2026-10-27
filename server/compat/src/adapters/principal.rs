//! Compat principal: app-password verification plus a profile lookup.
//!
//! The password stores authenticate to a user id; the Subsonic handlers
//! also need the username, display name, and admin flag, which
//! [`SubsonicVerifier`] reads from the users table.

use crate::subsonic::auth::Credentials;
use crate::subsonic::error::SubsonicError;
use droppedneedle_auth::compat_auth::prod::ProdCompatPasswords;
use droppedneedle_auth::compat_auth::subsonic::{SubsonicDenied, SubsonicParams, authenticate};
use droppedneedle_auth::users::UsersDeps;

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
    /// `user`, `trusted` or `admin`.
    pub role: String,
}

/// Profile for a user id, or `None` when the account is gone.
async fn profile(users: &UsersDeps, user_id: &str) -> Option<CompatProfile> {
    let row = users.users.get_by_id(user_id).await.ok().flatten()?;
    let username = row.username.clone().unwrap_or_default();
    Some(CompatProfile {
        user_id: row.id,
        username_display: row
            .username_display
            .clone()
            .unwrap_or_else(|| username.clone()),
        username,
        display_name: row.display_name,
        role: row.role.as_str().to_owned(),
    })
}

/// Authenticated compat caller.
#[derive(Debug, Clone)]
pub struct CompatPrincipal {
    /// Profile facts.
    pub profile: CompatProfile,
}

impl CompatPrincipal {
    /// Owning user id.
    pub fn user_id(&self) -> &str {
        &self.profile.user_id
    }

    /// Login username.
    pub fn username(&self) -> &str {
        &self.profile.username
    }

    /// Display username (for avatar self-match).
    pub fn username_display(&self) -> &str {
        &self.profile.username_display
    }

    /// Display name (for now-playing attribution).
    pub fn display_name(&self) -> &str {
        &self.profile.display_name
    }

    /// `user`, `trusted` or `admin` (the download setting).
    pub fn role(&self) -> &str {
        &self.profile.role
    }

    /// Admin role (startScan gate, getUser roles).
    pub fn is_admin(&self) -> bool {
        self.role() == "admin"
    }
}

/// Subsonic secret verifier over the app-password store. The
/// classified [`Credentials`] map back onto [`SubsonicParams`] so the
/// `compat_auth` [`authenticate`] runs the exact contract (schemes, precedence,
/// caps); denials keep their codes, and a vanished account fails as code
/// 40 like an unknown credential.
#[derive(Clone)]
pub struct SubsonicVerifier {
    passwords: ProdCompatPasswords,
    users: UsersDeps,
}

impl SubsonicVerifier {
    /// Wrap the password store and the users the profiles come from.
    pub fn new(passwords: ProdCompatPasswords, users: UsersDeps) -> Self {
        Self { passwords, users }
    }

    /// Verify classified credentials against the app-password store.
    pub async fn verify(
        &self,
        credentials: &Credentials,
    ) -> Result<CompatPrincipal, SubsonicError> {
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
        let profile = profile(&self.users, &principal.user_id)
            .await
            .ok_or_else(|| {
                SubsonicError::code_only(
                    droppedneedle_auth::compat_auth::subsonic::WRONG_CREDENTIALS,
                )
            })?;
        Ok(CompatPrincipal { profile })
    }
}
