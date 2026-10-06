//! The slskd downloads mount probe: is the folder DroppedNeedle reads
//! slskd's finished downloads from usable, can imports move files out of
//! it in one step, and can slskd's finished downloads actually be found
//! there.
//!
//! Ported from v2 (`check_downloads_mount`, `filesystem_mounts`, and the
//! advisory text in the download client status route). The filesystem
//! checks block, so callers run them off the async workers.

use std::path::{Path, PathBuf};

use crate::acquire::slskd::MountDiagnosis;

/// Why the mount is or is not usable, and whether moves stay on one
/// filesystem. The wire values match v2's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountReason {
    /// Usable; a move into a library root is a rename (or no root exists
    /// yet to compare against).
    Ok,
    /// `SLSKD_DOWNLOADS_PATH` is empty.
    NotSet,
    /// The folder does not exist inside the container.
    Missing,
    /// The folder exists but DroppedNeedle cannot write to it.
    NotWritable,
    /// Usable, but every library root sits on another mount: imports copy
    /// and then delete instead of renaming.
    DifferentMount,
    /// Usable, but every library root sits on another filesystem.
    DifferentFilesystem,
    /// Usable, but the filesystem could not be compared.
    StatError,
}

impl MountReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotSet => "not_set",
            Self::Missing => "missing",
            Self::NotWritable => "not_writable",
            Self::DifferentMount => "different_mount",
            Self::DifferentFilesystem => "different_filesystem",
            Self::StatError => "stat_error",
        }
    }
}

/// The verdict on the downloads folder itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountCheck {
    /// The folder exists and is writable.
    pub ok: bool,
    /// Imports can rename into at least one library root.
    pub move_supported: bool,
    pub reason: MountReason,
    pub path: String,
}

/// Everything the slskd status shows about the downloads mount.
#[derive(Debug, Clone, PartialEq)]
pub struct SlskdMountReport {
    pub mount: MountCheck,
    /// One plain sentence about a mount that looks fine but cannot see
    /// slskd's finished downloads, with what to change.
    pub advisory: Option<String>,
    /// slskd's own downloads folder, in slskd's container.
    pub client_downloads_dir: Option<String>,
    /// The folder DroppedNeedle actually reads: the mount plus the
    /// downloads subfolder from settings.
    pub effective_path: String,
}

/// Check the downloads folder against the library roots. Blocking.
pub fn check_downloads_mount(path: &Path, library_roots: &[PathBuf]) -> MountCheck {
    let shown = path.display().to_string();
    let verdict = |ok, move_supported, reason| MountCheck {
        ok,
        move_supported,
        reason,
        path: shown.clone(),
    };
    if path.as_os_str().is_empty() {
        return verdict(false, false, MountReason::NotSet);
    }
    if !path.exists() {
        return verdict(false, false, MountReason::Missing);
    }
    if !writable(path) {
        return verdict(false, false, MountReason::NotWritable);
    }
    let roots: Vec<&PathBuf> = library_roots.iter().filter(|root| root.exists()).collect();
    if roots.is_empty() {
        return verdict(true, false, MountReason::Ok);
    }
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .map(|content| parse_mountinfo(&content))
        .unwrap_or_default();
    let boundaries: Vec<MountReason> = roots
        .iter()
        .map(|root| move_boundary(path, root, &mounts))
        .collect();
    if boundaries.contains(&MountReason::Ok) {
        return verdict(true, true, MountReason::Ok);
    }
    // The most specific reason wins, as in v2.
    let reason = [
        MountReason::DifferentMount,
        MountReason::DifferentFilesystem,
        MountReason::StatError,
    ]
    .into_iter()
    .find(|reason| boundaries.contains(reason))
    .unwrap_or(MountReason::StatError);
    verdict(true, false, reason)
}

fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the
    // call; `access` only reads it.
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

/// `/proc/self/mountinfo` rows as (mount id, mount point).
fn parse_mountinfo(content: &str) -> Vec<(i64, PathBuf)> {
    content
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 6 || !fields.contains(&"-") {
                return None;
            }
            let id = fields[0].parse().ok()?;
            Some((id, PathBuf::from(unescape_mount_path(fields[4]))))
        })
        .collect()
}

fn unescape_mount_path(value: &str) -> String {
    value
        .replace(r"\040", " ")
        .replace(r"\011", "\t")
        .replace(r"\012", "\n")
        .replace(r"\134", "\\")
}

/// The innermost mount holding `path`.
fn containing_mount(path: &Path, mounts: &[(i64, PathBuf)]) -> Option<i64> {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    mounts
        .iter()
        .filter(|(_, mount)| resolved.starts_with(mount))
        .max_by_key(|(_, mount)| mount.components().count())
        .map(|(id, _)| *id)
}

/// Whether a rename from `source` to `destination` stays on one mount.
/// Mount ids decide when mountinfo is readable; the device id is the
/// fallback.
fn move_boundary(source: &Path, destination: &Path, mounts: &[(i64, PathBuf)]) -> MountReason {
    if let (Some(from), Some(to)) = (
        containing_mount(source, mounts),
        containing_mount(destination, mounts),
    ) {
        return if from == to {
            MountReason::Ok
        } else {
            MountReason::DifferentMount
        };
    }
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(source), std::fs::metadata(destination)) {
        (Ok(a), Ok(b)) if a.dev() == b.dev() => MountReason::Ok,
        (Ok(_), Ok(_)) => MountReason::DifferentFilesystem,
        _ => MountReason::StatError,
    }
}

/// The folder DroppedNeedle reads: the mount plus the confined subfolder.
pub fn effective_path(mount: &Path, subpath: &str) -> PathBuf {
    let mut path = mount.to_path_buf();
    for part in subpath.split(['/', '\\']).map(str::trim) {
        if !part.is_empty() && part != "." && part != ".." {
            path.push(part);
        }
    }
    path
}

/// The advisory for a mount that passes the basic checks but cannot see
/// slskd's finished downloads: empty, or full of other files. None when
/// nothing looks wrong or nothing can be told yet.
pub fn advisory(
    mount: &MountCheck,
    diagnosis: &MountDiagnosis,
    subpath: &str,
    effective: &str,
) -> Option<String> {
    if !mount.ok || !diagnosis.supported || diagnosis.completed_downloads == 0 {
        return None;
    }
    let count = diagnosis.completed_downloads;
    let saves_to = diagnosis
        .client_downloads_dir
        .as_deref()
        .map(|dir| format!(" slskd saves to {dir}."))
        .unwrap_or_default();
    if !diagnosis.mount_has_files {
        let has_subpath = !subpath.trim().is_empty();
        return Some(if has_subpath {
            format!(
                "slskd has {count} finished download(s), but nothing is visible in {effective}. \
                 That path is your mount plus the downloads subfolder below.{saves_to} If the \
                 mount already points at slskd's folder, clear the subfolder box and save again."
            )
        } else {
            format!(
                "slskd has {count} finished download(s), but the downloads folder at {} looks \
                 empty.{saves_to} Make sure it points to slskd's downloads directory and that \
                 the container can read it (check the PUID/GID).",
                mount.path
            )
        });
    }
    if diagnosis.sampled_downloads > 0 && diagnosis.resolvable_downloads == 0 {
        return Some(format!(
            "None of slskd's {count} finished download(s) are visible under {}.{saves_to} The \
             downloads mount usually covers a parent folder (for example your whole media \
             share) instead of slskd's completed-downloads folder. Type the rest of the path in \
             the downloads subfolder below.",
            mount.path
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_mount() -> MountCheck {
        MountCheck {
            ok: true,
            move_supported: true,
            reason: MountReason::Ok,
            path: "/downloads".into(),
        }
    }

    fn diagnosis(has_files: bool, resolvable: usize) -> MountDiagnosis {
        MountDiagnosis {
            supported: true,
            completed_downloads: 3,
            mount_has_files: has_files,
            resolvable_downloads: resolvable,
            sampled_downloads: 3,
            client_downloads_dir: Some("/app/downloads".into()),
        }
    }

    #[test]
    fn advisory_names_the_fix_for_each_misconfiguration() {
        let empty = advisory(&ok_mount(), &diagnosis(false, 0), "", "/downloads").unwrap();
        assert!(empty.contains("looks empty") && empty.contains("slskd saves to /app/downloads"));
        let doubled = advisory(&ok_mount(), &diagnosis(false, 0), "x", "/downloads/x").unwrap();
        assert!(doubled.contains("clear the subfolder box"));
        let parent = advisory(&ok_mount(), &diagnosis(true, 0), "", "/downloads").unwrap();
        assert!(parent.contains("parent folder"));
        assert_eq!(
            advisory(&ok_mount(), &diagnosis(true, 1), "", "/downloads"),
            None
        );
    }

    #[test]
    fn mountinfo_picks_the_innermost_mount() {
        let mounts = parse_mountinfo(
            "22 1 0:21 / / rw - ext4 /dev/root rw\n\
             40 22 0:40 / /mnt/my\\040music rw - nfs host:/x rw\n",
        );
        assert_eq!(
            containing_mount(Path::new("/mnt/my music/a"), &mounts),
            Some(40)
        );
        assert_eq!(containing_mount(Path::new("/srv"), &mounts), Some(22));
    }
}
