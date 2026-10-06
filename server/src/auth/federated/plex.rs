//! Unified Plex journey: one PIN start plus one poll per purpose.
//!
//! v2 ran three PIN flows through two services and three route pairs:
//! login (`/auth/plex/pin` + `/auth/plex/poll`, membership gated only
//! when a server is configured, issues a session), link
//! (`/me/connections/plex/auth/*`, machine id required, mandatory
//! membership gate, stores the link with no login side effects), and
//! settings OAuth (`/plex/auth/*`, returns the raw auth token). They all
//! minted the same PIN and the same `app.plex.tv` URL; only the poll
//! completion differed.
//!
//! This module keeps one [`PlexJourney`] with one [`PlexJourney::start`]
//! and three poll methods, one per v2 flow: login to
//! [`PlexJourney::poll_login`], link to [`PlexJourney::poll_link`],
//! settings to [`PlexJourney::poll_connect`].
//!
//! Rules that tighten v2:
//!
//! - Every PIN is bound to whoever started it. plex.tv PIN ids are
//!   sequential and v2 accepted any id on the poll, so a stranger could
//!   finish someone else's sign-in. Now the start mints a random secret,
//!   keeps only its hash with the PIN (and the starting user for link and
//!   connect), and every poll must present it. The first poll that sees
//!   the approval consumes the record.
//! - Login starts and completes only while the admin's Plex login switch
//!   is on (v2 only hid the tab).
//! - When a Plex server is configured, a login whose membership cannot be
//!   checked fails (v2 skipped the check when the server lookup failed).
//! - Connect (the settings flow) is admin only; link needs a session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::users::{
    FederatedProfile, FederatedUserStore, PROVIDER_PLEX, ROLE_ADMIN, StoredUser,
    find_or_create_federated_user,
};
use super::{FederatedError, SessionIssuer, json_string};
use crate::auth::session::tokens::{constant_time_eq, hash_token, mint_token};

/// Product token in the `app.plex.tv` URL.
pub const PRODUCT: &str = "DroppedNeedle";
/// How long a started PIN may be polled. plex.tv expires strong PINs
/// sooner; this only bounds our own records.
pub const PIN_TTL: Duration = Duration::from_secs(30 * 60);
/// Most PINs waiting at once. Starts are public, so the map is bounded:
/// when it is full of live PINs, new starts are refused until some finish
/// or expire.
pub const MAX_PENDING_PINS: usize = 10_000;

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

    /// The configured server's machine id. `Ok(None)` only when no Plex
    /// server URL is set; a configured server that cannot be read is an
    /// error, so the membership gate never fails open.
    fn server_machine_id(
        &self,
    ) -> impl Future<Output = Result<Option<String>, FederatedError>> + Send;

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

/// Which flow a Plex PIN serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlexPurpose {
    /// Login flow: public, works with no server configured.
    Login,
    /// Per-user connection link: needs a session and the configured server.
    Link,
    /// Settings OAuth: admin only.
    Connect,
}

/// Why a start failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlexStartDenied {
    /// Link start with no Plex server configured (the route maps this to
    /// 400 with the v2 message verbatim).
    NotConfigured,
    /// Connect start by a non-admin.
    Forbidden,
    /// PIN creation or the server lookup failed, or login is switched off.
    StartFailed(FederatedError),
    /// [`MAX_PENDING_PINS`] sign-ins are already waiting.
    Busy,
}

/// A started PIN: what the browser gets back, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexStart {
    /// PIN id to poll.
    pub pin_id: i64,
    /// `app.plex.tv` URL the user approves at.
    pub authorize_url: String,
    /// Proof that the poller started this PIN; sent on every poll.
    pub pin_secret: String,
}

/// Who is polling: the PIN, its secret, and the signed-in user (link and
/// connect only).
#[derive(Debug, Clone, Copy)]
pub struct PinClaim<'a> {
    /// PIN id from the start.
    pub pin_id: i64,
    /// Secret from the start.
    pub pin_secret: &'a str,
    /// The polling user, for link and connect.
    pub user_id: Option<&'a str>,
}

#[derive(Debug)]
struct PendingPin {
    purpose: PlexPurpose,
    secret_hash: String,
    user_id: Option<String>,
    started: Instant,
}

/// In-memory records of started PINs. A restart drops them; the user
/// starts again.
#[derive(Debug, Clone, Default)]
struct PendingPins(Arc<Mutex<HashMap<i64, PendingPin>>>);

fn unknown_pin() -> FederatedError {
    FederatedError::Authentication("Unknown or expired Plex sign-in".to_owned())
}

impl PendingPins {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<i64, PendingPin>>, PlexStartDenied> {
        self.0.lock().map_err(|_| {
            PlexStartDenied::StartFailed(FederatedError::StoreUnavailable(
                "plex pin lock".to_owned(),
            ))
        })
    }

    /// Make room as of `now`: drop expired PINs when the map is full, and
    /// refuse when it is still full.
    fn make_room(pins: &mut HashMap<i64, PendingPin>, now: Instant) -> Result<(), PlexStartDenied> {
        if pins.len() >= MAX_PENDING_PINS {
            pins.retain(|_, pin| now.saturating_duration_since(pin.started) < PIN_TTL);
        }
        if pins.len() >= MAX_PENDING_PINS {
            tracing::warn!("too many Plex sign-ins are waiting; refusing new starts");
            return Err(PlexStartDenied::Busy);
        }
        Ok(())
    }

    /// Check there is room before minting a PIN at plex.tv.
    fn has_room(&self, now: Instant) -> Result<(), PlexStartDenied> {
        Self::make_room(&mut *self.lock()?, now)
    }

    fn insert(&self, pin_id: i64, record: PendingPin, now: Instant) -> Result<(), PlexStartDenied> {
        let mut pins = self.lock()?;
        if !pins.contains_key(&pin_id) {
            Self::make_room(&mut pins, now)?;
        }
        pins.insert(pin_id, record);
        Ok(())
    }

    /// Check that `claim` matches the record for its PIN and `purpose`.
    /// Missing, expired, or mismatched records fail closed.
    fn check(&self, claim: &PinClaim<'_>, purpose: PlexPurpose) -> Result<(), FederatedError> {
        let pins = self
            .0
            .lock()
            .map_err(|_| FederatedError::StoreUnavailable("plex pin lock".to_owned()))?;
        let pin = pins.get(&claim.pin_id).ok_or_else(unknown_pin)?;
        let secret_ok = constant_time_eq(&hash_token(claim.pin_secret), &pin.secret_hash);
        if !secret_ok
            || pin.purpose != purpose
            || pin.user_id.as_deref() != claim.user_id
            || pin.started.elapsed() >= PIN_TTL
        {
            return Err(unknown_pin());
        }
        Ok(())
    }

    /// Consume the record once the PIN is approved. Only one poller wins.
    fn consume(&self, pin_id: i64) -> Result<(), FederatedError> {
        let mut pins = self
            .0
            .lock()
            .map_err(|_| FederatedError::StoreUnavailable("plex pin lock".to_owned()))?;
        pins.remove(&pin_id).map(|_| ()).ok_or_else(unknown_pin)
    }
}

/// The unified journey. Generic over stores so tests inject fakes.
#[derive(Debug, Clone)]
pub struct PlexJourney<S, C, L, N> {
    users: S,
    client: C,
    links: L,
    sessions: N,
    pins: PendingPins,
}

impl<S, C, L, N> PlexJourney<S, C, L, N> {
    /// Wire the journey from its ports.
    pub fn new(users: S, client: C, links: L, sessions: N) -> Self {
        Self {
            users,
            client,
            links,
            sessions,
            pins: PendingPins::default(),
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
    /// Start a flow: check who may start it, mint a PIN, and bind it to
    /// the caller with a fresh secret. `caller` is the signed-in user for
    /// link and connect (the routes require a session for those).
    pub async fn start(
        &self,
        purpose: PlexPurpose,
        caller: Option<&str>,
    ) -> Result<PlexStart, PlexStartDenied> {
        match purpose {
            PlexPurpose::Login => {
                if !self.client.login_enabled() {
                    return Err(PlexStartDenied::StartFailed(login_disabled()));
                }
            }
            PlexPurpose::Link => match self.client.server_machine_id().await {
                Ok(Some(_)) => {}
                Ok(None) => return Err(PlexStartDenied::NotConfigured),
                Err(error) => return Err(PlexStartDenied::StartFailed(error)),
            },
            PlexPurpose::Connect => {
                let user = match caller {
                    Some(user_id) => self
                        .users
                        .get_user_by_id(user_id)
                        .await
                        .map_err(PlexStartDenied::StartFailed)?,
                    None => None,
                };
                if user.is_none_or(|user| user.role != ROLE_ADMIN) {
                    return Err(PlexStartDenied::Forbidden);
                }
            }
        }
        self.pins.has_room(Instant::now())?;
        let pin = self.client.create_pin().await.map_err(|error| {
            tracing::warn!(%error, "could not create a Plex PIN");
            PlexStartDenied::StartFailed(FederatedError::ProviderUnavailable(
                "Could not start Plex authentication".to_owned(),
            ))
        })?;
        let pin_secret = mint_token()
            .map_err(|_| PlexStartDenied::StartFailed(FederatedError::RngUnavailable))?;
        self.pins.insert(
            pin.id,
            PendingPin {
                purpose,
                secret_hash: hash_token(&pin_secret),
                user_id: match purpose {
                    PlexPurpose::Login => None,
                    PlexPurpose::Link | PlexPurpose::Connect => caller.map(str::to_owned),
                },
                started: Instant::now(),
            },
            Instant::now(),
        )?;
        Ok(PlexStart {
            pin_id: pin.id,
            authorize_url: plex_auth_url(&self.client.client_id(), &pin.code),
            pin_secret,
        })
    }

    /// Check the claim, poll plex.tv, and consume the record on approval.
    async fn approved_token(
        &self,
        claim: &PinClaim<'_>,
        purpose: PlexPurpose,
    ) -> Result<Option<String>, FederatedError> {
        self.pins.check(claim, purpose)?;
        let Some(auth_token) = self.client.poll_pin(claim.pin_id).await? else {
            return Ok(None);
        };
        self.pins.consume(claim.pin_id)?;
        Ok(Some(auth_token))
    }

    /// Login flow: poll, gate membership when a server is configured,
    /// import the user, auto-link, and mint a session.
    pub async fn poll_login(
        &self,
        claim: &PinClaim<'_>,
        user_agent: Option<&str>,
    ) -> Result<PlexPoll<(StoredUser, String)>, FederatedError> {
        if !self.client.login_enabled() {
            return Err(login_disabled());
        }
        let Some(auth_token) = self.approved_token(claim, PlexPurpose::Login).await? else {
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

    /// Link flow: poll, verify the profile and store it as the caller's
    /// Plex media link. The machine id is required and the membership gate
    /// is mandatory; no session is minted.
    pub async fn poll_link(
        &self,
        claim: &PinClaim<'_>,
    ) -> Result<PlexPoll<PlexProfile>, FederatedError> {
        let user_id = claim.user_id.ok_or_else(unknown_pin)?;
        let Some(auth_token) = self.approved_token(claim, PlexPurpose::Link).await? else {
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
    pub async fn poll_connect(
        &self,
        claim: &PinClaim<'_>,
    ) -> Result<PlexPoll<String>, FederatedError> {
        Ok(
            match self.approved_token(claim, PlexPurpose::Connect).await? {
                Some(token) => PlexPoll::Complete(token),
                None => PlexPoll::Pending,
            },
        )
    }

    /// Fetch the profile and enforce the membership gate. With no server
    /// URL configured the gate is skipped, except for the link flow
    /// (`require_machine`), which always needs one. A configured server
    /// that cannot be read fails the sign-in.
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
        let machine_id = self.client.server_machine_id().await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(started: Instant) -> PendingPin {
        PendingPin {
            purpose: PlexPurpose::Login,
            secret_hash: String::new(),
            user_id: None,
            started,
        }
    }

    #[test]
    fn pending_pins_are_capped_and_expired_ones_make_room() {
        let pins = PendingPins::default();
        let start = Instant::now();
        for id in 0..MAX_PENDING_PINS as i64 {
            pins.insert(id, pending(start), start).unwrap();
        }
        assert_eq!(
            pins.insert(-1, pending(start), start),
            Err(PlexStartDenied::Busy)
        );
        assert_eq!(pins.has_room(start), Err(PlexStartDenied::Busy));
        // Once the waiting PINs expire, a new start prunes them.
        let later = start + PIN_TTL + Duration::from_secs(1);
        pins.insert(-1, pending(later), later).unwrap();
        assert_eq!(pins.lock().unwrap().len(), 1);
    }
}
