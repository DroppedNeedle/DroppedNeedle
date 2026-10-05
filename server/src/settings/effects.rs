//! Settings-save side effects.
//!
//! v2 cleared memoized provider graphs and kicked the events sweep on
//! save. v3 builds clients per request from stored config, so there is no
//! graph to rebuild; what remains is cache invalidation per section and
//! the single-flight events kick. Both ride behind trait ports so tests
//! observe them through a recorder and production wires the real cache
//! and the jobs-owned sweep.

use std::sync::Arc;

use crate::providers::cache::{ProviderCache, invalidate_source};
use futures_util::future::BoxFuture;

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
    /// Anything else (no cache roots).
    Other,
}

/// Kick the upcoming-events sweep. Single-flight: overlapping kicks
/// collapse into one, so a burst of saves schedules one sweep.
pub trait EventsKick: Send + Sync {
    /// Request a sweep. Cheap, idempotent under concurrency.
    fn kick(&self);
}

/// No-op kick, for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct NoopKick;

#[cfg(any(test, feature = "test-support"))]
impl EventsKick for NoopKick {
    fn kick(&self) {}
}

/// Closure adapter so wiring passes the real sweep with one line.
pub struct FnKick<F> {
    /// Kick closure.
    pub kick_fn: F,
}

impl<F: Fn() + Send + Sync> EventsKick for FnKick<F> {
    fn kick(&self) {
        (self.kick_fn)();
    }
}

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
                SavedSection::Events | SavedSection::Other => &[],
            };
            for root in roots {
                invalidate_source(self.cache.as_ref(), root).await;
            }
            if section == SavedSection::Events {
                self.kick.kick();
            }
        })
    }
}
