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
//!
//! The other direction, a user's file moved out of the way under a hidden
//! name, is `set_aside_with`. It never links then unlinks, because
//! there the name to unlink would be the user's.

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
        let error = match self {
            Self::Collision => {
                PublishError::Collision(format!("late occupier at {root}:{rel}").into())
            }
            Self::CrossDevice | Self::Unavailable(_) => {
                PublishError::NoSafePlacement(NO_SAFE_METHOD.into())
            }
            Self::Interrupted => PublishError::InjectedCrash(super::CrashPoint::AfterLink),
            Self::Unsynced(error) | Self::Io(error) => PublishError::Io(error.to_string().into()),
        };
        error.file(root, rel)
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
    /// The link test makes its link and then answers EEXIST, as NFS does
    /// when the reply to a LINK that succeeded is lost and the retry finds
    /// the name taken. Wins over `link_errno` for the link test only.
    pub lost_link_reply: bool,
    /// Every step answers EXDEV, as two filesystems do.
    pub cross_device: bool,
    /// The folder sync after a good placement fails.
    pub unsynced: bool,
    /// A copy to another disk stops part way with a write error.
    pub short_write: bool,
    /// A put-back copies its file instead of linking it, as when the
    /// recycle bin is on another disk.
    pub copy_put_back: bool,
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
    if seams.unsynced {
        return Err(PlaceError::Unsynced(std::io::Error::other(
            "folder sync refused by a test seam",
        )));
    }
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
    if seams.cross_device {
        return Err(PlaceError::CrossDevice);
    }
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

/// A user's file that is about to be moved out of the way.
#[derive(Debug, Clone, Copy)]
pub struct Original<'a> {
    /// The name it holds now.
    pub visible: &'a Path,
    /// The journaled hidden name it moves to.
    pub hidden: &'a Path,
    /// The name it goes back to. Only a case-only rename makes this differ
    /// from `visible`: the original spelling, not the new one.
    pub home: &'a Path,
    /// The bytes it held when it was last read. `None` skips the check.
    pub sha256: Option<&'a str>,
    /// The device of the library folder it is under, when known. On a
    /// folder whose probe found hard links work, a file on that same
    /// device that refuses a link is not the server's to change, or is
    /// locked; anywhere else the refusal can also mean no hard links.
    pub root_dev: Option<u64>,
    /// Its identity when the caller already knows it holds `sha256`: then
    /// a set-aside file that is still that file is not read again.
    pub known: Option<super::paths::FileStat>,
}

/// Why a user's file was not set aside.
#[derive(Debug, thiserror::Error)]
pub enum SetAsideError {
    /// The file cannot be hard-linked, so it could be set aside but never
    /// put back by a safe step. Nothing was moved.
    #[error("the file cannot be hard-linked (os error {errno})")]
    NotLinkable {
        errno: i32,
        /// Hard links are known to work where the file is, so the refusal
        /// is about the file itself.
        links_work: bool,
    },
    /// The folder has no safe step to put the file back with. Nothing was
    /// moved.
    #[error("no safe way to put the file back (os error {0})")]
    Unavailable(i32),
    /// The hidden name holds another file. Nothing was moved.
    #[error("the hidden name is taken")]
    HiddenTaken,
    /// Another program replaced the file after it was last read. What was
    /// set aside is back under its own name.
    #[error("the file was replaced while it was being set aside")]
    Replaced,
    /// As `Replaced`, but the replacement could not be put back. It is
    /// kept under the hidden name.
    #[error("the file was replaced, and its replacement could not be put back: {0}")]
    ReplacedKept(PlaceError),
    /// Any other filesystem failure.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl SetAsideError {
    /// The publisher error for a file at `root:rel` that was not set
    /// aside.
    pub(super) fn into_publish(self, root: &str, rel: &str) -> super::PublishError {
        use super::PublishError;
        let error = match self {
            Self::NotLinkable {
                links_work: true, ..
            } => PublishError::NotLinkable(
                format!(
                    "DroppedNeedle can't safely change \"{rel}\": it does not own the file and \
                 cannot write to it, or the file is locked against changes."
                )
                .into(),
            ),
            Self::NotLinkable {
                links_work: false, ..
            } => PublishError::NotLinkable(
                format!(
                    "DroppedNeedle can't safely change \"{rel}\": it does not own the file and \
                 cannot write to it, the file is locked against changes, or the disk it is on \
                 does not support hard links."
                )
                .into(),
            ),
            Self::Unavailable(_) => PublishError::NoSafePlacement(NO_SAFE_METHOD.into()),
            Self::HiddenTaken => PublishError::Journal(
                format!("an earlier change left a set-aside copy of {root}:{rel} behind").into(),
            ),
            Self::Replaced | Self::ReplacedKept(_) => PublishError::Collision(
                format!("{root}:{rel} was replaced while it was being changed").into(),
            ),
            Self::Io(error) => PublishError::Io(error.to_string().into()),
        };
        error.file(root, rel)
    }
}

/// Set a user's file aside under its journaled hidden name, in a way that
/// can neither lose it nor strand it there.
///
/// 1. Where the way back would be a hard link (a `Link` or `Unknown`
///    root), the link is tried first, onto the hidden name, and removed
///    again. A file that cannot be linked is refused before it moves.
/// 2. The file moves by [`set_aside_to_hidden`].
/// 3. What arrived under the hidden name is hashed. The last read of the
///    file can be a whole album old, and a file saved since by another
///    program is not the one this journal is about: it goes straight back
///    to `home` through [`place_from_hidden`].
///
/// `before_move` is a test seam: it runs with the visible name after the
/// first link test, just before the file moves.
pub(super) fn set_aside_with(
    mode: PlacementMode,
    original: Original<'_>,
    seams: Seams,
    before_move: Option<&dyn Fn(&Path)>,
) -> Result<(), SetAsideError> {
    let Original {
        visible,
        hidden,
        home,
        sha256,
        root_dev,
        known,
    } = original;
    if matches!(mode, PlacementMode::Link | PlacementMode::Unknown) {
        let links_work = mode == PlacementMode::Link
            && root_dev.is_some_and(|root_dev| {
                use std::os::unix::fs::MetadataExt as _;
                std::fs::symlink_metadata(visible).is_ok_and(|meta| meta.dev() == root_dev)
            });
        link_test(visible, hidden, seams, links_work)?;
    }
    if let Some(hook) = before_move {
        hook(visible);
    }
    // A rename root puts a file back by the no-replace rename, whoever
    // owns it, so it needs no link test. Unless that rename does not work
    // for this file after all (a different disk mounted below the root):
    // then the way back is a link here too.
    set_aside_to_hidden(visible, hidden, seams, mode == PlacementMode::Rename)?;
    if let Some(parent) = visible.parent() {
        sync_dir(parent)?;
    }
    if let Some(parent) = hidden.parent()
        && Some(parent) != visible.parent()
    {
        sync_dir(parent)?;
    }
    let Some(expected) = sha256 else {
        return Ok(());
    };
    let still_known = known
        .is_some_and(|known| paths::FileStat::read(hidden).is_ok_and(|now| known.same_moved(&now)));
    let intact = still_known || paths::holds(hidden, expected);
    if intact {
        return Ok(());
    }
    match place_with(mode, hidden, home, seams) {
        Ok(_) => Err(SetAsideError::Replaced),
        Err(PlaceError::Unsynced(error)) => {
            tracing::warn!(file = %home.display(), %error, "directory sync failed");
            Err(SetAsideError::Replaced)
        }
        Err(error) => {
            tracing::warn!(
                file = %home.display(),
                kept = %hidden.display(),
                %error,
                "a file replaced during a managed write could not be put back; it is kept"
            );
            Err(SetAsideError::ReplacedKept(error))
        }
    }
}

/// Move a user-visible file to a hidden name the publisher made: the
/// no-replace rename, else a check that the hidden name is free and a
/// plain rename.
///
/// Never link-then-unlink, on any root. That would remove the visible
/// name, and with it whatever another program saved there between the
/// link and the unlink. A plain rename moves whatever holds the name at
/// that instant, and the caller's hash check sees a swap. It is safe on
/// the hidden side: only the publisher makes these names, one per journal.
///
/// `link_test_before_plain_rename` runs [`link_test`] once the no-replace
/// rename has turned out not to work, before anything moves.
fn set_aside_to_hidden(
    from: &Path,
    hidden_to: &Path,
    seams: Seams,
    link_test_before_plain_rename: bool,
) -> Result<(), SetAsideError> {
    free_hidden_name(from, hidden_to)?;
    if !seams.skip_rename
        && let Some(renamed) = rename_noreplace(from, hidden_to)
    {
        match renamed {
            Ok(()) => return Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::EEXIST) => {
                return Err(SetAsideError::HiddenTaken);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(SetAsideError::Io(error));
            }
            Err(_) => {}
        }
    }
    if link_test_before_plain_rename {
        link_test(from, hidden_to, seams, false)?;
    }
    match std::fs::symlink_metadata(hidden_to) {
        Ok(_) => Err(SetAsideError::HiddenTaken),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(from, hidden_to)?;
            Ok(())
        }
        Err(error) => Err(SetAsideError::Io(error)),
    }
}

/// The hidden name has to be free before a file is set aside under it.
/// If it is the visible file itself (same device and inode), an earlier
/// put-back linked the file home and stopped: the hidden name goes.
/// Anything else there is not ours to move.
fn free_hidden_name(visible: &Path, hidden: &Path) -> Result<(), SetAsideError> {
    match std::fs::symlink_metadata(hidden) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SetAsideError::Io(error)),
        Ok(_) if paths::same_file(visible, hidden) => Ok(remove_hidden(hidden)?),
        Ok(_) => Err(SetAsideError::HiddenTaken),
    }
}

/// Prove the file can come back by a hard link before it is set aside:
/// link it to its hidden name, then remove that name. Only the hidden name
/// is ever removed. Linux refuses to link a file the caller neither owns
/// nor can write (`fs.protected_hardlinks`), and an immutable one, with
/// EPERM. So does a disk with no hard links at all, such as FAT.
/// `links_work` says hard links are known to work where the file is.
fn link_test(
    visible: &Path,
    hidden: &Path,
    seams: Seams,
    links_work: bool,
) -> Result<(), SetAsideError> {
    free_hidden_name(visible, hidden)?;
    let linked = match seams.link_errno {
        _ if seams.lost_link_reply => std::fs::hard_link(visible, hidden)
            .and_then(|()| Err(std::io::Error::from_raw_os_error(libc::EEXIST))),
        Some(code) => Err(std::io::Error::from_raw_os_error(code)),
        None => std::fs::hard_link(visible, hidden),
    };
    let error = match linked {
        Ok(()) => return Ok(remove_hidden(hidden)?),
        Err(error) => error,
    };
    let refusal = match error.raw_os_error() {
        // The name was free a moment ago, so a hidden name that is this
        // file is our own link: its reply was lost and the retry found it.
        Some(libc::EEXIST) if paths::same_file(visible, hidden) => {
            return Ok(remove_hidden(hidden)?);
        }
        Some(libc::EEXIST) => SetAsideError::HiddenTaken,
        Some(libc::EPERM) => SetAsideError::NotLinkable {
            errno: libc::EPERM,
            links_work,
        },
        Some(code) if no_link_support(code) || code == libc::EMLINK || code == libc::EXDEV => {
            SetAsideError::Unavailable(code)
        }
        _ => return Err(SetAsideError::Io(error)),
    };
    tracing::warn!(
        file = %visible.display(),
        errno = error.raw_os_error(),
        "this file could not be put back safely once set aside; it was left alone"
    );
    Err(refusal)
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
fn remove_hidden(hidden: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(hidden) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
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
    match (measured, cleared) {
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
    }
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
    use droppedneedle_kernel::db::scratch::ScratchDir;

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

    fn aside<'a>(visible: &'a Path, hidden: &'a Path, home: &'a Path) -> Original<'a> {
        Original {
            visible,
            hidden,
            home,
            sha256: Some("not-checked-here"),
            root_dev: None,
            known: None,
        }
    }

    /// A file is refused before it moves when it could not come back: its
    /// hidden name is someone else's, or it cannot be linked where a link
    /// is the way back. A root that puts back by rename asks for no link.
    #[test]
    fn a_file_that_could_not_come_back_is_not_set_aside() {
        let no_links = Seams {
            link_errno: Some(libc::EPERM),
            ..Seams::default()
        };
        let no_safe_step = Seams {
            skip_rename: true,
            ..no_links
        };
        let scratch = ScratchDir::new("aside-refused").expect("scratch dir");
        let visible = scratch.path().join("song.flac");
        let hidden = scratch.path().join(".hidden");
        std::fs::write(&visible, b"theirs").expect("writes");
        let untouched = || {
            assert_eq!(listing(scratch.path()), ["song.flac"]);
            assert_eq!(std::fs::read(&visible).expect("reads"), b"theirs");
        };

        for (mode, seams) in [
            (PlacementMode::Link, no_links),
            (PlacementMode::Unknown, no_links),
            (PlacementMode::Rename, no_safe_step),
        ] {
            let refused = set_aside_with(mode, aside(&visible, &hidden, &visible), seams, None);
            assert!(
                matches!(refused, Err(SetAsideError::NotLinkable { .. })),
                "{mode:?}: got {refused:?}"
            );
            untouched();
        }

        std::fs::write(&hidden, b"someone else's").expect("writes");
        for seams in [
            Seams::default(),
            Seams {
                skip_rename: true,
                ..Seams::default()
            },
        ] {
            for mode in [PlacementMode::Rename, PlacementMode::Link] {
                let refused = set_aside_with(mode, aside(&visible, &hidden, &visible), seams, None);
                assert!(
                    matches!(refused, Err(SetAsideError::HiddenTaken)),
                    "{mode:?}: got {refused:?}"
                );
                assert_eq!(std::fs::read(&visible).expect("reads"), b"theirs");
                assert_eq!(std::fs::read(&hidden).expect("reads"), b"someone else's");
            }
        }
        std::fs::remove_file(&hidden).expect("removes");

        // Each of these succeeds with every link but the link test's
        // refused, so the move itself never links.
        // - Rename root: the no-replace rename works, so no link is asked
        //   for at all.
        // - Check root: no link test, and the move is a plain rename.
        // - Link root: the link test passes (its link was made and the
        //   reply lost, which also shows a retried link is not taken for
        //   someone else's file), then the file moves by a plain rename. A
        //   set-aside that linked then unlinked would be refused here.
        let only_the_link_test = Seams {
            lost_link_reply: true,
            ..no_safe_step
        };
        for (mode, seams) in [
            (PlacementMode::Rename, no_links),
            (PlacementMode::Check, no_safe_step),
            (PlacementMode::Link, only_the_link_test),
        ] {
            let original = Original {
                sha256: None,
                ..aside(&visible, &hidden, &visible)
            };
            set_aside_with(mode, original, seams, None).expect("set aside");
            assert_eq!(listing(scratch.path()), [".hidden"]);
            std::fs::rename(&hidden, &visible).expect("renames");
        }
    }

    /// A file another program saves over the original while it is being
    /// set aside is never deleted and never left hidden: it goes back to
    /// the original's home name. If that name is taken too, it is kept
    /// under the hidden name. A hidden name left by an interrupted
    /// put-back is cleared first.
    #[test]
    fn a_file_swapped_in_during_a_set_aside_is_kept() {
        let sha = super::super::snapshots::sha256_hex(b"original");
        let link_only = Seams {
            skip_rename: true,
            ..Seams::default()
        };
        let swap = |visible: &Path| {
            let temp = visible.with_extension("tagger");
            std::fs::write(&temp, b"saved since").expect("writes");
            std::fs::rename(&temp, visible).expect("renames");
        };
        for seams in [Seams::default(), link_only] {
            let scratch = ScratchDir::new("aside-swapped").expect("scratch dir");
            let visible = scratch.path().join("song.flac");
            let hidden = scratch.path().join(".hidden");
            let original = Original {
                visible: &visible,
                hidden: &hidden,
                home: &visible,
                sha256: Some(&sha),
                root_dev: None,
                known: None,
            };
            std::fs::write(&visible, b"original").expect("writes");
            std::fs::hard_link(&visible, &hidden).expect("links");

            set_aside_with(PlacementMode::Link, original, seams, None).expect("set aside");
            assert_eq!(listing(scratch.path()), [".hidden"]);
            assert_eq!(std::fs::read(&hidden).expect("reads"), b"original");
            std::fs::rename(&hidden, &visible).expect("renames");

            let swapped = set_aside_with(PlacementMode::Link, original, seams, Some(&swap));
            assert!(
                matches!(swapped, Err(SetAsideError::Replaced)),
                "got {swapped:?}"
            );
            assert_eq!(listing(scratch.path()), ["song.flac"]);
            assert_eq!(std::fs::read(&visible).expect("reads"), b"saved since");

            let home = scratch.path().join("Song.flac");
            std::fs::write(&visible, b"original").expect("writes");
            std::fs::write(&home, b"also theirs").expect("writes");
            let away_from_home = Original {
                home: &home,
                ..original
            };
            let kept = set_aside_with(PlacementMode::Link, away_from_home, seams, Some(&swap));
            assert!(
                matches!(
                    kept,
                    Err(SetAsideError::ReplacedKept(PlaceError::Collision))
                ),
                "got {kept:?}"
            );
            assert_eq!(listing(scratch.path()), [".hidden", "Song.flac"]);
            assert_eq!(std::fs::read(&hidden).expect("reads"), b"saved since");
            assert_eq!(std::fs::read(&home).expect("reads"), b"also theirs");
        }
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
