//! Scripted fakes for the federated slice: in-memory stores, canned IdP
//! answers, and a deterministic session issuer. No network, no clock, no
//! real crypto. Clones share state through `Arc`, so a test can hold one
//! handle while the service under test holds another.
//!
//! Test-only by convention (same precedent as the sibling in-memory
//! session store): production adapters replace every fake at wiring.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::FederatedError;
use super::SessionIssuer;
use super::jellyfin_login::{JellyfinConnectionLink, JellyfinIdp, JellyfinProfile};
use super::oidc::{
    DiscoveryDoc, OidcConfig, OidcExchangeStore, OidcIdp, OidcStateStore, OidcTokens, RawClaims,
    TokenRequest,
};
use super::password_import::PasswordHasher;
use super::plex::{PlexAccount, PlexConnectionLink, PlexPin, PlexPinClient, PlexProfile};
use super::users::{FederatedUserStore, NewFederatedUser, ProviderBinding, StoredUser};

fn locked<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, FederatedError> {
    mutex
        .lock()
        .map_err(|_| FederatedError::StoreUnavailable("fake lock poisoned".to_owned()))
}

// --- user store ---

#[derive(Debug, Default)]
struct FakeUserRows {
    by_id: HashMap<String, StoredUser>,
    by_email: HashMap<String, String>,
    by_username: HashMap<String, String>,
    providers: HashMap<(String, String), ProviderBinding>,
    provider_tokens: HashMap<String, String>,
    next_user: u64,
    next_provider: u64,
}

/// In-memory [`FederatedUserStore`]. Ids are `user-N` / `provider-N`.
#[derive(Debug, Clone, Default)]
pub struct FakeUserStore {
    rows: Arc<Mutex<FakeUserRows>>,
}

impl FakeUserStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sealed tokens currently stored on a binding (plaintext in the fake).
    pub fn provider_tokens(&self, binding_id: &str) -> Option<String> {
        locked(&self.rows)
            .ok()?
            .provider_tokens
            .get(binding_id)
            .cloned()
    }

    /// Binding id for a provider pair, when one exists.
    pub fn binding_id(&self, provider: &str, uid: &str) -> Option<String> {
        locked(&self.rows)
            .ok()?
            .providers
            .get(&(provider.to_owned(), uid.to_owned()))
            .map(|binding| binding.id.clone())
    }
}

impl FederatedUserStore for FakeUserStore {
    async fn get_provider(
        &self,
        provider: &str,
        provider_uid: &str,
    ) -> Result<Option<ProviderBinding>, FederatedError> {
        let rows = locked(&self.rows)?;
        Ok(rows
            .providers
            .get(&(provider.to_owned(), provider_uid.to_owned()))
            .cloned())
    }

    async fn get_user_by_id(&self, user_id: &str) -> Result<Option<StoredUser>, FederatedError> {
        let rows = locked(&self.rows)?;
        Ok(rows.by_id.get(user_id).cloned())
    }

    async fn get_user_by_email(&self, email: &str) -> Result<Option<StoredUser>, FederatedError> {
        let rows = locked(&self.rows)?;
        let id = rows.by_email.get(email).cloned();
        Ok(id.and_then(|id| rows.by_id.get(&id).cloned()))
    }

    async fn get_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<StoredUser>, FederatedError> {
        let rows = locked(&self.rows)?;
        let id = rows.by_username.get(username).cloned();
        Ok(id.and_then(|id| rows.by_id.get(&id).cloned()))
    }

    async fn has_any_users(&self) -> Result<bool, FederatedError> {
        Ok(!locked(&self.rows)?.by_id.is_empty())
    }

    async fn create_user(&self, user: NewFederatedUser) -> Result<StoredUser, FederatedError> {
        let mut rows = locked(&self.rows)?;
        if rows.by_username.contains_key(&user.username) {
            return Err(FederatedError::UsernameTaken);
        }
        if let Some(email) = user.email.as_deref()
            && rows.by_email.contains_key(email)
        {
            return Err(FederatedError::UsernameTaken);
        }
        rows.next_user += 1;
        let stored = StoredUser {
            id: format!("user-{}", rows.next_user),
            display_name: user.display_name,
            role: user.role,
            email: user.email.clone(),
            avatar_url: user.avatar_url,
            username: user.username.clone(),
            username_display: user.username_display,
        };
        rows.by_username
            .insert(stored.username.clone(), stored.id.clone());
        if let Some(email) = stored.email.clone() {
            rows.by_email.insert(email, stored.id.clone());
        }
        rows.by_id.insert(stored.id.clone(), stored.clone());
        Ok(stored)
    }

    async fn create_provider(
        &self,
        user_id: &str,
        provider: &str,
        provider_uid: &str,
        token_json: &str,
    ) -> Result<ProviderBinding, FederatedError> {
        let mut rows = locked(&self.rows)?;
        rows.next_provider += 1;
        let binding = ProviderBinding {
            id: format!("provider-{}", rows.next_provider),
            user_id: user_id.to_owned(),
            provider: provider.to_owned(),
            provider_uid: provider_uid.to_owned(),
        };
        rows.provider_tokens
            .insert(binding.id.clone(), token_json.to_owned());
        rows.providers.insert(
            (provider.to_owned(), provider_uid.to_owned()),
            binding.clone(),
        );
        Ok(binding)
    }

    async fn update_provider_tokens(
        &self,
        binding_id: &str,
        token_json: &str,
    ) -> Result<(), FederatedError> {
        locked(&self.rows)?
            .provider_tokens
            .insert(binding_id.to_owned(), token_json.to_owned());
        Ok(())
    }
}

// --- sessions ---

#[derive(Debug, Default)]
struct FakeSessionRows {
    live: HashMap<String, String>,
    next: u64,
}

/// Deterministic [`SessionIssuer`]: tokens are `test-session-N`.
#[derive(Debug, Clone, Default)]
pub struct FakeSessionIssuer {
    rows: Arc<Mutex<FakeSessionRows>>,
}

impl FakeSessionIssuer {
    /// Empty issuer.
    pub fn new() -> Self {
        Self::default()
    }

    /// True while the token is live.
    pub fn is_live(&self, token: &str) -> bool {
        locked(&self.rows)
            .map(|rows| rows.live.contains_key(token))
            .unwrap_or(false)
    }

    /// Sessions issued so far.
    pub fn issued(&self) -> u64 {
        locked(&self.rows).map(|rows| rows.next).unwrap_or(0)
    }

    /// Drop every session (simulates the import wipe).
    pub fn revoke_all(&self) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.live.clear();
        }
    }
}

impl SessionIssuer for FakeSessionIssuer {
    async fn issue_session(
        &self,
        user_id: &str,
        _user_agent: Option<&str>,
    ) -> Result<String, FederatedError> {
        let mut rows = locked(&self.rows)?;
        rows.next += 1;
        let token = format!("test-session-{}", rows.next);
        rows.live.insert(token.clone(), user_id.to_owned());
        Ok(token)
    }
}

// --- OIDC ---

/// Scripted [`OidcIdp`]. Fields are `pub` so tests set exact answers;
/// `calls` records method names in order.
#[derive(Debug, Clone)]
pub struct FakeOidcIdp {
    /// Discovery answer.
    pub discovery: Result<DiscoveryDoc, FederatedError>,
    /// Token answer.
    pub tokens: Result<OidcTokens, FederatedError>,
    /// Claims answer.
    pub claims: Result<RawClaims, FederatedError>,
    /// Last token request seen.
    pub last_token_request: Arc<Mutex<Option<TokenRequest>>>,
    /// Call log.
    pub calls: Arc<Mutex<Vec<String>>>,
}

impl FakeOidcIdp {
    /// Fake answering with the given discovery, tokens, and claims.
    pub fn new(discovery: DiscoveryDoc, tokens: OidcTokens, claims: RawClaims) -> Self {
        Self {
            discovery: Ok(discovery),
            tokens: Ok(tokens),
            claims: Ok(claims),
            last_token_request: Arc::new(Mutex::new(None)),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A usable default config matching [`FakeOidcIdp::new`] docs.
    pub fn config() -> OidcConfig {
        OidcConfig {
            enabled: true,
            issuer: "https://idp.test".to_owned(),
            client_id: "droppedneedle".to_owned(),
            client_secret: Some("secret".to_owned()),
            redirect_uri: "https://music.test/api/v3/auth/oidc/callback".to_owned(),
            scopes: "openid profile email".to_owned(),
        }
    }

    /// A discovery doc under `https://idp.test`.
    pub fn discovery_doc() -> DiscoveryDoc {
        DiscoveryDoc {
            issuer: "https://idp.test".to_owned(),
            authorization_endpoint: "https://idp.test/authorize".to_owned(),
            token_endpoint: "https://idp.test/token".to_owned(),
            userinfo_endpoint: Some("https://idp.test/userinfo".to_owned()),
        }
    }

    fn log(&self, call: &str) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call.to_owned());
        }
    }
}

impl OidcIdp for FakeOidcIdp {
    async fn discover(&self, _issuer: &str) -> Result<DiscoveryDoc, FederatedError> {
        self.log("discover");
        self.discovery.clone()
    }

    async fn exchange_code(&self, request: &TokenRequest) -> Result<OidcTokens, FederatedError> {
        self.log("exchange_code");
        if let Ok(mut last) = self.last_token_request.lock() {
            *last = Some(request.clone());
        }
        self.tokens.clone()
    }

    async fn fetch_claims(
        &self,
        _userinfo_endpoint: Option<&str>,
        _access_token: &str,
        _id_token: &str,
    ) -> Result<RawClaims, FederatedError> {
        self.log("fetch_claims");
        self.claims.clone()
    }
}

/// In-memory [`OidcStateStore`]; TTLs are the caller's problem.
#[derive(Debug, Clone, Default)]
pub struct FakeOidcStates {
    rows: Arc<Mutex<HashMap<String, String>>>,
}

impl FakeOidcStates {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl OidcStateStore for FakeOidcStates {
    async fn store_state(&self, state: &str, code_verifier: &str) -> Result<(), FederatedError> {
        locked(&self.rows)?.insert(state.to_owned(), code_verifier.to_owned());
        Ok(())
    }

    async fn consume_state(&self, state: &str) -> Result<Option<String>, FederatedError> {
        Ok(locked(&self.rows)?.remove(state))
    }
}

/// In-memory [`OidcExchangeStore`]; single-use by construction.
#[derive(Debug, Clone, Default)]
pub struct FakeOidcExchanges {
    rows: Arc<Mutex<HashMap<String, (String, String)>>>,
}

impl FakeOidcExchanges {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl OidcExchangeStore for FakeOidcExchanges {
    async fn store_exchange(
        &self,
        code: &str,
        user_id: &str,
        raw_token: &str,
    ) -> Result<(), FederatedError> {
        locked(&self.rows)?.insert(code.to_owned(), (user_id.to_owned(), raw_token.to_owned()));
        Ok(())
    }

    async fn take_exchange(&self, code: &str) -> Result<Option<(String, String)>, FederatedError> {
        Ok(locked(&self.rows)?.remove(code))
    }
}

// --- Jellyfin ---

/// Scripted [`JellyfinIdp`]: `(username, password)` pairs map to profiles.
#[derive(Debug, Clone, Default)]
pub struct FakeJellyfinIdp {
    /// False simulates an unconfigured server.
    pub configured: bool,
    /// Accepted credentials and their profiles.
    pub accepted: HashMap<(String, String), JellyfinProfile>,
    /// Call log.
    pub calls: Arc<Mutex<Vec<String>>>,
}

impl FakeJellyfinIdp {
    /// Fake accepting nothing (tests insert pairs).
    pub fn new(configured: bool) -> Self {
        Self {
            configured,
            accepted: HashMap::new(),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Accept one credential pair with its profile.
    pub fn accept(&mut self, username: &str, password: &str, profile: JellyfinProfile) {
        self.accepted
            .insert((username.to_owned(), password.to_owned()), profile);
    }
}

impl JellyfinIdp for FakeJellyfinIdp {
    fn is_configured(&self) -> bool {
        self.configured
    }

    async fn authenticate_by_name(
        &self,
        username: &str,
        password: &str,
    ) -> Result<JellyfinProfile, FederatedError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(format!("authenticate:{username}"));
        }
        self.accepted
            .get(&(username.to_owned(), password.to_owned()))
            .cloned()
            .ok_or_else(|| {
                FederatedError::Authentication("Invalid Jellyfin username or password".to_owned())
            })
    }
}

/// Recording [`JellyfinConnectionLink`].
#[derive(Debug, Clone, Default)]
pub struct FakeJellyfinLink {
    /// Linked `(user_id, jellyfin_user_id)` pairs in order.
    pub linked: Arc<Mutex<Vec<(String, String)>>>,
}

impl FakeJellyfinLink {
    /// Empty link log.
    pub fn new() -> Self {
        Self::default()
    }
}

impl JellyfinConnectionLink for FakeJellyfinLink {
    async fn link(&self, user_id: &str, profile: &JellyfinProfile) {
        if let Ok(mut linked) = self.linked.lock() {
            linked.push((user_id.to_owned(), profile.jellyfin_user_id.clone()));
        }
    }
}

// --- Plex ---

/// Scripted [`PlexPinClient`].
#[derive(Debug, Clone)]
pub struct FakePlexPinClient {
    /// Stable install id.
    pub client_id: String,
    /// PIN outcomes by id: `None` pending, `Some` authorized.
    pub pins: Arc<Mutex<HashMap<i64, Option<String>>>>,
    /// Profiles by auth token.
    pub accounts: HashMap<String, PlexAccount>,
    /// Failing profile lookups (transport outage simulation).
    pub failing_accounts: Vec<String>,
    /// Configured server machine id; `None` means unconfigured.
    pub machine_id: Option<String>,
    /// Reachable servers by auth token.
    pub server_ids: HashMap<String, Vec<String>>,
    /// Server tokens by `(auth_token, machine_id)`.
    pub server_tokens: HashMap<(String, String), String>,
    /// Next PIN id.
    pub next_pin: Arc<Mutex<i64>>,
    /// Fail PIN creation when set.
    pub fail_create: bool,
    /// Call log.
    pub calls: Arc<Mutex<Vec<String>>>,
}

impl FakePlexPinClient {
    /// Fake with a fixed client id and no PINs.
    pub fn new() -> Self {
        Self {
            client_id: "test-client-id".to_owned(),
            pins: Arc::new(Mutex::new(HashMap::new())),
            accounts: HashMap::new(),
            failing_accounts: Vec::new(),
            machine_id: None,
            server_ids: HashMap::new(),
            server_tokens: HashMap::new(),
            next_pin: Arc::new(Mutex::new(1000)),
            fail_create: false,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Authorize a PIN id with a token.
    pub fn authorize_pin(&self, pin_id: i64, auth_token: &str) {
        if let Ok(mut pins) = self.pins.lock() {
            pins.insert(pin_id, Some(auth_token.to_owned()));
        }
    }

    fn log(&self, call: String) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }
}

impl Default for FakePlexPinClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PlexPinClient for FakePlexPinClient {
    fn client_id(&self) -> String {
        self.client_id.clone()
    }

    async fn create_pin(&self) -> Result<PlexPin, FederatedError> {
        self.log("create_pin".to_owned());
        if self.fail_create {
            return Err(FederatedError::ProviderUnavailable(
                "plex.tv down".to_owned(),
            ));
        }
        let mut next = locked(&self.next_pin)?;
        *next += 1;
        let id = *next;
        self.pins.lock().map(|mut pins| pins.insert(id, None)).ok();
        Ok(PlexPin {
            id,
            code: format!("code-{id}"),
        })
    }

    async fn poll_pin(&self, pin_id: i64) -> Result<Option<String>, FederatedError> {
        self.log(format!("poll_pin:{pin_id}"));
        Ok(locked(&self.pins)?.get(&pin_id).cloned().flatten())
    }

    async fn account_profile(&self, auth_token: &str) -> Result<PlexAccount, FederatedError> {
        self.log(format!("account_profile:{auth_token}"));
        if self
            .failing_accounts
            .iter()
            .any(|token| token == auth_token)
        {
            return Err(FederatedError::ProviderUnavailable(
                "plex.tv down".to_owned(),
            ));
        }
        self.accounts
            .get(auth_token)
            .cloned()
            .ok_or_else(|| FederatedError::ProviderUnavailable("unknown test token".to_owned()))
    }

    async fn server_machine_id(&self) -> Option<String> {
        self.log("server_machine_id".to_owned());
        self.machine_id.clone()
    }

    async fn account_server_ids(&self, auth_token: &str) -> Result<Vec<String>, FederatedError> {
        self.log(format!("account_server_ids:{auth_token}"));
        Ok(self.server_ids.get(auth_token).cloned().unwrap_or_default())
    }

    async fn server_access_token(
        &self,
        auth_token: &str,
        machine_id: &str,
    ) -> Result<Option<String>, FederatedError> {
        self.log(format!("server_access_token:{machine_id}"));
        Ok(self
            .server_tokens
            .get(&(auth_token.to_owned(), machine_id.to_owned()))
            .cloned())
    }
}

/// Recording [`PlexConnectionLink`].
#[derive(Debug, Clone, Default)]
pub struct FakePlexLink {
    /// Linked `(user_id, plex_uuid)` pairs in order.
    pub linked: Arc<Mutex<Vec<(String, String)>>>,
}

impl FakePlexLink {
    /// Empty link log.
    pub fn new() -> Self {
        Self::default()
    }
}

impl PlexConnectionLink for FakePlexLink {
    async fn link(&self, user_id: &str, profile: &PlexProfile) {
        if let Ok(mut linked) = self.linked.lock() {
            linked.push((user_id.to_owned(), profile.uuid.clone()));
        }
    }
}

// --- password hashing ---

/// Deterministic [`PasswordHasher`]: bcrypt fixtures look like
/// `$2b$12$fake:{password}`, Argon2id hashes like `$argon2id$fake:{password}`.
/// Flow-only; the shapes prove scheme dispatch, never real hashing.
#[derive(Debug, Clone, Default)]
pub struct FakeHasher {
    dummy_calls: Arc<Mutex<u64>>,
}

impl FakeHasher {
    /// Fresh fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bcrypt hash the fake accepts for `password`.
    pub fn bcrypt_fixture(password: &str) -> String {
        format!("$2b$12$fake:{password}")
    }

    /// How many dummy verifies ran.
    pub fn dummy_calls(&self) -> u64 {
        self.dummy_calls.lock().map(|count| *count).unwrap_or(0)
    }
}

impl PasswordHasher for FakeHasher {
    fn verify_bcrypt(&self, password: &str, hash: &str) -> bool {
        hash == Self::bcrypt_fixture(password)
    }

    fn verify_argon2id(&self, password: &str, hash: &str) -> bool {
        hash == self.hash_argon2id(password)
    }

    fn hash_argon2id(&self, password: &str) -> String {
        format!("$argon2id$fake:{password}")
    }

    fn dummy_verify(&self) {
        if let Ok(mut count) = self.dummy_calls.lock() {
            *count += 1;
        }
    }
}
