//! Recycle bins as sandbox roots: which bins a session holds, the checks
//! a configured bin passes before it is used, fresh entry names, and the
//! move of a replaced file into its bin after its bundle committed.
//!
//! The configured bin (Library Management's `recycle_bin_path`) is
//! resolved once per session, symlinks and all, and every overlap check
//! runs on the resolved folder. A configured bin that is, contains or sits
//! inside a library root (other than as that root's `.recycle`), or that
//! overlaps a download folder, is not used: each root's `.recycle` takes
//! its place, logged once per change and shown on the Library Management
//! settings. While those settings cannot be read, or the configured bin's
//! folder cannot be reached or is not there (a share not mounted), the
//! bins are unknown and nothing that would go into a bin moves: no
//! `.recycle` stands in for a bin that is down. The app never makes the
//! configured folder; an admin makes it or mounts the share.
//!
//! An empty folder left under a share's mount point looks like any other,
//! so the configured bin is known by the marker file ([`BIN_MARKER`]) a
//! session leaves in it the first time it uses the path, marker first and
//! the path's row second. A path with a row whose folder lacks the marker
//! is not the bin, and nor is an unmarked folder holding no entry folder
//! while the journal still has copies in it or v2 used the path (the
//! upgrade records it, [`record_v2_path`]): files wait, as for a bin that
//! can't be reached, until the share is back or an admin accepts the
//! folder ([`BinSource::accept_folder`]), which marks it again. The
//! settings and expiry only compare; they never mark.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use droppedneedle_auth::Admin;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;

use super::PublishError;
use super::journal::{FileJournal, JournalKind, JournalReplacement, JournalWriter};
use super::paths::{self, FileStat, ResolvedRoot, Root, RootKind, Sandbox};
use super::place::Seams;
use super::publisher::durable;
use crate::scan::fs::RECYCLE_BIN_DIRECTORY_NAME;
use crate::scan::roots::RootRegistry;
use crate::settings_models::RecycleBinWarning;

/// Id of the configured bin in a session.
pub const CONFIGURED_BIN_ID: &str = "recycle-bin";

/// The file a session leaves in the configured bin's folder the first
/// time it uses the path. Expiry and Empty leave it alone (it is not named
/// like an entry), and nothing lists it.
pub const BIN_MARKER: &str = ".droppedneedle-recycle-bin";

/// What the marker says to a person who finds it.
const MARKER_TEXT: &[u8] = b"DroppedNeedle keeps its recycle bin in this folder. \
Leave this file here: without it the folder does not count as the bin, and removals wait.\n";

/// The settings warning while the configured bin's folder is not the one
/// marked as the bin.
const UNMARKED_WARNING: &str = "DroppedNeedle can't tell this is the recycle bin folder used before, so removals and upgrades wait. If its share isn't mounted, mount it; otherwise choose Use this folder.";

/// Folders a configured bin must stay clear of besides the library: the
/// download staging folder and SABnzbd's completed-downloads folder.
/// Acquisition names them and the builder hands its answer in. `None`
/// while a setting that names one cannot be read.
pub type DownloadTrees = Arc<dyn Fn() -> Option<Vec<PathBuf>> + Send + Sync>;

/// Why the configured bin is not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinRefusal {
    /// The path is not absolute, or has a `..` in it.
    NotAbsolute,
    /// The path could not be resolved or is a folder that isn't there (a
    /// share not mounted): permission denied, an I/O error, a link loop,
    /// or nothing at the path. A configured bin like this makes files
    /// wait; a `.recycle` link like this leaves its root with no bin.
    Unresolvable,
    /// It is, holds or sits inside a library folder.
    OverlapsLibrary,
    /// It is, holds or sits inside a download folder.
    OverlapsDownloads,
}

impl BinRefusal {
    /// What the Library Management settings say about it.
    pub fn message(self) -> &'static str {
        match self {
            Self::NotAbsolute => {
                "The recycle bin path is not a full path, so each library folder's .recycle is used instead."
            }
            Self::Unresolvable => {
                "The recycle bin folder isn't there or can't be opened, so removals and upgrades wait."
            }
            Self::OverlapsLibrary => {
                "The recycle bin overlaps a library folder, so each library folder's .recycle is used instead."
            }
            Self::OverlapsDownloads => {
                "The recycle bin overlaps a download folder, so each library folder's .recycle is used instead."
            }
        }
    }
}

/// The bins files go to now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinChoice {
    /// The configured bin, or one `.recycle` per library root whose
    /// `.recycle` passes its checks.
    pub roots: Vec<Root>,
    /// Why the configured bin is not used, when one is set.
    pub refused: Option<BinRefusal>,
    /// Library roots whose `.recycle` is a link that fails its checks:
    /// they have no bin, and their files wait.
    pub unusable: Vec<(String, BinRefusal)>,
}

/// Pick the bins for library roots `library` from the configured path:
/// the resolved configured folder when it passes every check, else each
/// root's `.recycle`. A `.recycle` that is a symlink is resolved and
/// checked the same way; one that fails leaves its root with no bin. The
/// roots come resolved; the download folders are asked for only when a
/// bin could reach them (a configured one, or a `.recycle` link), once.
/// `Err` while they cannot be read, or while the configured folder cannot
/// be reached or is not a folder that exists ([`BinsUnknown::Unreachable`]):
/// then nothing moves into a bin and no `.recycle` stands in. The overlap
/// checks come first, so a bin path inside a download folder is refused
/// whether or not it exists yet.
pub fn choose(
    configured: &str,
    library: &[ResolvedRoot],
    downloads: &mut dyn FnMut() -> Result<Vec<PathBuf>, BinsUnknown>,
) -> Result<BinChoice, BinsUnknown> {
    let real_library: Vec<PathBuf> = library
        .iter()
        .map(|root| root.folder().to_path_buf())
        .collect();
    let mut real_downloads: Option<Vec<PathBuf>> = None;
    let mut clear = |target: &Path| -> Result<Option<BinRefusal>, BinsUnknown> {
        if real_downloads.is_none() {
            real_downloads = Some(
                downloads()?
                    .iter()
                    .map(|tree| resolve_existing(tree).unwrap_or_else(|_| tree.clone()))
                    .collect(),
            );
        }
        let trees = real_downloads.as_deref().unwrap_or_default();
        Ok(check_target(target, &real_library, trees))
    };
    let trimmed = configured.trim();
    let mut refused = None;
    if !trimmed.is_empty() {
        let path = Path::new(trimmed);
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            refused = Some(BinRefusal::NotAbsolute);
        } else {
            let resolved = resolve_existing(path).map_err(|_| BinsUnknown::Unreachable)?;
            match clear(&resolved)? {
                Some(refusal) => refused = Some(refusal),
                None => {
                    let folder = std::fs::symlink_metadata(&resolved)
                        .is_ok_and(|meta| meta.file_type().is_dir());
                    if !folder {
                        return Err(BinsUnknown::Unreachable);
                    }
                    return Ok(BinChoice {
                        roots: vec![Root::bin(CONFIGURED_BIN_ID, resolved)],
                        refused: None,
                        unusable: Vec::new(),
                    });
                }
            }
        }
    }
    let mut choice = BinChoice {
        roots: Vec::new(),
        refused,
        unusable: Vec::new(),
    };
    for root in library {
        let own = root.configured.join(RECYCLE_BIN_DIRECTORY_NAME);
        let linked =
            std::fs::symlink_metadata(&own).is_ok_and(|meta| meta.file_type().is_symlink());
        if !linked {
            choice
                .roots
                .push(Root::recycle_of(&root.id, &root.configured));
            continue;
        }
        let target = match std::fs::canonicalize(&own) {
            Ok(target) => match clear(&target)? {
                None => Ok(target),
                Some(refusal) => Err(refusal),
            },
            Err(_) => Err(BinRefusal::Unresolvable),
        };
        match target {
            Ok(target) => choice.roots.push(Root {
                dir: target,
                ..Root::recycle_of(&root.id, &root.configured)
            }),
            Err(refusal) => choice.unusable.push((root.id.clone(), refusal)),
        }
    }
    Ok(choice)
}

/// Why a resolved bin folder may not be used: it is, holds or sits inside
/// a library root (other than as a root's own `.recycle`), or overlaps a
/// download folder. All paths are resolved.
fn check_target(target: &Path, library: &[PathBuf], downloads: &[PathBuf]) -> Option<BinRefusal> {
    if library
        .iter()
        .any(|dir| target != dir.join(RECYCLE_BIN_DIRECTORY_NAME) && overlaps(target, dir))
    {
        return Some(BinRefusal::OverlapsLibrary);
    }
    if downloads.iter().any(|tree| overlaps(target, tree)) {
        return Some(BinRefusal::OverlapsDownloads);
    }
    None
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The real path of `path`: its longest existing part resolved through
/// any symlinks, with the parts that do not exist yet added back. Those
/// cannot be links, since they are not there.
fn resolve_existing(path: &Path) -> std::io::Result<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match std::fs::canonicalize(existing) {
            Ok(real) => {
                return Ok(missing
                    .iter()
                    .rev()
                    .fold(real, |path: PathBuf, part| path.join(part)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
                    return Err(error);
                };
                missing.push(name.to_owned());
                existing = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Why no bin is known. Its text completes "no recycle bin is known
/// while ...".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BinsUnknown {
    /// The Library Management settings, which name the bin, cannot be
    /// read.
    SettingsUnread,
    /// A setting naming a download folder the bin must stay clear of
    /// cannot be read.
    DownloadsUnread,
    /// The configured bin folder cannot be reached (permission denied, an
    /// I/O error, a link loop) or is a folder that isn't there (a share
    /// not mounted): files wait for it rather than go to a library
    /// folder's `.recycle`.
    Unreachable,
    /// The configured bin's folder has no marker though its path was
    /// marked, or holds no entry folder while the journal has copies in it
    /// or v2 used its path (a share not mounted leaves an empty folder):
    /// files wait for the share or for an admin to accept the folder.
    Unmarked,
}

impl std::fmt::Display for BinsUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SettingsUnread => "the Library Management settings can't be read",
            Self::DownloadsUnread => "the slskd or SABnzbd settings can't be read",
            Self::Unreachable => "the recycle bin folder isn't there or can't be reached",
            Self::Unmarked => "the recycle bin folder may not be the one used before",
        })
    }
}

/// Where the library's bins come from: Library Management's recycle bin
/// path, checked against the library roots and the download folders.
#[derive(Clone)]
pub struct BinSource {
    db: Db,
    clock: Arc<dyn Clock>,
    management: Live<LibraryManagement>,
    library: Live<TypedLibrary>,
    downloads: DownloadTrees,
    logged: Arc<Mutex<Logged>>,
    /// The bin paths known to have their row, so a session that finds the
    /// marker reads the table once per path.
    recorded: Arc<Mutex<BTreeSet<String>>>,
}

/// Whether a check of the configured bin may mark it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marking {
    /// A session, about to move files: a path used for the first time is
    /// marked.
    Record,
    /// The settings and expiry: they look and write nothing.
    Compare,
}

/// Why an admin's choice of the recycle bin folder was not taken.
#[derive(Debug, thiserror::Error)]
pub enum AcceptFolderError {
    /// The saved recycle bin path is not the one the admin was shown.
    #[error("the recycle bin path changed")]
    PathChanged,
    /// The saved settings do not give that folder as the bin: it can't be
    /// reached, is refused, or the settings can't be read.
    #[error("the recycle bin path is not used as it is set")]
    NotTheBin,
    /// The marker could not be made in the folder.
    #[error("the recycle bin marker could not be written: {0}")]
    Unwritable(std::io::Error),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// What was logged about the bins: the last problem line, so each change
/// is logged once, and each cause a wait was logged for, so a bin that
/// comes and goes does not log on every flip.
#[derive(Default)]
struct Logged {
    problems: Option<String>,
    waits: BTreeSet<BinsUnknown>,
}

impl BinSource {
    /// Bins from `management`, checked against the roots in `library` and
    /// the folders `downloads` names. The marked bin paths are kept in
    /// `db`, stamped by `clock`.
    pub fn new(
        db: Db,
        clock: Arc<dyn Clock>,
        management: Live<LibraryManagement>,
        library: Live<TypedLibrary>,
        downloads: DownloadTrees,
    ) -> Self {
        Self {
            db,
            clock,
            management,
            library,
            downloads,
            logged: Arc::default(),
            recorded: Arc::default(),
        }
    }

    /// The bins for `registry`'s roots now. Blocking: it resolves the
    /// roots.
    pub fn current(&self, registry: &RootRegistry) -> Result<BinChoice, BinsUnknown> {
        self.current_over(&resolve_roots(registry))
    }

    /// The bins for `roots`, already resolved, now, for a session about to
    /// move files: a configured bin path used for the first time is marked
    /// here. `Err` with the cause while the Library Management settings or
    /// a setting naming a download folder cannot be read, or while the
    /// configured bin cannot be reached or is not the folder marked as the
    /// bin: then nothing moves into a bin and no other bin stands in.
    pub fn current_over(&self, roots: &[ResolvedRoot]) -> Result<BinChoice, BinsUnknown> {
        let choice = self.read(roots, None, Marking::Record);
        self.log(&choice);
        choice
    }

    fn read(
        &self,
        roots: &[ResolvedRoot],
        downloads: Option<&[PathBuf]>,
        marking: Marking,
    ) -> Result<BinChoice, BinsUnknown> {
        let management = self.management.read().ok_or(BinsUnknown::SettingsUnread)?;
        let mut trees = || match downloads {
            Some(trees) => Ok(trees.to_vec()),
            None => (self.downloads)().ok_or(BinsUnknown::DownloadsUnread),
        };
        let choice = choose(&management.recycle_bin_path, roots, &mut trees)?;
        self.held_to_mark(management.recycle_bin_path.trim(), choice, marking)
    }

    /// `choice` while its configured bin, when it has one, may be used:
    /// its folder holds the marker, or the path `path` was never marked
    /// and the folder does not look like a share that is not mounted.
    /// [`BinsUnknown::Unmarked`] otherwise. With [`Marking::Record`], a
    /// path used for the first time is marked: the marker first, then the
    /// path's row, so a stop between them marks it again on the next use.
    fn held_to_mark(
        &self,
        path: &str,
        choice: BinChoice,
        marking: Marking,
    ) -> Result<BinChoice, BinsUnknown> {
        let Some(dir) = configured_dir(&choice) else {
            return Ok(choice);
        };
        if has_marker(dir) {
            if marking == Marking::Record {
                self.record(path);
            }
            return Ok(choice);
        }
        let marked = self.marked(path).map_err(|error| {
            tracing::warn!(%error, "the recycle bin's mark could not be read; removals and upgrades wait");
            BinsUnknown::Unreachable
        })?;
        if marked || self.looks_unmounted(path, dir)? {
            return Err(BinsUnknown::Unmarked);
        }
        if marking == Marking::Record {
            if let Err(error) = mark_folder(dir) {
                tracing::warn!(%error, "the recycle bin marker could not be written; removals and upgrades wait");
                return Err(BinsUnknown::Unreachable);
            }
            self.record(path);
        }
        Ok(choice)
    }

    /// Whether the folder `dir` of the bin path `path`, not yet marked,
    /// looks like a share that is not mounted: it holds no entry folder at
    /// all while the journal still has copies in it (a bin used before the
    /// marker existed) or v2 used the path.
    fn looks_unmounted(&self, path: &str, dir: &Path) -> Result<bool, BinsUnknown> {
        if super::expiry::gone_for_sure(dir, None) {
            return Ok(false);
        }
        Ok(self.used_by_v2(path)? || self.holds_copies(dir)?)
    }

    /// Whether the upgrade recorded that v2 used the bin path `path`.
    fn used_by_v2(&self, path: &str) -> Result<bool, BinsUnknown> {
        self.db
            .read_blocking("publish.bin_v2_used", |conn| -> Result<bool, DbError> {
                Ok(conn
                    .prepare_cached(super::sql::BIN_V2_USED.text())?
                    .query_row(rusqlite::params![path], |row| row.get(0))?)
            })
            .map_err(|error| {
                tracing::warn!(%error, "the recycle bin's v2 record could not be read; removals and upgrades wait");
                BinsUnknown::Unreachable
            })
    }

    /// Whether the journal still has copies in the bin at `dir`.
    fn holds_copies(&self, dir: &Path) -> Result<bool, BinsUnknown> {
        let below = format!("{}/", dir.to_string_lossy());
        self.db
            .read_blocking("publish.bin_holds_copies", |conn| -> Result<bool, DbError> {
                Ok(conn
                    .prepare_cached(super::sql::BIN_HOLDS_COPIES.text())?
                    .query_row(rusqlite::params![below], |row| row.get(0))?)
            })
            .map_err(|error| {
                tracing::warn!(%error, "the recycle bin's copies could not be read; removals and upgrades wait");
                BinsUnknown::Unreachable
            })
    }

    /// Whether the bin path `path` has its row.
    fn marked(&self, path: &str) -> Result<bool, DbError> {
        if self.known_recorded(path) {
            return Ok(true);
        }
        let marked =
            self.db
                .read_blocking("publish.bin_marked", |conn| -> Result<bool, DbError> {
                    Ok(conn
                        .prepare_cached(super::sql::BIN_MARKED.text())?
                        .query_row(rusqlite::params![path], |row| row.get(0))?)
                })?;
        if marked {
            self.note_recorded(path);
        }
        Ok(marked)
    }

    /// Give the bin path `path`, whose folder holds the marker, its row
    /// when it has none. A write that fails is logged and tried again on
    /// the next use: the marker already proves the folder.
    fn record(&self, path: &str) {
        match self.marked(path) {
            Ok(true) => {}
            Ok(false) => match self.mark_row(path, Lane::Background) {
                Ok(()) => tracing::info!(path, "the recycle bin is marked"),
                Err(error) => tracing::warn!(%error, "the recycle bin's mark was not recorded"),
            },
            Err(error) => tracing::warn!(%error, "the recycle bin's mark could not be read"),
        }
    }

    fn mark_row(&self, path: &str, lane: Lane) -> Result<(), DbError> {
        let at = self.clock.now_ms();
        self.db
            .write_blocking(lane, "publish.bin_mark", |tx| -> Result<(), DbError> {
                tx.prepare_cached(super::sql::BIN_MARK.text())?
                    .execute(rusqlite::params![path, at])?;
                Ok(())
            })?;
        self.note_recorded(path);
        Ok(())
    }

    fn known_recorded(&self, path: &str) -> bool {
        self.recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(path)
    }

    fn note_recorded(&self, path: &str) {
        self.recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_owned());
    }

    /// An admin's answer to [`BinsUnknown::Unmarked`]: the folder the saved
    /// recycle bin path `path` names is the bin from now on, so it is
    /// marked again. True when it was marked; false when it already held
    /// the marker, and nothing was written. Refused when the saved path is
    /// another, or while the saved settings would not use that folder as
    /// the bin. Blocking: it resolves paths.
    pub fn accept_folder(&self, _admin: &Admin, path: &str) -> Result<bool, AcceptFolderError> {
        let management = self.management.read().ok_or(AcceptFolderError::NotTheBin)?;
        let saved = management.recycle_bin_path.trim();
        if saved != path.trim() {
            return Err(AcceptFolderError::PathChanged);
        }
        let library = self.library.read().ok_or(AcceptFolderError::NotTheBin)?;
        let registry = crate::settings::registry_from(&library);
        let mut trees = || (self.downloads)().ok_or(BinsUnknown::DownloadsUnread);
        let choice = choose(saved, &resolve_roots(&registry), &mut trees)
            .map_err(|_| AcceptFolderError::NotTheBin)?;
        let dir = configured_dir(&choice).ok_or(AcceptFolderError::NotTheBin)?;
        if has_marker(dir) {
            return Ok(false);
        }
        mark_folder(dir).map_err(AcceptFolderError::Unwritable)?;
        self.mark_row(saved, Lane::Foreground)?;
        tracing::info!(
            path = saved,
            "an admin chose the recycle bin folder; it is marked again"
        );
        Ok(true)
    }

    /// The bins expiry may delete from for `registry`'s roots now, with a
    /// line for each bin left out ([`expiry_guard`]). The bins are the
    /// ones a session would choose, from the same resolved roots and
    /// download folders. `Err` with the cause while they are unknown.
    /// Logs nothing: expiry keeps its own log memory.
    pub fn for_expiry(
        &self,
        registry: &RootRegistry,
    ) -> Result<(Vec<Root>, Vec<String>), BinsUnknown> {
        let resolved = resolve_roots(registry);
        let downloads = (self.downloads)().ok_or(BinsUnknown::DownloadsUnread)?;
        let choice = self.read(&resolved, Some(&downloads), Marking::Compare)?;
        let left_out = paths::left_out(&resolved);
        Ok(expiry_guard(
            choice.roots,
            &library_roots(registry),
            &downloads,
            &left_out,
        ))
    }

    /// What the Library Management settings say about the bins with
    /// `configured` as the bin path: why it would not be used, and each
    /// library folder whose `.recycle` is a link the checks refuse, so its
    /// removals wait. `None` when every bin would be used, or while that
    /// cannot be told. It marks nothing. Blocking: it resolves paths.
    pub fn warning(&self, configured: &str) -> Option<RecycleBinWarning> {
        let library = self.library.read()?;
        let registry = crate::settings::registry_from(&library);
        let mut trees = || (self.downloads)().ok_or(BinsUnknown::DownloadsUnread);
        let checked = choose(configured, &resolve_roots(&registry), &mut trees)
            .and_then(|choice| self.held_to_mark(configured.trim(), choice, Marking::Compare));
        let choice = match checked {
            Ok(choice) => choice,
            Err(BinsUnknown::Unreachable) => {
                return Some(RecycleBinWarning::new(BinRefusal::Unresolvable.message()));
            }
            Err(BinsUnknown::Unmarked) => {
                return Some(RecycleBinWarning {
                    message: UNMARKED_WARNING.to_owned(),
                    accept_folder: true,
                });
            }
            Err(BinsUnknown::SettingsUnread | BinsUnknown::DownloadsUnread) => return None,
        };
        let mut lines: Vec<String> = choice
            .refused
            .iter()
            .map(|refusal| refusal.message().to_owned())
            .collect();
        for (root_id, refusal) in &choice.unusable {
            let Some(root) = registry.resolve(root_id) else {
                continue;
            };
            let why = match refusal {
                BinRefusal::Unresolvable => "is a link that can't be followed",
                BinRefusal::NotAbsolute
                | BinRefusal::OverlapsLibrary
                | BinRefusal::OverlapsDownloads => "links into a library or download folder",
            };
            lines.push(format!(
                "Removals from {} wait: its .recycle {why}.",
                root.path.display()
            ));
        }
        (!lines.is_empty()).then(|| RecycleBinWarning::new(&lines.join(" ")))
    }

    fn log(&self, choice: &Result<BinChoice, BinsUnknown>) {
        let mut logged = self.logged.lock().unwrap_or_else(PoisonError::into_inner);
        match choice {
            Ok(choice) => {
                let state = problems(choice);
                if logged.problems != state {
                    if let Some(text) = &state {
                        tracing::warn!("{text}");
                    }
                    logged.problems = state;
                }
            }
            Err(cause) if logged.waits.insert(*cause) => {
                tracing::warn!("no recycle bin is known while {cause}; removals and upgrades wait");
            }
            Err(cause) => {
                tracing::debug!(
                    "no recycle bin is known while {cause}; removals and upgrades wait"
                );
            }
        }
    }
}

/// Record, in the upgrade's transaction `tx`, that v2 used the recycle bin
/// path `path`, at `at_ms` (unix milliseconds). Until that path is marked,
/// its folder is taken for the bin only while it holds an entry folder.
/// Answers how many rows it wrote: none when the path was recorded before.
pub fn record_v2_path(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    at_ms: i64,
) -> rusqlite::Result<usize> {
    tx.prepare_cached(super::sql::BIN_V2_CARRY.text())?
        .execute(rusqlite::params![path.trim(), at_ms])
}

/// The configured bin's folder, when `choice` uses it.
fn configured_dir(choice: &BinChoice) -> Option<&Path> {
    choice
        .roots
        .iter()
        .find(|root| root.id == CONFIGURED_BIN_ID)
        .map(|root| root.dir.as_path())
}

/// Whether expiry may delete the old entries no journal row names at the
/// top of `bin` by their date: a library folder's `.recycle`, or the
/// configured bin once its folder holds the marker. Until then the entries
/// v2 left are what shows its folder is the bin, so they wait for its first
/// use to mark it.
pub(super) fn prunes_old_entries(bin: &Root) -> bool {
    bin.id != CONFIGURED_BIN_ID || has_marker(&bin.dir)
}

/// Whether the folder `dir` holds the marker, as a plain file.
fn has_marker(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(BIN_MARKER)).is_ok_and(|meta| meta.file_type().is_file())
}

/// Make the marker in `dir`, a resolved folder, unless a plain file is
/// already at its name. Anything else there fails with `AlreadyExists`.
fn mark_folder(dir: &Path) -> std::io::Result<()> {
    match write_marker(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && has_marker(dir) => {
            Ok(())
        }
        written => written,
    }
}

/// Make the marker in `dir`, a resolved folder: every folder on the way is
/// opened without following a link, and the marker is made new, never
/// through a link at its name. It and its folder are synced before the
/// path's row is written. `AlreadyExists` when anything is at its name.
fn write_marker(dir: &Path) -> std::io::Result<()> {
    use droppedneedle_kernel::beneath::{RootLinks, parent_beneath};
    use std::io::Write as _;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let (folder, name) = parent_beneath(dir, &dir.join(BIN_MARKER), RootLinks::Refuse)?;
    // SAFETY: `folder` is an open descriptor and `name` a NUL-terminated
    // string, both alive for the call.
    let fd = unsafe {
        libc::openat(
            folder.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o644 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by a successful `openat` and nothing
    // else owns it.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(MARKER_TEXT)?;
    file.sync_all()?;
    std::fs::File::open(dir)?.sync_all()
}

/// Every saved root of `registry` with the real folder it resolves to now.
/// Blocking: it reads the disk.
fn resolve_roots(registry: &RootRegistry) -> Vec<ResolvedRoot> {
    registry
        .roots()
        .iter()
        .map(|root| ResolvedRoot::of(&root.id, &root.path))
        .collect()
}

/// What is wrong with `choice`, in one line for the log.
fn problems(choice: &BinChoice) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(refusal) = choice.refused {
        lines.push(format!(
            "the configured recycle bin is not used: {}",
            refusal.message()
        ));
    }
    for (root, refusal) in &choice.unusable {
        let why = match refusal {
            BinRefusal::Unresolvable => "its .recycle is a link that can't be followed",
            BinRefusal::NotAbsolute
            | BinRefusal::OverlapsLibrary
            | BinRefusal::OverlapsDownloads => {
                "its .recycle links into a library or download folder"
            }
        };
        lines.push(format!(
            "library root {root} has no usable recycle bin: {why}; its files wait"
        ));
    }
    (!lines.is_empty()).then(|| lines.join("; "))
}

/// The bin among `bins` that serves library root `root_id`.
pub fn serving<'b>(bins: &'b [Root], root_id: &str) -> Option<&'b Root> {
    bins.iter().find(|bin| match &bin.kind {
        RootKind::Bin { serves } => serves.as_deref().is_none_or(|id| id == root_id),
        RootKind::Library | RootKind::RemoveOnly => false,
    })
}

/// Every configured root of `registry`, as library roots.
pub fn library_roots(registry: &RootRegistry) -> Vec<Root> {
    registry
        .roots()
        .iter()
        .map(|root| Root::library(root.id.clone(), root.path.clone()))
        .collect()
}

/// A fresh entry for a file named `name` in the bin at `bin`:
/// `<bin>/<UTC stamp>-<8 hex>/<name>`, not made yet. `None` for a name
/// that is empty, `.`, `..` or holds a `/`. A backslash is an ordinary
/// character in a file name here.
pub fn fresh_entry(bin: &Path, name: &str, now: SystemTime) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    Some(
        bin.join(format!(
            "{}-{}",
            utc_stamp(now),
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ))
        .join(name),
    )
}

/// `YYYYMMDDTHHMMSS` in UTC: every bin entry's name starts with it, and
/// expiry reads it for an entry no row names ([`entry_stamp`]).
pub fn utc_stamp(now: SystemTime) -> String {
    jiff::Timestamp::try_from(now)
        .unwrap_or(jiff::Timestamp::UNIX_EPOCH)
        .strftime("%Y%m%dT%H%M%S")
        .to_string()
}

/// Whether `name` is the name of a bin entry folder made since removals
/// went through the publisher: `YYYYMMDDTHHMMSS-` and eight hex
/// characters.
pub(crate) fn entry_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 24
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'-'
        && bytes[16..].iter().all(u8::is_ascii_hexdigit)
}

/// When a bin entry folder of any DroppedNeedle release was made, in unix
/// seconds, read from its name: `<YYYYMMDDTHHMMSS>-<8 or more lowercase
/// hex>`. `None` for any other name. The name is the record, since a move
/// across disks rewrites modified times.
pub(crate) fn entry_stamp(name: &str) -> Option<i64> {
    let (stamp, unique) = name.split_once('-')?;
    if stamp.len() != 15
        || unique.len() < 8
        || !unique
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(4..6)?, number(6..8)?);
    if stamp.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    let (hour, minute, second) = (number(9..11)?, number(11..13)?, number(13..15)?);
    let narrow = |value: i64| i8::try_from(value).ok();
    let at = jiff::civil::DateTime::new(
        i16::try_from(year).ok()?,
        narrow(month)?,
        narrow(day)?,
        narrow(hour)?,
        narrow(minute)?,
        narrow(second)?,
        0,
    )
    .ok()?;
    Some(
        at.to_zoned(jiff::tz::TimeZone::UTC)
            .ok()?
            .timestamp()
            .as_second(),
    )
}

/// The bins expiry may delete from, out of `bins`: never one that is `/`,
/// is or holds a download folder, or holds a library folder, and never
/// the `.recycle` of a library folder `left_out` names (it takes no file
/// changes). Every path is compared resolved. Answers the bins kept and a
/// line for each one dropped.
pub fn expiry_guard(
    bins: Vec<Root>,
    library: &[Root],
    downloads: &[PathBuf],
    left_out: &[String],
) -> (Vec<Root>, Vec<String>) {
    let real = |path: &Path| resolve_existing(path).unwrap_or_else(|_| path.to_path_buf());
    let real_library: Vec<PathBuf> = library.iter().map(|root| real(&root.dir)).collect();
    let real_downloads: Vec<PathBuf> = downloads.iter().map(|tree| real(tree)).collect();
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for bin in bins {
        let dir = real(&bin.dir);
        let why = if dir.parent().is_none() {
            Some("is the filesystem root")
        } else if real_downloads.iter().any(|tree| tree.starts_with(&dir)) {
            Some("is or holds a download folder")
        } else if real_library.iter().any(|root| root.starts_with(&dir)) {
            Some("holds a library folder")
        } else if matches!(&bin.kind, RootKind::Bin { serves: Some(root) } if left_out.contains(root))
        {
            Some("belongs to a library folder that takes no file changes")
        } else {
            None
        };
        match why {
            Some(why) => dropped.push(format!(
                "recycle bin expiry leaves recycle bin {} alone: it {why}",
                bin.id
            )),
            None => kept.push(bin),
        }
    }
    (kept, dropped)
}

/// Move a committed row's replaced file from its set-aside name into its
/// bin entry, then record when it got there. An entry whose bin is no
/// longer one of the session's gets a fresh entry in the bin that serves
/// the row's root now, written durably before the file moves, with the
/// retired track's path when it still names the old entry. The entry
/// is checked under its bin (no symlink below the bin's folder) and the
/// file goes in by the bin's own placement mode. While the bins are
/// unknown nothing moves. A removal's file (a `remove` row) may sit under
/// a remove-only root. `known` is the file's identity when it was hashed,
/// when the caller has it. True when the file is in the bin now.
#[allow(clippy::too_many_arguments)]
pub(super) fn recycle_replaced(
    db: &Db,
    sandbox: &Sandbox,
    journal: &FileJournal,
    replaced: &JournalReplacement,
    backup: &Path,
    seams: Seams,
    clock: &dyn Clock,
    known: Option<FileStat>,
) -> Result<bool, PublishError> {
    sandbox.bins()?;
    match journal.kind {
        JournalKind::Remove => sandbox.ensure_under_removal_roots(backup)?,
        JournalKind::PutBack => {
            return Err(PublishError::Journal(
                format!(
                    "journal {} puts a file back; it sends nothing to a bin",
                    journal.id
                )
                .into(),
            ));
        }
        JournalKind::Audio
        | JournalKind::Sidecar
        | JournalKind::Artwork
        | JournalKind::Output
        | JournalKind::Add
        | JournalKind::Delete => sandbox.ensure_under_roots(backup)?,
    }
    let mut recycle_to = PathBuf::from(&replaced.recycle_to);
    if sandbox.bin_holding(&recycle_to).is_none() {
        if std::fs::symlink_metadata(backup).is_err() {
            return Ok(false);
        }
        let bin = sandbox.bin_for(&journal.dest_root)?;
        let name = recycle_to
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let fresh = fresh_entry(&bin.dir, name, clock.now_system()).ok_or_else(|| {
            PublishError::Cleanup("the replaced file has no name to recycle under".into())
        })?;
        let text = fresh.to_string_lossy().into_owned();
        let moved = durable(db, "publish.journal_rebin", |tx| {
            let writer = JournalWriter::new(tx);
            if !writer.set_recycle_to(&journal.id, &text)? {
                return Ok(false);
            }
            if let Some(track_id) = &replaced.track_id {
                writer.follow_rebin(track_id, &replaced.recycle_to, &text)?;
            }
            Ok(true)
        })?;
        if !moved {
            return Err(PublishError::Cleanup(
                "the replaced file's row changed while it was moved to a new recycle bin".into(),
            ));
        }
        tracing::info!(
            journal_id = %journal.id,
            bin = bin.id,
            "the replaced file's old recycle bin is gone; it goes to the current one"
        );
        recycle_to = fresh;
    }
    let bin = sandbox.ensure_under_bin(&recycle_to)?;
    let mode = sandbox.probe_bin_placement(bin);
    let moved = paths::recycle_backup(
        mode,
        seams,
        sandbox.bin_base(bin),
        backup,
        &recycle_to,
        &replaced.sha256,
        known,
    )?;
    if moved {
        // What the copy is once it landed: expiry deletes it while it is
        // still exactly this, whatever bytes it was given.
        let binned = FileStat::read(&recycle_to).ok();
        durable(db, "publish.journal_recycled", |tx| {
            // Stamped when it landed, not when its publish began: an empty
            // confirmed in between never counted it, so must not delete it.
            let landed = clock.now_ms() as f64 / 1000.0;
            JournalWriter::new(tx).set_recycled(&journal.id, landed, binned.as_ref())
        })?;
    }
    if let Some(parent) = backup.parent()
        && let Err(error) = super::journal::fsync_dir(parent)
    {
        tracing::warn!(dir = %parent.display(), %error, "directory sync failed");
    }
    Ok(moved)
}
