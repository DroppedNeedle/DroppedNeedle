//! Taking library files to the recycle bin under `remove` journal rows.
//!
//! A removal reads each file once, before the publish lock is taken: its
//! bytes for the journal, and its identity on disk (device, inode, size,
//! modified time and change time). Under the lock the plan checks the
//! whole identity again and seals each track's rows as they are.
//! Publishing checks the whole identity once more just before each file
//! moves, so a file saved since it was read stays where it is and the
//! removal unwinds; for a hard link to a file an earlier row already
//! moved, that check leaves out the change time. Once the file is under
//! its row's hidden name it is checked again without the change time,
//! which the rename moves: a file swapped in that moment goes straight
//! back. The commit marks
//! the tracks missing, in the same transaction that checks their rows,
//! and only after it does each file move into its bin entry. An unwind
//! puts every set-aside file back; a copy that cannot go back is kept
//! under its hidden name, never deleted.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use droppedneedle_kernel::db::Db;
use rusqlite::{Connection, OptionalExtension};

use super::journal::{FileJournal, JournalStore};
use super::paths::{self, FileStat, Sandbox};
use super::place::{self, Original, PlaceError, PlacementMode, Seams};
use super::planner::{
    FileFingerprint, Removal, RemoveFile, RemoveItem, RemoveSidecar, UndoRecord, removal_moved,
    removal_seals,
};
use super::{PublishError, sql};

/// One track a removal takes away, as the caller found it, with what was
/// read of its file before the publish lock.
#[derive(Debug, Clone)]
pub struct Target {
    pub track_id: String,
    pub root_id: String,
    pub relative_path: String,
    pub probe: Probe,
    /// The bytes the file must hold (a duplicate resolution read them when
    /// it was previewed). A file holding others, or none, is not removed.
    pub expect_sha256: Option<String>,
}

/// What was at a track's path before the publish lock.
#[derive(Debug, Clone)]
pub enum Probe {
    /// No file to move: it is missing, a symlink, or not a regular file.
    Gone,
    File {
        path: PathBuf,
        fingerprint: FileFingerprint,
        stat: FileStat,
    },
}

/// Why a removal did not happen.
#[derive(Debug)]
pub enum Refusal {
    /// Another write holds the library folder or one of the tracks.
    Busy,
    /// The library-wide switch is off.
    LibraryOff,
    /// A track's rows or its file changed since the caller read them.
    Changed,
    /// A track's library folder is not one the publisher holds.
    OutsideRoot,
    /// No recycle bin is known while this setting cannot be read.
    BinsUnknown(super::bin::BinsUnknown),
    /// This library folder has no usable recycle bin.
    NoBin(String),
    /// The publish refused or failed. `kept`: its unwind kept a file under
    /// a hidden name.
    Publish { error: PublishError, kept: bool },
}

impl From<PublishError> for Refusal {
    fn from(error: PublishError) -> Self {
        Self::Publish { error, kept: false }
    }
}

impl From<droppedneedle_kernel::db::DbError> for Refusal {
    fn from(error: droppedneedle_kernel::db::DbError) -> Self {
        Self::from(PublishError::from(error))
    }
}

/// Read the file at `path` under library folder `root` once, before the
/// publish lock: its bytes and its identity. A symlink below the folder,
/// a missing file or anything but a regular file has nothing to move. The
/// folder may be reached through a symlink, but one that is not a folder
/// now (gone, or a link to nothing) refuses (`FolderMissing`), so its
/// tracks are not marked missing while their files may still exist. A
/// path that is not plainly inside the folder refuses. A file that changed
/// while it was read refuses. A file found gone is checked against the
/// folder once more, so a folder unmounted mid-probe refuses rather than
/// reading as a missing file.
pub fn probe(root: &Path, path: &Path) -> Result<Probe, PublishError> {
    match probe_file(root, path)? {
        Probe::Gone => {
            paths::ensure_library_dir(root)?;
            Ok(Probe::Gone)
        }
        found => Ok(found),
    }
}

fn probe_file(root: &Path, path: &Path) -> Result<Probe, PublishError> {
    paths::ensure_library_dir(root)?;
    if paths::below_root(root, path).is_none_or(|below| below.as_os_str().is_empty()) {
        return Err(PublishError::UnsafePath(
            "path is not inside its library folder".into(),
        ));
    }
    match paths::no_symlink_below(root, path) {
        Ok(()) => {}
        Err(PublishError::UnsafePath(_)) => return Ok(Probe::Gone),
        Err(error) => return Err(error),
    }
    let before = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => FileStat::of(&meta),
        Ok(_) => return Ok(Probe::Gone),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Probe::Gone),
        Err(error) => return Err(PublishError::from(error)),
    };
    let hashed = paths::hash_regular_file(path)?;
    if hashed.stat != before {
        return Err(PublishError::Validation(
            "a file changed while it was read".into(),
        ));
    }
    Ok(Probe::File {
        path: path.to_path_buf(),
        fingerprint: FileFingerprint {
            size: hashed.size,
            sha256: hashed.sha256,
        },
        stat: before,
    })
}

/// A sidecar found to go with a removal, read once before the publish
/// lock as [`probe`] reads a track's file.
#[derive(Debug, Clone)]
pub struct SidecarTarget {
    pub root_id: String,
    pub relative_path: String,
    /// See [`RemoveSidecar::emptied_folder`].
    pub emptied_folder: Option<String>,
    pub probe: Probe,
}

/// What [`sidecars`] needs to know about one library folder's removal.
pub struct SidecarScope<'a> {
    pub root_id: &'a str,
    pub root: &'a Path,
    /// The removed tracks' paths in this library folder.
    pub removed: &'a [&'a str],
    /// The library folder's sidecar patterns, case-folded.
    pub patterns: &'a [Vec<char>],
    /// Whether a track that is not being removed is listed below a folder
    /// (relative to the library folder).
    pub listed_below: &'a dyn Fn(&str) -> bool,
}

/// Most entries read below one folder while looking for music left in it.
const MAX_FOLDER_ENTRIES: usize = 10_000;

/// The sidecars that go with removing `scope.removed`, read before the
/// publish lock. A file matching the folder's sidecar patterns goes:
///
/// - with its track, when it is named like the track's file (its lyrics,
///   its cue sheet), whatever else stays in the folder;
/// - with the folder, once no music is left there: no other track is
///   listed below it and no other music file is on disk below it. The
///   folders above go the same way, up to the first that still holds
///   music, never the library folder itself.
///
/// Anything else stays, and so does the folder that holds it. A link, a
/// management file and a folder too large to read are left alone.
pub fn sidecars(scope: &SidecarScope<'_>) -> Vec<SidecarTarget> {
    use std::collections::{BTreeMap, BTreeSet};
    let removed: BTreeSet<&str> = scope.removed.iter().copied().collect();
    let mut found: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut folders: BTreeSet<String> = BTreeSet::new();
    for rel in scope.removed {
        let rel_path = Path::new(rel);
        let folder = parent_rel(rel);
        let Some(stem) = rel_path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        for name in names_in(&scope.root.join(&folder)) {
            let named_like = Path::new(&name)
                .file_stem()
                .and_then(|other| other.to_str())
                .is_some_and(|other| other == stem);
            let sidecar = join_rel(&folder, &name);
            if named_like
                && !removed.contains(sidecar.as_str())
                && crate::tags::format_for_path(Path::new(&name)).is_err()
                && crate::management::planner::sidecar_match(
                    scope.patterns,
                    &caseless::default_case_fold_str(&name),
                )
            {
                found.insert(sidecar, None);
            }
        }
        if !folder.is_empty() {
            folders.insert(folder);
        }
    }
    let deep = scope.patterns.iter().any(|pattern| pattern.contains(&'/'));
    let mut emptied: BTreeMap<String, String> = BTreeMap::new();
    for folder in &folders {
        let mut chain = Vec::new();
        let mut cursor = Some(folder.clone());
        while let Some(current) = cursor.filter(|current| !current.is_empty()) {
            if (scope.listed_below)(&current)
                || music_below(&scope.root.join(&current), &current, &removed)
            {
                break;
            }
            cursor = Some(parent_rel(&current));
            chain.push(current);
        }
        let Some(top) = chain.last().cloned() else {
            continue;
        };
        for current in chain {
            emptied.insert(current, top.clone());
        }
    }
    for (folder, top) in &emptied {
        let Some(names) = crate::management::planner::sidecar_candidates(
            &scope.root.join(folder),
            deep,
            scope.patterns,
        ) else {
            tracing::info!(
                folder = %folder,
                "a removed album's folder was not read for its sidecars"
            );
            continue;
        };
        for name in names {
            let sidecar = join_rel(folder, &name);
            if !removed.contains(sidecar.as_str()) {
                found.entry(sidecar).or_insert_with(|| Some(top.clone()));
            }
        }
    }
    found
        .into_iter()
        .filter(|(rel, _)| !crate::scan::fs::is_management_artifact(Path::new(rel)))
        .filter_map(|(rel, emptied_folder)| {
            let probe = probe_file(scope.root, &scope.root.join(&rel)).ok()?;
            matches!(probe, Probe::File { .. }).then(|| SidecarTarget {
                root_id: scope.root_id.to_owned(),
                relative_path: rel,
                emptied_folder,
                probe,
            })
        })
        .collect()
}

/// The folder of a library-relative path, empty at the top.
fn parent_rel(rel: &str) -> String {
    Path::new(rel)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn join_rel(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

/// The names of the regular files directly in `dir`.
fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .collect()
}

/// Whether a music file other than `removed` is on disk below `dir`
/// (`rel` relative to the library folder). A folder that can't be read
/// in full, or holds a link, counts as holding music.
fn music_below(dir: &Path, rel: &str, removed: &std::collections::BTreeSet<&str>) -> bool {
    let mut pending = vec![(dir.to_path_buf(), rel.to_owned())];
    let mut examined = 0;
    while let Some((path, prefix)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            return true;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return true;
            };
            examined += 1;
            if examined > MAX_FOLDER_ENTRIES {
                return true;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return true;
            };
            if crate::scan::fs::is_management_artifact(Path::new(&name)) {
                continue;
            }
            let child = join_rel(&prefix, &name);
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => pending.push((entry.path(), child)),
                Ok(kind) if kind.is_file() => {
                    if crate::tags::format_for_path(Path::new(&name)).is_ok()
                        && !removed.contains(child.as_str())
                    {
                        return true;
                    }
                }
                _ => return true,
            }
        }
    }
    false
}

/// Plan a removal under the publish lock, against `sandbox`. First, in one
/// read: no other write may still hold any of the tracks (whatever the
/// file is doing), and each track's rows are sealed as they are now. Then
/// each file found before the lock must still be the same file at the
/// same place, and gets a fresh entry in the bin that serves its folder.
/// The sidecars found before the lock are checked the same way; one that
/// is gone or changed since stays out of the removal.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    db: &Db,
    sandbox: &Sandbox,
    bundle_id: &str,
    targets: &[Target],
    sidecars: &[SidecarTarget],
    reason_code: &str,
    actor: Option<&str>,
    undo: Option<UndoRecord>,
    now: SystemTime,
) -> Result<Removal, Refusal> {
    let sealed = db.read_blocking("publish.plan_removal", |conn| {
        let journals = JournalStore::new(conn);
        for target in targets {
            if journals
                .unsettled_bundle_for_track(&target.track_id, bundle_id)?
                .is_some()
            {
                return Ok::<_, PublishError>(None);
            }
        }
        removal_seals(conn, targets.iter().map(|target| target.track_id.as_str())).map(Some)
    })?;
    let Some(seals) = sealed else {
        return Err(Refusal::Busy);
    };
    let mut items = Vec::with_capacity(targets.len());
    for target in targets {
        let seal = seals.get(&target.track_id).ok_or(Refusal::Changed)?;
        if seal.root_id != target.root_id || seal.relative_path != target.relative_path {
            return Err(Refusal::Changed);
        }
        let file = match &target.probe {
            Probe::Gone if target.expect_sha256.is_some() => return Err(Refusal::Changed),
            Probe::Gone => {
                tracing::warn!(
                    file = %Path::new(&target.relative_path).file_name().unwrap_or_default().to_string_lossy(),
                    "file already absent; removing its catalog entry"
                );
                None
            }
            Probe::File {
                path,
                fingerprint,
                stat,
            } => {
                if target
                    .expect_sha256
                    .as_deref()
                    .is_some_and(|expected| expected != fingerprint.sha256)
                {
                    return Err(Refusal::Changed);
                }
                Some(plan_file(
                    sandbox,
                    (&target.root_id, &target.relative_path),
                    path,
                    fingerprint,
                    *stat,
                    now,
                )?)
            }
        };
        items.push(RemoveItem {
            track_id: target.track_id.clone(),
            seal: seal.clone(),
            file,
        });
    }
    let mut kept = Vec::with_capacity(sidecars.len());
    for sidecar in sidecars {
        let Probe::File {
            path,
            fingerprint,
            stat,
        } = &sidecar.probe
        else {
            continue;
        };
        let place = (sidecar.root_id.as_str(), sidecar.relative_path.as_str());
        match plan_file(sandbox, place, path, fingerprint, *stat, now) {
            Ok(file) => kept.push(RemoveSidecar {
                root_id: sidecar.root_id.clone(),
                relative_path: sidecar.relative_path.clone(),
                emptied_folder: sidecar.emptied_folder.clone(),
                file,
            }),
            Err(Refusal::Changed) => {
                tracing::info!(
                    file = %sidecar.relative_path,
                    "a sidecar changed since it was read; it stays"
                );
            }
            Err(refused) => return Err(refused),
        }
    }
    Ok(Removal {
        reason_code: reason_code.to_owned(),
        actor: actor.map(str::to_owned),
        items,
        sidecars: kept,
        undo,
        album_removed: None,
    })
}

fn plan_file(
    sandbox: &Sandbox,
    (root_id, rel): (&str, &str),
    path: &Path,
    fingerprint: &FileFingerprint,
    stat: FileStat,
    now: SystemTime,
) -> Result<RemoveFile, Refusal> {
    sandbox
        .removal_root_dir(root_id)
        .map_err(|_| Refusal::OutsideRoot)?;
    let resolved = sandbox
        .resolve_removal(root_id, rel)
        .map_err(|_| Refusal::Changed)?;
    if resolved != path || FileStat::read(&resolved).ok() != Some(stat) {
        return Err(Refusal::Changed);
    }
    if let Some(cause) = sandbox.bins_unknown() {
        return Err(Refusal::BinsUnknown(cause));
    }
    let bin = sandbox
        .bin_for(root_id)
        .map_err(|_| Refusal::NoBin(root_id.to_owned()))?;
    let recycle_to = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| super::bin::fresh_entry(&bin.dir, name, now))
        .ok_or_else(|| {
            Refusal::from(PublishError::UnsafePath(
                "the file has no name to recycle under".into(),
            ))
        })?;
    sandbox.ensure_under_bin(&recycle_to)?;
    Ok(RemoveFile {
        path: path.to_string_lossy().into_owned(),
        fingerprint: fingerprint.clone(),
        stat,
        recycle_to: recycle_to.to_string_lossy().into_owned(),
    })
}

/// The journal id of the removal's `index`th item.
pub(super) fn journal_id(bundle_id: &str, index: usize) -> String {
    format!("{bundle_id}-recycle-{index:03}")
}

/// The journal id of the removal's `index`th sidecar.
pub(super) fn sidecar_journal_id(bundle_id: &str, index: usize) -> String {
    format!("{bundle_id}-recycle-side-{index:03}")
}

/// Every file a removal moves, in the order its rows are written, each
/// with its journal id: the tracks' files, then the sidecars.
pub(super) fn files<'a>(bundle_id: &str, removal: &'a Removal) -> Vec<(String, &'a RemoveFile)> {
    let tracks = removal
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| Some((journal_id(bundle_id, index), item.file.as_ref()?)));
    let sidecars = removal
        .sidecars
        .iter()
        .enumerate()
        .map(|(index, sidecar)| (sidecar_journal_id(bundle_id, index), &sidecar.file));
    tracks.chain(sidecars).collect()
}

/// The ids of every row a removal's prepare writes: one per file.
pub(super) fn journal_ids(bundle_id: &str, removal: &Removal) -> Vec<String> {
    files(bundle_id, removal)
        .into_iter()
        .map(|(id, _)| id)
        .collect()
}

/// The planned file a `remove` row moves.
pub(crate) fn file_of<'a>(removal: &'a Removal, journal: &FileJournal) -> Option<&'a RemoveFile> {
    files(&journal.bundle_id, removal)
        .into_iter()
        .find(|(id, _)| *id == journal.id)
        .map(|(_, file)| file)
}

/// Whether the file a `remove` row moves is a hard link to the file of an
/// row of the same removal that is set aside first: one inode under two
/// names. Rows run in journal id order, which past 1000 items is not the
/// plan's (`-1000` sorts before `-101`). Setting the earlier one aside moves
/// the inode's change time.
pub(super) fn linked_to_earlier(removal: &Removal, journal: &FileJournal) -> bool {
    let files = files(&journal.bundle_id, removal);
    let Some(index) = files.iter().position(|(id, _)| *id == journal.id) else {
        return false;
    };
    let (mine, file) = (&files[index].0, files[index].1);
    files.iter().any(|(other, earlier)| {
        other < mine && earlier.stat.dev == file.stat.dev && earlier.stat.ino == file.stat.ino
    })
}

/// Whether the set-aside file at `path` holds `sha256`: by identity when
/// `known` still matches it as moved, else by reading it.
pub(super) fn holds(path: &Path, sha256: &str, known: Option<FileStat>) -> bool {
    if FileStat::read(path).is_ok_and(|now| known.is_some_and(|known| known.same_moved(&now))) {
        return true;
    }
    paths::holds(path, sha256)
}

/// Set the file at `root:rel` (`dest`) aside under `aside`. Safe to repeat:
/// a file already set aside is left there, whatever has since taken its
/// name, which is never touched. With `known` the file is checked by its
/// whole identity just before it moves (a file changed since it was read
/// refuses and stays), and again once it moved, change time aside, which
/// the rename moves; without it, by its bytes. `linked_earlier` (the file
/// is a hard link an earlier row of the removal already moved) leaves the
/// change time out of the first check too. A file that no longer matches
/// after the move goes straight back home and the removal refuses.
/// `before_move` is a test seam.
#[allow(clippy::too_many_arguments)]
pub(super) fn set_aside(
    mode: PlacementMode,
    root_dev: Option<u64>,
    (root, rel): (&str, &str),
    dest: &Path,
    aside: &Path,
    sha256: &str,
    known: Option<FileStat>,
    linked_earlier: bool,
    seams: Seams,
    before_move: Option<&dyn Fn(&Path)>,
) -> Result<(), PublishError> {
    // One file under both names is an interrupted link: the set-aside
    // below finishes it.
    if std::fs::symlink_metadata(aside).is_ok()
        && !paths::same_file(aside, dest)
        && holds(aside, sha256, known)
    {
        return Ok(());
    }
    let replaced = || {
        PublishError::Collision(format!("{root}:{rel} changed while it was being removed").into())
    };
    if let Some(known) = known {
        let unchanged = FileStat::read(dest).is_ok_and(|now| {
            if linked_earlier {
                known.same_moved(&now)
            } else {
                now == known
            }
        });
        if !unchanged {
            return Err(replaced());
        }
    }
    let original = Original {
        visible: dest,
        hidden: aside,
        home: dest,
        sha256: known.is_none().then_some(sha256),
        root_dev,
        known: None,
    };
    place::set_aside_with(mode, original, seams, before_move)
        .map_err(|error| error.into_publish(root, rel))?;
    let Some(known) = known else {
        return Ok(());
    };
    if FileStat::read(aside).is_ok_and(|now| known.same_moved(&now)) {
        return Ok(());
    }
    match place::place_with(mode, aside, dest, seams) {
        Ok(_) => Err(replaced()),
        Err(PlaceError::Unsynced(error)) => {
            tracing::warn!(%error, "directory sync failed");
            Err(replaced())
        }
        Err(error) => {
            tracing::warn!(
                kept = %aside.display(),
                %error,
                "a file changed during a removal could not be put back; it is kept"
            );
            Err(replaced())
        }
    }
}

/// How a removal's unwind of one row ended.
pub(super) enum PutBack {
    /// The file is home, or never left.
    Home,
    /// The hidden copy is kept, and why.
    Kept(&'static str),
    /// The file has no safe way back onto its name; it stays hidden.
    NoSafeWay,
}

/// Undo [`set_aside`] before a commit: the hidden copy goes home onto a
/// free name. A hidden copy that is not the removed file any more, or
/// whose name another file took, is kept where it is.
pub(super) fn put_back(
    mode: PlacementMode,
    dest: &Path,
    aside: &Path,
    sha256: &str,
    seams: Seams,
) -> Result<PutBack, PublishError> {
    match std::fs::symlink_metadata(aside) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(PutBack::Home),
        Err(error) => return Err(PublishError::from(error)),
    }
    if !paths::same_file(aside, dest) && !holds(aside, sha256, None) {
        return Ok(PutBack::Kept("its hidden copy is not the removed file"));
    }
    match place::place_with(mode, aside, dest, seams) {
        Ok(_) => Ok(PutBack::Home),
        Err(PlaceError::Unsynced(error)) => {
            tracing::warn!(%error, "directory sync failed");
            Ok(PutBack::Home)
        }
        Err(PlaceError::Collision) => Ok(PutBack::Kept("another file took its name")),
        Err(PlaceError::Unavailable(_) | PlaceError::CrossDevice) => Ok(PutBack::NoSafeWay),
        Err(error) => Err(PublishError::Io(error.to_string().into())),
    }
}

/// The catalog half of a removal, inside its bundle's commit: every
/// track's rows must still be as sealed, then each is marked missing with
/// its catalog action, no track may be listed in a folder whose sidecars
/// go because no music was left there, and the undo record is written
/// under the bundle's id when the removal moves a file.
pub(super) fn commit(
    conn: &Connection,
    bundle_id: &str,
    removal: &Removal,
    now: f64,
) -> Result<(), PublishError> {
    if let Some(moved) = removal_moved(conn, &removal.items)? {
        return Err(PublishError::Catalog(
            format!("track {moved} changed under bundle {bundle_id}").into(),
        ));
    }
    for item in &removal.items {
        let marked = mark_removed(
            conn,
            &item.track_id,
            &item.seal.album_id,
            &removal.reason_code,
            removal.actor.as_deref(),
            now,
        )?;
        if !marked {
            return Err(PublishError::Catalog(
                format!("track {} is no longer listed", item.track_id).into(),
            ));
        }
    }
    let emptied: std::collections::BTreeSet<(&str, &str)> = removal
        .sidecars
        .iter()
        .filter_map(|sidecar| Some((sidecar.root_id.as_str(), sidecar.emptied_folder.as_deref()?)))
        .collect();
    for (root_id, folder) in emptied {
        let listed: bool = conn
            .prepare_cached(sql::TRACK_LISTED_BELOW.text())?
            .query_row(rusqlite::params![root_id, folder], |row| row.get(0))?;
        if listed {
            return Err(PublishError::Catalog(
                format!("a track is listed under {root_id}:{folder} again").into(),
            ));
        }
    }
    if let Some(undo) = &removal.undo
        && moves_a_file(removal)
    {
        let track_ids: Vec<&str> = removal
            .items
            .iter()
            .map(|item| item.track_id.as_str())
            .collect();
        record_action(
            conn,
            &Action {
                id: bundle_id,
                actor: removal.actor.as_deref(),
                kind: RECYCLED_KIND,
                album_id: &undo.album_id,
                track_id: None,
                before: "{}",
                after: &serde_json::json!({
                    "track_ids": track_ids,
                    "bundle_id": bundle_id,
                })
                .to_string(),
                reason_code: &removal.reason_code,
                at: now,
            },
        )?;
    }
    Ok(())
}

/// How many tracks are listed in library folder `root_id` below `folder`.
pub fn tracks_listed_below(
    conn: &Connection,
    root_id: &str,
    folder: &str,
) -> rusqlite::Result<u64> {
    conn.prepare_cached(sql::TRACKS_LISTED_BELOW.text())?
        .query_row(rusqlite::params![root_id, folder], |row| row.get(0))
}

/// Whether a removal moves any file, a track's or a sidecar.
pub(crate) fn moves_a_file(removal: &Removal) -> bool {
    !removal.sidecars.is_empty() || removal.items.iter().any(|item| item.file.is_some())
}

/// The catalog action kind of a removal's undo record.
pub(crate) const RECYCLED_KIND: &str = "files_recycled";

/// Mark an indexed track missing and record one `remove_track` catalog
/// action for it; its rows and references stay. False when the track was
/// not indexed. The rule a removal's commit and a catalog-only removal
/// share.
pub(crate) fn mark_removed(
    conn: &Connection,
    track_id: &str,
    album_id: &str,
    reason_code: &str,
    actor: Option<&str>,
    now: f64,
) -> rusqlite::Result<bool> {
    let revision: Option<i64> = conn
        .prepare_cached(sql::TRACK_MARK_MISSING.text())?
        .query_row(rusqlite::params![track_id, crate::clock::ms(now)], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(revision) = revision else {
        return Ok(false);
    };
    let id = uuid::Uuid::new_v4().to_string();
    record_action(
        conn,
        &Action {
            id: &id,
            actor,
            kind: "remove_track",
            album_id,
            track_id: Some(track_id),
            before: &serde_json::json!({"availability": "indexed", "row_revision": revision - 1})
                .to_string(),
            after: &serde_json::json!({"availability": "missing", "row_revision": revision})
                .to_string(),
            reason_code,
            at: now,
        },
    )?;
    Ok(true)
}

pub(super) struct Action<'a> {
    pub id: &'a str,
    pub actor: Option<&'a str>,
    pub kind: &'a str,
    pub album_id: &'a str,
    pub track_id: Option<&'a str>,
    pub before: &'a str,
    pub after: &'a str,
    pub reason_code: &'a str,
    pub at: f64,
}

pub(super) fn record_action(conn: &Connection, action: &Action<'_>) -> rusqlite::Result<()> {
    conn.prepare_cached(crate::catalog_actions::sql::RECORD_REMOVAL_ACTION.text())?
        .execute(rusqlite::params![
            action.id,
            action.actor,
            action.kind,
            action.album_id,
            action.track_id,
            action.before,
            action.after,
            action.reason_code,
            crate::clock::ms(action.at),
        ])?;
    Ok(())
}
