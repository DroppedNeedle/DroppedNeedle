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
//! - Every login response (and every token mint) carries
//!   `Cache-Control: no-store`.
//!
//! Password hashing seam: production wires bcrypt verification for imported
//! rows with opportunistic Argon2id rehash on success, rows tagged
//! `bcrypt | argon2id` (spec D6). That needs the `bcrypt` + `argon2` deps plus
//! a provider-table adapter; this slice defines the `PasswordVerifier` port
//! and a fake, and the rehash-on-login step lands with the user slice.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(any(test, feature = "test-support"))]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use thiserror::Error;

use super::{
    cookies,
    extract::{Transport, unauthorized_response},
    store::{SessionKind, SessionRecord, SessionStore},
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
    /// Username (matched case-insensitively, v2 D3).
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
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
    /// Whether to mark the cookie `Secure` (HTTPS direct or proxied).
    pub secure: bool,
    /// Client label recorded on the session.
    pub user_agent: Option<String>,
    /// Now, unix seconds.
    pub now_unix: i64,
}

/// Successful login: who, how the token travels, and where it is.
pub struct LoginSuccess {
    /// Authenticated user id.
    pub user_id: String,
    /// Display name for the response body.
    pub display_name: String,
    /// Transport that carried the credential out.
    pub transport: Transport,
    /// Raw token; cookie mode embeds it only in `set_cookie`.
    pub raw_token: String,
    /// `Set-Cookie` value in cookie mode, `None` in Bearer mode.
    pub set_cookie: Option<String>,
}

/// Login failure. Both variants render identically on the wire.
#[derive(Debug, Error)]
pub enum LoginError {
    /// Unknown user OR wrong password: one uniform message, no oracle.
    #[error("Invalid username or password")]
    InvalidCredentials,
    /// Storage or entropy failure: 500 without detail.
    #[error("login unavailable")]
    Unavailable,
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
    /// Local credential for a lowercased username, or `None`.
    fn local_user(&self, username_lc: &str)
    -> impl Future<Output = Option<LocalCredential>> + Send;
}

/// Password-hash port. Production verifies bcrypt/argon2id by scheme tag and
/// rehashes legacy rows on success (spec D6); the dummy path runs the same
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
        let credential = self.users.local_user(&username_lc).await;
        let Some(credential) = credential else {
            self.verifier.dummy_verify();
            return Err(LoginError::InvalidCredentials);
        };
        if !self
            .verifier
            .verify(&request.password, &credential.stored_hash)
        {
            return Err(LoginError::InvalidCredentials);
        }
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
            .map_err(|_| LoginError::Unavailable)?;
        Ok(match request.transport {
            TransportParam::Cookie => LoginSuccess {
                user_id: credential.user_id,
                display_name: credential.display_name,
                transport: Transport::Cookie,
                set_cookie: Some(cookies::set_cookie_value(
                    &raw_token,
                    &ctx.base_path,
                    ctx.secure,
                )),
                raw_token,
            },
            TransportParam::Bearer => LoginSuccess {
                user_id: credential.user_id,
                display_name: credential.display_name,
                transport: Transport::Bearer,
                raw_token,
                set_cookie: None,
            },
        })
    }
}

/// Render a login success. `user_json` is the caller-supplied user object (its
/// shape is API-design scope); this adds the token ONLY in Bearer mode, sets
/// the cookie ONLY in cookie mode, and always stamps `no-store`.
pub fn login_response(
    user_json: serde_json::Value,
    success: &LoginSuccess,
) -> axum::response::Response {
    use axum::{Json, http::StatusCode, response::IntoResponse};
    let mut body = user_json;
    if success.transport == Transport::Bearer
        && let Some(object) = body.as_object_mut()
    {
        object.insert(
            "token".to_owned(),
            serde_json::Value::String(success.raw_token.clone()),
        );
    }
    let mut response = (StatusCode::OK, Json(body)).into_response();
    let headers = response.headers_mut();
    if let Some(set_cookie) = &success.set_cookie {
        cookies::push_set_cookie(headers, set_cookie);
    }
    if let Ok(value) = NO_STORE.parse() {
        headers.insert(axum::http::header::CACHE_CONTROL, value);
    }
    response
}

/// Render a login failure: uniform 401 for bad credentials, fixed 500 body
/// for outages (the request-scope middleware enforces the fixed body too).
pub fn login_error_response(error: &LoginError) -> axum::response::Response {
    match error {
        LoginError::InvalidCredentials => unauthorized_response(INVALID_CREDENTIALS),
        LoginError::Unavailable => {
            use axum::{Json, http::StatusCode, response::IntoResponse};
            let body = serde_json::json!({
                "error": { "code": "INTERNAL_ERROR", "message": "Internal server error", "details": null }
            });
            (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
        }
    }
}

/// Stable session-row id derived from the token hash (the production adapter
/// mints a uuid; the fake needs no uuid dependency at this layer).
fn uuid_simple(raw_token: &str, now_unix: i64) -> String {
    let digest = Sha256::digest(format!("{raw_token}:{now_unix}").as_bytes());
    format!("{digest:x}")
}

/// Fake verifier for tests: SHA-256 compare plus a counted dummy path.
/// Stored fake hashes are `fake$<sha256-hex>`.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub struct FakePasswordVerifier {
    dummy_calls: Arc<AtomicUsize>,
    verify_calls: Arc<AtomicUsize>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakePasswordVerifier {
    /// Fresh fake with zeroed counters.
    pub fn new() -> Self {
        Self {
            dummy_calls: Arc::new(AtomicUsize::new(0)),
            verify_calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Hash a password into the fake stored format.
    pub fn store(password: &str) -> String {
        format!("fake${:x}", Sha256::digest(password.as_bytes()))
    }

    /// How many dummy verifies ran (unknown-user logins).
    pub fn dummy_calls(&self) -> usize {
        self.dummy_calls.load(Ordering::SeqCst)
    }

    /// How many real verifies ran (known-user logins).
    pub fn verify_calls(&self) -> usize {
        self.verify_calls.load(Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for FakePasswordVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PasswordVerifier for FakePasswordVerifier {
    fn verify(&self, candidate: &str, stored: &str) -> bool {
        self.verify_calls.fetch_add(1, Ordering::SeqCst);
        let candidate_hash = format!("{:x}", Sha256::digest(candidate.as_bytes()));
        let expected = stored.strip_prefix("fake$").unwrap_or("");
        tokens::constant_time_eq(&candidate_hash, expected)
    }

    fn dummy_verify(&self) {
        self.dummy_calls.fetch_add(1, Ordering::SeqCst);
        let candidate_hash = format!("{:x}", Sha256::digest(b"dummy"));
        let _ = tokens::constant_time_eq(&candidate_hash, &Self::store("dummy-seed"));
    }
}

/// Fake user table for tests: map of lowercased username to credential.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Default)]
pub struct FakeUserTable {
    users: Arc<std::collections::HashMap<String, LocalCredential>>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeUserTable {
    /// Table with one user.
    pub fn with_user(username: &str, password: &str, user_id: &str, display_name: &str) -> Self {
        let mut users = std::collections::HashMap::new();
        users.insert(
            username.to_lowercase(),
            LocalCredential {
                user_id: user_id.to_owned(),
                display_name: display_name.to_owned(),
                stored_hash: FakePasswordVerifier::store(password),
            },
        );
        Self {
            users: Arc::new(users),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl CredentialLookup for FakeUserTable {
    async fn local_user(&self, username_lc: &str) -> Option<LocalCredential> {
        self.users.get(username_lc).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{store::MemorySessionStore, store::SessionStore};
    use super::*;

    fn service() -> (
        LoginService<MemorySessionStore, FakePasswordVerifier, FakeUserTable>,
        FakePasswordVerifier,
    ) {
        let verifier = FakePasswordVerifier::new();
        let users = FakeUserTable::with_user("Ada", "correct-horse-99", "user-1", "Ada");
        (
            LoginService::new(MemorySessionStore::new(), verifier.clone(), users),
            verifier,
        )
    }

    fn ctx() -> LoginContext {
        LoginContext {
            base_path: String::new(),
            secure: false,
            user_agent: None,
            now_unix: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn unknown_user_costs_one_dummy_and_matches_bad_password() {
        let (service, verifier) = service();
        let unknown = service
            .login(
                LoginRequest {
                    username: "nobody".to_owned(),
                    password: "whatever".to_owned(),
                    transport: TransportParam::Cookie,
                },
                ctx(),
            )
            .await;
        let wrong = service
            .login(
                LoginRequest {
                    username: "ada".to_owned(),
                    password: "wrong".to_owned(),
                    transport: TransportParam::Cookie,
                },
                ctx(),
            )
            .await;
        assert!(matches!(unknown, Err(LoginError::InvalidCredentials)));
        assert!(matches!(wrong, Err(LoginError::InvalidCredentials)));
        assert_eq!(verifier.dummy_calls(), 1);
        assert_eq!(verifier.verify_calls(), 1);
    }

    #[tokio::test]
    async fn success_mints_a_verifiable_session_per_transport() {
        let (service, _) = service();
        for transport in [TransportParam::Cookie, TransportParam::Bearer] {
            let success = service
                .login(
                    LoginRequest {
                        username: "ADA".to_owned(),
                        password: "correct-horse-99".to_owned(),
                        transport,
                    },
                    ctx(),
                )
                .await
                .unwrap();
            assert_eq!(success.user_id, "user-1");
            assert_eq!(
                success.set_cookie.is_some(),
                transport == TransportParam::Cookie
            );
            let stored = tokens::hash_token(&success.raw_token);
            let found = service
                .sessions
                .lookup_valid(&stored, ctx().now_unix)
                .await
                .unwrap();
            assert_eq!(found.map(|r| r.user_id).as_deref(), Some("user-1"));
        }
    }

    #[test]
    fn response_adds_token_only_for_bearer_and_cookie_only_for_cookie() {
        let user = serde_json::json!({"id": "user-1"});
        let cookie = LoginSuccess {
            user_id: "user-1".to_owned(),
            display_name: "Ada".to_owned(),
            transport: Transport::Cookie,
            raw_token: "raw".to_owned(),
            set_cookie: Some("droppedneedle_session=raw; Path=/api/v3".to_owned()),
        };
        let response = login_response(user.clone(), &cookie);
        assert!(
            response
                .headers()
                .contains_key(axum::http::header::SET_COOKIE)
        );
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .unwrap(),
            NO_STORE
        );

        let bearer = LoginSuccess {
            set_cookie: None,
            transport: Transport::Bearer,
            ..cookie
        };
        let response = login_response(user, &bearer);
        assert!(
            !response
                .headers()
                .contains_key(axum::http::header::SET_COOKIE)
        );
    }
}
