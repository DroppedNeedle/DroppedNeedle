//! Settings-save side effects.
//!
//! v2 cleared memoized provider graphs and kicked the events sweep on
//! save. v3 builds clients per request from stored config, so there is no
//! graph to rebuild; what remains is cache invalidation per section and
//! the single-flight events kick. Both ride behind trait ports so tests
//! observe them through a recorder and production wires the real cache
//! and the jobs-owned sweep.

use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::jobs::events_kick::EventsKick;
use crate::providers::cache::{ProviderCache, invalidate_source};

/// Sections whose save invalidates provider caches, and the events
/// section, which additionally kicks the sweep. Only sources with
/// registered cache roots invalidate; connection sections without cached
/// reads (Jellyfin, Navidrome, Plex, YouTube, OIDC, download clients)
/// need no invalidation because their clients build per request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedSection {
    /// ListenBrainz connection.
    ListenBrainz,
    /// Events sources (kicks the sweep; no cached reads to invalidate).
    Events,
    /// MusicBrainz connection.
    MusicBrainz,
    /// Advanced tunables (AudioDB key and TTLs).
    Advanced,
    /// Library settings (AcoustID key).
    Library,
    /// Home page settings (cached home and discover rows, plus the
    /// ListenBrainz and Last.fm reads they are built from).
    Home,
    /// Anything else (no cache roots).
    Other,
}

impl SavedSection {
    /// The fan-out for one config-file section key.
    #[must_use]
    pub fn for_key(key: &str) -> Self {
        match key {
            "listenbrainz_settings" => Self::ListenBrainz,
            "events" => Self::Events,
            "musicbrainz_settings" => Self::MusicBrainz,
            "advanced_settings" => Self::Advanced,
            "library_settings" => Self::Library,
            "home_settings" => Self::Home,
            _ => Self::Other,
        }
    }
}

/// Composite home and discover responses a home settings save makes stale
/// (v2 `clear_home_cache`).
const HOME_RESPONSE_PREFIXES: &[&str] = &["home_response:", "discover_response:"];

/// Post-save fan-out. Called after a section persists; failures are
/// impossible by construction (invalidation counts, kicks collapse).
pub trait SaveEffects: Send + Sync {
    /// Run the post-save fan-out for one section.
    fn after_save<'a>(&'a self, section: SavedSection) -> BoxFuture<'a, ()>;
}

/// Production fan-out: provider-cache invalidation plus the events kick.
pub struct LiveSaveEffects {
    /// Provider cache to invalidate.
    pub cache: Arc<dyn ProviderCache>,
    /// Events sweep kick.
    pub kick: Arc<dyn EventsKick>,
}

impl LiveSaveEffects {
    /// Build over the shared cache and a kick.
    pub fn new(cache: Arc<dyn ProviderCache>, kick: Arc<dyn EventsKick>) -> Self {
        Self { cache, kick }
    }
}

impl SaveEffects for LiveSaveEffects {
    fn after_save<'a>(&'a self, section: SavedSection) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let roots: &[&str] = match section {
                SavedSection::ListenBrainz => &["listenbrainz"],
                SavedSection::MusicBrainz => &["musicbrainz"],
                SavedSection::Advanced => &["audiodb"],
                SavedSection::Library => &["acoustid"],
                SavedSection::Home => &["listenbrainz", "lastfm"],
                SavedSection::Events | SavedSection::Other => &[],
            };
            for root in roots {
                invalidate_source(self.cache.as_ref(), root).await;
            }
            if section == SavedSection::Home {
                for prefix in HOME_RESPONSE_PREFIXES {
                    self.cache.clear_prefix(prefix).await;
                }
            }
            if section == SavedSection::Events {
                self.kick.kick();
            }
        })
    }
}
