//! Counting the audio files below a folder from its listings alone.
//!
//! Each folder is opened from the one above it with `O_NOFOLLOW`, so a
//! folder swapped for a link after it was listed fails to open instead of
//! being followed. Files are never opened and nothing is written. Names
//! starting with `.` are passed over, the recycle bin and the app's
//! management folders among them.

use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::time::Instant;

use droppedneedle_library::scan::walk::is_audio_file;

/// Folders below the counted one that are opened at most.
const MAX_DEPTH: usize = 24;

/// Where a count stops.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limit {
    /// The count stops at this moment.
    pub(crate) until: Instant,
    /// The count stops after this many names.
    pub(crate) entries: u64,
}

/// What a count found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Count {
    /// Audio files found.
    pub(crate) audio: u64,
    /// Those at least two folders below the counted one, as in
    /// `Artist/Album/track.flac`.
    pub(crate) deep: u64,
    /// Names read.
    pub(crate) entries: u64,
    /// False when the limit stopped the count early.
    pub(crate) complete: bool,
}

/// Count the audio files below `root`, which is itself opened without
/// following a link and must still be the folder `(device, inode)` names,
/// the one checked before. A folder swapped in since is not counted.
/// Blocking.
pub(crate) fn count_audio(root: &Path, key: (u64, u64), limit: Limit) -> io::Result<Count> {
    let path = CString::new(root.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path holds a NUL"))?;
    let dir = Dir::open(libc::AT_FDCWD, &path)?;
    if dir.key()? != key {
        return Err(io::Error::other("the folder changed since it was checked"));
    }
    let mut count = Count {
        complete: true,
        ..Count::default()
    };
    walk(dir, 0, limit, &mut count);
    Ok(count)
}

fn walk(mut dir: Dir, depth: usize, limit: Limit, count: &mut Count) {
    while let Some((name, kind)) = dir.next() {
        count.entries += 1;
        if count.entries > limit.entries || Instant::now() >= limit.until {
            count.complete = false;
            return;
        }
        if name.to_bytes().starts_with(b".") {
            continue;
        }
        match kind.or_else(|| dir.kind_of(&name)) {
            Some(Kind::Dir) if depth < MAX_DEPTH => {
                // A folder that became a link, or cannot be opened, is
                // passed over.
                if let Ok(child) = Dir::open(dir.fd(), &name) {
                    walk(child, depth + 1, limit, count);
                    if !count.complete {
                        return;
                    }
                }
            }
            Some(Kind::File) if is_audio_file(Path::new(OsStr::from_bytes(name.to_bytes()))) => {
                count.audio += 1;
                if depth >= 2 {
                    count.deep += 1;
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Other,
}

/// An open folder being listed.
struct Dir(std::ptr::NonNull<libc::DIR>);

impl Dir {
    /// Open `name` in the folder `parent` for listing, failing on a link.
    fn open(parent: libc::c_int, name: &CStr) -> io::Result<Self> {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `name` is a valid NUL-terminated string for the call.
        let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is an open folder descriptor this call takes over.
        let stream = unsafe { libc::fdopendir(fd) };
        match std::ptr::NonNull::new(stream) {
            Some(stream) => Ok(Self(stream)),
            None => {
                let error = io::Error::last_os_error();
                // SAFETY: `fdopendir` failed, so `fd` is still ours to close.
                unsafe { libc::close(fd) };
                Err(error)
            }
        }
    }

    /// The open folder's device and inode.
    fn key(&self) -> io::Result<(u64, u64)> {
        // SAFETY: an all-zero `stat` is a valid value for the call to fill.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: the folder is open for as long as `self` lives.
        if unsafe { libc::fstat(self.fd(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((stat.st_dev, stat.st_ino))
    }

    fn fd(&self) -> libc::c_int {
        // SAFETY: the stream is open for as long as `self` lives.
        unsafe { libc::dirfd(self.0.as_ptr()) }
    }

    /// The next name and, when the listing says, its kind.
    fn next(&mut self) -> Option<(CString, Option<Kind>)> {
        loop {
            // SAFETY: the stream is open; the entry is read before the next
            // call on the same stream.
            let entry = unsafe { libc::readdir(self.0.as_ptr()) };
            if entry.is_null() {
                return None;
            }
            // SAFETY: `readdir` answered a valid entry, whose name is
            // NUL-terminated.
            let (name, kind) =
                unsafe { (CStr::from_ptr((*entry).d_name.as_ptr()), (*entry).d_type) };
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            let kind = match kind {
                libc::DT_DIR => Some(Kind::Dir),
                libc::DT_REG => Some(Kind::File),
                libc::DT_UNKNOWN => None,
                _ => Some(Kind::Other),
            };
            return Some((name.to_owned(), kind));
        }
    }

    /// The kind of `name`, for a listing that does not say, read without
    /// following a link.
    fn kind_of(&self, name: &CStr) -> Option<Kind> {
        // SAFETY: an all-zero `stat` is a valid value for the call to fill.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: the folder is open and `name` is NUL-terminated.
        let read = unsafe {
            libc::fstatat(
                self.fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if read != 0 {
            return None;
        }
        Some(match stat.st_mode & libc::S_IFMT {
            libc::S_IFDIR => Kind::Dir,
            libc::S_IFREG => Kind::File,
            _ => Kind::Other,
        })
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: the stream is open and closed once, here.
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}
