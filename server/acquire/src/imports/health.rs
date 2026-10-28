//! Acquisition health smoke and per-source release gates.
//!
//! The download-client status reads are saved-config probes whose
//! verdicts travel in the body, never as leaked 5xx. The smoke answers one
//! question, whether music can be acquired, from [`crate::readiness`], the
//! answer requests and Discover read too. The four release gates (slskd,
//! SABnzbd, Newznab, Lidarr import) each gate independently: one red gate never
//! flips another.
//!
//! Every probe is a minimal local trait. The live implementations sit over
//! the slskd and SABnzbd clients and the Newznab indexers
//! (`crate::probes`).

use super::models::{
    AcquireHealth, DownloadsMountView, SabnzbdStatusResponse, SlskdStatusResponse, SourceGate,
};
use super::mount::SlskdMountReport;
use crate::readiness::Readiness;

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

/// slskd client probe.
pub trait SlskdProbe: Send + Sync {
    /// Probe the saved slskd config.
    fn status(&self) -> ClientProbe;
    /// The downloads mount report, once a probe pass made one.
    fn mount(&self) -> Option<SlskdMountReport> {
        None
    }
}

/// SABnzbd client probe.
pub trait SabnzbdProbe: Send + Sync {
    /// Probe the saved SABnzbd config.
    fn status(&self) -> ClientProbe;
    /// Category list, when the probe reached the client.
    fn categories(&self) -> Vec<String>;
    /// Completed dir (the mount hint), when the probe reached it.
    fn complete_dir(&self) -> Option<String>;
}

/// Newznab indexer probes.
pub trait NewznabProbe: Send + Sync {
    /// Probe every configured indexer.
    fn indexers(&self) -> Vec<IndexerProbe>;
}

/// Lidarr import readiness probe, backed by the import settings plus
/// a `system/status` reachability check.
pub trait LidarrReadinessProbe: Send + Sync {
    /// Probe the saved Lidarr import connection.
    fn status(&self) -> ClientProbe;
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
}

/// Build the acquisition health smoke. `ready` is whether a download
/// source is on (slskd, SABnzbd or a plugin), `ready_via` the ones the last
/// probe reached, and each release gate reports independently. Newznab
/// indexers gate Usenet *search* separately, so a bare client still counts
/// the path ready while the Newznab gate stays red on its own.
pub fn smoke(probes: &HealthProbes, readiness: &Readiness) -> AcquireHealth {
    let slskd = probes.slskd.status();
    let sabnzbd = probes.sabnzbd.status();
    let indexers = probes.newznab.indexers();
    let lidarr = probes.lidarr.status();

    let slskd_ready = slskd.enabled && slskd.configured && slskd.reachable;
    let usenet_ready = sabnzbd.enabled && sabnzbd.configured && sabnzbd.reachable;
    let ready = readiness.can_acquire();
    let ready_via = readiness
        .reachable
        .iter()
        .map(ToString::to_string)
        .collect();

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

    // Error when no source is on, degraded when an enabled path is down,
    // ok when every enabled path is up.
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

/// Render the live slskd client status (any authenticated user may read
/// it).
pub fn slskd_status(probe: &dyn SlskdProbe) -> SlskdStatusResponse {
    let status = probe.status();
    let report = probe.mount();
    SlskdStatusResponse {
        configured: status.configured,
        reachable: status.reachable,
        version: status.version,
        message: status.message,
        mount: report.as_ref().map(|report| DownloadsMountView {
            ok: report.mount.ok,
            move_supported: report.mount.move_supported,
            reason: report.mount.reason.as_str().to_owned(),
            path: report.mount.path.clone(),
        }),
        mount_advisory: report.as_ref().and_then(|r| r.advisory.clone()),
        slskd_downloads_dir: report.as_ref().and_then(|r| r.client_downloads_dir.clone()),
        effective_downloads_path: report.map(|r| r.effective_path),
    }
}

/// Render the live SABnzbd status against the saved config (admin only).
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
