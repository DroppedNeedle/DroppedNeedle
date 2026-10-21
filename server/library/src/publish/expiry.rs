//! Recycle-bin expiry: a recycled copy is deleted once its retention is
//! up, and only the copy a journal row names, as it was binned.
//!
//! A pass pages through every row whose copy reached the bin before the
//! retention, whose own write is done and whose track no put-back holds.
//! Each copy is checked outside the publish lock: it must be the
//! `<bin>/<entry>/<name>` its row names, under a bin set now, with no link
//! at or below the bin's folder, a regular file holding the bytes that
//! were recycled. Then, under the lock, so no put-back or removal is
//! reading a copy, each one is checked again (its file unchanged since it
//! was hashed) and deleted with its emptied entry folder, and its row is
//! marked. A copy whose bytes changed, in a bin no longer set, or behind a
//! link is kept and logged once. A copy already gone is marked, but only
//! where the bin shows it is there to look in (its entry folder, or
//! another entry, still in it): an empty folder may be a share that is
//! not mounted.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use droppedneedle_kernel::db::{CancelFlag, Db, Lane};
use rusqlite::params;
use sha2::{Digest as _, Sha256};

use super::paths::{FileStat, Sandbox, incoming_temp};
use super::{PublishError, sql};

/// What expiry remembers between passes: what it last logged about each
/// thing, so a warning repeats only when it changes, and the copies found
/// changed, so an unchanged one is not read again every pass.
#[derive(Default)]
pub struct ExpiryMemory {
    logged: Mutex<HashMap<String, String>>,
    changed: Mutex<HashMap<String, FileStat>>,
}

impl ExpiryMemory {
    /// Log `text` about `key` once, until it changes; `None` clears it.
    pub fn note(&self, key: &str, text: Option<String>) {
        let mut logged = self.logged.lock().unwrap_or_else(PoisonError::into_inner);
        match text {
            Some(text) => {
                if logged.get(key) != Some(&text) {
                    tracing::warn!("{text}");
                    logged.insert(key.to_owned(), text);
                }
            }
            None => {
                logged.remove(key);
            }
        }
    }

    fn known_changed(&self, journal_id: &str, now: &FileStat) -> bool {
        self.changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(journal_id)
            == Some(now)
    }

    fn remember_changed(&self, journal_id: &str, stat: FileStat) {
        self.changed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(journal_id.to_owned(), stat);
    }
}

/// What one pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExpiryReport {
    /// Copies deleted.
    pub deleted: usize,
    /// Copies found already gone, their rows marked.
    pub gone: usize,
    /// Copies kept because their bytes changed in the bin.
    pub changed: usize,
    /// Copies kept because they are not where a bin set now holds them,
    /// sit behind a link, are no plain file, or could not be read.
    pub kept: usize,
    /// Copies left for a later pass: their bin may not be mounted, or
    /// they changed while the pass ran.
    pub waited: usize,
    /// Old entry folders no row names, emptied and removed.
    pub legacy_deleted: usize,
}

/// One pass's settings.
#[derive(Debug, Clone, Copy)]
pub struct ExpiryPass {
    /// Days a copy stays in the bin; below zero counts as zero.
    pub retention_days: i64,
    /// Now, in unix milliseconds.
    pub now_ms: i64,
    /// Rows read at a time.
    pub page: usize,
}

impl ExpiryPass {
    /// Copies that reached the bin before this, in unix seconds, are due.
    fn cutoff_secs(&self) -> f64 {
        self.now_ms as f64 / 1000.0 - self.retention_days.max(0) as f64 * 86_400.0
    }
}

struct DueRow {
    journal_id: String,
    recycle_to: PathBuf,
    sha256: String,
}

/// What the check outside the lock found a copy to be.
enum Found {
    /// It holds the recycled bytes, with this identity when it was read.
    Due(FileStat),
    /// It is gone, and the bin shows it.
    Gone,
}

/// Run one expiry pass. `bins` builds the sandbox of the library folders
/// and the bins that may be pruned now (`None`: the pass waits); it is
/// built again under `hold`, which the caller makes the publish lock. A
/// row whose track a put-back holds is skipped, before and under the
/// lock. `go_on` is asked between rows; false ends the pass with nothing
/// deleted.
pub fn expire<G>(
    db: &Db,
    pass: &ExpiryPass,
    memory: &ExpiryMemory,
    bins: &dyn Fn() -> Option<Sandbox>,
    hold: impl FnOnce() -> G,
    go_on: &dyn Fn() -> bool,
) -> Result<ExpiryReport, PublishError> {
    let mut report = ExpiryReport::default();
    let Some(sandbox) = bins() else {
        return Ok(report);
    };
    let cutoff = pass.cutoff_secs();
    let mut checked: Vec<(DueRow, Found)> = Vec::new();
    for row in due_rows(db, cutoff, pass.page.max(1))? {
        if !go_on() {
            return Ok(report);
        }
        if let Some(found) = examine(&sandbox, &row, memory, &mut report) {
            checked.push((row, found));
        }
    }
    let _held = hold();
    let Some(sandbox) = bins() else {
        return Ok(report);
    };
    let mut expired = Vec::new();
    for (row, found) in checked {
        if held_by_put_back(db, &row.journal_id)? {
            continue;
        }
        let Ok(bin) = sandbox.ensure_under_bin(&row.recycle_to) else {
            report.waited += 1;
            continue;
        };
        let entry = entry_of(&row.recycle_to);
        match found {
            Found::Due(stat) => {
                if FileStat::read(&row.recycle_to).ok() != Some(stat) {
                    report.waited += 1;
                    continue;
                }
                std::fs::remove_file(&row.recycle_to)?;
                report.deleted += 1;
            }
            Found::Gone => {
                let missing = matches!(
                    std::fs::symlink_metadata(&row.recycle_to),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                );
                if !missing || !gone_for_sure(&bin.dir, entry) {
                    report.waited += 1;
                    continue;
                }
                report.gone += 1;
            }
        }
        drop_entry(&row.recycle_to, entry);
        expired.push(row.journal_id);
    }
    if !expired.is_empty() {
        db.write_blocking(Lane::Background, "publish.expiry_mark", |tx| {
            let mut mark = tx.prepare_cached(sql::EXPIRY_MARK.text())?;
            for id in &expired {
                mark.execute(params![id, pass.now_ms])?;
            }
            Ok::<_, PublishError>(())
        })?;
    }
    report.legacy_deleted = legacy(db, &sandbox, cutoff, memory)?;
    Ok(report)
}

/// Every row due at `cutoff`, read `page` at a time in the order they
/// reached the bin, so a row kept for good never holds up the rows behind
/// it.
fn due_rows(db: &Db, cutoff: f64, page: usize) -> Result<Vec<DueRow>, PublishError> {
    let mut rows = Vec::new();
    let mut cursor = (f64::MIN, String::new());
    loop {
        let found: Vec<(String, String, String, f64)> =
            db.read_long("publish.expiry_due", &CancelFlag::never(), |conn| {
                let mut statement = conn.prepare_cached(sql::EXPIRY_DUE.text())?;
                let found = statement
                    .query_map(params![cutoff, cursor.0, cursor.1, page as i64], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, PublishError>(found)
            })?;
        let last = found.len() < page;
        for (journal_id, recycle_to, sha256, recycled_at) in found {
            cursor = (recycled_at, journal_id.clone());
            rows.push(DueRow {
                journal_id,
                recycle_to: PathBuf::from(recycle_to),
                sha256,
            });
        }
        if last {
            return Ok(rows);
        }
    }
}

/// Check one row's copy where it is, without the lock. `None` keeps it
/// for now; the report says why.
fn examine(
    sandbox: &Sandbox,
    row: &DueRow,
    memory: &ExpiryMemory,
    report: &mut ExpiryReport,
) -> Option<Found> {
    let key = row.journal_id.as_str();
    let keep = |report: &mut ExpiryReport, why: String| {
        memory.note(key, Some(why));
        report.kept += 1;
        None
    };
    let name = row
        .recycle_to
        .file_name()
        .map(|name| name.to_string_lossy());
    let entry = entry_of(&row.recycle_to);
    let shaped = entry
        .and_then(|entry| entry.file_name())
        .is_some_and(|entry| super::bin::entry_name(&entry.to_string_lossy()));
    let bin = match sandbox.ensure_under_bin(&row.recycle_to) {
        Ok(bin) => bin,
        Err(error) => {
            return keep(
                report,
                format!("recycle bin expiry keeps the copy of journal {key}: {error}"),
            );
        }
    };
    if !shaped || name.is_none() || entry.and_then(Path::parent) != Some(bin.dir.as_path()) {
        return keep(
            report,
            format!("recycle bin expiry keeps the copy of journal {key}: it is not a bin entry"),
        );
    }
    let meta = match std::fs::symlink_metadata(&row.recycle_to) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if gone_for_sure(&bin.dir, entry) {
                return Some(Found::Gone);
            }
            memory.note(
                key,
                Some(format!(
                    "recycle bin expiry waits for the copy of journal {key}: its bin {} looks empty",
                    bin.id
                )),
            );
            report.waited += 1;
            return None;
        }
        Err(error) => {
            return keep(
                report,
                format!("recycle bin expiry can't look at the copy of journal {key}: {error}"),
            );
        }
    };
    if !meta.file_type().is_file() {
        return keep(
            report,
            format!("recycle bin expiry keeps the copy of journal {key}: it is not a plain file"),
        );
    }
    let stat = FileStat::of(&meta);
    if memory.known_changed(key, &stat) {
        report.changed += 1;
        return None;
    }
    match hash_file(&row.recycle_to, &stat) {
        Ok(Some(sha)) if sha == row.sha256 => Some(Found::Due(stat)),
        Ok(Some(_)) => {
            memory.remember_changed(key, stat);
            memory.note(
                key,
                Some(format!(
                    "recycle bin expiry keeps the copy of journal {key}: its bytes changed in the bin"
                )),
            );
            report.changed += 1;
            None
        }
        Ok(None) => {
            report.waited += 1;
            None
        }
        Err(error) => keep(
            report,
            format!("recycle bin expiry can't read the copy of journal {key}: {error}"),
        ),
    }
}

/// Whether a copy missing from `entry` is really gone: its entry folder
/// is still in the bin, or the bin holds some other entry. An empty bin
/// folder may be a share that is not mounted, so it proves nothing.
fn gone_for_sure(bin: &Path, entry: Option<&Path>) -> bool {
    if entry.is_some_and(|entry| {
        std::fs::symlink_metadata(entry).is_ok_and(|meta| meta.file_type().is_dir())
    }) {
        return true;
    }
    std::fs::read_dir(bin).is_ok_and(|mut children| children.next().is_some())
}

/// The SHA-256 of the file at `path`, read in pieces. `None` when the
/// file opened is not the one `stat` describes (it was swapped since).
fn hash_file(path: &Path, stat: &FileStat) -> std::io::Result<Option<String>> {
    let mut file = std::fs::File::open(path)?;
    if FileStat::of(&file.metadata()?) != *stat {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    ))
}

/// Whether a put-back holds the track of row `journal_id`'s copy.
fn held_by_put_back(db: &Db, journal_id: &str) -> Result<bool, PublishError> {
    db.read_long("publish.expiry_held", &CancelFlag::never(), |conn| {
        Ok::<_, PublishError>(
            conn.prepare_cached(sql::EXPIRY_HELD.text())?
                .query_row([journal_id], |row| row.get(0))?,
        )
    })
}

fn entry_of(copy: &Path) -> Option<&Path> {
    copy.parent()
}

/// After a copy went: the hidden temp a copy across disks may have left
/// beside it, then its entry folder when that is empty.
fn drop_entry(copy: &Path, entry: Option<&Path>) {
    if let Some(temp) = incoming_temp(copy)
        && let Err(error) = std::fs::remove_file(&temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "an unfinished recycle bin copy was not removed");
    }
    let Some(entry) = entry else {
        return;
    };
    match std::fs::remove_dir(entry) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => tracing::debug!(%error, "recycle bin entry folder kept"),
    }
    if let Some(bin) = entry.parent()
        && let Err(error) = super::journal::fsync_dir(bin)
    {
        tracing::warn!(%error, "directory sync failed");
    }
}

/// Entries no row names, in every bin of the pass: a top-level folder
/// named like ours (any release's `<stamp>-<hex>`) whose stamp is past
/// the retention loses its plain files, then itself. A folder holding
/// anything else is kept and logged; a name not like ours is never
/// touched (a `.recycle` may be a Samba share's). Answers how many went.
///
/// Every entry made since removals went through the publisher has a row,
/// and rows are never purged, so this only ever meets entries from
/// before. Were journal rows ever purged, their replacement rows would go
/// with them (the foreign key cascades) and this rule would then delete
/// those entries by name and age alone, unchecked: a purge must keep the
/// replacement rows, or mark them, first.
fn legacy(
    db: &Db,
    sandbox: &Sandbox,
    cutoff: f64,
    memory: &ExpiryMemory,
) -> Result<usize, PublishError> {
    let named = entry_names(db)?;
    let mut deleted = 0;
    for bin in sandbox.bins()? {
        if sandbox.ensure_bin_dir(bin).is_err() {
            continue;
        }
        let Ok(children) = std::fs::read_dir(&bin.dir) else {
            continue;
        };
        for child in children.flatten() {
            let name = child.file_name().to_string_lossy().into_owned();
            let old = super::bin::entry_stamp(&name).is_some_and(|stamp| (stamp as f64) < cutoff);
            if !old || named.contains(&name) || !child.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let path = child.path();
            match empty_of_plain_files(&path, memory) {
                Ok(true) => deleted += 1,
                Ok(false) => {}
                Err(error) => memory.note(
                    &path.to_string_lossy(),
                    Some(format!(
                        "recycle bin expiry skips the old entry {name}: {error}"
                    )),
                ),
            }
        }
    }
    Ok(deleted)
}

/// Delete the plain files in `entry`, then `entry`, when it holds nothing
/// else. True when it went.
fn empty_of_plain_files(entry: &Path, memory: &ExpiryMemory) -> Result<bool, PublishError> {
    let key = entry.to_string_lossy();
    let mut files = Vec::new();
    for child in std::fs::read_dir(entry)? {
        let child = child?;
        if !child.file_type()?.is_file() {
            memory.note(
                &key,
                Some(format!(
                    "recycle bin expiry keeps the old entry {}: it holds more than plain files",
                    child
                        .path()
                        .parent()
                        .and_then(Path::file_name)
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
                )),
            );
            return Ok(false);
        }
        files.push(child.path());
    }
    for file in &files {
        std::fs::remove_file(file)?;
    }
    std::fs::remove_dir(entry)?;
    Ok(true)
}

/// The entry folder name of every bin entry a row names, expired or not.
fn entry_names(db: &Db) -> Result<HashSet<String>, PublishError> {
    db.read_long("publish.expiry_entries", &CancelFlag::never(), |conn| {
        let mut statement = conn.prepare_cached(sql::EXPIRY_ENTRIES.text())?;
        let paths = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, PublishError>(
            paths
                .iter()
                .filter_map(|path| {
                    let entry = Path::new(path).parent()?.file_name()?;
                    Some(entry.to_string_lossy().into_owned())
                })
                .collect(),
        )
    })
}
