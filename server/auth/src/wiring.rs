//! Production auth wiring: live sign-in clients, config bridges, one bundle.
//!
//! This module binds the live parts for serving traffic: the OIDC,
//! Jellyfin, plex.tv, Last.fm and breach-check clients over the shared HTTP
//! client, the live settings sections auth reads, and [`AuthSetup`], the
//! single bundle `create_app` builds its routers from. The sign-in media
//! links and the configured Plex server come in through the
//! [`MediaLinks`] and [`PlexServerIdentity`] ports.

use std::sync::Arc;

use super::federated::MediaLinks;
use super::federated::jellyfin_http::JellyfinHttp;
use super::federated::oidc::OidcLogin;
use super::federated::oidc_http::OidcHttp;
use super::federated::plex::PlexServerIdentity;
use super::federated::plex_http::PlexTv;
use super::federated::settings::SignInSettings;
use super::prod::ProdAuth;
use super::routes::federated::{JellyfinRouteState, OidcRouteState, PlexRouteState};
use super::routes::native::NativeAuthState;
use super::session::middleware::SessionAuth;
use super::session::rate_limit::RateLimiter;
use super::session::stated::StatedAddresses;
use super::users::UsersDeps;
use super::users::lastfm_http::LastFmAuthHttp;
use super::users::stores::LastFmSwitch;
use axum::extract::FromRef;
use droppedneedle_kernel::config::TrustedProxies;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::SecuritySettings;

/// Where the plex.tv and breach-check clients send their calls.
/// Production uses the real services; the integration tests point them at
/// local mocks. OIDC and Jellyfin need no entry: their URLs come from the
/// settings.
#[derive(Debug, Clone)]
pub struct Upstreams {
    /// plex.tv account API root.
    pub plex_tv: String,
    /// The breach check's range URL, up to the hash prefix.
    pub pwned_passwords: String,
}

/// Everything `create_app` needs to mount the auth routes and the session
/// gate, built once at boot.
#[derive(Clone)]
pub struct AuthSetup {
    /// Account, device, recovery, and app-password routes.
    pub users: UsersDeps,
    /// Session gate state for the middleware layer.
    pub session_auth: SessionAuth,
    /// Request limiter, mounted inside the session gate.
    pub limits: Arc<RateLimiter>,
    /// Login/logout/setup routes.
    pub native: NativeAuthState,
    /// OIDC routes.
    pub oidc: OidcRouteState,
    /// Jellyfin login route.
    pub jellyfin: JellyfinRouteState,
    /// Plex journey routes.
    pub plex: PlexRouteState,
    /// The sign-in sections, read by the provider list.
    pub sign_in: SignInSettings,
    /// The security section, read by the response security headers.
    pub security: Live<SecuritySettings>,
    /// Deployment base path (`""` at the domain root).
    pub base_path: String,
}

/// Warn at boot when OIDC is on with a plain-http issuer on a public
/// address: v2 accepted that, v3 refuses it at login.
fn warn_on_public_http_oidc(sign_in: &SignInSettings) {
    let config = sign_in.oidc_config();
    if config.enabled && super::federated::transport::looks_public_http(&config.issuer) {
        tracing::warn!(
            issuer = %config.issuer,
            concat!(
                "OIDC sign-in uses plain http on what looks like a public host; logins will be ",
                "refused unless it resolves to this machine or your local network. Switch the ",
                "provider to https.",
            )
        );
    }
}

impl AuthSetup {
    /// Build the bundle over `platform`: the sign-in clients use its
    /// shared HTTP client, OIDC its no-redirect one, and every forwarded
    /// header is read through its trusted proxies. `upstreams` gives the
    /// plex.tv and breach-check addresses; `lastfm` is the one Last.fm
    /// client, which account linking goes through; `plex_server` reads
    /// the configured Plex server's machine id for the membership gate;
    /// `links` saves the media link a Plex or Jellyfin sign-in hands back.
    pub fn build(
        platform: &Platform,
        auth: ProdAuth,
        upstreams: &Upstreams,
        lastfm: &droppedneedle_providers::lastfm::LastFmClient,
        plex_server: Arc<dyn PlexServerIdentity>,
        links: MediaLinks,
    ) -> Self {
        use super::users::hibp::{HibpScreen, PwnedPasswordsHttp};

        let config_store = &platform.settings;
        let base_path = platform.config.base_path.as_str();
        let (clock, ids, crypto) = (&platform.clock, &platform.ids, &platform.crypto);
        let sign_in = SignInSettings::new(config_store);
        warn_on_public_http_oidc(&sign_in);
        let no_redirect = platform.http.no_redirect().clone();
        let http = platform.http.shared().clone();
        let plex_tv = PlexTv::with_base(
            http.clone(),
            config_store.clone(),
            &upstreams.plex_tv,
            plex_server,
        );
        let jellyfin_http = JellyfinHttp::new(http.clone(), config_store.clone());
        let users = UsersDeps {
            users: auth.users.clone(),
            sessions: auth.sessions.clone(),
            app_passwords: auth.app_passwords.clone(),
            lastfm: auth.lastfm.clone(),
            recovery: auth.recovery.clone(),
            avatars: auth.avatars.clone(),
            passwords: auth.passwords.clone(),
            screen: HibpScreen::new(
                Arc::new(PwnedPasswordsHttp {
                    client: http.clone(),
                    range_url: upstreams.pwned_passwords.clone(),
                }),
                config_store.live(),
            ),
            clock: clock.clone(),
            ids: ids.clone(),
            crypto: crypto.clone(),
            lastfm_client: Arc::new(LastFmAuthHttp::new(lastfm.clone())),
            lastfm_switch: LastFmSwitch::new(config_store.live()),
            lastfm_pending: Arc::default(),
            jellyfin_directory: Arc::new(jellyfin_http.clone()),
            plex_directory: Arc::new(plex_tv.clone()),
        };
        let limits = Arc::new(RateLimiter::new());
        // One handle for the gate and every sign-in route: they read the
        // same saved list and share its one warning.
        let stated = StatedAddresses::new(config_store.live());
        let native = NativeAuthState::new(
            auth.sessions.clone(),
            auth.passwords.clone(),
            users.clone(),
            base_path,
        )
        .with_limits(limits.clone())
        .with_stated_addresses(stated.clone());
        let oidc = OidcRouteState::new(
            OidcLogin::new(
                auth.federated.clone(),
                OidcHttp::new(no_redirect),
                auth.oidc_states.clone(),
                auth.sessions.clone(),
                clock.clone(),
            ),
            sign_in.clone(),
            base_path,
        )
        .with_stated_addresses(stated.clone());
        let jellyfin = JellyfinRouteState::new(
            auth.federated.clone(),
            jellyfin_http,
            links.clone(),
            auth.sessions.clone(),
            base_path,
        )
        .with_limits(limits.clone())
        .with_stated_addresses(stated.clone());
        let plex = PlexRouteState::new(
            auth.federated.clone(),
            plex_tv,
            links,
            auth.sessions.clone(),
            base_path,
        )
        .with_stated_addresses(stated.clone());
        Self {
            users,
            session_auth: SessionAuth::new(auth.sessions.clone(), clock.clone(), base_path)
                .with_limits(limits.clone())
                .with_stated_addresses(stated),
            limits,
            native,
            oidc,
            jellyfin,
            plex,
            sign_in,
            security: config_store.live(),
            base_path: base_path.to_owned(),
        }
        .with_trusted_proxies(platform.config.trusted_proxies.clone())
    }

    /// Trust the given reverse proxies everywhere auth reads forwarded
    /// headers: the session gate's origin check, the login `Secure` cookie
    /// flag, and the client address the limiter keys on
    /// (`TRUSTED_PROXY_IPS`).
    #[must_use]
    fn with_trusted_proxies(mut self, trusted: TrustedProxies) -> Self {
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
        self.jellyfin = self
            .jellyfin
            .with_trusted_proxies(trusted.clone())
            .with_limits(limits.clone());
        self.plex = self.plex.with_trusted_proxies(trusted);
        self.limits = limits;
        self
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
}

/// Every auth route under `/api/v3`, in the group its handler's extractor
/// implies. The sign-in steps are public: they run before there is a
/// session. Linking a Plex account needs one, and connecting Plex for the
/// server settings needs an admin.
pub fn routes() -> Routes<AuthSetup> {
    use super::routes::federated::*;
    use super::routes::native::*;

    super::users::routes()
        .public(routes!(login_handler))
        .public(routes!(logout_handler))
        .public(routes!(setup_handler))
        .public(routes!(setup_status_handler))
        .public(routes!(providers_handler))
        .public(routes!(oidc_authorize_handler))
        .public(routes!(oidc_callback_handler))
        .public(routes!(oidc_exchange_handler))
        .public(routes!(jellyfin_login_handler))
        .public(routes!(plex_start_handler))
        .user(routes!(plex_start_link_handler))
        .admin(routes!(plex_start_connect_handler))
        .public(routes!(plex_poll_login_handler))
        .user(routes!(plex_poll_link_handler))
        .user(routes!(plex_poll_connect_handler))
}

impl FromRef<AuthSetup> for UsersDeps {
    fn from_ref(setup: &AuthSetup) -> Self {
        setup.users.clone()
    }
}

impl FromRef<AuthSetup> for NativeAuthState {
    fn from_ref(setup: &AuthSetup) -> Self {
        setup.native.clone()
    }
}

impl FromRef<AuthSetup> for SignInSettings {
    fn from_ref(setup: &AuthSetup) -> Self {
        setup.sign_in.clone()
    }
}

impl FromRef<AuthSetup> for OidcRouteState {
    fn from_ref(setup: &AuthSetup) -> Self {
        setup.oidc.clone()
    }
}

impl FromRef<AuthSetup> for JellyfinRouteState {
    fn from_ref(setup: &AuthSetup) -> Self {
        setup.jellyfin.clone()
    }
}

impl FromRef<AuthSetup> for PlexRouteState {
    fn from_ref(setup: &AuthSetup) -> Self {
        setup.plex.clone()
    }
}
