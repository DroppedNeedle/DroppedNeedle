//! Sandbox roots and symlink-free path handling.
//!
//! Every publisher write lands under a configured library root. Paths
//! arrive as `(root id, relative path)` pairs and are resolved here:
//! absolute inputs, `..` escapes, NUL bytes, and symlink components all
//! fail closed before any mutation. Media symlinks are never followed;
//! that is a security boundary, not a profile toggle.

use std::path::{Component, Path, PathBuf};

use super::{HIDDEN_PREFIX, PublishError};

/// A configured library root: a stable id plus its absolute directory.
#[derive(Debug, Clone)]
pub struct Root {
    /// Stable identifier used in journals and collision evidence.
    pub id: String,
    /// Absolute directory the root owns.
    pub dir: PathBuf,
}

/// The writable world for one publisher run: library roots plus the
/// metadata directory that holds journals, blobs, and snapshots.
///
/// The metadata directory must itself sit under one of the roots, so
/// "writes only under sandbox roots" covers journal and snapshot
/// writes too.
#[derive(Debug, Clone)]
pub struct Sandbox {
    roots: Vec<Root>,
    meta_dir: PathBuf,
}

impl Sandbox {
    /// Build a sandbox, rejecting a metadata directory that escapes
    /// every root and roots that are not absolute directories.
    pub fn new(roots: Vec<Root>, meta_dir: PathBuf) -> Result<Self, PublishError> {
        if roots.is_empty() {
            return Err(PublishError::UnsafePath(
                "sandbox needs at least one root".into(),
            ));
        }
        for root in &roots {
            if !root.dir.is_absolute() {
                return Err(PublishError::UnsafePath(format!(
                    "root {} is not absolute",
                    root.id
                )));
            }
        }
        let sandbox = Self { roots, meta_dir };
        let mut inside = false;
        for root in &sandbox.roots {
            if sandbox.meta_dir.starts_with(&root.dir) {
                inside = true;
            }
        }
        if !inside {
            return Err(PublishError::UnsafePath(
                "metadata directory must sit under a sandbox root".into(),
            ));
        }
        Ok(sandbox)
    }

    /// Look up a root directory by stable id.
    pub fn root_dir(&self, id: &str) -> Result<&Path, PublishError> {
        self.roots
            .iter()
            .find(|root| root.id == id)
            .map(|root| root.dir.as_path())
            .ok_or_else(|| PublishError::UnsafePath(format!("unknown root {id}")))
    }

    /// Metadata directory for journals, blobs, and snapshots.
    pub fn meta_dir(&self) -> &Path {
        &self.meta_dir
    }

    /// Resolve a root-relative path to an absolute path, rejecting
    /// traversal, absolute inputs, and NUL bytes. The result is
    /// guaranteed to stay under the root directory.
    pub fn resolve(&self, root_id: &str, relative: &str) -> Result<PathBuf, PublishError> {
        if relative.is_empty() {
            return Err(PublishError::UnsafePath("empty relative path".into()));
        }
        if relative.contains('\0') {
            return Err(PublishError::UnsafePath("NUL byte in path".into()));
        }
        let root_dir = self.root_dir(root_id)?.to_path_buf();
        let mut clean = PathBuf::new();
        for component in Path::new(relative).components() {
            match component {
                Component::Normal(part) => clean.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(PublishError::UnsafePath(format!(
                        "path escapes root: {relative}"
                    )));
                }
            }
        }
        if clean.as_os_str().is_empty() {
            return Err(PublishError::UnsafePath(
                "empty path after normalization".into(),
            ));
        }
        Ok(root_dir.join(clean))
    }

    /// Resolve and additionally prove that no existing component on the
    /// path (and the final component, when present) is a symlink.
    /// Missing trailing components are fine; every ancestor that
    /// exists must be symlink-free.
    pub fn resolve_no_symlink(
        &self,
        root_id: &str,
        relative: &str,
    ) -> Result<PathBuf, PublishError> {
        let path = self.resolve(root_id, relative)?;
        ensure_no_symlink(&path)?;
        Ok(path)
    }

    /// Hidden staging temp for a destination. It lives in the
    /// destination directory, so the publish rename never crosses
    /// filesystems, and carries the reserved prefix so scan
    /// discovery prunes it.
    pub fn temp_path_for(&self, dest: &Path, journal_id: &str) -> Result<PathBuf, PublishError> {
        let parent = dest.parent().ok_or_else(|| {
            PublishError::UnsafePath("destination has no parent directory".into())
        })?;
        let file_name = dest
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| PublishError::UnsafePath("destination name is not UTF-8".into()))?;
        assert_no_symlink_component(journal_id)?;
        Ok(parent.join(format!("{HIDDEN_PREFIX}{journal_id}.{file_name}.tmp")))
    }

    /// Hidden same-directory backup for a same-path write. The original
    /// is retained here across the publish/catalog window and restored
    /// from it on compensation.
    pub fn backup_path_for(&self, dest: &Path, journal_id: &str) -> Result<PathBuf, PublishError> {
        let parent = dest.parent().ok_or_else(|| {
            PublishError::UnsafePath("destination has no parent directory".into())
        })?;
        let file_name = dest
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| PublishError::UnsafePath("destination name is not UTF-8".into()))?;
        assert_no_symlink_component(journal_id)?;
        Ok(parent.join(format!("{HIDDEN_PREFIX}{journal_id}.{file_name}.bak")))
    }

    /// Prove that an absolute path is under one of the sandbox roots.
    /// Used as a final guard before any write lands.
    pub fn ensure_under_roots(&self, path: &Path) -> Result<(), PublishError> {
        for root in &self.roots {
            if path.starts_with(&root.dir) {
                return Ok(());
            }
        }
        // 4xx-safe: the absolute server path never leaves the server.
        Err(PublishError::UnsafePath(
            "path escapes all sandbox roots".to_owned(),
        ))
    }
}

/// File name for a 4xx message. Absolute server paths never leave the
/// server; callers with a (root id, rel path) pair name that instead.
fn message_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file")
        .to_owned()
}

/// Fail closed when any existing component of `path` is a symlink.
/// Missing trailing components are allowed so new destinations can be
/// validated before they exist.
pub fn ensure_no_symlink(path: &Path) -> Result<(), PublishError> {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component.as_os_str());
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(PublishError::UnsafePath(format!(
                        "refusing to follow symlink: {}",
                        message_name(&cursor)
                    )));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(PublishError::Io(err.to_string())),
        }
    }
    Ok(())
}

/// Prove a path names a symlink-free regular file, returning its bytes.
pub fn read_regular_file(path: &Path) -> Result<Vec<u8>, PublishError> {
    let meta = std::fs::symlink_metadata(path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => {
            PublishError::Validation(format!("missing file: {}", message_name(path)))
        }
        _ => PublishError::Io(err.to_string()),
    })?;
    if meta.file_type().is_symlink() {
        return Err(PublishError::UnsafePath(format!(
            "refusing to read through symlink: {}",
            message_name(path)
        )));
    }
    if !meta.file_type().is_file() {
        return Err(PublishError::Validation(format!(
            "not a regular file: {}",
            message_name(path)
        )));
    }
    std::fs::read(path).map_err(PublishError::from)
}

/// Unicode/case-fold collision key: NFC normalization plus full
/// casefold, so names that differ only by case or by canonically
/// equivalent sequences can never publish side by side.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CollisionKey(String);

/// Build the collision key for one root-relative destination.
pub fn collision_key(root_id: &str, relative: &str) -> CollisionKey {
    use unicode_normalization::UnicodeNormalization;
    let normalized: String = relative.nfc().collect();
    let folded = caseless::default_case_fold_str(&normalized);
    CollisionKey(format!("{root_id}\u{1f}{folded}"))
}

fn assert_no_symlink_component(journal_id: &str) -> Result<(), PublishError> {
    if journal_id.contains('/') || journal_id.contains('\\') || journal_id.contains('\0') {
        return Err(PublishError::UnsafePath(
            "journal id is not a file-name part".into(),
        ));
    }
    Ok(())
}
