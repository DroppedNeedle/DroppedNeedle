//! Confinement: a finished download is only read from inside the folder
//! its client downloads into.
//!
//! A download client or plugin names the files it finished. Those names
//! come from outside, so each one is resolved by name (never by following
//! it) and must sit under one of the client's downloads folders with no
//! link anywhere below that folder's root. The root is the admin's setting
//! and may be a link. A subfolder set below it (slskd's downloads
//! subfolder) lives in the volume the client writes, so it is checked like
//! anything the client reports. This is a check made when the download
//! lands, not a guard on every later read of the same path.

use std::path::{Path, PathBuf};

use super::{Reported, lexical_absolute};

/// One folder a client downloads into: the admin's root, which is resolved
/// even through a link, and a subfolder below it, which may hold no link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadsFolder {
    root: PathBuf,
    below: PathBuf,
}

/// Why a downloads folder has no usable real path right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderFault {
    /// The root or a subfolder is missing or unreadable.
    Unresolved,
    /// A subfolder below the root is a link.
    Link,
}

impl DownloadsFolder {
    /// The root itself, with no subfolder.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            below: PathBuf::new(),
        }
    }

    /// The root plus a subfolder from settings. Empty, `.` and `..` parts
    /// are dropped, so the subfolder never climbs out of the root.
    #[must_use]
    pub fn with_subpath(root: PathBuf, subpath: &str) -> Self {
        let below = subpath
            .split(['/', '\\'])
            .map(str::trim)
            .filter(|part| !part.is_empty() && *part != "." && *part != "..")
            .collect();
        Self { root, below }
    }

    /// The folder as configured: the root joined with the subfolder.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        if self.below.as_os_str().is_empty() {
            return self.root.clone();
        }
        self.root.join(&self.below)
    }

    /// The folder's real path: the root resolved, then each subfolder part
    /// checked with `lstat`. Blocking.
    pub fn resolve(&self) -> Result<PathBuf, FolderFault> {
        let mut real = self
            .root
            .canonicalize()
            .map_err(|_| FolderFault::Unresolved)?;
        for part in self.below.components() {
            real.push(part);
            match std::fs::symlink_metadata(&real) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(FolderFault::Link),
                Ok(meta) if meta.is_dir() => {}
                _ => return Err(FolderFault::Unresolved),
            }
        }
        Ok(real)
    }
}

impl From<PathBuf> for DownloadsFolder {
    fn from(root: PathBuf) -> Self {
        Self::new(root)
    }
}

/// What a finished download reports: its files, and the folders its
/// client downloads into.
#[derive(Debug, Clone, Default)]
pub struct Landed {
    /// The client's downloads folders. A file outside all of them is
    /// refused, so a client with none lands nothing.
    pub mounts: Vec<DownloadsFolder>,
    /// The files or job folders the client reported.
    pub files: Vec<Reported>,
}

/// Where a reported path sits against its client's downloads folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// Inside a folder, as this link-free path.
    Inside(PathBuf),
    /// Outside every folder, or reached through a link.
    Outside,
    /// Not inside any folder that resolved, and at least one folder could
    /// not be resolved right now (a share that is not answering, a folder
    /// not created yet), so nothing can be said about the path.
    Unknown,
}

/// Place `path` against `mounts`.
///
/// `..` is resolved by name before anything is compared, the comparison is
/// by whole path components (`/downloads-other` is not under `/downloads`),
/// and every component below the folder's root, its subfolder included, is
/// checked with `lstat`. A path that does not exist yet is allowed: nothing
/// below a missing component can be a link, and the probe reports it
/// missing. A folder whose subfolder is a link holds nothing. Blocking.
pub fn place(path: &Path, mounts: &[DownloadsFolder]) -> Place {
    let Some(named) = lexical_absolute(path) else {
        return Place::Outside;
    };
    let mut unresolved = false;
    let inside = mounts.iter().find_map(|mount| {
        let real = match mount.resolve() {
            Ok(real) => real,
            Err(FolderFault::Unresolved) => {
                unresolved = true;
                return None;
            }
            Err(FolderFault::Link) => {
                tracing::warn!(
                    folder = %mount.path().display(),
                    "a downloads subfolder is a link; nothing in it is read"
                );
                return None;
            }
        };
        // The client may name the folder as configured or as it resolves.
        let below = named.strip_prefix(&real).ok().or_else(|| {
            let configured = lexical_absolute(&mount.path())?;
            named.strip_prefix(&configured).ok()
        })?;
        let mut resolved = real;
        let mut parts = below.components();
        for part in parts.by_ref() {
            resolved.push(part);
            match std::fs::symlink_metadata(&resolved) {
                Ok(meta) if meta.file_type().is_symlink() => return None,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        resolved.extend(parts);
        Some(resolved)
    });
    match inside {
        Some(resolved) => Place::Inside(resolved),
        None if unresolved => Place::Unknown,
        None => Place::Outside,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn only_paths_inside_the_mount_without_a_link_are_kept() {
        let scratch =
            droppedneedle_kernel::db::scratch::ScratchDir::new("confine").expect("scratch");
        let scratch = scratch.path().canonicalize().expect("scratch path");
        let mount = scratch.join("downloads");
        let lookalike = scratch.join("downloads-other");
        let outside = scratch.join("elsewhere");
        for dir in [&mount.join("job"), &lookalike, &outside] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        std::fs::write(mount.join("job/01.flac"), b"x").expect("file");
        std::fs::write(outside.join("secret.flac"), b"x").expect("file");
        std::fs::write(lookalike.join("01.flac"), b"x").expect("file");
        std::os::unix::fs::symlink(&outside, mount.join("linked-dir")).expect("link");
        std::os::unix::fs::symlink(outside.join("secret.flac"), mount.join("linked.flac"))
            .expect("link");
        std::os::unix::fs::symlink(mount.join("job"), mount.join("inner-link")).expect("link");
        let alias = scratch.join("alias");
        std::os::unix::fs::symlink(&mount, &alias).expect("link");
        let mounts = vec![DownloadsFolder::new(alias.clone())];
        let check = |path: PathBuf| match place(&path, &mounts) {
            Place::Inside(resolved) => Some(resolved),
            Place::Outside => None,
            Place::Unknown => panic!("the mount resolves"),
        };

        let file = mount.join("job/01.flac");
        assert_eq!(check(file.clone()), Some(file.clone()));
        assert_eq!(check(alias.join("job/01.flac")), Some(file.clone()));
        assert_eq!(check(mount.join("job/")), Some(mount.join("job")));
        assert_eq!(check(mount.join("job/../job/./01.flac")), Some(file));
        assert_eq!(
            check(mount.join("job/not-yet.flac")),
            Some(mount.join("job/not-yet.flac"))
        );

        assert_eq!(check(outside.join("secret.flac")), None, "elsewhere");
        assert_eq!(check(mount.join("../elsewhere/secret.flac")), None, "..");
        assert_eq!(check(mount.join("job/../../elsewhere")), None, "..");
        assert_eq!(check(lookalike.join("01.flac")), None, "name prefix");
        assert_eq!(check(mount.join("linked.flac")), None, "top-level link");
        assert_eq!(check(mount.join("linked-dir")), None, "linked folder");
        assert_eq!(
            check(mount.join("linked-dir/secret.flac")),
            None,
            "linked parent"
        );
        assert_eq!(
            check(mount.join("inner-link/01.flac")),
            None,
            "a link is never followed, even to somewhere inside"
        );
        assert_eq!(check(PathBuf::from("job/01.flac")), None, "relative");
        assert_eq!(place(&mount.join("job/01.flac"), &[]), Place::Outside);
        assert_eq!(
            place(&outside.join("secret.flac"), &[scratch.join("gone").into()]),
            Place::Unknown,
            "a folder that does not resolve says nothing about the path"
        );
        assert_eq!(
            place(
                &outside.join("secret.flac"),
                &[scratch.join("gone").into(), alias.into()]
            ),
            Place::Unknown
        );
    }
}
