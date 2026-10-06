//! Library settings as the engine reads them.
//!
//! The library roots, their path rules, the scan schedule, and the
//! filesystem watcher all live in the runtime [`ConfigStore`], the same
//! sections the settings pages edit. The engine re-reads them on every
//! supervisor and watcher tick, so a saved change takes effect without a
//! restart, and the roots survive restarts with the config file.

use std::path::Path;

use super::scan::models::EffectivePolicy;
use super::scan::roots::{LibraryRoot, PolicyRule, RootRegistry, fingerprint_roots};
use super::scan::scheduler::{InclusionRule, ScheduleSettings};
use super::scan::watcher::WatcherSettings;
use super::service::ServiceError;
use crate::runtime_config::secret_sections::{
    IdentificationPolicy, LibraryRoot as StoredRoot, TypedLibrary,
};
use crate::runtime_config::sections::{FilesystemWatcher, LibraryScanSchedule, ScanFrequency};
use crate::runtime_config::{ConfigStore, UpdateError};
use crate::settings::error::SettingsError;
use crate::settings::library_policy::{self, clean_absolute};

/// Timezone daily schedules resolve against. The scheduler has no tz
/// database, so daily times read as UTC.
const SCHEDULE_TIMEZONE: &str = "UTC";

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

/// Current registry from the config store.
pub fn registry(config: &ConfigStore) -> Result<RootRegistry, ServiceError> {
    let settings = config
        .get_masked::<TypedLibrary>()
        .map_err(|error| ServiceError::internal(&error))?
        .into_inner();
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

fn frequency(value: ScanFrequency) -> &'static str {
    match value {
        ScanFrequency::Manual => "manual",
        ScanFrequency::Min5 => "5min",
        ScanFrequency::Min10 => "10min",
        ScanFrequency::Min30 => "30min",
        ScanFrequency::Hr1 => "1hr",
        ScanFrequency::Hr6 => "6hr",
        ScanFrequency::Hr12 => "12hr",
        ScanFrequency::Hr24 => "24hr",
        ScanFrequency::Days3 => "3d",
        ScanFrequency::Days7 => "7d",
        ScanFrequency::Daily => "daily",
    }
}

/// Automatic-scan schedule. An unreadable section schedules nothing.
pub fn schedule(config: &ConfigStore) -> ScheduleSettings {
    match config.get::<LibraryScanSchedule>() {
        Ok(stored) => ScheduleSettings::new(
            frequency(stored.scan_frequency),
            &stored.daily_scan_time,
            SCHEDULE_TIMEZONE,
        ),
        Err(error) => {
            tracing::warn!(%error, "cannot read the scan schedule; automatic scans paused");
            ScheduleSettings::manual()
        }
    }
}

/// Filesystem watcher settings. An unreadable section turns it off.
pub fn watcher(config: &ConfigStore) -> WatcherSettings {
    match config.get::<FilesystemWatcher>() {
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

/// Add one root to the stored settings. The id must be new; the settings
/// resolver the settings page saves through then refuses a path that
/// equals, holds, or sits inside another root (or staging). The label is
/// the directory name, made unique against the other roots. The checks
/// and the write run as one locked store update, the same lock every
/// settings-page library save takes.
pub fn add_root(config: &ConfigStore, root: &LibraryRoot) -> Result<(), ServiceError> {
    config
        .update_secret::<TypedLibrary, ServiceError, _>(|stored| {
            with_root(stored.into_inner(), root)
        })
        .map(|_| ())
        .map_err(|error| match error {
            UpdateError::Rejected(error) => error,
            UpdateError::Config(error) => ServiceError::internal(&error),
        })
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
    let resolved = library_policy::resolve(&settings).map_err(|error| match error {
        SettingsError::InvalidInput { message } | SettingsError::Conflict { message } => {
            ServiceError::Conflict { message }
        }
        other => ServiceError::internal(&format!("{other:?}")),
    })?;
    Ok(resolved.settings)
}
