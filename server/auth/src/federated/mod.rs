//! Federated login: OIDC, Jellyfin, and the unified Plex journey, plus v2
//! password-hash import with Argon2id rehash.
//!
//! OIDC keeps PKCE states and the
//! short-lived exchange code; Jellyfin keeps the per-user connection
//! auto-link; Plex unifies v2's three PIN flows (login, link, settings
//! OAuth) into one journey. v2 bcrypt rows verify once and rehash
//! to Argon2id on success; sessions never survive import.
//!
//! Ported from v2's OIDC, Jellyfin, Plex and local auth services, the auth
//! store's username derivation (`_derive_username`, `_slugify`), and the
//! auth, connection and Plex auth routes.
//!
//! Each provider has one concrete client in a `*_http` module that talks
//! through the shared outbound client: [`oidc_http::OidcHttp`] at the
//! issuer and endpoints the OIDC settings and discovery name,
//! [`jellyfin_http::JellyfinHttp`] at the Jellyfin settings' server, and
//! [`plex_http::PlexTv`] at the plex.tv root the server's outbound
//! addresses give. Wire shapes live in the matching `*_models` module. The
//! integration tests point the same clients at local mock servers.
//!
//! Contracts with the rest of auth:
//!
//! - A federated login's session comes from
//!   [`Sessions::issue_session`](crate::sqlite::Sessions::issue_session):
//!   a fresh token, a `Standard` row and a `last_login_at` update.
//! - Imported password hashes verify through [`crate::passwords::Passwords`],
//!   over the `bcrypt` and `argon2` crates.
//! - Federated accounts are read and written through
//!   [`FederatedUsers`](crate::sqlite::FederatedUsers), which seals
//!   `token_json` with the deployment key before writing `provider_data`.

pub mod jellyfin_http;
pub mod jellyfin_login;
pub mod jellyfin_models;
pub mod jwt;
pub mod oidc;
pub mod oidc_http;
pub mod oidc_models;
pub mod password_import;
pub mod plex;
pub mod plex_http;
pub mod plex_models;
pub mod settings;
pub mod transport;
pub mod users;

use std::sync::Arc;

use reqwest::Url;
use thiserror::Error;

use self::jellyfin_login::JellyfinProfile;
use self::plex::PlexProfile;
use crate::users::stores::BoxFuture;

/// Failure of a federated login or import step. HTTP mapping lives in the
/// handler layer: `Authentication` is 401 on login routes (403 on the Plex
/// poll, v2 parity), `NotConfigured` and `ProviderUnavailable` are 503,
/// `StoreUnavailable` and `RngUnavailable` are 500 with a fixed body.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FederatedError {
    /// Bad credential, bad/expired state or exchange code, rejected
    /// membership, or any other caller fault. Messages mirror v2 verbatim.
    #[error("authentication failed: {0}")]
    Authentication(String),
    /// The remote IdP failed or answered unexpectedly (v2
    /// `ExternalServiceError`).
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String),
    /// The provider is disabled or half-configured (v2
    /// `ConfigurationError`).
    #[error("not configured: {0}")]
    NotConfigured(String),
    /// Local persistence failed.
    #[error("store unavailable: {0}")]
    StoreUnavailable(String),
    /// A username insert raced a concurrent create; the caller re-derives.
    #[error("username taken")]
    UsernameTaken,
    /// The OS random source failed; the request fails, never falls back.
    #[error("random source unavailable")]
    RngUnavailable,
}

/// Saves the media link a Plex or Jellyfin sign-in hands back into the
/// user's media connections. A trait because it cuts a crate dependency:
/// the connection store lives in the media crate, above auth.
pub trait MediaLinkStore: Send + Sync {
    /// Store the user's fresh Plex tokens for later playback.
    fn link_plex<'a>(
        &'a self,
        user_id: &'a str,
        profile: &'a PlexProfile,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// Store the user's fresh Jellyfin token for later playback.
    fn link_jellyfin<'a>(
        &'a self,
        user_id: &'a str,
        profile: &'a JellyfinProfile,
    ) -> BoxFuture<'a, Result<(), String>>;
}

/// The media links the Plex and Jellyfin sign-ins write through. Whether a
/// failed link fails the request is the flow's call, not this type's.
#[derive(Clone)]
pub struct MediaLinks(Arc<dyn MediaLinkStore>);

impl MediaLinks {
    /// Write links through `store`.
    pub fn new(store: Arc<dyn MediaLinkStore>) -> Self {
        Self(store)
    }

    /// Store the user's fresh Plex tokens for later playback.
    pub async fn link_plex(&self, user_id: &str, profile: &PlexProfile) -> Result<(), String> {
        self.0.link_plex(user_id, profile).await
    }

    /// Store the user's fresh Jellyfin token for later playback.
    pub async fn link_jellyfin(
        &self,
        user_id: &str,
        profile: &JellyfinProfile,
    ) -> Result<(), String> {
        self.0.link_jellyfin(user_id, profile).await
    }
}

/// `raw` as a web address: http or https, with a host and no credentials.
/// Anything else is `None`.
pub fn web_address(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    let usable = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none();
    usable.then_some(url)
}

/// The non-empty `segments` appended to the path of `base`, each
/// percent-encoded as one segment, with the base's query and fragment
/// dropped. A value from a server's answer goes in here, never into the URL
/// as text.
pub fn endpoint(base: &Url, segments: &[&str]) -> Url {
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    if let Ok(mut path) = url.path_segments_mut() {
        path.pop_if_empty();
        path.extend(segments.iter().filter(|segment| !segment.is_empty()));
    }
    url
}

/// Append `value` as one JSON string literal (quotes included).
pub fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    json_escape_into(&mut out, value);
    out.push('"');
    out
}

/// Escape `value` for embedding in a JSON string (no surrounding quotes).
pub fn json_escape_into(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}
