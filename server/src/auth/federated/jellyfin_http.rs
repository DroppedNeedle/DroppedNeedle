//! The live Jellyfin client for sign-in and the admin user import.
//!
//! Settings are read per call. Passwords and tokens never reach a log line;
//! transport errors are logged without their URL.

use std::sync::Arc;
use std::time::Duration;

use super::FederatedError;
use super::jellyfin_login::{JellyfinIdp, JellyfinProfile, emby_auth_header};
use super::jellyfin_models::{AuthenticationResultWire, UserWire};
use super::settings::{InstallIds, jellyfin_login_server};
use super::users::PROVIDER_JELLYFIN;
use crate::auth::users::stores::{BoxFuture, DirectoryError, DirectoryUser, UserDirectory};
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::JellyfinConnection;

/// Request timeout (v2 parity).
const TIMEOUT: Duration = Duration::from_secs(15);

/// Live [`JellyfinIdp`] and Jellyfin [`UserDirectory`].
#[derive(Clone)]
pub struct JellyfinHttp {
    http: reqwest::Client,
    store: Arc<ConfigStore>,
    ids: InstallIds,
}

impl JellyfinHttp {
    /// Build over the shared outbound client and the live settings.
    pub fn new(http: reqwest::Client, store: Arc<ConfigStore>) -> Self {
        Self {
            ids: InstallIds::new(store.clone()),
            http,
            store,
        }
    }
}

fn avatar_url(base_url: &str, user: &UserWire) -> Option<String> {
    user.has_avatar()
        .then(|| format!("{base_url}/Users/{}/Images/Primary", user.id))
}

impl JellyfinIdp for JellyfinHttp {
    fn is_configured(&self) -> bool {
        jellyfin_login_server(&self.store).is_some()
    }

    async fn authenticate_by_name(
        &self,
        username: &str,
        password: &str,
    ) -> Result<JellyfinProfile, FederatedError> {
        let Some(base_url) = jellyfin_login_server(&self.store) else {
            return Err(FederatedError::Authentication(
                "Jellyfin is not configured on this server".to_owned(),
            ));
        };
        let body = serde_json::json!({ "Username": username, "Pw": password });
        let response = self
            .http
            .post(format!("{base_url}/Users/AuthenticateByName"))
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
        let bytes = response.bytes().await.map_err(|_| {
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
            avatar_url: avatar_url(&base_url, &result.user),
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

impl UserDirectory for JellyfinHttp {
    fn provider(&self) -> &'static str {
        PROVIDER_JELLYFIN
    }

    fn list_users(&self) -> BoxFuture<'_, Result<Vec<DirectoryUser>, DirectoryError>> {
        Box::pin(async move {
            let section = self
                .store
                .get_raw::<JellyfinConnection>()
                .map_err(|error| {
                    tracing::warn!(%error, "cannot read jellyfin settings for user import");
                    DirectoryError::NotConfigured
                })?;
            let base_url = section.jellyfin_url.trim_end_matches('/');
            let api_key = section.api_key.expose();
            if !section.enabled || base_url.is_empty() || api_key.is_empty() {
                return Err(DirectoryError::NotConfigured);
            }
            let response = self
                .http
                .get(format!("{base_url}/Users"))
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
            let bytes = response
                .bytes()
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
                    avatar_url: Some(format!("{base_url}/Users/{}/Images/Primary", user.id)),
                    display_name: user.name.unwrap_or_else(|| "Unknown".to_owned()),
                    provider_uid: user.id,
                    email: None,
                })
                .collect())
        })
    }
}
