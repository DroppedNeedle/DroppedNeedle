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
//!
//! A database held in memory (tmpfs, ramfs) is lost with its backups when
//! the machine or container restarts, as on an Unraid box whose appdata
//! path names a pool it does not have. Boot logs a warning and the admin
//! backups list says so, but neither refuses: tests and some setups keep
//! the database in memory on purpose.

use std::path::{Path, PathBuf};

use super::error::DbError;

/// statfs magics for filesystems refused at boot. Each is a network, cluster,
/// or emulated filesystem whose locking the v3 database must never depend
/// on. FUSE and FUSEBLK cover sshfs, glusterfs, s3fs, mergerfs, and ntfs-3g,
/// which all present remote or emulated locking behind a local mount.
/// Verified against `linux/magic.h`, `linux/gfs2_ondisk.h`, and `man statfs`
/// on Linux, except Lustre (out-of-tree; value from the Lustre source) and
/// FUSEBLK/PVFS2 (stable kernel values, no header on every distro). VxFS is
/// absent on purpose: it is a local filesystem, not a remote one.
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
/// Pure and total, so a test pins the table directly without mounting a
/// network share.
pub fn filesystem_is_local(magic: i64) -> bool {
    !REMOTE_MAGICS.iter().any(|(known, _)| *known == magic)
}

/// statfs magics for filesystems held in memory, from `linux/magic.h`.
const MEMORY_MAGICS: &[(i64, &str)] = &[(0x01021994, "tmpfs"), (0x858458F6, "ramfs")];

/// The warning boot logs for a database held in memory. How to move it
/// safely depends on the setup, so the docs hold the steps.
pub const IN_MEMORY_WARNING: &str = "database held in memory: it and its backups are lost on \
     restart. Map /app/cache to a disk or pool path (docs/configuration.md, Storage and \
     permissions)";

/// The name of the filesystem a statfs magic names when that filesystem is
/// held in memory, `None` for any other.
fn memory_filesystem(magic: i64) -> Option<&'static str> {
    MEMORY_MAGICS
        .iter()
        .find(|(known, _)| *known == magic)
        .map(|(_, name)| *name)
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
    check_filesystem(path).map(|_| ())
}

/// [`reject_remote_filesystem`], also naming the filesystem held in memory
/// that the database sits on, if it sits on one, from the same statfs.
pub fn check_filesystem(path: &Path) -> Result<Option<&'static str>, DbError> {
    let anchor = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let magic = statfs_magic(&anchor)?;
    if filesystem_is_local(magic) {
        return Ok(memory_filesystem(magic));
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

    /// The refusal says what to do: map /app/cache to a local disk, on
    /// Unraid the template's pool path, a disk path for a box with no pool
    /// or an exclusive share, on Docker Desktop a named volume.
    #[test]
    fn the_remote_filesystem_refusal_names_the_fix() {
        let refusal = DbError::NonLocalFilesystem {
            path: PathBuf::from("/app/cache/library.db"),
            fs_type: "fuse.shfs".to_owned(),
            mount: "/app/cache".to_owned(),
        }
        .to_string();
        for fix in [
            "/mnt/cache/appdata/droppedneedle/cache",
            "/mnt/disk1/appdata/droppedneedle/cache",
            "exclusive",
            "Docker Desktop",
            "named volume",
            "local disk",
        ] {
            assert!(refusal.contains(fix), "{fix}: {refusal}");
        }
    }

    /// tmpfs and ramfs are held in memory and warn, without being refused;
    /// a disk or a container layer does not warn.
    #[test]
    fn a_database_held_in_memory_warns() {
        for (magic, name) in [(0x01021994, "tmpfs"), (0x858458F6, "ramfs")] {
            assert_eq!(memory_filesystem(magic), Some(name));
            assert!(filesystem_is_local(magic), "{name} must not be refused");
        }
        for (magic, name) in [
            (0xEF53, "ext4"),
            (0x58465342, "xfs"),
            (0x9123683E, "btrfs"),
            (0x794C7630, "overlayfs"),
        ] {
            assert_eq!(memory_filesystem(magic), None, "{name}");
        }
    }

    /// A link as the database file or as a folder above it is refused; a
    /// real path and a file not there yet pass.
    #[test]
    fn links_on_the_database_path_are_refused() {
        let dir = std::env::temp_dir().join(format!("db-fs-{}", std::process::id()));
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let target = dir.join("library.db");
        std::fs::write(&target, b"x").unwrap();
        let link = dir.join("link.db");
        let link_dir = dir.join("linked");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, &link).unwrap();
            std::os::unix::fs::symlink(&real, &link_dir).unwrap();
            assert!(
                matches!(reject_symlink(&link), Err(DbError::SymlinkRefused { .. })),
                "a link as the file"
            );
            assert!(
                matches!(
                    reject_symlink(&link_dir.join("library.db")),
                    Err(DbError::SymlinkRefused { .. })
                ),
                "a link as a parent folder"
            );
        }
        assert!(reject_symlink(&target).is_ok(), "a real file");
        assert!(
            reject_symlink(&real.join("library.db")).is_ok(),
            "a real parent folder"
        );
        assert!(
            reject_symlink(&dir.join("missing.db")).is_ok(),
            "a missing file"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
