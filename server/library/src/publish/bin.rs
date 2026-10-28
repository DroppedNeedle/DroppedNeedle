//! Recycle bins as sandbox roots: which bins a session holds, the checks
//! a configured bin passes before it is used, fresh entry names, and the
//! move of a replaced file into its bin after its bundle committed.
//!
//! The configured bin (the download settings' `recycle_bin_path`) is
//! resolved once per session, symlinks and all, and every overlap check
//! runs on the resolved folder. A configured bin that is, contains or sits
//! inside a library root (other than as that root's `.recycle`), or that
//! overlaps a download folder, is not used: each root's `.recycle` takes
//! its place, logged once per change and shown on the download settings.
//! While the download settings cannot be read the bins are unknown, and
//! nothing that would go into a bin moves.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

use super::PublishError;
use super::journal::{FileJournal, JournalKind, JournalReplacement, JournalWriter};
use super::paths::{self, FileStat, Root, RootKind, Sandbox};
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
    /// The path could not be resolved.
    Unresolvable,
    /// It is, holds or sits inside a library folder.
    OverlapsLibrary,
    /// It is, holds or sits inside a download folder.
    OverlapsDownloads,
}

impl BinRefusal {
    /// What the download settings say about it.
    pub fn message(self) -> &'static str {
        match self {
            Self::NotAbsolute => {
                "The recycle bin path is not a full path, so each library folder's .recycle is used instead."
            }
            Self::Unresolvable => {
                "The recycle bin path can't be read, so each library folder's .recycle is used instead."
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
/// checked the same way; one that fails leaves its root with no bin.
pub fn choose(configured: &str, library: &[Root], downloads: &[PathBuf]) -> BinChoice {
    let real_library: Vec<PathBuf> = library
        .iter()
        .map(|root| resolve_existing(&root.dir).unwrap_or_else(|_| root.dir.clone()))
        .collect();
    let real_downloads: Vec<PathBuf> = downloads
        .iter()
        .map(|tree| resolve_existing(tree).unwrap_or_else(|_| tree.clone()))
        .collect();
    let clear = |target: &Path| check_target(target, &real_library, &real_downloads);
    let per_root = |refused| {
        let mut choice = BinChoice {
            roots: Vec::new(),
            refused,
            unusable: Vec::new(),
        };
        for root in library {
            let own = root.dir.join(RECYCLE_BIN_DIRECTORY_NAME);
            let linked =
                std::fs::symlink_metadata(&own).is_ok_and(|meta| meta.file_type().is_symlink());
            if !linked {
                choice.roots.push(Root::recycle_of(&root.id, &root.dir));
                continue;
            }
            let target = std::fs::canonicalize(&own)
                .map_err(|_| BinRefusal::Unresolvable)
                .and_then(|target| clear(&target).map_or(Ok(target), Err));
            match target {
                Ok(target) => choice.roots.push(Root {
                    dir: target,
                    ..Root::recycle_of(&root.id, &root.dir)
                }),
                Err(refusal) => choice.unusable.push((root.id.clone(), refusal)),
            }
        }
        choice
    };
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return per_root(None);
    }
    let path = Path::new(trimmed);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return per_root(Some(BinRefusal::NotAbsolute));
    }
    let Ok(resolved) = resolve_existing(path) else {
        return per_root(Some(BinRefusal::Unresolvable));
    };
    if let Some(refusal) = clear(&resolved) {
        return per_root(Some(refusal));
    }
    BinChoice {
        roots: vec![Root::bin(CONFIGURED_BIN_ID, resolved)],
        refused: None,
        unusable: Vec::new(),
    }
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

/// Why no bin is known: the setting that cannot be read.
pub type BinsUnknown = &'static str;

/// Where the library's bins come from: the download settings' recycle bin
/// path, checked against the library roots and the download folders.
#[derive(Clone)]
pub struct BinSource {
    policy: Live<DownloadPolicy>,
    library: Live<TypedLibrary>,
    downloads: DownloadTrees,
    /// What was last logged about the bins, so each change is logged once.
    logged: Arc<Mutex<Option<String>>>,
}

impl BinSource {
    /// Bins from `policy`, checked against the roots in `library` and the
    /// folders `downloads` names.
    pub fn new(
        policy: Live<DownloadPolicy>,
        library: Live<TypedLibrary>,
        downloads: DownloadTrees,
    ) -> Self {
        Self {
            policy,
            library,
            downloads,
            logged: Arc::default(),
        }
    }

    /// The bins for `registry`'s roots now. `Err` with the cause while the
    /// download settings, or a setting naming a download folder, cannot be
    /// read: then nothing moves into a bin and no other bin stands in.
    pub fn current(&self, registry: &RootRegistry) -> Result<BinChoice, BinsUnknown> {
        let choice = self.read(registry);
        self.log_once(match &choice {
            Ok(choice) => problems(choice),
            Err(cause) => Some(format!("no recycle bin is known while {cause}")),
        });
        choice
    }

    fn read(&self, registry: &RootRegistry) -> Result<BinChoice, BinsUnknown> {
        let policy = self
            .policy
            .read()
            .ok_or("the download settings cannot be read")?;
        let library = library_roots(registry);
        // The download folders matter only to a bin that could reach them:
        // a configured one, or a `.recycle` that is a link.
        let reaches_out = !policy.recycle_bin_path.trim().is_empty()
            || library.iter().any(|root| {
                std::fs::symlink_metadata(root.dir.join(RECYCLE_BIN_DIRECTORY_NAME))
                    .is_ok_and(|meta| meta.file_type().is_symlink())
            });
        let downloads = if reaches_out {
            (self.downloads)().ok_or("the SABnzbd settings cannot be read")?
        } else {
            Vec::new()
        };
        Ok(choose(&policy.recycle_bin_path, &library, &downloads))
    }

    /// The bins expiry may delete from for `registry`'s roots now, with a
    /// line for each bin left out ([`expiry_guard`]). `Err` with the cause
    /// while the download settings, or a setting naming a download folder,
    /// cannot be read. Logs nothing: expiry keeps its own log memory.
    pub fn for_expiry(
        &self,
        registry: &RootRegistry,
    ) -> Result<(Vec<Root>, Vec<String>), BinsUnknown> {
        let choice = self.read(registry)?;
        let downloads = (self.downloads)().ok_or("the SABnzbd settings cannot be read")?;
        let resolved: Vec<paths::ResolvedRoot> = registry
            .roots()
            .iter()
            .map(|root| paths::ResolvedRoot::of(&root.id, &root.path))
            .collect();
        let left_out = paths::left_out(&resolved);
        Ok(expiry_guard(
            choice.roots,
            &library_roots(registry),
            &downloads,
            &left_out,
        ))
    }

    /// Why `configured` would not be used as the bin, for the download
    /// settings. `None` when it would be, or while that cannot be told.
    /// Blocking: it resolves paths.
    pub fn refusal(&self, configured: &str) -> Option<BinRefusal> {
        if configured.trim().is_empty() {
            return None;
        }
        let library = self.library.read()?;
        let downloads = (self.downloads)()?;
        let registry = crate::settings::registry_from(&library);
        choose(configured, &library_roots(&registry), &downloads).refused
    }

    fn log_once(&self, state: Option<String>) {
        let mut logged = self.logged.lock().unwrap_or_else(PoisonError::into_inner);
        if *logged == state {
            return;
        }
        if let Some(text) = &state {
            tracing::warn!("{text}");
        }
        *logged = state;
    }
}

/// What is wrong with `choice`, in one line for the log.
fn problems(choice: &BinChoice) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(refusal) = choice.refused {
        lines.push(format!(
            "the configured recycle bin is not used ({refusal:?}); each library folder's .recycle is"
        ));
    }
    for (root, refusal) in &choice.unusable {
        lines.push(format!(
            "library root {root} has no usable recycle bin: its .recycle links somewhere it may not ({refusal:?}); its files wait"
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
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
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
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Howard Hinnant's days-from-civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
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
/// the row's root now, written durably before the file moves. The entry
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
    now: SystemTime,
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
        let fresh = fresh_entry(&bin.dir, name, now).ok_or_else(|| {
            PublishError::Cleanup("the replaced file has no name to recycle under".into())
        })?;
        let text = fresh.to_string_lossy().into_owned();
        let moved = durable(db, "publish.journal_rebin", |tx| {
            JournalWriter::new(tx).set_recycle_to(&journal.id, &text)
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
    let moved = paths::recycle_backup(mode, seams, backup, &recycle_to, &replaced.sha256, known)?;
    if moved {
        durable(db, "publish.journal_recycled", |tx| {
            JournalWriter::new(tx).set_recycled(&journal.id, unix_secs(now))
        })?;
    }
    if let Some(parent) = backup.parent()
        && let Err(error) = super::journal::fsync_dir(parent)
    {
        tracing::warn!(dir = %parent.display(), %error, "directory sync failed");
    }
    Ok(moved)
}

fn unix_secs(now: SystemTime) -> f64 {
    now.duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
