//! The statements of the per-user section prefs and the library policy
//! catalog reads.

droppedneedle_kernel::sql! {
    /// Whether user `?1` has an enabled ListenBrainz link.
    pub(super) LISTENBRAINZ_LINKED = "SELECT 1 FROM user_connections \
        WHERE user_id = ?1 AND service = 'listenbrainz' AND enabled = 1 LIMIT 1";
    /// The section keys user `?1` turned off on page `?2`.
    pub(super) DISABLED_SECTIONS = "SELECT section_key FROM user_section_prefs \
        WHERE user_id = ?1 AND page = ?2 AND enabled = 0";
    /// Forget user `?1`'s choices on page `?2`.
    pub(super) CLEAR_SECTION_PREFS = "DELETE FROM user_section_prefs \
        WHERE user_id = ?1 AND page = ?2";
    /// Turn one section off for one user page.
    pub(super) DISABLE_SECTION = "INSERT INTO user_section_prefs \
        (user_id, page, section_key, enabled, updated_at) \
        VALUES (?1, ?2, ?3, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))";

    /// The scopes asked about, from a JSON array of
    /// `[root_id, key, prefix, pattern]` in `?1`.
    fragment REQUESTED = "WITH requested AS (SELECT \
        json_extract(value, '$[0]') AS root_id, json_extract(value, '$[1]') AS scope_key, \
        json_extract(value, '$[2]') AS prefix, json_extract(value, '$[3]') AS pattern \
        FROM json_each(?1))";
    /// A track under a requested scope: the whole root, the folder itself
    /// or anything below it.
    fragment UNDER_SCOPE = "track.root_id = requested.root_id AND (requested.prefix = '.' \
        OR track.relative_path = requested.prefix \
        OR track.relative_path LIKE requested.pattern ESCAPE '\\')";
    /// Indexed and on-disk files under each requested scope.
    pub(super) SCOPE_COUNTS = REQUESTED + " SELECT requested.root_id, requested.scope_key, \
        COALESCE(SUM(track.availability = 'indexed'), 0) AS indexed, \
        COALESCE(SUM(track.availability IN ('indexed', 'excluded')), 0) AS on_disk \
        FROM requested LEFT JOIN local_tracks track ON " + UNDER_SCOPE + " \
        GROUP BY requested.root_id, requested.scope_key";
    /// Indexed, on-disk and all files under any requested scope, each
    /// counted once.
    pub(super) SCOPE_TOTALS = REQUESTED + " SELECT \
        COALESCE(SUM(track.availability = 'indexed'), 0) AS indexed, \
        COALESCE(SUM(track.availability IN ('indexed', 'excluded')), 0) AS on_disk, \
        COUNT(*) AS all_files \
        FROM local_tracks track WHERE EXISTS (SELECT 1 FROM requested WHERE " + UNDER_SCOPE
        + ")";
    /// Whether the catalog holds any track.
    pub(super) ANY_TRACK = "SELECT EXISTS(SELECT 1 FROM local_tracks)";
    /// Each library folder the catalog holds tracks for, with one sample
    /// path. SQLite fills the bare `relative_path` from the row that holds
    /// `MIN(file_path)`, so both sample paths come from one row.
    pub(super) CATALOG_ROOTS = "SELECT root_id, COUNT(*) AS track_count, \
        MIN(file_path) AS sample_file_path, relative_path AS sample_relative_path, \
        SUM(availability IN ('indexed', 'excluded')) > 0 AS any_on_disk \
        FROM local_tracks GROUP BY root_id ORDER BY root_id";

    /// One scope's file counts.
    pub(super) row ScopeCountRow for [SCOPE_COUNTS] {
        root_id: String,
        scope_key: String,
        indexed: i64,
        on_disk: i64,
    }
    /// The file counts across the requested scopes.
    pub(super) row ScopeTotalsRow for [SCOPE_TOTALS] {
        indexed: i64,
        on_disk: i64,
        all_files: i64,
    }
    /// One library folder the catalog holds tracks for.
    pub(super) row CatalogRootRow for [CATALOG_ROOTS] {
        root_id: String,
        track_count: i64,
        sample_file_path: String,
        sample_relative_path: String,
        any_on_disk: bool,
    }
}
