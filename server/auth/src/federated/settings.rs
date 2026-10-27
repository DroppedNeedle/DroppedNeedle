//! Live reads of the sign-in settings: which providers are on, the OIDC
//! connection, and the stable install ids sent to Plex and Jellyfin.
//!
//! Every read goes through a live handle, so an admin's change applies to
//! the next login without a restart.

use std::sync::Arc;

use reqwest::Url;

use super::oidc::OidcConfig;
use super::web_address;
use droppedneedle_kernel::db::DurableLane;
use droppedneedle_kernel::runtime_config::secret_sections::{
    JellyfinConnection, OidcConnection, PlexConnection,
};
use droppedneedle_kernel::runtime_config::sections::InternalState;
use droppedneedle_kernel::runtime_config::{ConfigError, ConfigStore, Live, UpdateError};

/// Which sign-in methods the login page offers (v2 `AuthProvidersResponse`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnabledProviders {
    /// Username and password; always on.
    pub local: bool,
    /// Plex PIN login.
    pub plex: bool,
    /// Jellyfin credential login.
    pub jellyfin: bool,
    /// OIDC single sign-on.
    pub oidc: bool,
}

/// The sign-in sections, read live: an admin's change applies to the next
/// login without a restart.
#[derive(Clone, Debug)]
pub struct SignInSettings {
    plex: Live<PlexConnection>,
    jellyfin: Live<JellyfinConnection>,
    oidc: Live<OidcConnection>,
}

impl SignInSettings {
    /// Handles on the Plex, Jellyfin and OIDC sections.
    #[must_use]
    pub fn new(store: &ConfigStore) -> Self {
        Self {
            plex: store.live(),
            jellyfin: store.live(),
            oidc: store.live(),
        }
    }

    /// Read the sign-in switches. An unreadable section reads as off.
    #[must_use]
    pub fn enabled_providers(&self) -> EnabledProviders {
        EnabledProviders {
            local: true,
            plex: self.plex_login_enabled(),
            jellyfin: self.jellyfin_login_server().is_some(),
            oidc: self.oidc_config().is_usable(),
        }
    }

    /// True when the admin turned Plex login on. Plex login works without
    /// a configured server (anyone with access to the shared server gets
    /// in when there is one; v2 parity), so only the switch counts.
    #[must_use]
    pub fn plex_login_enabled(&self) -> bool {
        match self.plex.get() {
            Ok(section) => section.login_enabled,
            Err(error) => {
                tracing::warn!(%error, "cannot read plex settings; plex login is off");
                false
            }
        }
    }

    /// The Jellyfin server's address when Jellyfin login is on and the
    /// server is set up with a web address, else `None`.
    #[must_use]
    pub fn jellyfin_login_server(&self) -> Option<Url> {
        match self.jellyfin.get() {
            Ok(section) if section.enabled && section.login_enabled => {
                if section.jellyfin_url.trim().is_empty() {
                    return None;
                }
                let url = web_address(&section.jellyfin_url);
                if url.is_none() {
                    tracing::warn!(
                        "the saved Jellyfin address is not a web address; jellyfin login is off"
                    );
                }
                url
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "cannot read jellyfin settings; jellyfin login is off");
                None
            }
        }
    }

    /// The OIDC connection with the client secret decrypted. Never log it.
    /// An unreadable section reads as disabled.
    #[must_use]
    pub fn oidc_config(&self) -> OidcConfig {
        match self.oidc.get() {
            Ok(connection) => {
                let secret = connection.client_secret.expose();
                OidcConfig {
                    enabled: connection.enabled,
                    issuer: connection.issuer.clone(),
                    client_id: connection.client_id.clone(),
                    client_secret: (!secret.is_empty()).then(|| secret.to_owned()),
                    redirect_uri: connection.redirect_uri.clone(),
                    scopes: connection.scopes.clone(),
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cannot read oidc settings; oidc login is off");
                OidcConfig {
                    enabled: false,
                    issuer: String::new(),
                    client_id: String::new(),
                    client_secret: None,
                    redirect_uri: String::new(),
                    scopes: String::new(),
                }
            }
        }
    }
}

/// The stable ids this install presents to Plex and Jellyfin.
#[derive(Clone)]
pub struct InstallIds {
    store: Arc<ConfigStore>,
    /// Used only when no id could be stored: random per process, never a
    /// value shared with other installs.
    fallback: Arc<str>,
}

impl InstallIds {
    /// Read ids from the shared store.
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self {
            store,
            fallback: uuid::Uuid::new_v4().to_string().into(),
        }
    }

    fn stored_or_fallback(&self, pick: impl FnOnce(InternalState) -> Option<String>) -> String {
        match self.store.get::<InternalState>().ok().and_then(pick) {
            Some(id) => id,
            None => {
                tracing::warn!("no install id is stored; using a temporary one until restart");
                self.fallback.to_string()
            }
        }
    }

    /// Create any missing id, in one settings write that keeps ids already
    /// stored. Called once at boot so request paths only read (v2 created
    /// them lazily with `get_or_create_setting`).
    pub async fn ensure(&self) -> Result<(), ConfigError> {
        self.store
            .update::<InternalState, ConfigError, _>(DurableLane::Background, |mut internal| {
                internal
                    .plex_client_id
                    .get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
                internal
                    .droppedneedle_device_id
                    .get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
                Ok(internal)
            })
            .await
            .map(|_| ())
            .map_err(|error| match error {
                UpdateError::Rejected(error) | UpdateError::Config(error) => error,
            })
    }

    /// `X-Plex-Client-Identifier` for plex.tv calls.
    pub fn plex_client_id(&self) -> String {
        self.stored_or_fallback(|internal| internal.plex_client_id)
    }

    /// `DeviceId` for Jellyfin calls.
    pub fn jellyfin_device_id(&self) -> String {
        self.stored_or_fallback(|internal| internal.droppedneedle_device_id)
    }
}
