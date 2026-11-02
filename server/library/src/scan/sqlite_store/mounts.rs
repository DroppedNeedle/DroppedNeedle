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
                crate::scan::sql::RECORD_MOUNT.text(),
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
        let rows = self.read_long(stop, "scan_empty_mounts", |conn| {
            let mut stmt = conn.prepare(crate::scan::sql::EMPTY_MOUNTS.text())?;
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
