//! Hold files for a person: copy each into the held folder and record it
//! in the download's entry of the held list, with the evidence that
//! stopped it. The copy lands and syncs before its
//! row, so a row never points at a missing file; a track (or unpaired
//! file) the entry already holds keeps its first copy, whichever candidate
//! or retry brought it.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use droppedneedle_kernel::db::DbError;

use crate::db::AcquireDb;
use crate::held::store::{HeldRows, NewEntry, NewFile};

/// One file to hold and what to record about it. `file.path` is filled
/// with the copy's name once it is made.
#[derive(Debug, Clone)]
pub struct HoldItem {
    pub source: PathBuf,
    pub file: NewFile,
}

/// Hold every item in `entry`, each source read beneath one of the
/// client's `downloads_folders` (real paths). Returns how many were
/// recorded (files the entry already holds count). A failed copy or row
/// is logged and skipped: the source stays where it is, so a reimport
/// can still find it.
pub async fn hold(
    db: &AcquireDb,
    held_dir: &Path,
    downloads_folders: &[PathBuf],
    entry: &NewEntry,
    items: Vec<HoldItem>,
) -> usize {
    let mut held = 0;
    for item in items {
        // Already held by an earlier candidate or pass: no second copy.
        let (probe_entry, probe_file) = (entry.clone(), item.file.clone());
        let known = db
            .db()
            .read("downloads.held_known", move |conn| {
                Ok::<_, DbError>(HeldRows::new(conn).already_held(&probe_entry, &probe_file)?)
            })
            .await;
        if matches!(known, Ok(true)) {
            held += 1;
            continue;
        }
        let dir = held_dir.to_path_buf();
        let source = item.source.clone();
        let folders = downloads_folders.to_vec();
        let copied = tokio::task::spawn_blocking(move || copy_into(&dir, &folders, &source)).await;
        let name = match copied {
            Ok(Ok(name)) => name,
            Ok(Err(error)) => {
                tracing::warn!(file = %item.file.original_name, %error, "held copy failed");
                continue;
            }
            Err(error) => {
                tracing::warn!(file = %item.file.original_name, %error, "held copy join failed");
                continue;
            }
        };
        let mut file = item.file;
        file.path = name.clone();
        let entry = entry.clone();
        let clock = db.clock().clone();
        let recorded = db
            .write_background("downloads.hold_file", move |tx| {
                let (_, held) = HeldRows::new(tx).hold(&entry, &[file], clock.now_ms())?;
                Ok(held.first().copied().flatten())
            })
            .await;
        match recorded {
            Ok(Some(_)) => held += 1,
            Ok(None) => {
                held += 1;
                remove_copy(&held_dir.join(&name));
            }
            Err(error) => {
                tracing::warn!(%error, "held row not recorded; copy removed");
                remove_copy(&held_dir.join(&name));
            }
        }
    }
    held
}

/// Copy one file, read beneath `folders`, into the held folder under a
/// fresh name, synced. Answers the copy's name, which is its path below
/// the folder.
fn copy_into(dir: &Path, folders: &[PathBuf], source: &Path) -> std::io::Result<String> {
    let mut reader = super::confine::open_beneath(folders, source)?;
    std::fs::create_dir_all(dir)?;
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "held.bin".to_owned());
    let name = format!("{}_{name}", uuid::Uuid::new_v4().simple());
    let dest = dir.join(&name);
    let mut writer = std::fs::File::create_new(&dest)?;
    let written = std::io::copy(&mut reader, &mut writer)
        .and_then(|_| writer.flush())
        .and_then(|()| writer.sync_all());
    if let Err(error) = written {
        drop(writer);
        remove_copy(&dest);
        return Err(error);
    }
    Ok(name)
}

fn remove_copy(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "held copy not removed");
    }
}
