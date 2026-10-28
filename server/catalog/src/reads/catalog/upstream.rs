//! The upstream clients behind the catalog, built per call from live
//! settings.
//!
//! Clients are cheap (a reqwest handle plus pacing), so each call builds
//! the ones it needs from the settings as they are right now: a source
//! switch, a new AudioDB key or a Last.fm toggle takes effect on the next
//! request without a restart. Pacing and the answer cache come from the one
//! shared [`Providers`], so every client in the process respects the same
//! rate rows.

use std::sync::Arc;

use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::runtime_config::secret_sections::{
    AdvancedSettings, ListenBrainzConnection,
};
use droppedneedle_kernel::runtime_config::sections::{
    ListenSource, MusicBrainzSettings, PrimaryMusicSource, UserPreferences,
};
use droppedneedle_kernel::runtime_config::{ConfigStore, Listed, Live};
use droppedneedle_library::edition_prefs::LivePreferences;
use droppedneedle_providers::audiodb::AudioDbClient;
use droppedneedle_providers::itunes::ITunesClient;
use droppedneedle_providers::lastfm::{LastFmClient, LastFmCredentials};
use droppedneedle_providers::listenbrainz::{ListenBrainzClient, ListenBrainzCredentials};
use droppedneedle_providers::musicbrainz::{MbSource, MusicBrainzView};
use droppedneedle_providers::preview::PreviewClient;
use droppedneedle_providers::wikidata::WikidataClient;
use droppedneedle_providers::{ProviderCacheStore, ProviderError, Providers, RequestPriority};

/// The settings the catalog reads, once per call.
pub trait CatalogSettings: Send + Sync {
    /// MusicBrainz source selection; `None` while it does not read.
    fn musicbrainz(&self) -> Option<MusicBrainzSettings>;
    /// Release-type filters.
    fn preferences(&self) -> UserPreferences;
    /// Cache lifetimes and the AudioDB switch and key.
    fn advanced(&self) -> AdvancedSettings;
    /// Whether ListenBrainz reads are switched on.
    fn listenbrainz_enabled(&self) -> bool;
    /// Which provider answers discovery sections by default.
    fn primary_source(&self) -> ListenSource;
    /// Two-letter store region for purchase lookups; `None` while it does
    /// not read.
    fn store_region(&self) -> Option<String>;
    /// How to pick an edition of an album the library does not hold;
    /// `None` while the preferences do not read.
    fn edition_preferences(&self) -> Option<droppedneedle_library::edition_prefs::Preferences> {
        Some(droppedneedle_library::edition_prefs::Preferences::default())
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

/// The cache namespace while the MusicBrainz settings do not read.
pub const UNREADABLE_NAMESPACE: &str = "unreadable:g0";

/// The advanced section's defaults with AudioDB switched off, for when the
/// saved section does not read.
pub fn audiodb_off() -> AdvancedSettings {
    AdvancedSettings {
        audiodb_enabled: false,
        ..AdvancedSettings::default()
    }
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
    fn musicbrainz(&self) -> Option<MusicBrainzSettings> {
        self.musicbrainz.read().map(|settings| (**settings).clone())
    }

    fn preferences(&self) -> UserPreferences {
        or_default(&self.preferences)
    }

    /// Unreadable advanced settings keep the default tuning but never
    /// call AudioDB: its switch and key are in this section.
    fn advanced(&self) -> AdvancedSettings {
        self.advanced
            .read()
            .map_or_else(audiodb_off, |settings| (**settings).clone())
    }

    fn listenbrainz_enabled(&self) -> bool {
        or_default(&self.listenbrainz).enabled
    }

    fn primary_source(&self) -> ListenSource {
        or_default(&self.primary).source
    }

    fn store_region(&self) -> Option<String> {
        self.editions.store_region()
    }

    fn edition_preferences(&self) -> Option<droppedneedle_library::edition_prefs::Preferences> {
        self.editions.current()
    }
}

/// The MusicBrainz client type the catalog uses.
pub type CatalogMusicBrainz = MusicBrainzView;
/// The ListenBrainz client type the catalog uses.
pub type CatalogListenBrainz = ListenBrainzClient;
/// The Last.fm client type the catalog uses.
pub type CatalogLastFm = LastFmClient;

/// The AudioDB client with the key saved in Settings, read when the call
/// was set up. The key is never printed.
#[derive(Clone)]
pub struct CatalogAudioDb {
    client: AudioDbClient,
    api_key: String,
}

impl CatalogAudioDb {
    /// Look an artist up by MusicBrainz id.
    pub async fn artist_by_mbid(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<Option<droppedneedle_providers::audiodb::AudioDbArtist>, ProviderError> {
        self.client
            .artist_by_mbid(&self.api_key, mbid, priority)
            .await
    }

    /// Search for an artist by name.
    pub async fn search_artist(
        &self,
        name: &str,
        priority: RequestPriority,
    ) -> Result<Option<droppedneedle_providers::audiodb::AudioDbArtist>, ProviderError> {
        self.client
            .search_artist(&self.api_key, name, priority)
            .await
    }

    /// Look an album up by MusicBrainz id.
    pub async fn album_by_mbid(
        &self,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<Option<droppedneedle_providers::audiodb::AudioDbAlbum>, ProviderError> {
        self.client
            .album_by_mbid(&self.api_key, mbid, priority)
            .await
    }

    /// Search for an album by artist and title.
    pub async fn search_album(
        &self,
        artist: &str,
        album: &str,
        priority: RequestPriority,
    ) -> Result<Option<droppedneedle_providers::audiodb::AudioDbAlbum>, ProviderError> {
        self.client
            .search_album(&self.api_key, artist, album, priority)
            .await
    }
}

/// Everything needed to reach the catalog's upstreams.
#[derive(Clone)]
pub struct Upstream {
    providers: Arc<Providers>,
    settings: Arc<dyn CatalogSettings>,
    users: UsersDeps,
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
    /// Assemble from the shared provider deps.
    pub fn new(
        providers: Arc<Providers>,
        settings: Arc<dyn CatalogSettings>,
        users: UsersDeps,
    ) -> Self {
        Self {
            providers,
            settings,
            users,
        }
    }

    /// The live settings.
    pub fn settings(&self) -> &dyn CatalogSettings {
        self.settings.as_ref()
    }

    /// The shared provider answer cache.
    pub fn cache(&self) -> &ProviderCacheStore {
        &self.providers.cache
    }

    /// A MusicBrainz client for the configured source, plus the cache
    /// namespace for that source: switching sources (or bumping its
    /// generation) starts a fresh namespace, so one source's answers are
    /// never served as another's.
    pub fn musicbrainz(&self, priority: RequestPriority) -> (CatalogMusicBrainz, String) {
        // Settings that do not read get a source that sends nothing and a
        // fixed namespace that no real source shares.
        let musicbrainz = &self.providers.musicbrainz;
        let (source, namespace) = match self.settings.musicbrainz() {
            Some(settings) => (
                musicbrainz.source_from(&settings),
                format!("{}:g{}", settings.source_id, settings.generation),
            ),
            None => (MbSource::Unreadable, UNREADABLE_NAMESPACE.to_owned()),
        };
        (musicbrainz.fixed(source, priority), namespace)
    }

    /// The ListenBrainz client, or `None` when ListenBrainz is switched off.
    /// The reads used here are public and need no token.
    pub fn listenbrainz(&self) -> Option<(CatalogListenBrainz, ListenBrainzCredentials)> {
        self.settings.listenbrainz_enabled().then(|| {
            (
                self.providers.listenbrainz.clone(),
                ListenBrainzCredentials::default(),
            )
        })
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
        Some((
            self.providers.lastfm.clone(),
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

    /// The AudioDB client with the configured key, or `None` while
    /// AudioDB is switched off.
    pub fn audiodb(&self, advanced: &AdvancedSettings) -> Option<CatalogAudioDb> {
        advanced.audiodb_enabled.then(|| CatalogAudioDb {
            client: self.providers.audiodb.clone(),
            api_key: advanced.audiodb_api_key.expose().to_owned(),
        })
    }

    /// The Wikidata, Wikipedia and Commons client.
    pub fn wikidata(&self) -> &WikidataClient {
        &self.providers.wikidata
    }

    /// The Deezer client behind artist pictures.
    pub fn deezer(&self) -> &PreviewClient {
        &self.providers.previews
    }

    /// The iTunes Search client behind purchase links.
    pub fn itunes(&self) -> &ITunesClient {
        &self.providers.itunes
    }

    /// The users store (roles, Last.fm links).
    pub fn users(&self) -> &UsersDeps {
        &self.users
    }
}
