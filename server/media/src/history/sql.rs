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
    /// imported again until they ask. A person who had no import yet gets
    /// the mark too, so a first import the tick spread out never starts.
    pub(super) FORGET_IMPORT = "INSERT INTO media_history_imports \
         (user_id, state, source, account, export_id, import_id, entries_read, \
          last_synced_at, updated_at) \
         VALUES (?1, ?2, NULL, NULL, NULL, NULL, 0, NULL, ?3) \
         ON CONFLICT (user_id) DO UPDATE SET \
          state = excluded.state, source = NULL, account = NULL, export_id = NULL, \
          import_id = NULL, entries_read = 0, last_synced_at = NULL, \
          updated_at = excluded.updated_at";
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
    /// One person's import as its steps read it, with the newest listen
    /// kept (the unique index answers it) and whether any is.
    pub(super) IMPORT_ROW = "SELECT state, account, export_id, import_id, entries_read, \
          last_synced_at, updated_at, \
          (SELECT MAX(listened_at) FROM media_imported_listens WHERE user_id = ?1) \
           AS newest_listened_at \
         FROM media_history_imports WHERE user_id = ?1";
    /// One person's import as its steps read it.
    pub(super) row ImportRow for [IMPORT_ROW] {
        state: HistoryImportState,
        account: Option<String>,
        export_id: Option<String>,
        import_id: Option<String>,
        entries_read: i64,
        last_synced_at: Option<Millis>,
        updated_at: Millis,
        newest_listened_at: Option<Millis>,
    }
    /// A first import from ListenBrainz for a person who has none: an
    /// export is to be asked for.
    pub(super) FIRST_EXPORT = "INSERT INTO media_history_imports \
         (user_id, state, source, account, export_id, import_id, entries_read, \
          last_synced_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, 0, NULL, ?5) \
         ON CONFLICT (user_id) DO NOTHING";
    /// Ask for an export again for a person whose import is in `?6`: what
    /// is kept stays.
    pub(super) EXPORT_AGAIN = "UPDATE media_history_imports SET \
          state = ?2, source = ?3, account = ?4, export_id = NULL, import_id = NULL, \
          entries_read = 0, updated_at = ?5 \
         WHERE user_id = ?1 AND state = ?6";
    /// Replace one person's import with one from another account: only
    /// the mark stays, and an export is to be asked for.
    pub(super) REPLACE_ACCOUNT = "UPDATE media_history_imports SET \
          state = ?2, source = ?3, account = ?4, export_id = NULL, import_id = NULL, \
          entries_read = 0, last_synced_at = NULL, updated_at = ?5 \
         WHERE user_id = ?1";
    /// Whether one person's import is still at the step a run read: the
    /// same state, account and export.
    pub(super) STILL_AT = "SELECT EXISTS (SELECT 1 FROM media_history_imports \
         WHERE user_id = ?1 AND state = ?2 AND account IS ?3 AND export_id IS ?4)";
    /// Move one person's import on from the step a run read to `?5`, now
    /// waiting on export `?6`.
    pub(super) EXPORT_MOVED = "UPDATE media_history_imports SET \
          state = ?5, export_id = ?6, updated_at = ?7 \
         WHERE user_id = ?1 AND state = ?2 AND account IS ?3 AND export_id IS ?4";
    /// The export is downloaded: its file `?6` is to be read.
    pub(super) EXPORT_DOWNLOADED = "UPDATE media_history_imports SET \
          state = ?5, import_id = ?6, entries_read = 0, updated_at = ?7 \
         WHERE user_id = ?1 AND state = ?2 AND account IS ?3 AND export_id IS ?4";
    /// ListenBrainz could not make the export, or it cannot be had: the
    /// import failed. What is kept stays.
    pub(super) EXPORT_FAILED = "UPDATE media_history_imports SET \
          state = ?5, import_id = NULL, entries_read = 0, updated_at = ?6 \
         WHERE user_id = ?1 AND state = ?2 AND account IS ?3 AND export_id IS ?4";
    /// Whether one person's import is current and kept from `?3`'s account,
    /// or from an upload no link has read since.
    pub(super) STILL_CURRENT = "SELECT EXISTS (SELECT 1 FROM media_history_imports \
         WHERE user_id = ?1 AND state = ?2 AND (account IS NULL OR lower(account) = lower(?3)))";
    /// One person's history is brought up to date from `?3`'s account.
    pub(super) SYNCED = "UPDATE media_history_imports SET \
          account = COALESCE(account, ?3), last_synced_at = ?4, updated_at = ?4 \
         WHERE user_id = ?1 AND state = ?2 AND (account IS NULL OR lower(account) = lower(?3))";
    /// The people with a ListenBrainz link, and where their import stands.
    pub(super) LINKED = "SELECT c.user_id AS user_id, h.state AS state, \
          h.last_synced_at AS last_synced_at, h.updated_at AS updated_at \
         FROM user_connections c LEFT JOIN media_history_imports h ON h.user_id = c.user_id \
         WHERE c.service = 'listenbrainz' AND c.enabled = 1";
    /// A person with a ListenBrainz link, and where their import stands.
    pub(super) row LinkedRow for [LINKED] {
        user_id: String,
        state: Option<HistoryImportState>,
        last_synced_at: Option<Millis>,
        updated_at: Option<Millis>,
    }
    /// A read of one person's new listens from `?3`'s account failed: the
    /// try is stamped, the time the history was brought up to date stays.
    pub(super) READ_TRIED = "UPDATE media_history_imports SET updated_at = ?4 \
         WHERE user_id = ?1 AND state = ?2 AND (account IS NULL OR lower(account) = lower(?3))";
}
