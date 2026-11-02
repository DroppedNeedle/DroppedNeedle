//! Download client status for the settings page.
//!
//! The status reads are saved-config probes whose verdicts travel in the
//! body, never as leaked 5xx. Every probe is a minimal local trait. The live
//! implementations sit over the slskd and SABnzbd clients
//! (`crate::probes`).

use super::models::{DownloadsMountView, SabnzbdStatusResponse, SlskdStatusResponse};
use super::mount::SlskdMountReport;

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

/// The probes behind the status reads, injected by constructor.
pub struct HealthProbes {
    /// slskd client probe.
    pub slskd: std::sync::Arc<dyn SlskdProbe>,
    /// SABnzbd client probe.
    pub sabnzbd: std::sync::Arc<dyn SabnzbdProbe>,
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
            reason: report.mount.reason,
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
