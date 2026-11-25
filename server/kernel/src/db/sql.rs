//! The statements of the database runtime: the planner statistics, and the
//! checks run on a database file before it is opened or after it is copied.

use super::time::Millis;

crate::sql! {
    /// What the last planner statistics run recorded.
    pub(super) LAST_ANALYZE = "SELECT analyzed_at, indexed_tracks FROM db_planner_stats WHERE singleton = 1";
    /// Whether SQLite holds planner statistics at all.
    pub(super) STATS_PRESENT = "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'sqlite_stat1')";
    /// The streamable track count the statistics are measured against.
    pub(super) INDEXED_TRACKS = "SELECT COALESCE(SUM(indexed_tracks), 0) FROM catalog_track_format_stats";
    /// Every table to analyze: no SQLite, virtual or full-text shadow tables.
    pub(super) ANALYZED_TABLES = "SELECT name FROM sqlite_schema WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' AND sql NOT LIKE 'CREATE VIRTUAL TABLE%' \
             AND name NOT LIKE '%\\_fts\\_%' ESCAPE '\\' ORDER BY name";
    /// Forget the last planner statistics run, so the next check takes
    /// them again: a migration may have added indexes or rebuilt tables,
    /// whose statistics SQLite then holds none of.
    pub(super) FORGET_ANALYZE = "DELETE FROM db_planner_stats";
    /// Record a planner statistics run.
    pub(super) RECORD_ANALYZE = "INSERT INTO db_planner_stats (singleton, analyzed_at, indexed_tracks) \
                     VALUES (1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET \
                     analyzed_at = excluded.analyzed_at, indexed_tracks = excluded.indexed_tracks";
    /// The table count the schema runner reads beside a file's stamp:
    /// SQLite's own tables and sqlx's old `_sqlx_migrations` do not count.
    pub(super) USER_TABLE_COUNT = "SELECT count(*) FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'";
    /// The old bookkeeping table `_sqlx_migrations`, which the schema
    /// runner's stamp replaces.
    pub(super) DROP_SQLX_MIGRATIONS = "DROP TABLE IF EXISTS _sqlx_migrations";
    /// How many of the two undo-copy tables a database file has.
    pub(super) UNDO_TABLES_PRESENT = "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' \
             AND name IN ('library_publish_blob_refs', 'library_publish_blobs')";
    /// The undo copies a database names that its own table does not hold:
    /// the ones that live in the undo folder.
    pub(super) UNDO_NAMED_IN_FOLDER = "SELECT DISTINCT r.sha256 FROM library_publish_blob_refs r \
             WHERE NOT EXISTS (SELECT 1 FROM library_publish_blobs b WHERE b.sha256 = r.sha256) \
             ORDER BY r.sha256";
    /// Foreign-key violations in a database copy.
    pub(super) FOREIGN_KEY_VIOLATIONS = "SELECT COUNT(*) FROM pragma_foreign_key_check";
    /// What the last planner statistics run recorded.
    pub(super) row LastAnalyze for [LAST_ANALYZE] {
        analyzed_at: Millis,
        indexed_tracks: i64,
    }
}
