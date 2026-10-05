//! Advisory lock between the server and the offline tool.
//!
//! The server holds a shared lock on `<db>.lock` for as long as it runs.
//! The offline import and restore take the same lock exclusively before
//! they touch the database and keep it for the whole run, so they refuse a
//! live server (even an idle one) and a server refuses to start mid-run.
//! The lock is an OS file lock: it goes away with the process, so a crash
//! never leaves a stale lock behind.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Why the lock could not be taken.
#[derive(Debug, Error)]
pub enum DataLockError {
    /// Another process holds the lock in a conflicting mode.
    #[error("{} is held by another process (a running server or an offline import)", .path.display())]
    Held {
        /// The lock file.
        path: PathBuf,
    },
    /// The lock file cannot be opened or locked.
    #[error("cannot lock {}: {reason}", .path.display())]
    Io {
        /// The lock file.
        path: PathBuf,
        /// OS reason.
        reason: String,
    },
}

/// A held lock; released on drop.
#[derive(Debug)]
pub struct DataLock {
    _file: File,
    path: PathBuf,
}

impl DataLock {
    /// Take the server's shared lock next to `db_path`. Several server
    /// handles in one process may hold it at once.
    pub fn shared(db_path: &Path) -> Result<Self, DataLockError> {
        Self::take(db_path, false)
    }

    /// Take the offline tool's exclusive lock next to `db_path`.
    pub fn exclusive(db_path: &Path) -> Result<Self, DataLockError> {
        Self::take(db_path, true)
    }

    /// The lock file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn take(db_path: &Path, exclusive: bool) -> Result<Self, DataLockError> {
        let path = lock_path(db_path);
        let io = |error: std::io::Error| DataLockError::Io {
            path: path.clone(),
            reason: error.to_string(),
        };
        if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io)?;
        let taken = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match taken {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(DataLockError::Held { path }),
            Err(TryLockError::Error(error)) => Err(io(error)),
        }
    }
}

/// File name of the lock beside a database named `db_name`.
#[must_use]
pub fn lock_file_name(db_name: &str) -> String {
    format!("{db_name}.lock")
}

/// `<db>.lock` beside the database file.
#[must_use]
pub fn lock_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}
