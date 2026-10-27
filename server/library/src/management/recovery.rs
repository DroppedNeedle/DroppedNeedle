//! Writes that need an administrator: what the publish journal still
//! holds (unfinished writes recovery will settle, cleanups to retry,
//! writes it has been retrying for more than a day, and writes it could
//! not decide about), the hidden files no journal row names, and closing
//! a write of the undecided kind once its files are verified where the
//! write put them. Closing changes no file and keeps the journal's
//! evidence; it records that someone looked and everything was in place.

use droppedneedle_kernel::db::Lane;
use std::collections::BTreeMap;

use rusqlite::{OptionalExtension as _, params};

use super::models::ManagementError;
use super::service::Management;
use crate::clock::LibraryClock as _;
use crate::publish::recovery::Leftover;

/// One write that needs an administrator.
#[derive(Debug, Clone)]
pub struct AttentionBundle {
    pub bundle_id: String,
    pub file_count: i64,
    /// A few of the paths the write was putting files at.
    pub sample_paths: Vec<String>,
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
    /// Hidden files no journal row names, as the pass after startup found
    /// them. Zero until that pass has finished.
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
    Ok(AttentionBundle {
        bundle_id: row.get(0)?,
        file_count: row.get(1)?,
        sample_paths: row
            .get::<_, Option<String>>(2)?
            .unwrap_or_default()
            .lines()
            .take(5)
            .map(str::to_owned)
            .collect(),
    })
}

/// What closing a write does, said plainly: it records that the files
/// were checked, and changes nothing else.
const RESOLVED_NOTE: &str = "Marked as handled: every file was checked in place. Nothing on disk \
     or in the library catalog was changed. If the library does not list these files, rescan \
     their folder; leftover temporary copies stay where they are until you remove them.";

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
            params![now_ms - RETRYING_AFTER_MS, ATTENTION_LIST_MAX],
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
        self.setup
            .db
            .read_blocking("management.download_publish_settled", |conn| {
                let unsettled: bool = conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
                 WHERE bundle_id IN (SELECT bundle_id FROM library_publish_journal \
                 WHERE substr(source_rel, 1, length(?1)) = ?1) \
                 AND state NOT IN ('cleaned', 'compensated', 'cleanup_pending') \
                 AND NOT (state = 'needs_attention' AND bundle_id IN \
                 (SELECT bundle_id FROM library_publish_resolutions)))",
                    params![folder],
                    |row| row.get(0),
                )?;
                Ok(!unsettled)
            })
    }

    /// What the publish journal holds. Blocking.
    pub fn recovery_diagnostics(&self) -> Result<Diagnostics, ManagementError> {
        self.setup.db.read_blocking("management.recovery_diagnostics", |conn| {
            let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
            let open = "state NOT IN ('cleaned', 'compensated', 'needs_attention')";
            let attention = "state = 'needs_attention' AND bundle_id NOT IN \
                 (SELECT bundle_id FROM library_publish_resolutions)";
            let mut diagnostics = Diagnostics {
                recoverable_bundle_count: count(&format!(
                    "SELECT COUNT(DISTINCT bundle_id) FROM library_publish_journal WHERE {open}"
                ))?,
                nonterminal_journal_count: count(&format!(
                    "SELECT COUNT(*) FROM library_publish_journal WHERE {open}"
                ))?,
                needs_attention_count: count(&format!(
                    "SELECT COUNT(*) FROM library_publish_journal WHERE {attention}"
                ))?,
                cleanup_pending_count: count(
                    "SELECT COUNT(*) FROM library_publish_journal WHERE state = 'cleanup_pending'",
                )?,
                ..Diagnostics::default()
            };
            diagnostics.oldest_updated_at = conn
                .prepare_cached(crate::publish::sql::OLDEST_OPEN_WRITE.text())?
                .query_row([], |row| row.get::<_, Option<i64>>(0))?
                .map(|ms| ms as f64 / 1000.0);
            let mut states = conn.prepare(
                "SELECT state, COUNT(*) FROM library_publish_journal GROUP BY state ORDER BY state",
            )?;
            for row in states.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))? {
                let (state, count): (String, i64) = row?;
                diagnostics.state_counts.insert(state, count);
            }
            let mut bundles = conn.prepare(&format!(
                "SELECT bundle_id, COUNT(*), GROUP_CONCAT(dest_rel, char(10)) \
                 FROM library_publish_journal WHERE {attention} \
                 GROUP BY bundle_id ORDER BY bundle_id LIMIT ?1"
            ))?;
            for row in bundles.query_map(params![ATTENTION_LIST_MAX], attention_bundle)? {
                diagnostics.needs_attention_bundles.push(row?);
            }
            diagnostics.retrying_bundles = retrying_bundles(conn, self.setup.clock.now_ms())?;
            Ok(diagnostics)
        })
        .map(|mut diagnostics| {
            if let Some(report) = self.setup.leftover_files.latest() {
                diagnostics.leftover_file_count = report.count as i64;
                diagnostics.leftover_files = report.files;
            }
            diagnostics
        })
    }

    /// Close a write that needs an administrator, once every file it was
    /// putting in place is there with the bytes it wrote. Nothing on disk
    /// changes. A file that is missing or different refuses with which
    /// one, so it can be put right first. Blocking.
    pub fn resolve_bundle(
        &self,
        bundle_id: &str,
        actor: &str,
    ) -> Result<Resolved, ManagementError> {
        let journals: Vec<(String, String, String, String, String)> = self.setup.db.read_blocking("management.bundle_journals", |conn| {
            let mut statement = conn.prepare(
                "SELECT state, kind, dest_root, dest_rel, staged_sha256 FROM library_publish_journal \
                 WHERE bundle_id = ?1 ORDER BY id",
            )?;
            let rows = statement.query_map(params![bundle_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })?;
            Ok::<_, ManagementError>(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        if journals.is_empty() {
            return Err(ManagementError::NotFound(
                "This write is not in the journal.",
            ));
        }
        let earlier: Option<i64> =
            self.setup
                .db
                .read_blocking("management.resolution", |conn| {
                    Ok::<_, ManagementError>(conn
                .query_row(
                    "SELECT verified_files FROM library_publish_resolutions WHERE bundle_id = ?1",
                    params![bundle_id],
                    |row| row.get(0),
                )
                .optional()?)
                })?;
        if let Some(verified) = earlier {
            return Ok(Resolved {
                bundle_id: bundle_id.to_owned(),
                state: "resolved".to_owned(),
                verified_files: verified,
                total_files: verified,
                note: RESOLVED_NOTE.to_owned(),
            });
        }
        let pending: Vec<_> = journals
            .iter()
            .filter(|(state, ..)| state == "needs_attention")
            .collect();
        if pending.is_empty() {
            return Err(ManagementError::Conflict(
                "Only a write that needs attention can be closed; recovery settles the others."
                    .to_owned(),
            ));
        }
        let sandbox = self.setup.publishing.publish_sandbox().map_err(|_| {
            ManagementError::Conflict(
                "The library folders are not available right now, so the files cannot be checked."
                    .to_owned(),
            )
        })?;
        let mut problems = Vec::new();
        for (_, kind, root, rel, staged) in &pending {
            // A delete's staged copy is a safety backup, never a live file.
            if kind == "delete" {
                continue;
            }
            let found = sandbox
                .resolve_no_symlink(root, rel)
                .map_err(|_| "could not be checked")
                .and_then(|path| {
                    crate::publish::paths::hash_regular_file(&path).map_err(|_| "is missing")
                });
            match found {
                Ok(hashed) if &hashed.sha256 == staged => {}
                Ok(_) => problems.push(format!("{rel} holds different bytes")),
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
        self.setup.db.write_blocking(Lane::Foreground, "management.resolve_bundle", |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO library_publish_resolutions (bundle_id, resolved_by_user_id, \
                 verified_files, resolved_at) VALUES (?1, ?2, ?3, ?4)",
                params![bundle_id, actor, verified, self.setup.clock.now_secs()],
            )?;
            Ok::<_, ManagementError>(())
        })?;
        tracing::info!(
            bundle_id,
            verified,
            "write needing attention closed after checking its files"
        );
        Ok(Resolved {
            bundle_id: bundle_id.to_owned(),
            state: "resolved".to_owned(),
            verified_files: verified,
            total_files: verified,
            note: RESOLVED_NOTE.to_owned(),
        })
    }
}
