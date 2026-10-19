//! The upstream clients behind the catalog, built per call from live
//! settings.
//!
//! Clients are cheap (a reqwest handle plus pacing), so each call builds
//! the ones it needs from the settings as they are right now: a source
//! switch, a new AudioDB key or a Last.fm toggle takes effect on the next
//! request without a restart. Pacing and the byte cache come from the one
//! shared [`Providers`], so every client in the process respects the same
//! rate rows.

use std::sync::Arc;

use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::runtime_config::secret_sections::{
    AdvancedSettings, ListenBrainzConnection,
};
use droppedneedle_kernel::runtime_config::sections::{
    MusicBrainzSettings, MusicSource, PrimaryMusicSource, UserPreferences,
};
use droppedneedle_kernel::runtime_config::{ConfigStore, Listed, Live};
use droppedneedle_library::edition_prefs::LivePreferences;
use droppedneedle_providers::adapters::{CorePacer, HealthSink, ReqwestGet};
use droppedneedle_providers::audiodb::{self, AudioDbClient};
use droppedneedle_providers::lastfm::{self, LastFmClient, LastFmCredentials};
use droppedneedle_providers::listenbrainz::{self, ListenBrainzClient, ListenBrainzCredentials};
use droppedneedle_providers::musicbrainz::{
    MbPacing, MbSource, MusicBrainzClient, ReqwestMbTransport,
};
use droppedneedle_providers::{ProviderCache, Providers, RequestPriority};

/// The settings the catalog reads, once per call.
pub trait CatalogSettings: Send + Sync {
    /// MusicBrainz source selection.
    fn musicbrainz(&self) -> MusicBrainzSettings;
    /// Release-type filters.
    fn preferences(&self) -> UserPreferences;
    /// Cache lifetimes and the AudioDB switch and key.
    fn advanced(&self) -> AdvancedSettings;
    /// Whether ListenBrainz reads are switched on.
    fn listenbrainz_enabled(&self) -> bool;
    /// Which provider answers discovery sections by default.
    fn primary_source(&self) -> MusicSource;
    /// Two-letter store region for purchase lookups.
    fn store_region(&self) -> String;
    /// How to pick an edition of an album the library does not hold.
    fn edition_preferences(&self) -> droppedneedle_library::edition_prefs::Preferences {
        droppedneedle_library::edition_prefs::Preferences::default()
    }
}

/// Read one section now, falling back to its default with a log line: a
/// broken settings file must not take the artist pages down.
fn or_default<S: Listed + Default + Clone>(settings: &Live<S>) -> S {
    settings.get().map_or_else(
        |error| {
            tracing::warn!(section = S::KEY, %error, "settings read failed; catalog uses defaults");
            S::default()
        },
        |section| S::clone(&section),
    )
}

/// The catalog's settings, each a live handle read per call.
pub struct LiveCatalogSettings {
    musicbrainz: Live<MusicBrainzSettings>,
    preferences: Live<UserPreferences>,
    advanced: Live<AdvancedSettings>,
    listenbrainz: Live<ListenBrainzConnection>,
    primary: Live<PrimaryMusicSource>,
    editions: LivePreferences,
}

impl LiveCatalogSettings {
    /// Live handles on the catalog's sections in `config`.
    pub fn new(config: &ConfigStore) -> Self {
        Self {
            musicbrainz: config.live(),
            preferences: config.live(),
            advanced: config.live(),
            listenbrainz: config.live(),
            primary: config.live(),
            editions: LivePreferences::new(config),
        }
    }
}

impl CatalogSettings for LiveCatalogSettings {
    fn musicbrainz(&self) -> MusicBrainzSettings {
        or_default(&self.musicbrainz)
    }

    fn preferences(&self) -> UserPreferences {
        or_default(&self.preferences)
    }

    fn advanced(&self) -> AdvancedSettings {
        or_default(&self.advanced)
    }

    fn listenbrainz_enabled(&self) -> bool {
        or_default(&self.listenbrainz).enabled
    }

    fn primary_source(&self) -> MusicSource {
        or_default(&self.primary).source
    }

    fn store_region(&self) -> String {
        self.editions.store_region()
    }

    fn edition_preferences(&self) -> droppedneedle_library::edition_prefs::Preferences {
        self.editions.current()
    }
}

/// Base URLs of the upstreams whose address is fixed in code. A
/// MusicBrainz mirror follows its own settings section; tests point
/// everything here at a loopback fixture server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// ListenBrainz API root.
    pub listenbrainz: String,
    /// Last.fm API endpoint.
    pub lastfm: String,
    /// TheAudioDB API root.
    pub audiodb: String,
    /// Wikidata origin.
    pub wikidata: String,
    /// Wikipedia origin pattern with `{lang}`.
    pub wikipedia: String,
    /// Wikimedia Commons origin.
    pub commons: String,
    /// iTunes Search endpoint.
    pub itunes: String,
    /// Deezer API origin.
    pub deezer: String,
    /// Cover Art Archive origin.
    pub cover_art_archive: String,
    /// LRCLIB origin.
    pub lrclib: String,
    /// The releases list behind the version check.
    pub github_releases: String,
    /// YouTube Data API host.
    pub youtube: String,
    /// Official MusicBrainz API root.
    pub musicbrainz: String,
    /// BrainzMash API root.
    pub brainzmash: String,
}

/// The MusicBrainz client type the catalog uses.
pub type CatalogMusicBrainz = MusicBrainzClient<ReqwestMbTransport, HealthSink>;
/// The ListenBrainz client type the catalog uses.
pub type CatalogListenBrainz = ListenBrainzClient<CorePacer, HealthSink>;
/// The Last.fm client type the catalog uses.
pub type CatalogLastFm = LastFmClient<CorePacer, HealthSink>;
/// The AudioDB client type the catalog uses.
pub type CatalogAudioDb = AudioDbClient<CorePacer, HealthSink>;

/// Everything needed to reach the catalog's upstreams.
#[derive(Clone)]
pub struct Upstream {
    http: reqwest::Client,
    no_redirect: reqwest::Client,
    providers: Arc<Providers>,
    settings: Arc<dyn CatalogSettings>,
    users: UsersDeps,
    endpoints: Endpoints,
}

/// The API key the admin saved under Settings > Last.fm (the sealed
/// `lastfm_settings` pair), read on every call through the same switch the
/// scrobble forwarder uses, so a new key applies without a restart. Last.fm
/// reads use a user's own key first and fall back to this one; `None`
/// means no usable instance key is saved.
pub fn instance_lastfm_key(users: &UsersDeps) -> Option<String> {
    users
        .lastfm_switch
        .instance_keys()
        .map(|keys| keys.api_key)
        .filter(|key| !key.trim().is_empty())
}

/// A user's Last.fm link as the reads use it.
#[derive(Debug, Default)]
pub struct UserLastFm {
    /// The linked Last.fm username, when the user finished linking.
    pub username: Option<String>,
    /// The user's own API key, when they saved one that decrypts.
    pub api_key: Option<String>,
}

/// Read one user's Last.fm link. A store or decrypt failure logs and reads
/// as absent, so Last.fm reads fall back instead of failing the page.
pub async fn user_lastfm(users: &UsersDeps, user_id: &str) -> UserLastFm {
    let link = match users.lastfm.get(user_id).await {
        Ok(Some(link)) => link,
        Ok(None) => return UserLastFm::default(),
        Err(error) => {
            tracing::warn!(?error, "last.fm link read failed");
            return UserLastFm::default();
        }
    };
    let api_key = link
        .api_key_encrypted
        .and_then(|sealed| match users.crypto.decrypt(&sealed) {
            Ok(key) if !key.trim().is_empty() => Some(key),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "last.fm key does not decrypt");
                None
            }
        });
    UserLastFm {
        username: link.username.filter(|name| !name.trim().is_empty()),
        api_key,
    }
}

impl Upstream {
    /// Assemble from the shared HTTP clients and provider deps, reaching
    /// the upstreams at `endpoints`.
    pub fn new(
        http: &droppedneedle_kernel::http_client::HttpClientFactory,
        providers: Arc<Providers>,
        settings: Arc<dyn CatalogSettings>,
        users: UsersDeps,
        endpoints: Endpoints,
    ) -> Self {
        Self::from_clients(
            http.shared().clone(),
            http.no_redirect().clone(),
            providers,
            settings,
            users,
            endpoints,
        )
    }

    /// Assemble from the factory's two clients when only the clients are
    /// at hand (the discover wiring receives them, not the factory).
    pub fn from_clients(
        http: reqwest::Client,
        no_redirect: reqwest::Client,
        providers: Arc<Providers>,
        settings: Arc<dyn CatalogSettings>,
        users: UsersDeps,
        endpoints: Endpoints,
    ) -> Self {
        Self {
            http,
            no_redirect,
            providers,
            settings,
            users,
            endpoints,
        }
    }

    /// The live settings.
    pub fn settings(&self) -> &dyn CatalogSettings {
        self.settings.as_ref()
    }

    /// The shared byte cache.
    pub fn cache(&self) -> &dyn ProviderCache {
        self.providers.cache.as_ref()
    }

    /// An owned handle on the shared byte cache, for work that outlives
    /// the borrow (coalesced fetches).
    pub fn cache_handle(&self) -> Arc<dyn ProviderCache> {
        self.providers.cache.clone()
    }

    /// A MusicBrainz client for the configured source, plus the cache
    /// namespace for that source: switching sources (or bumping its
    /// generation) starts a fresh namespace, so one source's answers are
    /// never served as another's.
    pub fn musicbrainz(&self, priority: RequestPriority) -> (CatalogMusicBrainz, String) {
        let settings = self.settings.musicbrainz();
        let source = MbSource::from_settings(
            &settings,
            &self.endpoints.musicbrainz,
            &self.endpoints.brainzmash,
        );
        let namespace = format!("{}:g{}", settings.source_id, settings.generation);
        let client = MusicBrainzClient::new(
            ReqwestMbTransport::new(self.no_redirect.clone()),
            source,
            MbPacing::new(self.providers.clone()),
        )
        .with_sink(HealthSink::new(&self.providers))
        .with_priority(priority);
        (client, namespace)
    }

    /// The ListenBrainz client, or `None` when ListenBrainz is switched off.
    /// The reads used here are public and need no token.
    pub fn listenbrainz(&self) -> Option<(CatalogListenBrainz, ListenBrainzCredentials)> {
        if !self.settings.listenbrainz_enabled() {
            return None;
        }
        let pacer = CorePacer::for_source(self.providers.clone(), listenbrainz::SOURCE)?;
        Some((
            ListenBrainzClient::new(
                self.http.clone(),
                &self.endpoints.listenbrainz,
                pacer,
                HealthSink::new(&self.providers),
            ),
            ListenBrainzCredentials::default(),
        ))
    }

    /// The Last.fm client with this user's own API key, else the instance
    /// key (as the scrobble forwarder chooses), or `None` when Last.fm is
    /// switched off or neither key is usable.
    pub async fn lastfm(&self, user_id: &str) -> Option<(CatalogLastFm, LastFmCredentials)> {
        if !self.users.lastfm_switch.enabled() {
            return None;
        }
        let api_key = match self.user_lastfm_key(user_id).await {
            Some(key) => key,
            None => instance_lastfm_key(&self.users)?,
        };
        let pacer = CorePacer::for_source(self.providers.clone(), lastfm::SOURCE)?;
        Some((
            LastFmClient::new(
                self.http.clone(),
                &self.endpoints.lastfm,
                pacer,
                HealthSink::new(&self.providers),
            ),
            LastFmCredentials {
                api_key,
                ..LastFmCredentials::default()
            },
        ))
    }

    /// The user's own Last.fm API key, when they saved one that decrypts.
    async fn user_lastfm_key(&self, user_id: &str) -> Option<String> {
        user_lastfm(&self.users, user_id).await.api_key
    }

    /// The AudioDB client with the configured key and switch.
    pub fn audiodb(&self, advanced: &AdvancedSettings) -> Option<CatalogAudioDb> {
        if !advanced.audiodb_enabled {
            return None;
        }
        let pacer = CorePacer::for_source(self.providers.clone(), audiodb::SOURCE)?;
        Some(
            AudioDbClient::new(
                self.http.clone(),
                &self.endpoints.audiodb,
                pacer,
                HealthSink::new(&self.providers),
            )
            .with_api_key(advanced.audiodb_api_key.expose()),
        )
    }

    /// The plain GET transport behind the Wikidata and iTunes clients.
    pub fn http_get(&self) -> ReqwestGet {
        ReqwestGet::new(self.http.clone())
    }

    /// The users store (roles, Last.fm links).
    pub fn users(&self) -> &UsersDeps {
        &self.users
    }

    /// The configured endpoints.
    pub fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }
}
