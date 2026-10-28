//! Live acquisition-health probes over config snapshots plus cached
//! verdicts.
//!
//! The imports health traits are sync, but reachability needs HTTP. The
//! split: [`LiveProbes`] implements all four traits over a [`ProbeCache`]
//! snapshot, and [`refresh_probes`] recomputes the snapshot with real
//! client health checks. Wiring runs one refresh at boot, again after a
//! client's settings are saved, and on a slow loop, so the smoke and the
//! status routes always render a fresh verdict without blocking a request
//! on five network calls. A verdict is kept with the client set it probed:
//! until a refresh lands for the current one, a read answers from the
//! saved settings alone.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::clients::LiveClients;
use super::imports::health::{
    ClientProbe, HealthProbes, IndexerProbe, LidarrReadinessProbe, NewznabProbe, SabnzbdProbe,
    SlskdProbe,
};
use super::imports::lidarr::{LidarrClient, LidarrError};
use super::imports::mount::{self, SlskdMountReport};
use super::landing::confine::FolderFault;
use super::slskd::{ReqwestSlskdHttp, SlskdRepository};
use super::usenet::newznab::NewznabIndexer;
use super::usenet::prowlarr::ProwlarrIndexer;
use super::usenet::sabnzbd::SabnzbdQueue;
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor, TickCtx};
use droppedneedle_kernel::runtime_config::secret_sections::{
    DownloadClients, LidarrImportConnection, NewznabIndexer as ConfigIndexer, ProwlarrConnection,
};

/// One cached verdict per probe.
#[derive(Debug, Clone, Default)]
pub struct CachedProbes {
    /// slskd client verdict.
    pub slskd: ClientProbe,
    /// slskd downloads mount report.
    pub slskd_mount: Option<SlskdMountReport>,
    /// SABnzbd client verdict.
    pub sabnzbd: ClientProbe,
    /// SABnzbd categories, when the probe reached the client.
    pub sabnzbd_categories: Vec<String>,
    /// SABnzbd complete dir, when the probe reached it.
    pub sabnzbd_complete_dir: Option<String>,
    /// Per-indexer verdicts (native entries plus Prowlarr).
    pub indexers: Vec<IndexerProbe>,
    /// Lidarr import readiness.
    pub lidarr: ClientProbe,
}

/// The last refresh and the client set it probed.
struct Refreshed {
    inputs: Arc<ProbeInputs>,
    probes: CachedProbes,
}

/// Shared verdict snapshot behind the live probes.
pub struct ProbeCache {
    /// The last refresh, `None` until one lands.
    inner: Mutex<Option<Refreshed>>,
    /// The clients the saved settings build, which a read before the
    /// first refresh answers from.
    clients: Arc<LiveClients>,
}

impl ProbeCache {
    /// Cache over the live clients, with no refresh yet.
    pub fn new(clients: Arc<LiveClients>) -> Self {
        Self {
            inner: Mutex::new(None),
            clients,
        }
    }

    /// Current snapshot: the last refresh of the clients the saved
    /// settings build now, or until one lands, the enabled and configured
    /// flags of those settings. A save is seen at once.
    pub fn snapshot(&self) -> CachedProbes {
        let current = self.clients.current().probe_inputs.clone();
        let guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        match guard.as_ref() {
            Some(refreshed) if Arc::ptr_eq(&refreshed.inputs, &current) => refreshed.probes.clone(),
            _ => seed(&current),
        }
    }

    /// Replace the snapshot after a refresh pass over `inputs`.
    fn update(&self, inputs: &Arc<ProbeInputs>, probes: CachedProbes) {
        let next = Some(Refreshed {
            inputs: inputs.clone(),
            probes,
        });
        match self.inner.lock() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }
}

/// All five health traits over one shared cache.
#[derive(Clone)]
pub struct LiveProbes {
    cache: Arc<ProbeCache>,
}

impl LiveProbes {
    /// Probes over a shared cache.
    pub fn new(cache: Arc<ProbeCache>) -> Self {
        Self { cache }
    }

    /// Bundle for the imports deps. One struct serves all four traits.
    pub fn bundle(probes: &Arc<LiveProbes>) -> HealthProbes {
        HealthProbes {
            slskd: probes.clone(),
            sabnzbd: probes.clone(),
            newznab: probes.clone(),
            lidarr: probes.clone(),
        }
    }
}

impl SlskdProbe for LiveProbes {
    fn status(&self) -> ClientProbe {
        self.cache.snapshot().slskd
    }

    fn mount(&self) -> Option<SlskdMountReport> {
        self.cache.snapshot().slskd_mount
    }
}

impl SabnzbdProbe for LiveProbes {
    fn status(&self) -> ClientProbe {
        self.cache.snapshot().sabnzbd
    }

    fn categories(&self) -> Vec<String> {
        self.cache.snapshot().sabnzbd_categories
    }

    fn complete_dir(&self) -> Option<String> {
        self.cache.snapshot().sabnzbd_complete_dir
    }
}

impl NewznabProbe for LiveProbes {
    fn indexers(&self) -> Vec<IndexerProbe> {
        self.cache.snapshot().indexers
    }
}

impl LidarrReadinessProbe for LiveProbes {
    fn status(&self) -> ClientProbe {
        self.cache.snapshot().lidarr
    }
}

/// Everything one refresh pass needs: live clients plus config snapshots.
pub struct ProbeInputs {
    /// slskd repository, when the section configures one.
    pub slskd: Option<Arc<SlskdRepository<ReqwestSlskdHttp>>>,
    /// slskd master switch.
    pub slskd_enabled: bool,
    /// `SLSKD_DOWNLOADS_PATH`: the downloads mount.
    pub slskd_mount_base: PathBuf,
    /// The downloads subfolder inside the mount, from settings.
    pub slskd_subpath: String,
    /// Library root folders, to tell whether imports can rename.
    pub library_roots: Vec<PathBuf>,
    /// SABnzbd queue, when the section configures one.
    pub sabnzbd: Option<Arc<SabnzbdQueue>>,
    /// SABnzbd section, for the switch and mount-independent fields.
    pub sabnzbd_section: DownloadClients,
    /// Native indexer fan-out plus its config entries.
    pub newznab: Arc<NewznabIndexer>,
    /// Native config entries, for names and switches.
    pub newznab_entries: Vec<ConfigIndexer>,
    /// Prowlarr fan-out plus its connection.
    pub prowlarr: Arc<ProwlarrIndexer>,
    /// Prowlarr connection, for the switch.
    pub prowlarr_section: ProwlarrConnection,
    /// Lidarr client plus its saved connection.
    pub lidarr: LidarrClient,
    /// Lidarr saved connection (raw, with the real key).
    pub lidarr_section: LidarrImportConnection,
}

/// Recompute every verdict with live health checks and store the snapshot.
/// One erroring client never fails the pass: its verdict goes red while
/// the others still refresh.
pub async fn refresh_probes(cache: &ProbeCache, inputs: &Arc<ProbeInputs>) {
    let probes = CachedProbes {
        slskd: refresh_slskd(inputs).await,
        slskd_mount: Some(refresh_slskd_mount(inputs).await),
        sabnzbd: refresh_sabnzbd(inputs).await,
        sabnzbd_categories: refresh_sabnzbd_categories(inputs).await,
        sabnzbd_complete_dir: refresh_sabnzbd_complete_dir(inputs).await,
        indexers: refresh_indexers(inputs).await,
        lidarr: refresh_lidarr(inputs).await,
    };
    cache.update(inputs, probes);
}

/// Verdicts from the settings alone (no network): enabled and configured
/// read correctly while reachability waits for the first refresh pass.
fn seed(inputs: &ProbeInputs) -> CachedProbes {
    let slskd_configured = inputs.slskd.is_some();
    let (indexers, prowlarr, lidarr) = (
        &inputs.newznab_entries,
        &inputs.prowlarr_section,
        &inputs.lidarr_section,
    );
    let sab = &inputs.sabnzbd_section.sabnzbd;
    let sab_configured = !sab.url.is_empty() && !sab.api_key.expose().is_empty();
    let mut entries: Vec<IndexerProbe> = indexers
        .iter()
        .map(|entry| IndexerProbe {
            name: entry.name.clone(),
            enabled: entry.enabled,
            configured: !entry.url.is_empty() && !entry.api_key.expose().is_empty(),
            reachable: false,
        })
        .collect();
    entries.push(IndexerProbe {
        name: "prowlarr".to_owned(),
        enabled: prowlarr.enabled,
        configured: !prowlarr.url.is_empty() && !prowlarr.api_key.expose().is_empty(),
        reachable: false,
    });
    let lidarr_configured = !lidarr.url.is_empty() && !lidarr.api_key.expose().is_empty();
    CachedProbes {
        slskd: ClientProbe {
            enabled: inputs.slskd_enabled,
            configured: slskd_configured,
            reachable: false,
            version: None,
            message: if slskd_configured {
                "Probe pending".to_owned()
            } else {
                "Not configured".to_owned()
            },
        },
        sabnzbd: ClientProbe {
            enabled: sab.enabled,
            configured: sab_configured,
            reachable: false,
            version: None,
            message: if sab_configured {
                "Probe pending".to_owned()
            } else {
                "Not configured".to_owned()
            },
        },
        lidarr: ClientProbe {
            enabled: lidarr_configured,
            configured: lidarr_configured,
            reachable: false,
            version: None,
            message: if lidarr_configured {
                "Probe pending".to_owned()
            } else {
                "Not configured".to_owned()
            },
        },
        indexers: entries,
        ..CachedProbes::default()
    }
}

async fn refresh_slskd(inputs: &ProbeInputs) -> ClientProbe {
    let configured = inputs
        .slskd
        .as_ref()
        .is_some_and(|repo| repo.is_configured());
    if !configured {
        return ClientProbe {
            enabled: inputs.slskd_enabled,
            configured: false,
            reachable: false,
            version: None,
            message: "Not configured".to_owned(),
        };
    }
    let Some(repo) = &inputs.slskd else {
        return ClientProbe::default();
    };
    let verdict = repo.health_check().await;
    ClientProbe {
        enabled: inputs.slskd_enabled,
        configured: true,
        reachable: verdict.ok,
        version: verdict.version,
        message: verdict.message,
    }
}

/// Shown when the downloads subfolder is a link: nothing is read through
/// it, so the subfolder hint the other advisories give would mislead.
const SUBFOLDER_LINK_ADVISORY: &str = "The downloads subfolder is a link, and DroppedNeedle \
     never reads through one. Point the mount at slskd's folder and clear the subfolder, or \
     make the subfolder a real folder.";

/// Check the downloads mount and, when it looks usable and slskd is set
/// up, whether slskd's finished downloads can be found in it.
async fn refresh_slskd_mount(inputs: &ProbeInputs) -> SlskdMountReport {
    let base = inputs.slskd_mount_base.clone();
    let roots = inputs.library_roots.clone();
    let check = match tokio::task::spawn_blocking(move || {
        mount::check_downloads_mount(&base, &roots)
    })
    .await
    {
        Ok(check) => check,
        Err(error) => {
            tracing::warn!(%error, "downloads mount check did not finish; reporting it unchecked");
            mount::MountCheck {
                ok: false,
                move_supported: false,
                reason: mount::MountReason::StatError,
                path: inputs.slskd_mount_base.display().to_string(),
            }
        }
    };
    let effective = mount::effective_path(&inputs.slskd_mount_base, &inputs.slskd_subpath)
        .display()
        .to_string();
    let (advisory, client_downloads_dir) = match &inputs.slskd {
        Some(repo) if repo.is_configured() && check.ok => {
            let folder = repo.downloads_folder().clone();
            let linked = match tokio::task::spawn_blocking(move || {
                matches!(folder.resolve(), Err(FolderFault::Link))
            })
            .await
            {
                Ok(linked) => linked,
                Err(error) => {
                    tracing::warn!(%error, "downloads subfolder check did not finish");
                    false
                }
            };
            let diagnosis = repo.diagnose_downloads_mount().await;
            let advisory = if linked {
                Some(SUBFOLDER_LINK_ADVISORY.to_owned())
            } else {
                mount::advisory(&check, &diagnosis, &inputs.slskd_subpath, &effective)
            };
            (advisory, diagnosis.client_downloads_dir)
        }
        _ => (None, None),
    };
    SlskdMountReport {
        mount: check,
        advisory,
        client_downloads_dir,
        effective_path: effective,
    }
}

async fn refresh_sabnzbd(inputs: &ProbeInputs) -> ClientProbe {
    let enabled = inputs.sabnzbd_section.sabnzbd.enabled;
    let configured = inputs
        .sabnzbd
        .as_ref()
        .is_some_and(|queue| queue.is_configured());
    if !configured {
        return ClientProbe {
            enabled,
            configured: false,
            reachable: false,
            version: None,
            message: "Not configured".to_owned(),
        };
    }
    let Some(queue) = &inputs.sabnzbd else {
        return ClientProbe::default();
    };
    let verdict = queue.health_check().await;
    ClientProbe {
        enabled,
        configured: true,
        reachable: verdict.status == "ok",
        version: verdict.version,
        message: verdict.message,
    }
}

async fn refresh_sabnzbd_categories(inputs: &ProbeInputs) -> Vec<String> {
    match &inputs.sabnzbd {
        Some(queue) if queue.is_configured() => queue.get_categories().await.unwrap_or_default(),
        _ => Vec::new(),
    }
}

async fn refresh_sabnzbd_complete_dir(inputs: &ProbeInputs) -> Option<String> {
    match &inputs.sabnzbd {
        Some(queue) if queue.is_configured() => queue.get_complete_dir().await.ok(),
        _ => None,
    }
}

async fn refresh_indexers(inputs: &ProbeInputs) -> Vec<IndexerProbe> {
    let native_ok = if inputs.newznab.is_configured() {
        inputs.newznab.health_check().await.status == "ok"
    } else {
        false
    };
    let mut out: Vec<IndexerProbe> = inputs
        .newznab_entries
        .iter()
        .map(|entry| {
            let configured = !entry.url.is_empty() && !entry.api_key.expose().is_empty();
            IndexerProbe {
                name: entry.name.clone(),
                enabled: entry.enabled,
                configured,
                // The fan-out reports one aggregate verdict; per-indexer
                // reachability is a later refinement, so an enabled and
                // configured entry shares the aggregate answer.
                reachable: native_ok && entry.enabled && configured,
            }
        })
        .collect();
    let prowlarr_configured = inputs.prowlarr.is_configured();
    let prowlarr_ok = if prowlarr_configured {
        inputs.prowlarr.health_check().await.status == "ok"
    } else {
        false
    };
    out.push(IndexerProbe {
        name: "prowlarr".to_owned(),
        enabled: inputs.prowlarr_section.enabled,
        configured: prowlarr_configured,
        reachable: prowlarr_ok && inputs.prowlarr_section.enabled,
    });
    out
}

async fn refresh_lidarr(inputs: &ProbeInputs) -> ClientProbe {
    let section = &inputs.lidarr_section;
    let configured = !section.url.is_empty() && !section.api_key.expose().is_empty();
    if !configured {
        return ClientProbe {
            enabled: false,
            configured: false,
            reachable: false,
            version: None,
            message: "Not configured".to_owned(),
        };
    }
    // The import has no master switch of its own: configured means enabled.
    match inputs
        .lidarr
        .system_status(&section.url, section.api_key.expose())
        .await
    {
        Ok(status) => ClientProbe {
            enabled: true,
            configured: true,
            reachable: true,
            version: if status.version.is_empty() {
                None
            } else {
                Some(status.version)
            },
            message: "Connected".to_owned(),
        },
        Err(LidarrError::Auth) => ClientProbe {
            enabled: true,
            configured: true,
            reachable: false,
            version: None,
            message: "Lidarr rejected the stored API key; check Settings".to_owned(),
        },
        Err(LidarrError::Unavailable(detail)) => {
            tracing::warn!(%detail, "lidarr probe failed");
            ClientProbe {
                enabled: true,
                configured: true,
                reachable: false,
                version: None,
                message: "Couldn't reach Lidarr. Check the URL and that Lidarr is running."
                    .to_owned(),
            }
        }
    }
}

/// Health-probe refresh cadence.
const PROBE_INTERVAL: Duration = Duration::from_secs(300);

/// Job name of the probe refresh loop.
pub const PROBE_JOB: &str = "acquire-probe-refresh";

/// Probe refresh: one pass now, then one on every wake and at least
/// every five minutes, each over the clients the current settings
/// build. A pass waiting on a slow client ends at the stop; it only
/// reads, and the cache keeps the last pass.
pub(crate) fn spawn_probe_loop(
    supervisor: &Supervisor,
    cache: Arc<ProbeCache>,
    clients: Arc<LiveClients>,
    wake: &watch::Sender<u64>,
) -> Result<(), AlreadyRunning> {
    let mut wake = wake.subscribe();
    wake.mark_changed();
    supervisor.on_wakeup(
        PROBE_JOB,
        wake,
        Schedule::new(PROBE_INTERVAL),
        FnTick(move |ctx: TickCtx| {
            let cache = cache.clone();
            let inputs = clients.current().probe_inputs.clone();
            async move {
                tokio::select! {
                    () = ctx.token().cancelled() => {}
                    () = refresh_probes(&cache, &inputs) => {}
                }
                Ok(Next::Continue)
            }
        }),
    )
}
