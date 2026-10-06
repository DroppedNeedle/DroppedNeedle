//! Unified Plex journey: one PIN start plus one poll per purpose.
//!
//! v2 ran three PIN flows through two services and three route pairs:
//! login (`/auth/plex/pin` + `/auth/plex/poll`, membership gated only
//! when a server is configured, issues a session), link
//! (`/me/connections/plex/auth/*`, machine id required, mandatory
//! membership gate, returns the profile with no login side effects), and
//! settings OAuth (`/plex/auth/*`, returns the raw auth token). They all
//! minted the same PIN and the same `app.plex.tv` URL; only the poll
//! completion differed.
//!
//! This module keeps one [`PlexJourney`] with one [`PlexJourney::start`]
//! and three poll methods. The per-purpose completion rules are the v2
//! rules verbatim, so each old flow maps to exactly one method:
//! login to [`PlexJourney::poll_login`], link to [`PlexJourney::poll_link`],
//! settings to [`PlexJourney::poll_connect`].
//!
//! One rule is new: login starts and completes only while the admin has
//! Plex login switched on. v2 hid the tab but still accepted the calls,
//! which let any Plex account create a user when no server was set up.

use super::users::{
    FederatedProfile, FederatedUserStore, PROVIDER_PLEX, StoredUser, find_or_create_federated_user,
};
use super::{FederatedError, SessionIssuer, json_string};

/// Product token in the `app.plex.tv` URL.
pub const PRODUCT: &str = "DroppedNeedle";

/// A freshly minted PIN awaiting authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexPin {
    /// PIN id for polling.
    pub id: i64,
    /// Short code embedded in the auth URL.
    pub code: String,
}

/// Plex account facts behind an auth token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexAccount {
    /// Plex uuid; the provider uid.
    pub uuid: String,
    /// Account email; empty when the API omits it.
    pub email: String,
    /// Account display name.
    pub display_name: String,
    /// Account avatar URL.
    pub thumb: Option<String>,
}

/// Verified profile returned by the link flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexProfile {
    /// Plex uuid; the provider uid.
    pub uuid: String,
    /// Account email; empty when the API omits it.
    pub email: String,
    /// Account display name.
    pub display_name: String,
    /// Account avatar URL.
    pub thumb: Option<String>,
    /// The authorized account token.
    pub auth_token: String,
    /// Server-scoped token; empty when no server is configured (login).
    pub server_access_token: String,
}

/// Network edge for the Plex PIN flow. The production adapter speaks to
/// `plex.tv` (PIN create/poll, account profile, resources) and to the
/// configured server (machine identifier). Transport failures surface as
/// [`FederatedError::ProviderUnavailable`]; the journey maps them to the
/// v2 user-facing messages at each step.
pub trait PlexPinClient: Clone + Send + Sync + 'static {
    /// Stable install id (`plex_client_id` setting).
    fn client_id(&self) -> String;

    /// The admin's Plex login switch, read live.
    fn login_enabled(&self) -> bool;

    /// Mint a PIN.
    fn create_pin(&self) -> impl Future<Output = Result<PlexPin, FederatedError>> + Send;

    /// Poll a PIN: `Ok(None)` while pending, `Ok(Some(token))` once the
    /// user authorizes.
    fn poll_pin(
        &self,
        pin_id: i64,
    ) -> impl Future<Output = Result<Option<String>, FederatedError>> + Send;

    /// Fetch the account profile behind an auth token.
    fn account_profile(
        &self,
        auth_token: &str,
    ) -> impl Future<Output = Result<PlexAccount, FederatedError>> + Send;

    /// Configured server machine id; `None` when Plex is disabled or the
    /// server is unreachable (v2 swallows this lookup: it never fails).
    fn server_machine_id(&self) -> impl Future<Output = Option<String>> + Send;

    /// Machine ids of servers this account can access.
    fn account_server_ids(
        &self,
        auth_token: &str,
    ) -> impl Future<Output = Result<Vec<String>, FederatedError>> + Send;

    /// Server-scoped token for this account; `None` when unresolvable.
    fn server_access_token(
        &self,
        auth_token: &str,
        machine_id: &str,
    ) -> impl Future<Output = Result<Option<String>, FederatedError>> + Send;
}

/// Stores a user's Plex media link (the per-user connection used for
/// playback). The login flow treats a failure as a warning; the link flow,
/// where linking is the whole point, fails the request.
pub trait PlexConnectionLink: Clone + Send + Sync + 'static {
    /// Store the fresh user-scoped tokens for later playback.
    fn link(
        &self,
        user_id: &str,
        profile: &PlexProfile,
    ) -> impl Future<Output = Result<(), String>> + Send;
}

/// Poll outcome: still pending, or finished with a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlexPoll<T> {
    /// The user has not authorized the PIN yet.
    Pending,
    /// The PIN authorized; carries the purpose-specific result.
    Complete(T),
}

/// Which flow a Plex start serves. Link and connect starts need a
/// configured Plex server (v2 400s without one); login starts never gate
/// (v2 parity: login works server-less).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlexPurpose {
    /// Login flow: gate-free, works with no server configured.
    Login,
    /// Per-user connection link: needs the configured server.
    Link,
    /// Settings OAuth: needs the configured server.
    Connect,
}

impl PlexPurpose {
    /// True when the start must refuse without a configured server.
    pub fn needs_server(self) -> bool {
        !matches!(self, Self::Login)
    }
}

/// Why a purpose-gated start failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlexStartDenied {
    /// Link/connect start with no Plex server configured (the route maps
    /// this to 400 with the v2 message verbatim).
    NotConfigured,
    /// PIN creation failed (the route maps this like [`PlexJourney::start`]).
    StartFailed(FederatedError),
}

/// The unified journey. Generic over stores so tests inject fakes.
#[derive(Debug, Clone)]
pub struct PlexJourney<S, C, L, N> {
    users: S,
    client: C,
    links: L,
    sessions: N,
}

impl<S, C, L, N> PlexJourney<S, C, L, N> {
    /// Wire the journey from its ports.
    pub fn new(users: S, client: C, links: L, sessions: N) -> Self {
        Self {
            users,
            client,
            links,
            sessions,
        }
    }
}

impl<S, C, L, N> PlexJourney<S, C, L, N>
where
    S: FederatedUserStore,
    C: PlexPinClient,
    L: PlexConnectionLink,
    N: SessionIssuer,
{
    /// Start any of the three flows: mint a PIN and its browser URL.
    pub async fn start(&self) -> Result<(i64, String), FederatedError> {
        let pin = match self.client.create_pin().await {
            Ok(pin) => pin,
            Err(_) => {
                return Err(FederatedError::Authentication(
                    "Could not start Plex authentication".to_owned(),
                ));
            }
        };
        Ok((pin.id, plex_auth_url(&self.client.client_id(), &pin.code)))
    }

    /// Start gated by purpose: link/connect refuse without a configured
    /// Plex server (no resolvable machine id); login needs the admin's
    /// Plex login switch.
    pub async fn start_for_purpose(
        &self,
        purpose: PlexPurpose,
    ) -> Result<(i64, String), PlexStartDenied> {
        if purpose == PlexPurpose::Login && !self.client.login_enabled() {
            return Err(PlexStartDenied::StartFailed(login_disabled()));
        }
        if purpose.needs_server() && self.client.server_machine_id().await.is_none() {
            return Err(PlexStartDenied::NotConfigured);
        }
        self.start().await.map_err(PlexStartDenied::StartFailed)
    }

    /// Login flow: poll, gate membership when a server is configured,
    /// import the user, auto-link, and mint a session.
    pub async fn poll_login(
        &self,
        pin_id: i64,
        user_agent: Option<&str>,
    ) -> Result<PlexPoll<(StoredUser, String)>, FederatedError> {
        if !self.client.login_enabled() {
            return Err(login_disabled());
        }
        let Some(auth_token) = self.client.poll_pin(pin_id).await? else {
            return Ok(PlexPoll::Pending);
        };
        let profile = self.verified_profile(&auth_token, false).await?;
        let email = if profile.email.is_empty() {
            None
        } else {
            Some(profile.email.clone())
        };
        let user = find_or_create_federated_user(
            &self.users,
            PROVIDER_PLEX,
            &FederatedProfile {
                provider_uid: profile.uuid.clone(),
                display_name: profile.display_name.clone(),
                email,
                // plex.tv does not say whether the address was confirmed,
                // so it never links or claims an existing account.
                email_verified: false,
                avatar_url: profile.thumb.clone(),
                token_json: plex_token_json(&profile.auth_token),
            },
        )
        .await?;
        // v2 parity: a failed auto-link never fails the sign-in.
        if let Err(error) = self.links.link(&user.id, &profile).await {
            tracing::warn!(%error, "could not link the signed-in Plex account; the user can link it by hand");
        }
        let raw_token = self.sessions.issue_session(&user.id, user_agent).await?;
        Ok(PlexPoll::Complete((user, raw_token)))
    }

    /// Link flow: poll, verify the profile and store it as `user_id`'s
    /// Plex media link. The machine id is required and the membership gate
    /// is mandatory; no session is minted.
    pub async fn poll_link(
        &self,
        pin_id: i64,
        user_id: &str,
    ) -> Result<PlexPoll<PlexProfile>, FederatedError> {
        let Some(auth_token) = self.client.poll_pin(pin_id).await? else {
            return Ok(PlexPoll::Pending);
        };
        let profile = self.verified_profile(&auth_token, true).await?;
        self.links
            .link(user_id, &profile)
            .await
            .map_err(FederatedError::StoreUnavailable)?;
        Ok(PlexPoll::Complete(profile))
    }

    /// Settings flow: poll and return the raw auth token untouched.
    pub async fn poll_connect(&self, pin_id: i64) -> Result<PlexPoll<String>, FederatedError> {
        let token = self.client.poll_pin(pin_id).await?;
        Ok(match token {
            Some(token) => PlexPoll::Complete(token),
            None => PlexPoll::Pending,
        })
    }

    /// Fetch the profile and enforce the membership gate. When
    /// `require_machine` is set (link flow), a missing machine id fails;
    /// otherwise an unconfigured server skips the gate (login flow).
    async fn verified_profile(
        &self,
        auth_token: &str,
        require_machine: bool,
    ) -> Result<PlexProfile, FederatedError> {
        let account = match self.client.account_profile(auth_token).await {
            Ok(account) => account,
            Err(_) => {
                return Err(FederatedError::Authentication(
                    "Could not verify Plex account".to_owned(),
                ));
            }
        };
        let machine_id = self.client.server_machine_id().await;
        if machine_id.is_none() && require_machine {
            return Err(FederatedError::Authentication(
                "Could not verify the configured Plex server".to_owned(),
            ));
        }
        let mut server_access_token = String::new();
        if let Some(machine_id) = machine_id {
            let server_ids = match self.client.account_server_ids(auth_token).await {
                Ok(ids) => ids,
                Err(_) => {
                    return Err(FederatedError::Authentication(
                        "Could not verify server access".to_owned(),
                    ));
                }
            };
            if !server_ids.iter().any(|id| id == &machine_id) {
                return Err(FederatedError::Authentication(
                    "Your Plex account does not have access to this server".to_owned(),
                ));
            }
            server_access_token = match self
                .client
                .server_access_token(auth_token, &machine_id)
                .await
            {
                Ok(Some(token)) if !token.is_empty() => token,
                _ => {
                    return Err(FederatedError::Authentication(
                        "Could not verify Plex server access".to_owned(),
                    ));
                }
            };
        }
        Ok(PlexProfile {
            uuid: account.uuid,
            email: account.email,
            display_name: account.display_name,
            thumb: account.thumb,
            auth_token: auth_token.to_owned(),
            server_access_token,
        })
    }
}

fn login_disabled() -> FederatedError {
    FederatedError::NotConfigured("Plex login is not enabled".to_owned())
}

/// Browser URL for a PIN. The one builder all three flows share (both v2
/// spellings were already identical).
pub fn plex_auth_url(client_id: &str, pin_code: &str) -> String {
    format!(
        "https://app.plex.tv/auth#?clientID={client_id}&code={pin_code}&context%5Bdevice%5D%5Bproduct%5D={PRODUCT}"
    )
}

/// Plaintext token JSON for the store to seal (v2 field names kept).
pub fn plex_token_json(auth_token: &str) -> String {
    format!("{{\"auth_token\":{}}}", json_string(auth_token))
}
