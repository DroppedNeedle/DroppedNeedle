//! App connect: a native app signs in through the browser and gets an
//! ordinary app password for the signed-in user.
//!
//! The flow is the OAuth 2.0 authorization code grant with PKCE for native
//! apps (RFC 6749, RFC 7636, RFC 8252), so standard app libraries such as
//! AppAuth work with it:
//!
//! 1. The app opens `<base>/connect?response_type=code&client_id=…
//!    &redirect_uri=…&code_challenge=…&code_challenge_method=S256&state=…
//!    &name=<device>` in a browser tab (the authorization endpoint). The page
//!    signs the user in first if needed, then asks
//!    `GET /api/v3/auth/app-connect/request` what is being asked and shows
//!    it: the app's name from the settings (never from the request), the
//!    device name the app reported, and who is signed in.
//! 2. On "Allow" the page calls `POST /api/v3/auth/app-connect/authorize`
//!    with a standard session and sends the browser to the redirect it
//!    answers, `<redirect_uri>?code=…&state=…`. "Cancel" sends it to the
//!    request's `error=access_denied` address.
//! 3. The app trades the code and its verifier at
//!    `POST /api/v3/auth/app-connect/token` (the token endpoint: public, a
//!    form body, the RFC's answers) for an app password, named
//!    `<app name> · <device>`. One with the same name of the same user is
//!    revoked in the same transaction, so signing in again replaces the old
//!    password instead of piling up new ones.
//!
//! Only the apps in the `app_connect` settings section may ask, each at
//! exactly its listed return addresses; with none listed the page's routes
//! answer 404 and every trade `invalid_client`. The user confirms every
//! time: another app on the device can claim the same private-use scheme
//! and start a flow of its own, and the confirmation (with its warning) is
//! what stops that. PKCE stops a code intercepted from a flow the real app
//! started.
//!
//! Codes are 32 random bytes, held as their SHA-256 for
//! [`CODE_TTL_SECS`] in memory only: the server runs as one process, and a
//! restart costs at most a sign-in started in the last two minutes. A code
//! is gone after its first trade, also a failed one. Codes, verifiers and
//! secrets are never logged.

mod codes;
pub mod request;
pub mod routes;
pub mod token;

use std::sync::Arc;

use droppedneedle_kernel::runtime_config::sections::AppConnect;
use droppedneedle_kernel::runtime_config::{Live, Plain};

use crate::users::services::{self, MAX_ACTIVE_APP_PASSWORDS};
use crate::users::{Principal, UsersDeps, UsersError};
use codes::{Grant, PendingCodes};
use request::{ConnectRequest, Rejection, check};
use routes::{AppConnectRedirect, AppConnectRequestView};
use token::{
    AppConnectTokenErrorCode, AppConnectTokenRequest, AppConnectTokenResponse, AppConnectTokenType,
};

pub use codes::CODE_TTL_SECS;
pub(crate) use codes::Redeemed;

/// Why the page's request or its Allow cannot go on.
#[derive(Debug)]
pub enum AppConnectError {
    /// No app is listed: app connect is off.
    Off,
    /// The app or its return address is not listed.
    UnknownClient,
    /// The app is known but the request is malformed; `redirect` hands the
    /// error back to the app.
    Invalid {
        /// What is wrong, for the page.
        reason: &'static str,
        /// The error address for the app.
        redirect: String,
    },
    /// The user holds the most app passwords allowed under other names.
    AtLimit,
    /// The section does not read; nothing is allowed until it is saved.
    SettingsUnreadable,
    /// The caller, the account store or the code source failed.
    Users(UsersError),
}

impl From<UsersError> for AppConnectError {
    fn from(error: UsersError) -> Self {
        Self::Users(error)
    }
}

impl From<Rejection> for AppConnectError {
    fn from(rejection: Rejection) -> Self {
        match rejection {
            Rejection::Disabled => Self::Off,
            Rejection::UnknownClient => Self::UnknownClient,
            Rejection::Invalid { reason, redirect } => Self::Invalid { reason, redirect },
        }
    }
}

/// Why a trade did not mint.
#[derive(Debug)]
pub enum TokenStepError {
    /// Refused, with the RFC's code for the app.
    Refused(AppConnectTokenErrorCode),
    /// The section does not read; no code is taken until it is saved.
    SettingsUnreadable,
    /// The account store failed.
    Users(UsersError),
}

impl From<UsersError> for TokenStepError {
    fn from(error: UsersError) -> Self {
        Self::Users(error)
    }
}

/// App connect, built once at boot.
#[derive(Clone)]
pub struct AppConnectService {
    users: UsersDeps,
    codes: Arc<PendingCodes>,
    settings: Live<AppConnect>,
}

impl AppConnectService {
    /// The service over the account stores and the live section.
    pub fn new(users: UsersDeps, settings: Live<AppConnect>) -> Self {
        Self {
            users,
            codes: Arc::default(),
            settings,
        }
    }

    fn apps(&self) -> Option<Arc<Plain<AppConnect>>> {
        self.settings.read()
    }

    /// Live app passwords of `user_id` under names other than `name`.
    async fn others(&self, user_id: &str, name: &str) -> Result<u64, UsersError> {
        let own = services::list_app_passwords(&self.users, user_id)
            .await?
            .app_passwords;
        let others = own.iter().filter(|row| row.name != name).count();
        Ok(u64::try_from(others).unwrap_or(u64::MAX))
    }

    /// What `request` asks of `caller`, for the confirmation page.
    pub async fn describe(
        &self,
        caller: &Principal,
        request: &ConnectRequest,
    ) -> Result<AppConnectRequestView, AppConnectError> {
        let apps = self.apps().ok_or(AppConnectError::SettingsUnreadable)?;
        let checked = check(&apps, request)?;
        let own = services::list_app_passwords(&self.users, caller.user_id())
            .await?
            .app_passwords;
        let replaces_existing = own.iter().any(|row| row.name == checked.password_name);
        let others = own
            .iter()
            .filter(|row| row.name != checked.password_name)
            .count();
        Ok(AppConnectRequestView {
            client_id: checked.client.client_id.clone(),
            client_name: checked.client.name.clone(),
            device_name: checked.device_name.clone(),
            password_name: checked.password_name.clone(),
            replaces_existing,
            at_limit: u64::try_from(others).unwrap_or(u64::MAX) >= MAX_ACTIVE_APP_PASSWORDS,
            cancel_redirect: checked.cancel_redirect(),
        })
    }

    /// Allow `request` for `caller`: issue its one-time code and answer the
    /// address that hands it to the app. Needs a standard session, as
    /// creating an app password by hand does.
    pub async fn allow(
        &self,
        caller: &Principal,
        request: &ConnectRequest,
    ) -> Result<AppConnectRedirect, AppConnectError> {
        caller.require_standard_session()?;
        let apps = self.apps().ok_or(AppConnectError::SettingsUnreadable)?;
        let checked = check(&apps, request)?;
        if self
            .others(caller.user_id(), &checked.password_name)
            .await?
            >= MAX_ACTIVE_APP_PASSWORDS
        {
            return Err(AppConnectError::AtLimit);
        }
        let code = self
            .codes
            .issue(
                Grant {
                    user_id: caller.user_id().to_owned(),
                    client_id: checked.client.client_id.clone(),
                    redirect_uri: checked.redirect_uri.clone(),
                    code_challenge: checked.code_challenge.clone(),
                    password_name: checked.password_name.clone(),
                },
                self.users.clock.now_unix(),
            )
            .map_err(|cause| UsersError::internal(&cause))?;
        tracing::info!(
            user_id = caller.user_id(),
            client_id = %checked.client.client_id,
            "app connect allowed"
        );
        Ok(AppConnectRedirect {
            redirect: checked.code_redirect(&code),
        })
    }

    /// Trade a code and its verifier for an app password of the user who
    /// allowed it. A code the app may hold is taken only once the section
    /// reads and the app is listed; after that it is gone, whatever the
    /// answer.
    pub async fn exchange(
        &self,
        trade: AppConnectTokenRequest,
    ) -> Result<AppConnectTokenResponse, TokenStepError> {
        use AppConnectTokenErrorCode::{InvalidClient, InvalidGrant};

        let apps = self.apps().ok_or(TokenStepError::SettingsUnreadable)?;
        // Answering `invalid_client` before `invalid_grant` tells a caller
        // which client ids are listed; they ship inside the apps anyway.
        if !apps.has_client(&trade.client_id) {
            return Err(TokenStepError::Refused(InvalidClient));
        }
        let Some(redeemed) = self.codes.redeem(
            &trade.code,
            &trade.client_id,
            &trade.redirect_uri,
            &trade.code_verifier,
            self.users.clock.now_unix(),
        ) else {
            tracing::debug!("app connect trade refused: unknown, used, expired or mismatched code");
            return Err(TokenStepError::Refused(InvalidGrant));
        };
        // The admin may have taken the address off the list since.
        if apps.client(&trade.client_id, &trade.redirect_uri).is_none() {
            tracing::debug!("app connect trade refused: the address is no longer listed");
            return Err(TokenStepError::Refused(InvalidGrant));
        }
        let user = self
            .users
            .users
            .get_by_id(redeemed.user_id())
            .await
            .map_err(services::store_internal)?;
        let Some(user) = user else {
            tracing::debug!("app connect trade refused: the user is gone");
            return Err(TokenStepError::Refused(InvalidGrant));
        };
        let Some(minted) = services::replace_app_password(&self.users, &redeemed).await? else {
            tracing::debug!("app connect trade refused: app-password limit reached");
            return Err(TokenStepError::Refused(InvalidGrant));
        };
        Ok(AppConnectTokenResponse {
            access_token: minted.secret,
            token_type: AppConnectTokenType::Bearer,
            username: user.username_display.or(user.username).unwrap_or(user.id),
        })
    }
}
