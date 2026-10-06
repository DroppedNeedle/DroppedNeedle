//! The live plex.tv client for the PIN sign-in and the admin user import.
//!
//! Account calls go to plex.tv with the user's own token; the machine id
//! comes from the admin's configured server. Settings are read per call.
//! Tokens never reach a log line; transport errors are logged without
//! their URL.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};

use super::FederatedError;
use super::plex::{PRODUCT, PlexAccount, PlexPin, PlexPinClient};
use super::plex_models::{AccountListWire, AccountWire, PinPollWire, PinWire};
use super::settings::{InstallIds, plex_login_enabled};
use super::users::PROVIDER_PLEX;
use crate::auth::users::stores::{BoxFuture, DirectoryError, DirectoryUser, UserDirectory};
use crate::remotes::plex::{PLEX_TV_BASE, PlexAdapter};
use crate::remotes::plex_models::Resource;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::PlexConnection;

/// PIN create and account calls (v2 parity).
const TIMEOUT: Duration = Duration::from_secs(15);
/// PIN poll (v2 parity).
const POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// Live [`PlexPinClient`] and Plex [`UserDirectory`].
#[derive(Clone)]
pub struct PlexTv {
    http: reqwest::Client,
    store: Arc<ConfigStore>,
    ids: InstallIds,
    base: String,
}

impl PlexTv {
    /// Build against the real plex.tv.
    pub fn new(http: reqwest::Client, store: Arc<ConfigStore>) -> Self {
        Self::with_base(http, store, PLEX_TV_BASE)
    }

    /// Build against another plex.tv root (tests point this at a mock).
    pub fn with_base(http: reqwest::Client, store: Arc<ConfigStore>, base: &str) -> Self {
        Self {
            ids: InstallIds::new(store.clone()),
            http,
            store,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// Headers for one plex.tv call, with an account token when given.
    fn headers(&self, token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("X-Plex-Product", HeaderValue::from_static(PRODUCT));
        headers.insert("X-Plex-Version", HeaderValue::from_static("1.0"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if let Ok(value) = HeaderValue::from_str(&self.ids.plex_client_id()) {
            headers.insert("X-Plex-Client-Identifier", value);
        }
        if let Some(token) = token
            && let Ok(value) = HeaderValue::from_str(token)
        {
            headers.insert("X-Plex-Token", value);
        }
        headers
    }

    /// One account-scoped GET; JSON body on 200.
    async fn get_json(
        &self,
        path: &str,
        token: &str,
        query: &[(&str, &str)],
    ) -> Result<serde_json::Value, FederatedError> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .query(query)
            .headers(self.headers(Some(token)))
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(FederatedError::Authentication(format!(
                "Plex authentication failed ({status})"
            )));
        }
        if status != reqwest::StatusCode::OK {
            return Err(FederatedError::ProviderUnavailable(format!(
                "Plex request failed ({status})"
            )));
        }
        let bytes = response.bytes().await.map_err(unreachable)?;
        serde_json::from_slice(&bytes).map_err(|_| {
            FederatedError::ProviderUnavailable(format!("Plex returned invalid JSON for {path}"))
        })
    }

    /// The account's devices from `/resources`. Client devices carry no
    /// token, so each entry decodes on its own.
    async fn resources(&self, auth_token: &str) -> Result<Vec<Resource>, FederatedError> {
        let value = self
            .get_json(
                "/resources",
                auth_token,
                &[("includeHttps", "1"), ("includeRelay", "1")],
            )
            .await?;
        let serde_json::Value::Array(devices) = value else {
            tracing::warn!("unexpected Plex /resources response shape");
            return Err(FederatedError::ProviderUnavailable(
                "Unexpected Plex /resources response shape".to_owned(),
            ));
        };
        Ok(devices
            .into_iter()
            .filter_map(|device| serde_json::from_value::<Resource>(device).ok())
            .collect())
    }

    /// One admin-token account list for the import (`/home/users` or
    /// `/friends`).
    async fn account_list(&self, path: &str, token: &str) -> Result<Vec<AccountWire>, String> {
        let value = self
            .get_json(path, token, &[])
            .await
            .map_err(|error| error.to_string())?;
        serde_json::from_value::<AccountListWire>(value)
            .map(AccountListWire::into_accounts)
            .map_err(|_| format!("unexpected Plex {path} response shape"))
    }
}

fn unreachable(cause: reqwest::Error) -> FederatedError {
    tracing::debug!(cause = %cause.without_url(), "plex.tv request failed");
    FederatedError::ProviderUnavailable("Could not reach plex.tv".to_owned())
}

fn is_server(device: &Resource) -> bool {
    device
        .provides
        .as_deref()
        .is_some_and(|provides| provides.contains("server"))
}

impl PlexPinClient for PlexTv {
    fn client_id(&self) -> String {
        self.ids.plex_client_id()
    }

    fn login_enabled(&self) -> bool {
        plex_login_enabled(&self.store)
    }

    async fn create_pin(&self) -> Result<PlexPin, FederatedError> {
        let response = self
            .http
            .post(format!("{}/pins", self.base))
            .headers(self.headers(None))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body("strong=true")
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        if response.status() != reqwest::StatusCode::CREATED {
            tracing::warn!(status = %response.status(), "plex.tv refused to create a PIN");
            return Err(FederatedError::ProviderUnavailable(format!(
                "Failed to create OAuth pin ({})",
                response.status()
            )));
        }
        let bytes = response.bytes().await.map_err(unreachable)?;
        let pin: PinWire = serde_json::from_slice(&bytes).map_err(|_| {
            FederatedError::ProviderUnavailable("plex.tv returned an unreadable PIN".to_owned())
        })?;
        Ok(PlexPin {
            id: pin.id,
            code: pin.code,
        })
    }

    async fn poll_pin(&self, pin_id: i64) -> Result<Option<String>, FederatedError> {
        let response = self
            .http
            .get(format!("{}/pins/{pin_id}", self.base))
            .headers(self.headers(None))
            .timeout(POLL_TIMEOUT)
            .send()
            .await
            .map_err(unreachable)?;
        // v2 parity: any non-200 reads as "not authorized yet".
        if response.status() != reqwest::StatusCode::OK {
            return Ok(None);
        }
        let bytes = response.bytes().await.map_err(unreachable)?;
        let poll: PinPollWire = serde_json::from_slice(&bytes).map_err(|_| {
            FederatedError::ProviderUnavailable("plex.tv returned an unreadable PIN".to_owned())
        })?;
        Ok(poll.auth_token.filter(|token| !token.is_empty()))
    }

    async fn account_profile(&self, auth_token: &str) -> Result<PlexAccount, FederatedError> {
        let value = self.get_json("/user", auth_token, &[]).await?;
        let account: AccountWire = serde_json::from_value(value).map_err(|_| {
            FederatedError::ProviderUnavailable("Plex /user response missing uuid".to_owned())
        })?;
        let uuid = account.uuid.trim().to_owned();
        if uuid.is_empty() {
            return Err(FederatedError::ProviderUnavailable(
                "Plex /user response missing uuid".to_owned(),
            ));
        }
        Ok(PlexAccount {
            display_name: account.profile_name(),
            email: account.email().unwrap_or_default(),
            thumb: account.thumb(),
            uuid,
        })
    }

    async fn server_machine_id(&self) -> Option<String> {
        let section = match self.store.get_raw::<PlexConnection>() {
            Ok(section) => section,
            Err(error) => {
                tracing::warn!(%error, "cannot read plex settings");
                return None;
            }
        };
        if !section.enabled || section.plex_url.is_empty() {
            return None;
        }
        let server = PlexAdapter::new(
            self.http.clone(),
            section.plex_url,
            section.plex_token.expose().to_owned(),
            self.ids.plex_client_id(),
            Vec::new(),
        );
        match server.machine_identifier().await {
            Ok(machine_id) => machine_id,
            Err(error) => {
                tracing::warn!(%error, "could not read the Plex server machine id");
                None
            }
        }
    }

    async fn account_server_ids(&self, auth_token: &str) -> Result<Vec<String>, FederatedError> {
        Ok(self
            .resources(auth_token)
            .await?
            .into_iter()
            .filter(is_server)
            .filter_map(|device| device.client_identifier)
            .collect())
    }

    async fn server_access_token(
        &self,
        auth_token: &str,
        machine_id: &str,
    ) -> Result<Option<String>, FederatedError> {
        Ok(self
            .resources(auth_token)
            .await?
            .into_iter()
            .find(|device| {
                is_server(device) && device.client_identifier.as_deref() == Some(machine_id)
            })
            .and_then(|device| device.access_token)
            .filter(|token| !token.is_empty()))
    }
}

impl UserDirectory for PlexTv {
    fn provider(&self) -> &'static str {
        PROVIDER_PLEX
    }

    /// Plex Home users plus friends, merged by uuid, read with the admin's
    /// token. One list failing still returns the other (v2 parity); both
    /// failing is an outage.
    fn list_users(&self) -> BoxFuture<'_, Result<Vec<DirectoryUser>, DirectoryError>> {
        Box::pin(async move {
            let section = self.store.get_raw::<PlexConnection>().map_err(|error| {
                tracing::warn!(%error, "cannot read plex settings for user import");
                DirectoryError::NotConfigured
            })?;
            let token = section.plex_token.expose().to_owned();
            if token.is_empty() {
                return Err(DirectoryError::NotConfigured);
            }
            let home = self.account_list("/home/users", &token).await;
            let friends = self.account_list("/friends", &token).await;
            if let (Err(home), Err(friends)) = (&home, &friends) {
                tracing::warn!(%home, %friends, "plex user lists unavailable");
                return Err(DirectoryError::Transport);
            }
            let mut seen = HashSet::new();
            let mut users = Vec::new();
            for list in [home, friends] {
                let accounts = list.unwrap_or_else(|error| {
                    tracing::warn!(%error, "one plex user list unavailable; importing the other");
                    Vec::new()
                });
                for account in accounts {
                    let uuid = account.uuid.trim().to_owned();
                    if seen.insert(uuid.clone()) {
                        users.push(DirectoryUser {
                            display_name: account.directory_name(),
                            avatar_url: account.thumb(),
                            email: account.email(),
                            provider_uid: uuid,
                        });
                    }
                }
            }
            Ok(users)
        })
    }
}
