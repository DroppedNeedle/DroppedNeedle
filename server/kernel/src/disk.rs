//! Free space on the filesystem that holds a path.

use std::io;
use std::path::Path;

/// Bytes an unprivileged process may still write on the filesystem holding
/// `path` (`f_bavail * f_frsize`). Blocking: call it where a blocking call
/// is allowed.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt as _;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `statvfs` is plain old data, so all-zero is a valid value.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a NUL-terminated string that outlives the call,
    // and `stat` is a valid, writable `statvfs`.
    let status = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    // The field widths differ by platform (u32 blocks on macOS).
    #[allow(clippy::unnecessary_cast)]
    let free = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
    Ok(free)
}
