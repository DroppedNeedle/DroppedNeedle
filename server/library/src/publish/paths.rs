//! Sandbox roots and symlink-free path handling.
//!
//! Every publisher write lands under a configured library root or, for a
//! file leaving the library, under a recycle bin. Library paths arrive as
//! `(root id, relative path)` pairs and are resolved here: absolute
//! inputs, `..` escapes, NUL bytes, and symlink components all fail closed
//! before any mutation. Media symlinks are never followed; that is a
//! security boundary, not a profile toggle. A bin entry is an absolute
//! path that must sit under one of the session's bins with no symlink
//! below the bin's own folder, and library audio never resolves into a
//! bin.

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
    /// `Err` with the cause while the bins are unknown (a setting they
    /// come from cannot be read): nothing then moves into a bin, and no
    /// other bin stands in.
    bins: Result<Vec<Root>, &'static str>,
    /// Keyed by (is a bin, id), so a bin id never shadows a root id.
    modes: Arc<Mutex<BTreeMap<(bool, String), PlacementMode>>>,
}

impl Sandbox {
    /// Build a sandbox over library roots and bins, rejecting roots that
    /// are not absolute. It needs at least one library root.
    pub fn new(roots: Vec<Root>) -> Result<Self, PublishError> {
        for root in &roots {
            if !root.dir.is_absolute() {
                return Err(PublishError::UnsafePath(format!(
                    "root {} is not absolute",
                    root.id
                )));
            }
        }
        let (bins, roots): (Vec<Root>, Vec<Root>) = roots.into_iter().partition(Root::is_bin);
        if roots.is_empty() {
            return Err(PublishError::UnsafePath(
                "sandbox needs at least one root".into(),
            ));
        }
        Ok(Self {
            roots,
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
        if let Err(error) = std::fs::create_dir_all(&bin.dir) {
            tracing::warn!(bin = bin.id, %error, "recycle bin folder could not be made");
            return PlacementMode::Unknown;
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
            .ok_or_else(|| PublishError::UnsafePath(format!("unknown root {id}")))
    }

    /// Resolve a root-relative path to an absolute path, rejecting
    /// traversal, absolute inputs, NUL bytes, and anything inside a
    /// recycle bin. The result is guaranteed to stay under the library
    /// root directory.
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
        let path = root_dir.join(clean);
        if self.in_a_bin(&path) {
            return Err(PublishError::UnsafePath(format!(
                "path is inside the recycle bin: {relative}"
            )));
        }
        Ok(path)
    }

    /// True when `path` is inside a recycle bin: a library root's
    /// `.recycle` folder, or one of the session's bins.
    fn in_a_bin(&self, path: &Path) -> bool {
        self.roots
            .iter()
            .any(|root| path.starts_with(root.dir.join(RECYCLE_BIN_DIRECTORY_NAME)))
            || self
                .bins
                .iter()
                .flatten()
                .any(|bin| path.starts_with(&bin.dir))
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

    /// Why the session's bins are unknown, while they are.
    pub fn bins_unknown(&self) -> Option<&'static str> {
        self.bins.as_ref().err().copied()
    }

    /// The session's bins, or a refusal while they are unknown.
    pub fn bins(&self) -> Result<&[Root], PublishError> {
        self.bins.as_deref().map_err(|cause| {
            PublishError::Cleanup(format!("no recycle bin is known while {cause}"))
        })
    }

    /// The bin a file from library root `root_id` goes to now: the
    /// configured bin, else the root's own `.recycle`. A root whose
    /// `.recycle` failed its checks has none, and its files wait.
    pub fn bin_for(&self, root_id: &str) -> Result<&Root, PublishError> {
        super::bin::serving(self.bins()?, root_id).ok_or_else(|| {
            PublishError::Cleanup(format!("library root {root_id} has no usable recycle bin"))
        })
    }

    /// Refuse a bin whose own folder is now a symlink. A root's plain
    /// `.recycle` is checked itself; a resolved folder (the configured bin,
    /// or the target of a `.recycle` link) is checked all the way down,
    /// since it held no link when the session opened.
    fn ensure_bin_dir(&self, bin: &Root) -> Result<(), PublishError> {
        let plain_recycle = match &bin.kind {
            RootKind::Bin { serves: Some(root) } => self
                .root_dir(root)
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
            Err(error) => Err(PublishError::Io(error.to_string())),
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
                    return Err(PublishError::UnsafePath(format!(
                        "refusing to follow symlink in the recycle bin: {}",
                        message_name(&cursor)
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Err(PublishError::Io(error.to_string())),
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
                "path is inside the recycle bin".to_owned(),
            ));
        }
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

/// Move a replaced file's backup into the recycle bin after commit. The
/// backup must still hold `sha256`; a bin path that already holds those
/// bytes (an earlier attempt copied it there) only drops the backup, and
/// a bin path holding anything else is refused, never overwritten. The
/// file goes in through [`place::place_from_hidden`], so an entry that
/// turns up after those checks is not replaced either. On another
/// filesystem the bytes are copied under a temporary name, synced, read
/// back and checked against `sha256`, placed the same way, and only then
/// is the backup removed. `mode` is the bin's placement mode. The caller
/// has proved `recycle_to` is an entry of a bin with no symlink below it.
/// True when the file is in the bin now; false when there was nothing
/// left to move.
pub fn recycle_backup(
    mode: PlacementMode,
    seams: Seams,
    backup: &Path,
    recycle_to: &Path,
    sha256: &str,
) -> Result<bool, PublishError> {
    use super::snapshots::sha256_hex;
    let in_bin = || {
        read_regular_file(recycle_to)
            .ok()
            .map(|bytes| sha256_hex(&bytes))
    };
    let bytes = match read_regular_file(backup) {
        Ok(bytes) => bytes,
        Err(_) if in_bin().as_deref() == Some(sha256) => return Ok(true),
        Err(_) if std::fs::symlink_metadata(backup).is_err() => return Ok(false),
        Err(error) => return Err(error),
    };
    if sha256_hex(&bytes) != sha256 {
        return Err(PublishError::Validation(format!(
            "set-aside file {} changed; it stays where it is",
            message_name(backup)
        )));
    }
    match in_bin() {
        // An earlier attempt copied it there. The backup goes only once the
        // bin's folder is synced, so the copy is on disk first.
        Some(found) if found == sha256 => {
            if let Some(entry) = recycle_to.parent() {
                super::journal::fsync_dir(entry)?;
            }
            std::fs::remove_file(backup).map_err(PublishError::from)?;
            return Ok(true);
        }
        Some(_) => {
            return Err(PublishError::Collision(format!(
                "recycle bin entry {} is taken",
                message_name(recycle_to)
            )));
        }
        None if std::fs::symlink_metadata(recycle_to).is_ok() => {
            return Err(PublishError::Collision(format!(
                "recycle bin entry {} is taken",
                message_name(recycle_to)
            )));
        }
        None => {}
    }
    let parent = recycle_to
        .parent()
        .ok_or_else(|| PublishError::UnsafePath("recycle bin path has no folder".into()))?;
    std::fs::create_dir_all(parent).map_err(PublishError::from)?;
    let refused = |error: PlaceError| match error {
        PlaceError::Collision => PublishError::Collision(format!(
            "recycle bin entry {} is taken",
            message_name(recycle_to)
        )),
        PlaceError::Unavailable(_) | PlaceError::CrossDevice => PublishError::Cleanup(format!(
            "the recycle bin folder has no safe way to add {} without risking an overwrite",
            message_name(recycle_to)
        )),
        other => PublishError::Io(other.to_string()),
    };
    match place::place_with(mode, backup, recycle_to, seams) {
        Ok(_) => {}
        Err(PlaceError::Unsynced(error)) => {
            tracing::warn!(%error, "recycle bin folder sync failed");
        }
        // The bin is on another disk: the one caller that copies. The copy
        // is read back before it is placed, and the backup goes only once
        // a verified copy is in the bin and the bin's folder is synced: a
        // failed sync keeps the backup, and the row stays pending.
        Err(PlaceError::CrossDevice) => {
            let temp = parent.join(format!(
                "{HIDDEN_PREFIX}incoming-{}",
                short_hash(&recycle_to.to_string_lossy())
            ));
            let mut file = create_hidden(&temp).map_err(PublishError::from)?;
            file.write_all(&bytes).map_err(PublishError::from)?;
            super::journal::fsync_file(&file)?;
            drop(file);
            if read_regular_file(&temp).map(|copied| sha256_hex(&copied))? != sha256 {
                std::fs::remove_file(&temp).map_err(PublishError::from)?;
                return Err(PublishError::Cleanup(format!(
                    "the copy of {} in the recycle bin did not read back the same",
                    message_name(recycle_to)
                )));
            }
            let within_bin = Seams {
                cross_device: false,
                ..seams
            };
            place::place_with(mode, &temp, recycle_to, within_bin).map_err(refused)?;
            std::fs::remove_file(backup).map_err(PublishError::from)?;
        }
        Err(error) => return Err(refused(error)),
    }
    Ok(true)
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
/// is carried lossily. A retry of the same journal finds the same name. A
/// plugin temp keeps its tag in front of the hash, which the plugin sweep
/// matches on.
fn hidden_path(dest: &Path, journal_id: &str, ending: &str) -> Result<PathBuf, PublishError> {
    use super::plugin_files::TEMP_TAG;
    let parent = dest
        .parent()
        .ok_or_else(|| PublishError::UnsafePath("destination has no parent directory".into()))?;
    let tag = if journal_id.starts_with(TEMP_TAG) {
        TEMP_TAG
    } else {
        ""
    };
    let name = dest
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let mut cut = name.len().min(KEPT_NAME_MAX);
    while !name.is_char_boundary(cut) {
        cut -= 1;
    }
    Ok(parent.join(format!(
        "{HIDDEN_PREFIX}{tag}{}.{}.{ending}",
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
