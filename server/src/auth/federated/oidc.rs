//! OIDC login: authorize URL (PKCE S256), callback, exchange-code hand-off.
//!
//! v2 parity: discovery is cached by the production adapter, the PKCE
//! verifier is persisted against the state row, the callback mints the
//! session and returns a 60-second single-use exchange code, and userinfo
//! is tried before the `id_token` fallback. Claims normalisation keeps
//! the v2 fallback chain (`name`, `preferred_username`, `nickname`,
//! email local-part, `"OIDC User"`).

use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

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
}

/// Tokens from the token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcTokens {
    /// Bearer [REDACTED] userinfo calls.
    pub access_token: String,
    /// Stored for later refresh; may be empty.
    pub refresh_token: String,
    /// JWT fallback for claims.
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
    /// Resolved display name.
    pub name: String,
    /// Avatar URL, or `None` when absent/blank.
    pub thumb: Option<String>,
}

/// Network edge for OIDC. Production adapter rules (v2 parity):
/// discovery over HTTPS with a 10s timeout (non-200 becomes
/// `ProviderUnavailable`, missing endpoints become `NotConfigured`);
/// token POST with a 15s timeout (transport failure is
/// `ProviderUnavailable`, non-2xx is `Authentication("OIDC token exchange
/// failed")`, bad JSON is `ProviderUnavailable`); `fetch_claims` tries
/// userinfo first (200 only) then [`jwt_payload_claims`], else
/// `Authentication("Could not retrieve user info from OIDC provider")`.
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

    /// Resolve claims: userinfo when reachable, else the id_token payload.
    fn fetch_claims(
        &self,
        userinfo_endpoint: Option<&str>,
        access_token: &str,
        id_token: &str,
    ) -> impl Future<Output = Result<RawClaims, FederatedError>> + Send;
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
    /// The `oidc` flag in the providers response: enabled with an issuer
    /// and client id set.
    pub fn providers_flag(config: &OidcConfig) -> bool {
        config.enabled && !config.issuer.is_empty() && !config.client_id.is_empty()
    }

    /// Start a login: discover, mint PKCE + state, return the browser URL.
    pub async fn build_authorize_url(&self, config: &OidcConfig) -> Result<String, FederatedError> {
        let doc = self.idp.discover(&require_config(config)?.issuer).await?;
        let verifier = generate_verifier()?;
        let challenge = pkce_challenge(&verifier);
        let state = generate_state()?;
        self.states.store_state(&state, &verifier).await?;
        Ok(authorize_url(
            &doc.authorization_endpoint,
            config,
            &state,
            &challenge,
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
        let raw = self
            .idp
            .fetch_claims(
                doc.userinfo_endpoint.as_deref(),
                &tokens.access_token,
                &tokens.id_token,
            )
            .await?;
        let claims = normalise_claims(&raw)?;
        let user = find_or_create_federated_user(
            &self.users,
            PROVIDER_OIDC,
            &FederatedProfile {
                provider_uid: claims.sub,
                display_name: claims.name,
                email: claims.email,
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

/// Build the browser redirect URL. Param order and `+` for spaces match
/// v2 (`urllib.parse.urlencode` insertion order).
pub fn authorize_url(
    authorization_endpoint: &str,
    config: &OidcConfig,
    state: &str,
    challenge: &str,
) -> String {
    format!(
        "{authorization_endpoint}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        form_encode(&config.client_id),
        form_encode(&config.redirect_uri),
        form_encode(&config.scopes),
        form_encode(state),
        form_encode(challenge),
    )
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
        name,
        thumb,
    })
}

/// Decode the payload of a compact JWT into raw claims. `None` on any
/// malformed input (wrong part count, bad base64, bad JSON).
pub fn jwt_payload_claims(id_token: &str) -> Option<RawClaims> {
    let mut parts = id_token.split('.');
    let (Some(_header), Some(payload), Some(_sig)) = (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if parts.next().is_some() {
        return None;
    }
    let mut padded = payload.to_owned();
    if padded.len() % 4 == 1 {
        return None;
    }
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let decoded = URL_SAFE.decode(padded.as_bytes()).ok()?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    let obj = value.as_object()?;
    Some(RawClaims {
        sub: str_field(obj, "sub"),
        email: str_field(obj, "email"),
        name: str_field(obj, "name"),
        preferred_username: str_field(obj, "preferred_username"),
        nickname: str_field(obj, "nickname"),
        picture: str_field(obj, "picture"),
        avatar: str_field(obj, "avatar"),
    })
}

fn str_field(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key)?.as_str().map(str::to_owned)
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
