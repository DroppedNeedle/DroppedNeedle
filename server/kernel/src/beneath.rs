//! Opening a file below a folder without following a link.
//!
//! A path below a library or downloads folder can be changed by whoever
//! writes that folder, so a check made before the read proves nothing at
//! the read. These openers walk from the folder one component at a time
//! with `O_NOFOLLOW`: a link met on the way, even one swapped in a moment
//! ago, fails the open instead of being followed. `.` and `..` below the
//! folder are refused.

use std::ffi::{CStr, CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path};

/// Whether the folder itself may be reached through a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootLinks {
    /// The folder is an admin's setting and may be a link: it is opened as
    /// named. Only what lies below it is checked.
    Follow,
    /// The folder is a real path already resolved: it is opened from `/`
    /// one component at a time like the rest, so a link anywhere fails.
    /// The folder must be absolute.
    Refuse,
}

/// Open the regular file at `path`, which must lie strictly below `root`,
/// for reading. A FIFO or device fails rather than hanging the open.
/// Blocking.
pub fn open_file_beneath(root: &Path, path: &Path, links: RootLinks) -> io::Result<File> {
    let (dir, name) = parent_beneath(root, path, links)?;
    // Non-blocking so a FIFO cannot hang the open; cleared once the file
    // is known to be regular, as a FUSE mount may honour it on reads.
    let file = File::from(open_at(
        Some(&dir),
        &name,
        libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_NOFOLLOW,
    )?);
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    // SAFETY: `file` owns an open descriptor for the length of both calls.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    // SAFETY: as above; only the status flags change.
    if flags < 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

/// The opened folder holding `path`, which must lie strictly below `root`,
/// and the name of `path` in it. Every folder on the way is opened as
/// [`open_file_beneath`] opens them; the name itself is not touched, so a
/// caller can act on it with an `*at` call that does not follow a link.
/// Blocking.
pub fn parent_beneath(
    root: &Path,
    path: &Path,
    links: RootLinks,
) -> io::Result<(OwnedFd, CString)> {
    let refused = || io::Error::new(io::ErrorKind::InvalidInput, "not below its folder");
    let below = path.strip_prefix(root).map_err(|_| refused())?;
    let mut parts = plain_names(below).ok_or_else(refused)?;
    let name = parts.pop().ok_or_else(refused)?;
    let mut dir = match links {
        RootLinks::Follow => open_at(None, &c_name(root.as_os_str())?, DIR_FLAGS)?,
        RootLinks::Refuse => {
            if !root.is_absolute() {
                return Err(refused());
            }
            let mut dir = open_at(None, c"/", DIR_FLAGS)?;
            for part in plain_names(root).ok_or_else(refused)? {
                dir = open_at(Some(&dir), &c_name(part)?, DIR_FLAGS | libc::O_NOFOLLOW)?;
            }
            dir
        }
    };
    for part in parts {
        dir = open_at(Some(&dir), &c_name(part)?, DIR_FLAGS | libc::O_NOFOLLOW)?;
    }
    Ok((dir, c_name(name)?))
}

/// How a folder on the way is opened: as a handle to walk from, which
/// needs only search permission on it.
const DIR_FLAGS: libc::c_int = libc::O_PATH | libc::O_DIRECTORY;

/// The plain names of `path` past its root, or `None` when it holds `.`
/// or `..`.
fn plain_names(path: &Path) -> Option<Vec<&OsStr>> {
    path.components()
        .filter(|part| !matches!(part, Component::RootDir))
        .map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect()
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name holds a NUL byte"))
}

/// `openat` one name in `dir`, or a path from the working folder when
/// there is no `dir`.
fn open_at(dir: Option<&OwnedFd>, name: &CStr, flags: libc::c_int) -> io::Result<OwnedFd> {
    let dir = dir.map_or(libc::AT_FDCWD, |dir| dir.as_raw_fd());
    // SAFETY: `dir` is an open descriptor or `AT_FDCWD`, and `name` a
    // NUL-terminated string, both alive for the call.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by a successful `openat` and nothing
    // else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::path::PathBuf;

    use super::*;

    /// No link below the folder is followed, so a folder swapped for a
    /// link after any earlier check is refused at the read itself. The
    /// folder may be a link only when the caller says so.
    #[test]
    fn the_opener_never_follows_a_link_below_its_folder() {
        let scratch = crate::db::scratch::ScratchDir::new("beneath-open").expect("scratch");
        let base = scratch.path().canonicalize().expect("scratch path");
        let (music, outside) = (base.join("music"), base.join("outside"));
        std::fs::create_dir_all(music.join("Artist/Album")).expect("dir");
        std::fs::create_dir_all(outside.join("Album")).expect("dir");
        std::fs::write(music.join("Artist/Album/cover.png"), b"inside").expect("file");
        std::fs::write(outside.join("Album/cover.png"), b"private").expect("file");
        std::os::unix::fs::symlink(&music, base.join("alias")).expect("link");
        let read = |root: &Path, path: &Path, links| {
            let mut bytes = Vec::new();
            open_file_beneath(root, path, links)?.read_to_end(&mut bytes)?;
            io::Result::Ok(bytes)
        };
        let cover = music.join("Artist/Album/cover.png");
        let aliased = base.join("alias/Artist/Album/cover.png");

        for links in [RootLinks::Follow, RootLinks::Refuse] {
            assert_eq!(read(&music, &cover, links).expect("plain"), b"inside");
        }
        assert_eq!(
            read(&base.join("alias"), &aliased, RootLinks::Follow).expect("linked folder"),
            b"inside"
        );
        assert!(read(&base.join("alias"), &aliased, RootLinks::Refuse).is_err());
        assert!(read(&PathBuf::from("music"), &cover, RootLinks::Refuse).is_err());
        assert!(
            read(&music, &music, RootLinks::Follow).is_err(),
            "the folder itself"
        );
        assert!(
            read(&music, &music.join("Artist"), RootLinks::Follow).is_err(),
            "a folder"
        );

        let dotted = music.join("Artist/../Artist/Album/cover.png");
        assert!(read(&music, &dotted, RootLinks::Follow).is_err(), "..");
        assert!(read(&music, &outside.join("Album/cover.png"), RootLinks::Follow).is_err());

        std::os::unix::fs::symlink(
            outside.join("Album/cover.png"),
            music.join("Artist/link.png"),
        )
        .expect("link");
        assert!(read(&music, &music.join("Artist/link.png"), RootLinks::Follow).is_err());

        std::fs::rename(music.join("Artist"), base.join("held")).expect("move");
        std::os::unix::fs::symlink(&outside, music.join("Artist")).expect("link");
        for links in [RootLinks::Follow, RootLinks::Refuse] {
            assert!(
                read(&music, &cover, links).is_err(),
                "a parent swapped for a link"
            );
        }
    }
}
