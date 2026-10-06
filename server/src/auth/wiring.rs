//! Production auth wiring: live sign-in clients, config bridges, one bundle.
//!
//! The auth modules define ports and the adapters implement them. This
//! module binds the live ones for serving traffic: the OIDC, Jellyfin,
//! plex.tv and Last.fm clients over the shared HTTP client, the sign-in
//! links into the per-user media connections, bridges from the config
//! store to the live-read policy traits, and [`AuthSetup`], the single
//! bundle `create_app` builds its routers from.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use super::federated::FederatedError;
use super::federated::jellyfin_http::JellyfinHttp;
use super::federated::oidc::{EXCHANGE_TTL_SECS, OidcExchangeStore, OidcLogin};
use super::federated::oidc_http::OidcHttp;
use super::federated::plex_http::PlexTv;
use super::federated::settings::InstallIds;
use super::prod::{
    ProdAuth, SqliteFederatedStore, SqliteOidcStateStore, SqliteSessionIssuer, SqliteSessionStore,
};
use super::routes::federated::{
    JellyfinRouteState, OidcRouteState, PlexRouteState, StoreOidcConfig,
};
use super::routes::native::NativeAuthState;
use super::session::middleware::SessionAuth;
use super::session::middleware::TrustedProxies;
use super::session::rate_limit::RateLimiter;
use super::users::lastfm_http::LastFmAuthHttp;
use super::users::stores::{Clock, HibpPolicy, LastFmKeys, LastFmSwitch, SecurityPolicy};
use super::users::{UsersDeps, admin_router, public_router, users_router};
use crate::ids::IdGenerator;
use crate::remotes::connections::{
    ConfigServers, ConnectionResolver, ConnectionStore, CredentialCoder, SignInLinks,
    SqliteConnectionStore,
};
use crate::runtime_config::secret_sections::LastFmSettings;
use crate::runtime_config::sections::SecuritySettings;
use crate::runtime_config::{ConfigStore, Crypto};

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

    fn instance_keys(&self) -> Option<LastFmKeys> {
        let section = match self.store.get_raw::<LastFmSettings>() {
            Ok(section) => section,
            Err(error) => {
                tracing::warn!(%error, "cannot read the Last.fm app key pair");
                return None;
            }
        };
        let (api_key, shared_secret) = (section.api_key.expose(), section.shared_secret.expose());
        (!api_key.is_empty() && !shared_secret.is_empty()).then(|| LastFmKeys {
            api_key: api_key.to_owned(),
            shared_secret: shared_secret.to_owned(),
        })
    }
}

/// Where the plex.tv and Last.fm clients send their calls. Production uses
/// the real services; the integration tests point both at local mocks.
/// OIDC and Jellyfin need no entry: their URLs come from the settings.
#[derive(Debug, Clone)]
pub struct Upstreams {
    /// plex.tv account API root.
    pub plex_tv: String,
    /// Last.fm web service root.
    pub lastfm: String,
}

impl Default for Upstreams {
    fn default() -> Self {
        Self {
            plex_tv: crate::remotes::plex::PLEX_TV_BASE.to_owned(),
            lastfm: crate::providers::lastfm::DEFAULT_BASE_URL.to_owned(),
        }
    }
}

/// Production OIDC route state.
pub type ProdOidcRoutes = OidcRouteState<
    SqliteFederatedStore,
    OidcHttp,
    SqliteOidcStateStore,
    MemoryOidcExchangeStore,
    SqliteSessionIssuer,
    StoreOidcConfig,
>;
/// Production Jellyfin route state.
pub type ProdJellyfinRoutes =
    JellyfinRouteState<SqliteFederatedStore, JellyfinHttp, SignInLinks, SqliteSessionIssuer>;
/// Production Plex route state.
pub type ProdPlexRoutes =
    PlexRouteState<SqliteFederatedStore, PlexTv, SignInLinks, SqliteSessionIssuer>;

/// Everything `create_app` needs to mount `/api/v3`, built once at boot.
#[derive(Clone)]
pub struct AuthSetup {
    /// Account, device, recovery, and app-password routes.
    pub users: UsersDeps,
    /// Session gate state for the middleware layer.
    pub session_auth: SessionAuth<SqliteSessionStore>,
    /// Request limiter, mounted inside the session gate.
    pub limits: Arc<RateLimiter>,
    /// Login/logout/setup routes.
    pub native: NativeAuthState<
        SqliteSessionStore,
        super::prod::Argon2idHasher,
        super::prod::SqliteCredentialLookup,
    >,
    /// OIDC routes.
    pub oidc: ProdOidcRoutes,
    /// Jellyfin login route.
    pub jellyfin: ProdJellyfinRoutes,
    /// Plex journey routes.
    pub plex: ProdPlexRoutes,
    /// Live settings, read by the provider list.
    pub config_store: Arc<ConfigStore>,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
}

/// Saves the media link a Plex or Jellyfin sign-in hands back into the
/// per-user connections table (the same rows the remotes routes read).
fn sign_in_links(
    rows: Arc<dyn ConnectionStore>,
    crypto: Arc<Crypto>,
    config_store: Arc<ConfigStore>,
) -> SignInLinks {
    SignInLinks::new(Arc::new(ConnectionResolver::new(
        rows,
        Arc::new(CredentialCoder::new(crypto)),
        Arc::new(ConfigServers::new(config_store)),
    )))
}

impl AuthSetup {
    /// Build the production bundle. `http` is the shared outbound client
    /// every sign-in client uses; `ids` mints row ids; `clock` drives
    /// management-side expiry.
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
        Self::build_with_upstreams(
            auth,
            config_store,
            crypto,
            http,
            ids,
            clock,
            base_path,
            &Upstreams::default(),
        )
    }

    /// [`AuthSetup::build`] with the plex.tv and Last.fm roots given.
    #[allow(clippy::too_many_arguments)]
    pub fn build_with_upstreams(
        auth: ProdAuth,
        config_store: Arc<ConfigStore>,
        crypto: Arc<Crypto>,
        http: reqwest::Client,
        ids: Arc<dyn IdGenerator>,
        clock: Arc<dyn Clock>,
        base_path: &str,
        upstreams: &Upstreams,
    ) -> Result<Self, crate::runtime_config::ConfigError> {
        use super::users::hibp::{HibpScreen, PwnedPasswordsHttp};

        InstallIds::new(config_store.clone()).ensure();
        let plex_tv = PlexTv::with_base(http.clone(), config_store.clone(), &upstreams.plex_tv);
        let jellyfin_http = JellyfinHttp::new(http.clone(), config_store.clone());
        let users = UsersDeps {
            users: Arc::new(auth.users.clone()),
            sessions: Arc::new(auth.session_manager.clone()),
            app_passwords: Arc::new(auth.app_passwords.clone()),
            lastfm: Arc::new(auth.lastfm.clone()),
            recovery: Arc::new(auth.recovery.clone()),
            avatars: Arc::new(auth.avatars.clone()),
            passwords: Arc::new(auth.hasher.clone()),
            screen: Arc::new(HibpScreen::new(Arc::new(PwnedPasswordsHttp {
                client: http.clone(),
            }))),
            clock: clock.clone(),
            ids: ids.clone(),
            crypto: crypto.clone(),
            lastfm_client: Arc::new(LastFmAuthHttp::with_base(http.clone(), &upstreams.lastfm)),
            lastfm_switch: Arc::new(StoreLastFmSwitch::new(config_store.clone())),
            lastfm_pending: Arc::default(),
            security: Arc::new(StoreSecurityPolicy::new(config_store.clone())),
            jellyfin_directory: Arc::new(jellyfin_http.clone()),
            plex_directory: Arc::new(plex_tv.clone()),
        };
        let rows: Arc<dyn ConnectionStore> = match auth.db.handles() {
            Some((pool, lane)) => Arc::new(SqliteConnectionStore::new(pool, lane)),
            None => {
                return Err(crate::runtime_config::ConfigError::Validation {
                    section: "auth",
                    field: "database",
                    reason: "the production auth bundle needs a live database".to_owned(),
                });
            }
        };
        let links = sign_in_links(rows, crypto, config_store.clone());
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
                OidcHttp::new(http),
                auth.oidc_states.clone(),
                MemoryOidcExchangeStore::default(),
                auth.issuer.clone(),
            ),
            StoreOidcConfig(config_store.clone()),
            ids.clone(),
            base_path,
        );
        let jellyfin = JellyfinRouteState::new(
            auth.federated.clone(),
            jellyfin_http,
            links.clone(),
            auth.issuer.clone(),
            ids.clone(),
            base_path,
        );
        let plex = PlexRouteState::new(
            auth.federated.clone(),
            plex_tv,
            links,
            auth.issuer.clone(),
            ids,
            base_path,
        );
        Ok(Self {
            users,
            session_auth: SessionAuth::new(auth.sessions.clone(), base_path)
                .with_limits(limits.clone()),
            limits,
            native,
            oidc,
            jellyfin,
            plex,
            config_store,
            base_path: base_path.to_owned(),
        })
    }

    /// Trust the given reverse proxies everywhere auth reads forwarded
    /// headers: the session gate's origin check, the login `Secure` cookie
    /// flag, and the client address the limiter keys on
    /// (`TRUSTED_PROXY_IPS`).
    #[must_use]
    pub fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
        let limits = Arc::new(RateLimiter::new().with_trusted_proxies(trusted.clone()));
        self.session_auth = self
            .session_auth
            .with_trusted_proxies(trusted.clone())
            .with_limits(limits.clone());
        self.native = self
            .native
            .with_trusted_proxies(trusted.clone())
            .with_limits(limits.clone());
        self.oidc = self.oidc.with_trusted_proxies(trusted.clone());
        self.jellyfin = self.jellyfin.with_trusted_proxies(trusted.clone());
        self.plex = self.plex.with_trusted_proxies(trusted);
        self.limits = limits;
        self
    }

    /// Mount every auth router under `/api/v3`. Layers are applied by
    /// `create_app`, not here.
    pub fn router(&self) -> axum::Router {
        use super::routes::federated::{
            jellyfin_router, oidc_router, plex_router, providers_router,
        };
        use super::routes::native::native_auth_router;

        axum::Router::new()
            .merge(native_auth_router(self.native.clone()))
            .merge(users_router(self.users.clone()))
            .merge(admin_router(self.users.clone()))
            .merge(public_router(self.users.clone()))
            .merge(providers_router(self.config_store.clone()))
            .merge(oidc_router(self.oidc.clone()))
            .merge(jellyfin_router(self.jellyfin.clone()))
            .merge(plex_router(self.plex.clone()))
    }

    /// The OIDC callback at its v2 path. v2 registered
    /// `/api/v1/auth/oidc/callback` with identity providers and the import
    /// keeps that redirect URI, so serving it here keeps a migrated SSO
    /// login working without touching the provider. Mounted outside
    /// `/api/v3`; like every OIDC step it needs no session.
    pub fn legacy_oidc_router(&self) -> axum::Router {
        use super::routes::federated::oidc_callback_handler;

        axum::Router::new()
            .route(
                "/api/v1/auth/oidc/callback",
                axum::routing::get(oidc_callback_handler),
            )
            .with_state(self.oidc.clone())
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
            SqliteCredentialLookup, SqliteLastFmStore, SqliteRecoveryStore, SqliteSessionManager,
            SqliteUserStore,
        };
        use super::users::hibp::{HibpScreen, PwnedPasswordsHttp};
        use super::users::stores::SystemClock;
        use crate::ids::UuidGenerator;
        use crate::remotes::connections::MemoryConnectionStore;

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tag = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Never written: the store only touches disk on save, and nothing
        // here saves.
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
        let http = reqwest::Client::new();
        let db = AuthDb::unwired();
        let hasher = Argon2idHasher::new();
        let plex_tv = PlexTv::new(http.clone(), store.clone());
        let jellyfin_http = JellyfinHttp::new(http.clone(), store.clone());
        let users = UsersDeps {
            users: Arc::new(SqliteUserStore::new(&db)),
            sessions: Arc::new(SqliteSessionManager::new(&db, clock.clone())),
            app_passwords: Arc::new(SqliteAppPasswordStore::new(&db)),
            lastfm: Arc::new(SqliteLastFmStore::new(&db)),
            recovery: Arc::new(SqliteRecoveryStore::new(&db)),
            avatars: Arc::new(FileAvatarStore::unwired()),
            passwords: Arc::new(hasher.clone()),
            screen: Arc::new(HibpScreen::new(Arc::new(PwnedPasswordsHttp {
                client: http.clone(),
            }))),
            clock,
            ids: ids.clone(),
            crypto: crypto.clone(),
            lastfm_client: Arc::new(LastFmAuthHttp::new(http.clone())),
            lastfm_switch: Arc::new(StoreLastFmSwitch::new(store.clone())),
            lastfm_pending: Arc::default(),
            security: Arc::new(StoreSecurityPolicy::new(store.clone())),
            jellyfin_directory: Arc::new(jellyfin_http.clone()),
            plex_directory: Arc::new(plex_tv.clone()),
        };
        let links = sign_in_links(
            Arc::new(MemoryConnectionStore::new()),
            crypto.clone(),
            store.clone(),
        );
        let sessions = SqliteSessionStore::new(&db);
        let native = NativeAuthState::new(
            sessions.clone(),
            hasher,
            SqliteCredentialLookup::new(&db),
            users.clone(),
            "",
        );
        let federated = SqliteFederatedStore::new(&db, crypto, ids.clone());
        let issuer = SqliteSessionIssuer::new(&db, ids.clone());
        let oidc = OidcRouteState::new(
            OidcLogin::new(
                federated.clone(),
                OidcHttp::new(http),
                SqliteOidcStateStore::new(&db),
                MemoryOidcExchangeStore::default(),
                issuer.clone(),
            ),
            StoreOidcConfig(store.clone()),
            ids.clone(),
            "",
        );
        let jellyfin = JellyfinRouteState::new(
            federated.clone(),
            jellyfin_http,
            links.clone(),
            issuer.clone(),
            ids.clone(),
            "",
        );
        let plex = PlexRouteState::new(federated, plex_tv, links, issuer, ids, "");
        let limits = Arc::new(RateLimiter::new());
        Ok(Self {
            users,
            session_auth: SessionAuth::new(sessions, "").with_limits(limits.clone()),
            limits,
            native,
            oidc,
            jellyfin,
            plex,
            config_store: store,
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
