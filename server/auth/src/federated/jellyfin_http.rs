//! The live Jellyfin client for sign-in and the admin user import.
//!
//! The server's address comes from the Jellyfin settings, read live on
//! each call and parsed as a web address; every request URL is built from
//! that parsed base, and an id from the server's answer goes in as one
//! path segment. Passwords and tokens never reach a log line; transport
//! errors are logged without their URL.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;

use super::jellyfin_login::{JellyfinProfile, emby_auth_header};
use super::jellyfin_models::{AuthenticationResultWire, UserWire};
use super::settings::{InstallIds, SignInSettings};
use super::users::PROVIDER_JELLYFIN;
use super::{FederatedError, endpoint, web_address};
use crate::users::stores::{BoxFuture, DirectoryError, DirectoryUser, UserDirectory};
use droppedneedle_kernel::http_client::read_capped;
use droppedneedle_kernel::runtime_config::secret_sections::JellyfinConnection;
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};

/// Request timeout (v2 parity).
const TIMEOUT: Duration = Duration::from_secs(15);
/// Largest answer read from a Jellyfin server.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The Jellyfin client for sign-in, and the Jellyfin [`UserDirectory`].
#[derive(Clone)]
pub struct JellyfinHttp {
    http: reqwest::Client,
    sign_in: SignInSettings,
    connection: Live<JellyfinConnection>,
    ids: InstallIds,
}

impl JellyfinHttp {
    /// Build over the shared outbound client and the live settings.
    pub fn new(http: reqwest::Client, store: Arc<ConfigStore>) -> Self {
        Self {
            sign_in: SignInSettings::new(&store),
            connection: store.live(),
            ids: InstallIds::new(store),
            http,
        }
    }

    /// Check credentials with no DroppedNeedle side effects (v2 parity):
    /// POST `{base}/Users/AuthenticateByName` with [`emby_auth_header`] and
    /// `{"Username","Pw"}`. No configured server is
    /// `Authentication("Jellyfin is not configured on this server")`;
    /// timeouts and connect failures are `ProviderUnavailable`; 401 and 403
    /// are `Authentication`; any other non-2xx, bad JSON, or a missing user
    /// id or token is `ProviderUnavailable`.
    pub async fn authenticate_by_name(
        &self,
        username: &str,
        password: &str,
    ) -> Result<JellyfinProfile, FederatedError> {
        let Some(base_url) = self.sign_in.jellyfin_login_server() else {
            return Err(FederatedError::Authentication(
                "Jellyfin is not configured on this server".to_owned(),
            ));
        };
        let body = serde_json::json!({ "Username": username, "Pw": password });
        let response = self
            .http
            .post(endpoint(&base_url, &["Users", "AuthenticateByName"]))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::AUTHORIZATION,
                emby_auth_header(&self.ids.jellyfin_device_id()),
            )
            .body(body.to_string())
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|cause| {
                let message = if cause.is_timeout() {
                    "Jellyfin connection timed out"
                } else {
                    "Could not connect to Jellyfin"
                };
                tracing::debug!(cause = %cause.without_url(), "jellyfin sign-in request failed");
                FederatedError::ProviderUnavailable(message.to_owned())
            })?;
        match response.status().as_u16() {
            401 => {
                return Err(FederatedError::Authentication(
                    "Invalid Jellyfin username or password".to_owned(),
                ));
            }
            403 => {
                return Err(FederatedError::Authentication(
                    "This Jellyfin account does not have access".to_owned(),
                ));
            }
            200 | 204 => {}
            status => {
                tracing::debug!(status, "jellyfin AuthenticateByName answered an error");
                return Err(FederatedError::ProviderUnavailable(
                    "Jellyfin authentication failed".to_owned(),
                ));
            }
        }
        let bytes = read_capped(response, MAX_BODY_BYTES).await.map_err(|_| {
            FederatedError::ProviderUnavailable("Jellyfin authentication failed".to_owned())
        })?;
        let result: AuthenticationResultWire = serde_json::from_slice(&bytes).map_err(|_| {
            FederatedError::ProviderUnavailable(
                "Jellyfin returned an unexpected response".to_owned(),
            )
        })?;
        if result.user.id.is_empty() || result.access_token.is_empty() {
            return Err(FederatedError::ProviderUnavailable(
                "Jellyfin returned incomplete auth data".to_owned(),
            ));
        }
        Ok(JellyfinProfile {
            avatar_url: result
                .user
                .has_avatar()
                .then(|| avatar_url(&base_url, &result.user.id)),
            username: result
                .user
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| username.to_owned()),
            jellyfin_user_id: result.user.id,
            access_token: result.access_token,
        })
    }
}

/// The primary image of user `id` on the server at `base`.
fn avatar_url(base: &Url, id: &str) -> String {
    endpoint(base, &["Users", id, "Images", "Primary"]).into()
}

impl UserDirectory for JellyfinHttp {
    fn provider(&self) -> &'static str {
        PROVIDER_JELLYFIN
    }

    fn list_users(&self) -> BoxFuture<'_, Result<Vec<DirectoryUser>, DirectoryError>> {
        Box::pin(async move {
            let section = self.connection.get().map_err(|error| {
                tracing::warn!(%error, "cannot read jellyfin settings for user import");
                DirectoryError::NotConfigured
            })?;
            let api_key = section.api_key.expose();
            if !section.enabled || section.jellyfin_url.trim().is_empty() || api_key.is_empty() {
                return Err(DirectoryError::NotConfigured);
            }
            let Some(base_url) = web_address(&section.jellyfin_url) else {
                tracing::warn!("the saved Jellyfin address is not a web address");
                return Err(DirectoryError::NotConfigured);
            };
            let response = self
                .http
                .get(endpoint(&base_url, &["Users"]))
                .header(reqwest::header::ACCEPT, "application/json")
                .header(
                    reqwest::header::AUTHORIZATION,
                    format!("MediaBrowser Token=\"{api_key}\""),
                )
                .timeout(TIMEOUT)
                .send()
                .await
                .map_err(|cause| {
                    tracing::warn!(cause = %cause.without_url(), "jellyfin user list failed");
                    DirectoryError::Transport
                })?;
            if !response.status().is_success() {
                tracing::warn!(status = %response.status(), "jellyfin user list answered an error");
                return Err(DirectoryError::Transport);
            }
            let bytes = read_capped(response, MAX_BODY_BYTES)
                .await
                .map_err(|_| DirectoryError::Transport)?;
            let users: Vec<serde_json::Value> =
                serde_json::from_slice(&bytes).map_err(|_| DirectoryError::Transport)?;
            // A user without an id cannot be bound to an account; skip it.
            Ok(users
                .into_iter()
                .filter_map(|user| serde_json::from_value::<UserWire>(user).ok())
                .filter(|user| !user.id.is_empty())
                .map(|user| DirectoryUser {
                    // GET /Users may omit the avatar flags, so the picker
                    // always gets the URL and falls back on a 404 (v2).
                    avatar_url: Some(avatar_url(&base_url, &user.id)),
                    display_name: user.name.unwrap_or_else(|| "Unknown".to_owned()),
                    provider_uid: user.id,
                    email: None,
                })
                .collect())
        })
    }
}
