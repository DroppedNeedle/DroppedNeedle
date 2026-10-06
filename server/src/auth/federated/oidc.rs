//! OIDC login: authorize URL (PKCE S256 plus nonce), callback, exchange-code
//! hand-off.
//!
//! v2 parity: discovery is cached by the production adapter, the PKCE
//! verifier is persisted against the state row, the callback mints the
//! session and returns a 60-second single-use exchange code, and claims
//! normalisation keeps the v2 fallback chain (`name`, `preferred_username`,
//! `nickname`, email local-part, `"OIDC User"`).
//!
//! Tighter than v2: the `id_token` is required and verified (signature
//! against the provider's JWKS, issuer, audience, expiry, nonce) before
//! anything in it is trusted. Userinfo, when the provider has it, fills in
//! profile fields, but only when its `sub` matches the verified token.

use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::jwt::{IdTokenError, IdTokenRules, verify_id_token};
use super::oidc_models::Jwk;
use super::users::{
    FederatedProfile, FederatedUserStore, PROVIDER_OIDC, StoredUser, find_or_create_federated_user,
};
use super::{FederatedError, SessionIssuer, json_string};

/// Discovery document path appended to the issuer.
pub const DISCOVERY_SUFFIX: &str = "/.well-known/openid-configuration";
/// State row lifetime, seconds (v2 `_STATE_TTL`).
pub const STATE_TTL_SECS: u64 = 600;
/// Exchange-code lifetime, seconds (v2 `_CODE_TTL`).
pub const EXCHANGE_TTL_SECS: u64 = 60;
/// Random bytes for states, exchange codes, and PKCE verifiers.
pub const RANDOM_BYTES: usize = 32;

/// OIDC connection settings (typed-config owned; this is the read view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfig {
    /// Master switch.
    pub enabled: bool,
    /// Issuer base URL.
    pub issuer: String,
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret; `None` for public clients.
    pub client_secret: Option<String>,
    /// Registered callback URL.
    pub redirect_uri: String,
    /// Space-separated scopes.
    pub scopes: String,
}

impl OidcConfig {
    /// Enabled with an issuer and client id: the login page's `oidc` flag.
    pub fn is_usable(&self) -> bool {
        self.enabled && !self.issuer.is_empty() && !self.client_id.is_empty()
    }
}

/// Cached discovery document (production adapter caches per issuer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryDoc {
    /// `issuer` from the document.
    pub issuer: String,
    /// Where browsers are sent.
    pub authorization_endpoint: String,
    /// Where codes are exchanged.
    pub token_endpoint: String,
    /// Where profiles are fetched; `None` means id_token only.
    pub userinfo_endpoint: Option<String>,
    /// Where the provider publishes its signing keys.
    pub jwks_uri: String,
}

/// Tokens from the token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcTokens {
    /// Bearer token for userinfo calls.
    pub access_token: String,
    /// Stored for later refresh; may be empty.
    pub refresh_token: String,
    /// Signed identity token; empty when the provider sent none.
    pub id_token: String,
}

/// Code-exchange request. The production adapter POSTs this as a form to
/// the token endpoint: `grant_type=authorization_code` plus these fields,
/// `client_secret` only for confidential clients, `code_verifier` always
/// (PKCE was used to start the flow).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRequest {
    /// Token endpoint URL.
    pub token_endpoint: String,
    /// The authorization code.
    pub code: String,
    /// Must match the authorize call.
    pub redirect_uri: String,
    /// OAuth client id.
    pub client_id: String,
    /// Confidential clients only.
    pub client_secret: Option<String>,
    /// The verifier stored against the state row.
    pub code_verifier: Option<String>,
}

/// Raw claim fields before normalisation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawClaims {
    /// Subject; required.
    pub sub: Option<String>,
    /// Email; optional.
    pub email: Option<String>,
    /// The IdP's `email_verified` claim; absent reads as unverified.
    pub email_verified: Option<bool>,
    /// Display name candidates, tried in order.
    pub name: Option<String>,
    /// Display name candidates, tried in order.
    pub preferred_username: Option<String>,
    /// Display name candidates, tried in order.
    pub nickname: Option<String>,
    /// Avatar candidates, tried in order.
    pub picture: Option<String>,
    /// Avatar candidates, tried in order.
    pub avatar: Option<String>,
}

/// Normalised profile: every field resolved, `sub` enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcClaims {
    /// Subject; the provider uid.
    pub sub: String,
    /// Lowercased email, or `None` when absent/blank.
    pub email: Option<String>,
    /// True only when the IdP sent `email_verified: true`.
    pub email_verified: bool,
    /// Resolved display name.
    pub name: String,
    /// Avatar URL, or `None` when absent/blank.
    pub thumb: Option<String>,
}

/// Network edge for OIDC. Production adapter rules (v2 parity where v2
/// had the step): discovery with a 10s timeout, cached per issuer (non-200
/// is `ProviderUnavailable`, a document missing an endpoint or `jwks_uri`
/// is `NotConfigured`); token POST with a 15s timeout (transport failure
/// is `ProviderUnavailable`, non-2xx is `Authentication("OIDC token
/// exchange failed")`, bad JSON is `ProviderUnavailable`); signing keys
/// cached per `jwks_uri` and refetched when `refresh` is set; userinfo
/// answers `None` on any failure so the verified `id_token` claims stand.
pub trait OidcIdp: Clone + Send + Sync + 'static {
    /// Fetch (and cache) the discovery document for `issuer`.
    fn discover(
        &self,
        issuer: &str,
    ) -> impl Future<Output = Result<DiscoveryDoc, FederatedError>> + Send;

    /// Exchange an authorization code for tokens.
    fn exchange_code(
        &self,
        request: &TokenRequest,
    ) -> impl Future<Output = Result<OidcTokens, FederatedError>> + Send;

    /// The provider's published signing keys. `refresh` bypasses the cache
    /// (used once when a token names a key the cached set lacks).
    fn signing_keys(
        &self,
        jwks_uri: &str,
        refresh: bool,
    ) -> impl Future<Output = Result<Vec<Jwk>, FederatedError>> + Send;

    /// Userinfo claims, or `None` when the endpoint fails.
    fn userinfo(
        &self,
        userinfo_endpoint: &str,
        access_token: &str,
    ) -> impl Future<Output = Option<RawClaims>> + Send;
}

/// Short-lived PKCE state rows (`auth_oidc_states` in production).
pub trait OidcStateStore: Clone + Send + Sync + 'static {
    /// Persist `state` with its verifier for [`STATE_TTL_SECS`].
    fn store_state(
        &self,
        state: &str,
        code_verifier: &str,
    ) -> impl Future<Output = Result<(), FederatedError>> + Send;

    /// Take and delete `state`; `None` means unknown or expired.
    fn consume_state(
        &self,
        state: &str,
    ) -> impl Future<Output = Result<Option<String>, FederatedError>> + Send;
}

/// Single-use exchange codes bridging the callback and the SPA.
pub trait OidcExchangeStore: Clone + Send + Sync + 'static {
    /// Persist an exchange code for [`EXCHANGE_TTL_SECS`].
    fn store_exchange(
        &self,
        code: &str,
        user_id: &str,
        raw_token: &str,
    ) -> impl Future<Output = Result<(), FederatedError>> + Send;

    /// Take and delete an exchange code; `None` means unknown or expired.
    fn take_exchange(
        &self,
        code: &str,
    ) -> impl Future<Output = Result<Option<(String, String)>, FederatedError>> + Send;
}

/// OIDC login service. Generic over stores so tests inject fakes.
#[derive(Debug, Clone)]
pub struct OidcLogin<S, I, T, E, N> {
    users: S,
    idp: I,
    states: T,
    exchanges: E,
    sessions: N,
}

impl<S, I, T, E, N> OidcLogin<S, I, T, E, N> {
    /// Wire the service from its ports.
    pub fn new(users: S, idp: I, states: T, exchanges: E, sessions: N) -> Self {
        Self {
            users,
            idp,
            states,
            exchanges,
            sessions,
        }
    }
}

impl<S, I, T, E, N> OidcLogin<S, I, T, E, N>
where
    S: FederatedUserStore,
    I: OidcIdp,
    T: OidcStateStore,
    E: OidcExchangeStore,
    N: SessionIssuer,
{
    /// Start a login: discover, mint PKCE + state, return the browser URL.
    /// The nonce is derived from the verifier, so the state row holds
    /// everything the callback needs.
    pub async fn build_authorize_url(&self, config: &OidcConfig) -> Result<String, FederatedError> {
        let doc = self.idp.discover(&require_config(config)?.issuer).await?;
        let verifier = generate_verifier()?;
        let state = generate_state()?;
        self.states.store_state(&state, &verifier).await?;
        Ok(authorize_url(
            &doc.authorization_endpoint,
            config,
            &AuthorizeParams {
                state: &state,
                challenge: &pkce_challenge(&verifier),
                nonce: &nonce_for(&verifier),
            },
        ))
    }

    /// Finish a login: consume state, exchange the code, import the user,
    /// mint a session, and return a single-use exchange code for the SPA.
    pub async fn handle_callback(
        &self,
        config: &OidcConfig,
        code: &str,
        state: &str,
        user_agent: Option<&str>,
    ) -> Result<String, FederatedError> {
        let verifier = self.states.consume_state(state).await?.ok_or_else(|| {
            FederatedError::Authentication("Invalid or expired OIDC state".to_owned())
        })?;
        let config = require_config(config)?;
        let doc = self.idp.discover(&config.issuer).await?;
        let nonce = nonce_for(&verifier);
        let tokens = self
            .idp
            .exchange_code(&TokenRequest {
                token_endpoint: doc.token_endpoint.clone(),
                code: code.to_owned(),
                redirect_uri: config.redirect_uri.clone(),
                client_id: config.client_id.clone(),
                client_secret: config.client_secret.clone(),
                code_verifier: Some(verifier),
            })
            .await?;
        let mut raw = self.verified_claims(&config, &doc, &tokens, &nonce).await?;
        if let Some(endpoint) = doc.userinfo_endpoint.as_deref()
            && !tokens.access_token.is_empty()
            && let Some(profile) = self.idp.userinfo(endpoint, &tokens.access_token).await
        {
            raw = merge_userinfo(raw, profile);
        }
        let claims = normalise_claims(&raw)?;
        let user = find_or_create_federated_user(
            &self.users,
            PROVIDER_OIDC,
            &FederatedProfile {
                provider_uid: claims.sub,
                display_name: claims.name,
                email: claims.email,
                email_verified: claims.email_verified,
                avatar_url: claims.thumb,
                token_json: sealed_token_json(&tokens.access_token, &tokens.refresh_token),
            },
        )
        .await?;
        let raw_token = self.sessions.issue_session(&user.id, user_agent).await?;
        let exchange_code = generate_state()?;
        self.exchanges
            .store_exchange(&exchange_code, &user.id, &raw_token)
            .await?;
        Ok(exchange_code)
    }

    /// Verify the `id_token` and return its claims. A token naming a key
    /// the cached set lacks refreshes the set once (key rotation).
    async fn verified_claims(
        &self,
        config: &OidcConfig,
        doc: &DiscoveryDoc,
        tokens: &OidcTokens,
        nonce: &str,
    ) -> Result<RawClaims, FederatedError> {
        if tokens.id_token.is_empty() {
            return Err(FederatedError::Authentication(
                "OIDC provider returned no id_token; check that the scopes include openid"
                    .to_owned(),
            ));
        }
        let rules = IdTokenRules {
            issuer: &doc.issuer,
            client_id: &config.client_id,
            client_secret: config.client_secret.as_deref(),
            nonce,
            now_unix: unix_now(),
        };
        let keys = self.idp.signing_keys(&doc.jwks_uri, false).await?;
        let verified = match verify_id_token(&tokens.id_token, &keys, &rules) {
            Err(IdTokenError::UnknownKey) => {
                let keys = self.idp.signing_keys(&doc.jwks_uri, true).await?;
                verify_id_token(&tokens.id_token, &keys, &rules)
            }
            other => other,
        };
        match verified {
            Ok(claims) => Ok(raw_claims(&claims)),
            Err(error) => {
                tracing::warn!(%error, "OIDC id_token rejected");
                Err(FederatedError::Authentication(error.to_string()))
            }
        }
    }

    /// Swap a single-use exchange code for the user and raw token.
    pub async fn exchange_code(
        &self,
        exchange_code: &str,
    ) -> Result<(StoredUser, String), FederatedError> {
        let (user_id, raw_token) = self
            .exchanges
            .take_exchange(exchange_code)
            .await?
            .ok_or_else(|| {
                FederatedError::Authentication("Invalid or expired exchange code".to_owned())
            })?;
        let user = self
            .users
            .get_user_by_id(&user_id)
            .await?
            .ok_or_else(|| FederatedError::Authentication("User not found".to_owned()))?;
        Ok((user, raw_token))
    }
}

/// Reject a disabled or half-configured provider.
fn require_config(config: &OidcConfig) -> Result<OidcConfig, FederatedError> {
    if !config.enabled {
        return Err(FederatedError::NotConfigured(
            "OIDC login is not enabled".to_owned(),
        ));
    }
    if config.issuer.is_empty() || config.client_id.is_empty() || config.redirect_uri.is_empty() {
        return Err(FederatedError::NotConfigured(
            "OIDC configuration is incomplete".to_owned(),
        ));
    }
    Ok(config.clone())
}

/// Per-login values baked into the authorize URL.
#[derive(Debug, Clone, Copy)]
pub struct AuthorizeParams<'a> {
    /// CSRF state, echoed back on the callback.
    pub state: &'a str,
    /// PKCE S256 challenge.
    pub challenge: &'a str,
    /// Replay guard the `id_token` must echo.
    pub nonce: &'a str,
}

/// Build the browser redirect URL. Param order and `+` for spaces match
/// v2 (`urllib.parse.urlencode` insertion order); `nonce` is new and last.
pub fn authorize_url(
    authorization_endpoint: &str,
    config: &OidcConfig,
    params: &AuthorizeParams<'_>,
) -> String {
    let separator = if authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    format!(
        "{authorization_endpoint}{separator}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256&nonce={}",
        form_encode(&config.client_id),
        form_encode(&config.redirect_uri),
        form_encode(&config.scopes),
        form_encode(params.state),
        form_encode(params.challenge),
        form_encode(params.nonce),
    )
}

/// The nonce for a login, derived from its PKCE verifier: unpadded
/// base64url of SHA-256 over a fixed label plus the verifier. The verifier
/// never leaves the server, so the nonce cannot be predicted, and the
/// label keeps it distinct from the PKCE challenge.
pub fn nonce_for(verifier: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"droppedneedle-oidc-nonce:");
    digest.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest.finalize())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Percent-encode one form value (Python `quote_plus` parity: alnum plus
/// `-_.~` pass through, space becomes `+`, the rest `%XX` uppercase).
pub fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// S256 challenge for a verifier: unpadded base64url of its SHA-256.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Fresh PKCE verifier: 32 random bytes, unpadded base64url (43 chars).
pub fn generate_verifier() -> Result<String, FederatedError> {
    let mut bytes = [0u8; RANDOM_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| FederatedError::RngUnavailable)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Fresh state/exchange code: 32 random bytes, padded base64url (v2
/// `_random_state` parity, 44 chars).
pub fn generate_state() -> Result<String, FederatedError> {
    let mut bytes = [0u8; RANDOM_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| FederatedError::RngUnavailable)?;
    Ok(URL_SAFE.encode(bytes))
}

/// Plaintext token JSON for the store to seal (v2 field names kept).
pub fn sealed_token_json(access_token: &str, refresh_token: &str) -> String {
    format!(
        "{{\"access_token\":{},\"refresh_token\":{}}}",
        json_string(access_token),
        json_string(refresh_token),
    )
}

/// Normalise raw claims. `sub` is required; email is lowercased (blank
/// becomes `None`); the name falls back through `name`,
/// `preferred_username`, `nickname`, the raw email local-part, then
/// `"OIDC User"`; the avatar falls back through `picture`, `avatar`.
pub fn normalise_claims(raw: &RawClaims) -> Result<OidcClaims, FederatedError> {
    let sub = raw.sub.as_deref().unwrap_or("");
    if sub.is_empty() {
        return Err(FederatedError::Authentication(
            "OIDC token missing 'sub' claim".to_owned(),
        ));
    }
    let email_raw = raw.email.as_deref().unwrap_or("");
    let email = email_raw.to_lowercase().trim().to_owned();
    let email = if email.is_empty() { None } else { Some(email) };
    let local_part = email_raw.split('@').next().unwrap_or("");
    let name = [
        raw.name.as_deref(),
        raw.preferred_username.as_deref(),
        raw.nickname.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| !candidate.is_empty())
    .unwrap_or(if local_part.is_empty() {
        "OIDC User"
    } else {
        local_part
    })
    .to_owned();
    let thumb = [raw.picture.as_deref(), raw.avatar.as_deref()]
        .into_iter()
        .flatten()
        .find(|candidate| !candidate.is_empty())
        .map(str::to_owned);
    Ok(OidcClaims {
        sub: sub.to_owned(),
        email,
        email_verified: raw.email_verified == Some(true),
        name,
        thumb,
    })
}

/// Read the claim fields we use from a JSON object (verified `id_token`
/// payload or userinfo answer).
pub fn raw_claims(obj: &Map<String, Value>) -> RawClaims {
    RawClaims {
        sub: str_field(obj, "sub"),
        email: str_field(obj, "email"),
        email_verified: bool_field(obj, "email_verified"),
        name: str_field(obj, "name"),
        preferred_username: str_field(obj, "preferred_username"),
        nickname: str_field(obj, "nickname"),
        picture: str_field(obj, "picture"),
        avatar: str_field(obj, "avatar"),
    }
}

/// Lay userinfo over the verified token claims, field by field. Userinfo
/// for another subject is ignored (OIDC Core 5.3.2).
fn merge_userinfo(verified: RawClaims, userinfo: RawClaims) -> RawClaims {
    if userinfo.sub != verified.sub {
        tracing::warn!("OIDC userinfo subject differs from the id_token; ignoring userinfo");
        return verified;
    }
    RawClaims {
        sub: verified.sub,
        email: userinfo.email.or(verified.email),
        email_verified: userinfo.email_verified.or(verified.email_verified),
        name: userinfo.name.or(verified.name),
        preferred_username: userinfo.preferred_username.or(verified.preferred_username),
        nickname: userinfo.nickname.or(verified.nickname),
        picture: userinfo.picture.or(verified.picture),
        avatar: userinfo.avatar.or(verified.avatar),
    }
}

fn str_field(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key)?.as_str().map(str::to_owned)
}

/// A boolean claim; some IdPs send `"true"`/`"false"` strings instead.
fn bool_field(obj: &Map<String, Value>, key: &str) -> Option<bool> {
    match obj.get(key)? {
        Value::Bool(flag) => Some(*flag),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_appendix_b() {
        let challenge = pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn form_encode_matches_quote_plus() {
        assert_eq!(form_encode("a b+c~d"), "a+b%2Bc~d");
        assert_eq!(
            form_encode("https://x.test/cb?a=1"),
            "https%3A%2F%2Fx.test%2Fcb%3Fa%3D1"
        );
    }

    #[test]
    fn generated_values_match_v2_shapes() {
        let verifier = generate_verifier().unwrap();
        assert_eq!(verifier.len(), 43);
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let state = generate_state().unwrap();
        assert_eq!(state.len(), 44);
        assert!(state.ends_with('='));
    }
}
