//! The listening history store's statements.

use droppedneedle_kernel::db::time::Millis;

use super::{HistoryImportSource, HistoryImportState};

droppedneedle_kernel::sql! {
    /// Start reading an uploaded file for one person: the row says which
    /// file and that nothing of it is read yet. The last finished import's
    /// time stays.
    pub(super) START_UPLOAD = "INSERT INTO media_history_imports \
         (user_id, state, source, account, export_id, import_id, entries_read, \
          last_synced_at, updated_at) \
         VALUES (?1, ?2, ?3, NULL, NULL, ?4, 0, NULL, ?5) \
         ON CONFLICT (user_id) DO UPDATE SET \
          state = excluded.state, source = excluded.source, account = NULL, \
          export_id = NULL, import_id = excluded.import_id, entries_read = 0, \
          updated_at = excluded.updated_at";
    /// Whether one person's import still reads `import_id` and has read
    /// `entries_read` of its month files: no forget, new upload or other
    /// run has moved it on.
    pub(super) STILL_READING = "SELECT EXISTS (SELECT 1 FROM media_history_imports \
         WHERE user_id = ?1 AND state = ?2 AND import_id = ?3 AND entries_read = ?4)";
    /// The file one person's import reads and how far it got.
    pub(super) READING = "SELECT import_id, entries_read FROM media_history_imports \
         WHERE user_id = ?1 AND state = ?2 AND import_id IS NOT NULL";
    /// Every file an import is reading, by person.
    pub(super) FILES_READ = "SELECT user_id, import_id FROM media_history_imports \
         WHERE state = ?1 AND import_id IS NOT NULL";
    /// Keep one listen, once. A listen in the same second as one of the
    /// person's plays here is that play coming back from ListenBrainz, and
    /// is not kept.
    pub(super) INSERT_LISTEN = "INSERT INTO media_imported_listens \
         (user_id, listened_at, recording_mbid, artist_mbid, artist_name, track_name, \
          release_name, release_mbid, release_group_mbid, imported_at) \
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9 \
         WHERE NOT EXISTS (SELECT 1 FROM library_play_history p \
          WHERE p.user_id = ?1 AND p.played_at >= ?2 AND p.played_at < ?2 + 1000) \
         ON CONFLICT DO NOTHING";
    /// One month file is read.
    pub(super) MONTH_READ = "UPDATE media_history_imports SET \
          entries_read = entries_read + 1, updated_at = ?5 \
         WHERE user_id = ?1 AND state = ?2 AND import_id = ?3 AND entries_read = ?4";
    /// The whole file is read: the import is current.
    pub(super) FINISHED = "UPDATE media_history_imports SET \
          state = ?5, import_id = NULL, entries_read = 0, \
          last_synced_at = ?6, updated_at = ?6 \
         WHERE user_id = ?1 AND state = ?2 AND import_id = ?3 AND entries_read = ?4";
    /// The file could not be read: the import failed. With no file named,
    /// whichever file the import reads.
    pub(super) FAILED = "UPDATE media_history_imports SET \
          state = ?4, import_id = NULL, entries_read = 0, updated_at = ?5 \
         WHERE user_id = ?1 AND state = ?2 AND (?3 IS NULL OR import_id = ?3)";
    /// Every listen imported for one person.
    pub(super) FORGET_LISTENS = "DELETE FROM media_imported_listens WHERE user_id = ?1";
    /// One person's import, forgotten: only the mark stays, so nothing is
    /// imported again until they ask.
    pub(super) FORGET_IMPORT = "UPDATE media_history_imports SET \
          state = ?2, source = NULL, account = NULL, export_id = NULL, import_id = NULL, \
          entries_read = 0, last_synced_at = NULL, updated_at = ?3 \
         WHERE user_id = ?1";
    /// Where one person's history stands, with their plays here beside it.
    pub(super) STATUS = "SELECT h.state AS state, h.source AS source, h.account AS account, \
          h.last_synced_at AS last_synced_at, \
          (SELECT COUNT(*) FROM media_imported_listens WHERE user_id = ?1) AS listens, \
          (SELECT MIN(listened_at) FROM media_imported_listens WHERE user_id = ?1) \
           AS oldest_listened_at, \
          (SELECT MAX(listened_at) FROM media_imported_listens WHERE user_id = ?1) \
           AS newest_listened_at, \
          (SELECT COUNT(*) FROM library_play_history WHERE user_id = ?1) AS plays \
         FROM (SELECT 1) LEFT JOIN media_history_imports h ON h.user_id = ?1";
    /// Where one person's history stands.
    pub(super) row StatusRow for [STATUS] {
        state: Option<HistoryImportState>,
        source: Option<HistoryImportSource>,
        account: Option<String>,
        last_synced_at: Option<Millis>,
        listens: i64,
        oldest_listened_at: Option<Millis>,
        newest_listened_at: Option<Millis>,
        plays: i64,
    }
    /// The file one person's import reads and how far it got.
    pub(super) row ReadingRow for [READING] {
        import_id: String,
        entries_read: i64,
    }
    /// A file an import is reading.
    pub(super) row FileRow for [FILES_READ] {
        user_id: String,
        import_id: String,
    }
}
