//! Sandbox roots and symlink-free path handling.
//!
//! Every publisher write lands under a configured library root or, for a
//! file leaving the library, under a recycle bin. Library paths arrive as
//! `(root id, relative path)` pairs and are resolved here: absolute
//! inputs, `..` escapes, NUL bytes, and symlink components all fail closed
//! before any mutation. A library folder is used as the admin set it, and
//! that path may go through a link; below the folder, media symlinks are
//! never followed. That is a security boundary, not a profile toggle. A
//! bin entry is an absolute path that must sit under one of the session's
//! bins with no symlink below the bin's own folder, and library audio
//! never resolves into a bin.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use super::place::{self, PlaceError, PlacementMode, Seams};
use super::{HIDDEN_PREFIX, PublishError};
use crate::scan::fs::RECYCLE_BIN_DIRECTORY_NAME;

/// What a sandbox root holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootKind {
    /// A library folder: audio is read and written here.
    Library,
    /// A recycle bin: files leaving the library go here, each into a fresh
    /// entry folder. `serves` names the one library root whose `.recycle`
    /// this is; `None` is the configured bin, which serves every root.
    Bin { serves: Option<String> },
    /// A library folder whose policy keeps DroppedNeedle's writes out of
    /// it, though files in its included subfolders are in the catalog. A
    /// person can still remove those files and put them back: only a
    /// removal's and its put-back's own steps resolve paths here, and every
    /// other write is refused.
    RemoveOnly,
}

/// A sandbox root: a stable id, its absolute directory and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    /// Stable identifier used in journals and collision evidence.
    pub id: String,
    /// Absolute directory the root owns.
    pub dir: PathBuf,
    /// Library folder or recycle bin.
    pub kind: RootKind,
}

impl Root {
    /// A library root.
    pub fn library(id: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            dir: dir.into(),
            kind: RootKind::Library,
        }
    }

    /// A library folder only removals and their put-backs may touch.
    pub fn remove_only(id: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            dir: dir.into(),
            kind: RootKind::RemoveOnly,
        }
    }

    /// The configured recycle bin, serving every library root. `dir` is
    /// the bin's resolved folder.
    pub fn bin(id: impl Into<String>, dir: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            dir: dir.into(),
            kind: RootKind::Bin { serves: None },
        }
    }

    /// The `.recycle` folder at the top of library root `root_id`.
    pub fn recycle_of(root_id: &str, root_dir: &Path) -> Self {
        Self {
            id: format!("{RECYCLE_BIN_DIRECTORY_NAME}:{root_id}"),
            dir: root_dir.join(RECYCLE_BIN_DIRECTORY_NAME),
            kind: RootKind::Bin {
                serves: Some(root_id.to_owned()),
            },
        }
    }

    fn is_bin(&self) -> bool {
        matches!(self.kind, RootKind::Bin { .. })
    }
}

/// The writable world for one publisher run: the library roots and the
/// recycle bins. The journal, snapshots, and baselines live in the
/// application database; the only things written under a root are the
/// files themselves and the hidden staging and backup temps beside them,
/// and under a bin only the entries files leave the library into.
///
/// Each root and bin also carries how files can be placed under it
/// ([`PlacementMode`]), found by a probe the first time something is
/// placed there. It is held in memory only and shared by every clone, so
/// a rebuilt sandbox starts every root at `Unknown` again.
#[derive(Debug, Clone)]
pub struct Sandbox {
    roots: Vec<Root>,
    /// Library folders only removals and their put-backs may touch.
    remove_only: Vec<Root>,
    /// `Err` with the cause while the bins are unknown (a setting they
    /// come from cannot be read): nothing then moves into a bin, and no
    /// other bin stands in.
    bins: Result<Vec<Root>, &'static str>,
    /// Keyed by (is a bin, id), so a bin id never shadows a root id.
    modes: Arc<Mutex<BTreeMap<(bool, String), PlacementMode>>>,
}

impl Sandbox {
    /// Build a sandbox over library roots, remove-only roots and bins,
    /// rejecting roots that are not absolute. It needs at least one library
    /// or remove-only root.
    pub fn new(roots: Vec<Root>) -> Result<Self, PublishError> {
        for root in &roots {
            if !root.dir.is_absolute() {
                return Err(PublishError::UnsafePath(
                    format!("root {} is not absolute", root.id).into(),
                ));
            }
        }
        let (bins, roots): (Vec<Root>, Vec<Root>) = roots.into_iter().partition(Root::is_bin);
        let (remove_only, roots): (Vec<Root>, Vec<Root>) = roots
            .into_iter()
            .partition(|root| root.kind == RootKind::RemoveOnly);
        if roots.is_empty() && remove_only.is_empty() {
            return Err(PublishError::UnsafePath(
                "sandbox needs at least one root".into(),
            ));
        }
        Ok(Self {
            roots,
            remove_only,
            bins: Ok(bins),
            modes: Arc::default(),
        })
    }

    /// The same sandbox while its bins are unknown because `cause`: no
    /// entry is checked or written until a session that knows them opens.
    #[must_use]
    pub fn with_bins_unknown(mut self, cause: &'static str) -> Self {
        self.bins = Err(cause);
        self
    }

    /// How files are placed under a library root. `Unknown` until a bundle
    /// has targeted it.
    pub fn placement_mode(&self, root_id: &str) -> PlacementMode {
        self.mode(false, root_id)
    }

    /// How files go into a bin. `Unknown` until a file has gone into it
    /// this session.
    pub fn bin_placement_mode(&self, bin_id: &str) -> PlacementMode {
        self.mode(true, bin_id)
    }

    fn mode(&self, bin: bool, id: &str) -> PlacementMode {
        self.modes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(bin, id.to_owned()))
            .copied()
            .unwrap_or(PlacementMode::Unknown)
    }

    /// Probe a root whose placement mode is still `Unknown`, because a
    /// bundle is about to place files under it. Apart from an
    /// administrator's check, this is the only time the probe writes to a
    /// library. A root that answers `Unknown` again is probed again for
    /// the next bundle.
    pub fn probe_placement(&self, root_id: &str) -> Result<PlacementMode, PublishError> {
        let known = self.placement_mode(root_id);
        if known != PlacementMode::Unknown {
            return Ok(known);
        }
        let dir = self.root_dir(root_id)?;
        let mode = place::probe(dir);
        warn_check_root(dir, mode);
        self.store_placement_mode(false, root_id, mode);
        Ok(mode)
    }

    /// Probe a bin whose placement mode is still `Unknown`, because a file
    /// is about to go into it; the bin's folder is made first. On a disk
    /// with neither safe step files go in by check-then-rename, which is
    /// safe here: every entry is a fresh, uniquely named folder.
    pub fn probe_bin_placement(&self, bin: &Root) -> PlacementMode {
        let known = self.bin_placement_mode(&bin.id);
        if known != PlacementMode::Unknown {
            return known;
        }
        if let Err(error) = self.ensure_bin_dir(bin) {
            tracing::warn!(bin = bin.id, %error, "recycle bin folder refused");
            return PlacementMode::Unknown;
        }
        match make_dirs(&bin.dir) {
            Ok(made) => {
                if let Err(error) = sync_made(&made) {
                    tracing::warn!(bin = bin.id, %error, "recycle bin folder sync failed");
                }
            }
            Err(error) => {
                tracing::warn!(bin = bin.id, %error, "recycle bin folder could not be made");
                return PlacementMode::Unknown;
            }
        }
        let mode = place::probe(&bin.dir);
        if mode == PlacementMode::Check {
            tracing::info!(
                bin = bin.id,
                "the recycle bin's disk has neither a no-replace rename nor hard links; \
                 files go in by check-then-rename, each into a new entry folder"
            );
        }
        self.store_placement_mode(true, &bin.id, mode);
        mode
    }

    /// Probe a root again whatever is known about it, because an
    /// administrator asked, and return the probe's answer. A probe that
    /// could not tell (`Unknown`) leaves what was known in place: a full
    /// disk must not clear a standing `Check`.
    pub fn recheck_placement(&self, root_id: &str) -> Result<PlacementMode, PublishError> {
        let dir = self.root_dir(root_id)?;
        let mode = place::probe(dir);
        warn_check_root(dir, mode);
        if mode != PlacementMode::Unknown {
            self.store_placement_mode(false, root_id, mode);
        }
        Ok(mode)
    }

    /// Fix a root's placement mode without probing. Tests only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_placement_mode(&self, root_id: &str, mode: PlacementMode) {
        self.store_placement_mode(false, root_id, mode);
    }

    /// Fix a bin's placement mode without probing. Tests only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_bin_placement_mode(&self, bin_id: &str, mode: PlacementMode) {
        self.store_placement_mode(true, bin_id, mode);
    }

    fn store_placement_mode(&self, bin: bool, id: &str, mode: PlacementMode) {
        self.modes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((bin, id.to_owned()), mode);
    }

    /// Look up a library root directory by stable id.
    pub fn root_dir(&self, id: &str) -> Result<&Path, PublishError> {
        self.roots
            .iter()
            .find(|root| root.id == id)
            .map(|root| root.dir.as_path())
            .ok_or_else(|| PublishError::UnsafePath(format!("unknown root {id}").into()))
    }

    /// Resolve a root-relative path to an absolute path, rejecting
    /// traversal, absolute inputs, NUL bytes, and anything inside a
    /// recycle bin. The result is guaranteed to stay under the library
    /// root directory.
    pub fn resolve(&self, root_id: &str, relative: &str) -> Result<PathBuf, PublishError> {
        let root_dir = self.root_dir(root_id)?.to_path_buf();
        self.resolve_under(&root_dir, relative)
    }

    fn resolve_under(&self, root_dir: &Path, relative: &str) -> Result<PathBuf, PublishError> {
        if relative.is_empty() {
            return Err(PublishError::UnsafePath("empty relative path".into()));
        }
        if relative.contains('\0') {
            return Err(PublishError::UnsafePath("NUL byte in path".into()));
        }
        let mut clean = PathBuf::new();
        for component in Path::new(relative).components() {
            match component {
                Component::Normal(part) => clean.push(part),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(PublishError::UnsafePath(
                        format!("path escapes root: {relative}").into(),
                    ));
                }
            }
        }
        if clean.as_os_str().is_empty() {
            return Err(PublishError::UnsafePath(
                "empty path after normalization".into(),
            ));
        }
        let path = root_dir.join(clean);
        if self.in_a_bin(&path) {
            return Err(PublishError::UnsafePath(
                format!("path is inside the recycle bin: {relative}").into(),
            ));
        }
        Ok(path)
    }

    /// True when `path` is inside a recycle bin: a library root's
    /// `.recycle` folder, or one of the session's bins.
    fn in_a_bin(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .chain(&self.remove_only)
            .any(|root| path.starts_with(root.dir.join(RECYCLE_BIN_DIRECTORY_NAME)))
            || self
                .bins
                .iter()
                .flatten()
                .any(|bin| path.starts_with(&bin.dir))
    }

    /// Resolve and additionally prove that the library folder is there and
    /// that no existing component below it (the final one included) is a
    /// symlink. Missing trailing components are fine. The folder itself,
    /// and the path above it, are the admin's and may go through links.
    pub fn resolve_no_symlink(
        &self,
        root_id: &str,
        relative: &str,
    ) -> Result<PathBuf, PublishError> {
        let path = self.resolve(root_id, relative)?;
        ensure_no_symlink_below(self.root_dir(root_id)?, &path)?;
        Ok(path)
    }

    /// The folder of a root a removal may take files from and its put-back
    /// return them to: a library root or a remove-only one. Only a
    /// removal's and a put-back's own steps call this.
    pub fn removal_root_dir(&self, id: &str) -> Result<&Path, PublishError> {
        self.roots
            .iter()
            .chain(&self.remove_only)
            .find(|root| root.id == id)
            .map(|root| root.dir.as_path())
            .ok_or_else(|| PublishError::UnsafePath(format!("unknown root {id}").into()))
    }

    /// [`Self::resolve_no_symlink`] for a file a removal takes away or its
    /// put-back returns, which may also sit under a remove-only root. Only
    /// a removal's and a put-back's own steps call this; every other write
    /// resolves through the library roots.
    pub fn resolve_removal(&self, root_id: &str, relative: &str) -> Result<PathBuf, PublishError> {
        let root_dir = self.removal_root_dir(root_id)?.to_path_buf();
        let path = self.resolve_under(&root_dir, relative)?;
        ensure_no_symlink_below(&root_dir, &path)?;
        Ok(path)
    }

    /// [`Self::ensure_under_roots`] for a removal's or a put-back's own
    /// paths, which may also sit under a remove-only root.
    pub fn ensure_under_removal_roots(&self, path: &Path) -> Result<(), PublishError> {
        if self.in_a_bin(path) {
            return Err(PublishError::UnsafePath(
                "path is inside the recycle bin".to_owned().into(),
            ));
        }
        if self
            .roots
            .iter()
            .chain(&self.remove_only)
            .any(|root| path.starts_with(&root.dir))
        {
            return Ok(());
        }
        Err(PublishError::UnsafePath(
            "path escapes all sandbox roots".to_owned().into(),
        ))
    }

    /// [`Self::probe_placement`] for a root a removal sets a file aside
    /// under or a put-back places one under, which may be a remove-only
    /// root.
    pub fn probe_removal_placement(&self, root_id: &str) -> Result<PlacementMode, PublishError> {
        let known = self.placement_mode(root_id);
        if known != PlacementMode::Unknown {
            return Ok(known);
        }
        let dir = self.removal_root_dir(root_id)?;
        let mode = place::probe(dir);
        warn_check_root(dir, mode);
        self.store_placement_mode(false, root_id, mode);
        Ok(mode)
    }

    /// Why the session's bins are unknown, while they are.
    pub fn bins_unknown(&self) -> Option<&'static str> {
        self.bins.as_ref().err().copied()
    }

    /// The session's bins, or a refusal while they are unknown.
    pub fn bins(&self) -> Result<&[Root], PublishError> {
        self.bins.as_deref().map_err(|cause| {
            PublishError::Cleanup(format!("no recycle bin is known while {cause}").into())
        })
    }

    /// The bin a file from library root `root_id` goes to now: the
    /// configured bin, else the root's own `.recycle`. A root whose
    /// `.recycle` failed its checks has none, and its files wait.
    pub fn bin_for(&self, root_id: &str) -> Result<&Root, PublishError> {
        super::bin::serving(self.bins()?, root_id).ok_or_else(|| {
            PublishError::Cleanup(
                format!("library root {root_id} has no usable recycle bin").into(),
            )
        })
    }

    /// Refuse a bin whose own folder is now a symlink. A root's plain
    /// `.recycle` (a library or remove-only root's) is checked itself, as
    /// its root may be reached through a link; a resolved folder (the
    /// configured bin, or the target of a `.recycle` link) is checked all
    /// the way down, since it held no link when the session opened.
    pub(crate) fn ensure_bin_dir(&self, bin: &Root) -> Result<(), PublishError> {
        let plain_recycle = match &bin.kind {
            RootKind::Bin { serves: Some(root) } => self
                .removal_root_dir(root)
                .is_ok_and(|dir| dir.join(RECYCLE_BIN_DIRECTORY_NAME) == bin.dir),
            _ => false,
        };
        if !plain_recycle {
            return ensure_no_symlink(&bin.dir);
        }
        match std::fs::symlink_metadata(&bin.dir) {
            Ok(meta) if meta.file_type().is_symlink() => Err(PublishError::UnsafePath(
                "the recycle bin folder is a symlink".into(),
            )),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PublishError::Io(error.to_string().into())),
        }
    }

    /// The bin `path` names an entry of, by name only: nothing on disk is
    /// checked.
    pub fn bin_holding(&self, path: &Path) -> Option<&Root> {
        self.bins
            .iter()
            .flatten()
            .find(|bin| path != bin.dir && path.starts_with(&bin.dir))
    }

    /// Prove that an absolute bin entry path sits under one of the
    /// session's bins, with no `.` or `..` part, a bin folder that has not
    /// turned into a symlink, and no symlink below it. Returns the bin.
    /// Checked before an entry is planned and again before a file moves
    /// into it.
    pub fn ensure_under_bin(&self, path: &Path) -> Result<&Root, PublishError> {
        self.bins()?;
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(PublishError::UnsafePath(
                "recycle bin entry is not a plain absolute path".into(),
            ));
        }
        let bin = self.bin_holding(path).ok_or_else(|| {
            PublishError::UnsafePath("recycle bin entry is outside every recycle bin".into())
        })?;
        self.ensure_bin_dir(bin)?;
        let below = path
            .strip_prefix(&bin.dir)
            .map_err(|_| PublishError::UnsafePath("recycle bin entry escapes its bin".into()))?;
        let mut cursor = bin.dir.clone();
        for component in below.components() {
            cursor.push(component.as_os_str());
            match std::fs::symlink_metadata(&cursor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(PublishError::UnsafePath(
                        format!(
                            "refusing to follow symlink in the recycle bin: {}",
                            message_name(&cursor)
                        )
                        .into(),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(PublishError::Io(error.to_string().into())),
            }
        }
        Ok(bin)
    }

    /// Hidden staging temp for a destination. It lives in the
    /// destination directory, so the publish rename never crosses
    /// filesystems, and carries the reserved prefix so scan
    /// discovery prunes it.
    pub fn temp_path_for(&self, dest: &Path, journal_id: &str) -> Result<PathBuf, PublishError> {
        hidden_path(dest, journal_id, "tmp")
    }

    /// [`Self::temp_path_for`] for a temp the tag writer saves in: it ends
    /// in the container's own extension, which names the format.
    pub fn audio_temp_path_for(
        &self,
        dest: &Path,
        journal_id: &str,
        extension: &str,
    ) -> Result<PathBuf, PublishError> {
        hidden_path(dest, journal_id, &format!("tmp.{extension}"))
    }

    /// Hidden same-directory backup for a same-path write. The original
    /// is retained here across the publish/catalog window and restored
    /// from it on compensation.
    pub fn backup_path_for(&self, dest: &Path, journal_id: &str) -> Result<PathBuf, PublishError> {
        hidden_path(dest, journal_id, "bak")
    }

    /// Prove that an absolute path is under one of the library roots and
    /// not inside a recycle bin. Used as a final guard before any write
    /// lands.
    pub fn ensure_under_roots(&self, path: &Path) -> Result<(), PublishError> {
        if self.in_a_bin(path) {
            return Err(PublishError::UnsafePath(
                "path is inside the recycle bin".to_owned().into(),
            ));
        }
        for root in &self.roots {
            if path.starts_with(&root.dir) {
                return Ok(());
            }
        }
        // 4xx-safe: the absolute server path never leaves the server.
        Err(PublishError::UnsafePath(
            "path escapes all sandbox roots".to_owned().into(),
        ))
    }
}

/// A library root with the real folder its configured path resolves to
/// now (`None` while it does not resolve).
#[derive(Debug, Clone)]
pub struct ResolvedRoot {
    pub id: String,
    pub configured: PathBuf,
    pub real: Option<PathBuf>,
}

impl ResolvedRoot {
    /// Resolve root `id` at `configured`. Blocking: it reads the disk.
    pub fn of(id: &str, configured: &Path) -> Self {
        Self {
            id: id.to_owned(),
            configured: configured.to_path_buf(),
            real: std::fs::canonicalize(configured).ok(),
        }
    }

    fn folder(&self) -> &Path {
        self.real.as_deref().unwrap_or(&self.configured)
    }
}

/// The roots that take no managed writes: each one reached through a link
/// (its real folder is not its configured path) whose real folder is,
/// holds or sits inside another root's. Through both ids, two catalogs
/// would manage one set of files: automatic management could move them
/// to and fro, a collision between the two would be caught only by the
/// operating system's placement call, and a removal that takes the lock at
/// once guards only its own root, so a scan of the other one runs beside
/// it. A root whose path is its real folder is never left out, so nothing
/// that took file changes before linked folders did loses them.
///
/// Library folders against download and staging folders stay as the
/// settings check them, by name: the publisher writes only under a root,
/// a landed file already inside a library folder is refused on resolved
/// paths, and the download side deletes only folders it named itself.
pub fn left_out(roots: &[ResolvedRoot]) -> Vec<String> {
    roots
        .iter()
        .filter(|root| {
            root.real
                .as_ref()
                .is_some_and(|real| *real != root.configured)
        })
        .filter(|root| {
            let mine = root.folder();
            roots.iter().any(|other| {
                other.id != root.id && {
                    let theirs = other.folder();
                    mine.starts_with(theirs) || theirs.starts_with(mine)
                }
            })
        })
        .map(|root| root.id.clone())
        .collect()
}

/// Warn about a library root whose disk has neither safe placement step.
fn warn_check_root(root: &Path, mode: PlacementMode) {
    if mode == PlacementMode::Check {
        tracing::warn!(
            root = %root.display(),
            "this library's disk has neither a no-replace rename nor hard links; \
             files are placed by check-then-rename, which another program can race"
        );
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

/// Fail closed unless library folder `root` is a folder now. It is
/// followed through any link: the folder an admin set is trusted as set.
/// A folder that is gone, or a link to nothing, answers
/// [`PublishError::FolderMissing`].
pub(crate) fn ensure_library_dir(root: &Path) -> Result<(), PublishError> {
    match std::fs::metadata(root) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(PublishError::FolderMissing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(PublishError::FolderMissing)
        }
        Err(error) => Err(PublishError::Io(error.to_string().into())),
    }
}

/// The part of `path` below `root`, when it is made only of plain names.
/// `None` for a path outside `root`, or one with a `.` or `..` in it.
pub(crate) fn below_root<'p>(root: &Path, path: &'p Path) -> Option<&'p Path> {
    path.strip_prefix(root).ok().filter(|below| {
        below
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    })
}

/// Fail closed unless library folder `root` is a folder now and no
/// existing component of `path` below it is a symlink. `root` and what
/// lies above it are the admin's and are not checked for links. Missing
/// trailing components are allowed so new destinations can be validated
/// before they exist. Readers outside the publisher use it too, so a file
/// is never read through a link below a library folder.
pub fn ensure_no_symlink_below(root: &Path, path: &Path) -> Result<(), PublishError> {
    ensure_library_dir(root)?;
    no_symlink_below(root, path)
}

/// The walk of [`ensure_no_symlink_below`] alone, for a caller that has
/// just checked the folder. A path not [`below_root`] refuses.
pub(crate) fn no_symlink_below(root: &Path, path: &Path) -> Result<(), PublishError> {
    let below = below_root(root, path)
        .ok_or_else(|| PublishError::UnsafePath("path is not inside its library folder".into()))?;
    let mut cursor = root.to_path_buf();
    for component in below.components() {
        cursor.push(component.as_os_str());
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PublishError::UnsafePath(
                    format!("refusing to follow symlink: {}", message_name(&cursor)).into(),
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(PublishError::Io(err.to_string().into())),
        }
    }
    Ok(())
}

/// Fail closed when any existing component of `path` is a symlink. Only
/// for a resolved bin folder, which held no link when the session opened.
fn ensure_no_symlink(path: &Path) -> Result<(), PublishError> {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component.as_os_str());
        match std::fs::symlink_metadata(&cursor) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(PublishError::UnsafePath(
                        format!("refusing to follow symlink: {}", message_name(&cursor)).into(),
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(PublishError::Io(err.to_string().into())),
        }
    }
    Ok(())
}

/// What one streamed read of a file found: its SHA-256 and size, and its
/// identity on disk while it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hashed {
    pub sha256: String,
    pub size: u64,
    pub stat: FileStat,
}

/// The piece size of a streamed read.
const CHUNK: usize = 1 << 17;

/// Open a symlink-free regular file for reading, never through a link. A
/// FIFO swapped in at the name does not block the open.
fn open_regular(path: &Path) -> Result<(std::fs::File, FileStat), PublishError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let unsafe_path = || {
        PublishError::UnsafePath(
            format!("refusing to read through symlink: {}", message_name(path)).into(),
        )
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => {
                PublishError::Validation(format!("missing file: {}", message_name(path)).into())
            }
            _ if err.raw_os_error() == Some(libc::ELOOP) => unsafe_path(),
            _ => PublishError::Io(err.to_string().into()),
        })?;
    let meta = file.metadata().map_err(PublishError::from)?;
    if !meta.file_type().is_file() {
        return Err(PublishError::Validation(
            format!("not a regular file: {}", message_name(path)).into(),
        ));
    }
    Ok((file, FileStat::of(&meta)))
}

/// Stream `file` into `sink` in pieces, hashing it, and check it kept its
/// identity throughout. The size read must be the size it had.
fn stream(
    path: &Path,
    file: &mut std::fs::File,
    stat: FileStat,
    mut sink: impl FnMut(&[u8]) -> Result<(), PublishError>,
) -> Result<Hashed, PublishError> {
    use sha2::Digest as _;
    use std::io::Read as _;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0_u8; CHUNK];
    let mut size = 0_u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(PublishError::from(error)),
        };
        hasher.update(&buffer[..read]);
        sink(&buffer[..read])?;
        size += read as u64;
    }
    let after = FileStat::of(&file.metadata().map_err(PublishError::from)?);
    if after != stat || size != stat.size {
        return Err(PublishError::Validation(
            format!("{} changed while it was read", message_name(path)).into(),
        ));
    }
    Ok(Hashed {
        sha256: super::snapshots::hex_digest(&hasher.finalize()),
        size,
        stat,
    })
}

/// Prove a path names a symlink-free regular file and hash it, reading it
/// in pieces: no file is ever held whole in memory.
pub fn hash_regular_file(path: &Path) -> Result<Hashed, PublishError> {
    let (mut file, stat) = open_regular(path)?;
    stream(path, &mut file, stat, |_| Ok(()))
}

/// Whether the symlink-free regular file at `path` holds `sha256`.
pub fn holds(path: &Path, sha256: &str) -> bool {
    hash_regular_file(path).is_ok_and(|hashed| hashed.sha256 == sha256)
}

/// Copy the symlink-free regular file `source` into the new hidden file
/// `temp`, hashing the bytes as they pass, in one read. The copy gets
/// `mode` (else the default bits) and is on disk when this returns. What
/// comes back is the source's hash, size and identity; the caller checks
/// them against what it expects. A copy that fails part way is removed.
pub fn copy_hashed(source: &Path, temp: &Path, mode: Option<u32>) -> Result<Hashed, PublishError> {
    let (mut file, stat) = open_regular(source)?;
    let copied = (|| {
        let mut out = create_hidden(temp).map_err(PublishError::from)?;
        let hashed = stream(source, &mut file, stat, |piece| {
            out.write_all(piece).map_err(PublishError::from)
        })?;
        lend_mode(&out, mode, temp);
        super::journal::fsync_file(&out)?;
        Ok(hashed)
    })();
    if copied.is_err()
        && let Err(error) = std::fs::remove_file(temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(temp = %temp.display(), %error, "partial copy not removed");
    }
    copied
}

/// Give a staged file the permission bits of the file it stands in for.
/// The bits go on the open file, never on the name, so a link swapped in
/// at the name is not followed. A folder that refuses the change does not
/// fail the write: the file keeps the default bits.
pub(crate) fn lend_mode(file: &std::fs::File, mode: Option<u32>, temp: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(bits) = mode else {
        return;
    };
    if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(bits)) {
        tracing::warn!(temp = %temp.display(), %error, "staged file keeps the default permission bits");
    }
}

/// Hashes one publish worked out for files it read or wrote, each with the
/// whole identity the file had then. Between adjacent steps of that
/// publish a file whose identity is unchanged, change time included, holds
/// the bytes hashed, so it is not read again. Any write moves the change
/// time, and so does a rename or link, so a file the publisher itself
/// moved is read again. Recovery runs in a new process and starts empty:
/// it reads every file it checks.
#[derive(Debug, Default)]
pub struct Known(BTreeMap<FileStat, String>);

impl Known {
    /// Forget every file, at the start of a publish.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Note that the file with identity `stat` holds `sha256`.
    pub fn record(&mut self, stat: FileStat, sha256: &str) {
        self.0.insert(stat, sha256.to_owned());
    }

    /// The identity of the regular file at `path` when it is a file this
    /// publish knows to hold `sha256`.
    pub fn vouch(&self, path: &Path, sha256: &str) -> Option<FileStat> {
        let stat = FileStat::read(path).ok()?;
        (self.0.get(&stat).map(String::as_str) == Some(sha256)).then_some(stat)
    }

    /// The SHA-256 of the regular file at `path`: the one this publish
    /// knows for it, else read. `None` when it is not a readable regular
    /// file.
    pub fn hash(&self, path: &Path) -> Option<String> {
        let stat = FileStat::read(path).ok()?;
        if let Some(known) = self.0.get(&stat) {
            return Some(known.clone());
        }
        hash_regular_file(path).ok().map(|hashed| hashed.sha256)
    }

    /// Whether the regular file at `path` holds `sha256`.
    pub fn holds(&self, path: &Path, sha256: &str) -> bool {
        self.vouch(path, sha256).is_some() || holds(path, sha256)
    }
}

/// True when two paths name the same file on disk: a case-only rename
/// on a filesystem that folds case reaches its own source through the
/// new name. Missing files are never the same file.
pub fn same_file(left: &Path, right: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        match (
            std::fs::symlink_metadata(left),
            std::fs::symlink_metadata(right),
        ) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        left == right
    }
}

/// True when a move's destination is its own source under another letter
/// case: the names fold together and, where the destination exists, it is
/// the same file. A hard link of the source under an unrelated name is not
/// an alias: the names must fold together too.
pub fn case_alias(sandbox: &Sandbox, source: (&str, &str), dest: (&str, &str)) -> bool {
    if collision_key(source.0, source.1) != collision_key(dest.0, dest.1) {
        return false;
    }
    match (
        sandbox.resolve_no_symlink(source.0, source.1),
        sandbox.resolve_no_symlink(dest.0, dest.1),
    ) {
        (Ok(source), Ok(dest)) => source == dest || same_file(&source, &dest),
        _ => false,
    }
}

/// Where a set-aside original goes back to on an unwind: the source name
/// for a case-only rename, so the original letter case returns; the
/// destination otherwise (a same-path write).
pub fn backup_home(
    sandbox: &Sandbox,
    source: Option<&(String, String)>,
    dest_root: &str,
    dest_rel: &str,
) -> Result<PathBuf, PublishError> {
    match source {
        Some((root, rel)) if collision_key(root, rel) == collision_key(dest_root, dest_rel) => {
            sandbox.resolve_no_symlink(root, rel)
        }
        _ => sandbox.resolve_no_symlink(dest_root, dest_rel),
    }
}

/// A file's identity on disk: its device and inode, with its size,
/// modified time and change time. Before a file moves, all five must match
/// for it to count as the file that was read: any write or time change
/// moves the change time, which nothing can set back. A rename on one disk
/// moves the change time too, but keeps the other four, so after a move
/// [`FileStat::same_moved`] compares those.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct FileStat {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
}

impl FileStat {
    /// The identity in `meta`.
    pub fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.len(),
            mtime_ns: crate::scan::revision::mtime_ns_from_metadata(meta),
            ctime_ns: meta
                .ctime()
                .saturating_mul(1_000_000_000)
                .saturating_add(meta.ctime_nsec()),
        }
    }

    /// The identity of the regular file at `path`, never through a
    /// symlink.
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.file_type().is_file() {
            return Err(std::io::Error::other("not a regular file"));
        }
        Ok(Self::of(&meta))
    }

    /// Whether `now` is this file after a rename on one disk: everything
    /// but the change time, which the rename moves.
    pub fn same_moved(&self, now: &Self) -> bool {
        self.dev == now.dev
            && self.ino == now.ino
            && self.size == now.size
            && self.mtime_ns == now.mtime_ns
    }
}

/// Move a set-aside file into its recycle bin entry after commit. The
/// file must still hold `sha256`, unless `known` (its identity when it was
/// hashed) still matches: then it is the file that was set aside, a move
/// on one disk does not read it again, and bytes changed in place since
/// (an edit that kept its size and modified time) go to the bin as they
/// are now, logged, rather than holding the move for good. A bin path
/// that already holds those bytes (an earlier attempt copied it there)
/// only drops the set-aside file, and a bin path holding anything else is
/// refused, never overwritten. The file goes in through
/// [`place::place_from_hidden`], so an entry that turns up after those
/// checks is not replaced either. On another filesystem the bytes are
/// copied under a temporary name with the file's permission bits, synced,
/// read back and checked against the bytes read, placed the same way, and
/// only then, with the folders above the copy on disk, is the set-aside
/// file removed. A copy that fails part way is removed. `mode` is the
/// placement mode where `recycle_to` is; the caller has checked that
/// `recycle_to` is a place the file may go. True when the file is there
/// now; false when there was nothing left to move. Cheap checks come
/// first, so a retry while the bin cannot take the file reads nothing.
pub fn recycle_backup(
    mode: PlacementMode,
    seams: Seams,
    backup: &Path,
    recycle_to: &Path,
    sha256: &str,
    known: Option<FileStat>,
) -> Result<bool, PublishError> {
    let hash_of = |path: &Path| hash_regular_file(path).ok().map(|hashed| hashed.sha256);
    let changed = || {
        PublishError::Validation(
            format!(
                "set-aside file {} changed; it stays where it is",
                message_name(backup)
            )
            .into(),
        )
    };
    let backup_meta = match std::fs::symlink_metadata(backup) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(hash_of(recycle_to).as_deref() == Some(sha256));
        }
        Err(error) => return Err(PublishError::Io(error.to_string().into())),
    };
    if !backup_meta.file_type().is_file() {
        return Err(changed());
    }
    // The set-aside file itself, by identity: what it holds now is what
    // goes to the bin.
    let trusted = known.is_some_and(|known| known.same_moved(&FileStat::of(&backup_meta)));
    let parent = recycle_to
        .parent()
        .ok_or_else(|| PublishError::UnsafePath("recycle bin path has no folder".into()))?;
    if std::fs::symlink_metadata(recycle_to).is_ok() {
        // An earlier attempt copied it there. The set-aside file goes only
        // once the copy's folder and the one above it are synced.
        let held = hash_of(backup);
        let copied = hash_of(recycle_to);
        if held.is_some() && held == copied && (trusted || held.as_deref() == Some(sha256)) {
            super::journal::fsync_dir(parent)?;
            if let Some(above) = parent.parent() {
                super::journal::fsync_dir(above)?;
            }
            std::fs::remove_file(backup).map_err(PublishError::from)?;
            return Ok(true);
        }
        return Err(taken(recycle_to));
    }
    let made = make_dirs(parent).map_err(PublishError::from)?;
    let made_synced = sync_made(&made);
    // Whether the bytes read are the ones expected: a trusted file that
    // changed in place goes as it is now, logged.
    let check = |actual: &str| -> Result<(), PublishError> {
        if actual != sha256 {
            if !trusted {
                return Err(changed());
            }
            tracing::warn!(
                file = %message_name(backup),
                "a removed file changed in place after it was read; it goes to the recycle bin as it is now"
            );
        }
        Ok(())
    };
    if !trusted {
        check(&hash_regular_file(backup)?.sha256)?;
    }
    match place::place_with(mode, backup, recycle_to, seams) {
        Ok(_) => {}
        Err(PlaceError::Unsynced(error)) => {
            tracing::warn!(%error, "recycle bin folder sync failed");
        }
        // Another disk: the one case that copies. A failed sync keeps the
        // set-aside file, and the row stays pending.
        Err(PlaceError::CrossDevice) => {
            made_synced?;
            let bits = {
                use std::os::unix::fs::PermissionsExt as _;
                backup_meta.permissions().mode() & 0o777
            };
            copy_verified(backup, recycle_to, &check, bits, mode, seams)?;
            std::fs::remove_file(backup).map_err(PublishError::from)?;
            return Ok(true);
        }
        Err(error) => return Err(refused(error, recycle_to)),
    }
    if let Err(error) = made_synced {
        tracing::warn!(%error, "recycle bin folder sync failed");
    }
    Ok(true)
}

fn taken(recycle_to: &Path) -> PublishError {
    PublishError::Collision(
        format!("recycle bin entry {} is taken", message_name(recycle_to)).into(),
    )
}

fn refused(error: PlaceError, recycle_to: &Path) -> PublishError {
    match error {
        PlaceError::Collision => taken(recycle_to),
        PlaceError::Unavailable(_) | PlaceError::CrossDevice => PublishError::Cleanup(
            format!(
                "the recycle bin folder has no safe way to add {} without risking an overwrite",
                message_name(recycle_to)
            )
            .into(),
        ),
        other => PublishError::Io(other.to_string().into()),
    }
}

/// Copy `backup` to `recycle_to` on another disk in one streamed read:
/// under a hidden temporary name with permission bits `bits`, the bytes
/// read passed by `check`, synced and read back, then placed. A temp that
/// did not end up placed is ours, and goes.
fn copy_verified(
    backup: &Path,
    recycle_to: &Path,
    check: &dyn Fn(&str) -> Result<(), PublishError>,
    bits: u32,
    mode: PlacementMode,
    seams: Seams,
) -> Result<(), PublishError> {
    let temp = incoming_temp(recycle_to)
        .ok_or_else(|| PublishError::UnsafePath("recycle bin path has no folder".into()))?;
    let copied = (|| -> Result<(), PublishError> {
        let read = copy_hashed(backup, &temp, Some(bits))?;
        if seams.short_write {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&temp)
                .and_then(|file| file.set_len(read.size / 2))
                .map_err(PublishError::from)?;
            return Err(PublishError::Io("the copy stopped part way".into()));
        }
        check(&read.sha256)?;
        if hash_regular_file(&temp)?.sha256 != read.sha256 {
            return Err(PublishError::Cleanup(
                format!(
                    "the copy of {} did not read back the same",
                    message_name(recycle_to)
                )
                .into(),
            ));
        }
        let within = Seams {
            cross_device: false,
            ..seams
        };
        place::place_with(mode, &temp, recycle_to, within)
            .map(|_| ())
            .map_err(|error| refused(error, recycle_to))
    })();
    if copied.is_err()
        && let Err(error) = std::fs::remove_file(&temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(temp = %temp.display(), %error, "unfinished copy not removed");
    }
    copied
}

/// The hidden temp a copy into the bin across disks is written under,
/// beside `recycle_to`.
pub(crate) fn incoming_temp(recycle_to: &Path) -> Option<PathBuf> {
    Some(recycle_to.parent()?.join(format!(
        "{HIDDEN_PREFIX}incoming-{}",
        short_hash(&recycle_to.to_string_lossy())
    )))
}

/// Make `dir` and any folder above it that is missing. Answers the
/// folders made, top first.
fn make_dirs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut missing = Vec::new();
    let mut cursor = Some(dir);
    while let Some(path) = cursor {
        match std::fs::symlink_metadata(path) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(path.to_path_buf());
                cursor = path.parent();
            }
            Err(error) => return Err(error),
        }
    }
    std::fs::create_dir_all(dir)?;
    missing.reverse();
    Ok(missing)
}

/// Put each made folder's own entry on disk, by syncing the folder that
/// holds it.
fn sync_made(made: &[PathBuf]) -> Result<(), PublishError> {
    for dir in made {
        if let Some(parent) = dir.parent() {
            super::journal::fsync_dir(parent)?;
        }
    }
    Ok(())
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

/// Bytes of a file's own name a hidden name carries.
const KEPT_NAME_MAX: usize = 64;

/// A hidden name beside `dest` for one journal: the reserved prefix, 16
/// hex characters hashed from the journal id, the start of the file's own
/// name, and the ending. The kept part lets a person tell which file a
/// leftover belongs to; it is cut at [`KEPT_NAME_MAX`] bytes, so a file
/// name near the 255-byte limit still has room. A name that is not UTF-8
/// is carried lossily. A retry of the same journal finds the same name.
fn hidden_path(dest: &Path, journal_id: &str, ending: &str) -> Result<PathBuf, PublishError> {
    let parent = dest
        .parent()
        .ok_or_else(|| PublishError::UnsafePath("destination has no parent directory".into()))?;
    let name = dest
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let mut cut = name.len().min(KEPT_NAME_MAX);
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    Ok(parent.join(format!(
        "{HIDDEN_PREFIX}{}.{}.{ending}",
        short_hash(journal_id),
        &name[..cut]
    )))
}

/// The first 16 hex characters of a SHA-256, for a hidden name.
fn short_hash(text: &str) -> String {
    let mut hash = super::snapshots::sha256_hex(text.as_bytes());
    hash.truncate(16);
    hash
}

/// Create a hidden working file that must be new. Whatever an earlier
/// attempt left under the name is removed first, by name: a symlink put
/// there is unlinked, never written through.
pub fn create_hidden(path: &Path) -> std::io::Result<std::fs::File> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Permission bits of the file a staged copy stands in for.
pub fn permission_bits(path: &Path) -> Result<u32, PublishError> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::symlink_metadata(path).map_err(PublishError::from)?;
    Ok(meta.permissions().mode() & 0o777)
}
