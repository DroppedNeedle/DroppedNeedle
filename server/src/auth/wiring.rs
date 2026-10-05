//! Production auth wiring: disabled providers, config bridges, one bundle.
//!
//! The auth modules define ports and the adapters implement the SQLite
//! ones. This module holds what is left for serving traffic: providers
//! with no live client yet (503s, never fakes), bridges from the config
//! store to the live-read policy traits, and [`AuthSetup`], the single
//! bundle `create_app` builds its routers from.
//!
//! Live IdP clients (OIDC discovery/token/userinfo, Jellyfin auth, Plex
//! PIN/account/resources, Last.fm web calls) are not written yet. Their
//! contracts are tested against fakes, and production reports an outage
//! until they exist: federated `NotConfigured` maps to 503 `UPSTREAM_ERROR`
//! (the federated contract's "unconfigured-or-provider-down" row), while
//! the users routes map Last.fm `Transport` faults to 502. Same fixed body
//! and error-id shape on both.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::federated::FederatedError;
use super::federated::jellyfin_login::{JellyfinIdp, JellyfinProfile, NoopJellyfinLink};
use super::federated::oidc::{
    DiscoveryDoc, EXCHANGE_TTL_SECS, OidcConfig, OidcExchangeStore, OidcIdp, OidcTokens, RawClaims,
    TokenRequest,
};
use super::federated::plex::{NoopPlexLink, PlexAccount, PlexPin, PlexPinClient};
use super::prod::ProdAuth;
use super::routes::federated::{
    JellyfinRouteState, OidcRouteState, PlexRouteState, StaticOidcConfig,
};
use super::routes::native::NativeAuthState;
use super::session::middleware::SessionAuth;
use super::session::middleware::TrustedProxies;
use super::session::rate_limit::RateLimiter;
use super::users::stores::{
    BoxFuture, Clock, HibpPolicy, LastFmAuthClient, LastFmError, LastFmSwitch, SecurityPolicy,
};
use super::users::{UsersDeps, admin_router, public_router, users_router};
use crate::ids::IdGenerator;
use crate::runtime_config::secret_sections::OidcConnection;
use crate::runtime_config::sections::{LastFmSettings, SecuritySettings};
use crate::runtime_config::{ConfigStore, Crypto};

/// OIDC IdP with no live client: every call reports unconfigured.
#[derive(Debug, Clone, Default)]
pub struct DisabledOidcIdp;

impl OidcIdp for DisabledOidcIdp {
    async fn discover(&self, _issuer: &str) -> Result<DiscoveryDoc, FederatedError> {
        Err(FederatedError::NotConfigured("oidc client".to_owned()))
    }

    async fn exchange_code(&self, _request: &TokenRequest) -> Result<OidcTokens, FederatedError> {
        Err(FederatedError::NotConfigured("oidc client".to_owned()))
    }

    async fn fetch_claims(
        &self,
        _userinfo_endpoint: Option<&str>,
        _access_token: &str,
        _id_token: &str,
    ) -> Result<RawClaims, FederatedError> {
        Err(FederatedError::NotConfigured("oidc client".to_owned()))
    }
}

/// Jellyfin IdP with no live client. `is_configured` is false so the login
/// service short-circuits before any network use.
#[derive(Debug, Clone, Default)]
pub struct DisabledJellyfinIdp;

impl JellyfinIdp for DisabledJellyfinIdp {
    fn is_configured(&self) -> bool {
        false
    }

    async fn authenticate_by_name(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<JellyfinProfile, FederatedError> {
        Err(FederatedError::NotConfigured("jellyfin client".to_owned()))
    }
}

/// Plex PIN client with no live client. `server_machine_id` is None, which
/// v2 treats as "disabled or unreachable" without failing.
#[derive(Debug, Clone, Default)]
pub struct DisabledPlexPinClient;

impl PlexPinClient for DisabledPlexPinClient {
    fn client_id(&self) -> String {
        String::new()
    }

    async fn create_pin(&self) -> Result<PlexPin, FederatedError> {
        Err(FederatedError::NotConfigured("plex client".to_owned()))
    }

    async fn poll_pin(&self, _pin_id: i64) -> Result<Option<String>, FederatedError> {
        Err(FederatedError::NotConfigured("plex client".to_owned()))
    }

    async fn account_profile(&self, _auth_token: &str) -> Result<PlexAccount, FederatedError> {
        Err(FederatedError::NotConfigured("plex client".to_owned()))
    }

    async fn server_machine_id(&self) -> Option<String> {
        None
    }

    async fn account_server_ids(&self, _auth_token: &str) -> Result<Vec<String>, FederatedError> {
        Err(FederatedError::NotConfigured("plex client".to_owned()))
    }

    async fn server_access_token(
        &self,
        _auth_token: &str,
        _machine_id: &str,
    ) -> Result<Option<String>, FederatedError> {
        Err(FederatedError::NotConfigured("plex client".to_owned()))
    }
}

/// Last.fm web client with no live client: every call reports an outage.
#[derive(Debug, Clone, Default)]
pub struct DisabledLastFmAuthClient;

impl LastFmAuthClient for DisabledLastFmAuthClient {
    fn request_token<'a>(
        &'a self,
        _api_key: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>> {
        Box::pin(async { Err(LastFmError::Transport) })
    }

    fn exchange_session<'a>(
        &'a self,
        _api_key: &'a str,
        _shared_secret: &'a str,
        _token: &'a str,
    ) -> BoxFuture<'a, Result<(String, String), LastFmError>> {
        Box::pin(async { Err(LastFmError::Transport) })
    }
}

/// One stored exchange code: owning user, sealed token, expiry.
type StoredExchange = (String, String, SystemTime);

/// Process-local single-use OIDC exchange codes. There is no baseline table
/// for these 60-second bridge codes: a restart drops in-flight logins and
/// the SPA restarts the flow cleanly, so a mutex map is enough.
///
/// Inserts past [`EXCHANGE_PURGE_THRESHOLD`] entries sweep expired codes so
/// abandoned logins cannot grow the map without bound.
#[derive(Debug, Clone, Default)]
pub struct MemoryOidcExchangeStore {
    codes: Arc<Mutex<HashMap<String, StoredExchange>>>,
}

/// Insert count past which storing a code also sweeps expired ones.
const EXCHANGE_PURGE_THRESHOLD: usize = 1024;

impl OidcExchangeStore for MemoryOidcExchangeStore {
    async fn store_exchange(
        &self,
        code: &str,
        user_id: &str,
        raw_token: &str,
    ) -> Result<(), FederatedError> {
        let expires = SystemTime::now() + Duration::from_secs(EXCHANGE_TTL_SECS);
        let mut codes = self
            .codes
            .lock()
            .map_err(|_| FederatedError::StoreUnavailable("exchange store lock".to_owned()))?;
        codes.insert(
            code.to_owned(),
            (user_id.to_owned(), raw_token.to_owned(), expires),
        );
        if codes.len() > EXCHANGE_PURGE_THRESHOLD {
            let now = SystemTime::now();
            codes.retain(|_, (_, _, expires)| *expires > now);
        }
        Ok(())
    }

    async fn take_exchange(&self, code: &str) -> Result<Option<(String, String)>, FederatedError> {
        let mut codes = self
            .codes
            .lock()
            .map_err(|_| FederatedError::StoreUnavailable("exchange store lock".to_owned()))?;
        let Some((user_id, raw_token, expires)) = codes.remove(code) else {
            return Ok(None);
        };
        if SystemTime::now() > expires {
            return Ok(None);
        }
        Ok(Some((user_id, raw_token)))
    }
}

/// Live-read HIBP knobs from the config store. An unreadable section
/// disables screening (advisory only; auth itself never depends on it).
#[derive(Debug, Clone)]
pub struct StoreSecurityPolicy {
    store: Arc<ConfigStore>,
}

impl StoreSecurityPolicy {
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self { store }
    }
}

impl SecurityPolicy for StoreSecurityPolicy {
    fn hibp(&self) -> HibpPolicy {
        self.store
            .get::<SecuritySettings>()
            .map(|section| HibpPolicy {
                check: section.hibp_check,
                local_path: section.hibp_local_path,
            })
            .unwrap_or(HibpPolicy {
                check: false,
                local_path: String::new(),
            })
    }
}

/// Live-read Last.fm master switch from the config store.
#[derive(Debug, Clone)]
pub struct StoreLastFmSwitch {
    store: Arc<ConfigStore>,
}

impl StoreLastFmSwitch {
    pub fn new(store: Arc<ConfigStore>) -> Self {
        Self { store }
    }
}

impl LastFmSwitch for StoreLastFmSwitch {
    fn enabled(&self) -> bool {
        // Fail closed: an unreadable section disables fan-out. Last.fm is
        // optional enrichment, never auth-critical, so a broken read must
        // degrade to off rather than guess at enabled.
        self.store
            .get::<LastFmSettings>()
            .map(|section| section.enabled)
            .unwrap_or(false)
    }
}

/// Snapshot the OIDC connection into route config. The secret is decrypted
/// here, once, and never logged. The snapshot is taken at boot; re-reading
/// it per request is not wired yet.
pub fn oidc_route_config(
    store: &ConfigStore,
) -> Result<StaticOidcConfig, crate::runtime_config::ConfigError> {
    let connection = store.get_raw::<OidcConnection>()?;
    let secret = connection.client_secret.expose();
    Ok(StaticOidcConfig(OidcConfig {
        enabled: connection.enabled,
        issuer: connection.issuer,
        client_id: connection.client_id,
        client_secret: (!secret.is_empty()).then(|| secret.to_owned()),
        redirect_uri: connection.redirect_uri,
        scopes: connection.scopes,
    }))
}

/// Everything `create_app` needs to mount `/api/v3`, built once at boot.
#[derive(Clone)]
pub struct AuthSetup {
    /// Account, device, recovery, and app-password routes.
    pub users: UsersDeps,
    /// Session gate state for the middleware layer.
    pub session_auth: SessionAuth<super::prod::SqliteSessionStore>,
    /// Request limiter, mounted inside the session gate.
    pub limits: Arc<RateLimiter>,
    /// Login/logout/setup routes.
    pub native: NativeAuthState<
        super::prod::SqliteSessionStore,
        super::prod::Argon2idHasher,
        super::prod::SqliteCredentialLookup,
    >,
    /// OIDC routes.
    pub oidc: OidcRouteState<
        super::prod::SqliteFederatedStore,
        DisabledOidcIdp,
        super::prod::SqliteOidcStateStore,
        MemoryOidcExchangeStore,
        super::prod::SqliteSessionIssuer,
        StaticOidcConfig,
    >,
    /// Jellyfin login route.
    pub jellyfin: JellyfinRouteState<
        super::prod::SqliteFederatedStore,
        DisabledJellyfinIdp,
        NoopJellyfinLink,
        super::prod::SqliteSessionIssuer,
    >,
    /// Plex journey routes.
    pub plex: PlexRouteState<
        super::prod::SqliteFederatedStore,
        DisabledPlexPinClient,
        NoopPlexLink,
        super::prod::SqliteSessionIssuer,
    >,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
}

impl AuthSetup {
    /// Build the production bundle. `http` feeds the HIBP range client;
    /// `ids` mints row ids; `clock` drives management-side expiry.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        auth: ProdAuth,
        config_store: Arc<ConfigStore>,
        crypto: Arc<Crypto>,
        http: reqwest::Client,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
        base_path: &str,
    ) -> Result<Self, crate::runtime_config::ConfigError> {
        use super::federated::oidc::OidcLogin;
        use super::users::hibp::{HibpScreen, PwnedPasswordsHttp};

        let users = UsersDeps {
            users: Arc::new(auth.users.clone()),
            sessions: Arc::new(auth.session_manager.clone()),
            app_passwords: Arc::new(auth.app_passwords.clone()),
            lastfm: Arc::new(auth.lastfm.clone()),
            recovery: Arc::new(auth.recovery.clone()),
            avatars: Arc::new(auth.avatars.clone()),
            passwords: Arc::new(auth.hasher.clone()),
            screen: Arc::new(HibpScreen::new(Arc::new(PwnedPasswordsHttp {
                client: http,
            }))),
            clock: clock.clone(),
            ids: ids.clone(),
            crypto,
            lastfm_client: Arc::new(DisabledLastFmAuthClient),
            lastfm_switch: Arc::new(StoreLastFmSwitch::new(config_store.clone())),
            security: Arc::new(StoreSecurityPolicy::new(config_store.clone())),
        };
        let limits = Arc::new(RateLimiter::new());
        let native = NativeAuthState::new(
            auth.sessions.clone(),
            auth.hasher.clone(),
            auth.credentials.clone(),
            users.clone(),
            base_path,
        )
        .with_limits(limits.clone());
        let oidc = OidcRouteState::new(
            OidcLogin::new(
                auth.federated.clone(),
                DisabledOidcIdp,
                auth.oidc_states.clone(),
                MemoryOidcExchangeStore::default(),
                auth.issuer.clone(),
            ),
            oidc_route_config(&config_store)?,
            ids.clone(),
            base_path,
        );
        let jellyfin = JellyfinRouteState::new(
            auth.federated.clone(),
            DisabledJellyfinIdp,
            NoopJellyfinLink,
            auth.issuer.clone(),
            ids.clone(),
            base_path,
        );
        let plex = PlexRouteState::new(
            auth.federated.clone(),
            DisabledPlexPinClient,
            NoopPlexLink,
            auth.issuer.clone(),
            ids,
            base_path,
        );
        Ok(Self {
            users,
            session_auth: SessionAuth::new(auth.sessions.clone(), base_path),
            limits,
            native,
            oidc,
            jellyfin,
            plex,
            base_path: base_path.to_owned(),
        })
    }

    /// Trust the given reverse proxies everywhere auth reads forwarded
    /// headers: the session gate's origin check, the login `Secure` cookie
    /// flag, and the client address the limiter keys on
    /// (`TRUSTED_PROXY_IPS`).
    #[must_use]
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        self.session_auth = self.session_auth.with_trusted_proxies(trusted.clone());
        let limits = Arc::new(RateLimiter::new().with_trusted_proxies(trusted.clone()));
        self.native = self
            .native
            .with_trusted_proxies(trusted.clone())
            .with_limits(limits.clone());
        self.oidc = self.oidc.with_trusted_proxies(trusted);
        self.limits = limits;
        self
    }

    /// Mount every auth router under `/api/v3`. Layers are applied by
    /// `create_app`, not here.
    pub fn router(&self) -> axum::Router {
        use super::routes::federated::{jellyfin_router, oidc_router, plex_router};
        use super::routes::native::native_auth_router;

        axum::Router::new()
            .merge(native_auth_router(self.native.clone()))
            .merge(users_router(self.users.clone()))
            .merge(admin_router(self.users.clone()))
            .merge(public_router(self.users.clone()))
            .merge(oidc_router(self.oidc.clone()))
            .merge(jellyfin_router(self.jellyfin.clone()))
            .merge(plex_router(self.plex.clone()))
    }

    /// Test bundle over unwired adapters. Nothing here touches a database:
    /// every SQLite adapter fails closed until wired, which is exactly what
    /// non-auth tests need (the middleware passes non-v3 paths through;
    /// credential-less v3 requests 401 at the gate while any credentialed
    /// v3 request 500s on the unwired store lookup). Auth behavior tests
    /// build the real bundle instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests() -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};

        use super::prod::{
            Argon2idHasher, AuthDb, FileAvatarStore, SqliteAppPasswordStore,
            SqliteCredentialLookup, SqliteFederatedStore, SqliteLastFmStore, SqliteOidcStateStore,
            SqliteRecoveryStore, SqliteSessionIssuer, SqliteSessionManager, SqliteSessionStore,
            SqliteUserStore,
        };
        use super::users::hibp::{HibpScreen, PwnedPasswordsHttp};
        use super::users::stores::SystemClock;
        use crate::ids::UuidGenerator;

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tag = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droppedneedle-auth-test-{}-{tag}",
            std::process::id()
        ));
        let test_key =
            || Crypto::from_key_bytes(&[7u8; 32]).map_err(|error| format!("test crypto: {error}"));
        let crypto = Arc::new(test_key()?);
        let ids = Arc::new(UuidGenerator);
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let store = ConfigStore::open(&dir.join("config.json"), test_key()?)
            .map_err(|error| format!("test config: {error}"))?;
        let store = Arc::new(store);
        let db = AuthDb::unwired();
        let hasher = Argon2idHasher::new();
        let users = UsersDeps {
            users: Arc::new(SqliteUserStore::new(&db)),
            sessions: Arc::new(SqliteSessionManager::new(&db, clock.clone())),
            app_passwords: Arc::new(SqliteAppPasswordStore::new(&db)),
            lastfm: Arc::new(SqliteLastFmStore::new(&db)),
            recovery: Arc::new(SqliteRecoveryStore::new(&db)),
            avatars: Arc::new(FileAvatarStore::unwired()),
            passwords: Arc::new(hasher.clone()),
            screen: Arc::new(HibpScreen::new(Arc::new(PwnedPasswordsHttp {
                client: reqwest::Client::new(),
            }))),
            clock,
            ids: ids.clone(),
            crypto: crypto.clone(),
            lastfm_client: Arc::new(DisabledLastFmAuthClient),
            lastfm_switch: Arc::new(StoreLastFmSwitch::new(store.clone())),
            security: Arc::new(StoreSecurityPolicy::new(store.clone())),
        };
        let sessions = SqliteSessionStore::new(&db);
        let native = NativeAuthState::new(
            sessions.clone(),
            hasher,
            SqliteCredentialLookup::new(&db),
            users.clone(),
            "",
        );
        let federated = SqliteFederatedStore::new(&db, crypto.clone(), ids.clone());
        let issuer = SqliteSessionIssuer::new(&db, ids.clone());
        let oidc = OidcRouteState::new(
            super::federated::oidc::OidcLogin::new(
                federated.clone(),
                DisabledOidcIdp,
                SqliteOidcStateStore::new(&db),
                MemoryOidcExchangeStore::default(),
                issuer.clone(),
            ),
            oidc_route_config(&store).map_err(|error| format!("test oidc: {error}"))?,
            ids.clone(),
            "",
        );
        let jellyfin = JellyfinRouteState::new(
            federated.clone(),
            DisabledJellyfinIdp,
            NoopJellyfinLink,
            issuer.clone(),
            ids.clone(),
            "",
        );
        let plex = PlexRouteState::new(
            federated,
            DisabledPlexPinClient,
            NoopPlexLink,
            issuer,
            ids,
            "",
        );
        Ok(Self {
            users,
            session_auth: SessionAuth::new(sessions, ""),
            limits: Arc::new(RateLimiter::new()),
            native,
            oidc,
            jellyfin,
            plex,
            base_path: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exchange_inserts_past_threshold_keep_every_code() {
        let store = MemoryOidcExchangeStore::default();
        for n in 0..EXCHANGE_PURGE_THRESHOLD + 50 {
            store
                .store_exchange(&format!("code-{n}"), "user-1", "raw")
                .await
                .unwrap();
        }
        for n in 0..EXCHANGE_PURGE_THRESHOLD + 50 {
            let taken = store.take_exchange(&format!("code-{n}")).await.unwrap();
            assert_eq!(taken, Some(("user-1".to_owned(), "raw".to_owned())));
        }
    }
}
