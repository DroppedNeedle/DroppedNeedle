//! Local-filesystem enforcement for the live database.
//!
//! SQLite locking over a network filesystem is the canonical corruption
//! vector, so boot refuses to open the database anywhere but a local
//! filesystem, and refuses to follow a symlink to get there. Backup staging
//! stays on the same filesystem as the database so the final rename is
//! atomic.
//!
//! The check is a deny-list over statfs magics for filesystems whose locking
//! is remote or emulated (NFS, CIFS/SMB, 9P, Ceph, Lustre, AFS, FUSE mounts,
//! GFS2, OCFS2, PVFS2). Everything else passes, including overlayfs and
//! tmpfs: container layers and scratch directories are legitimate local homes
//! for the database, and SQLite locking is correct on them. A strict
//! allow-list would refuse Docker deployments and the test scratch area for
//! no safety gain.

use std::path::{Path, PathBuf};

use super::error::DbError;

/// statfs magics for filesystems refused at boot. Each is a network, cluster,
/// or emulated filesystem whose locking the v3 database must never depend
/// on. FUSE and FUSEBLK cover sshfs, glusterfs, s3fs, mergerfs, and ntfs-3g,
/// which all present remote or emulated locking behind a local mount.
/// Verified against `linux/magic.h`, `linux/gfs2_ondisk.h`, and `man statfs`
/// on Linux, except Lustre (out-of-tree; value from the Lustre source) and
/// FUSEBLK/PVFS2 (stable kernel values, no header on every distro). VxFS is
/// deliberately absent: it is a local filesystem, not a remote one.
const REMOTE_MAGICS: &[(i64, &str)] = &[
    (0x6969, "nfs"),
    (0xFF534D42, "cifs"),
    (0xFE534D42, "smb2"),
    (0x517B, "smb"),
    (0x01021997, "9p"),
    (0x00C36400, "ceph"),
    (0x0BD00BD0, "lustre"),
    (0x5346414F, "afs"),
    (0x73757245, "coda"),
    (0x564C, "ncpfs"),
    (0x47504653, "gpfs"),
    (0x65735546, "fuse"),
    (0x65735543, "fuseblk"),
    (0x01161970, "gfs2"),
    (0x7461636f, "ocfs2"),
    (0x20030528, "pvfs2"),
];

/// True when a statfs magic names a filesystem the database may live on.
///
/// Pure and total, so the brief pins the table directly without mounting a
/// network share.
pub fn filesystem_is_local(magic: i64) -> bool {
    !REMOTE_MAGICS.iter().any(|(known, _)| *known == magic)
}

/// Name a refused magic for the error, falling back to the hex value.
fn name_magic(magic: i64) -> String {
    REMOTE_MAGICS
        .iter()
        .find(|(known, _)| *known == magic)
        .map(|(_, name)| (*name).to_owned())
        .unwrap_or_else(|| format!("magic {magic:#x}"))
}

/// Read the statfs magic for the filesystem holding `path`.
#[cfg(target_os = "linux")]
fn statfs_magic(path: &Path) -> std::io::Result<i64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let bytes = CString::new(path.as_os_str().as_bytes())?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let status = unsafe { libc::statfs(bytes.as_ptr(), &mut stat) };
    if status != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(stat.f_type)
}

/// Non-Linux hosts have no checked table; the database boots there only in
/// development. The refusal is enforced where it matters.
#[cfg(not(target_os = "linux"))]
fn statfs_magic(_path: &Path) -> std::io::Result<i64> {
    Ok(0)
}

/// Find the mount point and filesystem type for `path` from the mount table.
#[cfg(target_os = "linux")]
fn mount_of(path: &Path) -> (String, String) {
    let table = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mut best: Option<(String, String)> = None;
    let mut best_len = 0;
    for line in table.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let separator = fields.iter().position(|field| *field == "-");
        let Some(dash) = separator else { continue };
        if fields.len() < 5 || fields.len() <= dash + 2 {
            continue;
        }
        let mount_point = fields[4].replace("\\040", " ");
        if path.starts_with(&mount_point) && mount_point.len() > best_len {
            best_len = mount_point.len();
            best = Some((mount_point, fields[dash + 1].to_owned()));
        }
    }
    best.unwrap_or_else(|| ("/".to_owned(), "unknown".to_owned()))
}

#[cfg(not(target_os = "linux"))]
fn mount_of(_path: &Path) -> (String, String) {
    ("/".to_owned(), "unknown".to_owned())
}

/// Refuse a database path with a symlink in any component, not just the
/// final one. A symlinked parent directory redirects the database as surely
/// as a symlinked file. Missing components are skipped: they cannot be
/// links, and the boot check runs before the file exists.
pub fn reject_symlink(path: &Path) -> Result<(), DbError> {
    for component in path.ancestors() {
        match std::fs::symlink_metadata(component) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(DbError::SymlinkRefused {
                    path: component.to_owned(),
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Refuse a database whose directory sits on a refused filesystem.
///
/// The error names the filesystem type and the mount point so the operator
/// knows what to move.
pub fn reject_remote_filesystem(path: &Path) -> Result<(), DbError> {
    let anchor = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let magic = statfs_magic(&anchor)?;
    if filesystem_is_local(magic) {
        return Ok(());
    }
    let (mount, fs_type) = mount_of(&anchor);
    let fs_type = if fs_type == "unknown" {
        name_magic(magic)
    } else {
        fs_type
    };
    Err(DbError::NonLocalFilesystem {
        path: path.to_owned(),
        fs_type,
        mount,
    })
}

/// True when both paths sit on the same filesystem (same device number).
pub fn same_filesystem(first: &Path, second: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let first_dev = std::fs::metadata(first)?.dev();
    let second_dev = std::fs::metadata(second)?.dev();
    Ok(first_dev == second_dev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_magics_refused_and_local_magics_pass() {
        for (magic, name) in REMOTE_MAGICS {
            assert!(!filesystem_is_local(*magic), "{name} must be refused");
        }
        for (magic, name) in [
            (0xEF53, "ext4"),
            (0x58465342, "xfs"),
            (0x9123683E, "btrfs"),
            (0x01021994, "tmpfs"),
            (0x794C7630, "overlayfs"),
        ] {
            assert!(filesystem_is_local(magic), "{name} must pass");
        }
    }

    #[test]
    fn symlink_paths_are_refused() {
        let dir = std::env::temp_dir().join(format!("db-fs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("library.db");
        std::fs::write(&target, b"x").unwrap();
        let link = dir.join("link.db");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            reject_symlink(&link),
            Err(DbError::SymlinkRefused { .. })
        ));
        assert!(reject_symlink(&target).is_ok());
        assert!(reject_symlink(&dir.join("missing.db")).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn symlinked_parent_directories_are_refused() {
        let dir = std::env::temp_dir().join(format!("db-fs-parent-{}", std::process::id()));
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link_dir = dir.join("linked");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link_dir).unwrap();
        let nested = link_dir.join("library.db");
        #[cfg(unix)]
        assert!(matches!(
            reject_symlink(&nested),
            Err(DbError::SymlinkRefused { .. })
        ));
        assert!(reject_symlink(&real.join("library.db")).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
