//! Download client status for the settings page.
//!
//! The status reads are saved-config probes whose verdicts travel in the
//! body, never as leaked 5xx. Every probe is a minimal local trait, so a
//! status read stays a sync read of the cached verdict. The live
//! implementations sit over the slskd, SABnzbd, NZBGet and Deemix clients
//! (`crate::probes`).

use serde::Serialize;
use utoipa::ToSchema;

use super::models::{DownloadsMountView, SlskdStatusResponse, UsenetStatusResponse};
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

/// Usenet client probe (SABnzbd or NZBGet).
pub trait UsenetProbe: Send + Sync {
    /// Probe the saved client config.
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
    pub sabnzbd: std::sync::Arc<dyn UsenetProbe>,
    /// NZBGet client probe.
    pub nzbget: std::sync::Arc<dyn UsenetProbe>,
    /// Deemix probe.
    pub deemix: std::sync::Arc<dyn DeemixProbe>,
}

/// Deemix probe.
pub trait DeemixProbe: Send + Sync {
    /// The last check of the saved Deemix config.
    fn status(&self) -> DeemixStatusResponse;
}

/// What Deemix answered a check with, for the settings card. Every value is
/// DroppedNeedle's own words; nothing Deemix wrote is passed on but its
/// version numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct DeemixStatusResponse {
    /// Admin master switch.
    pub enabled: bool,
    /// A URL is set.
    pub configured: bool,
    /// Deemix answered.
    pub reachable: bool,
    /// Deemix's web UI version, when it answered.
    pub webui_version: Option<String>,
    /// Deemix's library version, when it answered.
    pub deemix_version: Option<String>,
    /// Deemix can reach Deezer.
    pub deezer_reachable: bool,
    /// DroppedNeedle's session with Deemix is signed in to Deezer.
    pub signed_in: bool,
    /// Not signed in, and Deemix offers the Deezer sign-in it saved, which
    /// DroppedNeedle uses when it needs to.
    pub sign_in_offered: bool,
    /// The best quality the Deezer account downloads; null while not
    /// signed in.
    pub account_quality: Option<DeemixQuality>,
    /// The best quality the account gives that the quality settings take;
    /// null while not signed in, while those settings cannot be read, or
    /// when they take none of it.
    pub best_quality: Option<DeemixQuality>,
    /// What to change, in a fixed order.
    pub warnings: Vec<DeemixWarning>,
    /// User-safe summary.
    pub message: String,
}

/// A quality Deezer serves through Deemix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeemixQuality {
    /// FLAC, 16-bit 44.1 kHz.
    Flac,
    /// MP3 at 320 kbps.
    #[serde(rename = "mp3_320")]
    Mp3At320,
    /// MP3 at 128 kbps.
    #[serde(rename = "mp3_128")]
    Mp3At128,
}

/// Something about Deemix or its folder that keeps downloads from working
/// as they should.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeemixWarning {
    /// Deemix's "Overwrite files" is not "Yes".
    OverwriteOff,
    /// One of Deemix's fallback options is on, so it may lower quality.
    FallbackOn,
    /// Not signed in, and Deemix runs in multi-user mode, where it keeps
    /// no Deezer sign-in to offer.
    SingleUserOff,
    /// Deemix runs a command after downloads, which may move the files.
    CommandSet,
    /// The downloads folder is missing or not writable.
    MountUnusable,
    /// The downloads folder is, holds or sits inside a library, recycle
    /// bin, import, app or other download folder.
    MountOverlaps,
    /// A file Deemix says it downloaded lies outside its own download
    /// folder, or is there but opens only through a link or outside the
    /// downloads folder.
    MappingWrong,
    /// The account's quality is known and the quality settings take none
    /// of it.
    QualityNotTaken,
}

impl Default for DeemixStatusResponse {
    fn default() -> Self {
        Self {
            enabled: false,
            configured: false,
            reachable: false,
            webui_version: None,
            deemix_version: None,
            deezer_reachable: false,
            signed_in: false,
            sign_in_offered: false,
            account_quality: None,
            best_quality: None,
            warnings: Vec::new(),
            message: "Not configured".to_owned(),
        }
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
            reason: report.mount.reason,
            path: report.mount.path.clone(),
        }),
        mount_advisory: report.as_ref().and_then(|r| r.advisory.clone()),
        slskd_downloads_dir: report.as_ref().and_then(|r| r.client_downloads_dir.clone()),
        effective_downloads_path: report.map(|r| r.effective_path),
    }
}

/// Render a Usenet client's live status against the saved config (admin
/// only).
pub fn usenet_status(probe: &dyn UsenetProbe) -> UsenetStatusResponse {
    let status = probe.status();
    if !status.configured {
        return UsenetStatusResponse {
            valid: false,
            version: None,
            message: "Not configured".to_owned(),
            categories: Vec::new(),
            complete_dir: None,
        };
    }
    if !(status.enabled && status.reachable) {
        return UsenetStatusResponse {
            valid: false,
            version: status.version,
            message: status.message,
            categories: Vec::new(),
            complete_dir: None,
        };
    }
    UsenetStatusResponse {
        valid: true,
        version: status.version,
        message: status.message,
        categories: probe.categories(),
        complete_dir: probe.complete_dir(),
    }
}
