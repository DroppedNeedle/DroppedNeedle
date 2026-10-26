//! Whether the library can take a download now.
//!
//! A download does not start, and a finished one does not land, while the
//! library folder downloads go to is missing, cannot be written or does
//! not answer: it waits, the queue names the folder, and it goes ahead by
//! itself once the folder is back. Source readiness ([`crate::readiness`])
//! is a separate answer, and requests are taken and held either way.
//!
//! Nothing is written to find out. The folder must be a directory, its
//! filesystem must not be mounted read-only, and `faccessat(W_OK)` with
//! the effective ids must allow it. That is only a hint: as root it says
//! yes whatever the permissions, and on NFS with root squash it can say
//! yes to a write the server then refuses. A wrong yes costs a download
//! that lands and then waits with `library_folder_missing` or
//! `library_unavailable`.
//!
//! The worker runs [`check`] once a pass under a timeout and keeps the
//! answer; everything else reads the kept answer.

use std::path::{Path, PathBuf};

use crate::reason::{self, Reason};

/// Why a download waits for the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryWait {
    /// The folder is not there, or is not a directory.
    Missing(PathBuf),
    /// The folder cannot be read or written.
    Unavailable(PathBuf),
    /// The check did not finish in time (a hung network share). The folder
    /// is the last one known, if any.
    NotAnswering(Option<PathBuf>),
}

impl LibraryWait {
    /// The folder the download waits for, when known.
    pub fn folder(&self) -> Option<&Path> {
        match self {
            Self::Missing(folder) | Self::Unavailable(folder) => Some(folder),
            Self::NotAnswering(folder) => folder.as_deref(),
        }
    }

    /// The reason a waiting download shows.
    pub fn reason(&self) -> Reason {
        match self {
            Self::Missing(_) => Reason::LibraryFolderMissing,
            Self::Unavailable(_) | Self::NotAnswering(_) => Reason::LibraryUnavailable,
        }
    }

    /// The sentence the queue shows.
    pub fn sentence(&self) -> String {
        let problem = match self {
            Self::Missing(_) => reason::FolderProblem::Missing,
            Self::Unavailable(_) => reason::FolderProblem::NotWritable,
            Self::NotAnswering(_) => reason::FolderProblem::NotAnswering,
        };
        reason::library_folder_wait(self.folder(), problem)
    }
}

/// Check `folder`. Blocking, and it can hang on a network share: the
/// worker bounds it. `None` when a download may go ahead.
pub fn check(folder: &Path) -> Option<LibraryWait> {
    match std::fs::metadata(folder) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Some(LibraryWait::Missing(folder.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(LibraryWait::Missing(folder.to_path_buf()));
        }
        Err(_) => return Some(LibraryWait::Unavailable(folder.to_path_buf())),
    }
    if read_only_mount(folder) || !writable(folder) {
        return Some(LibraryWait::Unavailable(folder.to_path_buf()));
    }
    None
}

/// The folder's filesystem is mounted read-only. A failed `statvfs` says
/// nothing, and the access check decides.
fn read_only_mount(folder: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c_path) = std::ffi::CString::new(folder.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `statvfs` is plain old data, so all-zero is a valid value.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
    // call, and `stat` is a valid, writable `statvfs`.
    let status = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    status == 0 && stat.f_flag & libc::ST_RDONLY != 0
}

/// The server's effective ids may write in the folder.
fn writable(folder: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c_path) = std::ffi::CString::new(folder.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
    // call; `faccessat` only reads it.
    unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            libc::W_OK,
            libc::AT_EACCESS,
        ) == 0
    }
}
