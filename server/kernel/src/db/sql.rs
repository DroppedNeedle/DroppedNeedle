//! The statements of the database runtime: the loop liveness rows, the planner
//! statistics, and the checks run on a database file before it is opened or
//! after it is copied.

crate::sql! {
    /// Forget every loop row: the supervisor rebuilds them at each start.
    pub(super) CLEAR_LOOPS = "DELETE FROM durable_loops";
    /// Write one loop's row as it stands now.
    pub(super) PUT_LOOP = "INSERT INTO durable_loops (name, state, last_heartbeat_at, last_error, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (name) DO UPDATE SET
                     state = excluded.state,
                     last_heartbeat_at = excluded.last_heartbeat_at,
                     last_error = excluded.last_error,
                     updated_at = excluded.updated_at";
    /// Every loop row in name order.
    pub(super) ALL_LOOPS = "SELECT name, state, last_heartbeat_at, last_error, updated_at
                 FROM durable_loops ORDER BY name";
    /// What the last planner statistics run recorded.
    pub(super) LAST_ANALYZE = "SELECT analyzed_at, indexed_tracks FROM db_planner_stats WHERE singleton = 1";
    /// Whether SQLite holds planner statistics at all.
    pub(super) STATS_PRESENT = "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'sqlite_stat1')";
    /// The streamable track count the statistics are measured against.
    pub(super) INDEXED_TRACKS = "SELECT COALESCE(SUM(indexed_tracks), 0) FROM library_track_format_stats";
    /// Every table to analyze: no SQLite, virtual or full-text shadow tables.
    pub(super) ANALYZED_TABLES = "SELECT name FROM sqlite_schema WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' AND sql NOT LIKE 'CREATE VIRTUAL TABLE%' \
             AND name NOT LIKE '%\\_fts\\_%' ESCAPE '\\' ORDER BY name";
    /// Record a planner statistics run.
    pub(super) RECORD_ANALYZE = "INSERT INTO db_planner_stats (singleton, analyzed_at, indexed_tracks) \
                     VALUES (1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET \
                     analyzed_at = excluded.analyzed_at, indexed_tracks = excluded.indexed_tracks";
    /// The user table count of a database file, beside its stamp.
    pub(super) USER_TABLE_COUNT = "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'";
    /// How many of the two undo-copy tables a database file has (2 since
    /// migration 0012).
    pub(super) UNDO_TABLES_PRESENT = "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
             AND name IN ('library_publish_blob_refs', 'library_publish_blobs')";
    /// The undo copies a database names that its own table does not hold:
    /// the ones that live in the undo folder.
    pub(super) UNDO_NAMED_IN_FOLDER = "SELECT DISTINCT r.sha256 FROM library_publish_blob_refs r \
             WHERE NOT EXISTS (SELECT 1 FROM library_publish_blobs b WHERE b.sha256 = r.sha256) \
             ORDER BY r.sha256";
    /// Foreign-key violations in a database copy.
    pub(super) FOREIGN_KEY_VIOLATIONS = "SELECT COUNT(*) FROM pragma_foreign_key_check";
}
