//! Artist and album pages: the native `/api/v3/artists/{artist_mbid}` and
//! `/api/v3/albums/{album_id}` read surface, plus the MusicBrainz half of
//! unified search.
//!
//! Pages are built from MusicBrainz (identity, discography, tracklists),
//! decorated by optional sources (Wikidata/Wikipedia biography, TheAudioDB
//! images, ListenBrainz and Last.fm listening data, iTunes store links),
//! and flagged against the local library (in library, requested, owned
//! edition). Optional sources degrade into `service_status`; a dead
//! MusicBrainz falls back to the library's own copy when there is one.
//!
//! Every upstream answer goes through the shared provider cache with v2's
//! lifetimes (read from settings per call) and is coalesced so identical
//! concurrent page loads cost one upstream call. Library flags are read
//! fresh on every request, so they never go stale behind a cache.
//!
//! Auth is the deny-by-default session gate: every signed-in role sees the
//! same pages, and per-user data (Last.fm) is keyed by the session user.

pub mod album;
pub mod artist;
pub mod discovery;
pub mod error;
pub mod handlers;
pub mod library;
pub mod mapping;
pub mod models;
pub mod plugin_sources;
pub mod ports;
pub mod precache;
pub mod search;
pub mod upstream;

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_providers::Singleflight;
use droppedneedle_providers::musicbrainz::MbError;

use self::error::CatalogError;
use self::library::LocalCatalog;
use self::upstream::Upstream;

/// Negative-cache lifetime for MusicBrainz misses (v2
/// `_ARTIST_NOT_FOUND_TTL_SECONDS`), short so a fresh MusicBrainz edit
/// shows up soon.
pub const MISS_TTL: Duration = Duration::from_secs(600);

/// The catalog service. Clone shares everything.
#[derive(Clone)]
pub struct Catalog {
    inner: Arc<Inner>,
    follows: Arc<dyn ports::FollowLookup>,
    purchase_links: Arc<dyn ports::PurchaseLinkSource>,
    details: Arc<dyn ports::DetailsSource>,
}

/// How long plugin details are kept.
const PLUGIN_DETAILS_TTL: Duration = Duration::from_secs(60 * 60);

struct Inner {
    upstream: Upstream,
    local: LocalCatalog,
    flights: Flights,
    /// Artists whose discography is being fetched in the background.
    warming: Mutex<HashSet<String>>,
}

/// One coalescing table per cached shape.
#[derive(Default)]
struct Flights {
    artist: Singleflight<artist::ArtistCore, CatalogError>,
    release_groups: Singleflight<artist::ReleaseGroupList, CatalogError>,
    group: Singleflight<album::GroupCore, CatalogError>,
    release: Singleflight<album::ReleaseCore, CatalogError>,
    extended: Singleflight<artist::Biography, CatalogError>,
    search: Singleflight<search::RemoteHits, CatalogError>,
    other: Singleflight<serde_json::Value, CatalogError>,
}

impl Catalog {
    /// Serve pages from these upstreams and this library.
    pub fn new(upstream: Upstream, local: LocalCatalog) -> Self {
        Self {
            inner: Arc::new(Inner {
                upstream,
                local,
                flights: Flights::default(),
                warming: Mutex::new(HashSet::new()),
            }),
            follows: Arc::new(ports::NoFollows),
            purchase_links: Arc::new(ports::NoPurchaseLinks),
            details: Arc::new(ports::NoDetails),
        }
    }

    /// Add extra purchase links (the plugins' `purchase_links`) from this
    /// source.
    #[must_use]
    pub fn with_purchase_links(mut self, source: Arc<dyn ports::PurchaseLinkSource>) -> Self {
        self.purchase_links = source;
        self
    }

    /// Fill gaps in artist and album pages (the plugins'
    /// `metadata_provider`) from this source.
    #[must_use]
    pub fn with_details(mut self, source: Arc<dyn ports::DetailsSource>) -> Self {
        self.details = source;
        self
    }

    /// Read follow state for the artist header from this store.
    #[must_use]
    pub fn with_follows(mut self, follows: Arc<dyn ports::FollowLookup>) -> Self {
        self.follows = follows;
        self
    }

    fn follows(&self) -> &dyn ports::FollowLookup {
        self.follows.as_ref()
    }

    pub(crate) fn upstream(&self) -> &Upstream {
        &self.inner.upstream
    }

    fn local(&self) -> &LocalCatalog {
        &self.inner.local
    }

    /// Cache-aside plus coalescing for one upstream-backed value. A hit
    /// returns the cached copy; a miss runs `fetch` once for every
    /// concurrent caller of the same key and stores the result for the
    /// lifetime `fetch` chose (`None` stores nothing: degraded answers are
    /// never cached).
    async fn cached<T, F, Fut>(
        &self,
        flights: &Singleflight<T, CatalogError>,
        key: String,
        fetch: F,
    ) -> Result<T, CatalogError>
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(T, Option<Duration>), CatalogError>> + Send + 'static,
    {
        if let Some(hit) = self.upstream().cache().get_bytes(&key).await
            && let Ok(value) = serde_json::from_slice::<T>(&hit)
        {
            return Ok(value);
        }
        let cache = self.upstream().cache_handle();
        let store_key = key.clone();
        flights
            .run(&key, move || async move {
                let (value, ttl) = fetch().await?;
                if let Some(ttl) = ttl.filter(|ttl| !ttl.is_zero())
                    && let Ok(bytes) = serde_json::to_vec(&value)
                {
                    cache.set_bytes(&store_key, bytes, ttl).await;
                }
                Ok(value)
            })
            .await
            .map(|value| (*value).clone())
            .map_err(|error| (*error).clone())
    }

    /// Plugin details for one artist or album, cached for an hour per
    /// set of answering plugins. No plugin means no call and no cache.
    async fn plugin_details<F, Fut>(
        &self,
        kind: &str,
        mbid: &str,
        fetch: F,
    ) -> Result<models::PluginDetails, CatalogError>
    where
        F: FnOnce(Catalog) -> Fut + Send + 'static,
        Fut: Future<Output = models::PluginDetails> + Send + 'static,
    {
        let token = self.details.token();
        if token.is_empty() {
            return Ok(models::PluginDetails::default());
        }
        let key =
            droppedneedle_providers::digest_key("plugin_details:", &[kind, mbid, &token], "v1");
        let catalog = self.clone();
        let value = self
            .cached(&self.inner.flights.other, key, move || async move {
                let details = fetch(catalog).await;
                let value = serde_json::to_value(&details).map_err(|error| {
                    CatalogError::Internal(format!("plugin details encode: {error}"))
                })?;
                Ok((value, Some(PLUGIN_DETAILS_TTL)))
            })
            .await?;
        Ok(serde_json::from_value(value).unwrap_or_default())
    }

    /// Drop cached entries by exact key.
    async fn forget(&self, keys: &[String]) {
        for key in keys {
            self.upstream().cache().delete(key).await;
        }
    }

    /// Owned and requested flags for a set of album MBIDs. A failed read
    /// logs and reads as "not owned, not requested": flags decorate a page,
    /// they never fail it.
    pub(crate) async fn album_flags(&self, mbids: &[String]) -> (HashSet<String>, HashSet<String>) {
        if mbids.is_empty() {
            return (HashSet::new(), HashSet::new());
        }
        let (owned, requested) = tokio::join!(
            self.local().owned_albums(mbids),
            self.local().requested_albums(mbids)
        );
        let owned = owned.unwrap_or_else(|error| {
            tracing::warn!(%error, "library album flags failed; showing none");
            HashSet::new()
        });
        let requested = requested.unwrap_or_else(|error| {
            tracing::warn!(%error, "request flags failed; showing none");
            HashSet::new()
        });
        (owned, requested)
    }

    /// Owned flags for a set of artist MBIDs, failing soft like
    /// [`Self::album_flags`].
    pub(crate) async fn artist_flags(&self, mbids: &[String]) -> HashSet<String> {
        if mbids.is_empty() {
            return HashSet::new();
        }
        self.local()
            .owned_artists(mbids)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "library artist flags failed; showing none");
                HashSet::new()
            })
    }
}

/// Map a MusicBrainz failure onto the catalog error. A rejected MBID is the
/// caller's fault; everything else means MusicBrainz could not answer.
fn mb_error(error: MbError) -> CatalogError {
    match error {
        MbError::InvalidMbid(_) => {
            CatalogError::Invalid("MusicBrainz does not accept this id".to_owned())
        }
        other => CatalogError::Unavailable(other.to_string()),
    }
}

/// Run one MusicBrainz read with a single bounded retry: a rate limit
/// waits out its `Retry-After` when that is short, a dead connection waits
/// half a second. Reads are idempotent, so retrying is safe; the bound
/// keeps a page from hanging on a struggling mirror.
async fn mb_retry<T, F, Fut>(mut call: F) -> Result<T, MbError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, MbError>>,
{
    const MAX_WAIT_SECS: f64 = 2.5;
    let wait = match call().await {
        Err(MbError::RateLimited { retry_after_secs }) => match retry_after_secs.unwrap_or(1.0) {
            secs if secs <= MAX_WAIT_SECS => Duration::from_secs_f64(secs.max(0.0)),
            _ => return Err(MbError::RateLimited { retry_after_secs }),
        },
        Err(MbError::Unavailable(_)) => Duration::from_millis(500),
        other => return other,
    };
    tokio::time::sleep(wait).await;
    call().await
}

/// Record a MusicBrainz outage in the request's degradation context.
fn record_mb_down(cause: &CatalogError) {
    if matches!(cause, CatalogError::Unavailable(_)) {
        droppedneedle_providers::record_current(
            "musicbrainz",
            droppedneedle_providers::IntegrationStatus::Error,
            false,
        );
    }
}

/// True for a MusicBrainz id shape (`8-4-4-4-12` hex).
pub fn is_mbid(value: &str) -> bool {
    droppedneedle_providers::musicbrainz::is_valid_mbid(value)
}

// Cache lifetimes are fixed in code, not settings: one policy for every
// install, at the values v2 shipped as defaults.

/// A MusicBrainz search result, reused for an hour. Tag and album
/// searches keep theirs for twice as long.
pub const SEARCH_TTL: Duration = Duration::from_secs(3600);
/// A TheAudioDB hit, kept for a week: artwork rarely changes.
const AUDIODB_FOUND_TTL: Duration = Duration::from_secs(7 * 86400);
/// A TheAudioDB miss, kept for a day so new artwork shows up soon.
const AUDIODB_MISSING_TTL: Duration = Duration::from_secs(86400);

/// Dependencies the routes need. Clone shares them.
#[derive(Clone)]
pub struct CatalogDeps {
    /// The service.
    pub catalog: Catalog,
}

/// The artist and album routes at their full `/api/v3` paths, for any
/// signed-in user. Path parameter names match the follow and edition
/// routes mounted on the same prefixes (the router requires one name per
/// segment).
pub fn routes() -> Routes<CatalogDeps> {
    use handlers::*;

    Routes::new()
        .user(routes!(artist))
        .user(routes!(artist_extended))
        .user(routes!(artist_releases))
        .user(routes!(similar_artists))
        .user(routes!(top_songs))
        .user(routes!(top_albums))
        .user(routes!(artist_lastfm))
        .user(routes!(artist_purchase_options))
        .user(routes!(album))
        .user(routes!(album_basic))
        .user(routes!(album_tracks))
        .user(routes!(album_editions))
        .user(routes!(edition_tracks))
        .user(routes!(album_refresh))
        .user(routes!(similar_albums))
        .user(routes!(album_plugin_details))
        .user(routes!(more_by_artist))
        .user(routes!(album_lastfm))
        .user(routes!(album_purchase_options))
}
