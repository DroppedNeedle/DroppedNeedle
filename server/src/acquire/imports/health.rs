//! Acquisition health smoke and per-source release gates.
//!
//! Ports the v2 download-status halves (`backend/api/v1/routes/
//! download_client.py` `/status`, `download_clients.py` `/sabnzbd/status`,
//! `status.py` + `status_service.py`): saved-config probes whose verdicts
//! travel in the body, never as leaked 5xx. The smoke answers one question:
//! Free OR slskd OR Usenet readiness. The four release gates (slskd,
//! SABnzbd, Newznab, Lidarr import) each gate independently: one red gate
//! never flips another.
//!
//! SEAM: every probe is a minimal local trait. The downloads slice owns the
//! slskd/SABnzbd clients, the indexers slice owns Newznab, and Free Music
//! readiness reads the policy slice; each swaps its scripted probe below
//! for the live one without touching the handlers.

use std::sync::Mutex;

use super::models::{AcquireHealth, SabnzbdStatusResponse, SlskdStatusResponse, SourceGate};

/// Live verdict from one client probe.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientProbe {
    /// Admin master switch.
    pub enabled: bool,
    /// Credentials/URL present.
    pub configured: bool,
    /// Live probe answered.
    pub reachable: bool,
    /// Version, when the probe reached it.
    pub version: Option<String>,
    /// User-safe summary.
    pub message: String,
}

/// One Newznab indexer verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexerProbe {
    /// Indexer name.
    pub name: String,
    /// Admin switch for this indexer.
    pub enabled: bool,
    /// Key/URL present.
    pub configured: bool,
    /// Live probe answered.
    pub reachable: bool,
}

/// Free Music readiness (v2 `FreeMusicSettings`: enabled by default,
/// lawful-licensed Internet Archive downloads, no signup).
#[derive(Debug, Clone, PartialEq)]
pub struct FreeReadiness {
    /// Master switch.
    pub enabled: bool,
    /// Preferred format label (`flac` or `mp3`).
    pub preferred_format: String,
}

/// slskd client probe (v2 `DownloadClientRepository` health half). SEAM:
/// the downloads slice owns the live client.
pub trait SlskdProbe: Send + Sync {
    /// Probe the saved slskd config.
    fn status(&self) -> ClientProbe;
}

/// SABnzbd client probe (v2 `SabnzbdDownloadClient` health half). SEAM:
/// the downloads slice owns the live client.
pub trait SabnzbdProbe: Send + Sync {
    /// Probe the saved SABnzbd config.
    fn status(&self) -> ClientProbe;
    /// Category list, when the probe reached the client.
    fn categories(&self) -> Vec<String>;
    /// Completed dir (the mount hint), when the probe reached it.
    fn complete_dir(&self) -> Option<String>;
}

/// Newznab indexer probes (v2 indexer health half). SEAM: the indexers
/// slice owns the live probes.
pub trait NewznabProbe: Send + Sync {
    /// Probe every configured indexer.
    fn indexers(&self) -> Vec<IndexerProbe>;
}

/// Lidarr import readiness probe, backed by the slice's own settings plus
/// a `system/status` reachability check.
pub trait LidarrReadinessProbe: Send + Sync {
    /// Probe the saved Lidarr import connection.
    fn status(&self) -> ClientProbe;
}

/// Free Music readiness (v2 `FreeMusicSettings` + lawful-source guard).
/// SEAM: the policy slice owns the live settings read.
pub trait FreeMusicProbe: Send + Sync {
    /// Current Free Music readiness.
    fn readiness(&self) -> FreeReadiness;
}

/// Scripted slskd probe for briefs and the pre-client tier.
#[derive(Debug, Default)]
pub struct ScriptedSlskd {
    inner: Mutex<ClientProbe>,
}

/// Scripted SABnzbd probe.
#[derive(Debug, Default)]
pub struct ScriptedSabnzbd {
    inner: Mutex<ScriptedSabnzbdState>,
}

/// Scripted SABnzbd state.
#[derive(Debug, Default)]
struct ScriptedSabnzbdState {
    status: ClientProbe,
    categories: Vec<String>,
    complete_dir: Option<String>,
}

/// Scripted Newznab probes.
#[derive(Debug, Default)]
pub struct ScriptedNewznab {
    inner: Mutex<Vec<IndexerProbe>>,
}

/// Scripted Lidarr readiness.
#[derive(Debug, Default)]
pub struct ScriptedLidarr {
    inner: Mutex<ClientProbe>,
}

/// Scripted Free Music readiness.
#[derive(Debug, Default)]
pub struct ScriptedFree {
    inner: Mutex<FreeReadiness>,
}

impl Default for ClientProbe {
    fn default() -> Self {
        Self {
            enabled: false,
            configured: false,
            reachable: false,
            version: None,
            message: "Not configured".to_owned(),
        }
    }
}

impl Default for FreeReadiness {
    fn default() -> Self {
        Self {
            enabled: true,
            preferred_format: "flac".to_owned(),
        }
    }
}

impl ScriptedSlskd {
    /// Unconfigured probe.
    pub fn new() -> Self {
        Self::default()
    }

    /// Script one verdict.
    pub fn set(&self, status: ClientProbe) {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
    }
}

impl SlskdProbe for ScriptedSlskd {
    fn status(&self) -> ClientProbe {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl ScriptedSabnzbd {
    /// Unconfigured probe.
    pub fn new() -> Self {
        Self::default()
    }

    /// Script one verdict with categories and completed dir.
    pub fn set(&self, status: ClientProbe, categories: Vec<String>, complete_dir: Option<String>) {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = ScriptedSabnzbdState {
            status,
            categories,
            complete_dir,
        };
    }
}

impl SabnzbdProbe for ScriptedSabnzbd {
    fn status(&self) -> ClientProbe {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .status
            .clone()
    }

    fn categories(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .categories
            .clone()
    }

    fn complete_dir(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .complete_dir
            .clone()
    }
}

impl ScriptedNewznab {
    /// No indexers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the indexer list.
    pub fn set(&self, indexers: Vec<IndexerProbe>) {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = indexers;
    }
}

impl NewznabProbe for ScriptedNewznab {
    fn indexers(&self) -> Vec<IndexerProbe> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl ScriptedLidarr {
    /// Unconfigured probe.
    pub fn new() -> Self {
        Self::default()
    }

    /// Script one verdict.
    pub fn set(&self, status: ClientProbe) {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
    }
}

impl LidarrReadinessProbe for ScriptedLidarr {
    fn status(&self) -> ClientProbe {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl ScriptedFree {
    /// Default-on Free Music.
    pub fn new() -> Self {
        Self::default()
    }

    /// Script one readiness.
    pub fn set(&self, readiness: FreeReadiness) {
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = readiness;
    }
}

impl FreeMusicProbe for ScriptedFree {
    fn readiness(&self) -> FreeReadiness {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Every probe the smoke reads, injected by constructor.
pub struct HealthProbes {
    /// slskd client probe.
    pub slskd: std::sync::Arc<dyn SlskdProbe>,
    /// SABnzbd client probe.
    pub sabnzbd: std::sync::Arc<dyn SabnzbdProbe>,
    /// Newznab indexer probes.
    pub newznab: std::sync::Arc<dyn NewznabProbe>,
    /// Lidarr import readiness.
    pub lidarr: std::sync::Arc<dyn LidarrReadinessProbe>,
    /// Free Music readiness.
    pub free: std::sync::Arc<dyn FreeMusicProbe>,
}

/// Build the acquisition health smoke: `ready` is Free OR slskd OR Usenet
/// readiness, and each release gate reports independently. Usenet readiness
/// means the SABnzbd client probes clean; Newznab indexers gate Usenet
/// *search* separately, so a bare client still counts the path ready while
/// the Newznab gate stays red on its own.
pub fn smoke(probes: &HealthProbes) -> AcquireHealth {
    let slskd = probes.slskd.status();
    let sabnzbd = probes.sabnzbd.status();
    let indexers = probes.newznab.indexers();
    let lidarr = probes.lidarr.status();
    let free = probes.free.readiness();

    let free_ready = free.enabled;
    let slskd_ready = slskd.enabled && slskd.configured && slskd.reachable;
    let usenet_ready = sabnzbd.enabled && sabnzbd.configured && sabnzbd.reachable;

    let mut ready_via = Vec::new();
    if free_ready {
        ready_via.push("free".to_owned());
    }
    if slskd_ready {
        ready_via.push("slskd".to_owned());
    }
    if usenet_ready {
        ready_via.push("usenet".to_owned());
    }
    let ready = !ready_via.is_empty();

    let newznab_open = indexers
        .iter()
        .any(|indexer| indexer.enabled && indexer.configured && indexer.reachable);
    let newznab_message = if indexers.is_empty() {
        "No indexers configured".to_owned()
    } else if newznab_open {
        let open = indexers
            .iter()
            .filter(|indexer| indexer.enabled && indexer.configured && indexer.reachable)
            .count();
        format!("{open}/{} indexer(s) reachable", indexers.len())
    } else {
        "No indexer reachable".to_owned()
    };

    let gates = vec![
        SourceGate {
            source: "slskd".to_owned(),
            enabled: slskd.enabled,
            configured: slskd.configured,
            reachable: slskd.reachable,
            open: slskd_ready,
            message: slskd.message,
        },
        SourceGate {
            source: "sabnzbd".to_owned(),
            enabled: sabnzbd.enabled,
            configured: sabnzbd.configured,
            reachable: sabnzbd.reachable,
            open: usenet_ready,
            message: sabnzbd.message,
        },
        SourceGate {
            source: "newznab".to_owned(),
            enabled: indexers.iter().any(|indexer| indexer.enabled),
            configured: indexers.iter().any(|indexer| indexer.configured),
            reachable: indexers.iter().any(|indexer| indexer.reachable),
            open: newznab_open,
            message: newznab_message,
        },
        SourceGate {
            source: "lidarr_import".to_owned(),
            enabled: lidarr.enabled,
            configured: lidarr.configured,
            reachable: lidarr.reachable,
            open: lidarr.enabled && lidarr.configured && lidarr.reachable,
            message: lidarr.message,
        },
    ];

    // Overall verdict mirrors v2 `StatusService`: error when nothing can
    // serve, degraded when some path is down but one serves, ok when every
    // enabled path is up (a fully-disabled install still errors: ready is
    // false with no path to serve).
    let status = if !ready {
        "error"
    } else if gates.iter().any(|gate| gate.enabled && !gate.open) {
        "degraded"
    } else {
        "ok"
    }
    .to_owned();

    AcquireHealth {
        status,
        ready,
        ready_via,
        gates,
    }
}

/// Render the live slskd client status (v2 `/download-client/status`
/// client half; any authenticated user may read it).
pub fn slskd_status(probe: &dyn SlskdProbe) -> SlskdStatusResponse {
    let status = probe.status();
    SlskdStatusResponse {
        configured: status.configured,
        reachable: status.reachable,
        version: status.version,
        message: status.message,
    }
}

/// Render the live SABnzbd status against the saved config (v2
/// `/download-clients/sabnzbd/status`; admin-only there, admin-only here).
pub fn sabnzbd_status(probe: &dyn SabnzbdProbe) -> SabnzbdStatusResponse {
    let status = probe.status();
    if !status.configured {
        return SabnzbdStatusResponse {
            valid: false,
            version: None,
            message: "Not configured".to_owned(),
            categories: Vec::new(),
            complete_dir: None,
        };
    }
    if !(status.enabled && status.reachable) {
        return SabnzbdStatusResponse {
            valid: false,
            version: status.version,
            message: status.message,
            categories: Vec::new(),
            complete_dir: None,
        };
    }
    SabnzbdStatusResponse {
        valid: true,
        version: status.version,
        message: status.message,
        categories: probe.categories(),
        complete_dir: probe.complete_dir(),
    }
}
