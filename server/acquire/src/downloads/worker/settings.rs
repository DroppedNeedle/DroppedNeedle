//! The sections the download worker reads, and the worker tuning they
//! translate to.

use std::path::PathBuf;
use std::sync::Arc;

use crate::clients::LiveClients;
use crate::downloads::source_tag::SourceTag;
use crate::downloads::watchdog::{RetryPolicy, WatchdogConfig};
use crate::downloads::worker::{SourceSet, WorkerConfig};
use crate::settings::{clamp_u64, clamp_usize};
use droppedneedle_kernel::runtime_config::secret_sections::{DownloadClients, TypedLibrary};
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, MusicBrainzSettings, SourcePriority,
};
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};
use tokio::sync::watch;

/// The sections the download worker reads, once per pass.
pub struct WorkerSettings {
    clients: Arc<LiveClients>,
    policy: Live<DownloadPolicy>,
    source_priority: Live<SourcePriority>,
    usenet: Live<DownloadClients>,
    library: Live<TypedLibrary>,
    musicbrainz: Live<MusicBrainzSettings>,
}

impl WorkerSettings {
    /// Handles on `store`'s sections, with the sources `clients` build.
    pub fn new(store: &ConfigStore, clients: Arc<LiveClients>) -> Self {
        Self {
            clients,
            policy: store.live(),
            source_priority: store.live(),
            usenet: store.live(),
            library: store.live(),
            musicbrainz: store.live(),
        }
    }

    /// The sources turned on right now, plugin download clients included.
    /// None while the download settings do not read.
    pub(crate) fn sources(&self) -> SourceSet {
        if !self.readable() {
            return SourceSet::default();
        }
        self.policy
            .read()
            .map(|policy| self.clients.sources_with_plugins(&policy))
            .unwrap_or_default()
    }

    /// The sources on now, for work outside the worker's passes (the
    /// wanted watcher's search), or `None` while the download settings do
    /// not read.
    pub fn current(&self) -> Option<SourceSet> {
        self.readable().then(|| self.sources())
    }

    /// One change notice per section the sources and tuning are read
    /// from.
    pub(crate) fn notices(&self) -> Vec<watch::Receiver<i64>> {
        let mut notices = self.clients.settings.subscribe();
        notices.push(self.source_priority.subscribe());
        notices.push(self.musicbrainz.subscribe());
        notices
    }

    /// Whether every section the worker cannot stand in for reads. While
    /// one does not, no pass runs: a default quality band would land the
    /// wrong files, and an unreadable MusicBrainz source would fail every
    /// landing's match.
    pub(crate) fn readable(&self) -> bool {
        self.policy.read().is_some()
            && self.source_priority.read().is_some()
            && self.library.read().is_some()
            && self.musicbrainz.read().is_some()
    }

    /// The worker tuning as saved right now, or `None` while a section it
    /// needs does not read. An unreadable Usenet client section is an
    /// unconfigured one, as in the client set, so slskd keeps working.
    pub(crate) fn config(&self) -> Option<WorkerConfig> {
        if !self.readable() {
            return None;
        }
        let policy = self.policy.read()?;
        let source_priority = self.source_priority.read()?;
        let usenet = self.usenet.read().unwrap_or_default();
        Some(worker_config(
            &policy,
            &source_priority,
            orphan_mounts(&usenet),
        ))
    }
}

/// Every Usenet client's completed-downloads folder while that client is
/// set up, enabled or not: the folders no recycle bin may overlap.
pub(crate) fn usenet_mounts(section: &DownloadClients) -> Vec<PathBuf> {
    distinct(
        [
            (&section.sabnzbd.url, &section.sabnzbd.downloads_mount),
            (&section.nzbget.url, &section.nzbget.downloads_mount),
        ]
        .into_iter()
        .filter(|(url, _)| !url.is_empty())
        .map(|(_, mount)| PathBuf::from(mount)),
    )
}

/// The folders the orphan sweep walks: the completed-downloads folder of
/// each Usenet client the client set builds (SABnzbd needs its key as well
/// as its URL), enabled or not, so a job a client took is swept there
/// after a switch.
fn orphan_mounts(section: &DownloadClients) -> Vec<PathBuf> {
    let sab = &section.sabnzbd;
    let nzbget = &section.nzbget;
    let sab_set_up = !sab.url.is_empty() && !sab.api_key.expose().is_empty();
    distinct(
        [
            sab_set_up.then(|| PathBuf::from(&sab.downloads_mount)),
            (!nzbget.url.is_empty()).then(|| PathBuf::from(&nzbget.downloads_mount)),
        ]
        .into_iter()
        .flatten(),
    )
}

/// The folders once each, in order.
fn distinct(mounts: impl Iterator<Item = PathBuf>) -> Vec<PathBuf> {
    mounts.fold(Vec::new(), |mut kept, mount| {
        if !kept.contains(&mount) {
            kept.push(mount);
        }
        kept
    })
}

/// Worker tuning bound from the download-policy section. Only the
/// Usenet complete dirs are walked: slskd folders cannot prove client
/// inactivity from the folder name, so they always keep.
fn worker_config(
    policy: &DownloadPolicy,
    source_priority: &SourcePriority,
    usenet_mounts: Vec<PathBuf>,
) -> WorkerConfig {
    let mut source_order: Vec<SourceTag> = source_priority
        .0
        .iter()
        .filter_map(|source| source.parse().ok())
        .collect();
    if source_order.is_empty() {
        source_order = vec![SourceTag::Soulseek, SourceTag::Usenet];
    }
    let orphan_roots = usenet_mounts
        .into_iter()
        .map(|mount| (SourceTag::Usenet, mount))
        .collect();
    WorkerConfig {
        max_concurrent_downloads: clamp_usize(policy.max_concurrent_downloads).max(1),
        max_failover_attempts: policy.max_failover_attempts.max(0),
        retry: RetryPolicy::from_policy(policy),
        watchdog: WatchdogConfig {
            poll_interval_seconds: 2.0,
            stall_timeout_seconds: (clamp_u64(policy.download_stall_timeout_minutes).max(1) * 60)
                as f64,
            queued_timeout_seconds: (clamp_u64(policy.download_queued_timeout_minutes).max(1) * 60)
                as f64,
            preferred_quality_wait_seconds: (clamp_u64(policy.preferred_quality_wait_minutes)
                .max(1)
                * 60) as f64,
            deadline_seconds: 6.0 * 3600.0,
            materialize_seconds: 90.0,
            reap_threshold_seconds: 1800.0,
        },
        source_order,
        orphan_roots,
        worker_id: "download-worker".to_owned(),
        review: crate::search_jobs::review::ReviewMode::from_setting(&policy.search_review),
    }
}
