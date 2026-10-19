//! Settings changed from the host, for an admin the web UI refuses.
//!
//! Someone whose every save is answered `403 Origin not allowed` cannot
//! save the setting that would fix it, and neither can someone whose
//! browser shows a blank page. Whoever can run this can already
//! write the data directory, so it asks for no credential. A change goes
//! through the section's own validated save, the same one the settings
//! route uses. The exclusive [`DataLock`] is held for the run, so the
//! server has to be stopped first.
//!
//! The settings live in the database. Opening them here does what the
//! server's start does first: a `config.json` from before settings moved
//! into the database is copied in once, even by a command that only reads.

use std::path::PathBuf;

use thiserror::Error;

use droppedneedle::bootstrap::data_dirs::{BootRefusal, DataDirs, check_bootable, refuse_lost_key};
use droppedneedle_kernel::db::datalock::{DataLock, DataLockError};
use droppedneedle_kernel::db::{DbConfig, DurableLane, open_runtime};
use droppedneedle_kernel::runtime_config::crypto::CryptoError;
use droppedneedle_kernel::runtime_config::sections::{
    ContentSecurityPolicyMode, PublicAddress, SecuritySettings,
};
use droppedneedle_kernel::runtime_config::{
    ConfigError, ConfigStore, Crypto, LegacyConfig, UpdateError,
};

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
    /// The server never started on these folders: no key, or no database.
    #[error("There is no DroppedNeedle instance in {}.", .0.display())]
    NoInstance(PathBuf),
    /// The section's validation refused the new value.
    #[error("Not saved: {0}")]
    Refused(String),
    /// The lock, the key, the database or the settings failed.
    #[error("Could not open the settings: {0}")]
    Failed(String),
}

/// Run `change` on the settings of a stopped instance: take the lock, check
/// the folders hold a v3 instance, open the database and the settings on
/// it with the instance's key.
pub async fn with_stopped_settings<T>(
    dirs: &DataDirs,
    change: impl AsyncFnOnce(&ConfigStore) -> Result<T, ConfigError>,
) -> Result<T, HostSettingsError> {
    let failed = |error: &dyn std::fmt::Display| HostSettingsError::Failed(error.to_string());
    // The lock creates the folder and a `.lock` file, so a folder with no
    // database, or a v2 one, is refused before it is taken. The same checks
    // run again under the lock.
    check_bootable(dirs).map_err(HostSettingsError::NotV3)?;
    if !dirs.db_path.is_file() {
        return Err(HostSettingsError::NoInstance(dirs.config_dir.clone()));
    }
    let _lock = DataLock::exclusive(&dirs.db_path).map_err(|error| match error {
        DataLockError::Held { path } => HostSettingsError::ServerRunning(path),
        other @ DataLockError::Io { .. } => failed(&other),
    })?;
    check_bootable(dirs).map_err(HostSettingsError::NotV3)?;
    refuse_lost_key(&dirs.config_dir, &dirs.db_path).map_err(HostSettingsError::NotV3)?;
    let crypto = Crypto::load(&dirs.config_dir).map_err(|error| match error {
        CryptoError::KeyMissing { .. } => HostSettingsError::NoInstance(dirs.config_dir.clone()),
        other => failed(&other),
    })?;
    // Opening creates a missing database, and a server that never started
    // has nothing to change.
    if !dirs.db_path.is_file() {
        return Err(HostSettingsError::NoInstance(dirs.config_dir.clone()));
    }
    let runtime = open_runtime(&DbConfig::new(&dirs.db_path))
        .await
        .map_err(|error| failed(&error))?;
    let outcome = async {
        let store = ConfigStore::open(
            runtime.db().clone(),
            crypto,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            LegacyConfig::at(&dirs.config_dir, false),
        )
        .await
        .map_err(|error| failed(&error))?;
        change(&store).await.map_err(|error| match error {
            ConfigError::Validation { field, reason, .. } => {
                HostSettingsError::Refused(format!("{field}: {reason}"))
            }
            other => failed(&other),
        })
    }
    .await;
    runtime.shutdown().await;
    outcome
}

/// Choose how the web UI's content-security policy is sent, for an admin
/// whose browser shows a blank page under it and who therefore cannot reach
/// the Security tab. Only that field of the security settings changes.
/// Returns the mode as stored.
pub async fn set_content_security_policy(
    dirs: &DataDirs,
    mode: ContentSecurityPolicyMode,
) -> Result<ContentSecurityPolicyMode, HostSettingsError> {
    with_stopped_settings(dirs, async move |store: &ConfigStore| {
        store
            .update::<SecuritySettings, ConfigError, _>(
                DurableLane::Foreground,
                move |mut security| {
                    security.content_security_policy = mode;
                    Ok(security)
                },
            )
            .await
            .map_err(|error| match error {
                UpdateError::Rejected(error) | UpdateError::Config(error) => error,
            })
    })
    .await
    .map(|security| security.content_security_policy)
}

/// The stated public addresses, as stored.
pub async fn public_address(dirs: &DataDirs) -> Result<Vec<String>, HostSettingsError> {
    with_stopped_settings(dirs, async move |store: &ConfigStore| {
        store.get::<PublicAddress>()
    })
    .await
    .map(|section| section.origins)
}

/// Replace the stated public addresses; an empty list clears them. Returns
/// the list as stored.
pub async fn set_public_address(
    dirs: &DataDirs,
    origins: Vec<String>,
) -> Result<Vec<String>, HostSettingsError> {
    with_stopped_settings(dirs, async move |store: &ConfigStore| {
        store.save(PublicAddress { origins }).await
    })
    .await
    .map(|section| section.origins)
}
