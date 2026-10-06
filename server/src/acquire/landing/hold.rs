//! Hold files for a person: copy each into the held directory and record
//! it in `held_imports` with the evidence that stopped it (v2
//! `_hold_for_review`). The copy lands and syncs before its row, so a row
//! never points at a missing file; a file the task already holds keeps
//! its first copy.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::acquire::dispatch::Journal;
use crate::acquire::downloads::landing_rows::HeldFile;

/// One file to hold and what to record about it.
#[derive(Debug, Clone)]
pub struct HoldItem {
    pub source: PathBuf,
    pub row: HeldFile,
}

/// Hold every item. Returns how many were recorded (already-held files
/// count). A failed copy or row is logged and skipped: the source stays
/// where it is, so a reimport can still find it.
pub async fn hold(
    journal: &Arc<Journal>,
    held_dir: &Path,
    items: Vec<HoldItem>,
    now: f64,
) -> usize {
    let mut held = 0;
    for item in items {
        let dir = held_dir.to_path_buf();
        let source = item.source.clone();
        let copied = tokio::task::spawn_blocking(move || copy_into(&dir, &source)).await;
        let held_path = match copied {
            Ok(Ok(path)) => path,
            Ok(Err(error)) => {
                tracing::warn!(file = %item.row.original_filename, %error, "held copy failed");
                continue;
            }
            Err(error) => {
                tracing::warn!(file = %item.row.original_filename, %error, "held copy join failed");
                continue;
            }
        };
        let mut row = item.row;
        row.held_path = held_path.to_string_lossy().into_owned();
        let recorded = journal
            .run("downloads.hold_file", move |store| {
                store.record_held_file(&row, now)
            })
            .await;
        match recorded {
            Ok(Some(_)) => held += 1,
            Ok(None) => {
                held += 1;
                remove_copy(&held_path);
            }
            Err(error) => {
                tracing::warn!(%error, "held row not recorded; copy removed");
                remove_copy(&held_path);
            }
        }
    }
    held
}

/// Copy one file into the held directory under a fresh name, synced.
fn copy_into(dir: &Path, source: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "held.bin".to_owned());
    let dest = dir.join(format!("{}_{name}", uuid::Uuid::new_v4().simple()));
    let mut reader = std::fs::File::open(source)?;
    let mut writer = std::fs::File::create_new(&dest)?;
    let written = std::io::copy(&mut reader, &mut writer)
        .and_then(|_| writer.flush())
        .and_then(|()| writer.sync_all());
    if let Err(error) = written {
        drop(writer);
        remove_copy(&dest);
        return Err(error);
    }
    Ok(dest)
}

fn remove_copy(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "held copy not removed");
    }
}
