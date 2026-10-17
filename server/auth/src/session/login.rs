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
//! - A store outage is [`LoginError::Unavailable`], never a failed login. A
//!   store too busy to answer in time is [`LoginError::Busy`], never either.
//!
//! Password hashing seam: production wires bcrypt verification for imported
//! rows with opportunistic Argon2id rehash on success, rows tagged
//! `bcrypt | argon2id`. This module defines the `PasswordVerifier` port;
//! [`crate::passwords`] holds the production verifier.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    extract::Transport,
    store::{SessionKind, SessionRecord, SessionStore, SessionStoreError},
    tokens,
};

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

/// Stored local credential returned by the user lookup.
#[derive(Debug, Clone)]
pub struct LocalCredential {
    /// User id.
    pub user_id: String,
    /// Display name.
    pub display_name: String,
    /// Tagged stored hash (`scheme + payload`; the verifier interprets it).
    pub stored_hash: String,
}

/// User-lookup port. Production reads `auth_users` + the `local` provider row.
pub trait CredentialLookup: Clone + Send + Sync + 'static {
    /// Local credential for a lowercased username, `None` when the user or
    /// its local credential is absent, `Err` when the store failed.
    fn local_user(
        &self,
        username_lc: &str,
    ) -> impl Future<Output = Result<Option<LocalCredential>, LoginError>> + Send;
}

/// Password-hash port. Production verifies bcrypt/argon2id by scheme tag and
/// rehashes legacy rows on success; the dummy path runs the same
/// cost class so unknown users cost one verify, exactly like wrong passwords.
pub trait PasswordVerifier: Clone + Send + Sync + 'static {
    /// Verify a candidate against the tagged stored hash.
    fn verify(&self, candidate: &str, stored: &str) -> bool;
    /// Burn one verify-equivalent against a fixed dummy hash (unknown user).
    fn dummy_verify(&self);
}

/// `Cache-Control` value on every login/token-mint response.
pub const NO_STORE: &str = "no-store";

/// Uniform login-failure message. One string for both failure causes.
pub const INVALID_CREDENTIALS: &str = "Invalid username or password";

/// Local-login service: lookup, verify-or-dummy, mint, hand off.
#[derive(Debug, Clone)]
pub struct LoginService<S, V, C> {
    sessions: S,
    verifier: V,
    users: C,
}

impl<S, V, C> LoginService<S, V, C>
where
    S: SessionStore,
    V: PasswordVerifier,
    C: CredentialLookup,
{
    /// Wire the service from its ports.
    pub fn new(sessions: S, verifier: V, users: C) -> Self {
        Self {
            sessions,
            verifier,
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
        let credential = self.users.local_user(&username_lc).await?;
        let verifier = self.verifier.clone();
        let stored = credential.as_ref().map(|found| found.stored_hash.clone());
        let password = request.password;
        let verified = tokio::task::spawn_blocking(move || match stored {
            Some(stored) => verifier.verify(&password, &stored),
            None => {
                verifier.dummy_verify();
                false
            }
        })
        .await
        .map_err(|_| LoginError::Unavailable)?;
        let Some(credential) = credential.filter(|_| verified) else {
            return Err(LoginError::InvalidCredentials);
        };
        let raw_token = tokens::mint_token().map_err(|_| LoginError::Unavailable)?;
        let record = SessionRecord {
            id: uuid_simple(&raw_token, ctx.now_unix),
            user_id: credential.user_id.clone(),
            token_hash: tokens::hash_token(&raw_token),
            kind: SessionKind::Standard,
            label: None,
            issued_at: ctx.now_unix,
            expires_at: tokens::expires_at(ctx.now_unix),
            last_seen_at: ctx.now_unix,
            revoked: false,
            user_agent: ctx.user_agent,
        };
        self.sessions
            .insert(record)
            .await
            .map_err(|error| match error {
                SessionStoreError::Busy => LoginError::Busy,
                SessionStoreError::Unavailable | SessionStoreError::Duplicate => {
                    LoginError::Unavailable
                }
            })?;
        Ok(LoginSuccess {
            user_id: credential.user_id,
            display_name: credential.display_name,
            transport: request.transport.into(),
            raw_token,
        })
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

/// Stable session-row id derived from the token hash, so this layer needs
/// no uuid dependency.
fn uuid_simple(raw_token: &str, now_unix: i64) -> String {
    let digest = Sha256::digest(format!("{raw_token}:{now_unix}").as_bytes());
    format!("{digest:x}")
}
