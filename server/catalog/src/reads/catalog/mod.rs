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
//! Every upstream answer goes through the provider cache's one helper,
//! with a fixed lifetime per kind of answer: identical concurrent loads
//! share one upstream call, and a kept answer stands in when the provider
//! fails. Library flags are read fresh on every request, so they never go
//! stale behind a cache.
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
use droppedneedle_providers::ProviderError;
use droppedneedle_providers::{CacheKey, Fetched, Policy, RequestPriority, Source};

use self::error::CatalogError;
use self::library::LocalCatalog;
use self::upstream::Upstream;
use crate::reads::platform::artwork::warm::{CoverWarm, WarmTargets};

/// The catalog service. Clone shares everything.
#[derive(Clone)]
pub struct Catalog {
    inner: Arc<Inner>,
    follows: Arc<dyn ports::FollowLookup>,
    purchase_links: Arc<dyn ports::PurchaseLinkSource>,
    details: Arc<dyn ports::DetailsSource>,
    cover_warm: Option<CoverWarm>,
}

struct Inner {
    upstream: Upstream,
    local: LocalCatalog,
    /// Discographies and tracklists being fetched in the background.
    warming: Mutex<HashSet<String>>,
}

impl Catalog {
    /// Serve pages from these upstreams and this library.
    pub fn new(upstream: Upstream, local: LocalCatalog) -> Self {
        Self {
            inner: Arc::new(Inner {
                upstream,
                local,
                warming: Mutex::new(HashSet::new()),
            }),
            follows: Arc::new(ports::NoFollows),
            purchase_links: Arc::new(ports::NoPurchaseLinks),
            details: Arc::new(ports::NoDetails),
            cover_warm: None,
        }
    }

    /// Queue a background warm of the images an artist page shows here.
    #[must_use]
    pub fn with_cover_warm(mut self, warm: CoverWarm) -> Self {
        self.cover_warm = Some(warm);
        self
    }

    /// Queue a background warm of `targets`, when warming is wired.
    fn warm_covers(&self, page: &'static str, targets: WarmTargets) {
        if let Some(warm) = &self.cover_warm {
            warm.queue(page, targets);
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

    pub(crate) fn local(&self) -> &LocalCatalog {
        &self.inner.local
    }

    /// One provider answer through the cache helper: a fresh row as it
    /// is, else one fetch at `priority` shared by every caller of the key,
    /// with a kept row standing in when that fetch fails. `None` is a
    /// stored "nothing there".
    async fn cached<T, F, Fut>(
        &self,
        key: &CacheKey,
        policy: Policy,
        priority: RequestPriority,
        fetch: F,
    ) -> Result<Option<T>, CatalogError>
    where
        T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
        F: FnOnce(RequestPriority) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = Result<Fetched<T>, CatalogError>> + Send + 'static,
    {
        self.upstream()
            .cache()
            .get_or_fetch(key, policy, priority, fetch)
            .await
    }

    /// Whether any answer is held under `key`, fresh or stale. Never
    /// dials out.
    async fn held(&self, key: &CacheKey) -> bool {
        self.upstream()
            .cache()
            .peek::<serde::de::IgnoredAny>(key)
            .await
            .is_some()
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
        F: FnOnce(Catalog) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = models::PluginDetails> + Send + 'static,
    {
        let token = self.details.token();
        if token.is_empty() {
            return Ok(models::PluginDetails::default());
        }
        let key = CacheKey::hashed(Source::Plugins, "details", &[kind, mbid, &token]);
        let catalog = self.clone();
        let details = self
            .cached(
                &key,
                PLUGIN_DETAILS,
                RequestPriority::UserInitiated,
                move |_| async move { Ok(Fetched::Found(fetch(catalog).await)) },
            )
            .await?;
        Ok(details.unwrap_or_default())
    }

    /// MusicBrainz first, the library's copy when MusicBrainz cannot
    /// answer. A MusicBrainz answer stands, a miss is 404. On a failure the
    /// outage is recorded, and only when MusicBrainz was unavailable does
    /// `library` read the library: its copy answers in MusicBrainz's place,
    /// and without one the failure stands. The library copy never enters
    /// the provider cache: a failed fetch stores nothing.
    pub(super) async fn musicbrainz_or_library<M, L, Fut>(
        &self,
        musicbrainz: Result<Option<M>, CatalogError>,
        library: impl FnOnce() -> Fut,
    ) -> Result<Answer<M, L>, CatalogError>
    where
        Fut: Future<Output = Result<Option<L>, CatalogError>>,
    {
        match musicbrainz {
            Ok(Some(found)) => Ok(Answer::MusicBrainz(found)),
            Ok(None) => Err(CatalogError::NotFound),
            Err(error) => {
                record_mb_down(&error);
                if matches!(error, CatalogError::Unavailable(_))
                    && let Some(copy) = library().await?
                {
                    return Ok(Answer::Library(copy));
                }
                Err(error)
            }
        }
    }

    /// Drop cached entries by exact key, once committed.
    async fn forget(&self, keys: &[CacheKey]) {
        self.upstream().cache().forget(keys).await;
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

/// Where a page's answer came from: MusicBrainz, or the library's copy
/// while MusicBrainz was down.
pub(super) enum Answer<M, L> {
    /// MusicBrainz answered.
    MusicBrainz(M),
    /// MusicBrainz was unavailable and the library holds a copy.
    Library(L),
}

/// Map a failed MusicBrainz lookup onto the catalog error. An id
/// MusicBrainz rejects (its 400 "Invalid mbid.") is the caller's fault;
/// everything else means MusicBrainz could not answer. The executor has
/// already retried what may be retried within a page's budget.
fn mb_error(error: ProviderError) -> CatalogError {
    match error {
        ProviderError::BadRequest { status: 400, .. } => {
            CatalogError::Invalid("MusicBrainz does not accept this id".to_owned())
        }
        other => CatalogError::Unavailable(other),
    }
}

/// Map a failed MusicBrainz search or browse onto the catalog error: a
/// rejected query is not an id the caller got wrong, so every failure means
/// MusicBrainz could not answer.
fn mb_search_error(error: ProviderError) -> CatalogError {
    CatalogError::Unavailable(error)
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

// Cache lifetimes are fixed in code, not settings: one policy per kind of
// answer for every install. The provider cache holds the shared ones;
// these are the catalog's own.

/// An hour. The discover and home pages keep their tag searches twice
/// this long.
pub const SEARCH_TTL: Duration = Duration::from_secs(3600);

/// Plugin details: an hour, so a plugin's new answers show up soon.
const PLUGIN_DETAILS: Policy = Policy {
    fresh: Duration::from_secs(3600),
    miss: Duration::from_secs(3600),
};
/// Last.fm artist and album details: an hour, as v2 kept them.
const LASTFM_DETAILS: Policy = Policy {
    fresh: Duration::from_secs(3600),
    miss: Duration::from_secs(3600),
};
/// Wikipedia and Wikidata biographies: six hours, as v2 kept them.
const BIOGRAPHY: Policy = Policy {
    fresh: Duration::from_secs(6 * 3600),
    miss: Duration::from_secs(6 * 3600),
};
/// Purchase options: a week, as v2 kept them.
const PURCHASE: Policy = Policy {
    fresh: Duration::from_secs(7 * 86400),
    miss: Duration::from_secs(7 * 86400),
};

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
