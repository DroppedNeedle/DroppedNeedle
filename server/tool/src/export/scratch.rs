//! Private scratch folders in a temp folder other checks and runs share.
//!
//! Each folder holds a lock file its owner keeps locked for the folder's
//! life, so a sweep of folders left by killed processes can tell a live
//! one from a dead one: a killed process loses its lock with it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// The lock file inside each scratch folder.
const LOCK_FILE: &str = ".owner.lock";

/// The name a folder is made under, before its lock is taken. No sweep
/// matches it, so a sweep never sees a folder that has no lock yet.
const UNLOCKED_PREFIX: &str = "droppedneedle-scratch-new-";

/// An owner-only folder, locked while this lives. It holds password hashes
/// and listening history, so its owner removes it with [`Self::remove`].
#[derive(Debug)]
pub struct ScratchFolder {
    path: PathBuf,
    lock: File,
}

impl ScratchFolder {
    /// A new folder in `parent`, named `prefix` and a random suffix, made
    /// owner-only and locked before it gets that name.
    pub fn create(parent: &Path, prefix: &str) -> io::Result<Self> {
        let mut name = [0u8; 8];
        getrandom::fill(&mut name).map_err(|_| io::Error::other("cannot pick a folder name"))?;
        let hex: String = name.iter().map(|byte| format!("{byte:02x}")).collect();
        let unlocked = parent.join(format!("{UNLOCKED_PREFIX}{hex}"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&unlocked)?;
        let path = parent.join(format!("{prefix}{hex}"));
        let locked = lock_new(&unlocked.join(LOCK_FILE))
            .and_then(|lock| std::fs::rename(&unlocked, &path).map(|()| lock));
        match locked {
            Ok(lock) => Ok(Self { path, lock }),
            Err(error) => {
                let _ = std::fs::remove_dir_all(&unlocked);
                Err(error)
            }
        }
    }

    /// The folder.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the folder.
    pub fn remove(self) -> io::Result<()> {
        finish(&self.path, Some(self.lock))
    }
}

fn lock_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        // Where the filesystem cannot lock, the folder goes unlocked, as
        // every folder did before locks, and a sweep may remove it.
        Err(TryLockError::Error(error)) if cannot_lock(&error) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(io::Error::other("a new lock file is held")),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn cannot_lock(error: &io::Error) -> bool {
    #[cfg(unix)]
    if matches!(error.raw_os_error(), Some(libc::ENOLCK | libc::EOPNOTSUPP)) {
        return true;
    }
    error.kind() == io::ErrorKind::Unsupported
}

/// Remove the entry at `path`, a scratch folder or what was left of one,
/// unless a live owner holds its lock. A folder with no lock file is from
/// an older build, or its owner has removed everything else in it. The
/// folder is emptied while the lock is held here, so no new owner can take
/// it in between. Links are removed, never followed.
pub(crate) fn remove_unless_owned(path: &Path) -> io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !meta.is_dir() {
        return std::fs::remove_file(path);
    }
    let mut options = OpenOptions::new();
    // Write access, as an exclusive lock on NFS needs.
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // A link or a FIFO planted in a shared temp folder neither misleads
        // nor stalls the sweep.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let lock = match options.open(path.join(LOCK_FILE)) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return finish(path, None),
        Err(error) => return Err(error),
    };
    if !lock.metadata()?.is_file() {
        return Err(io::Error::other("the lock file is not a file"));
    }
    match lock.try_lock() {
        Ok(()) => finish(path, Some(lock)),
        Err(TryLockError::Error(error)) if cannot_lock(&error) => finish(path, Some(lock)),
        Err(TryLockError::WouldBlock) => Ok(()),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Remove everything in the folder but its lock file while `lock` is held,
/// then close the lock and remove the lock file and the folder. The lock is
/// closed first because NFS keeps an open file that is removed as a hidden
/// one, which would leave the folder not empty. With the secrets gone, a
/// sweep that finishes the folder off at the same time does no harm.
fn finish(path: &Path, lock: Option<File>) -> io::Result<()> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name() == LOCK_FILE {
            continue;
        }
        if entry.file_type()?.is_dir() {
            gone(std::fs::remove_dir_all(entry.path()))?;
        } else {
            gone(std::fs::remove_file(entry.path()))?;
        }
    }
    drop(lock);
    gone(std::fs::remove_file(path.join(LOCK_FILE)))?;
    gone(std::fs::remove_dir(path))
}

/// A removal that found nothing to remove is done.
fn gone(removed: io::Result<()>) -> io::Result<()> {
    match removed {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        removed => removed,
    }
}
