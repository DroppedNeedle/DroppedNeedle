//! Media-backed inputs for the jobs loops: the now-playing feed over the
//! live presence registry, the Jellyfin/Navidrome/Plex session pollers and
//! the MBID warmups.
//! The Navidrome playlist exporter lives in
//! [`droppedneedle_kernel::jobs::playlist_export`].
//!
//! The pollers read sessions through the admin's shared accounts (v2 polled
//! with the configured server credentials), so a source counts as enabled
//! exactly when the admin configured it with a credential. Settings are
//! re-read on every cycle.

use std::sync::Arc;

use super::adapter::RemoteHandle;
use super::connections::{ConnectionResolver, Reads, ResolvedConnection};
use super::models::{SessionView, SourceName};
use super::warmup::{MbidIndex, Warmups};
use crate::playback::presence::{ExternalSession, PresenceRegistry};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::playlist_export::M3uPlaylistExporter;
use droppedneedle_kernel::jobs::presence::{
    PresenceSession, PresenceSources, PresenceStore, SourceStatus,
};

/// What the media bundle hands the jobs bundle.
#[derive(Clone)]
pub struct MediaJobs {
    presence: PresenceRegistry,
    resolver: Arc<ConnectionResolver>,
    http: reqwest::Client,
    pool: sqlx::SqlitePool,
    clock: Arc<dyn Clock>,
    index: MbidIndex,
}

impl MediaJobs {
    /// Inputs over the live presence registry the playback routes write,
    /// the shared connection resolver (admin servers and their shared
    /// accounts), the outbound client, the database the playlist export
    /// reads, the clock the presence sweep measures against and the MBID
    /// index the warmups fill.
    pub fn new(
        presence: PresenceRegistry,
        resolver: Arc<ConnectionResolver>,
        http: reqwest::Client,
        pool: sqlx::SqlitePool,
        clock: Arc<dyn Clock>,
        index: MbidIndex,
    ) -> Self {
        Self {
            presence,
            resolver,
            http,
            pool,
            clock,
            index,
        }
    }

    /// The MBID warmup passes over the shared accounts.
    pub fn warmups(&self) -> Warmups {
        Warmups::new(self.http.clone(), self.resolver.clone(), self.index.clone())
    }

    /// The now-playing feed over the registry.
    pub fn feed(&self) -> RegistryFeed {
        RegistryFeed {
            registry: self.presence.clone(),
            clock: self.clock.clone(),
        }
    }

    /// The upstream session pollers.
    pub fn pollers(&self) -> RemoteSessionPollers {
        RemoteSessionPollers {
            resolver: self.resolver.clone(),
            http: self.http.clone(),
        }
    }

    /// The m3u8 exporter.
    pub fn exporter(&self) -> M3uPlaylistExporter {
        M3uPlaylistExporter::new(Some(self.pool.clone()))
    }
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

/// The presence store the loop sweeps and reconciles: the same registry
/// `GET /now-playing` and compat `getNowPlaying` read.
#[derive(Clone)]
pub struct RegistryFeed {
    registry: PresenceRegistry,
    clock: Arc<dyn Clock>,
}

impl RegistryFeed {
    /// A feed over `registry` with an explicit clock.
    pub fn new(registry: PresenceRegistry, clock: Arc<dyn Clock>) -> Self {
        Self { registry, clock }
    }
}

impl PresenceStore for RegistryFeed {
    fn sweep(&self) -> BoxFuture<'_, ()> {
        self.registry.sweep(self.clock.now_unix());
        Box::pin(async {})
    }

    fn reconcile(&self, source: &str, sessions: Vec<PresenceSession>) -> BoxFuture<'_, ()> {
        let sessions = sessions
            .into_iter()
            .map(|session| ExternalSession {
                key: session.key,
                user_name: session.user_name,
                device_name: session.device_name,
                track_name: session.track_name,
                artist_name: session.artist_name,
                album_name: session.album_name,
                cover_url: session.cover_url,
                is_paused: session.is_paused,
                progress_ms: Some(session.progress_ms),
                duration_ms: Some(session.duration_ms),
            })
            .collect();
        self.registry
            .reconcile_source(source, sessions, self.clock.now_unix());
        Box::pin(async {})
    }
}

/// Session polling over the admin's shared accounts.
#[derive(Clone)]
pub struct RemoteSessionPollers {
    resolver: Arc<ConnectionResolver>,
    http: reqwest::Client,
}

impl RemoteSessionPollers {
    fn resolve(&self, source: SourceName) -> Result<ResolvedConnection, String> {
        self.resolver
            .resolve_shared(source)
            .map_err(|error| error.to_string())
    }

    fn handle(&self, source: SourceName) -> Result<RemoteHandle, String> {
        let resolved = self.resolve(source)?;
        Ok(self
            .resolver
            .client(&self.http, resolved, Reads::Background))
    }

    async fn poll(&self, source: SourceName) -> Result<Vec<PresenceSession>, String> {
        let handle = self.handle(source)?;
        let view = handle.sessions().await.map_err(|error| error.to_string())?;
        Ok(view
            .sessions
            .into_iter()
            .filter_map(|session| presence_session(source, session))
            .collect())
    }
}

/// One upstream session in feed shape. Sessions without a track title are
/// skipped (v2 `map_*`).
fn presence_session(source: SourceName, session: SessionView) -> Option<PresenceSession> {
    if session.track_title.is_empty() {
        return None;
    }
    Some(PresenceSession {
        key: format!("{}:{}", source.as_str(), session.session_id),
        user_name: session.user_name,
        device_name: session.device_name,
        track_name: session.track_title,
        artist_name: session.artist_name,
        album_name: Some(session.album_name).filter(|name| !name.is_empty()),
        cover_url: session.image_url.unwrap_or_default(),
        is_paused: session.is_paused,
        progress_ms: session.progress_ms,
        duration_ms: session.duration_ms,
    })
}

impl PresenceSources for RemoteSessionPollers {
    fn status(&self) -> BoxFuture<'_, SourceStatus> {
        let enabled = |source| self.resolve(source).is_ok();
        let status = SourceStatus {
            jellyfin: enabled(SourceName::Jellyfin),
            navidrome: enabled(SourceName::Navidrome),
            plex: enabled(SourceName::Plex),
        };
        Box::pin(async move { status })
    }

    fn poll_jellyfin(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>> {
        Box::pin(self.poll(SourceName::Jellyfin))
    }

    fn poll_navidrome(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>> {
        Box::pin(self.poll(SourceName::Navidrome))
    }

    fn poll_plex(&self) -> BoxFuture<'_, Result<Vec<PresenceSession>, String>> {
        Box::pin(self.poll(SourceName::Plex))
    }
}
