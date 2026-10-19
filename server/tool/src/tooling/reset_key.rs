//! A new data key for an instance whose key is lost for good.
//!
//! Every stored secret was sealed with the lost key, so it cannot be read
//! again. This command clears all of them, mints a new key and stores its
//! check value, so the server starts again with the secrets empty. It only
//! destroys: it refuses while the key file is there and belongs to the
//! database.
//!
//! Crash order: the new key is written to `data_enc.key.new` (synced)
//! first, then one transaction clears the secrets and stores the new
//! check, then the old key is copied to the first free `data_enc.key.old`,
//! `.old.1`, ... and removed, and `.new` is moved into place. A run that
//! finds a `.new` matching the stored check only finishes the moves; a
//! `.new` that does not was never committed and is replaced. The old key is
//! never overwritten or lost. The exclusive [`DataLock`] is held for the
//! run, so the server has to be stopped first.

use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use thiserror::Error;

use droppedneedle::bootstrap::data_dirs::{BootRefusal, DataDirs, check_bootable};
use droppedneedle::bootstrap::key_check::{Forgotten, forget_secrets};
use droppedneedle_kernel::db::datalock::{DataLock, DataLockError};
use droppedneedle_kernel::db::{Db, DbConfig, open_runtime};
use droppedneedle_kernel::runtime_config::Crypto;
use droppedneedle_kernel::runtime_config::crypto::{
    CryptoError, KEY_FILE_NAME, NEW_KEY_FILE_NAME, OLD_KEY_FILE_NAME,
};
use droppedneedle_kernel::runtime_config::ingest::LEGACY_CONFIG_FILE_NAME;
use droppedneedle_kernel::runtime_config::key_check::{Assessment, assess, stored_check};

/// Why the key was not reset. Nothing is changed in any of these cases.
#[derive(Debug, Error)]
pub enum ResetKeyError {
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
    /// The folders hold no database yet.
    #[error("There is no DroppedNeedle database at {}.", .0.display())]
    NoDatabase(PathBuf),
    /// The key file is there and belongs to the database.
    #[error(
        "The key file {} belongs to this database, so it is not lost. Nothing was changed: \
         this command only clears secrets.",
        .0.display()
    )]
    KeyNotLost(PathBuf),
    /// The key file is there but cannot be read (permissions or I/O).
    #[error(
        "Cannot read the key file ({0}). Fix its permissions and run the server or this \
         command again; nothing was changed."
    )]
    KeyUnreadable(CryptoError),
    /// The lock, the database or a file move failed.
    #[error("Could not reset the key: {0}")]
    Failed(String),
}

/// What a reset did.
#[derive(Debug)]
pub struct KeyReset {
    /// What was cleared, or `None` when an earlier run had already cleared
    /// it and this one only moved the key files into place.
    pub forgotten: Option<Forgotten>,
    /// Where the replaced key was kept, when there was one.
    pub old_key: Option<PathBuf>,
    /// A `config.json` beside the database. It stays unread.
    pub unread_config: Option<PathBuf>,
}

/// Reset the key of the stopped instance in `dirs`.
pub async fn reset(dirs: &DataDirs) -> Result<KeyReset, ResetKeyError> {
    let _lock = DataLock::exclusive(&dirs.db_path).map_err(|error| match error {
        DataLockError::Held { path } => ResetKeyError::ServerRunning(path),
        other @ DataLockError::Io { .. } => failed(&other),
    })?;
    check_bootable(dirs).map_err(ResetKeyError::NotV3)?;
    // Opening creates a missing database, which has nothing to reset.
    if !dirs.db_path.is_file() {
        return Err(ResetKeyError::NoDatabase(dirs.db_path.clone()));
    }
    let runtime = open_runtime(&DbConfig::new(&dirs.db_path))
        .await
        .map_err(|error| failed(&error))?;
    let (db, config_dir) = (runtime.db().clone(), dirs.config_dir.clone());
    let cleared = tokio::task::spawn_blocking(move || clear(&db, &config_dir))
        .await
        .map_err(|error| failed(&error));
    runtime.shutdown().await;
    let forgotten = cleared??;
    let old_key = move_keys(&dirs.config_dir)?;
    let config = dirs.config_dir.join(LEGACY_CONFIG_FILE_NAME);
    Ok(KeyReset {
        forgotten,
        old_key,
        unread_config: config.is_file().then_some(config),
    })
}

fn failed(error: &dyn std::fmt::Display) -> ResetKeyError {
    ResetKeyError::Failed(error.to_string())
}

/// Decide, then mint `.new` and clear in one transaction. `None` when a
/// committed earlier run left only the moves to do.
fn clear(db: &Db, config_dir: &Path) -> Result<Option<Forgotten>, ResetKeyError> {
    let stored = db
        .read_blocking("reset_key.stored", stored_check)
        .map_err(|error| failed(&error))?;
    let new_path = config_dir.join(NEW_KEY_FILE_NAME);
    if let (Some(stored), Ok(new_key)) = (&stored, Crypto::load_file(&new_path))
        && new_key.check_value() == *stored
    {
        return Ok(None);
    }
    let key_path = config_dir.join(KEY_FILE_NAME);
    match Crypto::load(config_dir) {
        Ok(key) => {
            let assessment = db
                .read_blocking("reset_key.assess", |conn| assess(conn, &key))
                .map_err(|error| failed(&error))?;
            if assessment != Assessment::Mismatch {
                return Err(ResetKeyError::KeyNotLost(key_path));
            }
        }
        Err(CryptoError::KeyMissing { .. } | CryptoError::KeyInvalid) => {}
        Err(other) => return Err(ResetKeyError::KeyUnreadable(other)),
    }
    // A `.new` that does not match the stored check was never committed:
    // nothing was sealed under it.
    match std::fs::remove_file(&new_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(&error)),
    }
    let new_key = Crypto::mint_file(&new_path).map_err(|error| failed(&error))?;
    forget_secrets(db, &new_key, &droppedneedle_kernel::clock::SystemClock)
        .map(Some)
        .map_err(|error| failed(&error))
}

/// Keep the old key (if any) as the first free `.old` name, then move
/// `.new` into place. Answers where the old key went.
fn move_keys(config_dir: &Path) -> Result<Option<PathBuf>, ResetKeyError> {
    let key_path = config_dir.join(KEY_FILE_NAME);
    let old = match std::fs::read(&key_path) {
        Ok(bytes) => {
            let kept = keep_old(config_dir, &bytes)?;
            std::fs::remove_file(&key_path).map_err(|error| failed(&error))?;
            Some(kept)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(failed(&error)),
    };
    std::fs::rename(config_dir.join(NEW_KEY_FILE_NAME), &key_path)
        .map_err(|error| failed(&error))?;
    sync_dir(config_dir)?;
    Ok(old)
}

/// Write `bytes` to the first `.old` name that does not exist yet, synced.
/// A copy, not a link or a rename, so it works on any filesystem and never
/// replaces a file.
fn keep_old(config_dir: &Path, bytes: &[u8]) -> Result<PathBuf, ResetKeyError> {
    for index in 0u32.. {
        let name = if index == 0 {
            OLD_KEY_FILE_NAME.to_owned()
        } else {
            format!("{OLD_KEY_FILE_NAME}.{index}")
        };
        let path = config_dir.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|error| failed(&error))?;
                sync_dir(config_dir)?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(failed(&error)),
        }
    }
    Err(ResetKeyError::Failed(
        "no free name for the old key".to_owned(),
    ))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), ResetKeyError> {
    std::fs::File::open(dir)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| failed(&error))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), ResetKeyError> {
    Ok(())
}
