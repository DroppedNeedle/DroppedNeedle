//! SQLite adapter for the library policy read port.
//!
//! Read-only queries over `local_tracks`, the catalog the scan engine
//! writes. Counts follow v2: "indexed" is `availability = 'indexed'`,
//! "on disk" adds excluded files, and missing files count only in the
//! all-rows total the apply preview uses. A scope is a root (`.`) or a
//! rule directory relative to its root.

use std::collections::BTreeSet;

use futures_util::future::BoxFuture;
use sqlx::Row;

use super::library_policy::{
    CatalogPath, CatalogRoot, LibraryPolicyCatalog, ScopeCounts, ScopeTotals,
};

/// The production catalog reads.
pub struct SqliteLibraryPolicyCatalog {
    /// Reader pool.
    pub pool: sqlx::SqlitePool,
}

/// Escape `\`, `%` and `_` for a `LIKE ... ESCAPE '\'` pattern.
fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The scopes as a JSON array of `[root_id, key, prefix, pattern]`,
/// where `key` is the relative path as the caller passed it, `prefix`
/// is `.` for a whole root, and `pattern` matches files below the
/// prefix. Duplicate scopes are sent once. Fed to `json_each` so one
/// query covers every scope.
fn requested_json(scopes: &[(String, String)]) -> String {
    let unique: BTreeSet<&(String, String)> = scopes.iter().collect();
    let rows: Vec<serde_json::Value> = unique
        .into_iter()
        .map(|(root_id, relative)| {
            let trimmed = relative.trim_matches('/');
            let prefix = if trimmed.is_empty() { "." } else { trimmed };
            serde_json::json!([
                root_id,
                relative,
                prefix,
                format!("{}/%", escape_like(prefix))
            ])
        })
        .collect();
    serde_json::Value::Array(rows).to_string()
}

const REQUESTED: &str = "WITH requested AS (SELECT \
     json_extract(value, '$[0]') AS root_id, json_extract(value, '$[1]') AS scope_key, \
     json_extract(value, '$[2]') AS prefix, json_extract(value, '$[3]') AS pattern \
     FROM json_each(?1))";

const UNDER_SCOPE: &str = "track.root_id = requested.root_id AND (requested.prefix = '.' \
     OR track.relative_path = requested.prefix \
     OR track.relative_path LIKE requested.pattern ESCAPE '\\')";

impl LibraryPolicyCatalog for SqliteLibraryPolicyCatalog {
    fn scope_counts<'a>(
        &'a self,
        scopes: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<ScopeCounts, String>> {
        Box::pin(async move {
            if scopes.is_empty() {
                return Ok(ScopeCounts::new());
            }
            let query = format!(
                "{REQUESTED} SELECT requested.root_id, requested.scope_key, \
                 COALESCE(SUM(track.availability = 'indexed'), 0), \
                 COALESCE(SUM(track.availability IN ('indexed', 'excluded')), 0) \
                 FROM requested LEFT JOIN local_tracks track ON {UNDER_SCOPE} \
                 GROUP BY requested.root_id, requested.scope_key"
            );
            let rows = sqlx::query(&query)
                .bind(requested_json(scopes))
                .fetch_all(&self.pool)
                .await
                .map_err(|cause| cause.to_string())?;
            let mut counts = ScopeCounts::new();
            for row in rows {
                let root_id: String = row.try_get(0).map_err(|cause| cause.to_string())?;
                let key: String = row.try_get(1).map_err(|cause| cause.to_string())?;
                let indexed: i64 = row.try_get(2).map_err(|cause| cause.to_string())?;
                let on_disk: i64 = row.try_get(3).map_err(|cause| cause.to_string())?;
                counts.insert((root_id, key), (indexed, on_disk));
            }
            Ok(counts)
        })
    }

    fn scope_totals<'a>(
        &'a self,
        scopes: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<ScopeTotals, String>> {
        Box::pin(async move {
            if scopes.is_empty() {
                return Ok(ScopeTotals::default());
            }
            let query = format!(
                "{REQUESTED} SELECT COALESCE(SUM(track.availability = 'indexed'), 0), \
                 COALESCE(SUM(track.availability IN ('indexed', 'excluded')), 0), COUNT(*) \
                 FROM local_tracks track \
                 WHERE EXISTS (SELECT 1 FROM requested WHERE {UNDER_SCOPE})"
            );
            let (indexed, on_disk, all): (i64, i64, i64) = sqlx::query_as(&query)
                .bind(requested_json(scopes))
                .fetch_one(&self.pool)
                .await
                .map_err(|cause| cause.to_string())?;
            Ok(ScopeTotals {
                indexed,
                on_disk,
                all,
            })
        })
    }

    fn has_tracks<'a>(&'a self) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM local_tracks)")
                .fetch_one(&self.pool)
                .await
                .map_err(|cause| cause.to_string())
        })
    }

    fn catalog_roots<'a>(&'a self) -> BoxFuture<'a, Result<Vec<CatalogRoot>, String>> {
        Box::pin(async move {
            // SQLite fills the bare `relative_path` from the row that holds
            // MIN(file_path), so both sample paths come from one row.
            let rows: Vec<(String, i64, String, String)> = sqlx::query_as(
                "SELECT root_id, COUNT(*), MIN(file_path), relative_path \
                 FROM local_tracks GROUP BY root_id ORDER BY root_id",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|cause| cause.to_string())?;
            Ok(rows
                .into_iter()
                .map(
                    |(root_id, track_count, sample_file_path, sample_relative_path)| CatalogRoot {
                        root_id,
                        sample_file_path,
                        sample_relative_path,
                        track_count,
                    },
                )
                .collect())
        })
    }

    fn track_paths<'a>(&'a self) -> BoxFuture<'a, Result<Vec<CatalogPath>, String>> {
        Box::pin(async move {
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT id, file_path FROM local_tracks ORDER BY id")
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|cause| cause.to_string())?;
            Ok(rows
                .into_iter()
                .map(|(track_id, file_path)| CatalogPath {
                    track_id,
                    file_path,
                })
                .collect())
        })
    }
}
