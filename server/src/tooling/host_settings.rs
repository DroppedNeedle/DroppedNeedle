//! Settings changed from the host, for an admin the web UI refuses.
//!
//! Someone whose every save is answered `403 Origin not allowed` cannot
//! save the setting that would fix it. Whoever can run this can already
//! write the data directory, so it asks for no credential. A change goes
//! through the section's own validated save, the same one the settings
//! route uses. The exclusive [`DataLock`] is held for the run, so the
//! server has to be stopped first: a running one holds the settings in
//! memory and would write over the change.

use std::path::PathBuf;

use thiserror::Error;

use crate::runtime_config::crypto::CryptoError;
use crate::runtime_config::sections::PublicAddress;
use crate::runtime_config::{ConfigError, ConfigStore, Crypto};
use crate::tooling::datalock::{DataLock, DataLockError};
use crate::tooling::upgrade::{BootRefusal, DataDirs, check_bootable};

/// Why the settings were not read or changed. Nothing is written in any of
/// these cases.
#[derive(Debug, Error)]
pub enum HostSettingsError {
    /// A running server (or another tool run) holds the data directory.
    #[error(
        "The data directory is in use ({} is locked). Stop the server first, then run this \
         again.",
        .0.display()
    )]
    ServerRunning(PathBuf),
    /// The folders do not hold a v3 instance.
    #[error("{0}")]
    NotV3(BootRefusal),
    /// The config folder has no key yet: the server never started on it.
    #[error("There is no DroppedNeedle instance in {}.", .0.display())]
    NoInstance(PathBuf),
    /// The section's validation refused the new value.
    #[error("Not saved: {0}")]
    Refused(String),
    /// The lock, the key or the settings file failed.
    #[error("Could not open the settings: {0}")]
    Failed(String),
}

/// Run `change` on the settings of a stopped instance: take the lock, check
/// the folders hold a v3 instance, open the settings file with its key.
pub fn with_stopped_settings<T>(
    dirs: &DataDirs,
    change: impl FnOnce(&ConfigStore) -> Result<T, ConfigError>,
) -> Result<T, HostSettingsError> {
    let failed = |error: &dyn std::fmt::Display| HostSettingsError::Failed(error.to_string());
    let _lock = DataLock::exclusive(&dirs.db_path).map_err(|error| match error {
        DataLockError::Held { path } => HostSettingsError::ServerRunning(path),
        other @ DataLockError::Io { .. } => failed(&other),
    })?;
    check_bootable(dirs).map_err(HostSettingsError::NotV3)?;
    let crypto = Crypto::load(&dirs.config_dir).map_err(|error| match error {
        CryptoError::KeyMissing { .. } => HostSettingsError::NoInstance(dirs.config_dir.clone()),
        other => failed(&other),
    })?;
    let store = ConfigStore::open(&dirs.config_file, crypto).map_err(|error| failed(&error))?;
    change(&store).map_err(|error| match error {
        ConfigError::Validation { reason, .. } => HostSettingsError::Refused(reason),
        other => failed(&other),
    })
}

/// The stated public addresses, as stored.
pub fn public_address(dirs: &DataDirs) -> Result<Vec<String>, HostSettingsError> {
    with_stopped_settings(dirs, ConfigStore::get::<PublicAddress>).map(|section| section.origins)
}

/// Replace the stated public addresses; an empty list clears them. Returns
/// the list as stored.
pub fn set_public_address(
    dirs: &DataDirs,
    origins: Vec<String>,
) -> Result<Vec<String>, HostSettingsError> {
    with_stopped_settings(dirs, |store| store.save(PublicAddress { origins }))
        .map(|section| section.origins)
}
