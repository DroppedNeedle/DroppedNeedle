//! App connect: a native app signs in through the browser and gets an
//! ordinary app password for the signed-in user.
//!
//! The flow is the OAuth 2.0 authorization code grant with PKCE for native
//! apps (RFC 6749, RFC 7636, RFC 8252), trimmed to what a player needs:
//!
//! 1. The app opens `<base>/connect?client_id=…&redirect_uri=…&code_challenge=…
//!    &code_challenge_method=S256&state=…&name=<device>` in a browser tab.
//!    The page signs the user in first if needed, then asks
//!    `GET /api/v3/auth/app-connect/request` what is being asked and shows
//!    it: the app's name from the settings (never from the request), the
//!    device name the app reported, and who is signed in.
//! 2. On "Allow" the page calls `POST /api/v3/auth/app-connect/authorize`
//!    with a standard session and sends the browser to the redirect it
//!    answers, `<redirect_uri>?code=…&state=…`. "Cancel" sends it to the
//!    request's `error=access_denied` address.
//! 3. The app trades the code and its verifier at
//!    `POST /api/v3/auth/app-connect/token` (public, no cookie) for an app
//!    password, named `<app name> · <device>`. One with the same name of the
//!    same user is revoked in the same transaction, so signing in again
//!    replaces the old password instead of piling up new ones.
//!
//! Only the apps in the `app_connect` settings section may ask, each at
//! exactly its listed return addresses; with none listed every route
//! answers 404. The user confirms every time: another app on the device can
//! claim the same private-use scheme and start a flow of its own, and the
//! confirmation (with its warning) is what stops that. PKCE stops a code
//! intercepted from a flow the real app started.
//!
//! Codes are 32 random bytes, held as their SHA-256 for
//! [`codes::CODE_TTL_SECS`] in memory only: the server runs as one process,
//! and a restart costs at most a sign-in started in the last two minutes.
//! A code is gone after its first trade, also a failed one. Codes,
//! verifiers and secrets are never logged.

pub mod codes;
pub mod request;
pub mod routes;

use std::sync::Arc;

use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::AppConnect;

use crate::users::UsersDeps;

/// What the app connect routes share.
#[derive(Clone)]
pub struct AppConnectState {
    /// Account rows and the app-password service the token step mints
    /// through.
    pub users: UsersDeps,
    /// Codes waiting for their trade.
    pub codes: Arc<codes::PendingCodes>,
    /// The apps that may ask, read live.
    pub settings: Live<AppConnect>,
}

impl AppConnectState {
    /// The state over the users routes' dependencies and the live section.
    pub fn new(users: UsersDeps, settings: Live<AppConnect>) -> Self {
        Self {
            users,
            codes: Arc::default(),
            settings,
        }
    }

    /// The section as stored; unreadable reads as no app listed (off).
    fn apps(&self) -> AppConnect {
        match self.settings.get() {
            Ok(section) => AppConnect::clone(&section),
            Err(error) => {
                tracing::warn!(%error, "cannot read app connect settings; app connect is off");
                AppConnect::default()
            }
        }
    }
}
