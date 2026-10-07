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
pub mod ports;
pub mod precache;
pub mod search;
pub mod upstream;

use crate::library::operations::port::{EditionChoices, NoEditionChoices};
use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Router,
    routing::{get, post, put},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::providers::Singleflight;
use crate::providers::musicbrainz::MbError;

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
    editions: Arc<dyn EditionChoices>,
}

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
            editions: Arc::new(NoEditionChoices),
        }
    }

    /// Choose album editions through the library's edition operation.
    #[must_use]
    pub fn with_edition_choices(mut self, editions: Arc<dyn EditionChoices>) -> Self {
        self.editions = editions;
        self
    }

    /// Add extra purchase links (the plugins' `purchase_links`) from this
    /// source.
    #[must_use]
    pub fn with_purchase_links(mut self, source: Arc<dyn ports::PurchaseLinkSource>) -> Self {
        self.purchase_links = source;
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

    fn upstream(&self) -> &Upstream {
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
        crate::providers::record_current(
            "musicbrainz",
            crate::providers::IntegrationStatus::Error,
            false,
        );
    }
}

/// True for a MusicBrainz id shape (`8-4-4-4-12` hex).
pub fn is_mbid(value: &str) -> bool {
    crate::providers::musicbrainz::is_valid_mbid(value)
}

/// Seconds from a settings value, never negative.
fn secs(value: i64) -> Duration {
    Duration::from_secs(u64::try_from(value).unwrap_or(0))
}

/// Dependencies the routes need. Clone shares them.
#[derive(Clone)]
pub struct CatalogDeps {
    /// The service.
    pub catalog: Catalog,
    /// Fresh ids for error correlation.
    pub ids: Arc<dyn crate::ids::IdGenerator>,
}

/// Mount the artist and album routes, relative to `/api/v3`. Path
/// parameter names match the follow and edition routes mounted by
/// acquisition on the same prefixes (the router requires one name per
/// segment).
pub fn router(deps: CatalogDeps) -> Router {
    Router::new()
        .route("/artists/{artist_mbid}", get(handlers::artist))
        .route(
            "/artists/{artist_mbid}/extended",
            get(handlers::artist_extended),
        )
        .route(
            "/artists/{artist_mbid}/releases",
            get(handlers::artist_releases),
        )
        .route(
            "/artists/{artist_mbid}/similar",
            get(handlers::similar_artists),
        )
        .route("/artists/{artist_mbid}/top-songs", get(handlers::top_songs))
        .route(
            "/artists/{artist_mbid}/top-albums",
            get(handlers::top_albums),
        )
        .route(
            "/artists/{artist_mbid}/lastfm",
            get(handlers::artist_lastfm),
        )
        .route(
            "/artists/{artist_mbid}/purchase-options",
            get(handlers::artist_purchase_options),
        )
        .route("/albums/{album_id}", get(handlers::album))
        .route("/albums/{album_id}/basic", get(handlers::album_basic))
        .route("/albums/{album_id}/tracks", get(handlers::album_tracks))
        .route("/albums/{album_id}/editions", get(handlers::album_editions))
        .route(
            "/albums/{album_id}/editions/{release_mbid}/tracks",
            get(handlers::edition_tracks),
        )
        .route("/albums/{album_id}/refresh", post(handlers::album_refresh))
        .route(
            "/albums/{album_id}/edition",
            put(handlers::set_album_edition).delete(handlers::clear_album_edition),
        )
        .route("/albums/{album_id}/similar", get(handlers::similar_albums))
        .route(
            "/albums/{album_id}/more-by-artist",
            get(handlers::more_by_artist),
        )
        .route("/albums/{album_id}/lastfm", get(handlers::album_lastfm))
        .route(
            "/albums/{album_id}/purchase-options",
            get(handlers::album_purchase_options),
        )
        .with_state(deps)
}
