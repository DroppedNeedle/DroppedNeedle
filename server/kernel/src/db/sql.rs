//! The statements of the database runtime: the job registry, the planner
//! statistics, and the checks run on a database file before it is opened or
//! after it is copied.

crate::sql! {
    /// Register a job, or refresh its kind when the name exists.
    pub(super) REGISTER_JOB = "INSERT INTO durable_job_registry (name, kind, state, updated_at)
                 VALUES (?1, ?2, 'idle', ?3)
                 ON CONFLICT (name) DO UPDATE SET
                     kind = excluded.kind,
                     updated_at = excluded.updated_at";
    /// Move a job to a new liveness state.
    pub(super) SET_JOB_STATE = "UPDATE durable_job_registry
                 SET state = ?1, updated_at = ?2 WHERE name = ?3";
    /// Beat a job's heart.
    pub(super) BEAT_JOB = "UPDATE durable_job_registry
                 SET last_heartbeat_at = ?1, updated_at = ?1 WHERE name = ?2";
    /// One registry row.
    pub(super) JOB_BY_NAME = "SELECT name, kind, state, last_heartbeat_at, updated_at
                 FROM durable_job_registry WHERE name = ?1";
    /// Every registry row in name order.
    pub(super) ALL_JOBS = "SELECT name, kind, state, last_heartbeat_at, updated_at
                 FROM durable_job_registry ORDER BY name";
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
    /// Foreign-key violations in a database copy.
    pub(super) FOREIGN_KEY_VIOLATIONS = "SELECT COUNT(*) FROM pragma_foreign_key_check";
}
