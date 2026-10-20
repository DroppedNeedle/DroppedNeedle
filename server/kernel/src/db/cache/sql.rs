//! The provider cache file's schema and statements. The schema is created
//! whole when the file is made; a different schema means a different
//! version stamp, and the file is rebuilt rather than migrated.

crate::sql! {
    schema = Cache;
    /// The whole schema, run once on a new file. Times are integer
    /// milliseconds; `value` is last so stats and the sweep never read a
    /// value's overflow pages, and is NULL exactly when the row records a
    /// "not found".
    pub fragment SCHEMA = "CREATE TABLE provider_cache_entries (
            key TEXT PRIMARY KEY NOT NULL,
            source TEXT NOT NULL CHECK (length(source) BETWEEN 1 AND 32),
            kind TEXT NOT NULL CHECK (kind IN ('found', 'missing')),
            size INTEGER NOT NULL,
            fetched_at INTEGER NOT NULL,
            fresh_until INTEGER NOT NULL,
            keep_until INTEGER NOT NULL,
            value BLOB,
            CHECK ((kind = 'found') = (value IS NOT NULL))
        ) STRICT;
        CREATE INDEX provider_cache_entries_keep_until ON provider_cache_entries (keep_until);
        CREATE INDEX provider_cache_entries_fetched_at ON provider_cache_entries (fetched_at);
        CREATE INDEX provider_cache_entries_source_size ON provider_cache_entries (source, size);";
    /// One row by key.
    pub(super) GET = "SELECT kind, fetched_at, fresh_until, keep_until, value
                 FROM provider_cache_entries WHERE key = ?1";
    /// The stored size of one row, before it is replaced.
    pub(super) SIZE_OF = "SELECT size FROM provider_cache_entries WHERE key = ?1";
    /// Store one row, replacing any row under the same key.
    pub(super) PUT = "INSERT INTO provider_cache_entries
                     (key, source, kind, size, fetched_at, fresh_until, keep_until, value)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (key) DO UPDATE SET
                     source = excluded.source,
                     kind = excluded.kind,
                     size = excluded.size,
                     fetched_at = excluded.fetched_at,
                     fresh_until = excluded.fresh_until,
                     keep_until = excluded.keep_until,
                     value = excluded.value";
    /// Drop one row by key.
    pub(super) DELETE_KEY = "DELETE FROM provider_cache_entries WHERE key = ?1 RETURNING size";
    /// Drop every row.
    pub(super) CLEAR = "DELETE FROM provider_cache_entries RETURNING size";
    /// Drop one chunk of rows kept past their time.
    pub(super) DELETE_EXPIRED = "DELETE FROM provider_cache_entries WHERE key IN (
                     SELECT key FROM provider_cache_entries WHERE keep_until <= ?1
                     ORDER BY keep_until LIMIT ?2)
                 RETURNING size";
    /// Drop one chunk of the oldest rows.
    pub(super) DELETE_OLDEST = "DELETE FROM provider_cache_entries WHERE key IN (
                     SELECT key FROM provider_cache_entries ORDER BY fetched_at LIMIT ?1)
                 RETURNING size";
    /// The bytes every row holds, read from the table itself rather than an
    /// index, so opening the file walks every table page.
    pub(super) TOTAL_SIZE = "SELECT COALESCE(SUM(size), 0) FROM provider_cache_entries NOT INDEXED";
    /// Rows and bytes per source.
    pub(super) BY_SOURCE = "SELECT source, COUNT(*), COALESCE(SUM(size), 0)
                 FROM provider_cache_entries GROUP BY source ORDER BY source";
}
