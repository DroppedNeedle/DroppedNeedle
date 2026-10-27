//! Jellyfin login: credential check against the configured server plus
//! user import with per-user connection auto-link.
//!
//! v2 parity: the outbound call sends the `Authorization: MediaBrowser ...`
//! header (not the legacy `X-Emby-Authorization` Jellyfin 10.11 removed),
//! email is always `None` (the endpoint exposes none), and the auto-link is
//! best-effort: it never fails the login.

use super::jellyfin_http::JellyfinHttp;
use super::users::{
    FederatedProfile, PROVIDER_JELLYFIN, StoredUser, find_or_create_federated_user,
};
use super::{FederatedError, MediaLinks, json_string};
use crate::sqlite::{FederatedUsers, Sessions};

/// Product token in the outbound auth header.
pub const PRODUCT: &str = "DroppedNeedle";
/// Client version advertised to the Jellyfin server (v2 parity).
pub const AUTH_CLIENT_VERSION: &str = "1.4.0";

/// Verified Jellyfin profile: ids and token the login hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinProfile {
    /// Jellyfin-side user id; the provider uid.
    pub jellyfin_user_id: String,
    /// Jellyfin display name.
    pub username: String,
    /// User-scoped access token, also stored for the media connection.
    pub access_token: String,
    /// Primary-image URL when the account has one.
    pub avatar_url: Option<String>,
}

/// The Jellyfin login service.
#[derive(Clone)]
pub struct JellyfinLogin {
    users: FederatedUsers,
    idp: JellyfinHttp,
    links: MediaLinks,
    sessions: Sessions,
}

impl std::fmt::Debug for JellyfinLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinLogin").finish_non_exhaustive()
    }
}

impl JellyfinLogin {
    /// Build the service over the Jellyfin client and the media links a
    /// sign-in saves.
    pub fn new(
        users: FederatedUsers,
        idp: JellyfinHttp,
        links: MediaLinks,
        sessions: Sessions,
    ) -> Self {
        Self {
            users,
            idp,
            links,
            sessions,
        }
    }

    /// Check credentials, import the user, auto-link the connection, and
    /// mint a session.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        user_agent: Option<&str>,
    ) -> Result<(StoredUser, String), FederatedError> {
        let profile = self.idp.authenticate_by_name(username, password).await?;
        let user = find_or_create_federated_user(
            &self.users,
            PROVIDER_JELLYFIN,
            &FederatedProfile {
                provider_uid: profile.jellyfin_user_id.clone(),
                display_name: profile.username.clone(),
                email: None,
                email_verified: false,
                avatar_url: profile.avatar_url.clone(),
                token_json: jellyfin_token_json(&profile.access_token),
            },
        )
        .await?;
        if let Err(error) = self.links.link_jellyfin(&user.id, &profile).await {
            tracing::warn!(%error, "could not link the signed-in Jellyfin account; the user can link it by hand");
        }
        let raw_token = self.sessions.issue_session(&user.id, user_agent).await?;
        Ok((user, raw_token))
    }
}

/// Outbound auth header for the credential check. Sent as `Authorization`;
/// both the header name and the value spelling matter (issue #151).
pub fn emby_auth_header(client_id: &str) -> String {
    format!(
        "MediaBrowser Client=\"{PRODUCT}\", Device=\"{PRODUCT}\", DeviceId=\"{client_id}\", Version=\"{AUTH_CLIENT_VERSION}\""
    )
}

/// Plaintext token JSON for the store to seal (v2 field names kept).
pub fn jellyfin_token_json(access_token: &str) -> String {
    format!("{{\"access_token\":{}}}", json_string(access_token))
}
