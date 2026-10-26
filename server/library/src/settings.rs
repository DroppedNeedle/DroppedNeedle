//! Library settings as the engine reads them.
//!
//! The library roots, their path rules, the scan schedule, and the
//! filesystem watcher all live in the runtime [`ConfigStore`], the same
//! sections the settings pages edit. The engine holds a live handle on
//! each and reads it on every supervisor and watcher tick, so a saved
//! change takes effect without a restart, and the roots survive restarts.

use std::path::Path;

use super::scan::models::EffectivePolicy;
use super::scan::roots::{LibraryRoot, PolicyRule, RootRegistry, fingerprint_roots};
use super::scan::scheduler::{InclusionRule, ScheduleSettings};
use super::scan::watcher::WatcherSettings;
use super::service::ServiceError;
use crate::edition_prefs::LivePreferences;
use crate::policy::{self as library_policy, clean_absolute};
use droppedneedle_kernel::runtime_config::secret_sections::{
    IdentificationPolicy, JellyfinConnection, LibraryRoot as StoredRoot, TypedLibrary,
};
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, FilesystemWatcher, LibraryManagement, LibraryScanSchedule, MusicBrainzSettings,
};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
use droppedneedle_kernel::runtime_config::{ConfigStore, Live, UpdateError};
use jiff::tz::TimeZone;

/// The settings sections the library reads, each a live handle, plus the
/// store for the few things the library writes or signs.
#[derive(Clone)]
pub struct LibrarySettings {
    /// Roots, path rules, the import naming script and the AcoustID key.
    pub library: Live<TypedLibrary>,
    /// The automatic-scan schedule.
    pub schedule: Live<LibraryScanSchedule>,
    /// The server's time zone, which a daily scan time is read in.
    pub schedule_zone: TimeZone,
    /// The filesystem watcher.
    pub watcher: Live<FilesystemWatcher>,
    /// Library Management's profiles and undo retention.
    pub management: Live<LibraryManagement>,
    /// The recycle bin.
    pub download_policy: Live<DownloadPolicy>,
    /// Edition preferences, for identification and management.
    pub editions: LivePreferences,
    /// The Jellyfin server refreshed after a management run.
    pub jellyfin: Live<JellyfinConnection>,
    /// The MusicBrainz source identification asks.
    pub musicbrainz: Live<MusicBrainzSettings>,
    /// Adding a root and Library Management's settings write here, and
    /// correction previews are signed with its data key.
    store: ConfigStore,
}

impl LibrarySettings {
    /// Live handles on the library's sections in `config`, with daily
    /// scans in `timezone` (the deployment's `TZ`).
    pub fn new(config: &ConfigStore, timezone: Option<&str>) -> Self {
        Self {
            library: config.live(),
            schedule: config.live(),
            schedule_zone: schedule_zone(timezone),
            watcher: config.live(),
            management: config.live(),
            download_policy: config.live(),
            editions: LivePreferences::new(config),
            jellyfin: config.live(),
            musicbrainz: config.live(),
            store: config.clone(),
        }
    }

    /// Whether every section identification decides with reads. While one
    /// does not, no identify job is claimed: a default edition preference
    /// would accept another edition, and an unreadable MusicBrainz source
    /// would send every job to review.
    pub fn identification_readable(&self) -> bool {
        self.editions.current().is_some() && self.musicbrainz.read().is_some()
    }

    /// The store, for tests that save a section the library then reads.
    #[cfg(any(test, feature = "test-support"))]
    pub fn store(&self) -> &ConfigStore {
        &self.store
    }

    /// Sign `message` for `purpose` with the server's data key.
    pub(crate) fn mac(&self, purpose: &str, message: &[u8]) -> [u8; 32] {
        self.store.mac(purpose, message)
    }

    /// Add one root to the stored settings. The id must be new; the
    /// settings resolver the settings page saves through then refuses a
    /// path that equals, holds, or sits inside another root (or staging).
    /// The label is the directory name, made unique against the other
    /// roots. The checks and the write run as one settings transaction,
    /// like every settings-page library save. Blocking: call it off the
    /// async workers.
    pub fn add_root(&self, root: &LibraryRoot) -> Result<(), ServiceError> {
        self.store
            .update_secret_blocking::<TypedLibrary, ServiceError, _>(|stored| {
                with_root(stored.into_inner(), root)
            })
            .map(|_| ())
            .map_err(|error| match error {
                UpdateError::Rejected(error) => error,
                UpdateError::Config(error) if error.is_busy() => ServiceError::Busy {
                    cause: error.to_string(),
                },
                UpdateError::Config(error) => ServiceError::internal(&error),
            })
    }
}

/// The zone daily scans run in: `TZ` as an IANA name from the system zone
/// database or a POSIX rule, else the host's own zone (`/etc/localtime`).
/// One that does not resolve logs a warning and falls back to UTC.
fn schedule_zone(configured: Option<&str>) -> TimeZone {
    let resolved = match configured.map(|name| name.trim().trim_start_matches(':')) {
        Some(name) => TimeZone::get(name).or_else(|_| TimeZone::posix(name)),
        None => TimeZone::try_system(),
    };
    resolved.unwrap_or_else(|error| {
        tracing::warn!(
            timezone = configured.unwrap_or("system"),
            %error,
            "time zone not found; daily scans run in UTC"
        );
        TimeZone::UTC
    })
}

fn policy(stored: IdentificationPolicy) -> EffectivePolicy {
    match stored {
        IdentificationPolicy::Automatic => EffectivePolicy::Automatic,
        IdentificationPolicy::LocalMetadata => EffectivePolicy::LocalMetadata,
        IdentificationPolicy::Excluded => EffectivePolicy::Excluded,
    }
}

fn stored_policy(policy: EffectivePolicy) -> IdentificationPolicy {
    match policy {
        EffectivePolicy::Automatic => IdentificationPolicy::Automatic,
        EffectivePolicy::LocalMetadata => IdentificationPolicy::LocalMetadata,
        EffectivePolicy::Excluded => IdentificationPolicy::Excluded,
    }
}

/// True when two root paths are the same directory or one holds the other.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Build the engine registry from the stored settings. Roots that cannot
/// be used (relative path, a path another root already covers) are left
/// out with a warning instead of failing every root. The library is
/// enabled when the master switch is on and at least one root exists.
pub fn registry_from(settings: &TypedLibrary) -> RootRegistry {
    let mut roots: Vec<LibraryRoot> = Vec::new();
    for stored in &settings.library_roots {
        let Some(path) = clean_absolute(&stored.path) else {
            tracing::warn!(
                root_id = stored.id,
                "library root path is not absolute; skipped"
            );
            continue;
        };
        if stored.id.trim().is_empty() {
            tracing::warn!(path = %path.display(), "library root has no id; skipped");
            continue;
        }
        if let Some(other) = roots
            .iter()
            .find(|root| root.id == stored.id || overlaps(&root.path, &path))
        {
            tracing::warn!(
                root_id = stored.id,
                other = other.id,
                "library root duplicates or overlaps another root; skipped"
            );
            continue;
        }
        let rules = stored
            .rules
            .iter()
            .map(|rule| PolicyRule {
                id: rule.id.clone(),
                relative_path: rule.relative_path.trim_matches('/').to_owned(),
                policy: policy(rule.policy),
            })
            .filter(|rule| !rule.relative_path.is_empty())
            .collect();
        roots.push(LibraryRoot::new(&stored.id, path, policy(stored.policy)).with_rules(rules));
    }
    let enabled = settings.enabled && !roots.is_empty();
    let revision = fingerprint_roots(&roots, enabled);
    RootRegistry::new(roots, enabled, &revision)
}

/// The registry the saved settings describe now.
pub fn registry(settings: &Live<TypedLibrary>) -> Result<RootRegistry, ServiceError> {
    let settings = settings
        .get()
        .map_err(|error| ServiceError::internal(&error))?;
    Ok(registry_from(&settings))
}

/// Included subpaths under excluded roots: the rules whose policy brings
/// a path back into scans.
pub fn inclusion_rules(registry: &RootRegistry) -> Vec<InclusionRule> {
    registry
        .roots()
        .iter()
        .filter(|root| root.policy == EffectivePolicy::Excluded)
        .flat_map(|root| {
            root.rules
                .iter()
                .filter(|rule| rule.policy != EffectivePolicy::Excluded)
                .map(|rule| InclusionRule::new(&root.id, &rule.id, &rule.relative_path))
        })
        .collect()
}

/// Automatic-scan schedule, daily times read in `zone`. An unreadable
/// section schedules nothing.
pub fn schedule(settings: &Live<LibraryScanSchedule>, zone: &TimeZone) -> ScheduleSettings {
    match settings.get() {
        Ok(stored) => {
            ScheduleSettings::new(stored.scan_frequency, &stored.daily_scan_time, zone.clone())
        }
        Err(error) => {
            tracing::warn!(%error, "cannot read the scan schedule; automatic scans paused");
            ScheduleSettings::manual()
        }
    }
}

/// Filesystem watcher settings. An unreadable section turns it off.
pub fn watcher(settings: &Live<FilesystemWatcher>) -> WatcherSettings {
    match settings.get() {
        Ok(stored) => WatcherSettings {
            enabled: stored.enabled,
            poll_interval_seconds: stored.poll_interval_seconds,
            batch_window_seconds: stored.batch_window_seconds,
        },
        Err(error) => {
            tracing::warn!(%error, "cannot read the watcher settings; watcher off");
            WatcherSettings {
                enabled: false,
                ..WatcherSettings::default()
            }
        }
    }
}

/// The stored settings with `root` added and normalized. The masked
/// AcoustID key in `settings` resolves back to the stored one on save.
fn with_root(mut settings: TypedLibrary, root: &LibraryRoot) -> Result<TypedLibrary, ServiceError> {
    if settings
        .library_roots
        .iter()
        .any(|stored| stored.id == root.id)
    {
        return Err(ServiceError::Conflict {
            message: "Root id already exists".to_owned(),
        });
    }
    let base = root
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&root.id)
        .to_owned();
    let taken = |label: &str| {
        settings
            .library_roots
            .iter()
            .any(|stored| stored.label.trim().to_lowercase() == label.to_lowercase())
    };
    let label = if taken(&base) {
        format!("{base} ({})", root.id)
    } else {
        base
    };
    settings.library_roots.push(StoredRoot {
        id: root.id.clone(),
        path: root.path.to_string_lossy().into_owned(),
        label,
        policy: stored_policy(root.policy),
        rules: Vec::new(),
    });
    library_policy::normalize(&settings).map_err(|error| match error {
        SettingsError::InvalidInput { message } | SettingsError::Conflict { message } => {
            ServiceError::Conflict { message }
        }
        other => ServiceError::internal(&format!("{other:?}")),
    })
}
