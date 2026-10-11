//! Writes that need an administrator: what the publish journal still
//! holds (unfinished writes recovery will settle, cleanups to retry, and
//! writes it could not decide about), and closing one of the last kind
//! once its files are verified where the write put them. Closing changes
//! no file and keeps the journal's evidence; it records that someone
//! looked and everything was in place.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension as _, params};

use super::models::ManagementError;
use super::service::Management;
use crate::library::clock::now_unix;
use crate::library::publish::snapshots::sha256_hex;

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
}

/// A closed write.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub bundle_id: String,
    pub state: String,
    pub verified_files: i64,
    pub total_files: i64,
}

/// Most writes needing attention listed at once.
const ATTENTION_LIST_MAX: i64 = 100;

impl Management {
    /// What the publish journal holds. Blocking.
    pub fn recovery_diagnostics(&self) -> Result<Diagnostics, ManagementError> {
        self.read(|conn| {
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
            diagnostics.oldest_updated_at = conn.query_row(
                &format!(
                    "SELECT MIN(o.created_day) FROM library_publish_operations o WHERE o.bundle_id IN \
                     (SELECT bundle_id FROM library_publish_journal WHERE {open} OR ({attention}))"
                ),
                [],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .map(|day| day as f64 * 86_400.0);
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
            for row in bundles.query_map(params![ATTENTION_LIST_MAX], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })? {
                let (bundle_id, file_count, paths) = row?;
                diagnostics.needs_attention_bundles.push(AttentionBundle {
                    bundle_id,
                    file_count,
                    sample_paths: paths
                        .unwrap_or_default()
                        .lines()
                        .take(5)
                        .map(str::to_owned)
                        .collect(),
                });
            }
            Ok(diagnostics)
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
        let journals: Vec<(String, String, String, String, String)> = self.read(|conn| {
            let mut statement = conn.prepare(
                "SELECT state, kind, dest_root, dest_rel, staged_sha256 FROM library_publish_journal \
                 WHERE bundle_id = ?1 ORDER BY id",
            )?;
            let rows = statement.query_map(params![bundle_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        if journals.is_empty() {
            return Err(ManagementError::NotFound(
                "This write is not in the journal.",
            ));
        }
        let earlier: Option<i64> = self.read(|conn| {
            Ok(conn
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
        let sandbox = self.setup.publish_sandbox().map_err(|_| {
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
                    crate::library::publish::paths::read_regular_file(&path)
                        .map_err(|_| "is missing")
                });
            match found {
                Ok(bytes) if &sha256_hex(&bytes) == staged => {}
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
        self.read(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO library_publish_resolutions (bundle_id, resolved_by_user_id, \
                 verified_files, resolved_at) VALUES (?1, ?2, ?3, ?4)",
                params![bundle_id, actor, verified, now_unix()],
            )?;
            Ok(())
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
        })
    }
}
