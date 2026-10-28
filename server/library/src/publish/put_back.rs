//! Putting files a removal took to the recycle bin back, under `put_back`
//! journal rows.
//!
//! Every removal a person asked for that moved a file writes an undo
//! record (a `files_recycled` catalog action) under its bundle id; that id
//! names the removal on the album's page and here. Every path comes from
//! the removal's own rows: home is the row's `(root, rel)`, resolved
//! through the sandbox, and the copy is the row's bin entry, or its hidden
//! name beside home while its move into the bin is still pending. Each
//! copy is hashed before the publish lock is taken; under it only its
//! identity on disk is compared. The row's hidden temp beside home is a
//! hard link of the copy when both are on one filesystem, checked to be
//! that file, else a verified copy of it. The temp is placed by a call
//! that refuses a taken name. The commit re-indexes each track from its
//! file, so the row keeps its id, and writes the put-back action; until
//! then the copy's name is untouched, so an unwind, which only removes the
//! temp and what it placed, loses nothing. Only after the commit is the
//! copy deleted, and only while it and the file at home both still hold
//! the removed bytes. A copy in a bin that is no longer set is never
//! deleted, and never linked.

use std::path::{Component, Path, PathBuf};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use rusqlite::{Connection, OptionalExtension, params};

use super::journal::{FileJournal, JournalKind, JournalState, JournalStore};
use super::paths::{self, FileStat, Sandbox};
use super::place::{self, PlaceError, Seams};
use super::planner::{CopyIn, PlanBundle, PutBack, PutBackItem, RemoveFile};
use super::snapshots::sha256_hex;
use super::{HIDDEN_PREFIX, PublishError, sql};
use crate::scan::models::EffectivePolicy;
use crate::scan::roots::RootRegistry;
use crate::scan::store::CommitIndexedItem;

/// Why a put-back did not happen.
#[derive(Debug)]
pub enum Refusal {
    /// Another write holds the library folder or one of the tracks.
    Busy,
    /// No removal with that id is left to take back.
    NothingToRestore,
    /// A track or its removal's rows are not as the put-back needs them.
    Changed,
    /// A file's library folder is not one the publisher holds, or a link
    /// stands on the way home.
    OutsideRoot,
    /// A file's library folder is not there.
    FolderMissing,
    /// A file's folder is now excluded from the library.
    Excluded,
    /// No recycle bin is known while this setting cannot be read.
    BinsUnknown(&'static str),
    /// Another file sits where a removed file was, while its copy is
    /// still there to come back.
    Occupied,
    /// None of the removed files' copies are left.
    AllGone,
    /// A removed file's track is listed somewhere else now.
    TrackElsewhere,
    /// The removal's own write needs an administrator's look first.
    RemovalNeedsAttention,
    /// A copy that is there could not be read.
    CopyUnreadable,
    /// The publish refused or failed.
    Publish(PublishError),
}

impl From<PublishError> for Refusal {
    fn from(error: PublishError) -> Self {
        Self::Publish(error)
    }
}

impl From<droppedneedle_kernel::db::DbError> for Refusal {
    fn from(error: droppedneedle_kernel::db::DbError) -> Self {
        Self::Publish(PublishError::from(error))
    }
}

/// The `before_json` a put-back action records, naming the removal it
/// undid. One per removal: its presence spends the undo.
pub(crate) fn marker(removal_id: &str) -> String {
    serde_json::json!({ "undoes": removal_id }).to_string()
}

/// The catalog action kind of a put-back.
const PUT_BACK_KIND: &str = "files_put_back";

/// Whether a put-back already spent the removal `removal_id`'s undo.
pub(super) fn spent(conn: &Connection, removal_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(sql::PUT_BACK_EXISTS.text())?
        .query_row(params![removal_id], |row| row.get(0))
}

/// What kind of removal an undo record undoes, read from its reason code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemovalKind {
    /// A whole album's files.
    Album,
    /// One track's file (a track removal, or the second copy a duplicate
    /// resolution sent away).
    Track,
    /// Files the album's edition has no place for.
    Extras,
}

impl RemovalKind {
    pub(super) fn of(reason_code: &str) -> Option<Self> {
        match reason_code {
            "ALBUM_FILES_RECYCLED" => Some(Self::Album),
            "FILE_RECYCLED" => Some(Self::Track),
            "EDITION_EXTRA_RECYCLED" => Some(Self::Extras),
            _ => None,
        }
    }
}

/// One removal an album's page can put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The removal's id (its bundle id).
    pub id: String,
    pub kind: RemovalKind,
    /// When it was removed, unix milliseconds.
    pub removed_at: i64,
    /// Its files whose copies are still in the recycle bin.
    pub files: u32,
    /// A file write of it waits for an admin, so it can't be put back yet.
    pub held: bool,
}

/// The removals of album `album_id` (a live album id) that can still be
/// put back, newest first: not put back yet, every file settled or held,
/// and at least one copy the bin's expiry has not deleted. A record with a
/// reason this does not know is left out, with one warning per process.
pub fn removals(db: &Db, album_id: &str) -> Result<Vec<Listed>, PublishError> {
    static UNKNOWN_WARNED: std::sync::Once = std::sync::Once::new();
    let rows: Vec<(String, Option<String>, f64, u32, bool)> =
        db.read_blocking("publish.album_removals", |conn| {
            let mut statement = conn.prepare_cached(sql::ALBUM_REMOVALS.text())?;
            let rows = statement.query_map(params![album_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?;
            Ok::<_, PublishError>(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, reason, at, files, held)| {
            let Some(kind) = reason.as_deref().and_then(RemovalKind::of) else {
                UNKNOWN_WARNED.call_once(|| {
                    tracing::warn!(
                        reason = reason.as_deref().unwrap_or(""),
                        "a removal's undo record has a reason this version does not list"
                    );
                });
                return None;
            };
            Some(Listed {
                id,
                kind,
                removed_at: (at * 1000.0) as i64,
                files,
                held,
            })
        })
        .collect())
}

/// One removal row, with what was planned for its file and its track now.
struct Found {
    row: FileJournal,
    file: Option<RemoveFile>,
    /// The track's (root, relative path, availability) now.
    track: Option<(String, String, String)>,
}

/// The removal `removal_id` on album `album_id` (or an album merged into
/// it), when it is still there to undo: its bundle id and its rows.
fn read_removal(
    conn: &Connection,
    removal_id: &str,
    album_id: &str,
) -> Result<Result<(String, Vec<Found>), Refusal>, PublishError> {
    let after: Option<String> = conn
        .prepare_cached(sql::RECYCLED_ACTION_OPEN.text())?
        .query_row(params![removal_id, album_id], |row| row.get(0))
        .optional()?;
    let Some(after) = after else {
        return Ok(Err(Refusal::NothingToRestore));
    };
    let bundle_id = serde_json::from_str::<serde_json::Value>(&after)
        .ok()
        .and_then(|value| value.get("bundle_id")?.as_str().map(str::to_owned));
    if bundle_id.as_deref() != Some(removal_id) {
        tracing::warn!(
            removal_id,
            "a removal's undo record names no bundle of its own; nothing is put back"
        );
        return Ok(Err(Refusal::NothingToRestore));
    }
    let bundle_id = removal_id.to_owned();
    let removal = super::operations::operation_json(conn, &bundle_id)?
        .and_then(|json| serde_json::from_str::<PlanBundle>(&json).ok())
        .and_then(|bundle| bundle.removal);
    let Some(removal) = removal else {
        tracing::warn!(
            bundle_id,
            "a removal's recorded operation does not read; nothing is put back"
        );
        return Ok(Err(Refusal::Changed));
    };
    let mut found = Vec::new();
    for row in JournalStore::new(conn).bundle(&bundle_id)? {
        if row.kind != JournalKind::Remove {
            continue;
        }
        let track = match row.replaces.as_ref().and_then(|r| r.track_id.as_deref()) {
            Some(track_id) => conn
                .prepare_cached(sql::TRACK_PLACE.text())?
                .query_row(params![track_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?,
            None => None,
        };
        let file = super::remove::file_of(&removal, &row).cloned();
        found.push(Found { row, file, track });
    }
    Ok(Ok((bundle_id, found)))
}

/// Copies hashed before the publish lock: each path that held the removed
/// bytes, with its identity on disk while it was read.
pub type Prehashed = std::collections::BTreeMap<PathBuf, FileStat>;

/// What a put-back reads before the publish lock: the library folders the
/// removal `removal_id` took files from, so the caller can hold them, and
/// each copy that holds the removed bytes (its bin entry, or its hidden
/// name while its move into the bin waits), hashed once. Empty when there
/// is nothing to take back; the plan then says why. A copy that can't be
/// read here is left for the plan to read under the lock.
pub fn prepare(
    db: &Db,
    removal_id: &str,
    album_id: &str,
) -> Result<(Vec<String>, Prehashed), PublishError> {
    let read = db.read_blocking("publish.put_back_roots", |conn| {
        read_removal(conn, removal_id, album_id)
    })?;
    let Ok((_, found)) = read else {
        return Ok((Vec::new(), Prehashed::new()));
    };
    let mut roots = Vec::new();
    let mut hashed = Prehashed::new();
    for found in &found {
        roots.push(found.row.dest_root.clone());
        let Some(file) = &found.file else {
            continue;
        };
        let mut candidates = vec![PathBuf::from(&found.row.staged)];
        if let Some(replaced) = &found.row.replaces {
            candidates.push(PathBuf::from(&replaced.recycle_to));
        }
        for path in candidates {
            if let Some(stat) = hash_copy(&path, file) {
                hashed.insert(path, stat);
            }
        }
    }
    roots.sort();
    roots.dedup();
    Ok((roots, hashed))
}

/// The identity of the regular file at `path` when it holds `file`'s
/// bytes and did not change while it was read.
fn hash_copy(path: &Path, file: &RemoveFile) -> Option<FileStat> {
    let before = FileStat::read(path).ok()?;
    if before.size != file.fingerprint.size {
        return None;
    }
    let hashed = paths::hash_regular_file(path).ok()?;
    (hashed.stat == before && hashed.sha256 == file.fingerprint.sha256).then_some(hashed.stat)
}

/// Plan putting back what the removal `removal_id` took from album
/// `album_id`, under the publish lock, against `sandbox`. Only folders in
/// `held` may be written. A copy in `prehashed` whose identity is
/// unchanged is not read again. Per file: a copy that is gone, or holds
/// other bytes, is named missing and the rest still come back; a file
/// already at home with the removed bytes is left as it is. Another file
/// at home while the copy is still there refuses the whole put-back, and
/// so does a track that is not missing at its old path (or the hidden
/// name it gave that path up for).
#[allow(clippy::too_many_arguments)]
pub fn plan(
    db: &Db,
    sandbox: &Sandbox,
    registry: &RootRegistry,
    bundle_id: &str,
    removal_id: &str,
    album_id: &str,
    actor: Option<&str>,
    held: &[String],
    prehashed: &Prehashed,
) -> Result<PutBack, Refusal> {
    let read = db.read_blocking("publish.plan_put_back", |conn| {
        let read = read_removal(conn, removal_id, album_id)?;
        if let Ok((_, found)) = &read {
            let journals = JournalStore::new(conn);
            for found in found {
                let track = found
                    .row
                    .replaces
                    .as_ref()
                    .and_then(|r| r.track_id.as_deref());
                if let Some(track) = track
                    && journals
                        .unsettled_bundle_for_track(track, bundle_id)?
                        .is_some()
                {
                    return Ok(Err(Refusal::Busy));
                }
            }
        }
        Ok::<_, PublishError>(read)
    })?;
    let (removal_bundle, found) = read?;
    let mut items = Vec::new();
    let mut missing = Vec::new();
    for found in found {
        let row = &found.row;
        let rel = row.dest_rel.as_str();
        let (Some(track_id), Some(file)) = (
            row.replaces.as_ref().and_then(|r| r.track_id.clone()),
            found.file.as_ref(),
        ) else {
            tracing::warn!(journal_id = %row.id, "a removal row has no planned file; nothing is put back");
            return Err(Refusal::Changed);
        };
        if !held.contains(&row.dest_root) {
            return Err(Refusal::Changed);
        }
        let entry = || -> Result<(PathBuf, CopyIn), Refusal> {
            let entry = row
                .replaces
                .as_ref()
                .map(|replaced| PathBuf::from(&replaced.recycle_to))
                .ok_or(Refusal::Changed)?;
            if let Some(cause) = sandbox.bins_unknown() {
                return Err(Refusal::BinsUnknown(cause));
            }
            let copy_in = if sandbox.bin_holding(&entry).is_some() {
                CopyIn::Bin
            } else {
                CopyIn::OldBin
            };
            Ok((entry, copy_in))
        };
        let (copy, copy_in) = match row.state {
            JournalState::Cleaned => entry()?,
            // The move into the bin is pending: the file waits under the
            // row's hidden name, unless the move got as far as the bin and
            // stopped before it was recorded.
            JournalState::Committed | JournalState::CleanupPending => {
                if std::fs::symlink_metadata(&row.staged).is_ok() {
                    (PathBuf::from(&row.staged), CopyIn::Aside)
                } else {
                    entry()?
                }
            }
            JournalState::NeedsAttention => return Err(Refusal::RemovalNeedsAttention),
            // Recovery has not finished the removal yet.
            JournalState::Prepared | JournalState::Staged | JournalState::Published => {
                return Err(Refusal::Busy);
            }
            JournalState::Compensated => {
                tracing::warn!(journal_id = %row.id, "a removal row was unwound; nothing is put back");
                return Err(Refusal::Changed);
            }
        };
        let home = sandbox
            .resolve_removal(&row.dest_root, rel)
            .map_err(|error| match error {
                PublishError::FolderMissing => Refusal::FolderMissing,
                other => {
                    tracing::warn!(%other, "a removed file's way home is refused");
                    Refusal::OutsideRoot
                }
            })?;
        let copy_stat = check_copy(sandbox, &copy, copy_in, file, prehashed.get(&copy))?;
        let gone = format!("{HIDDEN_PREFIX}gone/{track_id}/{rel}");
        let track_rel = match &found.track {
            Some((root, track_rel, availability))
                if *root == row.dest_root
                    && (track_rel == rel || *track_rel == gone)
                    && ready(availability, track_rel, rel) =>
            {
                Some(track_rel.clone())
            }
            _ => None,
        };
        let mut home_already = false;
        if std::fs::symlink_metadata(&home).is_ok() {
            let same_size =
                FileStat::read(&home).is_ok_and(|stat| stat.size == file.fingerprint.size);
            if !(same_size && super::remove::holds(&home, &file.fingerprint.sha256, None)) {
                if copy_stat.is_some() {
                    tracing::info!(file = rel, "another file sits where a removed file was");
                    return Err(Refusal::Occupied);
                }
                missing.push(rel.to_owned());
                continue;
            }
            // Someone put it back by hand: its track is listed again with
            // the rest, when it is still the one at this path.
            tracing::info!(file = rel, "a removed file is already back");
            if track_rel.is_none() {
                continue;
            }
            home_already = true;
        }
        let copy_stat = match (home_already, copy_stat) {
            (_, Some(stat)) => stat,
            (true, None) => FileStat::read(&home).map_err(PublishError::from)?,
            (false, None) => {
                missing.push(rel.to_owned());
                continue;
            }
        };
        let Some(track_rel) = track_rel else {
            tracing::info!(track_id, "a removed file's track is listed elsewhere now");
            return Err(Refusal::TrackElsewhere);
        };
        let root = registry
            .resolve(&row.dest_root)
            .ok_or(Refusal::OutsideRoot)?;
        let policy = root.policy_for(&home);
        if policy == EffectivePolicy::Excluded {
            return Err(Refusal::Excluded);
        }
        items.push(PutBackItem {
            track_id,
            root_id: row.dest_root.clone(),
            relative_path: rel.to_owned(),
            track_rel,
            copy: copy.to_string_lossy().into_owned(),
            copy_in,
            fingerprint: file.fingerprint.clone(),
            copy_stat,
            policy,
            policy_revision: registry.policy_revision().to_owned(),
            home_already,
            linkable: copy_in != CopyIn::OldBin
                && home_device(&home).is_some_and(|dev| dev == copy_stat.dev),
            removal_row: row.id.clone(),
        });
    }
    if items.is_empty() {
        return Err(if missing.is_empty() {
            Refusal::NothingToRestore
        } else {
            Refusal::AllGone
        });
    }
    Ok(PutBack {
        removal_bundle,
        album_id: album_id.to_owned(),
        actor: actor.map(str::to_owned),
        items,
        missing,
    })
}

/// The device of home's folder, or of its nearest folder that is there.
fn home_device(home: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    home.ancestors()
        .skip(1)
        .find_map(|folder| std::fs::metadata(folder).ok())
        .map(|meta| meta.dev())
}

/// Whether a track row at `track_rel` may take its file back at `rel`:
/// missing there or at the hidden name it gave the path up for, or still
/// listed at the path while nothing is there (a scan continued it onto a
/// file that has gone since; the placement refuses if one turns up).
pub(super) fn ready(availability: &str, track_rel: &str, rel: &str) -> bool {
    availability == "missing" || (availability == "indexed" && track_rel == rel)
}

/// Where a copy may be read from: under its bin (no link below the bin's
/// folder), beside home under its root, or, for a bin no longer set, an
/// entry folder of ours that is no link. A path that fails counts as
/// gone.
fn copy_place(sandbox: &Sandbox, copy: &Path, copy_in: CopyIn) -> Result<(), PublishError> {
    match copy_in {
        CopyIn::Bin => sandbox.ensure_under_bin(copy).map(|_| ()),
        CopyIn::Aside => sandbox.ensure_under_removal_roots(copy),
        CopyIn::OldBin => old_entry(copy),
    }
}

/// An entry of a bin no longer set: a plain absolute path whose folder is
/// one of ours (`<stamp>-<hex>`) and no link. What lies above that folder
/// was the admin's bin and may go through links. The file's own bytes are
/// the real check.
fn old_entry(copy: &Path) -> Result<(), PublishError> {
    let refused = || PublishError::UnsafePath("not a recycle bin entry".into());
    if !copy.is_absolute()
        || copy
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(refused());
    }
    let entry = copy.parent().ok_or_else(refused)?;
    let ours = entry
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(super::bin::entry_name);
    if !ours {
        return Err(refused());
    }
    match std::fs::symlink_metadata(entry) {
        Ok(meta) if meta.file_type().is_dir() => Ok(()),
        _ => Err(refused()),
    }
}

/// The copy's identity when it may be read from where it is and holds
/// the removed bytes, checked by its size and then its bytes, or by its
/// whole identity when it is still the file `hashed` saw holding them.
/// `None` when it is gone, is not a plain file, or holds other bytes; a
/// copy that is there but cannot be read refuses, so the undo is not
/// spent on it.
fn check_copy(
    sandbox: &Sandbox,
    copy: &Path,
    copy_in: CopyIn,
    file: &RemoveFile,
    hashed: Option<&FileStat>,
) -> Result<Option<FileStat>, Refusal> {
    if let Err(error) = copy_place(sandbox, copy, copy_in) {
        tracing::info!(%error, "a removed file's copy can't be read safely; it counts as gone");
        return Ok(None);
    }
    let unreadable = |error: &dyn std::fmt::Display| {
        tracing::warn!(%error, "a removed file's copy can't be read");
        Refusal::CopyUnreadable
    };
    let meta = match std::fs::symlink_metadata(copy) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(unreadable(&error)),
    };
    if !meta.file_type().is_file() || meta.len() != file.fingerprint.size {
        return Ok(None);
    }
    let stat = FileStat::of(&meta);
    if hashed == Some(&stat) {
        return Ok(Some(stat));
    }
    match paths::hash_regular_file(copy) {
        Ok(hashed) => Ok((hashed.sha256 == file.fingerprint.sha256).then_some(stat)),
        Err(PublishError::Io(error)) => Err(unreadable(&error)),
        Err(_) => Ok(None),
    }
}

/// The journal id of the put-back's `index`th item.
pub(super) fn journal_id(bundle_id: &str, index: usize) -> String {
    format!("{bundle_id}-putback-{index:03}")
}

/// The ids of every row a put-back's prepare writes.
pub(super) fn journal_ids(bundle_id: &str, put_back: &PutBack) -> Vec<String> {
    put_back
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| !item.home_already)
        .map(|(index, _)| journal_id(bundle_id, index))
        .collect()
}

/// The planned item a `put_back` row places.
pub(super) fn item_of<'a>(put_back: &'a PutBack, journal: &FileJournal) -> Option<&'a PutBackItem> {
    let track = journal.track_id.as_deref()?;
    put_back.items.iter().find(|item| item.track_id == track)
}

/// Copy the item's copy, checked where it is, into `temp` with permission
/// bits `bits`, in one read that checks it holds the removed bytes. A
/// copy that does not is refused, and its temp goes.
pub(super) fn copy_into(
    sandbox: &Sandbox,
    item: &PutBackItem,
    temp: &Path,
    bits: u32,
) -> Result<(), PublishError> {
    let copy = Path::new(&item.copy);
    copy_place(sandbox, copy, item.copy_in)?;
    let read = paths::copy_hashed(copy, temp, Some(bits))?;
    if read.size != item.fingerprint.size || read.sha256 != item.fingerprint.sha256 {
        if let Err(error) = std::fs::remove_file(temp) {
            tracing::warn!(%error, "a put-back temp with other bytes was not removed");
        }
        return Err(PublishError::Validation(
            format!("the removed copy of {} changed", item.relative_path).into(),
        ));
    }
    if let Some(parent) = temp.parent() {
        super::journal::fsync_dir(parent)?;
    }
    Ok(())
}

/// Make `temp` a hard link of the item's copy, checked to be that file as
/// the plan found it. The copy's name and bytes are left alone. False,
/// with nothing left behind, when it can't be linked (another
/// filesystem, a bin no longer set, a copy whose identity moved since the
/// plan, or a link that does not read back as the copy): the caller then
/// copies and verifies its bytes.
pub(super) fn link_temp(
    sandbox: &Sandbox,
    item: &PutBackItem,
    temp: &Path,
    seams: Seams,
) -> Result<bool, PublishError> {
    if item.copy_in == CopyIn::OldBin || seams.cross_device || seams.copy_put_back {
        return Ok(false);
    }
    let copy = Path::new(&item.copy);
    copy_place(sandbox, copy, item.copy_in)?;
    if FileStat::read(copy).ok() != Some(item.copy_stat) {
        return Ok(false);
    }
    if let Err(error) = std::fs::hard_link(copy, temp) {
        tracing::debug!(%error, "a removed file is copied back: its copy can't be linked");
        return Ok(false);
    }
    if FileStat::read(temp).is_ok_and(|now| item.copy_stat.same_moved(&now)) {
        return Ok(true);
    }
    // A filesystem that reports another identity for a hard link: the
    // link goes and the file is copied and verified instead.
    if let Err(error) = std::fs::remove_file(temp) {
        tracing::warn!(%error, "a put-back's hidden link was kept");
    }
    Ok(false)
}

/// Place a `put_back` row's temp on its home, by the call that refuses a
/// taken name. Safe to repeat: a temp already placed is done. `known` is
/// the temp's identity when it was written, when the caller has it; else
/// its bytes are read. `before_place` is a test seam.
pub(super) fn place(
    sandbox: &Sandbox,
    journal: &FileJournal,
    known: Option<FileStat>,
    seams: Seams,
    before_place: Option<&dyn Fn(&Path)>,
) -> Result<(), PublishError> {
    let (root, rel) = (journal.dest_root.as_str(), journal.dest_rel.as_str());
    let home = sandbox.resolve_removal(root, rel)?;
    let temp = Path::new(&journal.staged);
    sandbox.ensure_under_removal_roots(temp)?;
    let sha = journal.staged_sha256.as_str();
    match std::fs::symlink_metadata(temp) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if super::remove::holds(&home, sha, known) {
                Ok(())
            } else {
                Err(PublishError::Validation(
                    format!("the copy for {root}:{rel} is gone").into(),
                ))
            };
        }
        Err(error) => return Err(PublishError::from(error)),
    }
    if !paths::same_file(temp, &home) {
        if !super::remove::holds(temp, sha, known) {
            return Err(PublishError::Validation(
                format!("the copy for {root}:{rel} changed").into(),
            ));
        }
        if std::fs::symlink_metadata(&home).is_ok() {
            return Err(PublishError::Collision(
                format!("late occupier at {root}:{rel}").into(),
            ));
        }
        if let Some(hook) = before_place {
            hook(&home);
        }
    }
    match place::place_with(sandbox.placement_mode(root), temp, &home, seams) {
        Ok(_) => Ok(()),
        Err(PlaceError::Unsynced(error)) => {
            tracing::warn!(%error, "directory sync failed");
            Ok(())
        }
        Err(error) => Err(error.into_publish(root, rel)),
    }
}

/// After the commit: delete the copy a `put_back` row came from, when it
/// is in a bin that is still set (or is the removal's hidden name), it
/// still holds the removed bytes, and the file at home holds them too.
/// Anything else keeps the copy and logs why once; the row is done all
/// the same. Only a failed delete of a matching copy is an error, which
/// leaves the row pending. `home_known` is the placed file's identity,
/// when the caller has it. Once the copy is gone, the removal's row is
/// marked, at `now_ms`, as having left the bin, so the bin's page no
/// longer lists it; a copy found already gone is marked only where its
/// bin shows it is there to look in. A lost mark only waits for the
/// bin's expiry.
pub(super) fn drop_copy(
    db: &Db,
    sandbox: &Sandbox,
    journal: &FileJournal,
    item: Option<&PutBackItem>,
    home_known: Option<FileStat>,
    now_ms: i64,
) -> Result<(), PublishError> {
    let keep = |why: &str| {
        tracing::info!(journal_id = %journal.id, why, "a put-back keeps the removed file's copy");
        Ok(())
    };
    let Some(item) = item else {
        return keep("its plan does not read");
    };
    if item.copy_in == CopyIn::OldBin {
        return keep("its recycle bin is no longer set");
    }
    let copy = Path::new(&item.copy);
    if matches!(
        std::fs::symlink_metadata(copy),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ) {
        if item.copy_in == CopyIn::Bin
            && let Ok(bin) = sandbox.ensure_under_bin(copy)
            && super::expiry::gone_for_sure(&bin.dir, copy.parent())
        {
            mark_left(db, item, now_ms);
        }
        return Ok(());
    }
    let Ok(home) = sandbox.resolve_removal(&journal.dest_root, &journal.dest_rel) else {
        return keep("its home can't be checked");
    };
    let sha = item.fingerprint.sha256.as_str();
    if !super::remove::holds(&home, sha, home_known) {
        return keep("the file at home is not the one put back");
    }
    if copy_place(sandbox, copy, item.copy_in).is_err() {
        return keep("its recycle bin changed");
    }
    if !super::remove::holds(copy, sha, Some(item.copy_stat)) {
        return keep("the copy changed");
    }
    std::fs::remove_file(copy).map_err(PublishError::from)?;
    if let Some(entry) = copy.parent() {
        if item.copy_in == CopyIn::Bin {
            super::expiry::drop_entry(copy);
        }
        let synced = if item.copy_in == CopyIn::Bin {
            entry.parent()
        } else {
            Some(entry)
        };
        if let Some(dir) = synced
            && let Err(error) = super::journal::fsync_dir(dir)
        {
            tracing::warn!(%error, "directory sync failed");
        }
    }
    mark_left(db, item, now_ms);
    Ok(())
}

/// Mark the removal row of `item` as having left the bin.
fn mark_left(db: &Db, item: &PutBackItem, now_ms: i64) {
    if item.removal_row.is_empty() {
        return;
    }
    if let Err(error) = super::expiry::mark(db, &[item.removal_row.as_str()], now_ms) {
        tracing::warn!(journal_id = %item.removal_row, %error, "a put-back copy's row was not marked; the bin's expiry will");
    }
}

/// A put-back's catalog half: the plan and what each placed file indexes
/// as, read before the commit's transaction.
#[derive(Debug, Clone)]
pub struct PutBackCommit {
    pub put_back: PutBack,
    pub indexed: Vec<CommitIndexedItem>,
}

/// Read each placed file of a put-back as a scan would, before the
/// commit's transaction. `home` resolves an item's place.
pub(super) fn read_placed(
    sandbox: &Sandbox,
    put_back: &PutBack,
    clock: &dyn Clock,
) -> Result<PutBackCommit, PublishError> {
    let mut indexed = Vec::with_capacity(put_back.items.len());
    for item in &put_back.items {
        let home = sandbox.resolve_removal(&item.root_id, &item.relative_path)?;
        indexed.push(super::publisher::index_file(
            &item.root_id,
            &item.relative_path,
            &home,
            item.policy,
            &item.policy_revision,
            clock,
        )?);
    }
    Ok(PutBackCommit {
        put_back: put_back.clone(),
        indexed,
    })
}

/// The catalog half of a put-back, inside its bundle's commit: the undo
/// must not be spent yet and every track must still be missing where the
/// plan found it. A track that gave its path up gets it back, after any
/// other row that is missing there gives it up in turn (one still listed
/// refuses). Each track is then indexed from its file, keeping its id, and
/// the restored action names the files that did not come back.
pub(super) fn commit(
    conn: &Connection,
    bundle_id: &str,
    commit: &PutBackCommit,
    now: f64,
) -> Result<(), PublishError> {
    let put_back = &commit.put_back;
    if spent(conn, &put_back.removal_bundle)? {
        return Err(PublishError::Catalog(
            format!("removal {} was already put back", put_back.removal_bundle).into(),
        ));
    }
    for (item, indexed) in put_back.items.iter().zip(&commit.indexed) {
        let changed = || {
            PublishError::Catalog(
                format!("track {} changed under bundle {bundle_id}", item.track_id).into(),
            )
        };
        let place: Option<(String, String, String)> = conn
            .prepare_cached(sql::TRACK_PLACE.text())?
            .query_row(params![item.track_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        let Some((root, rel, availability)) = place else {
            return Err(changed());
        };
        if root != item.root_id
            || rel != item.track_rel
            || !ready(&availability, &rel, &item.relative_path)
        {
            return Err(changed());
        }
        if item.track_rel != item.relative_path {
            let occupant: Option<(String, String)> = conn
                .prepare_cached(sql::TRACK_AT.text())?
                .query_row(params![item.root_id, item.relative_path], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional()?;
            if let Some((occupant, availability)) = occupant {
                if availability == "indexed" {
                    return Err(PublishError::Catalog(
                        format!(
                            "track {occupant} is still listed where track {} goes back",
                            item.track_id
                        )
                        .into(),
                    ));
                }
                super::publisher::give_up(
                    conn,
                    &occupant,
                    &item.relative_path,
                    &indexed.absolute_path,
                )?;
            }
            conn.prepare_cached(sql::TRACK_SET_PATH.text())?
                .execute(params![
                    item.track_id,
                    item.relative_path,
                    indexed.absolute_path,
                    sha256_hex(item.relative_path.as_bytes()),
                ])?;
        }
        let adopted = crate::scan::sqlite_store::adopt_file(
            conn,
            indexed,
            &crate::scan::sqlite_store::Adopted {
                track_id: &item.track_id,
                album_id: None,
            },
        )?;
        if adopted != item.track_id {
            return Err(PublishError::Catalog(
                format!(
                    "the file put back took track {adopted}, not {}",
                    item.track_id
                )
                .into(),
            ));
        }
    }
    let track_ids: Vec<&str> = put_back
        .items
        .iter()
        .map(|item| item.track_id.as_str())
        .collect();
    super::remove::record_action(
        conn,
        &super::remove::Action {
            id: &uuid::Uuid::new_v4().to_string(),
            actor: put_back.actor.as_deref(),
            kind: PUT_BACK_KIND,
            album_id: &put_back.album_id,
            track_id: None,
            before: &marker(&put_back.removal_bundle),
            after: &serde_json::json!({
                "track_ids": track_ids,
                "missing": put_back.missing,
                "bundle_id": bundle_id,
            })
            .to_string(),
            reason_code: "FILES_PUT_BACK",
            at: now,
        },
    )?;
    Ok(())
}

/// What a `put_back` row's file shows on disk before the commit.
pub(super) enum Evidence {
    /// The temp holds the copy and home is free, or the temp is placed.
    Resume,
    /// Never written, never placed, or something else took home: the
    /// bundle unwinds, which touches nothing that is not its own.
    Unwind,
    /// A hidden temp that is not the copy.
    Ambiguous(String),
}

/// `known` is the temp's identity as this publish wrote it, when it did.
pub(super) fn evidence(
    sandbox: &Sandbox,
    journal: &FileJournal,
    known: Option<FileStat>,
) -> Evidence {
    let Ok(home) = sandbox.resolve_removal(&journal.dest_root, &journal.dest_rel) else {
        return Evidence::Ambiguous(format!("journal {} destination unsafe", journal.id));
    };
    let temp = Path::new(&journal.staged);
    if sandbox.ensure_under_removal_roots(temp).is_err() {
        return Evidence::Ambiguous(format!(
            "journal {} staged path escapes sandbox",
            journal.id
        ));
    }
    let holds = |path: &Path| super::remove::holds(path, &journal.staged_sha256, known);
    let temp_present = std::fs::symlink_metadata(temp).is_ok();
    if temp_present && !paths::same_file(temp, &home) && !holds(temp) {
        // A temp still being written belongs to the row; the unwind
        // removes it.
        if journal.state == JournalState::Prepared {
            return Evidence::Unwind;
        }
        return Evidence::Ambiguous(format!("journal {} staged bytes changed", journal.id));
    }
    if !temp_present || paths::same_file(temp, &home) {
        return if journal.state != JournalState::Prepared && holds(&home) {
            Evidence::Resume
        } else {
            Evidence::Unwind
        };
    }
    if std::fs::symlink_metadata(&home).is_ok() {
        Evidence::Unwind
    } else {
        Evidence::Resume
    }
}
