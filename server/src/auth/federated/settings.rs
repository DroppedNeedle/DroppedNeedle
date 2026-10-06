//! Live reads of the sign-in settings: which providers are on, the OIDC
//! connection, and the stable install ids sent to Plex and Jellyfin.
//!
//! Every read goes to the config store at call time, so an admin's change
//! applies to the next login without a restart.

use std::sync::Arc;

use super::oidc::OidcConfig;
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::{JellyfinConnection, OidcConnection, PlexConnection};
use crate::runtime_config::sections::InternalState;

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

/// Read the sign-in switches. An unreadable section reads as off.
pub fn enabled_providers(store: &ConfigStore) -> EnabledProviders {
    EnabledProviders {
        local: true,
        plex: plex_login_enabled(store),
        jellyfin: jellyfin_login_server(store).is_some(),
        oidc: oidc_config(store).is_usable(),
    }
}

/// True when the admin turned Plex login on. Plex login works without a
/// configured server (anyone with access to the shared server gets in
/// when there is one; v2 parity), so only the switch counts.
pub fn plex_login_enabled(store: &ConfigStore) -> bool {
    match store.get_raw::<PlexConnection>() {
        Ok(section) => section.login_enabled,
        Err(error) => {
            tracing::warn!(%error, "cannot read plex settings; plex login is off");
            false
        }
    }
}

/// The Jellyfin base URL when Jellyfin login is on and the server is set
/// up, else `None`.
pub fn jellyfin_login_server(store: &ConfigStore) -> Option<String> {
    match store.get_raw::<JellyfinConnection>() {
        Ok(section) if section.enabled && section.login_enabled => {
            let url = section.jellyfin_url.trim_end_matches('/');
            (!url.is_empty()).then(|| url.to_owned())
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
pub fn oidc_config(store: &ConfigStore) -> OidcConfig {
    match store.get_raw::<OidcConnection>() {
        Ok(connection) => {
            let secret = connection.client_secret.expose();
            OidcConfig {
                enabled: connection.enabled,
                issuer: connection.issuer,
                client_id: connection.client_id,
                client_secret: (!secret.is_empty()).then(|| secret.to_owned()),
                redirect_uri: connection.redirect_uri,
                scopes: connection.scopes,
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

/// Install id used when none is stored (matches the remotes fallback).
const FALLBACK_DEVICE_ID: &str = "droppedneedle";

/// The stable ids this install presents to Plex and Jellyfin.
#[derive(Clone)]
pub struct InstallIds {
    store: Arc<ConfigStore>,
}

impl InstallIds {
    /// Read ids from the shared store.
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self { store }
    }

    /// Create any missing id. Called once at boot so request paths only
    /// read (v2 created them lazily with `get_or_create_setting`).
    pub fn ensure(&self) {
        let mut internal = match self.store.get::<InternalState>() {
            Ok(internal) => internal,
            Err(error) => {
                tracing::warn!(%error, "cannot read install ids; using the fallback");
                return;
            }
        };
        if internal.plex_client_id.is_some() && internal.droppedneedle_device_id.is_some() {
            return;
        }
        internal
            .plex_client_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
        internal
            .droppedneedle_device_id
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string());
        if let Err(error) = self.store.save(internal) {
            tracing::warn!(%error, "cannot store install ids; using the fallback");
        }
    }

    /// `X-Plex-Client-Identifier` for plex.tv calls.
    pub fn plex_client_id(&self) -> String {
        self.store
            .get::<InternalState>()
            .ok()
            .and_then(|internal| internal.plex_client_id)
            .unwrap_or_else(|| FALLBACK_DEVICE_ID.to_owned())
    }

    /// `DeviceId` for Jellyfin calls.
    pub fn jellyfin_device_id(&self) -> String {
        self.store
            .get::<InternalState>()
            .ok()
            .and_then(|internal| internal.droppedneedle_device_id)
            .unwrap_or_else(|| FALLBACK_DEVICE_ID.to_owned())
    }
}
