//! Folders inside a library folder that are drives or shares of their own.
//!
//! The walk reports each folder on another filesystem than the folder above
//! it, and the scan remembers it (`library_scan_mounts`). A share that drops
//! out leaves its mount point empty, which looks like its files were
//! deleted, so the guard holds back a remembered mount that a walk found
//! empty while it still has indexed files.

use super::*;

impl SqliteScanStore {
    /// Remember that `relative_path` (an inventory key, never `.`) is a
    /// mount in the root. Kept for good.
    pub fn record_mount(
        &self,
        stop: &RunStop,
        root_id: &str,
        relative_path: &str,
    ) -> Result<(), ScanStoreError> {
        self.write(stop, Lane::Background, "scan_record_mount", |tx| {
            tx.execute(
                "INSERT INTO library_scan_mounts (root_id, relative_path) VALUES (?1, ?2) \
                 ON CONFLICT (root_id, relative_path) DO NOTHING",
                params![root_id, relative_path],
            )?;
            Ok(())
        })
    }

    /// The remembered mounts of the root at or under `scope_relative_path`
    /// that have no file at all under them in the run's current inventory,
    /// with how many of their indexed tracks the run walked past (only
    /// mounts where that is more than none).
    pub fn empty_mounts(
        &self,
        stop: &RunStop,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<Vec<(String, usize)>, ScanStoreError> {
        let sql = format!(
            "SELECT relative_path, indexed FROM (SELECT m.relative_path, \
               (SELECT COUNT(*) FROM local_tracks t WHERE t.root_id = m.root_id \
                AND t.availability = 'indexed' \
                AND t.relative_path > m.relative_path || '/' \
                AND t.relative_path < m.relative_path || '0' AND {}) AS indexed \
             FROM library_scan_mounts m \
             WHERE m.root_id = ?1 AND {} \
             AND NOT EXISTS (SELECT 1 FROM library_scan_inventory i \
               JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
               AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
               WHERE i.run_id = ?3 AND i.root_id = ?1 \
               AND i.relative_path > m.relative_path || '/' \
               AND i.relative_path < m.relative_path || '0' \
               AND s.discovery_generation = i.discovery_generation)) \
             WHERE indexed > 0 ORDER BY relative_path",
            super::commit::walked_past!("?3"),
            super::held_scopes::covers("?2", "m.relative_path"),
        );
        let rows = self.read_long(stop, "scan_empty_mounts", |conn| {
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params![root_id, scope_relative_path, run_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        Ok(rows
            .into_iter()
            .map(|(path, indexed)| (path, indexed.max(0) as usize))
            .collect())
    }
}
