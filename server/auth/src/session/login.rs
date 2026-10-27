//! Local login: one uniform failure, two transports, no token in cookie bodies.
//!
//! Rules pinned here:
//!
//! - Unknown user and wrong password are indistinguishable: both run exactly
//!   one hash verify (real or dummy) and return status 401 with the same
//!   `UNAUTHORIZED` envelope and `Invalid username or password` message.
//! - `transport` selects the credential handoff. Cookie mode (default) puts
//!   the raw token only inside the `Set-Cookie` value and the body carries no
//!   token field (v2 returned it alongside the cookie; that leak is closed).
//!   Bearer mode returns the raw token once in the body and sets no cookie.
//!   The handoff itself renders in the route layer
//!   ([`crate::routes::login_handoff`]).
//! - Password hashing runs on the blocking pool, never on an async worker.
//! - The session row, a rehash of a stored hash weaker than the pin and
//!   `last_login_at` are written in one transaction, and only while the
//!   stored credential is still the one the password was checked against
//!   ([`Sessions::insert_login`]). If it changed in between, the
//!   login checks the password once more against the current row; a second
//!   change in that window answers busy. A session is never stored for a
//!   credential the login did not verify.
//! - A store outage is [`LoginError::Unavailable`], never a failed login. A
//!   store too busy to answer in time is [`LoginError::Busy`], never either.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    extract::Transport,
    store::{NewSession, SessionStoreError},
    tokens,
};
use crate::passwords::{Passwords, StoredPassword, Verdict};
use crate::sqlite::{LoginWrite, Sessions, Users};

/// Login transport: cookie (browser default) or Bearer (devices and scripts).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportParam {
    /// httpOnly cookie session; body carries no token.
    #[default]
    Cookie,
    /// Raw token returned once in the body; no cookie set.
    Bearer,
}

/// Login request fields. The full clean-slate shape is API-design scope; the
/// `transport` param is mechanism and lives here.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginRequest {
    /// Username (matched case-insensitively, as in v2).
    pub username: String,
    /// Account password.
    pub password: String,
    /// Credential handoff; defaults to cookie.
    #[serde(default)]
    pub transport: TransportParam,
}

/// Per-request login inputs owned by the transport layer.
#[derive(Debug, Clone)]
pub struct LoginContext {
    /// Client label recorded on the session.
    pub user_agent: Option<String>,
    /// Now, unix seconds.
    pub now_unix: i64,
}

/// Successful login: who, and the token with the transport it travels on.
pub struct LoginSuccess {
    /// Authenticated user id.
    pub user_id: String,
    /// Display name for the response body.
    pub display_name: String,
    /// Transport that carries the credential out.
    pub transport: Transport,
    /// Raw token; cookie mode puts it only in `Set-Cookie`.
    pub raw_token: String,
}

/// Login failure. Only `InvalidCredentials` is the caller's fault.
#[derive(Debug, Error)]
pub enum LoginError {
    /// Unknown user or wrong password: one uniform message, no oracle.
    #[error("Invalid username or password")]
    InvalidCredentials,
    /// Storage or entropy failure: 500 without detail.
    #[error("login unavailable")]
    Unavailable,
    /// The database was too busy to answer in time: 503 with `Retry-After`.
    #[error("login store busy")]
    Busy,
}

/// Stored local credential returned by [`Users::login_credential`].
#[derive(Debug, Clone)]
pub struct LocalCredential {
    /// User id.
    pub user_id: String,
    /// Display name.
    pub display_name: String,
    /// The stored scheme tag and hash, as read.
    pub password: StoredPassword,
}

/// `Cache-Control` value on every login/token-mint response.
pub const NO_STORE: &str = "no-store";

/// Uniform login-failure message. One string for both failure causes.
pub const INVALID_CREDENTIALS: &str = "Invalid username or password";

/// Local-login service: lookup, verify-or-dummy, mint, hand off.
#[derive(Debug, Clone)]
pub struct LoginService {
    sessions: Sessions,
    passwords: Passwords,
    users: Users,
}

/// Login attempts before a credential that keeps changing answers busy:
/// the first, and one re-check after a change.
const LOGIN_ATTEMPTS: usize = 2;

impl LoginService {
    /// Wire the service from its parts.
    pub fn new(sessions: Sessions, passwords: Passwords, users: Users) -> Self {
        Self {
            sessions,
            passwords,
            users,
        }
    }

    /// Attempt a local login. Unknown users cost one dummy verify and fail
    /// with the same error as wrong passwords.
    pub async fn login(
        &self,
        request: LoginRequest,
        ctx: LoginContext,
    ) -> Result<LoginSuccess, LoginError> {
        let username_lc = request.username.trim().to_lowercase();
        let raw_token = tokens::mint_token().map_err(|_| LoginError::Unavailable)?;
        for _ in 0..LOGIN_ATTEMPTS {
            let credential = self.users.login_credential(&username_lc).await?;
            let passwords = self.passwords.clone();
            let stored = credential.as_ref().map(|found| found.password.clone());
            let password = request.password.clone();
            let verdict =
                tokio::task::spawn_blocking(move || passwords.verify(&password, stored.as_ref()))
                    .await
                    .map_err(|_| LoginError::Unavailable)?;
            let (Some(credential), Verdict::Verified { rehash }) = (credential, verdict) else {
                return Err(LoginError::InvalidCredentials);
            };
            let session = NewSession {
                user_id: credential.user_id.clone(),
                token_hash: tokens::hash_token(&raw_token),
                issued_at: ctx.now_unix,
                user_agent: ctx.user_agent.clone(),
            };
            let written = self
                .sessions
                .insert_login(session, &credential.password, rehash)
                .await
                .map_err(|error| match error {
                    SessionStoreError::Busy => LoginError::Busy,
                    SessionStoreError::Unavailable | SessionStoreError::Duplicate => {
                        LoginError::Unavailable
                    }
                })?;
            match written {
                LoginWrite::Stored => {
                    return Ok(LoginSuccess {
                        user_id: credential.user_id,
                        display_name: credential.display_name,
                        transport: request.transport.into(),
                        raw_token,
                    });
                }
                LoginWrite::CredentialChanged => continue,
            }
        }
        Err(LoginError::Busy)
    }
}

impl From<TransportParam> for Transport {
    fn from(param: TransportParam) -> Self {
        match param {
            TransportParam::Cookie => Transport::Cookie,
            TransportParam::Bearer => Transport::Bearer,
        }
    }
}
