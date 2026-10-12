//! Putting a publisher-made hidden file onto a name the user sees,
//! without being able to replace a file that is already there.
//!
//! A plain `rename(2)` replaces whatever holds the destination name, so a
//! file another program creates between the publisher's check and its
//! rename is lost. [`place_from_hidden`] uses the steps that cannot do
//! that, in order:
//!
//! 1. A no-replace rename (`renameat2` with `RENAME_NOREPLACE`, Linux).
//! 2. A hard link to the destination, then removing the hidden name.
//!    NFS has no no-replace rename (measured on NFSv3, Linux 6.12: EINVAL
//!    onto a free name), so this is the step network libraries take.
//! 3. Check that the name is free, then a plain rename. Only on a root
//!    whose probe found neither of the first two ([`PlacementMode::Check`]).
//!
//! Only the hidden name is ever removed. A destination that already is
//! the hidden file (same device and inode) means an earlier attempt
//! linked it and stopped: the hidden name goes and the file counts as
//! placed.

use std::path::Path;

use super::paths;

/// Folder the probe works in, at the top of a root. The reserved prefix
/// keeps scan and the watcher away from it.
pub const PROBE_DIR: &str = ".droppedneedle-management-probe";

/// How files can be placed under one library root, as its probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementMode {
    /// Not probed yet, or the probe could not tell. Treated like a safe
    /// root: steps 1 and 2 only.
    Unknown,
    /// The no-replace rename works.
    Rename,
    /// No no-replace rename, but hard links work.
    Link,
    /// Neither works. Files are placed by check-then-rename, which
    /// another program can race.
    Check,
}

/// Which step placed the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    /// The no-replace rename.
    Rename,
    /// Link then unlink, or an earlier link found in place.
    Link,
    /// Check then rename, on a [`PlacementMode::Check`] root.
    Checked,
}

/// Why a file was not placed. Its own type so callers can tell a taken
/// name from a filesystem that cannot place safely.
#[derive(Debug, thiserror::Error)]
pub enum PlaceError {
    /// The destination holds another file. Nothing was changed.
    #[error("the destination name is taken")]
    Collision,
    /// The two names are on different filesystems.
    #[error("the destination is on another filesystem")]
    CrossDevice,
    /// Neither safe step works for this file and the root is not a
    /// [`PlacementMode::Check`] root. Nothing was moved. Carries the errno
    /// the hard link answered.
    #[error("no safe way to place the file (os error {0})")]
    Unavailable(i32),
    /// The file is placed, but a parent folder could not be synced.
    #[error("placed, but the folder was not synced: {0}")]
    Unsynced(std::io::Error),
    /// Tests only: stopped between the link and the unlink.
    #[error("stopped after the link")]
    Interrupted,
    /// Any other filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl PlaceError {
    /// The publisher error for a file that could not be placed at
    /// `root:rel`.
    pub(super) fn into_publish(self, root: &str, rel: &str) -> super::PublishError {
        use super::PublishError;
        match self {
            Self::Collision => PublishError::Collision(format!("late occupier at {root}:{rel}")),
            Self::CrossDevice | Self::Unavailable(_) => {
                PublishError::NoSafePlacement(NO_SAFE_METHOD.to_owned())
            }
            Self::Interrupted => PublishError::InjectedCrash("after-link".to_owned()),
            Self::Unsynced(error) | Self::Io(error) => PublishError::Io(error.to_string()),
        }
    }
}

/// What a file fails with when no safe step can place it.
pub const NO_SAFE_METHOD: &str = "This folder has no safe way to add a file without risking an \
     overwrite, so the file was left alone.";

/// Test seams. Production always passes the default.
#[derive(Debug, Clone, Copy, Default)]
pub struct Seams {
    /// Start at step 2, as a filesystem without a no-replace rename does.
    pub skip_rename: bool,
    /// Step 2 answers this errno instead of linking.
    pub link_errno: Option<i32>,
    /// Stop after a good link, before the hidden name is removed.
    pub stop_after_link: bool,
}

/// Place `hidden_from` at `to`, never replacing a file at `to`. `mode` is
/// the placement mode of the root `to` is under. Syncs the parent
/// directory, both when they differ; a sync that fails after the file is
/// placed is [`PlaceError::Unsynced`].
pub fn place_from_hidden(
    mode: PlacementMode,
    hidden_from: &Path,
    to: &Path,
) -> Result<Placed, PlaceError> {
    place_with(mode, hidden_from, to, Seams::default())
}

/// [`place_from_hidden`] with test seams.
pub(super) fn place_with(
    mode: PlacementMode,
    hidden_from: &Path,
    to: &Path,
    seams: Seams,
) -> Result<Placed, PlaceError> {
    let placed = run_steps(mode, hidden_from, to, seams)?;
    let to_parent = to.parent();
    if let Some(parent) = to_parent {
        sync_dir(parent).map_err(PlaceError::Unsynced)?;
    }
    if let Some(parent) = hidden_from.parent()
        && Some(parent) != to_parent
    {
        sync_dir(parent).map_err(PlaceError::Unsynced)?;
    }
    Ok(placed)
}

fn run_steps(
    mode: PlacementMode,
    hidden_from: &Path,
    to: &Path,
    seams: Seams,
) -> Result<Placed, PlaceError> {
    if !seams.skip_rename
        && let Some(renamed) = rename_noreplace(hidden_from, to)
    {
        match renamed {
            Ok(()) => return Ok(Placed::Rename),
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                return already_linked(hidden_from, to);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PlaceError::Io(error));
            }
            // Anything else (EINVAL on NFS, ENOSYS, EPERM under seccomp)
            // goes to the link, which cannot replace a file either.
            Err(_) => {}
        }
    }
    let linked = match seams.link_errno {
        Some(code) => Err(std::io::Error::from_raw_os_error(code)),
        None => std::fs::hard_link(hidden_from, to),
    };
    let errno = match linked {
        Ok(()) => {
            if seams.stop_after_link {
                return Err(PlaceError::Interrupted);
            }
            remove_hidden(hidden_from)?;
            return Ok(Placed::Link);
        }
        Err(error) => match error.raw_os_error() {
            Some(libc::EEXIST) => return already_linked(hidden_from, to),
            Some(libc::EXDEV) => return Err(PlaceError::CrossDevice),
            Some(code) if no_link_support(code) || code == libc::EMLINK => code,
            _ => return Err(PlaceError::Io(error)),
        },
    };
    if mode != PlacementMode::Check {
        tracing::warn!(
            file = %to.display(),
            errno,
            "no safe way to place this file; it was not placed"
        );
        return Err(PlaceError::Unavailable(errno));
    }
    match std::fs::symlink_metadata(to) {
        Ok(_) => Err(PlaceError::Collision),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(hidden_from, to)?;
            Ok(Placed::Checked)
        }
        Err(error) => Err(PlaceError::Io(error)),
    }
}

/// The destination name was taken. If it is the hidden file itself, an
/// earlier attempt linked it (a crash before the unlink, or an NFS reply
/// lost on a link that succeeded): finish by removing the hidden name.
/// Anything else is someone else's file.
fn already_linked(hidden_from: &Path, to: &Path) -> Result<Placed, PlaceError> {
    if !paths::same_file(hidden_from, to) {
        return Err(PlaceError::Collision);
    }
    remove_hidden(hidden_from)?;
    Ok(Placed::Link)
}

/// Remove the hidden name. Already gone counts as done; any other
/// failure is returned, so a file never counts as placed under two names.
fn remove_hidden(hidden: &Path) -> Result<(), PlaceError> {
    match std::fs::remove_file(hidden) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PlaceError::Io(error)),
    }
}

/// Errnos that mean the filesystem has no hard links at all.
fn no_link_support(code: i32) -> bool {
    code == libc::EPERM || code == libc::EOPNOTSUPP || code == libc::ENOSYS
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// `renameat2(RENAME_NOREPLACE)`. `None` where the platform has no such
/// call. Called through `syscall` because not every libc wraps it.
#[cfg(target_os = "linux")]
fn rename_noreplace(from: &Path, to: &Path) -> Option<std::io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let call = || {
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        let status = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    Some(call())
}

#[cfg(not(target_os = "linux"))]
fn rename_noreplace(_from: &Path, _to: &Path) -> Option<std::io::Result<()>> {
    None
}

/// Find out how files can be placed under `root`, by trying each step on
/// two scratch files in [`PROBE_DIR`]. Writes nothing else and removes
/// what it wrote.
pub fn probe(root: &Path) -> PlacementMode {
    probe_with(root, Seams::default())
}

fn probe_with(root: &Path, seams: Seams) -> PlacementMode {
    let dir = root.join(PROBE_DIR);
    if let Err(error) = clear_probe(&dir) {
        tracing::warn!(root = %root.display(), %error, "placement probe could not start");
        return PlacementMode::Unknown;
    }
    let measured = measure(&dir, seams);
    let cleared = clear_probe(&dir);
    let mode = match (measured, cleared) {
        (Ok(mode), Ok(())) => mode,
        (Err(error), _) | (Ok(_), Err(error)) => {
            tracing::warn!(
                root = %root.display(),
                errno = error.raw_os_error(),
                %error,
                "placement probe gave no answer"
            );
            PlacementMode::Unknown
        }
    };
    if mode == PlacementMode::Check {
        tracing::warn!(
            root = %root.display(),
            "this library's disk has neither a no-replace rename nor hard links; \
             files are placed by check-then-rename, which another program can race"
        );
    }
    mode
}

/// Remove what a probe left: the two files and the folder. A folder
/// holding anything else, or a name that is not a real folder, is left
/// exactly as it is and reported.
fn clear_probe(dir: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(std::io::Error::other("the probe name is not a folder"));
        }
        Ok(_) => {}
    }
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        if name != "a" && name != "b" {
            return Err(std::io::Error::other("the probe folder holds another file"));
        }
    }
    for name in ["a", "b"] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    std::fs::remove_dir(dir)
}

/// The probe proper. Both attempts go onto a free name: with the name
/// taken the kernel answers EEXIST before it asks the filesystem, so a
/// filesystem without a no-replace rename would look as if it had one.
fn measure(dir: &Path, seams: Seams) -> std::io::Result<PlacementMode> {
    let a = dir.join("a");
    let b = dir.join("b");
    std::fs::create_dir(dir)?;
    std::fs::File::create_new(&a)?;
    if !seams.skip_rename && matches!(rename_noreplace(&a, &b), Some(Ok(()))) {
        return Ok(PlacementMode::Rename);
    }
    let linked = match seams.link_errno {
        Some(code) => Err(std::io::Error::from_raw_os_error(code)),
        None => std::fs::hard_link(&a, &b),
    };
    match linked {
        Ok(()) if link_count(&a)? == 2 => Ok(PlacementMode::Link),
        Ok(()) => Err(std::io::Error::other("link count is not 2 after a link")),
        // Only a clear "no hard links here" turns on check-then-rename; a
        // passing error must not.
        Err(error)
            if error.raw_os_error().is_some_and(no_link_support)
                && std::fs::symlink_metadata(&a).is_ok_and(|meta| meta.is_file()) =>
        {
            Ok(PlacementMode::Check)
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn link_count(path: &Path) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(std::fs::symlink_metadata(path)?.nlink())
}

#[cfg(not(unix))]
fn link_count(path: &Path) -> std::io::Result<u64> {
    std::fs::symlink_metadata(path).map(|_| 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tooling::scratch::ScratchDir;

    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("lists")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// A taken name is refused by every step and both files keep their
    /// bytes; a free name is placed and the hidden name is gone.
    #[test]
    fn rename_and_link_steps_never_replace() {
        for seams in [
            Seams::default(),
            Seams {
                skip_rename: true,
                ..Seams::default()
            },
        ] {
            let scratch = ScratchDir::new("place-steps").expect("scratch dir");
            let hidden = scratch.path().join(".hidden");
            let to = scratch.path().join("song.flac");
            std::fs::write(&hidden, b"staged").expect("writes");
            std::fs::write(&to, b"theirs").expect("writes");
            assert!(matches!(
                place_with(PlacementMode::Rename, &hidden, &to, seams),
                Err(PlaceError::Collision)
            ));
            assert_eq!(std::fs::read(&to).expect("reads"), b"theirs");
            assert_eq!(std::fs::read(&hidden).expect("reads"), b"staged");

            std::fs::remove_file(&to).expect("removes");
            let placed = place_with(PlacementMode::Rename, &hidden, &to, seams).expect("placed");
            let expected = if seams.skip_rename {
                Placed::Link
            } else {
                Placed::Rename
            };
            assert_eq!(placed, expected);
            assert_eq!(listing(scratch.path()), ["song.flac"]);
            assert_eq!(std::fs::read(&to).expect("reads"), b"staged");
        }
    }

    /// A destination that is already a link of the hidden file was placed
    /// by an earlier attempt: only the hidden name goes.
    #[test]
    fn an_earlier_link_is_finished_from_either_step() {
        for seams in [
            Seams::default(),
            Seams {
                skip_rename: true,
                ..Seams::default()
            },
        ] {
            let scratch = ScratchDir::new("place-linked").expect("scratch dir");
            let hidden = scratch.path().join(".hidden");
            let to = scratch.path().join("song.flac");
            std::fs::write(&hidden, b"staged").expect("writes");
            let stop = Seams {
                skip_rename: true,
                stop_after_link: true,
                ..Seams::default()
            };
            assert!(matches!(
                place_with(PlacementMode::Unknown, &hidden, &to, stop),
                Err(PlaceError::Interrupted)
            ));
            assert_eq!(listing(scratch.path()), [".hidden", "song.flac"]);
            let placed = place_with(PlacementMode::Unknown, &hidden, &to, seams).expect("placed");
            assert_eq!(placed, Placed::Link);
            assert_eq!(listing(scratch.path()), ["song.flac"]);
            assert_eq!(std::fs::read(&to).expect("reads"), b"staged");
        }
    }

    /// With no safe step, a file under a safe root is not moved at all;
    /// only a `Check` root falls back to check-then-rename, and that still
    /// refuses a taken name.
    #[test]
    fn check_then_rename_runs_only_on_a_check_root() {
        let no_safe_step = Seams {
            skip_rename: true,
            link_errno: Some(libc::EPERM),
            ..Seams::default()
        };
        let scratch = ScratchDir::new("place-check").expect("scratch dir");
        let hidden = scratch.path().join(".hidden");
        let to = scratch.path().join("song.flac");
        std::fs::write(&hidden, b"staged").expect("writes");

        for mode in [
            PlacementMode::Rename,
            PlacementMode::Link,
            PlacementMode::Unknown,
        ] {
            assert!(matches!(
                place_with(mode, &hidden, &to, no_safe_step),
                Err(PlaceError::Unavailable(code)) if code == libc::EPERM
            ));
            assert_eq!(listing(scratch.path()), [".hidden"], "{mode:?}");
        }

        std::fs::write(&to, b"theirs").expect("writes");
        assert!(matches!(
            place_with(PlacementMode::Check, &hidden, &to, no_safe_step),
            Err(PlaceError::Collision)
        ));
        assert_eq!(std::fs::read(&to).expect("reads"), b"theirs");
        std::fs::remove_file(&to).expect("removes");
        let placed = place_with(PlacementMode::Check, &hidden, &to, no_safe_step).expect("placed");
        assert_eq!(placed, Placed::Checked);
        assert_eq!(listing(scratch.path()), ["song.flac"]);
    }

    /// The probe answers from what the disk does, cleans up after itself
    /// and after a crashed run, and turns on check-then-rename only for a
    /// clear "no hard links" answer.
    #[test]
    fn probe_answers_and_leaves_nothing_behind() {
        let scratch = ScratchDir::new("place-probe").expect("scratch dir");
        let root = scratch.path();
        let leftover = root.join(PROBE_DIR);
        std::fs::create_dir(&leftover).expect("creates");
        std::fs::write(leftover.join("a"), b"").expect("writes");
        std::fs::write(leftover.join("b"), b"").expect("writes");

        let expected = if cfg!(target_os = "linux") {
            PlacementMode::Rename
        } else {
            PlacementMode::Link
        };
        assert_eq!(probe(root), expected);
        assert!(listing(root).is_empty());

        let link_only = Seams {
            skip_rename: true,
            ..Seams::default()
        };
        assert_eq!(probe_with(root, link_only), PlacementMode::Link);
        let no_links = Seams {
            link_errno: Some(libc::EOPNOTSUPP),
            ..link_only
        };
        assert_eq!(probe_with(root, no_links), PlacementMode::Check);
        let passing_error = Seams {
            link_errno: Some(libc::EIO),
            ..link_only
        };
        assert_eq!(probe_with(root, passing_error), PlacementMode::Unknown);
        assert!(listing(root).is_empty());

        // A probe folder holding someone else's file is not emptied.
        std::fs::create_dir(&leftover).expect("creates");
        std::fs::write(leftover.join("keep"), b"theirs").expect("writes");
        std::fs::write(leftover.join("a"), b"theirs too").expect("writes");
        assert_eq!(probe(root), PlacementMode::Unknown);
        assert_eq!(listing(&leftover), ["a", "keep"]);
        assert_eq!(
            std::fs::read(leftover.join("keep")).expect("reads"),
            b"theirs"
        );
    }
}
