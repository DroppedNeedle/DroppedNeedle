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

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
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

/// Id of the configured bin in a session.
pub const CONFIGURED_BIN_ID: &str = "recycle-bin";

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
}

impl std::fmt::Display for BinsUnknown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SettingsUnread => "the Library Management settings can't be read",
            Self::DownloadsUnread => "the slskd or SABnzbd settings can't be read",
            Self::Unreachable => "the recycle bin folder isn't there or can't be reached",
        })
    }
}

/// Where the library's bins come from: Library Management's recycle bin
/// path, checked against the library roots and the download folders.
#[derive(Clone)]
pub struct BinSource {
    management: Live<LibraryManagement>,
    library: Live<TypedLibrary>,
    downloads: DownloadTrees,
    logged: Arc<Mutex<Logged>>,
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
    /// the folders `downloads` names.
    pub fn new(
        management: Live<LibraryManagement>,
        library: Live<TypedLibrary>,
        downloads: DownloadTrees,
    ) -> Self {
        Self {
            management,
            library,
            downloads,
            logged: Arc::default(),
        }
    }

    /// The bins for `registry`'s roots now. Blocking: it resolves the
    /// roots.
    pub fn current(&self, registry: &RootRegistry) -> Result<BinChoice, BinsUnknown> {
        self.current_over(&resolve_roots(registry))
    }

    /// The bins for `roots`, already resolved, now. `Err` with the cause
    /// while the Library Management settings or a setting naming a download folder
    /// cannot be read, or while the configured bin cannot be reached: then
    /// nothing moves into a bin and no other bin stands in.
    pub fn current_over(&self, roots: &[ResolvedRoot]) -> Result<BinChoice, BinsUnknown> {
        let choice = self.read(roots, None);
        self.log(&choice);
        choice
    }

    fn read(
        &self,
        roots: &[ResolvedRoot],
        downloads: Option<&[PathBuf]>,
    ) -> Result<BinChoice, BinsUnknown> {
        let management = self.management.read().ok_or(BinsUnknown::SettingsUnread)?;
        let mut trees = || match downloads {
            Some(trees) => Ok(trees.to_vec()),
            None => (self.downloads)().ok_or(BinsUnknown::DownloadsUnread),
        };
        choose(&management.recycle_bin_path, roots, &mut trees)
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
        let choice = self.read(&resolved, Some(&downloads))?;
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
    /// cannot be told. Blocking: it resolves paths.
    pub fn warning(&self, configured: &str) -> Option<String> {
        let library = self.library.read()?;
        let registry = crate::settings::registry_from(&library);
        let mut trees = || (self.downloads)().ok_or(BinsUnknown::DownloadsUnread);
        let choice = match choose(configured, &resolve_roots(&registry), &mut trees) {
            Ok(choice) => choice,
            Err(BinsUnknown::Unreachable) => {
                return Some(BinRefusal::Unresolvable.message().to_owned());
            }
            Err(_) => return None,
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
        (!lines.is_empty()).then(|| lines.join(" "))
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
