//! The plex.tv side of the PIN sign-in and the admin user import.
//!
//! Every call goes through the one plex.tv client of the providers crate,
//! at user priority: a person is waiting on each of them. Account calls
//! carry the user's own token; the machine id comes from the admin's
//! configured server, read through [`PlexServerIdentity`]. Settings are
//! read per call. Failures surface as
//! [`FederatedError::ProviderUnavailable`] or, for a refused token,
//! [`FederatedError::Authentication`]; the journey maps them to the v2
//! user-facing messages at each step. Tokens never reach a log line.

use std::collections::HashSet;
use std::sync::Arc;

use super::FederatedError;
use super::plex::{PlexAccount, PlexPin, PlexServerIdentity};
use super::settings::{InstallIds, SignInSettings};
use super::users::PROVIDER_PLEX;
use crate::users::stores::{BoxFuture, DirectoryError, DirectoryUser, UserDirectory};
use droppedneedle_kernel::runtime_config::secret_sections::PlexConnection;
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};
use droppedneedle_providers::plextv::{self, PlexTvClient};
use droppedneedle_providers::{ProviderError, RequestPriority};

/// Sign-ins and the import are a person waiting on the answer.
const PRIORITY: RequestPriority = RequestPriority::UserInitiated;

/// plex.tv calls an approved login or link makes from its last poll on:
/// the poll, the account and one read of `/resources`.
const SIGN_IN_CALLS: u32 = 3;

/// plex.tv calls the user import makes: Home users and friends.
const IMPORT_CALLS: u32 = 2;

/// The plex.tv client for the PIN sign-in, and the Plex [`UserDirectory`].
#[derive(Clone)]
pub struct PlexTv {
    plextv: PlexTvClient,
    sign_in: SignInSettings,
    connection: Live<PlexConnection>,
    ids: InstallIds,
    server: Arc<dyn PlexServerIdentity>,
}

impl PlexTv {
    /// Calls plex.tv through `plextv`, reading the configured server's
    /// machine id through `server`.
    pub fn new(
        plextv: PlexTvClient,
        store: Arc<ConfigStore>,
        server: Arc<dyn PlexServerIdentity>,
    ) -> Self {
        Self {
            sign_in: SignInSettings::new(&store),
            connection: store.live(),
            ids: InstallIds::new(store),
            plextv,
            server,
        }
    }
}

/// A plex.tv read during a sign-in that did not succeed.
#[derive(Debug)]
pub enum PlexTvError {
    /// Worth asking again on the next poll: turned down for pace, rate
    /// limited, unreachable or failing.
    Retry,
    /// A settled answer: a refused token, an unreadable answer or an
    /// unusable address.
    Failed(FederatedError),
}

impl PlexTvError {
    fn of(error: ProviderError) -> Self {
        if error.is_retriable() {
            tracing::debug!(%error, "plex.tv read did not settle; the sign-in asks again");
            Self::Retry
        } else {
            Self::Failed(failed(error))
        }
    }
}

/// What `/resources` says about the account and one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerAccess {
    /// Whether the account can reach the server.
    pub member: bool,
    /// The account's token for the server, when plex.tv gives one.
    pub token: Option<String>,
}

/// What a plex.tv failure means for a sign-in. The cause goes to the log
/// only; it never carries a token or an address.
fn failed(error: ProviderError) -> FederatedError {
    match error {
        ProviderError::Unauthorized { .. } | ProviderError::Forbidden { .. } => {
            FederatedError::Authentication("Plex authentication failed".to_owned())
        }
        ProviderError::Transport { .. } | ProviderError::Invalid { .. } => {
            tracing::debug!(%error, "plex.tv request failed");
            FederatedError::ProviderUnavailable("Could not reach plex.tv".to_owned())
        }
        other => {
            tracing::warn!(error = %other, "plex.tv request failed");
            FederatedError::ProviderUnavailable("Plex request failed".to_owned())
        }
    }
}

impl PlexTv {
    /// Stable install id (`plex_client_id` setting).
    pub fn client_id(&self) -> String {
        self.ids.plex_client_id()
    }

    /// The admin's Plex login switch, read live.
    pub fn login_enabled(&self) -> bool {
        self.sign_in.plex_login_enabled()
    }

    /// Mint a PIN.
    pub async fn create_pin(&self) -> Result<PlexPin, FederatedError> {
        let pin = self
            .plextv
            .create_pin(&self.client_id(), PRIORITY)
            .await
            .map_err(failed)?;
        Ok(PlexPin {
            id: pin.id,
            code: pin.code,
        })
    }

    /// Poll a PIN: `Ok(None)` while pending, `Ok(Some(token))` once the
    /// user authorizes. While the plex.tv row cannot cover the calls an
    /// approval leads to, the poll stays pending without asking, and the
    /// browser's next poll asks again.
    pub async fn poll_pin(&self, pin_id: i64) -> Result<Option<String>, FederatedError> {
        if let Err(error) = self.plextv.room_for(SIGN_IN_CALLS) {
            tracing::debug!(%error, "plex.tv is paced; the PIN poll waits");
            return Ok(None);
        }
        match self
            .plextv
            .pin_token(pin_id, &self.client_id(), PRIORITY)
            .await
        {
            Ok(token) => Ok(token),
            Err(error @ (ProviderError::Transport { .. } | ProviderError::Invalid { .. })) => {
                Err(failed(error))
            }
            // v2 parity: any answer that is not an approval reads as "not
            // authorized yet".
            Err(error) => {
                tracing::debug!(%error, "plex.tv PIN not approved yet");
                Ok(None)
            }
        }
    }

    /// Fetch the account profile behind an auth token.
    pub async fn account_profile(&self, auth_token: &str) -> Result<PlexAccount, PlexTvError> {
        let account = self
            .plextv
            .account(auth_token, &self.client_id(), PRIORITY)
            .await
            .map_err(PlexTvError::of)?;
        let uuid = account.uuid.trim().to_owned();
        if uuid.is_empty() {
            return Err(PlexTvError::Failed(FederatedError::ProviderUnavailable(
                "Plex /user response missing uuid".to_owned(),
            )));
        }
        Ok(PlexAccount {
            display_name: account.profile_name(),
            email: account.email().unwrap_or_default(),
            thumb: account.thumb(),
            uuid,
        })
    }

    /// The configured server's machine id. `Ok(None)` only when no Plex
    /// server URL is set; a configured server that cannot be read is an
    /// error, so the membership gate never fails open.
    pub async fn server_machine_id(&self) -> Result<Option<String>, FederatedError> {
        self.server.machine_id().await
    }

    /// Whether the account can reach the server `machine_id`, and its token
    /// there, from one read of `/resources`.
    pub async fn server_access(
        &self,
        auth_token: &str,
        machine_id: &str,
    ) -> Result<ServerAccess, PlexTvError> {
        let devices = self
            .plextv
            .resources(auth_token, &self.client_id(), PRIORITY)
            .await
            .map_err(PlexTvError::of)?;
        Ok(ServerAccess {
            member: devices.iter().any(|device| {
                device.is_server() && device.client_identifier.as_deref() == Some(machine_id)
            }),
            token: plextv::server_access_token(&devices, machine_id),
        })
    }
}

impl UserDirectory for PlexTv {
    fn provider(&self) -> &'static str {
        PROVIDER_PLEX
    }

    /// Plex Home users plus friends, merged by uuid, read with the admin's
    /// token. One list failing still returns the other (v2 parity); both
    /// failing is an outage. When the plex.tv row cannot cover both reads
    /// at the start, neither is sent; a list turned down later still
    /// leaves only the other.
    fn list_users(&self) -> BoxFuture<'_, Result<Vec<DirectoryUser>, DirectoryError>> {
        Box::pin(async move {
            let section = self.connection.get().map_err(|error| {
                tracing::warn!(%error, "cannot read plex settings for user import");
                DirectoryError::NotConfigured
            })?;
            let token = section.plex_token.expose().to_owned();
            if token.is_empty() {
                return Err(DirectoryError::NotConfigured);
            }
            if let Err(error) = self.plextv.room_for(IMPORT_CALLS) {
                tracing::warn!(%error, "plex.tv is paced; the user import waits");
                return Err(DirectoryError::Transport);
            }
            let client_id = self.client_id();
            let home = self.plextv.home_users(&token, &client_id, PRIORITY).await;
            let friends = self.plextv.friends(&token, &client_id, PRIORITY).await;
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
