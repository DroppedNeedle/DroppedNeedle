//! Live acquisition-health probes over config snapshots plus cached
//! verdicts.
//!
//! The imports health traits are sync, but reachability needs HTTP. The
//! split: [`LiveProbes`] implements all five traits over a [`ProbeCache`]
//! snapshot, and [`refresh_probes`] recomputes the snapshot with real
//! client health checks. Wiring runs one refresh at boot and repeats it on
//! a slow loop, so the smoke and the status routes always render a fresh
//! verdict without blocking a request on five network calls.

use std::sync::{Arc, Mutex};

use super::imports::health::{
    ClientProbe, FreeMusicProbe, FreeReadiness, HealthProbes, IndexerProbe, LidarrReadinessProbe,
    NewznabProbe, SabnzbdProbe, SlskdProbe,
};
use super::imports::lidarr::{LidarrClient, LidarrError};
use super::slskd::{ReqwestSlskdHttp, SlskdRepository};
use super::usenet::newznab::NewznabIndexer;
use super::usenet::prowlarr::ProwlarrIndexer;
use super::usenet::sabnzbd::SabnzbdQueue;
use crate::runtime_config::secret_sections::{
    DownloadClients, LidarrImportConnection, NewznabIndexer as ConfigIndexer, ProwlarrConnection,
    SlskdConnection,
};
use crate::runtime_config::sections::{AudioFormat, FreeMusic};

/// One cached verdict per probe.
#[derive(Debug, Clone, Default)]
pub struct CachedProbes {
    /// slskd client verdict.
    pub slskd: ClientProbe,
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
    /// Free Music readiness.
    pub free: FreeReadiness,
}

/// Shared verdict snapshot behind the live probes.
#[derive(Debug, Default)]
pub struct ProbeCache {
    inner: Mutex<CachedProbes>,
}

impl ProbeCache {
    /// Cache over one snapshot (wiring seeds config-derived fields so the
    /// first smoke already reports enabled/configured honestly).
    pub fn new(initial: CachedProbes) -> Self {
        Self {
            inner: Mutex::new(initial),
        }
    }

    /// Current snapshot.
    pub fn snapshot(&self) -> CachedProbes {
        self.inner
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Replace the snapshot after a refresh pass.
    pub fn update(&self, next: CachedProbes) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = next;
        }
    }
}

/// All five health traits over one shared cache.
#[derive(Debug, Clone)]
pub struct LiveProbes {
    cache: Arc<ProbeCache>,
}

impl LiveProbes {
    /// Probes over a shared cache.
    pub fn new(cache: Arc<ProbeCache>) -> Self {
        Self { cache }
    }

    /// Bundle for the imports deps. One struct serves all five traits.
    pub fn bundle(probes: &Arc<LiveProbes>) -> HealthProbes {
        HealthProbes {
            slskd: probes.clone(),
            sabnzbd: probes.clone(),
            newznab: probes.clone(),
            lidarr: probes.clone(),
            free: probes.clone(),
        }
    }
}

impl SlskdProbe for LiveProbes {
    fn status(&self) -> ClientProbe {
        self.cache.snapshot().slskd
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

impl FreeMusicProbe for LiveProbes {
    fn readiness(&self) -> FreeReadiness {
        self.cache.snapshot().free
    }
}

/// Everything one refresh pass needs: live clients plus config snapshots.
pub struct ProbeInputs {
    /// slskd repository, when the section configures one.
    pub slskd: Option<Arc<SlskdRepository<ReqwestSlskdHttp>>>,
    /// slskd master switch.
    pub slskd_enabled: bool,
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
    /// Free Music settings.
    pub free: FreeMusic,
}

/// Recompute every verdict with live health checks and store the snapshot.
/// One erroring client never fails the pass: its verdict goes red while
/// the others still refresh.
pub async fn refresh_probes(cache: &ProbeCache, inputs: &ProbeInputs) {
    cache.update(CachedProbes {
        slskd: refresh_slskd(inputs).await,
        sabnzbd: refresh_sabnzbd(inputs).await,
        sabnzbd_categories: refresh_sabnzbd_categories(inputs).await,
        sabnzbd_complete_dir: refresh_sabnzbd_complete_dir(inputs).await,
        indexers: refresh_indexers(inputs).await,
        lidarr: refresh_lidarr(inputs).await,
        free: FreeReadiness {
            enabled: inputs.free.enabled,
            preferred_format: match inputs.free.preferred_format {
                AudioFormat::Mp3 => "mp3".to_owned(),
                AudioFormat::Flac | AudioFormat::Opus => "flac".to_owned(),
            },
        },
    });
}

/// Seed snapshot from config alone (no network): enabled/configured read
/// honestly while reachability waits for the first refresh pass.
pub fn seed_from_config(
    slskd: &SlskdConnection,
    sabnzbd: &DownloadClients,
    indexers: &[ConfigIndexer],
    prowlarr: &ProwlarrConnection,
    lidarr: &LidarrImportConnection,
    free: &FreeMusic,
) -> CachedProbes {
    let slskd_configured = !slskd.url.is_empty() && !slskd.api_key.expose().is_empty();
    let sab = &sabnzbd.sabnzbd;
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
            enabled: slskd.enabled,
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
        free: FreeReadiness {
            enabled: free.enabled,
            preferred_format: match free.preferred_format {
                AudioFormat::Mp3 => "mp3".to_owned(),
                AudioFormat::Flac | AudioFormat::Opus => "flac".to_owned(),
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
