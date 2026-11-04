//! Writes that need an administrator: what the publish journal still
//! holds (unfinished writes recovery will settle, cleanups to retry,
//! writes it has been retrying for more than a day, and writes it could
//! not decide about), the hidden files no journal row names, and closing
//! a write of the undecided kind once each of its destinations holds what
//! the write placed, what was there before it, or nothing. Closing changes
//! no file and keeps the journal's evidence; it records that someone looked
//! and what was found.

use droppedneedle_kernel::db::Lane;
use std::collections::BTreeMap;

use rusqlite::{OptionalExtension as _, params};

use super::models::ManagementError;
use super::service::Management;
use super::sql::{AttentionBundleRow, BundleResolution, JournalStateCount};
use crate::publish::journal::{FileJournal, JournalKind, JournalState, JournalStore};
use crate::publish::paths::Sandbox;
use crate::publish::recovery::Leftover;

/// One write that needs an administrator.
#[derive(Debug, Clone)]
pub struct AttentionBundle {
    pub bundle_id: String,
    pub file_count: i64,
    /// A few of the paths the write was putting files at.
    pub sample_paths: Vec<String>,
    /// The folders the write took its files from, relative to their
    /// library folder. For a download that is the hidden import folder
    /// holding the downloaded files.
    pub source_folders: Vec<String>,
}

/// The publish journal at a glance.
#[derive(Debug, Clone, Default)]
pub struct Diagnostics {
    pub recoverable_bundle_count: i64,
    pub nonterminal_journal_count: i64,
    pub needs_attention_count: i64,
    pub cleanup_pending_count: i64,
    pub oldest_updated_at: Option<f64>,
    pub state_counts: BTreeMap<String, i64>,
    pub needs_attention_bundles: Vec<AttentionBundle>,
    /// Writes recovery has been retrying for more than a day. It keeps
    /// retrying them.
    pub retrying_bundles: Vec<AttentionBundle>,
    /// Hidden files no journal row names, as the last check of each root
    /// found them. Zero until a root has been checked.
    pub leftover_file_count: i64,
    pub leftover_files: Vec<Leftover>,
}

/// A closed write.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub bundle_id: String,
    pub state: String,
    pub verified_files: i64,
    pub total_files: i64,
    /// What closing did, in words.
    pub note: String,
}

/// Most writes needing attention listed at once.
const ATTENTION_LIST_MAX: i64 = 100;

/// How long a write stays unsettled, from when it was recorded, before it
/// counts as still being retried. A healthy write settles in seconds.
const RETRYING_AFTER_MS: i64 = 24 * 60 * 60 * 1000;

fn attention_bundle(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttentionBundle> {
    let row = AttentionBundleRow::read(row)?;
    Ok(AttentionBundle {
        bundle_id: row.bundle_id,
        file_count: row.file_count,
        sample_paths: row
            .sample_paths
            .unwrap_or_default()
            .lines()
            .take(5)
            .map(str::to_owned)
            .collect(),
        source_folders: source_folders(row.source_paths.as_deref()),
    })
}

/// The distinct parent folders of newline-separated source paths, at most
/// five. A file at the top of its library folder has none.
fn source_folders(paths: Option<&str>) -> Vec<String> {
    let folders: std::collections::BTreeSet<&str> = paths
        .unwrap_or_default()
        .lines()
        .filter_map(|path| path.rsplit_once('/').map(|(folder, _)| folder))
        .collect();
    folders.into_iter().take(5).map(str::to_owned).collect()
}

/// What closing a write does, said plainly: it records what it found and
/// changes nothing else.
fn resolved_note(as_before: i64, empty: i64) -> String {
    format!(
        "Marked as handled: {as_before} back as before, {empty} gone, the rest as written. \
         No file was changed. A scan now updates the library. Leftover temporary copies stay \
         until you remove them."
    )
}

/// Writes recorded at least [`RETRYING_AFTER_MS`] before `now_ms` (unix
/// milliseconds) that recovery has still neither finished nor undone.
/// Each has been through recovery at startup and on every maintenance
/// tick since. A committed write whose cleanup is pending is finished and
/// counted elsewhere, so it is left out.
pub fn retrying_bundles(
    conn: &rusqlite::Connection,
    now_ms: i64,
) -> rusqlite::Result<Vec<AttentionBundle>> {
    conn.prepare_cached(crate::publish::sql::RETRYING_BUNDLES.text())?
        .query_map(
            params![
                now_ms - RETRYING_AFTER_MS,
                ATTENTION_LIST_MAX,
                JournalState::Prepared,
                JournalState::Staged,
                JournalState::Published,
                JournalState::Committed,
            ],
            attention_bundle,
        )?
        .collect()
}

impl Management {
    /// Whether every write that took files from a download task's import
    /// folder is over: cleaned up, undone, committed with only its own
    /// cleanup left, or closed by an administrator after needing
    /// attention. A write still on its way, one that needs attention and
    /// a state this does not know all answer no. No such write answers
    /// yes. Blocking.
    pub fn download_publish_settled(&self, task_id: &str) -> Result<bool, ManagementError> {
        let folder = format!("{}/", crate::publish::import_staging::staging_rel(task_id));
        self.parts
            .db
            .read_blocking("management.download_publish_settled", |conn| {
                let unsettled: bool = conn.query_row(
                    crate::management::sql::DOWNLOAD_PUBLISH_SETTLED.text(),
                    params![
                        folder,
                        JournalState::Cleaned,
                        JournalState::Compensated,
                        JournalState::CleanupPending,
                        JournalState::NeedsAttention,
                    ],
                    |row| row.get(0),
                )?;
                Ok(!unsettled)
            })
    }

    /// What the publish journal holds. Blocking.
    pub fn recovery_diagnostics(&self) -> Result<Diagnostics, ManagementError> {
        self.parts
            .db
            .read_blocking("management.recovery_diagnostics", |conn| {
                let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
                let mut diagnostics = Diagnostics {
                    recoverable_bundle_count: count(
                        crate::management::sql::OPEN_BUNDLE_COUNT.text(),
                    )?,
                    nonterminal_journal_count: count(
                        crate::management::sql::OPEN_JOURNAL_COUNT.text(),
                    )?,
                    needs_attention_count: count(
                        crate::management::sql::ATTENTION_JOURNAL_COUNT.text(),
                    )?,
                    cleanup_pending_count: count(
                        crate::management::sql::CLEANUP_PENDING_COUNT.text(),
                    )?,
                    ..Diagnostics::default()
                };
                diagnostics.oldest_updated_at = conn
                    .prepare_cached(crate::publish::sql::OLDEST_OPEN_WRITE.text())?
                    .query_row(
                        params![
                            JournalState::Cleaned,
                            JournalState::Compensated,
                            JournalState::NeedsAttention,
                        ],
                        |row| row.get::<_, Option<i64>>(0),
                    )?
                    .map(|ms| ms as f64 / 1000.0);
                let mut states =
                    conn.prepare(crate::management::sql::JOURNAL_COUNTS_BY_STATE.text())?;
                for row in states.query_map([], JournalStateCount::read)? {
                    let row = row?;
                    diagnostics.state_counts.insert(row.state, row.total);
                }
                let mut bundles = conn.prepare(crate::management::sql::ATTENTION_BUNDLES.text())?;
                for row in bundles.query_map(params![ATTENTION_LIST_MAX], attention_bundle)? {
                    diagnostics.needs_attention_bundles.push(row?);
                }
                diagnostics.retrying_bundles = retrying_bundles(conn, self.parts.clock.now_ms())?;
                Ok(diagnostics)
            })
            .and_then(|mut diagnostics| {
                let report = self
                    .parts
                    .leftover_files
                    .report(&self.parts.core.live_registry())?;
                diagnostics.leftover_file_count = i64::try_from(report.count).unwrap_or(i64::MAX);
                diagnostics.leftover_files = report.files;
                Ok(diagnostics)
            })
    }

    /// Close a write that needs an administrator, once each destination
    /// holds the bytes it wrote, the bytes from before it, or nothing.
    /// Nothing on disk changes, and a scan is asked to look. A file holding
    /// anything else, or that cannot be checked, refuses with which one, so
    /// it can be put right first. Blocking.
    pub fn resolve_bundle(
        &self,
        bundle_id: &str,
        actor: &str,
    ) -> Result<Resolved, ManagementError> {
        let journals = self
            .parts
            .db
            .read_blocking("management.bundle_journals", |conn| {
                JournalStore::new(conn)
                    .bundle(bundle_id)
                    .map_err(|error| ManagementError::Store(error.to_string()))
            })?;
        if journals.is_empty() {
            return Err(ManagementError::NotFound(
                "This write is not in the journal.",
            ));
        }
        let earlier = self
            .parts
            .db
            .read_blocking("management.resolution", |conn| {
                Ok::<_, ManagementError>(
                    conn.query_row(
                        crate::management::sql::BUNDLE_RESOLUTION.text(),
                        params![bundle_id],
                        BundleResolution::read,
                    )
                    .optional()?,
                )
            })?;
        if let Some(earlier) = earlier {
            return Ok(Resolved {
                bundle_id: bundle_id.to_owned(),
                state: "resolved".to_owned(),
                verified_files: earlier.verified_files,
                total_files: earlier.verified_files,
                note: resolved_note(earlier.as_before_files, earlier.empty_files),
            });
        }
        let pending: Vec<&FileJournal> = journals
            .iter()
            .filter(|journal| journal.state == JournalState::NeedsAttention)
            .collect();
        if pending.is_empty() {
            return Err(ManagementError::Conflict(
                "Only a write that needs attention can be closed; recovery settles the others."
                    .to_owned(),
            ));
        }
        let sandbox = self.parts.publishing.publish_sandbox().map_err(|_| {
            ManagementError::Conflict(
                "The library folders are not available right now, so the files cannot be checked."
                    .to_owned(),
            )
        })?;
        let mut problems = Vec::new();
        let (mut as_before, mut empty) = (0i64, 0i64);
        for journal in &pending {
            // A delete's staged copy is a safety backup, never a live file,
            // and a row with no staged bytes never placed anything.
            if journal.kind == JournalKind::Delete || journal.staged_sha256.is_empty() {
                continue;
            }
            let rel = &journal.dest_rel;
            match destination_holds(&sandbox, journal) {
                Ok(Found::AsWritten) => {}
                Ok(Found::AsBefore) => as_before += 1,
                Ok(Found::Nothing) => empty += 1,
                Ok(Found::Other) => problems.push(format!("{rel} holds different bytes")),
                Err(problem) => problems.push(format!("{rel} {problem}")),
            }
        }
        if !problems.is_empty() {
            return Err(ManagementError::Conflict(format!(
                "This write cannot be closed yet: {}. Put the files back or restore them from a backup, then try again.",
                problems.join("; ")
            )));
        }
        let verified = pending.len() as i64;
        self.parts
            .db
            .write_blocking(Lane::Foreground, "management.resolve_bundle", |conn| {
                conn.execute(
                    crate::management::sql::RECORD_BUNDLE_RESOLUTION.text(),
                    params![
                        bundle_id,
                        actor,
                        verified,
                        self.parts.clock.now_ms(),
                        as_before,
                        empty
                    ],
                )?;
                Ok::<_, ManagementError>(())
            })?;
        // The fence is lifted, so a scan can now settle these tracks.
        for journal in &pending {
            self.parts.core.dirty.mark(&journal.dest_root);
            if let Some((root, _)) = &journal.source {
                self.parts.core.dirty.mark(root);
            }
        }
        self.parts.wakeups.notify("scan");
        tracing::info!(
            bundle_id,
            verified,
            as_before,
            empty,
            "write needing attention closed after checking its files"
        );
        Ok(Resolved {
            bundle_id: bundle_id.to_owned(),
            state: "resolved".to_owned(),
            verified_files: verified,
            total_files: verified,
            note: resolved_note(as_before, empty),
        })
    }
}

/// What a closed write's destination holds.
enum Found {
    /// The bytes the write placed.
    AsWritten,
    /// The bytes from before the write: the file it replaced, or the source
    /// of a write to the same path.
    AsBefore,
    /// Nothing: the filesystem says so, never a failed read.
    Nothing,
    Other,
}

fn destination_holds(sandbox: &Sandbox, journal: &FileJournal) -> Result<Found, &'static str> {
    // A removal's and a put-back's files may sit under a folder that only
    // takes removals.
    let resolved = if matches!(journal.kind, JournalKind::Remove | JournalKind::PutBack) {
        sandbox.resolve_removal(&journal.dest_root, &journal.dest_rel)
    } else {
        sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)
    };
    let path = resolved.map_err(|_| "could not be checked")?;
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Found::Nothing),
        Err(_) => return Err("could not be checked"),
        Ok(_) => {}
    }
    let hashed = crate::publish::paths::hash_regular_file(&path)
        .map_err(|_| "could not be read")?
        .sha256;
    let same_path = journal
        .source
        .as_ref()
        .is_some_and(|(root, rel)| *root == journal.dest_root && *rel == journal.dest_rel);
    let before = match &journal.replaces {
        Some(replaced) => Some(replaced.sha256.as_str()),
        None if same_path => journal.source_sha256.as_deref(),
        None => None,
    };
    Ok(if hashed == journal.staged_sha256 {
        Found::AsWritten
    } else if before == Some(hashed.as_str()) {
        Found::AsBefore
    } else {
        Found::Other
    })
}
