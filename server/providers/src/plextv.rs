//! plex.tv, the Plex account service: the PIN sign-in, the account behind
//! a token, its Home users and friends, and the `/resources` list that
//! names each server's token for the account.
//!
//! Every call goes through the executor at the `plextv` rate row, so the
//! Plex sign-in, the admin's user import and the media server adapter's
//! token lookup share one pace. Each call sends Plex's product headers and
//! the caller's client id; account calls add the account token. A 404 and
//! every other failure come back as the executor's typed error for the
//! caller to judge. The wire shapes are in `plextv/models.rs`.

mod models;

use std::time::Duration;

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::ProviderError;
use crate::executor::{
    Backoff, Executor, Reply, Request, Spec, USER_COOLDOWN_WAIT, Url, standard_classify,
};

pub use models::{Account, Pin, Resource};
use models::{AccountList, Lenient, PinPoll};

/// plex.tv account API root.
pub const PLEX_TV_BASE: &str = "https://plex.tv/api/v2";
/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "plextv";
/// The product name every plex.tv call presents.
pub const PRODUCT: &str = "DroppedNeedle";
/// Largest answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// plex.tv on the wire: the standard rules, with the 15 second deadline
/// v2 gave its account calls. plex.tv documents no rate-limit answer
/// beyond a 429; one that names no wait rests ten seconds.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(10)),
    retry_after_cap: Duration::from_secs(600),
    classify: standard_classify,
};

/// The plex.tv client. Cheap to clone; clones share the row.
#[derive(Debug, Clone)]
pub struct PlexTvClient {
    executor: Executor,
    base: Option<Url>,
}

impl PlexTvClient {
    /// Calls through `executor`, on the `plextv` row, to the API root
    /// `base`.
    #[must_use]
    pub fn new(executor: Executor, base: &str) -> Self {
        Self {
            executor,
            base: parse_base(base, SOURCE),
        }
    }

    /// Whether the row can cover `calls` requests a person is waiting on.
    /// Each request may wait [USER_COOLDOWN_WAIT] for its turn, so the row
    /// covers them when the last one is due within that. If it cannot, the
    /// answer is a rate limit naming the wait for all of them, and nothing
    /// is spent.
    ///
    /// # Errors
    ///
    /// [`ProviderError::RateLimited`] with that wait.
    pub fn room_for(&self, calls: u32) -> Result<(), ProviderError> {
        let wait = self.executor.limiter().retry_after(calls);
        if wait <= USER_COOLDOWN_WAIT {
            return Ok(());
        }
        Err(ProviderError::RateLimited {
            provider: SOURCE,
            retry_after: Some(wait),
        })
    }

    /// Mint a strong PIN for the sign-in.
    ///
    /// # Errors
    ///
    /// The executor's typed failure, or a payload error for an answer
    /// without an id or code.
    pub async fn create_pin(
        &self,
        client_id: &str,
        priority: RequestPriority,
    ) -> Result<Pin, ProviderError> {
        let request = Request::post_form(
            self.base()?,
            &["pins"],
            vec![("strong".to_owned(), "true".to_owned())],
        )
        .map_err(|_| unusable_base(SOURCE))?;
        let reply = self
            .executor
            .send(headers(request, client_id, None), priority)
            .await?;
        decode(&reply)
    }

    /// The account token a PIN carries: `None` while the person has not
    /// approved it.
    ///
    /// # Errors
    ///
    /// The executor's typed failure (an unknown PIN is a 404), or a
    /// payload error for an answer that does not decode.
    pub async fn pin_token(
        &self,
        pin_id: i64,
        client_id: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let poll: PinPoll = decode(
            &self
                .get(
                    &["pins", &pin_id.to_string()],
                    &[],
                    client_id,
                    None,
                    priority,
                )
                .await?,
        )?;
        Ok(poll.auth_token.filter(|token| !token.is_empty()))
    }

    /// The account behind `token`.
    ///
    /// # Errors
    ///
    /// The executor's typed failure (a refused token is a 401), or a
    /// payload error for an answer without a uuid.
    pub async fn account(
        &self,
        token: &str,
        client_id: &str,
        priority: RequestPriority,
    ) -> Result<Account, ProviderError> {
        decode(
            &self
                .get(&["user"], &[], client_id, Some(token), priority)
                .await?,
        )
    }

    /// The devices `token`'s account can reach, servers with the
    /// account's token for each. Entries that do not read are skipped.
    ///
    /// # Errors
    ///
    /// The executor's typed failure, or a payload error for an answer
    /// that is not a list.
    pub async fn resources(
        &self,
        token: &str,
        client_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<Resource>, ProviderError> {
        let devices: Vec<Lenient<Resource>> = decode(
            &self
                .get(
                    &["resources"],
                    &[("includeHttps", "1"), ("includeRelay", "1")],
                    client_id,
                    Some(token),
                    priority,
                )
                .await?,
        )?;
        Ok(devices.into_iter().filter_map(Lenient::into_read).collect())
    }

    /// The Plex Home users of `token`'s account.
    ///
    /// # Errors
    ///
    /// As [PlexTvClient::resources].
    pub async fn home_users(
        &self,
        token: &str,
        client_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<Account>, ProviderError> {
        self.accounts(&["home", "users"], token, client_id, priority)
            .await
    }

    /// The friends of `token`'s account.
    ///
    /// # Errors
    ///
    /// As [PlexTvClient::resources].
    pub async fn friends(
        &self,
        token: &str,
        client_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<Account>, ProviderError> {
        self.accounts(&["friends"], token, client_id, priority)
            .await
    }

    async fn accounts(
        &self,
        path: &[&str],
        token: &str,
        client_id: &str,
        priority: RequestPriority,
    ) -> Result<Vec<Account>, ProviderError> {
        let list: AccountList = decode(
            &self
                .get(path, &[], client_id, Some(token), priority)
                .await?,
        )?;
        Ok(list.into_accounts())
    }

    fn base(&self) -> Result<&Url, ProviderError> {
        self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))
    }

    async fn get(
        &self,
        path: &[&str],
        query: &[(&str, &str)],
        client_id: &str,
        token: Option<&str>,
        priority: RequestPriority,
    ) -> Result<Reply, ProviderError> {
        let request = Request::get(self.base()?, path, query).map_err(|_| unusable_base(SOURCE))?;
        self.executor
            .send(headers(request, client_id, token), priority)
            .await
    }
}

/// The account's token for the server `machine_id` among `devices`, or
/// `None` when the account has no server with that id or no token for it.
#[must_use]
pub fn server_access_token(devices: &[Resource], machine_id: &str) -> Option<String> {
    devices
        .iter()
        .find(|device| {
            device.is_server() && device.client_identifier.as_deref() == Some(machine_id)
        })
        .and_then(|device| device.access_token.clone())
        .filter(|token| !token.is_empty())
}

/// Plex's product headers, the client id when known, and the account
/// token when given.
fn headers(request: Request, client_id: &str, token: Option<&str>) -> Request {
    let mut request = request
        .header("X-Plex-Product", PRODUCT)
        .header("X-Plex-Version", "1.0")
        .header("accept", "application/json");
    if !client_id.is_empty() {
        request = request.header("X-Plex-Client-Identifier", client_id);
    }
    if let Some(token) = token {
        request = request.header("X-Plex-Token", token);
    }
    request
}
