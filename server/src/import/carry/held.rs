//! Held downloads: the file lands in `<cache>/held` first, then the row
//! goes in with `held_path` pointing at it.
//!
//! The file keeps v2's name (v2 already made it unique). If v3 holds a
//! different file under that name, the carried one is named after its row
//! id instead; the same bytes under the same name are reused. A crash
//! between the file and the row leaves only an unreferenced file, which a
//! rerun reuses.

use std::path::{Path, PathBuf};

use sqlx::SqliteConnection;

use super::avatars::write_atomically;
use super::bundle::SCHEMA;
use super::{CarryError, SectionResult};
use crate::export::sections::{Column, TableSection};

/// The bundle column holding the file's bytes.
const FILE_COLUMN: &str = "file";

/// Where one held file lands, or why it cannot.
fn destination(
    dir: &Path,
    id: i64,
    stored: &str,
    bytes: &[u8],
) -> std::io::Result<Option<PathBuf>> {
    let Some(name) = Path::new(stored)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
    else {
        return Ok(None);
    };
    for candidate in [name.to_owned(), format!("v2-held-{id}-{name}")] {
        let path = dir.join(&candidate);
        match std::fs::read(&path) {
            Ok(existing) if existing == bytes => return Ok(Some(path)),
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(path)),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn io(error: impl std::fmt::Display) -> CarryError {
    CarryError::Io(error.to_string())
}

/// Carry every held row of the bundle with its file.
pub(crate) async fn apply(
    conn: &mut SqliteConnection,
    section: &TableSection,
    present: &[&'static Column],
    cache_dir: Option<&Path>,
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let name = section.name;
    let ids: Vec<i64> =
        sqlx::query_scalar(&format!("SELECT id FROM {SCHEMA}.\"{name}\" ORDER BY id"))
            .persistent(false)
            .fetch_all(&mut *conn)
            .await?;
    let mut result = SectionResult {
        rows: ids.len() as u64,
        ..SectionResult::default()
    };
    let Some(cache_dir) = cache_dir else {
        if !ids.is_empty() {
            result.counts.dropped_invalid += ids.len() as u64;
            result.note(
                String::new(),
                "dropped_invalid",
                "no v3 cache folder was given, so held files cannot be written",
            );
        }
        return Ok(result);
    };
    let dir = cache_dir.join("held");
    let columns: Vec<&str> = present
        .iter()
        .map(|column| column.name)
        .filter(|column| *column != FILE_COLUMN)
        .collect();
    let column_list = columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(", ");
    for id in ids {
        let existing: Option<(String, f64)> =
            sqlx::query_as("SELECT user_id, created_at FROM main.held_imports WHERE id = ?1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await?;
        let (user_id, created_at, held_path, file): (String, f64, String, Option<Vec<u8>>) =
            sqlx::query_as(&format!(
                "SELECT user_id, created_at, held_path, \"{FILE_COLUMN}\" \
                 FROM {SCHEMA}.\"{name}\" WHERE id = ?1"
            ))
            .bind(id)
            .persistent(false)
            .fetch_one(&mut *conn)
            .await?;
        if let Some((held_user, held_at)) = existing {
            if held_user == user_id && held_at == created_at {
                result.counts.skipped_identical += 1;
            } else {
                result.counts.conflict_kept_existing += 1;
                result.note(
                    id.to_string(),
                    "conflict_kept_existing",
                    "v3 already has a different held download with this id",
                );
            }
            continue;
        }
        let Some(bytes) = file else {
            result.counts.dropped_invalid += 1;
            result.note(
                id.to_string(),
                "dropped_invalid",
                "the held file is missing",
            );
            continue;
        };
        let (dir_for, stored) = (dir.clone(), held_path.clone());
        let (target, bytes) = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir_for)?;
            destination(&dir_for, id, &stored, &bytes).map(|target| (target, bytes))
        })
        .await
        .map_err(io)?
        .map_err(io)?;
        let Some(target) = target else {
            result.counts.dropped_invalid += 1;
            result.note(
                id.to_string(),
                "dropped_invalid",
                "the held file has no usable name",
            );
            continue;
        };
        result.counts.imported += 1;
        if dry_run {
            continue;
        }
        let path = target.clone();
        tokio::task::spawn_blocking(move || {
            if path.is_file() {
                Ok(())
            } else {
                write_atomically(&path, &bytes)
            }
        })
        .await
        .map_err(io)?
        .map_err(io)?;
        sqlx::query(&format!(
            "INSERT INTO main.held_imports ({column_list}) SELECT {column_list} \
             FROM {SCHEMA}.\"{name}\" WHERE id = ?1"
        ))
        .bind(id)
        .persistent(false)
        .execute(&mut *conn)
        .await?;
        sqlx::query("UPDATE main.held_imports SET held_path = ?1 WHERE id = ?2")
            .bind(target.to_string_lossy().into_owned())
            .bind(id)
            .execute(&mut *conn)
            .await?;
        result.written += 1;
    }
    Ok(result)
}
