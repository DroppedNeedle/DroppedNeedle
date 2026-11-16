//! Live acquisition-health probes over config snapshots plus cached
//! verdicts.
//!
//! The imports health traits are sync, but reachability needs HTTP. The
//! split: [`LiveProbes`] implements both traits over a [`ProbeCache`]
//! snapshot, and [`refresh_probes`] recomputes the snapshot with real
//! client health checks. Wiring runs one refresh at boot, again after a
//! client's settings are saved, and on a slow loop, so the
//! status routes always render a fresh verdict without blocking a request
//! on five network calls. A verdict is kept with the client set it probed:
//! until a refresh lands for the current one, a read answers from the
//! saved settings alone.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;

use super::clients::LiveClients;
use super::imports::health::{ClientProbe, HealthProbes, SlskdProbe, UsenetProbe};
use super::imports::mount::{self, SlskdMountReport};
use super::landing::confine::FolderFault;
use super::slskd::{ReqwestSlskdHttp, SlskdRepository};
use super::usenet::nzbget::NzbgetQueue;
use super::usenet::queue::UsenetQueue;
use super::usenet::sabnzbd::SabnzbdQueue;
use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor, TickCtx};
use droppedneedle_kernel::runtime_config::secret_sections::DownloadClients;

/// One cached verdict per probe.
#[derive(Debug, Clone, Default)]
pub struct CachedProbes {
    /// slskd client verdict.
    pub slskd: ClientProbe,
    /// slskd downloads mount report.
    pub slskd_mount: Option<SlskdMountReport>,
    /// SABnzbd verdict.
    pub sabnzbd: UsenetVerdict,
    /// NZBGet verdict.
    pub nzbget: UsenetVerdict,
}

/// One Usenet client's cached verdict.
#[derive(Debug, Clone, Default)]
pub struct UsenetVerdict {
    /// Client verdict.
    pub status: ClientProbe,
    /// Categories, when the probe reached the client.
    pub categories: Vec<String>,
    /// Complete dir, when the probe reached it.
    pub complete_dir: Option<String>,
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

    /// Bundle for the imports deps. One struct serves both traits.
    pub fn bundle(probes: &Arc<LiveProbes>) -> HealthProbes {
        HealthProbes {
            slskd: probes.clone(),
            sabnzbd: Arc::new(LiveUsenetProbe {
                cache: probes.cache.clone(),
                read: |probes| probes.sabnzbd,
            }),
            nzbget: Arc::new(LiveUsenetProbe {
                cache: probes.cache.clone(),
                read: |probes| probes.nzbget,
            }),
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

/// One Usenet client's probe over the shared cache.
struct LiveUsenetProbe {
    cache: Arc<ProbeCache>,
    read: fn(CachedProbes) -> UsenetVerdict,
}

impl UsenetProbe for LiveUsenetProbe {
    fn status(&self) -> ClientProbe {
        (self.read)(self.cache.snapshot()).status
    }

    fn categories(&self) -> Vec<String> {
        (self.read)(self.cache.snapshot()).categories
    }

    fn complete_dir(&self) -> Option<String> {
        (self.read)(self.cache.snapshot()).complete_dir
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
    /// NZBGet queue, when the section configures one.
    pub nzbget: Option<Arc<NzbgetQueue>>,
    /// Usenet clients section, for the switches and mount-independent
    /// fields.
    pub sabnzbd_section: DownloadClients,
}

/// Recompute every verdict with live health checks and store the snapshot.
/// One erroring client never fails the pass: its verdict goes red while
/// the others still refresh.
pub async fn refresh_probes(cache: &ProbeCache, inputs: &Arc<ProbeInputs>) {
    let probes = CachedProbes {
        slskd: refresh_slskd(inputs).await,
        slskd_mount: Some(refresh_slskd_mount(inputs).await),
        sabnzbd: match inputs.sabnzbd.clone() {
            Some(queue) => {
                let enabled = inputs.sabnzbd_section.sabnzbd.enabled;
                refresh_usenet(enabled, UsenetQueue::Sabnzbd(queue)).await
            }
            None => unconfigured_usenet(inputs.sabnzbd_section.sabnzbd.enabled),
        },
        nzbget: match inputs.nzbget.clone() {
            Some(queue) => {
                let enabled = inputs.sabnzbd_section.nzbget.enabled;
                refresh_usenet(enabled, UsenetQueue::Nzbget(queue)).await
            }
            None => unconfigured_usenet(inputs.sabnzbd_section.nzbget.enabled),
        },
    };
    cache.update(inputs, probes);
}

/// Verdicts from the settings alone (no network): enabled and configured
/// read correctly while reachability waits for the first refresh pass.
fn seed(inputs: &ProbeInputs) -> CachedProbes {
    let slskd_configured = inputs.slskd.is_some();
    let sab = &inputs.sabnzbd_section.sabnzbd;
    let sab_configured = !sab.url.is_empty() && !sab.api_key.expose().is_empty();
    let nzbget = &inputs.sabnzbd_section.nzbget;
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
        sabnzbd: UsenetVerdict {
            status: pending(sab.enabled, sab_configured),
            ..UsenetVerdict::default()
        },
        nzbget: UsenetVerdict {
            status: pending(nzbget.enabled, !nzbget.url.is_empty()),
            ..UsenetVerdict::default()
        },
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

/// A verdict from the settings alone, before the first probe.
fn pending(enabled: bool, configured: bool) -> ClientProbe {
    ClientProbe {
        enabled,
        configured,
        reachable: false,
        version: None,
        message: if configured {
            "Probe pending".to_owned()
        } else {
            "Not configured".to_owned()
        },
    }
}

/// The verdict for a Usenet client the section does not set up.
fn unconfigured_usenet(enabled: bool) -> UsenetVerdict {
    UsenetVerdict {
        status: pending(enabled, false),
        ..UsenetVerdict::default()
    }
}

async fn refresh_usenet(enabled: bool, queue: UsenetQueue) -> UsenetVerdict {
    if !queue.is_configured() {
        return unconfigured_usenet(enabled);
    }
    let verdict = queue.health_check().await;
    UsenetVerdict {
        status: ClientProbe {
            enabled,
            configured: true,
            reachable: verdict.status == "ok",
            version: verdict.version,
            message: verdict.message,
        },
        categories: queue.get_categories().await.unwrap_or_default(),
        complete_dir: queue.get_complete_dir().await.ok(),
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
