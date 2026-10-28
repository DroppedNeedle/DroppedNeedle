//! The acquisition clients, built from the saved settings: the slskd
//! repository, the SABnzbd queue and the Usenet indexers, as download
//! sources for the worker and as inputs for the health probes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::downloads::journal::Journal;
use crate::downloads::sources::DownloadSource;
use crate::downloads::worker::SourceSet;
use crate::imports::lidarr::LidarrClient;
use crate::probes::ProbeInputs;
use crate::slskd::{DownloadPolicy as SlskdPolicy, ReqwestSlskdHttp, SlskdClient, SlskdRepository};
use crate::sources::{SabnzbdSource, SlskdSource};
use crate::target::Targets;
use crate::usenet::newznab::{NewznabClient, NewznabIndexer, NewznabIndexerEntry};
use crate::usenet::policy::UsenetPolicy;
use crate::usenet::prowlarr::{ProwlarrClient, ProwlarrIndexer};
use crate::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
use crate::wiring::{ClientLives, usenet_indexers};
use droppedneedle_kernel::runtime_config::secret_sections::{
    DownloadClients, LidarrImportConnection, NewznabIndexer as ConfigIndexer, ProwlarrConnection,
    SlskdConnection,
};
use droppedneedle_kernel::runtime_config::sections::{DownloadPolicy, UsenetBackendSetting};
use droppedneedle_plugins::host::PluginHost;
use tokio::sync::watch;

/// The settings one client set is built from.
pub(crate) struct ClientSettings {
    pub(crate) slskd: SlskdConnection,
    pub(crate) sabnzbd: DownloadClients,
    pub(crate) indexers: Vec<ConfigIndexer>,
    pub(crate) prowlarr: ProwlarrConnection,
    pub(crate) lidarr: LidarrImportConnection,
    /// `None` when the download policy does not read.
    pub(crate) policy: Option<DownloadPolicy>,
    pub(crate) backend: UsenetBackendSetting,
    /// Library root folders (the mount probe compares filesystems).
    pub(crate) library_roots: Vec<PathBuf>,
}

/// One built set of clients for the current settings.
pub struct ClientSet {
    /// Download sources for the worker.
    pub sources: SourceSet,
    /// Health probe inputs.
    pub probe_inputs: Arc<ProbeInputs>,
}

/// A built client set and the change notices of the sections it was
/// built from.
struct Built {
    notices: Vec<watch::Receiver<i64>>,
    set: Arc<ClientSet>,
}

/// Acquisition clients over the live settings. The set is rebuilt only
/// after a section it was built from changes, so per-client caches survive
/// between uses while a saved setting takes effect on the next use.
pub struct LiveClients {
    http: reqwest::Client,
    pub(crate) settings: ClientLives,
    slskd_downloads: PathBuf,
    journal: Arc<Journal>,
    built: Mutex<Option<Built>>,
    /// Plugin sources and usenet-targeting plugin indexers join the
    /// pipeline through the host. `None` on the test bundle.
    plugins: Option<Arc<PluginHost>>,
    targets: Arc<Targets>,
}

impl LiveClients {
    /// Clients over the live sections. `slskd_downloads` is the slskd
    /// downloads mount (`SLSKD_DOWNLOADS_PATH`).
    pub fn new(
        http: reqwest::Client,
        settings: ClientLives,
        slskd_downloads: PathBuf,
        journal: Arc<Journal>,
        targets: Arc<Targets>,
        plugins: Option<Arc<PluginHost>>,
    ) -> Self {
        Self {
            http,
            settings,
            slskd_downloads,
            journal,
            built: Mutex::new(None),
            plugins,
            targets,
        }
    }

    /// What each task fetches, shared by every source.
    pub fn targets(&self) -> &Arc<Targets> {
        &self.targets
    }

    /// The built-in sources for the current settings plus one source per
    /// enabled plugin download client.
    pub fn sources_with_plugins(
        &self,
        policy: &DownloadPolicy,
    ) -> crate::downloads::worker::SourceSet {
        let built = self.current().sources.clone();
        let Some(host) = &self.plugins else {
            return built;
        };
        let keys = host.download_source_keys();
        if keys.is_empty() {
            return built;
        }
        let release_policy = crate::plugin_source::ReleasePolicy {
            usenet: UsenetPolicy::from_saved(policy),
            ignored_terms: policy.ignored_terms.clone(),
            required_terms: policy.required_terms.clone(),
            auto_accept: policy.preflight_score_auto_accept,
            manual_min: policy.preflight_score_manual_min,
        };
        let mut sources: Vec<Arc<dyn DownloadSource>> = built.iter().cloned().collect();
        for key in keys {
            sources.push(Arc::new(
                crate::plugin_source::PluginDownloadSource::new(
                    Arc::clone(host),
                    key,
                    self.journal.clone(),
                    release_policy.clone(),
                )
                .with_targets(self.targets.clone()),
            ));
        }
        Arc::new(sources)
    }

    /// The client set for the current settings.
    pub fn current(&self) -> Arc<ClientSet> {
        let mut built = match self.built.lock() {
            Ok(built) => built,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(built) = built.as_ref()
            && !built
                .notices
                .iter()
                .any(|notice| notice.has_changed().unwrap_or(false))
        {
            return built.set.clone();
        }
        // Subscribed before the read, so a change that lands during the
        // build is seen on the next use.
        let notices = self.settings.subscribe();
        let set = Arc::new(self.build(&self.settings.read()));
        *built = Some(Built {
            notices,
            set: set.clone(),
        });
        set
    }

    fn build(&self, settings: &ClientSettings) -> ClientSet {
        let http = &self.http;
        // The probes still see the saved clients while the download policy
        // does not read; nothing searches or downloads.
        let readable = settings.policy.is_some();
        let usenet_policy = UsenetPolicy::from_saved(&settings.policy.clone().unwrap_or_default());
        // Without a readable policy nothing searches; the repository then
        // only answers the probes.
        let slskd_policy = settings
            .policy
            .as_ref()
            .map(SlskdPolicy::from_saved)
            .unwrap_or_default();
        let slskd_repo =
            slskd_repository(http, &settings.slskd, &self.slskd_downloads, slskd_policy);
        let sab_queue = sabnzbd_queue(http, &settings.sabnzbd, &usenet_policy);
        let newznab = Arc::new(newznab_indexer(http, &settings.indexers));
        let prowlarr = Arc::new(prowlarr_indexer(http, &settings.prowlarr));
        let mut sources: Vec<Arc<dyn DownloadSource>> = Vec::new();
        if let Some(repo) = slskd_repo.clone()
            && readable
            && settings.slskd.enabled
        {
            sources.push(Arc::new(
                SlskdSource::new(repo, self.journal.clone()).with_targets(self.targets.clone()),
            ));
        }
        if let Some(queue) = sab_queue.clone()
            && readable
            && settings.sabnzbd.sabnzbd.enabled
        {
            sources.push(Arc::new(
                SabnzbdSource::new(
                    queue,
                    usenet_indexers(settings.backend, &newznab, &prowlarr, self.plugins.as_ref()),
                    usenet_policy,
                    self.journal.clone(),
                    Some(settings.sabnzbd.sabnzbd.category.clone()),
                )
                .with_targets(self.targets.clone()),
            ));
        }
        let probe_inputs = Arc::new(ProbeInputs {
            slskd: slskd_repo,
            slskd_enabled: settings.slskd.enabled,
            slskd_mount_base: self.slskd_downloads.clone(),
            slskd_subpath: settings.slskd.downloads_subpath.clone(),
            library_roots: settings.library_roots.clone(),
            sabnzbd: sab_queue,
            sabnzbd_section: settings.sabnzbd.clone(),
            newznab,
            newznab_entries: settings.indexers.clone(),
            prowlarr,
            prowlarr_section: settings.prowlarr.clone(),
            lidarr: LidarrClient::new(http.clone()),
            lidarr_section: settings.lidarr.clone(),
        });
        ClientSet {
            sources: Arc::new(sources),
            probe_inputs,
        }
    }
}

/// slskd repository, when the section configures one. The mount is
/// `SLSKD_DOWNLOADS_PATH` plus the confined subpath from settings, and
/// `policy` holds the saved quality band and recipe the picks follow.
fn slskd_repository(
    http: &reqwest::Client,
    section: &SlskdConnection,
    downloads: &Path,
    policy: SlskdPolicy,
) -> Option<Arc<SlskdRepository<ReqwestSlskdHttp>>> {
    if section.url.is_empty() || section.api_key.expose().is_empty() {
        return None;
    }
    let transport = ReqwestSlskdHttp::new(http.clone(), &section.url, section.api_key.expose());
    let client = SlskdClient::new(transport);
    let mount = crate::landing::DownloadsFolder::with_subpath(
        downloads.to_path_buf(),
        &section.downloads_subpath,
    );
    let mut repo = SlskdRepository::new(
        client,
        &section.url,
        section.api_key.expose(),
        mount,
        policy,
    );
    if !section.slskd_incomplete_mount.trim().is_empty() {
        repo = repo.with_incomplete_mount(PathBuf::from(section.slskd_incomplete_mount.trim()));
    }
    Some(Arc::new(repo))
}

/// SABnzbd queue, when the section configures one.
fn sabnzbd_queue(
    http: &reqwest::Client,
    section: &DownloadClients,
    policy: &UsenetPolicy,
) -> Option<Arc<SabnzbdQueue>> {
    let sab = &section.sabnzbd;
    if sab.url.is_empty() || sab.api_key.expose().is_empty() {
        return None;
    }
    let client = SabnzbdClient::new(
        http.clone(),
        &sab.url,
        sab.api_key.expose(),
        3,
        Duration::from_secs(1),
    );
    Some(Arc::new(SabnzbdQueue::new(
        client,
        &sab.url,
        sab.api_key.expose(),
        PathBuf::from(sab.downloads_mount.clone()),
        policy.clone(),
    )))
}

/// Native indexer fan-out over the configured entries.
fn newznab_indexer(http: &reqwest::Client, entries: &[ConfigIndexer]) -> NewznabIndexer {
    let entries = entries
        .iter()
        .map(|entry| NewznabIndexerEntry {
            client: NewznabClient::new(
                http.clone(),
                &entry.url,
                entry.api_key.expose(),
                &entry.id,
                &entry.name,
            ),
            id: entry.id.clone(),
            name: entry.name.clone(),
            categories: entry.categories.iter().map(|cat| *cat as i32).collect(),
            enabled: entry.enabled,
            priority: entry.priority.max(0) as u32,
            limit: 100,
        })
        .collect();
    NewznabIndexer::new(
        entries,
        Duration::from_secs(300),
        Duration::from_secs(60),
        Duration::from_secs(300),
        Duration::from_secs(30),
    )
}

/// Prowlarr fan-out over the single connection.
fn prowlarr_indexer(http: &reqwest::Client, section: &ProwlarrConnection) -> ProwlarrIndexer {
    let client = if section.url.is_empty() || section.api_key.expose().is_empty() {
        None
    } else {
        Some(ProwlarrClient::new(
            http.clone(),
            &section.url,
            section.api_key.expose(),
            "prowlarr",
        ))
    };
    ProwlarrIndexer::new(
        client,
        vec![3000, 3010, 3040],
        section.enabled,
        Duration::from_secs(300),
        Duration::from_secs(300),
        Duration::from_secs(30),
    )
}
