//! SQLite adapter for the library policy read port.
//!
//! Read-only queries over `local_tracks`, the catalog the scan engine
//! writes. Counts follow v2: "indexed" is `availability = 'indexed'`,
//! "on disk" adds excluded files, and missing files count only in the
//! all-rows total the apply preview uses. A scope is a root (`.`) or a
//! rule directory relative to its root.

use std::collections::BTreeSet;

use futures_util::future::BoxFuture;

use super::sql;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_library::policy::{CatalogRoot, LibraryPolicyCatalog, ScopeCounts, ScopeTotals};

/// The production catalog reads.
pub struct SqliteLibraryPolicyCatalog {
    db: Db,
}

impl SqliteLibraryPolicyCatalog {
    /// The reads over the database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }
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

impl LibraryPolicyCatalog for SqliteLibraryPolicyCatalog {
    fn scope_counts<'a>(
        &'a self,
        scopes: &'a [(String, String)],
    ) -> BoxFuture<'a, Result<ScopeCounts, String>> {
        Box::pin(async move {
            if scopes.is_empty() {
                return Ok(ScopeCounts::new());
            }
            let requested = requested_json(scopes);
            self.db
                .read("settings.policy.scope_counts", move |conn| {
                    let mut counts = ScopeCounts::new();
                    let mut statement = conn.prepare_cached(sql::SCOPE_COUNTS.text())?;
                    for row in statement.query_map([requested], sql::ScopeCountRow::read)? {
                        let row = row?;
                        counts.insert((row.root_id, row.scope_key), (row.indexed, row.on_disk));
                    }
                    Ok::<_, DbError>(counts)
                })
                .await
                .map_err(|cause| cause.to_string())
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
            let requested = requested_json(scopes);
            let totals = self
                .db
                .read("settings.policy.scope_totals", move |conn| {
                    conn.prepare_cached(sql::SCOPE_TOTALS.text())?
                        .query_row([requested], sql::ScopeTotalsRow::read)
                        .map_err(DbError::from)
                })
                .await
                .map_err(|cause| cause.to_string())?;
            Ok(ScopeTotals {
                indexed: totals.indexed,
                on_disk: totals.on_disk,
                all: totals.all_files,
            })
        })
    }

    fn has_tracks<'a>(&'a self) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async move {
            self.db
                .read("settings.policy.has_tracks", |conn| {
                    conn.prepare_cached(sql::ANY_TRACK.text())?
                        .query_row([], |row| row.get(0))
                        .map_err(DbError::from)
                })
                .await
                .map_err(|cause| cause.to_string())
        })
    }

    fn catalog_roots<'a>(&'a self) -> BoxFuture<'a, Result<Vec<CatalogRoot>, String>> {
        Box::pin(async move {
            self.db
                .read("settings.policy.catalog_roots", |conn| {
                    conn.prepare_cached(sql::CATALOG_ROOTS.text())?
                        .query_map([], |row| {
                            let row = sql::CatalogRootRow::read(row)?;
                            Ok(CatalogRoot {
                                root_id: row.root_id,
                                sample_file_path: row.sample_file_path,
                                sample_relative_path: row.sample_relative_path,
                                track_count: row.track_count,
                                any_on_disk: row.any_on_disk,
                            })
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map_err(DbError::from)
                })
                .await
                .map_err(|cause| cause.to_string())
        })
    }
}
