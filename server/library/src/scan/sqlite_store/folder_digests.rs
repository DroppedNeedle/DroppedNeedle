//! What the folder watcher last saw of each folder (`library_folder_digests`).
//!
//! The watcher compares each walk with these rows and asks for a scan of
//! the folders that moved; it writes the rows when it first sees a root and
//! after a scan it asked for has settled. Each call is tried once: a busy
//! database answers an error, and the watcher tries again on its next poll.

use super::super::walk::FolderDigest;
use super::*;

/// Rows a digest write commits per turn. Each row is a complete change: a
/// write cut short leaves rows the next poll compares against as they are.
const DIGEST_CHUNK: usize = 1000;

impl SqliteScanStore {
    /// The recorded digests of root `root_id`, by folder. Empty when the
    /// watcher has not seen the root.
    pub fn folder_digests(
        &self,
        root_id: &str,
    ) -> Result<HashMap<String, FolderDigest>, ScanStoreError> {
        self.read_long(&RunStop::never(), "scan_folder_digests", |conn| {
            let mut stmt = conn.prepare(
                "SELECT relative_path, digest, audio_files FROM library_folder_digests \
                 WHERE root_id = ?1",
            )?;
            let rows = stmt
                .query_map(params![root_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        FolderDigest {
                            digest: row.get(1)?,
                            audio_files: row.get(2)?,
                        },
                    ))
                })?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?;
            Ok(rows)
        })
    }

    /// Record `changes` for root `root_id`: a digest is written for its
    /// folder, `None` deletes the folder's row.
    pub fn record_folder_digests(
        &self,
        root_id: &str,
        changes: &[(String, Option<FolderDigest>)],
    ) -> Result<(), ScanStoreError> {
        let written = self.db.write_chunked_blocking(
            "scan_record_folder_digests",
            changes,
            DIGEST_CHUNK,
            RunStop::never().flag(),
            |rows: &[(String, Option<FolderDigest>)], tx| -> Result<(), ScanStoreError> {
                for (folder, digest) in rows {
                    match digest {
                        Some(digest) => tx.execute(
                            "INSERT INTO library_folder_digests \
                             (root_id, relative_path, digest, audio_files) VALUES (?1, ?2, ?3, ?4) \
                             ON CONFLICT (root_id, relative_path) DO UPDATE SET \
                             digest = excluded.digest, audio_files = excluded.audio_files",
                            params![root_id, folder, digest.digest, digest.audio_files],
                        )?,
                        None => tx.execute(
                            "DELETE FROM library_folder_digests \
                             WHERE root_id = ?1 AND relative_path = ?2",
                            params![root_id, folder],
                        )?,
                    };
                }
                Ok(())
            },
        );
        match written {
            Ok(_) => Ok(()),
            Err(ChunkedError::Cancelled { .. }) => Err(ScanStoreError::Stopped),
            Err(ChunkedError::Failed { error, .. }) => Err(error),
        }
    }
}
