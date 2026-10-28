//! A file added to the library with no track behind it (a plugin's
//! `host.library.write`), under one `add` journal row, and its undo under
//! one `delete` row.
//!
//! The bytes are written to the row's hidden temp beside the destination
//! and placed by a call that refuses a taken name; a root that can only
//! check, then rename, takes no added file, since that rename can replace
//! a file that appears in between. The commit changes no catalog row: the
//! next scan picks the file up like any other. The bundle records who
//! added the file, and its undo takes the file away only while it still
//! holds the bytes that were written, once, and only after the add landed.

use std::path::Path;

use rusqlite::Connection;

use super::journal::{FileJournal, JournalKind, JournalState, JournalStore};
use super::paths::{self, Sandbox};
use super::place::PlacementMode;
use super::planner::PlanBundle;
use super::{PublishError, sql};

/// Who made a change no person applied.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// A plugin, by name.
    Plugin(String),
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plugin(name) => write!(f, "plugin:{name}"),
        }
    }
}

/// The one file an add places.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AddedFile {
    pub root_id: String,
    pub relative_path: String,
    /// The written bytes.
    pub sha256: String,
    pub size: u64,
}

/// A file added with no track behind it, or its undo.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Added {
    pub origin: Origin,
    pub file: AddedFile,
    /// On an undo: the bundle that added the file this takes away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<String>,
}

/// The id of the one row an add or its undo writes.
pub(super) fn journal_id(bundle_id: &str, added: &Added) -> String {
    if added.undoes.is_some() {
        format!("{bundle_id}-takeback-000")
    } else {
        format!("{bundle_id}-add-000")
    }
}

/// The ids of every row a bundle's added file writes.
pub(super) fn journal_ids(bundle: &PlanBundle) -> Vec<String> {
    bundle
        .added
        .as_ref()
        .map(|added| journal_id(&bundle.id, added))
        .into_iter()
        .collect()
}

/// How an added file is placed on a root probed as `mode`: never by
/// check-then-rename, which can replace a file that turns up meanwhile.
pub(super) fn placement(mode: PlacementMode) -> PlacementMode {
    match mode {
        PlacementMode::Check => PlacementMode::Unknown,
        other => other,
    }
}

/// The placement mode for `journal`'s file under `sandbox`.
pub(super) fn mode_for(sandbox: &Sandbox, journal: &FileJournal) -> PlacementMode {
    let mode = sandbox.placement_mode(&journal.dest_root);
    if journal.kind == JournalKind::Add {
        placement(mode)
    } else {
        mode
    }
}

fn holds(path: &Path, sha256: &str) -> bool {
    paths::holds(path, sha256)
}

/// What an add's pre-commit row shows on disk.
pub(super) enum Evidence {
    /// Placed, or staged with its name still free: a resume finishes it.
    Resume,
    /// Never staged (its temp may be part written), nothing left to place,
    /// or another file took the name: the unwind removes only the temp.
    Unwind,
    Ambiguous(String),
}

/// Read one add row before its commit.
pub(super) fn evidence(sandbox: &Sandbox, journal: &FileJournal) -> Evidence {
    let Ok(dest) = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel) else {
        return Evidence::Ambiguous(format!("journal {} destination unsafe", journal.id));
    };
    let temp = Path::new(&journal.staged);
    if sandbox.ensure_under_roots(temp).is_err() {
        return Evidence::Ambiguous(format!(
            "journal {} staged path escapes sandbox",
            journal.id
        ));
    }
    // A temp still being written belongs to the row: it is never placed
    // before it is staged.
    if journal.state == JournalState::Prepared {
        return Evidence::Unwind;
    }
    let temp_present = std::fs::symlink_metadata(temp).is_ok();
    let one_file = paths::same_file(temp, &dest);
    if temp_present && !one_file && !holds(temp, &journal.staged_sha256) {
        return Evidence::Ambiguous(format!("journal {} staged bytes changed", journal.id));
    }
    if !temp_present || one_file {
        return if holds(&dest, &journal.staged_sha256) {
            Evidence::Resume
        } else {
            Evidence::Unwind
        };
    }
    if std::fs::symlink_metadata(&dest).is_ok() {
        Evidence::Unwind
    } else {
        Evidence::Resume
    }
}

/// Why an added file cannot be taken away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeBackRefusal {
    /// The bundle added no file.
    NotAnAdd,
    /// The bundle is itself the undo of an added file.
    IsATakeBack,
    /// The add never landed.
    NotLanded,
    /// The file was taken away already, or an undo of it is still settling.
    AlreadyTakenBack,
    /// Its library folder takes no file changes now (excluded, gone, or
    /// reached through a link into another library folder).
    FolderUnusable(String),
    /// The file is no longer there.
    Gone(String),
    /// The file no longer holds the bytes that were written.
    Changed(String),
}

/// What the undo of the add `bundle_id` carries, when the database lets it
/// run: the add landed and was not taken away before. Whether its file
/// still holds the written bytes is [`check_take_back`]'s question.
pub fn plan_take_back(
    conn: &Connection,
    bundle_id: &str,
) -> Result<Result<Added, TakeBackRefusal>, PublishError> {
    let Some(source) = super::operations::load_operation(conn, bundle_id)? else {
        return Ok(Err(TakeBackRefusal::NotAnAdd));
    };
    let Some(added) = source.added else {
        return Ok(Err(TakeBackRefusal::NotAnAdd));
    };
    if added.undoes.is_some() {
        return Ok(Err(TakeBackRefusal::IsATakeBack));
    }
    let landed = JournalStore::new(conn)
        .get(&journal_id(bundle_id, &added))?
        .is_some_and(|row| {
            matches!(
                row.state,
                JournalState::Committed | JournalState::CleanupPending | JournalState::Cleaned
            )
        });
    if !landed {
        return Ok(Err(TakeBackRefusal::NotLanded));
    }
    let taken: bool = conn
        .prepare_cached(sql::TAKE_BACK_EXISTS.text())?
        .query_row(rusqlite::params![undoes_marker(bundle_id)], |row| {
            row.get(0)
        })?;
    if taken {
        return Ok(Err(TakeBackRefusal::AlreadyTakenBack));
    }
    Ok(Ok(Added {
        origin: added.origin,
        file: added.file,
        undoes: Some(bundle_id.to_owned()),
    }))
}

/// Whether the file an undo takes away is still the one written, in a
/// library folder that takes file changes.
pub fn check_take_back(sandbox: &Sandbox, file: &AddedFile) -> Result<(), TakeBackRefusal> {
    let Ok(dest) = sandbox.resolve_no_symlink(&file.root_id, &file.relative_path) else {
        return Err(TakeBackRefusal::FolderUnusable(file.relative_path.clone()));
    };
    if std::fs::symlink_metadata(&dest).is_err() {
        return Err(TakeBackRefusal::Gone(file.relative_path.clone()));
    }
    if !holds(&dest, &file.sha256) {
        return Err(TakeBackRefusal::Changed(file.relative_path.clone()));
    }
    Ok(())
}

/// One file a plugin added that landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFile {
    /// The bundle that added it; its undo takes the file away.
    pub bundle_id: String,
    /// When it was added, in unix milliseconds.
    pub added_at: i64,
    pub file: AddedFile,
    /// An undo took it away, or is still settling.
    pub taken_back: bool,
}

/// The files plugin `plugin` added, newest first, at most `limit`, after
/// `before` (the `added_at` and bundle id of the last file of the page
/// before).
pub fn plugin_files(
    conn: &Connection,
    plugin: &str,
    before: Option<(i64, &str)>,
    limit: u32,
) -> Result<Vec<PluginFile>, PublishError> {
    let (at, id) = before.unwrap_or((i64::MAX, ""));
    let rows: Vec<(String, i64, String, bool)> = conn
        .prepare_cached(sql::PLUGIN_FILES.text())?
        .query_map(rusqlite::params![plugin, at, id, limit], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    rows.into_iter()
        .map(|(bundle_id, added_at, json, taken_back)| {
            let bundle: PlanBundle = serde_json::from_str(&json)?;
            let added = bundle.added.ok_or_else(|| {
                PublishError::Journal(format!("operation {bundle_id} added no file").into())
            })?;
            Ok(PluginFile {
                bundle_id,
                added_at,
                file: added.file,
                taken_back,
            })
        })
        .collect()
}

/// The text an undo's recorded bundle holds for the add it undoes.
fn undoes_marker(bundle_id: &str) -> String {
    format!("\"undoes\":{}", serde_json::Value::from(bundle_id))
}
