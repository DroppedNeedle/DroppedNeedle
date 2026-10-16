//! Library scanning: the scan schedule, the filesystem watcher, and the
//! default naming template.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Section, check_range, is_valid_hhmm};
use crate::runtime_config::error::ConfigError;

/// v2 default naming template (kept in sync with the publisher).
pub const DEFAULT_NAMING_TEMPLATE: &str =
    "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}";

// --- library_scan_schedule (the v2 sync section is dropped, this one kept) -

/// Automatic-scan cadence values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum ScanFrequency {
    /// Never scan automatically. `Manual` is still read: earlier builds
    /// wrote the variant name.
    #[serde(rename = "manual", alias = "Manual")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
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

// --- edition_preferences ----------------------------------------------------

/// Which release date wins when editions otherwise tie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditionDatePreference {
    /// The earliest release: usually the original.
    #[default]
    Earliest,
    /// The latest release: usually the newest remaster.
    Latest,
    /// Dates do not matter.
    Any,
}

/// Standard or expanded editions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditionVersionPreference {
    /// The plain album.
    #[default]
    Standard,
    /// Deluxe, expanded and anniversary editions.
    Deluxe,
    /// Either.
    Any,
}

/// How DroppedNeedle picks between editions of one album when the files
/// do not settle it, and which edition it fetches for an album you do
/// not have yet. A person's choice always wins, then the best fit for the
/// files, then these preferences, in this order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct EditionPreferences {
    /// Release statuses, most wanted first (`official`, `promotion`,
    /// `bootleg`, `pseudo-release`). Unlisted statuses come last.
    pub status_order: Vec<String>,
    /// Media formats, most wanted first (`digital media`, `cd`, `vinyl`,
    /// `cassette`). A format matches when its name contains the entry.
    pub format_order: Vec<String>,
    /// Release countries, most wanted first, as two-letter codes (`XW` is
    /// worldwide). Empty means your store region, then worldwide.
    pub countries: Vec<String>,
    /// Earliest, latest, or any release date.
    pub date: EditionDatePreference,
    /// Standard, deluxe, or either.
    pub version: EditionVersionPreference,
    /// Release types to avoid unless nothing else fits (`live`,
    /// `compilation`, `remix`, `soundtrack`, `demo`).
    pub avoid_types: Vec<String>,
    /// Let file tagging and organizing work on albums whose match is still
    /// unconfirmed. Off: those albums wait until someone confirms them.
    pub manage_unconfirmed: bool,
}

impl Default for EditionPreferences {
    fn default() -> Self {
        let owned = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
        Self {
            status_order: owned(&["official", "promotion", "bootleg", "pseudo-release"]),
            format_order: owned(&["digital media", "cd", "vinyl", "cassette"]),
            countries: Vec::new(),
            date: EditionDatePreference::Earliest,
            version: EditionVersionPreference::Standard,
            avoid_types: owned(&["live", "compilation"]),
            manage_unconfirmed: false,
        }
    }
}

impl Section for EditionPreferences {
    const KEY: &'static str = "edition_preferences";

    fn validate(&self) -> Result<(), ConfigError> {
        let country_ok = |code: &String| {
            let code = code.trim();
            code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_alphabetic())
        };
        if let Some(bad) = self.countries.iter().find(|code| !country_ok(code)) {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "countries",
                reason: format!("must be two-letter country codes, got {bad:?}"),
            });
        }
        for (field, list) in [
            ("status_order", &self.status_order),
            ("format_order", &self.format_order),
            ("avoid_types", &self.avoid_types),
        ] {
            if list.len() > 32 || list.iter().any(|entry| entry.trim().len() > 64) {
                return Err(ConfigError::Validation {
                    section: Self::KEY,
                    field,
                    reason: "at most 32 entries of at most 64 characters".to_owned(),
                });
            }
        }
        Ok(())
    }

    fn normalize(&mut self) {
        let clean = |list: &mut Vec<String>, upper: bool| {
            let mut seen: Vec<String> = Vec::new();
            for entry in list.drain(..) {
                let entry = entry.trim();
                let entry = if upper {
                    entry.to_ascii_uppercase()
                } else {
                    entry.to_lowercase()
                };
                if !entry.is_empty() && !seen.contains(&entry) {
                    seen.push(entry);
                }
            }
            *list = seen;
        };
        clean(&mut self.status_order, false);
        clean(&mut self.format_order, false);
        clean(&mut self.countries, true);
        clean(&mut self.avoid_types, false);
    }
}
