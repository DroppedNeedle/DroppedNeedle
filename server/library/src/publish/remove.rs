//! Taking library files to the recycle bin under `remove` journal rows.
//!
//! A removal reads each file once, before the publish lock is taken: its
//! bytes for the journal, and its identity on disk (device, inode, size,
//! modified time and change time). Under the lock the plan checks the
//! whole identity again and seals each track's rows as they are.
//! Publishing checks the whole identity once more just before each file
//! moves, so a file saved since it was read stays where it is and the
//! removal unwinds. Once the file is under its row's hidden name it is
//! checked again without the change time, which the rename moves: a file
//! swapped in that moment goes straight back. The commit marks
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
    FileFingerprint, Removal, RemoveFile, RemoveItem, UndoRecord, removal_moved, removal_seals,
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
    /// A track's rows or its file changed since the caller read them.
    Changed,
    /// A track's library folder is not one the publisher holds.
    OutsideRoot,
    /// No recycle bin is known while this setting cannot be read.
    BinsUnknown(&'static str),
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
/// while it was read refuses.
pub fn probe(root: &Path, path: &Path) -> Result<Probe, PublishError> {
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

/// Plan a removal under the publish lock, against `sandbox`. First, in one
/// read: no other write may still hold any of the tracks (whatever the
/// file is doing), and each track's rows are sealed as they are now. Then
/// each file found before the lock must still be the same file at the
/// same place, and gets a fresh entry in the bin that serves its folder.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    db: &Db,
    sandbox: &Sandbox,
    bundle_id: &str,
    targets: &[Target],
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
            } => Some(plan_file(sandbox, target, path, fingerprint, *stat, now)?),
        };
        items.push(RemoveItem {
            track_id: target.track_id.clone(),
            seal: seal.clone(),
            file,
        });
    }
    Ok(Removal {
        reason_code: reason_code.to_owned(),
        actor: actor.map(str::to_owned),
        items,
        undo,
        album_removed: None,
    })
}

fn plan_file(
    sandbox: &Sandbox,
    target: &Target,
    path: &Path,
    fingerprint: &FileFingerprint,
    stat: FileStat,
    now: SystemTime,
) -> Result<RemoveFile, Refusal> {
    sandbox
        .removal_root_dir(&target.root_id)
        .map_err(|_| Refusal::OutsideRoot)?;
    let resolved = sandbox
        .resolve_removal(&target.root_id, &target.relative_path)
        .map_err(|_| Refusal::Changed)?;
    if resolved != path || FileStat::read(&resolved).ok() != Some(stat) {
        return Err(Refusal::Changed);
    }
    if target
        .expect_sha256
        .as_deref()
        .is_some_and(|expected| expected != fingerprint.sha256)
    {
        return Err(Refusal::Changed);
    }
    if let Some(cause) = sandbox.bins_unknown() {
        return Err(Refusal::BinsUnknown(cause));
    }
    let bin = sandbox
        .bin_for(&target.root_id)
        .map_err(|_| Refusal::NoBin(target.root_id.clone()))?;
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

/// The ids of every row a removal's prepare writes: one per file.
pub(super) fn journal_ids(bundle_id: &str, removal: &Removal) -> Vec<String> {
    removal
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.file.is_some())
        .map(|(index, _)| journal_id(bundle_id, index))
        .collect()
}

/// The planned file a `remove` row moves.
pub(crate) fn file_of<'a>(removal: &'a Removal, journal: &FileJournal) -> Option<&'a RemoveFile> {
    let track = journal.replaces.as_ref()?.track_id.as_deref()?;
    removal
        .items
        .iter()
        .find(|item| item.track_id == track)?
        .file
        .as_ref()
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
/// the rename moves; without it, by its bytes. A file that no longer
/// matches after the move goes straight back home and the removal
/// refuses. `before_move` is a test seam.
#[allow(clippy::too_many_arguments)]
pub(super) fn set_aside(
    mode: PlacementMode,
    root_dev: Option<u64>,
    (root, rel): (&str, &str),
    dest: &Path,
    aside: &Path,
    sha256: &str,
    known: Option<FileStat>,
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
    if known.is_some_and(|known| FileStat::read(dest).ok() != Some(known)) {
        return Err(replaced());
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
/// its catalog action, and the undo record is written under the bundle's
/// id when the removal moves a file.
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
    if let Some(undo) = &removal.undo
        && removal.items.iter().any(|item| item.file.is_some())
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
        .query_row(rusqlite::params![track_id, now], |row| row.get(0))
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
    conn.prepare_cached(sql::CATALOG_ACTION_INSERT.text())?
        .execute(rusqlite::params![
            action.id,
            action.actor,
            action.kind,
            action.album_id,
            action.track_id,
            action.before,
            action.after,
            action.reason_code,
            action.at,
        ])?;
    Ok(())
}
