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

use crate::auth::users::UsersDeps;
use crate::auth::users::stores::LastFmSwitch;
use crate::providers::adapters::{CorePacer, CoreSink, ReqwestGet};
use crate::providers::audiodb::{self, AudioDbClient};
use crate::providers::lastfm::{self, LastFmClient, LastFmCredentials};
use crate::providers::listenbrainz::{self, ListenBrainzClient, ListenBrainzCredentials};
use crate::providers::musicbrainz::{MbPacing, MbSource, MusicBrainzClient, ReqwestMbTransport};
use crate::providers::{ProviderCache, Providers, RequestPriority};
use crate::runtime_config::ConfigStore;
use crate::runtime_config::secret_sections::{AdvancedSettings, ListenBrainzConnection};
use crate::runtime_config::sections::{
    GetIt, MusicBrainzSettings, MusicSource, PrimaryMusicSource, UserPreferences,
};

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
}

/// Read one section, falling back to its default with a log line: a
/// broken settings file must not take the artist pages down.
fn or_default<S: Default>(section: &'static str, read: Result<S, impl std::fmt::Display>) -> S {
    read.unwrap_or_else(|error| {
        tracing::warn!(section, %error, "settings read failed; catalog uses defaults");
        S::default()
    })
}

impl CatalogSettings for ConfigStore {
    fn musicbrainz(&self) -> MusicBrainzSettings {
        or_default("musicbrainz_settings", self.get::<MusicBrainzSettings>())
    }

    fn preferences(&self) -> UserPreferences {
        or_default("user_preferences", self.get::<UserPreferences>())
    }

    fn advanced(&self) -> AdvancedSettings {
        or_default("advanced_settings", self.get_raw::<AdvancedSettings>())
    }

    fn listenbrainz_enabled(&self) -> bool {
        or_default(
            "listenbrainz_settings",
            self.get_raw::<ListenBrainzConnection>(),
        )
        .enabled
    }

    fn primary_source(&self) -> MusicSource {
        or_default("primary_music_source", self.get::<PrimaryMusicSource>()).source
    }

    fn store_region(&self) -> String {
        or_default("get_it", self.get::<GetIt>()).store_region
    }
}

/// Base URLs for the non-MusicBrainz upstreams. MusicBrainz follows its
/// own settings section; tests point everything else at a loopback
/// fixture server.
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
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            lastfm: lastfm::DEFAULT_BASE_URL.to_owned(),
            audiodb: audiodb::DEFAULT_BASE_URL.to_owned(),
            wikidata: crate::providers::wikidata::WIKIDATA_BASE.to_owned(),
            wikipedia: crate::providers::wikidata::WIKIPEDIA_PATTERN.to_owned(),
            commons: crate::providers::wikidata::COMMONS_BASE.to_owned(),
            itunes: crate::providers::itunes::SEARCH_URL.to_owned(),
        }
    }
}

/// The MusicBrainz client type the catalog uses.
pub type CatalogMusicBrainz = MusicBrainzClient<ReqwestMbTransport, CoreSink>;
/// The ListenBrainz client type the catalog uses.
pub type CatalogListenBrainz = ListenBrainzClient<CorePacer, CoreSink>;
/// The Last.fm client type the catalog uses.
pub type CatalogLastFm = LastFmClient<CorePacer, CoreSink>;
/// The AudioDB client type the catalog uses.
pub type CatalogAudioDb = AudioDbClient<CorePacer, CoreSink>;

/// Everything needed to reach the catalog's upstreams.
#[derive(Clone)]
pub struct Upstream {
    http: reqwest::Client,
    no_redirect: reqwest::Client,
    providers: Arc<Providers>,
    settings: Arc<dyn CatalogSettings>,
    users: UsersDeps,
    endpoints: Endpoints,
    instance_lastfm_key: InstanceLastFmKey,
}

/// Reads the instance-wide Last.fm API key, per call. Last.fm reads use a
/// user's own key first and fall back to this one; `None` means no
/// instance key is saved. The default has none; boot plugs in
/// [`instance_lastfm_key`] through [`Upstream::with_instance_lastfm_key`].
pub type InstanceLastFmKey = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// The API key the admin saved under Settings > Last.fm (the sealed
/// `lastfm_settings` pair), read on every call through the same switch the
/// scrobble forwarder uses, so a new key applies without a restart.
pub fn instance_lastfm_key(switch: Arc<dyn LastFmSwitch>) -> InstanceLastFmKey {
    Arc::new(move || switch.instance_keys().map(|keys| keys.api_key))
}

impl Upstream {
    /// Assemble from the shared HTTP clients and provider deps.
    pub fn new(
        http: &crate::http_client::HttpClientFactory,
        providers: Arc<Providers>,
        settings: Arc<dyn CatalogSettings>,
        users: UsersDeps,
    ) -> Self {
        Self {
            http: http.shared().clone(),
            no_redirect: http.no_redirect().clone(),
            providers,
            settings,
            users,
            endpoints: Endpoints::default(),
            instance_lastfm_key: Arc::new(|| None),
        }
    }

    /// Fall back to this instance Last.fm key for users without their own.
    #[must_use]
    pub fn with_instance_lastfm_key(mut self, key: InstanceLastFmKey) -> Self {
        self.instance_lastfm_key = key;
        self
    }

    /// Point the non-MusicBrainz upstreams elsewhere (tests, mirrors).
    #[must_use]
    pub fn with_endpoints(mut self, endpoints: Endpoints) -> Self {
        self.endpoints = endpoints;
        self
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
        let source = MbSource::from_settings(&settings);
        let namespace = format!("{}:g{}", settings.source_id, settings.generation);
        let client = MusicBrainzClient::new(
            ReqwestMbTransport::new(self.no_redirect.clone()),
            source,
            MbPacing::new(self.providers.clone()),
        )
        .with_sink(CoreSink)
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
                CoreSink,
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
            None => (self.instance_lastfm_key)().filter(|key| !key.trim().is_empty())?,
        };
        let pacer = CorePacer::for_source(self.providers.clone(), lastfm::SOURCE)?;
        Some((
            LastFmClient::new(self.http.clone(), &self.endpoints.lastfm, pacer, CoreSink),
            LastFmCredentials {
                api_key,
                ..LastFmCredentials::default()
            },
        ))
    }

    /// The user's own Last.fm API key, when they saved one that decrypts.
    async fn user_lastfm_key(&self, user_id: &str) -> Option<String> {
        let link = match self.users.lastfm.get(user_id).await {
            Ok(link) => link?,
            Err(error) => {
                tracing::warn!(?error, "last.fm link read failed");
                return None;
            }
        };
        let sealed = link.api_key_encrypted?;
        match self.users.crypto.decrypt(&sealed) {
            Ok(key) if !key.trim().is_empty() => Some(key),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "last.fm key does not decrypt");
                None
            }
        }
    }

    /// The AudioDB client with the configured key and switch.
    pub fn audiodb(&self, advanced: &AdvancedSettings) -> Option<CatalogAudioDb> {
        if !advanced.audiodb_enabled {
            return None;
        }
        let pacer = CorePacer::for_source(self.providers.clone(), audiodb::SOURCE)?;
        Some(
            AudioDbClient::new(self.http.clone(), &self.endpoints.audiodb, pacer, CoreSink)
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
