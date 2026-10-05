//! Jellyfin login: credential check against the configured server plus
//! user import with per-user connection auto-link.
//!
//! v2 parity: the outbound call sends the `Authorization: MediaBrowser ...`
//! header (not the legacy `X-Emby-Authorization` Jellyfin 10.11 removed),
//! email is always `None` (the endpoint exposes none), and the auto-link is
//! best-effort: it never fails the login.

use super::users::{
    FederatedProfile, FederatedUserStore, PROVIDER_JELLYFIN, StoredUser,
    find_or_create_federated_user,
};
use super::{FederatedError, SessionIssuer, json_string};

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

/// Network edge for Jellyfin credential checks. Production adapter rules
/// (v2 parity): POST `{base}/Users/AuthenticateByName` with
/// [`emby_auth_header`] and `{"Username","Pw"}`; timeouts and connect
/// failures are `ProviderUnavailable`; 401 is
/// `Authentication("Invalid Jellyfin username or password")`; 403 is
/// `Authentication("This Jellyfin account does not have access")`; any
/// other non-2xx is `ProviderUnavailable`; bad JSON is
/// `ProviderUnavailable("Jellyfin returned an unexpected response")`;
/// missing user id or token is `ProviderUnavailable("Jellyfin returned
/// incomplete auth data")`.
pub trait JellyfinIdp: Clone + Send + Sync + 'static {
    /// False when no Jellyfin server is configured.
    fn is_configured(&self) -> bool;

    /// Check credentials; no DroppedNeedle side effects.
    fn authenticate_by_name(
        &self,
        username: &str,
        password: &str,
    ) -> impl Future<Output = Result<JellyfinProfile, FederatedError>> + Send;
}

/// Best-effort auto-link of the per-user media connection. Implementations
/// swallow their own failures: a failed link must never fail the login.
/// [`NoopJellyfinLink`] covers deployments without a connections store.
pub trait JellyfinConnectionLink: Clone + Send + Sync + 'static {
    /// Store the fresh user-scoped token for later playback.
    fn link(&self, user_id: &str, profile: &JellyfinProfile) -> impl Future<Output = ()> + Send;
}

/// No connections store: linking is a no-op.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoopJellyfinLink;

impl JellyfinConnectionLink for NoopJellyfinLink {
    async fn link(&self, _user_id: &str, _profile: &JellyfinProfile) {}
}

/// Jellyfin login service. Generic over stores so tests inject fakes.
#[derive(Debug, Clone)]
pub struct JellyfinLogin<S, I, L, N> {
    users: S,
    idp: I,
    links: L,
    sessions: N,
}

impl<S, I, L, N> JellyfinLogin<S, I, L, N> {
    /// Wire the service from its ports.
    pub fn new(users: S, idp: I, links: L, sessions: N) -> Self {
        Self {
            users,
            idp,
            links,
            sessions,
        }
    }
}

impl<S, I, L, N> JellyfinLogin<S, I, L, N>
where
    S: FederatedUserStore,
    I: JellyfinIdp,
    L: JellyfinConnectionLink,
    N: SessionIssuer,
{
    /// Check credentials, import the user, auto-link the connection, and
    /// mint a session.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        user_agent: Option<&str>,
    ) -> Result<(StoredUser, String), FederatedError> {
        let profile = self.authenticate_credentials(username, password).await?;
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
        self.links.link(&user.id, &profile).await;
        let raw_token = self.sessions.issue_session(&user.id, user_agent).await?;
        Ok((user, raw_token))
    }

    /// Check credentials with no DroppedNeedle side effects (used by the
    /// per-user connection link flow).
    pub async fn authenticate_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<JellyfinProfile, FederatedError> {
        if !self.idp.is_configured() {
            return Err(FederatedError::Authentication(
                "Jellyfin is not configured on this server".to_owned(),
            ));
        }
        self.idp.authenticate_by_name(username, password).await
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
