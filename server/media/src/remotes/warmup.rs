//! The MBID album index and the loops that keep it warm.
//!
//! An album match looks here first: each source's albums that carry a
//! MusicBrainz release or release-group id, in the server's order, each
//! with the Navidrome folder or Plex section it was listed under, so the
//! caller's scope is checked in memory. Each source's entries are stamped
//! with the server address they were read from, and a match against
//! another address ignores them. One loop per source on the loop
//! supervisor rebuilds that source's part shortly after start and every
//! four hours after, through the admin's shared account, read again on
//! every pass. A source that is not set up, or shares no account, holds no
//! entries; a pass that fails keeps the last good index.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickError, TickResult,
};

use super::adapter::request_id;
use super::connections::{ConnectionResolver, Reads, ResolveError};
use super::models::SourceName;

/// Jellyfin startup delay (v2 `warm_jellyfin_mbid_index`).
pub const JELLYFIN_STARTUP_DELAY: Duration = Duration::from_secs(8);
/// Navidrome startup delay (v2 `warm_navidrome_mbid_cache`).
pub const NAVIDROME_STARTUP_DELAY: Duration = Duration::from_secs(12);
/// Plex startup delay (v2 `warm_plex_mbid_cache`).
pub const PLEX_STARTUP_DELAY: Duration = Duration::from_secs(15);
/// Rebuild cadence for every source (v2 sleeps `14400`, four hours).
pub const WARMUP_INTERVAL: Duration = Duration::from_secs(14_400);
/// Spread of each rebuild, so the three passes drift apart.
pub const WARMUP_JITTER: Duration = Duration::from_secs(300);

/// The Jellyfin warmup's loop name.
pub const JELLYFIN_JOB: &str = "remotes-warmup:jellyfin";
/// The Navidrome warmup's loop name.
pub const NAVIDROME_JOB: &str = "remotes-warmup:navidrome";
/// The Plex warmup's loop name.
pub const PLEX_JOB: &str = "remotes-warmup:plex";

/// Albums one page of the index read asks for.
pub const INDEX_PAGE: i64 = 500;
/// The most entries one source's index holds, so a huge library cannot
/// grow the pass or the memory without bound.
pub const INDEX_CAP: usize = 200_000;
/// The most pages one source's pass reads, so a server that answers full
/// pages forever or reports a huge total cannot keep a pass running.
pub const INDEX_MAX_PAGES: usize = INDEX_CAP / INDEX_PAGE as usize;

/// The sources the warmups cover, in start order.
const SOURCES: [SourceName; 3] = [
    SourceName::Jellyfin,
    SourceName::Navidrome,
    SourceName::Plex,
];

/// Row code of a failed warmup pass.
const WARMUP_FAILED: &str = "WARMUP_FAILED";

/// The warmup loop name for `source`.
pub fn job_name(source: SourceName) -> &'static str {
    match source {
        SourceName::Jellyfin => JELLYFIN_JOB,
        SourceName::Navidrome => NAVIDROME_JOB,
        SourceName::Plex => PLEX_JOB,
    }
}

/// Delay before `source`'s first pass; the three are staggered so boot
/// work does not land at once.
pub fn startup_delay(source: SourceName) -> Duration {
    match source {
        SourceName::Jellyfin => JELLYFIN_STARTUP_DELAY,
        SourceName::Navidrome => NAVIDROME_STARTUP_DELAY,
        SourceName::Plex => PLEX_STARTUP_DELAY,
    }
}

/// An index entry, kept only when `mbid` has the MusicBrainz id shape
/// (36 characters, lowercase hex in 8-4-4-4-12 groups) and `id` is a plain
/// request id.
pub(crate) fn indexed(mbid: &str, id: &str, scope: Option<&str>) -> Option<IndexedAlbum> {
    let groups: Vec<&str> = mbid.split('-').collect();
    let shaped = groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| {
            group
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        });
    (shaped && request_id(id).is_ok()).then(|| IndexedAlbum {
        mbid: mbid.to_owned(),
        id: id.to_owned(),
        scope: scope.map(str::to_owned),
    })
}

/// One remote album that carries a MusicBrainz id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedAlbum {
    /// The release or release-group id it carries.
    pub mbid: String,
    /// Opaque remote album id.
    pub id: String,
    /// The Navidrome folder or Plex section it was listed under; Jellyfin
    /// records none.
    pub scope: Option<String>,
}

/// Each source's server address and indexed albums, by the id they carry.
type Sources = HashMap<SourceName, (String, HashMap<String, Vec<IndexedAlbum>>)>;

/// The in-memory MBID index, per source. Cheap to clone; every clone
/// shares one index.
#[derive(Clone, Default)]
pub struct MbidIndex {
    sources: Arc<RwLock<Sources>>,
}

impl MbidIndex {
    /// The albums of `source` that carry `mbid`, in the server's order,
    /// when they were read from the server at `server`.
    pub fn lookup(&self, source: SourceName, server: &str, mbid: &str) -> Vec<IndexedAlbum> {
        self.read()
            .get(&source)
            .filter(|(read_from, _)| read_from == server)
            .and_then(|(_, albums)| albums.get(mbid))
            .cloned()
            .unwrap_or_default()
    }

    /// Replace `source`'s entries with `albums` read from the server at
    /// `server`; answers how many ids it no longer holds.
    pub fn replace(&self, source: SourceName, server: &str, albums: Vec<IndexedAlbum>) -> usize {
        let mut by_mbid: HashMap<String, Vec<IndexedAlbum>> = HashMap::new();
        for album in albums {
            by_mbid.entry(album.mbid.clone()).or_default().push(album);
        }
        let kept: HashSet<&String> = by_mbid.keys().collect();
        let mut sources = self.write();
        let pruned = sources
            .get(&source)
            .map(|(_, old)| old.keys().filter(|mbid| !kept.contains(mbid)).count())
            .unwrap_or(0);
        sources.insert(source, (server.to_owned(), by_mbid));
        pruned
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Sources> {
        self.sources
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Sources> {
        self.sources
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// What one warmup pass accomplished, for logs and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupStats {
    /// Source that warmed.
    pub source: SourceName,
    /// Index entries built.
    pub warmed: usize,
    /// Ids dropped since the last pass.
    pub pruned: usize,
}

/// The warmup passes: the admin's shared accounts, the outbound client and
/// the index they fill.
#[derive(Clone)]
pub struct Warmups {
    http: reqwest::Client,
    resolver: Arc<ConnectionResolver>,
    index: MbidIndex,
}

impl Warmups {
    /// Passes over the shared accounts `resolver` reads, filling `index`.
    pub fn new(http: reqwest::Client, resolver: Arc<ConnectionResolver>, index: MbidIndex) -> Self {
        Self {
            http,
            resolver,
            index,
        }
    }

    /// Rebuild `source`'s part of the index. A source that is not set up or
    /// shares no account is emptied; a pass that fails leaves it as it was.
    pub async fn pass(&self, source: SourceName) -> Result<WarmupStats, String> {
        let resolved = match self.resolver.resolve_shared(source) {
            Ok(resolved) => resolved,
            Err(ResolveError::NotConfigured) => {
                let pruned = self.index.replace(source, "", Vec::new());
                return Ok(WarmupStats {
                    source,
                    warmed: 0,
                    pruned,
                });
            }
            Err(error) => return Err(error.to_string()),
        };
        let server = resolved.base_url.clone();
        let albums = self
            .resolver
            .client(&self.http, resolved, Reads::Background)
            .album_index(INDEX_CAP)
            .await
            .map_err(|error| error.to_string())?;
        if albums.len() >= INDEX_CAP {
            tracing::warn!(
                source = source.as_str(),
                cap = INDEX_CAP,
                "remote album index is full; later albums match by search only"
            );
        }
        let warmed = albums.len();
        let pruned = self.index.replace(source, &server, albums);
        Ok(WarmupStats {
            source,
            warmed,
            pruned,
        })
    }
}

/// Start the three warmups on `supervisor`.
pub fn start_all(supervisor: &Supervisor, warmups: &Warmups) -> Result<(), AlreadyRunning> {
    for source in SOURCES {
        let warmups = warmups.clone();
        start(supervisor, source, move || {
            let warmups = warmups.clone();
            async move { warmups.pass(source).await }
        })?;
    }
    Ok(())
}

/// Start the warmup for `source` on `supervisor`: after its startup delay,
/// one pass, then one every [`WARMUP_INTERVAL`].
pub fn start<F, Fut>(
    supervisor: &Supervisor,
    source: SourceName,
    work: F,
) -> Result<(), AlreadyRunning>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<WarmupStats, String>> + Send,
{
    let schedule = Schedule::new(WARMUP_INTERVAL)
        .with_jitter(WARMUP_JITTER)
        .with_initial_delay(startup_delay(source));
    supervisor.periodic(job_name(source), schedule, WarmupTick { source, work })
}

/// One warmup pass.
struct WarmupTick<F> {
    source: SourceName,
    work: F,
}

impl<F, Fut> Tick for WarmupTick<F>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<WarmupStats, String>> + Send,
{
    async fn tick(&mut self, _ctx: &TickCtx) -> TickResult {
        match (self.work)().await {
            Ok(stats) => {
                tracing::debug!(
                    source = self.source.as_str(),
                    warmed = stats.warmed,
                    pruned = stats.pruned,
                    "mbid warmup pass done"
                );
                Ok(Next::Continue)
            }
            Err(cause) => Err(TickError::new(
                WARMUP_FAILED,
                format!("{}: {cause}", self.source.as_str()),
            )),
        }
    }
}
