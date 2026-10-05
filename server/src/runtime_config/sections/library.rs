//! Library scanning: the scan schedule, the filesystem watcher, and the
//! default naming template.

use serde::{Deserialize, Serialize};

use super::{Section, check_range, is_valid_hhmm};
use crate::runtime_config::error::ConfigError;

/// v2 default naming template (kept in sync with the publisher).
pub const DEFAULT_NAMING_TEMPLATE: &str =
    "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}";

// --- library_scan_schedule (the v2 sync section is dropped, this one kept) -

/// Automatic-scan cadence values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ScanFrequency {
    /// Never scan automatically.
    Manual,
    /// Every 5 minutes.
    #[serde(rename = "5min")]
    Min5,
    /// Every 10 minutes.
    #[serde(rename = "10min")]
    Min10,
    /// Every 30 minutes.
    #[serde(rename = "30min")]
    Min30,
    /// Every hour.
    #[serde(rename = "1hr")]
    Hr1,
    /// Every 6 hours.
    #[serde(rename = "6hr")]
    Hr6,
    /// Every 12 hours.
    #[serde(rename = "12hr")]
    Hr12,
    /// Every 24 hours (rolling gap).
    #[serde(rename = "24hr")]
    #[default]
    Hr24,
    /// Every 3 days.
    #[serde(rename = "3d")]
    Days3,
    /// Every 7 days.
    #[serde(rename = "7d")]
    Days7,
    /// Once a day at `daily_scan_time`.
    #[serde(rename = "daily")]
    Daily,
}

/// Native automatic-scan schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryScanSchedule {
    /// Rolling-gap cadence, or `daily` for a fixed clock time.
    pub scan_frequency: ScanFrequency,
    /// Server-local `HH:MM` used when `scan_frequency` is `daily`.
    pub daily_scan_time: String,
    /// Unix time of the last scan, if any.
    pub last_scan: Option<i64>,
    /// Whether the last scan succeeded.
    pub last_scan_success: bool,
}

impl Default for LibraryScanSchedule {
    fn default() -> Self {
        Self {
            scan_frequency: ScanFrequency::Hr24,
            daily_scan_time: "03:00".to_owned(),
            last_scan: None,
            last_scan_success: true,
        }
    }
}

impl Section for LibraryScanSchedule {
    const KEY: &'static str = "library_scan_schedule";

    fn validate(&self) -> Result<(), ConfigError> {
        if !is_valid_hhmm(&self.daily_scan_time) {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "daily_scan_time",
                reason: format!(
                    "must be HH:MM (00:00-23:59), got {:?}",
                    self.daily_scan_time
                ),
            });
        }
        Ok(())
    }
}

// --- library_scan_filesystem_watcher --------------------------------------

/// Zero-dependency filesystem poller knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesystemWatcher {
    /// Master switch.
    pub enabled: bool,
    /// Stat-snapshot cadence in seconds (minimum 1).
    pub poll_interval_seconds: f64,
    /// Burst-collapse window in seconds (minimum 0).
    pub batch_window_seconds: f64,
}

impl Default for FilesystemWatcher {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_seconds: 300.0,
            batch_window_seconds: 60.0,
        }
    }
}

impl Section for FilesystemWatcher {
    const KEY: &'static str = "library_scan_filesystem_watcher";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "poll_interval_seconds",
            self.poll_interval_seconds,
            1.0,
            f64::MAX,
        )?;
        check_range(
            Self::KEY,
            "batch_window_seconds",
            self.batch_window_seconds,
            0.0,
            f64::MAX,
        )?;
        Ok(())
    }
}
